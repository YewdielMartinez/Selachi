//! `SettingRow`: the one row pattern of every settings-like screen (Ajustes, onboarding, layout
//! panel). A text column (label + wrapping description) and a control column that never overlap:
//!
//! * the text column is `flex_1 min_w_0` (it takes what is left and wraps),
//! * the control column is `flex_none` (it keeps its size),
//! * the gap is 16 px,
//! * when `stacked` (narrow windows) the control goes under the text, full width.
//!
//! Long unbreakable values (a URL) belong inside the control, which must size itself (a fixed max
//! width with its own truncation), never push the text.

use gpui::{
    AnyElement, App, Div, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, prelude::*, px,
};
use theme::Theme;

/// Gap between the text and the control column.
pub const SETTING_ROW_GAP: f32 = 16.0;

/// How a row distributes its two columns, as plain data (tested without a window).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowGeometry {
    /// Text column grows and shrinks (`flex_1`) and may shrink below its content (`min_w_0`).
    pub text_flexes: bool,
    /// Control column keeps its size.
    pub control_fixed: bool,
    pub gap: f32,
    /// Control under the text.
    pub stacked: bool,
}

impl RowGeometry {
    pub const fn new(stacked: bool) -> Self {
        Self {
            text_flexes: true,
            control_fixed: !stacked,
            gap: if stacked { 10.0 } else { SETTING_ROW_GAP },
            stacked,
        }
    }
}

/// Label, optional description and error on the left; the control on the right (below when
/// `stacked`).
///
/// ```ignore
/// SettingRow::new(&theme, "Hook statusLine", toggle).description("Opcional...").stacked(narrow)
/// ```
#[derive(IntoElement)]
pub struct SettingRow {
    theme: Theme,
    label: SharedString,
    description: Option<SharedString>,
    error: Option<SharedString>,
    control: AnyElement,
    stacked: bool,
    padded: bool,
    min_h: f32,
    weight: FontWeight,
}

impl SettingRow {
    pub fn new(theme: &Theme, label: impl Into<SharedString>, control: impl IntoElement) -> Self {
        Self {
            theme: *theme,
            label: label.into(),
            description: None,
            error: None,
            control: control.into_any_element(),
            stacked: false,
            padded: true,
            min_h: 0.0,
            weight: FontWeight::MEDIUM,
        }
    }

    pub fn description(mut self, text: impl Into<SharedString>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn description_opt(mut self, text: Option<SharedString>) -> Self {
        self.description = text;
        self
    }

    /// Shown under the text in the error color.
    pub fn error(mut self, text: Option<SharedString>) -> Self {
        self.error = text;
        self
    }

    /// Control under the text (narrow windows).
    pub fn stacked(mut self, stacked: bool) -> Self {
        self.stacked = stacked;
        self
    }

    /// Row padding (16 x 12). Off when the host already pads.
    pub fn padded(mut self, padded: bool) -> Self {
        self.padded = padded;
        self
    }

    /// Regular-weight label (dense panels); the default is medium.
    pub fn regular_label(mut self) -> Self {
        self.weight = FontWeight::NORMAL;
        self
    }

    pub fn min_height(mut self, px: f32) -> Self {
        self.min_h = px;
        self
    }
}

impl RenderOnce for SettingRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let g = RowGeometry::new(self.stacked);
        let text = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .min_w_0()
            .when(g.text_flexes && !g.stacked, |d| d.flex_1())
            .when(g.stacked, |d| d.w_full())
            .child(
                div()
                    .text_size(px(ty.ui_size_md))
                    .font_weight(self.weight)
                    .text_color(c.text)
                    .child(self.label),
            )
            .when_some(self.description, |d, t| {
                d.child(
                    div()
                        .text_size(px(ty.ui_size_sm))
                        .line_height(px(17.0))
                        .text_color(c.muted)
                        .child(t),
                )
            })
            .when_some(self.error, |d, e| {
                d.child(
                    div()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.error)
                        .child(e),
                )
            });
        let control: Div = div()
            .when(g.control_fixed, |d| d.flex_none())
            .when(g.stacked, |d| d.max_w_full())
            .child(self.control);
        div()
            .flex()
            .w_full()
            .min_w_0()
            .when(self.padded, |d| d.px(px(16.0)).py(px(12.0)))
            .when(self.min_h > 0.0, |d| d.min_h(px(self.min_h)))
            .gap(px(g.gap))
            .when(g.stacked, |d| d.flex_col())
            .when(!g.stacked, |d| d.flex_row().items_center())
            .child(text)
            .child(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_by_side_row_never_lets_the_control_squeeze_the_text() {
        let g = RowGeometry::new(false);
        assert!(g.text_flexes && g.control_fixed && !g.stacked);
        assert_eq!(g.gap, 16.0);
    }

    #[test]
    fn stacked_row_puts_the_control_under_the_text() {
        let g = RowGeometry::new(true);
        assert!(g.stacked && !g.control_fixed);
    }
}
