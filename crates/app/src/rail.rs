//! Left icon rail (52 px): shark logo on top, pixel icon buttons, settings at the bottom.

use gpui::{App, ClickEvent, IntoElement, Window, WindowControlArea, div, prelude::*, px};
use theme::Theme;
use ui_kit::{Icon, IconButton, Logo};

use crate::titlebar::TITLEBAR_H;

/// Width of the left icon rail.
pub const RAIL_W: f32 = 52.0;

/// Rail button. Exactly one is `active` (the current view): `surface_3` pill, icon `text`.
/// Hover is a different, lighter pill (`surface_2`) and keeps the muted icon, so a hovered
/// button can never be mistaken for the active one. Press is not brighter than active either.
fn rail_button(theme: &Theme, id: &'static str, icon: Icon, active: bool) -> IconButton {
    IconButton::new(id, theme, icon).active(active)
}

/// `git_active`: the git panel is open on the right; `on_git` toggles it.
pub fn rail(
    theme: &Theme,
    git_active: bool,
    on_git: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
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
                // Light art on dark/OLED, dark art on light: the file changes, never a chip.
                .child(Logo::new(theme.mode).size(40.0)),
        )
        .child(
            div()
                .mt(px(4.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(rail_button(theme, "rail-terminal", Icon::Terminal, true))
                .child(rail_button(theme, "rail-files", Icon::Files, false))
                .child(rail_button(theme, "rail-git", Icon::GitBranch, git_active).tooltip("Panel git").on_click(on_git))
                .child(rail_button(theme, "rail-agents", Icon::Agents, false))
                .child(rail_button(theme, "rail-history", Icon::History, false)),
        )
        .child(div().flex_1())
        .child(rail_button(theme, "rail-settings", Icon::Settings, false))
}
