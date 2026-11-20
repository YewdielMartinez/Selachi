//! Custom 40 px titlebar pieces (DESIGN.md): breadcrumb, pill tabs, "+" and window controls.
//!
//! The shell composes them (see `app_shell`): the row is a native drag area
//! (`WindowControlArea::Drag`); every interactive child `occlude()`s it so its own clicks are not
//! swallowed by the caption hit test.

use gpui::{
    App, ClickEvent, Div, IntoElement, SharedString, Stateful, Window, WindowControlArea, div, prelude::*,
    px, rgb,
};
use panel_api::TabInfo;
use theme::Theme;
use ui_kit::{Icon, IconButton, Pill, StatusGlyph, icon};

use crate::paths;

/// Height of the titlebar.
pub const TITLEBAR_H: f32 = 40.0;
const CTL_W: f32 = 46.0;
const TAB_PILL_H: f32 = 28.0;
const CLOSE_HOVER: u32 = 0xC42B1C;

fn control(
    theme: &Theme,
    id: &'static str,
    area: WindowControlArea,
    name: Icon,
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

pub fn breadcrumb(theme: &Theme, detail: SharedString) -> impl IntoElement {
    let c = theme.colors;
    let sep = || icon(Icon::ChevronRight, 16.0, c.outline);
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

/// Top pill of a tab: icon, title, a status glyph while it has one, and a close mark on the
/// active one. `on_select` / `on_close` get the tab's own handlers.
pub fn tab_pill(
    theme: &Theme,
    info: &TabInfo,
    active: bool,
    on_select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let c = theme.colors;
    let id = info.id.as_u64();
    let mut pill = Pill::new(format!("tab-{id}"), theme)
        .icon(info.icon.icon())
        .label(paths::middle_truncate(&info.title, 24))
        .active(active)
        .on_click(on_select);
    if let Some(status) = info.status {
        pill = pill.child(StatusGlyph::new(theme, status));
    }
    if active {
        pill = pill.child(
            div()
                .id(SharedString::from(format!("tab-close-{id}")))
                .size(px(18.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(move |s| s.bg(c.surface_4))
                .on_click(move |e, w, cx| {
                    cx.stop_propagation();
                    on_close(e, w, cx);
                })
                .child(icon(Icon::Close, 10.0, c.muted)),
        );
    }
    pill
}

pub fn new_tab_button(
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    IconButton::new("tab-new", theme, Icon::Plus)
        .size(TAB_PILL_H)
        .emphasize_on_hover(true)
        .tooltip("Nueva pestaña (Ctrl+Shift+N)")
        .on_click(on_click)
}

/// The three window controls (minimize, maximize / restore, close), 138 x 40.
pub fn window_controls(theme: &Theme, maximized: bool) -> impl IntoElement + use<> {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .child(control(theme, "ctl-min", WindowControlArea::Min, Icon::Minimize))
        .child(control(
            theme,
            "ctl-max",
            WindowControlArea::Max,
            if maximized { Icon::Restore } else { Icon::Maximize },
        ))
        .child(control(theme, "ctl-close", WindowControlArea::Close, Icon::Close))
}

/// An empty 40 px drag row with the shell's font; the caller adds the content.
pub fn title_row(theme: &Theme) -> Div {
    let ty = theme.typography;
    div()
        .h(px(TITLEBAR_H))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .window_control_area(WindowControlArea::Drag)
        .font_family(ty.ui_family)
        .text_size(px(ty.ui_size_md))
}
