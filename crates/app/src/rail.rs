//! Left icon rail (52 px): shark logo on top, pixel icon buttons, settings at the bottom.

use gpui::{
    Div, ImageSource, IntoElement, Resource, Stateful, WindowControlArea, div, img, prelude::*, px,
    rgb,
};
use theme::{Mode, Theme};

use crate::icons::icon;
use crate::titlebar::TITLEBAR_H;

/// Width of the left icon rail.
pub const RAIL_W: f32 = 52.0;
const BTN: f32 = 36.0;

fn rail_button(theme: &Theme, id: &'static str, name: &'static str, active: bool) -> Stateful<Div> {
    let c = theme.colors;
    let base = div()
        .id(id)
        .group(id)
        .size(px(BTN))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .cursor_pointer();
    let fg = if active { c.text } else { c.muted };
    base.when(active, |d| d.bg(c.surface_3))
        .hover(move |s| s.bg(if active { c.surface_3 } else { c.surface_2 }))
        .active(move |s| s.bg(c.surface_4))
        .child(icon(name, 16.0, fg).group_hover(id, move |s| s.text_color(c.text)))
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
                    // The logo art is light-on-dark: on the light theme it sits on a dark chip.
                    div()
                        .w(px(44.0))
                        .h(px(30.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.0))
                        .when(theme.mode == Mode::Light, |d| d.bg(rgb(0x131316)))
                        .child(
                            img(ImageSource::Resource(Resource::Embedded("logo.png".into())))
                                .w(px(40.0))
                                .h(px(40.0))
                                .flex_none(),
                        ),
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
