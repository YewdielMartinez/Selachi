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

/// How much room a panel got (docs/DESIGN.md: "Cada panel recibe su `WidthClass` del ancho real de
/// su zona"). Same thresholds as `panel_api::WidthClass`; convert with a `match` at the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WidthClass {
    /// Narrower than [`WidthClass::COMPACT_BELOW_PX`]: one line per item, no secondary columns.
    Compact,
    Regular,
}

impl WidthClass {
    /// Widths (logical px) below this are [`WidthClass::Compact`].
    pub const COMPACT_BELOW_PX: f32 = 320.0;

    pub fn from_width(width_px: f32) -> Self {
        if width_px < Self::COMPACT_BELOW_PX { Self::Compact } else { Self::Regular }
    }

    pub fn is_compact(self) -> bool {
        self == Self::Compact
    }
}

/// Window size class of the shell (docs/DESIGN.md, Responsive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WindowClass {
    /// < 720 px.
    Xs,
    /// 720 to 1023 px.
    S,
    /// 1024 to 1439 px.
    M,
    /// 1440 to 2559 px.
    L,
    /// 2560 px and wider.
    Xl,
}

impl WindowClass {
    pub fn from_width(width_px: f32) -> Self {
        match width_px {
            w if w < 720.0 => Self::Xs,
            w if w < 1024.0 => Self::S,
            w if w < 1440.0 => Self::M,
            w if w < 2560.0 => Self::L,
            _ => Self::Xl,
        }
    }
}

impl MeasuredWidth {
    /// A probe whose breakpoint is the panel threshold ([`WidthClass::COMPACT_BELOW_PX`]).
    pub fn panel() -> Self {
        Self::new(WidthClass::COMPACT_BELOW_PX)
    }

    /// The class of the measured width (the window's while nothing was measured yet). Use it with
    /// [`MeasuredWidth::panel`], or any probe, to branch on the real zone width.
    pub fn class(&self, window: &Window) -> WidthClass {
        WidthClass::from_width(self.width.get().unwrap_or_else(|| f32::from(window.viewport_size().width)))
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

    #[test]
    fn width_classes_follow_the_design_thresholds() {
        assert_eq!(WidthClass::from_width(319.9), WidthClass::Compact);
        assert_eq!(WidthClass::from_width(320.0), WidthClass::Regular);
        assert!(WidthClass::from_width(100.0).is_compact());
        assert_eq!(WindowClass::from_width(719.0), WindowClass::Xs);
        assert_eq!(WindowClass::from_width(720.0), WindowClass::S);
        assert_eq!(WindowClass::from_width(1023.0), WindowClass::S);
        assert_eq!(WindowClass::from_width(1024.0), WindowClass::M);
        assert_eq!(WindowClass::from_width(1440.0), WindowClass::L);
        assert_eq!(WindowClass::from_width(3840.0), WindowClass::Xl);
        assert!(WindowClass::Xs < WindowClass::Xl);
    }

    #[test]
    fn a_panel_probe_has_the_panel_breakpoint() {
        let m = MeasuredWidth::panel();
        assert_eq!(m.width(), None);
        assert!(m.breaks.first().is_some_and(|b| (*b - WidthClass::COMPACT_BELOW_PX).abs() < f32::EPSILON));
    }
}
