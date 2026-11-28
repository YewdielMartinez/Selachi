//! A terminal session: PTY + VT emulator + event pump.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term, TermMode};
#[cfg(not(windows))]
use alacritty_terminal::tty;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::block::{
    Block, BlockEvent, BlockId, BlockTracker, Clock, term_history_cap, truncate_middle,
};
use crate::color::{Palette, color_by_table_index};
use crate::osc::{Mark, OscEvent};
use crate::pty_loop::{self, Hooks, Msg, PtySender};
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

/// Extra parameters for [`Session::spawn_with_options`].
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    /// Extra environment variables for the child (they win over the built-in `TERM`,
    /// `COLORTERM` and `TERM_PROGRAM` and over the inherited environment).
    pub env: Vec<(String, String)>,
    /// Raw handle of a Windows Job Object (for example `proc_svc::JobGroup::raw_handle()` or
    /// `ui_procs::OwnerJob::job_handle`, cast with `as isize`) the shell must be born in.
    ///
    /// The shell is created with `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so it and everything it starts
    /// later belong to the job from its first instruction. The handle is only used during the
    /// call (the kernel keeps its own reference), but it must be a valid job handle at that
    /// moment. If the job cannot be joined at birth the shell is started suspended, assigned and
    /// resumed instead (see [`JobAttachment`]); spawning never fails because of the job.
    /// Ignored on other platforms.
    pub job: Option<isize>,
}

impl SpawnOptions {
    /// Sets [`SpawnOptions::job`] from a raw Windows handle.
    #[cfg(windows)]
    #[must_use]
    pub fn with_job_handle(mut self, handle: std::os::windows::io::RawHandle) -> Self {
        self.job = Some(handle as isize);
        self
    }

    /// Adds one extra environment variable for the child (replaces an earlier one with the
    /// same name).
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        self.env.retain(|(k, _)| *k != key);
        self.env.push((key, value.into()));
        self
    }

    /// Makes the shell a parent of delegated agents: exports [`ENV_PARENT_TAB`] and
    /// [`ENV_PARENT_PERMISSION`] (see [`parent_env`]) so that `selachi-agent run` launched from
    /// this terminal hangs from the tab in the agent map.
    #[must_use]
    pub fn with_parent_tab(mut self, tab_id: impl std::fmt::Display, permission: &str) -> Self {
        self.env
            .retain(|(k, _)| k != ENV_PARENT_TAB && k != ENV_PARENT_PERMISSION);
        self.env.extend(parent_env(tab_id, permission));
        self
    }
}

/// Environment variable read by `selachi-agent` (`agent_harness::policy::ENV_PARENT_TAB`): opaque
/// id of the terminal tab that launched the run.
pub const ENV_PARENT_TAB: &str = "SELACHI_PARENT_TAB";
/// Environment variable read by `selachi-agent` (`agent_harness::policy::ENV_PARENT_PERMISSION`):
/// permission of the calling session (`read-only | ask | accept-edits | full`).
pub const ENV_PARENT_PERMISSION: &str = "SELACHI_PARENT_PERMISSION";
/// Permission exported when the tab has no explicit policy.
pub const DEFAULT_PARENT_PERMISSION: &str = "ask";

/// The `SELACHI_PARENT_TAB` / `SELACHI_PARENT_PERMISSION` pairs for a terminal tab, ready for
/// [`SpawnOptions::env`]. `tab_id` is rendered with `Display` (the app uses
/// `TabId::as_u64()`, e.g. `7`); a blank `permission` falls back to
/// [`DEFAULT_PARENT_PERMISSION`].
pub fn parent_env(tab_id: impl std::fmt::Display, permission: &str) -> Vec<(String, String)> {
    let permission = permission.trim();
    let permission = if permission.is_empty() {
        DEFAULT_PARENT_PERMISSION
    } else {
        permission
    };
    vec![
        (ENV_PARENT_TAB.to_string(), tab_id.to_string()),
        (ENV_PARENT_PERMISSION.to_string(), permission.to_string()),
    ]
}

