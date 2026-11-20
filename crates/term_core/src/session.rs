//! A terminal session: PTY + VT emulator + event pump.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::color::{Palette, color_by_table_index};
use crate::snapshot::{Snapshot, build_snapshot_into};

/// Errors raised while creating or driving a [`Session`].
#[derive(thiserror::Error, Debug)]
pub enum TermError {
    /// The pseudo-terminal or child process could not be created.
    #[error("failed to create PTY / spawn `{program}`: {source}")]
    Pty {
        /// The program that was being launched.
        program: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The PTY event loop could not be created.
    #[error("failed to start terminal event loop: {0}")]
    EventLoop(#[source] std::io::Error),
}

/// Parameters for spawning a [`Session`].
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Program to run (for example `pwsh.exe`).
    pub program: String,
    /// Arguments passed to the program.
    pub args: Vec<String>,
    /// Working directory; `None` inherits the current one.
    pub cwd: Option<PathBuf>,
    /// Initial number of columns.
    pub cols: u16,
    /// Initial number of rows.
    pub rows: u16,
    /// Cell width in pixels (reported to the PTY / `CSI 14 t` style queries).
    pub cell_width: u16,
    /// Cell height in pixels.
    pub cell_height: u16,
    /// Color palette for resolving colors and answering color queries.
    pub palette: Palette,
    /// Maximum scrollback lines.
    pub scrollback: usize,
}

/// Notifications emitted by a [`Session`].
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Terminal content changed. Coalesced: no new `Wakeup` is sent until
    /// [`Session::snapshot`] has been called.
    Wakeup,
    /// The window title changed (empty string means "reset to default").
    Title(String),
    /// The bell rang.
    Bell,
    /// The child process exited (sent at most once).
    Exit,
}

/// State shared between the session handle, the event loop thread and the listener.
struct Shared {
    tx: UnboundedSender<SessionEvent>,
    /// True while a `Wakeup` has been sent but not yet consumed by `snapshot()`.
    wakeup_pending: AtomicBool,
    exited: AtomicBool,
    /// Packed `cols | rows << 16 | cell_w << 32 | cell_h << 48`.
    size: AtomicU64,
    palette: Palette,
    /// PTY sender, set once the event loop exists (needed to answer `PtyWrite`).
    pty: OnceLock<EventLoopSender>,
}

fn pack_size(cols: u16, rows: u16, cw: u16, ch: u16) -> u64 {
    u64::from(cols) | u64::from(rows) << 16 | u64::from(cw) << 32 | u64::from(ch) << 48
}

fn unpack_size(v: u64) -> WindowSize {
    WindowSize {
        num_cols: v as u16,
        num_lines: (v >> 16) as u16,
        cell_width: (v >> 32) as u16,
        cell_height: (v >> 48) as u16,
    }
}

impl Shared {
    fn emit(&self, event: SessionEvent) {
        // The receiver may have been dropped; that is fine.
        let _ = self.tx.unbounded_send(event);
    }

    fn write_pty(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        match self.pty.get() {
            Some(sender) => {
                if let Err(e) = sender.send(Msg::Input(Cow::Owned(bytes))) {
                    log::debug!("pty write failed: {e}");
                }
            },
            None => log::debug!("pty write before event loop was ready; dropped"),
        }
    }
}

/// Forwards alacritty events to the session channel and answers terminal queries.
#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let s = &self.0;
        match event {
            Event::Wakeup => {
                if !s.wakeup_pending.swap(true, Ordering::AcqRel) {
                    s.emit(SessionEvent::Wakeup);
                }
            },
            Event::Title(t) => s.emit(SessionEvent::Title(t)),
            Event::ResetTitle => s.emit(SessionEvent::Title(String::new())),
            Event::Bell => s.emit(SessionEvent::Bell),
            Event::Exit | Event::ChildExit(_) => {
                if !s.exited.swap(true, Ordering::AcqRel) {
                    s.emit(SessionEvent::Exit);
                }
            },
            // ConPTY sends DSR queries on startup; shells stall without the reply.
            Event::PtyWrite(text) => s.write_pty(text.into_bytes()),
            Event::ColorRequest(index, format) => {
                // The term lock is held while this runs, so only the base palette is used here.
                if let Some(c) = color_by_table_index(&s.palette, index) {
                    s.write_pty(format(c.into()).into_bytes());
                }
            },
            Event::TextAreaSizeRequest(format) => {
                let size = unpack_size(s.size.load(Ordering::Acquire));
                s.write_pty(format(size).into_bytes());
            },
            Event::ClipboardStore(..) | Event::ClipboardLoad(..) => {
                log::debug!("clipboard request ignored");
            },
            Event::MouseCursorDirty | Event::CursorBlinkingChange => {},
        }
    }
}

struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// A running terminal session. Dropping it shuts the PTY event loop down.
pub struct Session {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    shared: Arc<Shared>,
    pid: Option<u32>,
}

impl Session {
    /// Spawns the configured program in a new PTY.
    ///
    /// Returns the session and a receiver of [`SessionEvent`]s.
    pub fn spawn(
        cfg: SessionConfig,
    ) -> Result<(Session, UnboundedReceiver<SessionEvent>), TermError> {
        let cols = cfg.cols.max(1);
        let rows = cfg.rows.max(1);
        let (tx, rx) = unbounded();
        let shared = Arc::new(Shared {
            tx,
            wakeup_pending: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            size: AtomicU64::new(pack_size(cols, rows, cfg.cell_width, cfg.cell_height)),
            palette: cfg.palette.clone(),
            pty: OnceLock::new(),
        });
        let listener = Listener(shared.clone());

        let term_config = term::Config { scrolling_history: cfg.scrollback, ..Default::default() };
        let dims = TermSize { cols: cols as usize, rows: rows as usize };
        let term = Arc::new(FairMutex::new(Term::new(term_config, &dims, listener.clone())));

        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "Selachi".to_string());

        #[allow(clippy::needless_update)]
        let opts = tty::Options {
            shell: Some(tty::Shell::new(cfg.program.clone(), cfg.args.clone())),
            working_directory: cfg.cwd.clone(),
            drain_on_exit: true,
            env,
            #[cfg(windows)]
            escape_args: true,
            ..Default::default()
        };
        let window = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cfg.cell_width,
            cell_height: cfg.cell_height,
        };
        let pty = tty::new(&opts, window, 0)
            .map_err(|source| TermError::Pty { program: cfg.program.clone(), source })?;

        #[cfg(windows)]
        let pid = pty.child_watcher().pid().map(|p| p.get());
        #[cfg(not(windows))]
        let pid = None;

        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)
            .map_err(TermError::EventLoop)?;
        let sender = event_loop.channel();
        let _ = shared.pty.set(sender.clone());
        // The thread is detached; it ends on Shutdown or child exit.
        drop(event_loop.spawn());

        Ok((Session { term, sender, shared, pid }, rx))
    }

    /// Sends raw bytes (already encoded input, pasted text, ...) to the child's stdin.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        if let Err(e) = self.sender.send(Msg::Input(bytes)) {
            log::debug!("session write failed (event loop gone): {e}");
        }
    }

    /// Resizes both the emulator grid and the PTY. No-op if nothing changed.
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let packed = pack_size(cols, rows, cell_width, cell_height);
        if self.shared.size.swap(packed, Ordering::AcqRel) == packed {
            return;
        }
        self.term.lock().resize(TermSize { cols: cols as usize, rows: rows as usize });
        let window = WindowSize { num_lines: rows, num_cols: cols, cell_width, cell_height };
        if let Err(e) = self.sender.send(Msg::Resize(window)) {
            log::debug!("session resize failed (event loop gone): {e}");
        }
    }

    /// Copies the visible viewport. Also re-arms [`SessionEvent::Wakeup`] delivery.
    pub fn snapshot(&self) -> Snapshot {
        let mut snap = Snapshot::default();
        self.snapshot_into(&mut snap);
        snap
    }

    /// Like [`Session::snapshot`] but reuses the buffers of `out` (no allocation once warmed up).
    pub fn snapshot_into(&self, out: &mut Snapshot) {
        // Clear before locking so output arriving meanwhile produces a fresh Wakeup.
        self.shared.wakeup_pending.store(false, Ordering::Release);
        let term = self.term.lock();
        build_snapshot_into(&term, &self.shared.palette, out);
    }

    /// Scrolls the viewport by `delta_lines`; positive scrolls up into history.
    pub fn scroll(&self, delta_lines: i32) {
        if delta_lines != 0 {
            self.term.lock().scroll_display(Scroll::Delta(delta_lines));
        }
    }

    /// Scrolls the viewport back to the live bottom.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Process id of the child, if known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}
