//! Backpressure for the event stream: consecutive streaming text / thinking deltas
//! are merged in the reader thread so the UI sees a handful of events per frame
//! instead of one per token.
//!
//! The public channel stays unbounded; this only reduces how many events enter it.
//! Ordering is preserved: the pending delta is flushed before any other event,
//! before a delta of a different kind, when it grows past [`MAX_BYTES`], when it
//! has been held for [`WINDOW`], when the stream goes idle, and at end of stream.

use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;

use crate::types::AgentEvent;

/// Longest a delta is held back waiting for its successors.
pub const WINDOW: Duration = Duration::from_millis(16);
/// Flush as soon as the accumulated delta reaches this many bytes.
pub const MAX_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Text,
    Thinking,
}

struct Held {
    kind: Kind,
    text: String,
    since: Instant,
}

/// Merges consecutive same-kind deltas and forwards everything to `tx` in order.
pub struct Coalescer {
    tx: UnboundedSender<AgentEvent>,
    held: Option<Held>,
    window: Duration,
    max_bytes: usize,
}

impl Coalescer {
    pub fn new(tx: UnboundedSender<AgentEvent>) -> Self {
        Self::with_limits(tx, WINDOW, MAX_BYTES)
    }

    pub fn with_limits(
        tx: UnboundedSender<AgentEvent>,
        window: Duration,
        max_bytes: usize,
    ) -> Self {
        Self {
            tx,
            held: None,
            window,
            max_bytes,
        }
    }

    /// Queue `ev`. Returns `false` once the receiver is gone.
    pub fn push(&mut self, ev: AgentEvent) -> bool {
        let (kind, text) = match ev {
            AgentEvent::Text { text, delta: true } => (Kind::Text, text),
            AgentEvent::Thinking { text, delta: true } => (Kind::Thinking, text),
            other => return self.flush() && self.tx.unbounded_send(other).is_ok(),
        };
        if self.held.as_ref().is_some_and(|h| h.kind != kind) && !self.flush() {
            return false;
        }
        match &mut self.held {
            Some(h) => h.text.push_str(&text),
            None => {
                self.held = Some(Held {
                    kind,
                    text,
                    since: Instant::now(),
                })
            }
        }
        let full = self
            .held
            .as_ref()
            .is_some_and(|h| h.text.len() >= self.max_bytes);
        if full || self.is_due() {
            return self.flush();
        }
        true
    }

    /// Send the held delta, if any. Returns `false` once the receiver is gone.
    pub fn flush(&mut self) -> bool {
        match self.held.take() {
            None => true,
            Some(h) => {
                let ev = match h.kind {
                    Kind::Text => AgentEvent::Text {
                        text: h.text,
                        delta: true,
                    },
                    Kind::Thinking => AgentEvent::Thinking {
                        text: h.text,
                        delta: true,
                    },
                };
                self.tx.unbounded_send(ev).is_ok()
            }
        }
    }

    fn is_due(&self) -> bool {
        self.held
            .as_ref()
            .is_some_and(|h| h.since.elapsed() >= self.window)
    }

    /// How long the reader may block before the held delta must be flushed
    /// (`None`: nothing held, block indefinitely).
    pub fn timeout(&self) -> Option<Duration> {
        self.held
            .as_ref()
            .map(|h| self.window.saturating_sub(h.since.elapsed()))
    }
}

/// Read `src` line by line on a helper thread and forward each line to the returned
/// channel; the channel disconnects at EOF or on a read error. This lets [`pump`]
/// wake up on a timer even while the child is silent.
pub fn spawn_line_reader(
    name: &str,
    src: impl Read + Send + 'static,
) -> std::io::Result<Receiver<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            for line in BufReader::new(src).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return; // pump gone
                }
            }
        })?;
    Ok(rx)
}

