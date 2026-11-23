//! Layout helpers shared by every section: setting rows, groups, status pills, banners.
//!
//! The look follows docs/DESIGN.md: groups are `surface_1` cards (no divider lines, rows are
//! separated by space), titles 13 medium, descriptions 12 muted, the accent only where a state is
//! active. At narrow widths a row stacks its control under the text.

use gpui::{AnyElement, Div, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, px};
use theme::{Radius, Rgba, Theme};
use ui_kit::{Icon, SettingRow, icon};

/// Content width above which the content column stops growing (docs/DESIGN.md, Responsive: XL).
pub const MAX_CONTENT_WIDTH: f32 = 860.0;
/// Window widths below this fold the section list into a top menu (class XS).
pub const NARROW_BELOW: f32 = 720.0;
/// Width of the section list.
pub const NAV_WIDTH: f32 = 208.0;

/// Page title + subtitle at the top of a section.
pub fn page_header(theme: &Theme, title: &str, subtitle: &str) -> Div {
    let c = theme.colors;
        let ty = theme.typography;
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .pb(px(8.0))
        .child(
            div()
                .text_size(px(ty.ui_size_xl))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.text)
                .child(SharedString::from(title.to_string())),
        )
        .child(div().text_size(px(ty.ui_size_md)).text_color(c.muted).child(SharedString::from(subtitle.to_string())))
}

/// A titled card of rows. `title` is the 11 px micro label above it.
pub fn group(theme: &Theme, title: Option<&str>, rows: Vec<AnyElement>) -> Div {
    let c = theme.colors;
        let ty = theme.typography;
    let mut wrap = div().flex().flex_col().gap(px(8.0)).w_full();
    if let Some(t) = title {
        wrap = wrap.child(
            div()
                .px(px(4.0))
                .text_size(px(ty.ui_size_xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c.muted)
                .child(SharedString::from(t.to_string())),
        );
    }
    wrap.child(
        div()
            .flex()
            .flex_col()
            .w_full()
            .py(px(6.0))
            .rounded(px(Radius::MD))
            .bg(c.surface_1)
            .children(rows),
    )
}

/// A group header with a trailing action (for instance "Volver a comprobar").
pub fn group_with_action(theme: &Theme, title: &str, action: impl IntoElement, rows: Vec<AnyElement>) -> Div {
    let c = theme.colors;
        let ty = theme.typography;
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px(px(4.0))
                .child(
                    div()
                        .text_size(px(ty.ui_size_xs))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c.muted)
                        .child(SharedString::from(title.to_string())),
                )
                .child(action),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .py(px(6.0))
                .rounded(px(Radius::MD))
                .bg(c.surface_1)
                .children(rows),
        )
}

/// One setting: title + description on the left, the control on the right (below at narrow
/// widths). `error` is shown under the text in `error` color. This is the shared
/// [`ui_kit::SettingRow`]: text column `flex_1 min_w_0` (wraps), control column `flex_none`.
pub fn row(
    theme: &Theme,
    narrow: bool,
    title: impl Into<SharedString>,
    desc: Option<SharedString>,
    control: impl IntoElement,
    error: Option<SharedString>,
) -> Div {
    div().w_full().child(SettingRow::new(theme, title, control).description_opt(desc).error(error).stacked(narrow))
}

/// Free-form row: arbitrary content padded like a setting row.
pub fn plain_row(content: impl IntoElement) -> Div {
    div().flex().flex_col().w_full().px(px(16.0)).py(px(12.0)).child(content)
}

/// Small rounded label ("Listo", "v0.24.2", "Instalado").
pub fn status_pill(theme: &Theme, text: impl Into<SharedString>, tone: PillTone) -> Div {
    let c = theme.colors;
        let ty = theme.typography;
    let (bg, fg): (Rgba, Rgba) = match tone {
        PillTone::Neutral => (c.surface_3, c.muted),
        PillTone::Ok => (c.surface_3, c.text),
        PillTone::Warn => (c.surface_3, c.text),
    };
    let mut d = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.0))
        .h(px(22.0))
        .px(px(9.0))
        .rounded(px(Radius::CONTROL))
        .bg(bg)
        .text_size(px(ty.ui_size_sm))
        .text_color(fg);
    if tone == PillTone::Ok {
        // A signal dot (progress / ready) is where the accent belongs.
        d = d.child(div().size(px(6.0)).rounded_full().bg(c.accent));
    }
    d.child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillTone {
    Neutral,
    Ok,
    Warn,
}

/// A tonal info banner with an icon ("Reinicia Selachi para completar").
pub fn banner(theme: &Theme, ic: Icon, title: &str, body: &str, trailing: Option<AnyElement>) -> Div {
    let c = theme.colors;
        let ty = theme.typography;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .w_full()
        .px(px(16.0))
        .py(px(12.0))
        .rounded(px(Radius::MD))
        .bg(c.surface_2)
        .child(icon(ic, 16.0, c.text))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(px(ty.ui_size_md)).font_weight(FontWeight::MEDIUM).text_color(c.text).child(SharedString::from(title.to_string())))
                .child(div().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(SharedString::from(body.to_string()))),
        )
        .children(trailing)
}

/// Current value shown next to a slider (`13 pt`, `35 %`).
pub fn value_label(theme: &Theme, text: impl Into<SharedString>) -> Div {
    let ty = theme.typography;
    div()
        .w(px(52.0))
        .flex_none()
        .text_size(px(ty.ui_size_sm))
        .text_color(theme.colors.muted)
        .flex()
        .justify_end()
        .child(text.into())
}

/// Muted explanatory paragraph.
pub fn note(theme: &Theme, text: impl Into<SharedString>) -> Div {
    let ty = theme.typography;
    div().px(px(4.0)).text_size(px(ty.ui_size_sm)).text_color(theme.colors.muted).child(text.into())
}

/// Spanish decimal comma for display (`1.3` -> `1,3`).
pub fn decimal(v: f64, places: usize) -> String {
    format!("{v:.places$}").replace('.', ",")
}
