//! The PTY I/O loop, adapted from `alacritty_terminal::event_loop` (Apache-2.0).
//!
//! Why not use alacritty's loop: the command-block model needs the cursor position *at the
//! exact byte* where an OSC 133 mark appears. Alacritty's loop parses whole buffers, so by the
//! time anyone could look at the grid the cursor has moved on. Here [`Ingest::feed`] cuts the
//! stream at every OSC sequence the [`OscScanner`] recognizes, parses up to that byte, and calls
//! [`Hooks::osc`] with the terminal locked and in precisely that state. Everything else (polling,
//! write queue, child exit, synchronized updates, lock fairness) behaves like the original.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty::{ChildEvent, EventedPty};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use polling::{Event as PollingEvent, Events, PollMode, Poller};

use crate::osc::{OscEvent, OscScanner};

/// Bytes read from the PTY before the terminal is force-synchronized.
const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Bytes read while the terminal is locked before yielding it to other threads.
const MAX_LOCKED_READ: usize = u16::MAX as usize;
/// The parser is fed in slices of this size so that scrollback trimming (see
/// [`Hooks::after_slice`]) keeps the grid bounded even under `yes`-style floods.
const PARSE_SLICE: usize = 8 * 1024;
/// Poll keys used by `alacritty_terminal`'s PTYs (the Windows read/write key is 2, Unix 0).
#[cfg(windows)]
const PTY_READ_WRITE_TOKEN: usize = 2;
#[cfg(not(windows))]
const PTY_READ_WRITE_TOKEN: usize = 0;
const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// Messages for the loop.
#[derive(Debug)]
pub(crate) enum Msg {
    /// Bytes for the child's stdin.
    Input(Cow<'static, [u8]>),
    /// Resize the PTY.
    Resize(WindowSize),
    /// Stop the loop.
    Shutdown,
}

/// Handle used to talk to the loop from any thread.
#[derive(Clone)]
pub(crate) struct PtySender {
    tx: Sender<Msg>,
    poller: Arc<Poller>,
}

impl PtySender {
    pub(crate) fn send(&self, msg: Msg) -> io::Result<()> {
        self.tx
            .send(msg)
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "pty loop is gone"))?;
        self.poller.notify()
    }
}

/// What the loop calls while it owns the locked terminal.
pub(crate) trait Hooks: EventListener + Send + 'static {
    /// An OSC sequence (7, 133, 633;E) just ended; the terminal has parsed everything up to and
    /// including it.
    fn osc(&self, term: &mut Term<Self>, event: OscEvent)
    where
        Self: Sized;

    /// A slice of at most [`PARSE_SLICE`] bytes was parsed (also called after the last OSC).
    fn after_slice(&self, term: &mut Term<Self>)
    where
        Self: Sized;
}

/// Parser plus OSC scanner: turns raw PTY bytes into terminal state and OSC callbacks.
#[derive(Default)]
pub(crate) struct Ingest {
    parser: Processor<StdSyncHandler>,
    scanner: OscScanner,
}

impl Ingest {
    /// Parses `bytes`, calling the hooks at every OSC boundary and after every slice.
    pub(crate) fn feed<H: Hooks>(&mut self, term: &mut Term<H>, hooks: &H, bytes: &[u8]) {
        for slice in bytes.chunks(PARSE_SLICE) {
            let mut rest = slice;
            while let Some((end, event)) = self.scanner.next_event(rest) {
                self.parser.advance(term, &rest[..end]);
                hooks.osc(term, event);
                rest = &rest[end..];
            }
            self.parser.advance(term, rest);
            hooks.after_slice(term);
        }
    }

    fn stop_sync<H: Hooks>(&mut self, term: &mut Term<H>) {
        self.parser.stop_sync(term);
    }

    fn sync_bytes_count(&self) -> usize {
        self.parser.sync_bytes_count()
    }

    fn sync_timeout(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }
}

/// Starts the loop thread. The returned sender is the only way to feed or stop it.
///
/// `ready` receives the sender before the thread starts, so replies to terminal queries that
/// arrive immediately can already be written back.
pub(crate) fn spawn<P, H>(
    term: Arc<FairMutex<Term<H>>>,
    hooks: H,
    pty: P,
    ready: impl FnOnce(&PtySender),
) -> io::Result<PtySender>
where
    P: EventedPty + OnResize + Send + 'static,
    H: Hooks,
{
    let poller: Arc<Poller> = Poller::new()?.into();
    let (tx, rx) = mpsc::channel();
    let sender = PtySender {
        tx,
        poller: poller.clone(),
    };
    ready(&sender);
    std::thread::Builder::new()
        .name("selachi-pty".to_string())
        .spawn(move || run(pty, &term, &hooks, &poller, &rx))?;
    Ok(sender)
}

struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

#[derive(Default)]
struct WriteState {
    list: VecDeque<Cow<'static, [u8]>>,
    current: Option<Writing>,
}

impl WriteState {
    fn ensure_next(&mut self) {
        if self.current.is_none() {
            self.goto_next();
        }
    }

    fn goto_next(&mut self) {
        self.current = self
            .list
            .pop_front()
            .map(|source| Writing { source, written: 0 });
    }

    fn needs_write(&self) -> bool {
        self.current.is_some() || !self.list.is_empty()
    }
}

