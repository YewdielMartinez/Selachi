//! Row builders for the virtualized bodies: code lines, diff rows (unified / split), hex rows and
//! JSON/TOML/YAML tree rows. Every row has a fixed height so `uniform_list` can virtualize.

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _,
    StyledText, div, prelude::*, px,
};
use theme::Theme;
use ui_kit::{Icon, icon};
use viewer_svc::{DiffLine, HexLine, HighlightKind, LineKind, NodeKind};

use crate::data_tree::DataRow;
use crate::spans::{LineSpans, find_ranges, overlay};
use crate::syntax::{color_for, is_italic};

/// Height of every code / diff / hex row (Geist Mono 13 at about 1.4).
pub const ROW_H: f32 = 18.0;
/// Height of a data-tree row.
pub const DATA_ROW_H: f32 = 24.0;
/// Approximate advance of one mono column at 13 px (gutter sizing).
const CH: f32 = 7.9;

/// A theme color (a role such as `success`) at `alpha`.
pub fn tint(color: gpui::Rgba, alpha: f32) -> Hsla {
    gpui::Rgba { a: alpha, ..color }.into()
}

/// Width of a line-number gutter for `digits` digits.
pub fn gutter_width(digits: usize) -> f32 {
    (digits as f32 * CH).ceil() + 20.0
}

fn number_cell(theme: &Theme, n: Option<usize>, digits: usize) -> AnyElement {
    div()
        .flex_none()
        .w(px(gutter_width(digits)))
        .pr(px(10.0))
        .flex()
        .justify_end()
        .text_size(px(theme.typography.ui_size_sm))
        .text_color(theme.colors.outline)
        .child(SharedString::from(n.map(|n| n.to_string()).unwrap_or_default()))
        .into_any_element()
}

