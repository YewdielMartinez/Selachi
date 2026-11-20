//! Custom 40 px titlebar (DESIGN.md): breadcrumb, pill tabs, "+" and window controls.
//!
//! The row is a native drag area (`WindowControlArea::Drag`); every interactive child
//! `occlude()`s it so its own clicks are not swallowed by the caption hit test.

use gpui::{Div, IntoElement, SharedString, Stateful, WindowControlArea, div, prelude::*, px, rgb};
use theme::{Radius, Theme};

use crate::icons::icon;

/// Height of the titlebar.
pub const TITLEBAR_H: f32 = 40.0;
const CTL_W: f32 = 46.0;
const TAB_PILL_H: f32 = 28.0;
const PILL_R: f32 = 10.0;
const CLOSE_HOVER: u32 = 0xC42B1C;

fn control(
    theme: &Theme,
    id: &'static str,
    area: WindowControlArea,
    name: &'static str,
) -> Stateful<Div> {
    let c = theme.colors;
    let close = area == WindowControlArea::Close;
    div()
        .id(id)
        .group(id)
        .occlude()
        .window_control_area(area)
        .w(px(CTL_W))
        .h(px(TITLEBAR_H))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |s| {
            if close {
                s.bg(rgb(CLOSE_HOVER))
            } else {
                s.bg(c.surface_2)
            }
        })
        .active(move |s| {
            if close {
                s.bg(rgb(0x9E2216))
            } else {
                s.bg(c.surface_3)
            }
        })
        .child(icon(name, 10.0, c.muted).group_hover(id, move |s| {
            if close {
                s.text_color(rgb(0xFFFFFF))
            } else {
                s.text_color(c.text)
            }
        }))
}

fn segment(theme: &Theme, text: impl Into<SharedString>, last: bool) -> Div {
    let c = theme.colors;
    let d = div()
        .whitespace_nowrap()
        .text_color(if last { c.text } else { c.muted });
    // Only the last segment (the path) may shrink and ellipsize.
    if last {
        d.min_w_0().overflow_hidden().text_ellipsis()
    } else {
        d.flex_none()
    }
    .child(text.into())
}

fn breadcrumb(theme: &Theme, detail: SharedString) -> impl IntoElement {
    let c = theme.colors;
    let sep = || icon("chevron-right", 16.0, c.outline);
    div()
        .min_w_0()
        .max_w(px(420.0))
        .flex()
        .flex_row()
        .items_center()
        .child(segment(theme, "Selachi", false))
        .child(sep())
        .child(segment(theme, "Terminal", false))
        .child(sep())
        .child(segment(theme, detail, true))
}

fn tab(theme: &Theme, label: SharedString) -> impl IntoElement {
    let c = theme.colors;
    div()
        .id("tab-0")
        .occlude()
        .h(px(TAB_PILL_H))
        .flex_none()
        .px(px(12.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .rounded(px(PILL_R))
        .bg(c.surface_3)
        .text_color(c.text)
        .child(icon("terminal", 16.0, c.text))
        .child(label)
}

fn new_tab_button(theme: &Theme) -> impl IntoElement {
    let c = theme.colors;
    div()
        .id("tab-new")
        .group("tab-new")
        .occlude()
        .size(px(TAB_PILL_H))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(PILL_R))
        .cursor_pointer()
        .hover(move |s| s.bg(c.surface_2))
        .active(move |s| s.bg(c.surface_3))
        .child(icon("plus", 16.0, c.muted).group_hover("tab-new", move |s| s.text_color(c.text)))
}

pub fn titlebar(
    theme: &Theme,
    shell: SharedString,
    detail: SharedString,
    maximized: bool,
) -> impl IntoElement {
    let ty = theme.typography;
    div()
        .h(px(TITLEBAR_H))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(16.0))
        .pl(px(Radius::XS))
        .window_control_area(WindowControlArea::Drag)
        .font_family(ty.ui_family)
        .text_size(px(ty.ui_size_md))
        .child(breadcrumb(theme, detail))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(tab(theme, shell))
                .child(new_tab_button(theme)),
        )
        .child(div().flex_1())
        .child(control(
            theme,
            "ctl-min",
            WindowControlArea::Min,
            "minimize",
        ))
        .child(control(
            theme,
            "ctl-max",
            WindowControlArea::Max,
            if maximized { "restore" } else { "maximize" },
        ))
        .child(control(
            theme,
            "ctl-close",
            WindowControlArea::Close,
            "close",
        ))
}
