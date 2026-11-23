//! Selachi UI kit: the reusable, theme-driven gpui components of the app.
//!
//! Design rules (docs/DESIGN.md): zones are separated by tone, never by lines; the only 1 px
//! border is `outline_soft` on floating surfaces; the accent (`#00AAFF`) is the signal color and
//! `accent_strong` (`#005EEB`) the action color (one `ActionButton` per view); pixel art is an accent, not noise.
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
//! | Actions | [`Pill`], [`IconButton`], [`GhostButton`], [`Button`], [`ActionButton`] (`Button::action`) |
//! | Assets | [`Assets`], [`Icon`], [`icon`], [`Logo`] |
//! | Feedback | [`DitherMeter`], [`Spinner`], [`StatusGlyph`], [`Tooltip`] |
//! | Settings | [`SettingRow`], [`SegmentedControl`], [`Toggle`], [`Slider`], [`SwatchRow`] |
//! | Overlays | [`Popover`] |
//! | Lists | [`VirtualList`], [`ListRow`] |
//! | Layout | [`Wireframe`], [`SectionLabel`], [`Card`] |
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod action_button;
mod agent_mark;
mod assets;
mod button;
mod controls;
mod drop_zone;
mod file_icon;
mod icon;
mod list;
mod logo;
mod measure;
mod meter;
mod motion;
mod pill;
mod popover;
mod setting_row;
mod surface;
mod tooltip;
mod wireframe;

pub use action_button::{ActionButton, ActionSize};
pub use agent_mark::{AgentMark, agent_icon};
pub use assets::Assets;
pub use button::{Button, ButtonKind, GhostButton, IconButton};
pub use controls::{Segment, SegmentedControl, Slider, SwatchRow, Toggle};
pub use drop_zone::{DropZone, debug_forced as drop_debug_forced};
pub use file_icon::{FileIcon, file_icon};
pub use icon::{Icon, icon};
pub use list::{
    ListRow, RowStatus, Spinner, StatusGlyph, UniformListScrollHandle, VirtualList,
};
pub use logo::Logo;
pub use measure::{MeasuredWidth, is_narrow};
pub use meter::DitherMeter;
pub use motion::{reduced_motion, set_reduced_motion};
pub use pill::{Pill, PillSize};
pub use popover::{Placement, Popover, SHEET_BELOW_WIDTH};
pub use setting_row::{RowGeometry, SETTING_ROW_GAP, SettingRow};
pub use surface::{Card, CardTone, SectionLabel, glass_hover, glass_selected, glass_tint};
pub use tooltip::Tooltip;
pub use wireframe::{WireFill, WirePreset, WireRect, Wireframe};
