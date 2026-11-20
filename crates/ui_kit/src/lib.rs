//! Selachi UI kit: the reusable, theme-driven gpui components of the app.
//!
//! Design rules (docs/DESIGN.md): zones are separated by tone, never by lines; the only 1 px
//! border is `outline_soft` on floating surfaces; the accent appears only for active/progress
//! states; pixel art is an accent, not noise.
//!
//! Every component takes a `&Theme` and is stateless (the caller owns the value and re-renders
//! from callbacks). Register the assets once:
//!
//! ```ignore
//! Application::new().with_assets(ui_kit::Assets).run(|cx| { /* ... */ });
//! ```
//!
//! | Area | Components |
//! |---|---|
//! | Actions | [`Pill`], [`IconButton`], [`GhostButton`], [`Button`] |
//! | Assets | [`Assets`], [`Icon`], [`icon`], [`Logo`] |
//! | Feedback | [`DitherMeter`], [`Spinner`], [`StatusGlyph`], [`Tooltip`] |
//! | Settings | [`SegmentedControl`], [`Toggle`], [`Slider`], [`SwatchRow`] |
//! | Overlays | [`Popover`] |
//! | Lists | [`VirtualList`], [`ListRow`] |
//! | Layout | [`Wireframe`], [`SectionLabel`], [`Card`] |
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod assets;
mod button;
mod controls;
mod icon;
mod list;
mod logo;
mod meter;
mod motion;
mod pill;
mod popover;
mod surface;
mod tooltip;
mod wireframe;

pub use assets::Assets;
pub use button::{Button, ButtonKind, GhostButton, IconButton};
pub use controls::{Segment, SegmentedControl, Slider, SwatchRow, Toggle};
pub use icon::{Icon, icon};
pub use list::{
    ListRow, RowStatus, Spinner, StatusGlyph, UniformListScrollHandle, VirtualList,
};
pub use logo::Logo;
pub use meter::DitherMeter;
pub use motion::{reduced_motion, set_reduced_motion};
pub use pill::{Pill, PillSize};
pub use popover::{Placement, Popover};
pub use surface::{Card, CardTone, SectionLabel};
pub use tooltip::Tooltip;
pub use wireframe::{WireFill, WirePreset, WireRect, Wireframe};
