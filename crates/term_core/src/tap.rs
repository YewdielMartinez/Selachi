//! A PTY wrapper that lets [`OscScanner`](crate::osc::OscScanner) see every byte the child
//! writes, before `alacritty_terminal` parses it.
//!
//! `alacritty_terminal`'s event loop owns the PTY and has no hook for OSC 7 / OSC 133, but it is
//! generic over [`EventedPty`]. [`TappedPty`] delegates everything to the real PTY and reads
//! through itself, so the scanner observes the exact stream (in order, once) that the terminal
//! parses. The bytes themselves are passed through untouched.

use std::io;
use std::sync::Arc;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

use crate::osc::{OscEvent, OscScanner};

/// Receives what the scanner finds, on the PTY reader thread.
pub(crate) type Sink = Box<dyn FnMut(OscEvent) + Send>;

pub(crate) struct TappedPty<P> {
    pty: P,
    scanner: OscScanner,
    sink: Sink,
}

impl<P> TappedPty<P> {
    pub(crate) fn new(pty: P, sink: Sink) -> Self {
        Self { pty, scanner: OscScanner::default(), sink }
    }
}

impl<P: EventedPty> io::Read for TappedPty<P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.pty.reader().read(buf)?;
        if n > 0 {
            let sink = &mut self.sink;
            self.scanner.feed(&buf[..n], |event| sink(event));
        }
        Ok(n)
    }
}

impl<P: EventedPty> EventedReadWrite for TappedPty<P> {
    type Reader = Self;
    type Writer = P::Writer;

    unsafe fn register(&mut self, poll: &Arc<Poller>, event: Event, mode: PollMode) -> io::Result<()> {
        // SAFETY: the caller's contract (sources outlive their registration) is forwarded as is;
        // the wrapped PTY is owned by `self` and not moved while registered.
        unsafe { self.pty.register(poll, event, mode) }
    }

    fn reregister(&mut self, poll: &Arc<Poller>, event: Event, mode: PollMode) -> io::Result<()> {
        self.pty.reregister(poll, event, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self::Reader {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.pty.writer()
    }
}

impl<P: EventedPty> EventedPty for TappedPty<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl<P: OnResize> OnResize for TappedPty<P> {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.pty.on_resize(window_size);
    }
}