/// How the shell of a [`Session`] relates to the Job Object passed in [`SpawnOptions::job`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobAttachment {
    /// No job was requested (or the platform has none).
    None,
    /// Created inside the job through `PROC_THREAD_ATTRIBUTE_JOB_LIST`.
    AtBirth,
    /// The job list was rejected; the shell was created suspended, assigned to the job and then
    /// resumed. Still no escapes possible.
    AssignedAfterSpawn,
    /// The job could not be joined at all: the shell runs outside it.
    AssignFailed,
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
    /// The shell reported its working directory (OSC 7; needs the shell integration).
    Cwd(PathBuf),
    /// A command finished (OSC 133 `D`); `exit_code` is `None` if the shell did not report one.
    CommandFinished {
        /// Exit code of the command.
        exit_code: Option<i32>,
    },
    /// A command started running (OSC 133 `C`); see [`Session::blocks`].
    ///
    /// Only present with the `block-events` cargo feature (off by default so that crates that
    /// match `SessionEvent` exhaustively keep compiling; enable it together with the new arms).
    #[cfg(feature = "block-events")]
    BlockStarted(BlockId),
    /// A command finished; sent right after [`SessionEvent::CommandFinished`] (also when a new
    /// prompt closes a command that never reported `D`, with `exit_code: None`).
    ///
    /// Only present with the `block-events` cargo feature.
    #[cfg(feature = "block-events")]
    BlockFinished {
        /// The block that finished.
        id: BlockId,
        /// Exit code, if reported.
        exit_code: Option<i32>,
        /// How long it ran.
        duration: std::time::Duration,
    },
}

/// State shared between the session handle, the event loop thread and the listener.
struct Shared {
    tx: UnboundedSender<SessionEvent>,
    /// True while a `Wakeup` has been sent but not yet consumed by `snapshot()`.
    wakeup_pending: AtomicBool,
    exited: AtomicBool,
    /// Packed `cols | rows << 16 | cell_w << 32 | cell_h << 48`.
    size: AtomicU64,
    /// Current palette; swapped by [`Session::set_palette`], read on every snapshot.
    palette: Mutex<Arc<Palette>>,
    /// PTY sender, set once the event loop exists (needed to answer `PtyWrite`).
    pty: OnceLock<PtySender>,
    /// Command blocks. Only mutated by the PTY thread while the terminal lock is held; lock
    /// order is always terminal first, tracker second.
    blocks: Mutex<BlockTracker>,
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