/// Drive `lines` until the sender disconnects (EOF), turning each line into events via
/// `handle`, coalescing deltas and flushing on idle. Returns `false` if the event
/// receiver was dropped (the caller should stop), `true` on clean EOF with everything
/// flushed.
pub fn pump(
    lines: &Receiver<String>,
    co: &mut Coalescer,
    mut handle: impl FnMut(&str) -> Vec<AgentEvent>,
) -> bool {
    loop {
        let next = match co.timeout() {
            Some(t) => lines.recv_timeout(t),
            None => lines.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(line) => {
                for ev in handle(&line) {
                    if !co.push(ev) {
                        return false;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !co.flush() {
                    return false;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return co.flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentStatus;
    use futures::channel::mpsc::{unbounded, UnboundedReceiver};
    use std::sync::mpsc::channel;

    fn text(s: &str) -> AgentEvent {
        AgentEvent::Text {
            text: s.into(),
            delta: true,
        }
    }

    fn drain(rx: &mut UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut v = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            v.push(ev);
        }
        v
    }

    fn slow() -> Duration {
        Duration::from_secs(3600)
    }

    #[test]
    fn merges_consecutive_deltas_into_one_event() {
        let (tx, mut rx) = unbounded();
        let mut co = Coalescer::with_limits(tx, slow(), MAX_BYTES);
        for i in 0..100 {
            assert!(co.push(text(&format!("{i},"))));
        }
        assert!(drain(&mut rx).is_empty(), "held until flush");
        assert!(co.flush());
        let evs = drain(&mut rx);
        let expected: String = (0..100).map(|i| format!("{i},")).collect();
        assert_eq!(
            evs,
            vec![AgentEvent::Text {
                text: expected,
                delta: true
            }]
        );
    }

    #[test]
    fn non_delta_event_flushes_first_and_order_is_preserved() {
        let (tx, mut rx) = unbounded();
        let mut co = Coalescer::with_limits(tx, slow(), MAX_BYTES);
        co.push(text("a"));
        co.push(text("b"));
        co.push(AgentEvent::Thinking {
            text: "t".into(),
            delta: true,
        }); // kind switch
        co.push(AgentEvent::Text {
            text: "full".into(),
            delta: false,
        }); // not a delta
        co.push(text("c"));
        co.flush();
        assert_eq!(
            drain(&mut rx),
            vec![
                text("ab"),
                AgentEvent::Thinking {
                    text: "t".into(),
                    delta: true
                },
                AgentEvent::Text {
                    text: "full".into(),
                    delta: false
                },
                text("c"),
            ]
        );
    }

    #[test]
    fn flushes_when_size_cap_is_reached() {
        let (tx, mut rx) = unbounded();
        let mut co = Coalescer::with_limits(tx, slow(), 10);
        co.push(text("12345"));
        assert!(drain(&mut rx).is_empty());
        co.push(text("67890"));
        assert_eq!(drain(&mut rx), vec![text("1234567890")]);
    }

    #[test]
    fn flushes_when_window_elapsed() {
        let (tx, mut rx) = unbounded();
        let mut co = Coalescer::with_limits(tx, Duration::from_millis(5), MAX_BYTES);
        co.push(text("a"));
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(co.timeout(), Some(Duration::ZERO));
        co.push(text("b"));
        assert_eq!(drain(&mut rx), vec![text("ab")]);
    }

    #[test]
    fn pump_flushes_on_idle_and_on_eof() {
        let (tx, mut rx) = unbounded();
        let (ltx, lrx) = channel::<String>();
        let t = std::thread::spawn(move || {
            let mut co = Coalescer::with_limits(tx, Duration::from_millis(100), MAX_BYTES);
            pump(&lrx, &mut co, |l| match l {
                "tool" => vec![AgentEvent::Status(AgentStatus::Working)],
                other => vec![text(other)],
            })
        });
        ltx.send("a".into()).unwrap();
        ltx.send("b".into()).unwrap();
        // Idle (sender still open): the held delta must still arrive.
        let first = futures::executor::block_on(futures::StreamExt::next(&mut rx));
        assert_eq!(first, Some(text("ab")));
        ltx.send("c".into()).unwrap();
        ltx.send("tool".into()).unwrap();
        ltx.send("d".into()).unwrap();
        drop(ltx); // EOF
        assert!(t.join().unwrap());
        assert_eq!(
            drain(&mut rx),
            vec![
                text("c"),
                AgentEvent::Status(AgentStatus::Working),
                text("d")
            ]
        );
    }

    #[test]
    fn push_reports_dropped_receiver() {
        let (tx, rx) = unbounded();
        drop(rx);
        let mut co = Coalescer::new(tx);
        assert!(!co.push(AgentEvent::Status(AgentStatus::Done)));
    }
}