fn run<P, H>(
    mut pty: P,
    term: &FairMutex<Term<H>>,
    hooks: &H,
    poller: &Arc<Poller>,
    rx: &Receiver<Msg>,
) where
    P: EventedPty + OnResize,
    H: Hooks,
{
    let mut ingest = Ingest::default();
    let mut writes = WriteState::default();
    let mut buf = vec![0u8; READ_BUFFER_SIZE];
    let mut interest = PollingEvent::readable(PTY_READ_WRITE_TOKEN);
    let mode = PollMode::Level;

    // SAFETY: `pty` outlives its registration: it is deregistered below before it is dropped,
    // and it is not moved while registered.
    if let Err(err) = unsafe { pty.register(poller, interest, mode) } {
        log::error!("pty registration failed: {err}");
        return;
    }
    let mut events = Events::with_capacity(NonZeroUsize::MIN.saturating_add(1023));

    'event_loop: loop {
        let timeout = ingest
            .sync_timeout()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        events.clear();
        if let Err(err) = poller.wait(&mut events, timeout) {
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            log::error!("pty polling failed: {err}");
            break;
        }

        // Messages first.
        let mut got_message = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::Input(bytes)) => {
                    writes.list.push_back(bytes);
                    got_message = true;
                }
                Ok(Msg::Resize(size)) => {
                    pty.on_resize(size);
                    got_message = true;
                }
                Ok(Msg::Shutdown) | Err(TryRecvError::Disconnected) => break 'event_loop,
                Err(TryRecvError::Empty) => break,
            }
        }

        // A synchronized-update timeout woke us: flush what was buffered.
        if events.is_empty() && !got_message {
            ingest.stop_sync(&mut term.lock());
            hooks.send_event(Event::Wakeup);
            continue;
        }

        for event in events.iter() {
            match event.key {
                PTY_CHILD_EVENT_TOKEN => {
                    if let Some(ChildEvent::Exited(status)) = pty.next_child_event() {
                        if let Some(status) = status {
                            hooks.send_event(Event::ChildExit(status));
                        }
                        let _ = pty_read(&mut pty, term, hooks, &mut ingest, &mut buf);
                        term.lock().exit();
                        hooks.send_event(Event::Wakeup);
                        break 'event_loop;
                    }
                }
                PTY_READ_WRITE_TOKEN => {
                    if event.is_interrupt() {
                        continue;
                    }
                    if event.readable
                        && let Err(err) = pty_read(&mut pty, term, hooks, &mut ingest, &mut buf)
                    {
                        log::error!("error reading from the pty: {err}");
                        break 'event_loop;
                    }
                    if event.writable
                        && let Err(err) = pty_write(&mut pty, &mut writes)
                    {
                        log::error!("error writing to the pty: {err}");
                        break 'event_loop;
                    }
                }
                _ => {}
            }
        }

        let needs_write = writes.needs_write();
        if needs_write != interest.writable {
            interest.writable = needs_write;
            if let Err(err) = pty.reregister(poller, interest, mode) {
                log::error!("pty reregistration failed: {err}");
                break;
            }
        }
    }

    let _ = pty.deregister(poller);
}

/// Reads everything the PTY has and parses it under the terminal lock.
fn pty_read<P, H>(
    pty: &mut P,
    term: &FairMutex<Term<H>>,
    hooks: &H,
    ingest: &mut Ingest,
    buf: &mut [u8],
) -> io::Result<()>
where
    P: EventedPty,
    H: Hooks,
{
    let mut unprocessed = 0;
    let mut processed = 0;

    // Reserve the next terminal lock for PTY reading.
    let _lease = term.lease();
    let mut guard = None;

    loop {
        match pty.reader().read(&mut buf[unprocessed..]) {
            // Windows/macOS: nothing more to read.
            Ok(0) if unprocessed == 0 => break,
            Ok(got) => unprocessed += got,
            Err(err) => match err.kind() {
                ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                    if unprocessed == 0 {
                        break;
                    }
                }
                _ => return Err(err),
            },
        }

        let terminal = match &mut guard {
            Some(terminal) => terminal,
            None => guard.insert(match term.try_lock_unfair() {
                // Force a blocking lock once the buffer is full.
                None if unprocessed >= buf.len() => term.lock_unfair(),
                None => continue,
                Some(terminal) => terminal,
            }),
        };

        ingest.feed(&mut **terminal, hooks, &buf[..unprocessed]);

        processed += unprocessed;
        unprocessed = 0;

        // Do not hold the terminal for too long.
        if processed >= MAX_LOCKED_READ {
            break;
        }
    }

    // Queue a redraw unless everything was held back by a synchronized update.
    if ingest.sync_bytes_count() < processed && processed > 0 {
        hooks.send_event(Event::Wakeup);
    }
    Ok(())
}

fn pty_write<P: EventedPty>(pty: &mut P, state: &mut WriteState) -> io::Result<()> {
    state.ensure_next();

    'write_many: while let Some(mut current) = state.current.take() {
        loop {
            match pty.writer().write(&current.source[current.written..]) {
                Ok(0) => {
                    state.current = Some(current);
                    break 'write_many;
                }
                Ok(n) => {
                    current.written += n;
                    if current.written >= current.source.len() {
                        state.goto_next();
                        break;
                    }
                }
                Err(err) => {
                    state.current = Some(current);
                    match err.kind() {
                        ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                        _ => return Err(err),
                    }
                }
            }
        }
    }
    Ok(())
}
