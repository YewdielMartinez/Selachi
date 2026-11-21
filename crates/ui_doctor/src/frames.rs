//! Frame timing. gpui exposes no frame statistics, so the shell measures itself with two zero-size
//! probe elements: [`frame_begin`] as the **first** child of the window root and [`frame_end`] as
//! the **last**. Everything between them (prepaint and paint of the whole tree) is timed and
//! recorded in a shared [`FrameStats`]. Recording is atomics only: no entity update, no notify, so
//! measuring never causes a repaint.
//!
//! The number is CPU time spent preparing and painting the frame (layout is excluded, GPU time is
//! not visible to gpui). Apps that can time frames some other way call
//! [`DoctorModel::record_frame`](crate::DoctorModel::record_frame) instead.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{IntoElement, Styled, canvas, px};

/// Shared, lock-free frame counters.
#[derive(Debug, Default)]
pub struct FrameStats {
    frames: AtomicU64,
    last_ns: AtomicU64,
    slowest_ns: AtomicU64,
}

impl FrameStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records one painted frame that took `d`.
    pub fn record(&self, d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.last_ns.store(ns, Ordering::Relaxed);
        self.slowest_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Frames recorded since the process started.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The last frame, if any was recorded.
    pub fn last(&self) -> Option<Duration> {
        (self.frames() > 0).then(|| Duration::from_nanos(self.last_ns.load(Ordering::Relaxed)))
    }

    /// Slowest frame since the previous call (and resets it).
    pub fn take_slowest(&self) -> Option<Duration> {
        let ns = self.slowest_ns.swap(0, Ordering::Relaxed);
        (ns > 0).then(|| Duration::from_nanos(ns))
    }
}

thread_local! {
    static START: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Zero-size probe: put it first in the root element.
pub fn frame_begin() -> impl IntoElement {
    canvas(|_, _, _| START.with(|s| s.set(Some(Instant::now()))), |_, (), _, _| {}).size(px(0.0))
}

/// Zero-size probe: put it last in the root element.
pub fn frame_end(stats: Arc<FrameStats>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, (), _, _| {
            if let Some(t) = START.with(Cell::take) {
                stats.record(t.elapsed());
            }
        },
    )
    .size(px(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_count_last_and_slowest() {
        let s = FrameStats::new();
        assert_eq!((s.frames(), s.last(), s.take_slowest()), (0, None, None));
        s.record(Duration::from_millis(2));
        s.record(Duration::from_millis(9));
        s.record(Duration::from_millis(3));
        assert_eq!(s.frames(), 3);
        assert_eq!(s.last(), Some(Duration::from_millis(3)));
        assert_eq!(s.take_slowest(), Some(Duration::from_millis(9)));
        assert_eq!(s.take_slowest(), None, "reset after take");
    }
}
