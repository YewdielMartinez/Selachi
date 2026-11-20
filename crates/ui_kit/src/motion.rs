//! Process-wide motion switch and the easing helpers shared by the animated components.
//!
//! With reduced motion on, everything still works but nothing moves: popovers appear at once,
//! the working spinner shows a static frame.

use std::sync::atomic::{AtomicBool, Ordering};

use theme::motion::CubicBezier;

static REDUCED: AtomicBool = AtomicBool::new(false);

/// Turns the "reduce motion" accessibility setting on or off (call it from the settings layer).
pub fn set_reduced_motion(reduced: bool) {
    REDUCED.store(reduced, Ordering::Relaxed);
}

/// Whether components must skip animation.
pub fn reduced_motion() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

/// Easing for gpui animations. gpui asserts that the eased delta stays in `0..=1`, so the
/// overshooting curves (`POP`, `CLICK`) are clamped here.
pub fn clamped(curve: CubicBezier) -> impl Fn(f32) -> f32 + Clone + 'static {
    move |t| curve.solve(t).clamp(0.0, 1.0)
}
