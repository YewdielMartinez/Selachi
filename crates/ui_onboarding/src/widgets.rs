//! Small building blocks of the wizard, made from `ui_kit` pieces and `theme` tokens only.

use gpui::{
    Div, ElementId, FontWeight, IntoElement, SharedString, Stateful, div, prelude::*, px,
};
use theme::{Theme, rgb};
use ui_kit::{Icon, icon};

/// Widest content column (px).
pub const COLUMN_WIDTH: f32 = 560.0;
/// Widest column of the appearance step (five preset cards fit on one line).
pub const WIDE_COLUMN_WIDTH: f32 = 640.0;
/// Window width under which the wizard becomes a bottom sheet (DESIGN.md, class XS).
pub const XS_BELOW: f32 = 720.0;

/// Green of "ok" marks: the terminal palette's green, so it follows the mode.
pub fn ok_color(t: &Theme) -> theme::Rgba {
    rgb(t.terminal.ansi[2])
}

/// Amber of "attention" text: the terminal palette's yellow.
pub fn warn_color(t: &Theme) -> theme::Rgba {
    rgb(t.terminal.ansi[3])
}

/// `title` big and `subtitle` muted, centered.
pub fn title_block(t: &Theme, xs: bool, title: impl Into<SharedString>, subtitle: impl Into<SharedString>) -> Div {
    let c = t.colors;
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .child(
            div()
                .text_size(px(if xs { 24.0 } else { 30.0 }))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text)
                .text_center()
                .child(title.into()),
        )
        .child(div().max_w(px(460.0)).text_size(px(14.0)).line_height(px(21.0)).text_color(c.muted).text_center().child(subtitle.into()))
}

/// A row of a list inside a `surface_1` card: rounded, hover `surface_2`, selected `surface_3`.
pub fn row_shell(id: impl Into<ElementId>, t: &Theme, selected: bool, clickable: bool) -> Stateful<Div> {
    let c = t.colors;
    div()
        .id(id)
        .w_full()
        .min_h(px(44.0))
        .px(px(12.0))
        .py(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .rounded(px(12.0))
        .when(selected, |d| d.bg(c.surface_3))
        .when(!selected, |d| d.hover(move |s| s.bg(c.surface_2)))
        .when(clickable, |d| d.cursor_pointer())
}

/// Title (13, `text`) over an optional subtitle (12, `muted`); truncates instead of wrapping.
pub fn row_text(t: &Theme, title: impl Into<SharedString>, subtitle: Option<SharedString>, mono_subtitle: bool) -> Div {
    let c = t.colors;
    let ty = t.typography;
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_md))
                .text_color(c.text)
                .child(title.into()),
        )
        .when_some(subtitle, |d, s| {
            d.child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .when(mono_subtitle, |d| d.font_family(ty.mono_family))
                    .child(s),
            )
        })
}

/// Label (+ hint) on the left, the control on the right (below it on narrow windows).
pub fn setting_row(
    t: &Theme,
    xs: bool,
    title: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
) -> Div {
    let c = t.colors;
    let ty = t.typography;
    div()
        .w_full()
        .min_h(px(44.0))
        .px(px(12.0))
        .py(px(10.0))
        .flex()
        .when(xs, |d| d.flex_col().items_start())
        .when(!xs, |d| d.flex_row().items_center())
        .justify_between()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(px(ty.ui_size_md)).text_color(c.text).child(title.into()))
                .when_some(hint, |d, h| d.child(div().text_size(px(ty.ui_size_sm)).line_height(px(17.0)).text_color(c.muted).child(h))),
        )
        .child(div().flex_none().child(control))
}

/// Checkbox: `accent_strong` when on (a strong selection), `surface_3` when off.
pub fn checkbox(id: impl Into<ElementId>, t: &Theme, checked: bool) -> Stateful<Div> {
    let c = t.colors;
    div()
        .id(id)
        .occlude()
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .bg(if checked { c.accent_strong } else { c.surface_4 })
        .when(checked, |d| d.child(icon(Icon::Check, 12.0, c.on_accent_strong)))
}

/// Radio dot: a ring with a dot inside when selected.
pub fn radio(t: &Theme, selected: bool) -> Div {
    let c = t.colors;
    div()
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(if selected { c.accent_strong } else { c.surface_4 })
        .when(selected, |d| d.child(div().size(px(6.0)).rounded_full().bg(c.on_accent_strong)))
}

/// Star toggle of a project row.
///
/// TODO-SVG: `assets/icons/star.svg` and `star-filled.svg` (16x16, `currentColor`). Until they
/// exist the text glyphs are drawn.
pub fn star(id: impl Into<ElementId>, t: &Theme, on: bool) -> Stateful<Div> {
    let c = t.colors;
    div()
        .id(id)
        .occlude()
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .text_size(px(16.0))
        .text_color(if on { warn_color(t) } else { c.muted })
        .hover(move |s| s.bg(c.surface_3))
        .child(if on { "★" } else { "☆" })
}

/// Small tonal badge ("Rust", "git", "Recomendado").
pub fn badge(t: &Theme, text: impl Into<SharedString>) -> Div {
    let c = t.colors;
    div()
        .h(px(20.0))
        .px(px(8.0))
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(8.0))
        .bg(c.surface_3)
        .text_size(px(11.0))
        .text_color(c.muted)
        .child(text.into())
}

/// A `surface_1` card that holds rows.
pub fn group(t: &Theme) -> Div {
    div().w_full().p(px(6.0)).flex().flex_col().gap(px(2.0)).rounded(px(16.0)).bg(t.colors.surface_1)
}

/// 11 px muted heading above a group.
pub fn group_label(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().w_full().px(px(8.0)).pt(px(8.0)).text_size(px(11.0)).font_weight(FontWeight::MEDIUM).text_color(t.colors.muted).child(text.into())
}
