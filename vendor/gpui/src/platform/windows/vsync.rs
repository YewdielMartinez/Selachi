use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        let f = Box::new(|| unsafe { DwmFlush().is_ok() });
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}


// ---- Selachi patch: demand-driven frame clock -------------------------------------------------
//
// Upstream runs the vsync thread unconditionally: every vblank it calls `RedrawWindow`, which makes
// the main thread process a WM_PAINT and run the whole request_frame callback even when nothing
// changed. That costs ~1% of a core for a completely idle, empty window (measured). Here the
// thread parks on a condvar once nothing has asked for a frame for `TAIL`, and anything that can
// need a frame (a window invalidation, a next-frame callback, any window message) wakes it.

use std::sync::{Condvar, Mutex};

/// How long the clock keeps ticking after the last wake-up. Must exceed the 1 s "keep presenting
/// after input" window in `Window::new`'s request_frame callback.
const TAIL: Duration = Duration::from_millis(1200);

struct FrameClock {
    last_wake: Mutex<Instant>,
    cv: Condvar,
}

static FRAME_CLOCK: std::sync::OnceLock<FrameClock> = std::sync::OnceLock::new();

fn clock() -> &'static FrameClock {
    FRAME_CLOCK.get_or_init(|| FrameClock { last_wake: Mutex::new(Instant::now()), cv: Condvar::new() })
}

/// Tells the vsync thread that a frame may be needed. Cheap; callable from any thread.
pub(crate) fn wake_frame_clock() {
    let c = clock();
    // try_lock: if the vsync thread holds it, it is awake anyway and will see a fresh time soon.
    if let Ok(mut last) = c.last_wake.try_lock() {
        *last = Instant::now();
        c.cv.notify_one();
    }
}

/// Called by the vsync thread each tick: blocks while the clock has been idle for longer than TAIL.
pub(crate) fn park_frame_clock_if_idle() {
    let c = clock();
    let mut last = c.last_wake.lock().unwrap_or_else(|e| e.into_inner());
    while last.elapsed() > TAIL {
        last = c.cv.wait(last).unwrap_or_else(|e| e.into_inner());
    }
}