fn syntax_style(kind: HighlightKind, theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: color_for(kind, theme).map(Into::into),
        font_style: is_italic(kind).then_some(FontStyle::Italic),
        ..HighlightStyle::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Code
// ---------------------------------------------------------------------------------------------

/// One source line: gutter number plus colored text; `query` highlights find matches and
/// `current` marks the line of the current match.
pub fn code_row(
    theme: &Theme,
    line_no: usize,
    digits: usize,
    text: &str,
    spans: &LineSpans,
    query: Option<&str>,
    current: bool,
) -> AnyElement {
    let c = theme.colors;
    let found: Vec<_> = query.map(|q| find_ranges(text, q)).unwrap_or_default().into_iter().map(|r| (r, ())).collect();
    let match_bg = tint(theme.colors.warning, if current { 0.55 } else { 0.32 });
    let segments = overlay(spans, &found, text.len());
    let styles: Vec<_> = segments
        .into_iter()
        .map(|(range, kind, hit)| {
            let mut h = kind.map(|k| syntax_style(k, theme)).unwrap_or_default();
            if hit.is_some() {
                h.background_color = Some(match_bg);
            }
            (range, h)
        })
        .collect();
    div()
        .w_full()
        .h(px(ROW_H))
        .flex()
        .flex_row()
        .items_center()
        .font_family(theme.typography.mono_family)
        .text_size(px(theme.typography.mono_size))
        .line_height(px(ROW_H))
        .text_color(c.text)
        .when(current, |d| d.bg(c.surface_2))
        .child(number_cell(theme, Some(line_no), digits))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(StyledText::new(SharedString::from(text.to_owned())).with_highlights(styles)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------------------------

fn line_bg(theme: &Theme, kind: LineKind) -> Option<Hsla> {
    match kind {
        LineKind::Added => Some(tint(theme.colors.success, 0.13)),
        LineKind::Removed => Some(tint(theme.colors.error, 0.13)),
        LineKind::Context => None,
    }
}

fn diff_text(theme: &Theme, line: &DiffLine) -> AnyElement {
    let prepared = crate::spans::prepare_line(&line.text, &[], crate::spans::MAX_LINE_BYTES);
    let strong = match line.kind {
        LineKind::Added => tint(theme.colors.success, 0.34),
        _ => tint(theme.colors.error, 0.34),
    };
    // Inline ranges refer to the original text; tabs/truncation shift them, so only trust them when
    // the text was not rewritten.
    let styles: Vec<_> = if prepared.text == line.text {
        line.inline
            .iter()
            .map(|r| (r.clone(), HighlightStyle { background_color: Some(strong), ..HighlightStyle::default() }))
            .collect()
    } else {
        Vec::new()
    };
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(StyledText::new(SharedString::from(prepared.text)).with_highlights(styles))
        .into_any_element()
}

fn row_base(theme: &Theme) -> gpui::Div {
    div()
        .w_full()
        .h(px(ROW_H))
        .flex()
        .flex_row()
        .items_center()
        .font_family(theme.typography.mono_family)
        .text_size(px(theme.typography.mono_size))
        .line_height(px(ROW_H))
        .text_color(theme.colors.text)
}

/// `@@ -1,3 +1,4 @@`
pub fn hunk_row(theme: &Theme, header: &str) -> AnyElement {
    let c = theme.colors;
    row_base(theme)
        .bg(c.surface_1)
        .px(px(12.0))
        .text_size(px(theme.typography.ui_size_sm))
        .text_color(c.outline)
        .child(SharedString::from(header.to_owned()))
        .into_any_element()
}

fn sign(theme: &Theme, kind: LineKind) -> AnyElement {
    let (s, color) = match kind {
        LineKind::Added => ("+", theme.colors.success),
        LineKind::Removed => ("-", theme.colors.error),
        LineKind::Context => (" ", theme.colors.outline),
    };
    div().flex_none().w(px(14.0)).text_color(color).child(s).into_any_element()
}

pub fn unified_row(theme: &Theme, line: &DiffLine, digits: usize) -> AnyElement {
    row_base(theme)
        .when_some(line_bg(theme, line.kind), |d, bg| d.bg(bg))
        .child(number_cell(theme, line.old_line, digits))
        .child(number_cell(theme, line.new_line, digits))
        .child(sign(theme, line.kind))
        .child(diff_text(theme, line))
        .into_any_element()
}

fn half(theme: &Theme, line: Option<&DiffLine>, old_side: bool, digits: usize) -> AnyElement {
    let base = div().flex_1().min_w_0().h(px(ROW_H)).flex().flex_row().items_center();
    match line {
        Some(l) => base
            .when_some(line_bg(theme, l.kind), |d, bg| d.bg(bg))
            .child(number_cell(theme, if old_side { l.old_line } else { l.new_line }, digits))
            .child(sign(theme, l.kind))
            .child(diff_text(theme, l))
            .into_any_element(),
        None => base.bg(theme.colors.surface_1).into_any_element(),
    }
}

pub fn split_row(theme: &Theme, left: Option<&DiffLine>, right: Option<&DiffLine>, digits: usize) -> AnyElement {
    row_base(theme)
        .child(half(theme, left, true, digits))
        .child(div().flex_none().w(px(2.0)).h_full().bg(theme.colors.bg))
        .child(half(theme, right, false, digits))
        .into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Hex
// ---------------------------------------------------------------------------------------------

pub fn hex_row(theme: &Theme, line: &HexLine) -> AnyElement {
    let c = theme.colors;
    let mut hex = String::with_capacity(52);
    for (i, b) in line.bytes.iter().enumerate() {
        if i == 8 {
            hex.push(' ');
        }
        hex.push_str(&format!("{b:02x} "));
    }
    row_base(theme)
        .px(px(16.0))
        .gap(px(16.0))
        .text_size(px(theme.typography.mono_size))
        .child(div().flex_none().text_color(c.outline).child(SharedString::from(format!("{:08x}", line.offset))))
        .child(div().flex_none().w(px(CH * 49.0)).whitespace_nowrap().child(SharedString::from(hex)))
        .child(div().flex_none().text_color(c.muted).child(SharedString::from(line.ascii.clone())))
        .into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Data tree
// ---------------------------------------------------------------------------------------------

fn value_color(theme: &Theme, kind: NodeKind) -> Option<gpui::Rgba> {
    match kind {
        NodeKind::String => color_for(HighlightKind::String, theme),
        NodeKind::Number | NodeKind::DateTime => color_for(HighlightKind::Number, theme),
        NodeKind::Bool => color_for(HighlightKind::Keyword, theme),
        NodeKind::Null => Some(theme.colors.outline),
        NodeKind::Object | NodeKind::Array => Some(theme.colors.muted),
    }
}

/// One row of the structured tree. Click handling is attached by the caller (it knows the row path).
pub fn data_row(theme: &Theme, row: &DataRow) -> gpui::Div {
    let c = theme.colors;
    let indent = 14.0 + row.depth as f32 * 16.0;
    let lead: AnyElement = if row.expandable {
        icon(if row.expanded { Icon::ChevronDown } else { Icon::ChevronRight }, 12.0, c.muted).into_any_element()
    } else {
        div().w(px(12.0)).flex_none().into_any_element()
    };
    let base = div()
        .w_full()
        .h(px(DATA_ROW_H))
        .pl(px(indent))
        .pr(px(12.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .font_family(theme.typography.mono_family)
        .text_size(px(theme.typography.mono_size))
        .text_color(c.text);
    if row.more.is_some() {
        return base
            .child(div().w(px(12.0)).flex_none())
            .child(div().text_color(c.accent).child(SharedString::from(format!("... {}", row.preview))));
    }
    let key_color = color_for(HighlightKind::Property, theme).unwrap_or(c.text);
    let is_container = matches!(row.kind, NodeKind::Object | NodeKind::Array);
    base.child(lead)
        .child(
            div()
                .flex_none()
                .font_weight(if row.depth == 0 { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .text_color(key_color)
                .child(SharedString::from(row.key.clone())),
        )
        .child(div().flex_none().text_color(c.outline).child(":"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(value_color(theme, row.kind).unwrap_or(c.text))
                .when(is_container, |d| d.text_size(px(theme.typography.ui_size_sm)))
                .child(SharedString::from(row.preview.clone())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gutter_grows_with_digits() {
        assert!(gutter_width(5) > gutter_width(2));
        assert_eq!(gutter_width(2), (2.0f32 * CH).ceil() + 20.0);
    }

    #[test]
    fn tint_applies_alpha() {
        let t = Theme::dark();
        let c = tint(t.colors.success, 0.13);
        assert!((c.a - 0.13).abs() < 1e-6);
    }
}
