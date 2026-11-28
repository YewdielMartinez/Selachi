//! Small building blocks of the wizard, made from `ui_kit` pieces and `theme` tokens only.

use gpui::{Div, ElementId, FontWeight, IntoElement, SharedString, Stateful, div, prelude::*, px};
use theme::{Radius, Theme};
use ui_kit::{Icon, icon};

/// Widest content column (px).
pub const COLUMN_WIDTH: f32 = 560.0;
/// Widest column of the appearance step (five preset cards fit on one line).
pub const WIDE_COLUMN_WIDTH: f32 = 640.0;
/// Window width under which the wizard becomes a bottom sheet (DESIGN.md, class XS).
pub const XS_BELOW: f32 = 720.0;

/// `title` big and `subtitle` muted, centered.
pub fn title_block(
    t: &Theme,
    xs: bool,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
) -> Div {
    let c = t.colors;
    let ty = t.typography;
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .child(
            div()
                .text_size(px(if xs {
                    t.typography.ui_size_xl
                } else {
                    t.typography.ui_size_display
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text)
                .text_center()
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(460.0))
                .text_size(px(ty.ui_size_body))
                .line_height(px(21.0))
                .text_color(c.muted)
                .text_center()
                .child(subtitle.into()),
        )
}

/// A row of a list inside a `surface_1` card: rounded, hover `surface_2`, selected `surface_3`.
pub fn row_shell(
    id: impl Into<ElementId>,
    t: &Theme,
    selected: bool,
    clickable: bool,
) -> Stateful<Div> {
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
        .rounded(px(Radius::SM))
        .when(selected, |d| d.bg(c.surface_3))
        .when(!selected, |d| d.hover(move |s| s.bg(c.surface_2)))
        .when(clickable, |d| {
            // A tab stop: Enter / Space run the click; `Tab` is moved by the view root.
            d.cursor_pointer()
                .tab_index(0)
                .focus(move |s| s.bg(c.surface_4))
        })
}

/// Title (13, `text`) over an optional subtitle (12, `muted`); truncates instead of wrapping.
pub fn row_text(
    t: &Theme,
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    mono_subtitle: bool,
) -> Div {
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

/// Label (+ hint) on the left, the control on the right (below it on narrow windows): the shared
/// [`ui_kit::SettingRow`] (text column `flex_1 min_w_0` and wrapping, control column `flex_none`).
pub fn setting_row(
    t: &Theme,
    xs: bool,
    title: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
) -> Div {
    div().w_full().px(px(12.0)).py(px(10.0)).child(
        ui_kit::SettingRow::new(t, title, control)
            .description_opt(hint)
            .stacked(xs)
            .padded(false)
            .min_height(24.0)
            .regular_label(),
    )
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
        .bg(if selected {
            c.accent_strong
        } else {
            c.surface_4
        })
        .when(selected, |d| {
            d.child(div().size(px(6.0)).rounded_full().bg(c.on_accent_strong))
        })
}

/// Star toggle of a project row.
pub fn star(id: impl Into<ElementId>, t: &Theme, on: bool) -> Stateful<Div> {
    let c = t.colors;
    div()
        .id(id)
        .occlude()
        .tab_index(0)
        .focus(move |s| s.bg(c.surface_3))
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(Radius::CONTROL))
        .cursor_pointer()
        .hover(move |s| s.bg(c.surface_3))
        .child(icon(
            if on { Icon::StarFilled } else { Icon::Star },
            16.0,
            if on { c.warning } else { c.muted },
        ))
}

/// Small tonal badge ("Rust", "git", "Recomendado").
pub fn badge(t: &Theme, text: impl Into<SharedString>) -> Div {
    let c = t.colors;
    let ty = t.typography;
    div()
        .h(px(20.0))
        .px(px(8.0))
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(Radius::CONTROL))
        .bg(c.surface_3)
        .text_size(px(ty.ui_size_xs))
        .text_color(c.muted)
        .child(text.into())
}

/// A `surface_1` card that holds rows.
pub fn group(t: &Theme) -> Div {
    div()
        .w_full()
        .p(px(6.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .rounded(px(Radius::MD))
        .bg(t.colors.surface_1)
}

/// 11 px muted heading above a group.
pub fn group_label(t: &Theme, text: impl Into<SharedString>) -> Div {
    let ty = t.typography;
    div()
        .w_full()
        .px(px(8.0))
        .pt(px(8.0))
        .text_size(px(ty.ui_size_xs))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.colors.muted)
        .child(text.into())
}
