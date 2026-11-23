//! `MeasuredWidth`: responsive layout from the width a view really has (its zone), not from the
//! window. A zone is narrower than the window whenever a sidebar or right panel is open, so
//! `window.viewport_size()` is the wrong input for "is this panel narrow?".
//!
//! Put [`MeasuredWidth::probe`] inside the view's root (it must be `relative`); it paints nothing,
//! records the root's width during prepaint and, when the narrow/regular answer flips, asks the window
//! to redraw. Until the first measurement the window width is the fallback.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{IntoElement, Styled, Window, canvas, px};

/// Shared measured width of one view and its "narrow below" threshold (logical px).
#[derive(Clone)]
pub struct MeasuredWidth {
    width: Rc<Cell<Option<f32>>>,
    /// Width breakpoints (ascending); the first is the "narrow below" threshold. A redraw is
    /// requested whenever the measured width crosses any of them.
    breaks: Rc<[f32]>,
}

impl MeasuredWidth {
    pub fn new(narrow_below: f32) -> Self {
        Self::with_breaks(&[narrow_below])
    }

    /// Several breakpoints (the first one is the narrow threshold).
    pub fn with_breaks(breaks: &[f32]) -> Self {
        Self { width: Rc::new(Cell::new(None)), breaks: breaks.into() }
    }

    /// The last measured width.
    pub fn width(&self) -> Option<f32> {
        self.width.get()
    }

    /// Narrow by the measured width (the window's while nothing was measured yet).
    pub fn narrow(&self, window: &Window) -> bool {
        is_narrow(self.width.get(), f32::from(window.viewport_size().width), self.breaks.first().copied().unwrap_or(0.0))
    }

    /// Invisible full-size child of the (relative) root that measures it.
    pub fn probe(&self) -> impl IntoElement + use<> {
        let (cell, breaks) = (self.width.clone(), self.breaks.clone());
        let class = move |w: f32| breaks.iter().filter(|b| w >= **b).count();
        canvas(
            move |bounds, window, _| {
                let w = f32::from(bounds.size.width);
                let before = cell.get();
                cell.set(Some(w));
                if before.is_some_and(|b| class(b) != class(w)) {
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .size_full()
    }
}

/// The decision, pure: measured width wins, else the window's.
pub fn is_narrow(measured: Option<f32>, window_w: f32, below: f32) -> bool {
    measured.unwrap_or(window_w) < below
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_width_beats_window_width() {
        // 1280 px window, but the settings zone is 640 px (sidebar + right panel open).
        assert!(is_narrow(Some(640.0), 1280.0, 720.0));
        assert!(!is_narrow(Some(900.0), 700.0, 720.0));
        // Not measured yet: the window is the fallback.
        assert!(is_narrow(None, 600.0, 720.0));
        assert!(!is_narrow(None, 1280.0, 720.0));
    }
}