    fn palette(&self) -> Arc<Palette> {
        Arc::clone(&self.palette.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn tracker(&self) -> std::sync::MutexGuard<'_, BlockTracker> {
        self.blocks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Turns what the OSC scanner found on the PTY stream into session events and block updates.
    /// Runs on the PTY thread with the terminal locked and parsed up to this sequence.
    fn on_osc(&self, term: &Term<Listener>, event: OscEvent) {
        match &event {
            OscEvent::Cwd(path) => self.emit(SessionEvent::Cwd(path.clone())),
            OscEvent::Mark(Mark::Finished(exit_code)) => {
                self.emit(SessionEvent::CommandFinished {
                    exit_code: *exit_code,
                });
            }
            OscEvent::Mark(Mark::PromptStart | Mark::CommandStart | Mark::Executed)
            | OscEvent::Command(_) => {}
        }
        let events = self.tracker().on_osc(term, event, Clock::now());
        for event in events {
            match event {
                #[cfg(feature = "block-events")]
                BlockEvent::Started(id) => self.emit(SessionEvent::BlockStarted(id)),
                #[cfg(feature = "block-events")]
                BlockEvent::Finished {
                    id,
                    exit_code,
                    duration,
                } => {
                    self.emit(SessionEvent::BlockFinished {
                        id,
                        exit_code,
                        duration,
                    });
                }
                #[cfg(not(feature = "block-events"))]
                BlockEvent::Started(_) | BlockEvent::Finished { .. } => {}
            }
        }
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
            }
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
            }
            Event::Title(t) => s.emit(SessionEvent::Title(t)),
            Event::ResetTitle => s.emit(SessionEvent::Title(String::new())),
            Event::Bell => s.emit(SessionEvent::Bell),
            Event::Exit | Event::ChildExit(_) => {
                if !s.exited.swap(true, Ordering::AcqRel) {
                    s.emit(SessionEvent::Exit);
                }
            }
            // ConPTY sends DSR queries on startup; shells stall without the reply.
            Event::PtyWrite(text) => s.write_pty(text.into_bytes()),
            Event::ColorRequest(index, format) => {
                // The term lock is held while this runs, so only the base palette is used here.
                if let Some(c) = color_by_table_index(&s.palette(), index) {
                    s.write_pty(format(c.into()).into_bytes());
                }
            }
            Event::TextAreaSizeRequest(format) => {
                let size = unpack_size(s.size.load(Ordering::Acquire));
                s.write_pty(format(size).into_bytes());
            }
            Event::ClipboardStore(..) | Event::ClipboardLoad(..) => {
                log::debug!("clipboard request ignored");
            }
            Event::MouseCursorDirty | Event::CursorBlinkingChange => {}
        }
    }
}

impl Hooks for Listener {
    fn osc(&self, term: &mut Term<Self>, event: OscEvent) {
        self.0.on_osc(term, event);
    }

    fn after_slice(&self, term: &mut Term<Self>) {
        self.0.tracker().after_slice(term);
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
    sender: PtySender,
    shared: Arc<Shared>,
    pid: Option<u32>,
    attachment: JobAttachment,
}

impl Session {
    /// Spawns the configured program in a new PTY.
    ///
    /// Returns the session and a receiver of [`SessionEvent`]s.
    pub fn spawn(
        cfg: SessionConfig,
    ) -> Result<(Session, UnboundedReceiver<SessionEvent>), TermError> {
        Self::spawn_with_env(cfg, Vec::new())
    }

    /// Like [`Session::spawn`], with extra environment variables for the child (they win over
    /// the built-in `TERM`, `COLORTERM` and `TERM_PROGRAM`). The shell integration reads the
    /// completion settings from here, see `store::Config::completion_env`.
    pub fn spawn_with_env(
        cfg: SessionConfig,
        extra_env: Vec<(String, String)>,
    ) -> Result<(Session, UnboundedReceiver<SessionEvent>), TermError> {
        Self::spawn_with_options(
            cfg,
            SpawnOptions {
                env: extra_env,
                job: None,
            },
        )
    }

