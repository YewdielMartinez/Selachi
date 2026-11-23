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
//! | Inputs | [`TextField`] / [`TextArea`] (entity, IME, clipboard), [`Select`], [`Checkbox`], [`RadioGroup`] |
//! | Assets | [`Assets`], [`Icon`], [`icon`], [`Logo`] |
//! | Feedback | [`DitherMeter`], [`Spinner`], [`StatusGlyph`], [`Tooltip`] |
//! | Settings | [`SettingRow`], [`SegmentedControl`], [`Toggle`], [`Slider`], [`SwatchRow`] |
//! | Overlays | [`Popover`] |
//! | Lists | [`VirtualList`], [`ListRow`] |
//! | Layout | [`Wireframe`], [`SectionLabel`], [`Card`], [`MeasuredWidth`] + [`WidthClass`] |
//! | Tests | [`testing`] (feature `test-support`) |
//!
//! # Keyboard
//!
//! Every clickable component is a tab stop with a keyboard-only focus ring and an opt-out
//! `.focusable(false)` (see [`focus`]). Call [`init`] once at startup (components also call it
//! lazily, so a missing call is harmless) to bind `Tab` / `Shift+Tab`, the text field keys, the
//! slider keys and the select keys.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod action_button;
mod agent_mark;
mod assets;
mod button;
mod controls;
mod drop_zone;
mod checkbox;
mod file_icon;
pub mod focus;
mod icon;
mod list;
mod logo;
mod measure;
mod meter;
mod motion;
mod pill;
mod popover;
mod select;
mod setting_row;
mod surface;
pub mod text_field;
mod tooltip;
mod wireframe;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod tests;

pub use action_button::{ActionButton, ActionSize};
pub use agent_mark::{AgentMark, agent_icon};
pub use assets::Assets;
pub use button::{Button, ButtonKind, GhostButton, IconButton};
pub use checkbox::{Checkbox, RadioGroup};
pub use controls::{Segment, SegmentedControl, Slider, SwatchRow, Toggle};
pub use drop_zone::{DropZone, debug_forced as drop_debug_forced};
pub use file_icon::{FileIcon, file_icon};
pub use focus::{FocusNext, FocusPrev, InputModality, debug_force_focus_ring, input_modality};
pub use icon::{Icon, icon};
pub use list::{
    ListRow, RowStatus, Spinner, StatusGlyph, UniformListScrollHandle, VirtualList,
};
pub use logo::Logo;
pub use measure::{MeasuredWidth, WidthClass, WindowClass, is_narrow};
pub use meter::DitherMeter;
pub use motion::{reduced_motion, set_reduced_motion};
pub use pill::{Pill, PillSize};
pub use popover::{Placement, Popover, SHEET_BELOW_WIDTH};
pub use select::{Select, SelectEvent, SelectOption};
pub use setting_row::{RowGeometry, SETTING_ROW_GAP, SettingRow};
pub use surface::{Card, CardTone, SectionLabel, glass_hover, glass_selected, glass_tint};
pub use text_field::{EnterMode, Nav, TextArea, TextBuf, TextField, TextFieldEvent};
pub use tooltip::Tooltip;
pub use wireframe::{WireFill, WirePreset, WireRect, Wireframe};

/// Marker: [`init`] already ran for this app.
struct KitReady;

impl gpui::Global for KitReady {}

/// Registers the key bindings and input tracking of the kit: `Tab` / `Shift+Tab` focus movement,
/// the text field, slider and select keys, and the keyboard-modality tracker behind the focus
/// ring. Idempotent: call it once at startup; components also call it on first use, so a missing
/// call still works (an app binding of its own is only overridden where the contexts are the same).
pub fn init(cx: &mut gpui::App) {
    if cx.has_global::<KitReady>() {
        return;
    }
    cx.set_global(KitReady);
    focus::bind_keys(cx);
    focus::install_modality_tracking(cx);
    text_field::bind_keys(cx);
    controls::bind_slider_keys(cx);
    select::bind_keys(cx);
}

pub(crate) fn ensure_init(cx: &mut gpui::App) {
    init(cx);
}
