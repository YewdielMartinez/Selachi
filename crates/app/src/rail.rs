//! Left icon rail (52 px): shark logo on top, pixel icon buttons, settings at the bottom.

use gpui::{
    Div, ImageSource, IntoElement, Resource, Stateful, WindowControlArea, div, img, prelude::*, px,
};
use theme::{Mode, Theme};

use crate::icons::icon;
use crate::titlebar::TITLEBAR_H;

/// Width of the left icon rail.
pub const RAIL_W: f32 = 52.0;
const BTN: f32 = 36.0;

/// Rail button. Exactly one is `active` (the current view): `surface_3` pill, icon `text`.
/// Hover is a different, lighter pill (`surface_2`) and keeps the muted icon, so a hovered
/// button can never be mistaken for the active one. Press is not brighter than active either.
fn rail_button(theme: &Theme, id: &'static str, name: &'static str, active: bool) -> Stateful<Div> {
    let c = theme.colors;
    let base = div()
        .id(id)
        .size(px(BTN))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .cursor_pointer();
    let fg = if active { c.text } else { c.muted };
    base.when(active, |d| d.bg(c.surface_3))
        .when(!active, |d| d.hover(move |s| s.bg(c.surface_2)).active(move |s| s.bg(c.surface_2)))
        .child(icon(name, 16.0, fg))
}

/// Embedded logo for a theme mode.
fn logo_asset(mode: Mode) -> &'static str {
    match mode {
        Mode::Light => "logo-light-mode.png",
        Mode::Dark | Mode::Oled => "logo-dark-mode.png",
    }
}

pub fn rail(theme: &Theme) -> impl IntoElement {
    div()
        .w(px(RAIL_W))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .pb(px(6.0))
        // Logo: also part of the drag area so the window can be grabbed from the top-left.
        .child(
            div()
                .w_full()
                .h(px(TITLEBAR_H))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .window_control_area(WindowControlArea::Drag)
                .child(
                    // Light art on dark/OLED, dark art on light: the file changes, never a chip.
                    img(ImageSource::Resource(Resource::Embedded(logo_asset(theme.mode).into())))
                        .w(px(40.0))
                        .h(px(40.0))
                        .flex_none(),
                ),
        )
        .child(
            div()
                .mt(px(4.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(rail_button(theme, "rail-terminal", "terminal", true))
                .child(rail_button(theme, "rail-files", "files", false))
                .child(rail_button(theme, "rail-git", "git-branch", false))
                .child(rail_button(theme, "rail-agents", "agents", false))
                .child(rail_button(theme, "rail-history", "history", false)),
        )
        .child(div().flex_1())
        .child(rail_button(theme, "rail-settings", "settings", false))
}
