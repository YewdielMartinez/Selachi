//! `DitherMeter`: the product's signature pixel meter.

use gpui::{App, IntoElement, RenderOnce, Rgba, Window, div, prelude::*, px};
use theme::Theme;

/// Pixel size of one dither cell; a segment is 2 x 5 cells (4 x 10 px).
const CELL: f32 = 2.0;

/// Segmented pixel meter. Solid `accent` segments for the filled part, then one half-tone
/// (checkerboard) segment when the remainder is at least a quarter segment; the rest is
/// `surface_3`. `value: None` means "unknown": every segment is empty, nothing is invented.
///
/// ```ignore
/// DitherMeter::new(&theme).value(Some(0.42))      // 8 segments, 42 %
/// DitherMeter::new(&theme).segments(12).value(None)
/// ```
#[derive(IntoElement)]
pub struct DitherMeter {
    theme: Theme,
    segments: usize,
    value: Option<f32>,
    accent: Option<Rgba>,
}

impl DitherMeter {
    pub fn new(theme: &Theme) -> Self {
        Self {
            theme: *theme,
            segments: 8,
            value: None,
            accent: None,
        }
    }

    /// Number of segments (default 8).
    pub fn segments(mut self, segments: usize) -> Self {
        self.segments = segments.max(1);
        self
    }

    /// Fill fraction `0.0..=1.0`; `None` = unknown.
    pub fn value(mut self, value: Option<f32>) -> Self {
        self.value = value.filter(|v| v.is_finite());
        self
    }

    /// Fill color (default: the theme accent).
    pub fn accent(mut self, accent: Rgba) -> Self {
        self.accent = Some(accent);
        self
    }

    /// `(solid segments, half-tone segment present)` for the current value.
    pub(crate) fn fill(&self) -> (usize, bool) {
        let Some(v) = self.value else {
            return (0, false);
        };
        let exact = f64::from(v.clamp(0.0, 1.0)) * self.segments as f64;
        let filled = (exact.floor() as usize).min(self.segments);
        (
            filled,
            filled < self.segments && exact - exact.floor() >= 0.25,
        )
    }
}

impl RenderOnce for DitherMeter {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let accent = self.accent.unwrap_or(c.accent);
        let (filled, half) = self.fill();
        let seg = move |i: usize| {
            let col = div()
                .flex_none()
                .flex()
                .flex_row()
                .flex_wrap()
                .w(px(CELL * 2.0))
                .h(px(CELL * 5.0));
            if i < filled {
                col.bg(accent)
            } else if i == filled && half {
                col.children((0..10).map(move |k| {
                    // 2 columns x 5 rows, alternating cells.
                    let on = (k / 2 + k % 2) % 2 == 0;
                    div().size(px(CELL)).when(on, |d| d.bg(accent))
                }))
            } else {
                col.bg(c.surface_3)
            }
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .children((0..self.segments).map(seg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(segments: usize, v: Option<f32>) -> (usize, bool) {
        DitherMeter::new(&Theme::dark())
            .segments(segments)
            .value(v)
            .fill()
    }

    #[test]
    fn unknown_is_empty() {
        assert_eq!(fill(8, None), (0, false));
        assert_eq!(fill(8, Some(f32::NAN)), (0, false));
    }

    #[test]
    fn full_and_overfull_clamp() {
        assert_eq!(fill(8, Some(1.0)), (8, false));
        assert_eq!(fill(8, Some(3.0)), (8, false));
        assert_eq!(fill(8, Some(-1.0)), (0, false));
    }

    #[test]
    fn half_tone_needs_a_quarter_segment() {
        // 0.5 of 8 = 4.0 exactly -> 4 solid, no half-tone.
        assert_eq!(fill(8, Some(0.5)), (4, false));
        // 4.2 segments: remainder 0.2 < 0.25.
        assert_eq!(fill(8, Some(0.525)), (4, false));
        // 4.4 segments: remainder 0.4 -> half-tone.
        assert_eq!(fill(8, Some(0.55)), (4, true));
    }
}