    /// Like [`Session::spawn_with_env`], and can start the shell inside a Windows Job Object so
    /// that every process it launches is tracked (see [`SpawnOptions::job`]).
    ///
    /// [`Session::pid`] is the real shell pid, ready for `proc_svc::Tracker::register_root`.
    pub fn spawn_with_options(
        cfg: SessionConfig,
        options: SpawnOptions,
    ) -> Result<(Session, UnboundedReceiver<SessionEvent>), TermError> {
        let SpawnOptions {
            env: extra_env,
            job,
        } = options;
        let cols = cfg.cols.max(1);
        let rows = cfg.rows.max(1);
        let (tx, rx) = unbounded();
        let shared = Arc::new(Shared {
            tx,
            wakeup_pending: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            size: AtomicU64::new(pack_size(cols, rows, cfg.cell_width, cfg.cell_height)),
            palette: Mutex::new(Arc::new(cfg.palette.clone())),
            pty: OnceLock::new(),
            blocks: Mutex::new(BlockTracker::new(cfg.scrollback)),
        });
        let listener = Listener(shared.clone());

        // The terminal may hold more history than the user asked for; the PTY thread trims it
        // back after every parsed slice so absolute line numbers stay exact (see `block.rs`).
        let term_config = term::Config {
            scrolling_history: term_history_cap(cfg.scrollback),
            ..Default::default()
        };
        let dims = TermSize {
            cols: cols as usize,
            rows: rows as usize,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &dims,
            listener.clone(),
        )));

        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "Selachi".to_string());
        env.extend(extra_env);

        let window = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cfg.cell_width,
            cell_height: cfg.cell_height,
        };
        let pty_err = |source| TermError::Pty {
            program: cfg.program.clone(),
            source,
        };

        #[cfg(windows)]
        let (pty, pid, attachment) = {
            let env: Vec<(String, String)> = env.into_iter().collect();
            let pty = crate::conpty::spawn(&crate::conpty::SpawnSpec {
                program: &cfg.program,
                args: &cfg.args,
                cwd: cfg.cwd.as_deref(),
                env: &env,
                job,
                size: window,
                force_job_fallback: false,
            })
            .map_err(pty_err)?;
            let (pid, attachment) = (Some(pty.pid()), pty.attachment());
            (pty, pid, attachment)
        };
        #[cfg(not(windows))]
        let (pty, pid, attachment) = {
            let _ = job;
            #[allow(clippy::needless_update)]
            let opts = tty::Options {
                shell: Some(tty::Shell::new(cfg.program.clone(), cfg.args.clone())),
                working_directory: cfg.cwd.clone(),
                drain_on_exit: true,
                env,
                ..Default::default()
            };
            let pty = tty::new(&opts, window, 0).map_err(pty_err)?;
            (pty, None, JobAttachment::None)
        };

        let sender = pty_loop::spawn(term.clone(), listener, pty, |sender| {
            let _ = shared.pty.set(sender.clone());
        })
        .map_err(TermError::EventLoop)?;

        Ok((
            Session {
                term,
                sender,
                shared,
                pid,
                attachment,
            },
            rx,
        ))
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
        {
            let mut term = self.term.lock();
            let cols_changed = term.columns() != cols as usize;
            term.resize(TermSize {
                cols: cols as usize,
                rows: rows as usize,
            });
            // A width change reflows the text and invalidates the stored block lines.
            self.shared
                .tracker()
                .after_resize(term.grid().history_size(), cols_changed);
        }
        let window = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width,
            cell_height,
        };
        if let Err(e) = self.sender.send(Msg::Resize(window)) {
            log::debug!("session resize failed (event loop gone): {e}");
        }
    }

    /// Replaces the color palette. Cells are re-resolved on the next [`Session::snapshot`], so
    /// the caller only has to repaint. Colors set explicitly by programs (OSC 4/10/11) and
    /// truecolor cells are not affected.
    pub fn set_palette(&self, palette: Palette) {
        *self
            .shared
            .palette
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(palette);
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
        let palette = self.shared.palette();
        let term = self.term.lock();
        build_snapshot_into(&term, &palette, out);
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

    /// The grid point under viewport cell `(row, col)`, clamped to the screen.
    fn viewport_point(term: &Term<Listener>, row: usize, col: usize) -> Point {
        let row = row.min(term.screen_lines().saturating_sub(1));
        let col = col.min(term.columns().saturating_sub(1));
        Point::new(
            Line(row as i32 - term.grid().display_offset() as i32),
            Column(col),
        )
    }

    /// Starts a selection at viewport cell `(row, col)`; `right` is the right half of the cell.
    /// It follows the text when the output scrolls. Replaces the previous selection.
    pub fn selection_start(&self, row: usize, col: usize, right: bool) {
        let mut term = self.term.lock();
        let point = Self::viewport_point(&term, row, col);
        let side = if right { Side::Right } else { Side::Left };
        term.selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    /// Extends the selection to viewport cell `(row, col)` (no-op without a selection).
    pub fn selection_update(&self, row: usize, col: usize, right: bool) {
        let mut term = self.term.lock();
        let point = Self::viewport_point(&term, row, col);
        let side = if right { Side::Right } else { Side::Left };
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Drops the selection.
    pub fn selection_clear(&self) {
        self.term.lock().selection = None;
    }

    /// The selected text (`None` when nothing, or only an empty range, is selected).
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock();
        term.selection
            .as_ref()
            .filter(|s| !s.is_empty())
            .and_then(|_| term.selection_to_string())
            .filter(|t| !t.is_empty())
    }

    /// Process id of the shell, if known (on Windows always set after a successful spawn).
    ///
    /// This is the pid to hand to `proc_svc::Tracker::register_root`.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// How the shell joined the job given in [`SpawnOptions::job`].
    pub fn job_attachment(&self) -> JobAttachment {
        self.attachment
    }

    /// The command blocks of this session, oldest first (at most the last 500).
    ///
    /// Needs the shell integration (OSC 133). Cheap: clones a bounded list.
    pub fn blocks(&self) -> Vec<Block> {
        self.shared.tracker().blocks()
    }

    /// The block shown at viewport row `viewport_row` (0 = top row of what is on screen, so it
    /// accounts for scrolling). `None` outside any block and while a full-screen app runs.
    pub fn block_at_line(&self, viewport_row: usize) -> Option<BlockId> {
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = term.grid();
        let tracker = self.shared.tracker();
        let abs =
            tracker.abs_of_viewport_row(grid.history_size(), grid.display_offset(), viewport_row);
        tracker.block_at_abs(abs)
    }

    /// Plain text of the output of block `id`, trailing whitespace trimmed. When it is longer
    /// than `max_bytes` the middle is replaced by a `… N líneas omitidas …` marker. `None` if the
    /// block is unknown (or already trimmed away) or a full-screen app is showing.
    pub fn block_output_text(&self, id: BlockId, max_bytes: usize) -> Option<String> {
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let tracker = self.shared.tracker();
        let block = tracker.get(id)?;
        let end = tracker.output_limit(&term, block);
        let text = tracker.text_between(&term, block.output_start, end);
        Some(truncate_middle(&text, max_bytes))
    }

    /// Scrolls the viewport so the prompt of block `id` is on the first row (as far as the
    /// scrollback allows). Returns `false` if the block is unknown.
    pub fn scroll_to_block(&self, id: BlockId) -> bool {
        let mut term = self.term.lock();
        let (hist, offset) = (term.grid().history_size(), term.grid().display_offset());
        let wanted = {
            let tracker = self.shared.tracker();
            let Some(block) = tracker.get(id) else {
                return false;
            };
            tracker.offset_for_top(hist, block.prompt_line)
        };
        let wanted = wanted.clamp(0, hist as i64);
        let delta = wanted - offset as i64;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta as i32));
        }
        true
    }

    /// The block before `from`; with `None`, the last block that starts above the viewport.
    pub fn prev_block(&self, from: Option<BlockId>) -> Option<BlockId> {
        let (tracker, top) = self.navigation_anchor();
        tracker.prev(from, top)
    }

    /// The block after `from`; with `None`, the first block that starts below the viewport top.
    pub fn next_block(&self, from: Option<BlockId>) -> Option<BlockId> {
        let (tracker, top) = self.navigation_anchor();
        tracker.next(from, top)
    }

    /// Tracker lock plus the absolute line at the top of the viewport.
    fn navigation_anchor(&self) -> (std::sync::MutexGuard<'_, BlockTracker>, u64) {
        let term = self.term.lock();
        let grid = term.grid();
        let tracker = self.shared.tracker();
        let top = tracker.abs_of_viewport_row(grid.history_size(), grid.display_offset(), 0);
        (tracker, top)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}
