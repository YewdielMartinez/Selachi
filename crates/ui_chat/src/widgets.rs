//! Small shared pieces of the chat UI built from ui_kit primitives and theme tokens.

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, IntoElement, ParentElement as _, SharedString, Stateful, Styled as _,
    Window, div, prelude::*, px,
};
use theme::{Radius, Rgba, Theme};
use ui_kit::{FileIcon, Icon, Spinner, file_icon, icon};

pub type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A row of a popover menu: label, optional hint line, check when selected.
pub fn menu_row(
    id: impl Into<ElementId>,
    theme: &Theme,
    lead: Option<AnyElement>,
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
    selected: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let c = theme.colors;
    let ty = theme.typography;
    let hover = c.surface_3;
    let mut row = div()
        .id(id)
        .occlude()
        .w_full()
        .min_h(px(32.0))
        .px(px(10.0))
        .py(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .rounded(px(Radius::PILL))
        .font_family(ty.ui_family)
        .text_size(px(ty.ui_size_md))
        .text_color(if enabled { c.text } else { c.muted })
        .when(selected, |d| d.bg(c.surface_3))
        .when(enabled, |d| d.cursor_pointer().when(!selected, |d| d.hover(move |s| s.bg(hover))))
        .when(!enabled, |d| d.opacity(0.55))
        .when_some(lead, |d, l| d.child(l))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.into()))
                .when_some(hint, |d, h| d.child(div().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(h))),
        )
        .when(selected, |d| d.child(icon(Icon::Check, 16.0, c.text)));
    if enabled {
        row = row.on_click(move |e, w, cx| on_click(e, w, cx));
    }
    row
}

/// A 16 px file-type icon for `name`.
pub fn file_glyph(name: &str, color: Rgba) -> AnyElement {
    let kind = FileIcon::for_path(std::path::Path::new(name));
    file_icon(kind, 16.0, color).into_any_element()
}

/// A small chip: file icon + name (attachments, edited files).
pub fn file_chip(theme: &Theme, name: &str, color: Rgba) -> Div {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .h(px(24.0))
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(Radius::CONTROL))
        .bg(c.surface_3)
        .text_size(px(ty.ui_size_sm))
        .text_color(color)
        .child(file_glyph(name, color))
        .child(div().max_w(px(260.0)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(SharedString::from(name.to_string())))
}

/// The collapsed one-line row of tool activity: lead glyph, label, trailing meta and a chevron.
pub fn tool_line(
    id: impl Into<ElementId>,
    theme: &Theme,
    lead: AnyElement,
    label: AnyElement,
    meta: Option<AnyElement>,
    chevron: Option<bool>,
    on_click: Option<Handler>,
) -> Stateful<Div> {
    let c = theme.colors;
    let ty = theme.typography;
    let muted = c.muted;
    let text = c.text;
    let mut row = div()
        .id(id)
        .h(px(28.0))
        .max_w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .font_family(ty.ui_family)
        .text_size(px(ty.ui_size_md))
        .text_color(muted)
        .child(lead)
        .child(div().min_w_0().flex_shrink().overflow_hidden().whitespace_nowrap().child(label))
        .when_some(meta, |d, m| d.child(m))
        .when_some(chevron, |d, open| {
            d.child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 14.0, muted))
        });
    if let Some(f) = on_click {
        row = row.cursor_pointer().hover(move |s| s.text_color(text)).on_click(move |e, w, cx| f(e, w, cx));
    }
    row
}

/// The bars spinner as an element (accent: it is progress).
pub fn working_bars(theme: &Theme) -> AnyElement {
    Spinner::new(theme.colors.accent).into_any_element()
}
