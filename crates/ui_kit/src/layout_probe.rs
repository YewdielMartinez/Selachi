//! Test support: boxes the kit's components report about themselves, so a screen's layout can be
//! asserted without pixels (`ui_settings` opens every section at several widths and checks that
//! every control stays inside its card, controls share a right edge, text never turns vertical).
//!
//! Compiled only for tests / the `test-support` feature; in a release build [`mark`] is a no-op
//! that the optimizer removes.

use gpui::{Bounds, ParentElement, Pixels};

/// What a marked box is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxKind {
    /// A whole [`crate::SettingRow`].
    Row,
    /// The text column of a row.
    Text,
    /// The control column of a row.
    Control,
    /// A group card of rows.
    Card,
    /// A text field.
    Field,
    /// A drop-down trigger.
    Select,
}

/// One reported box.
#[derive(Clone, Debug)]
pub struct BoxProbe {
    pub kind: BoxKind,
    /// Row label, card title, ...
    pub label: String,
    /// The row put its control under its text.
    pub stacked: bool,
    pub bounds: Bounds<Pixels>,
}

#[cfg(any(test, feature = "test-support"))]
mod on {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        pub(super) static PROBE: RefCell<Option<Vec<BoxProbe>>> = const { RefCell::new(None) };
    }

    /// Starts (or restarts) recording on this thread.
    pub fn start() {
        PROBE.with(|p| *p.borrow_mut() = Some(Vec::new()));
    }

    /// Stops recording and returns what was recorded since [`start`].
    pub fn take() -> Vec<BoxProbe> {
        PROBE.with(|p| p.borrow_mut().take().unwrap_or_default())
    }

    pub(super) fn record(p: BoxProbe) {
        PROBE.with(|v| {
            if let Some(v) = v.borrow_mut().as_mut() {
                v.push(p);
            }
        });
    }
}

#[cfg(any(test, feature = "test-support"))]
pub use on::{start, take};

/// Adds an invisible overlay to `parent` that reports the parent's final bounds.
#[cfg(any(test, feature = "test-support"))]
pub fn mark<P: ParentElement>(
    parent: P,
    kind: BoxKind,
    label: impl Into<String>,
    stacked: bool,
) -> P {
    use gpui::{AnyElement, IntoElement, Styled, canvas};
    let label = label.into();
    let overlay: AnyElement = canvas(
        move |bounds, _, _| {
            on::record(BoxProbe {
                kind,
                label,
                stacked,
                bounds,
            });
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_full()
    .into_any_element();
    let mut parent = parent;
    parent.extend(std::iter::once(overlay));
    parent
}

/// No-op outside tests.
#[cfg(not(any(test, feature = "test-support")))]
#[inline(always)]
pub fn mark<P: ParentElement>(
    parent: P,
    _kind: BoxKind,
    _label: impl Into<String>,
    _stacked: bool,
) -> P {
    parent
}
