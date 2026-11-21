//! The 14-day pixel bar chart: one column per local day, each bar a stack of square-ish cells
//! (one tone per agent, muted; today in the accent), painted with `paint_quad`. Every column is a
//! hover target with its own tooltip.

use chrono::NaiveDate;
use gpui::{
    Bounds, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, StatefulInteractiveElement,
    Styled, Window, canvas, div, fill, point, px, size,
};
use theme::Theme;
use ui_kit::Tooltip;

use crate::data::{AGENT_ORDER, DayUsage, day_tooltip, stack_cells};

/// Height of one cell and the vertical pitch (cell + 1 px gap).
const CELL_H: f32 = 4.0;
const PITCH: f32 = CELL_H + 1.0;
/// Widest bar, in px (columns are narrower in a thin panel).
const MAX_BAR_W: f32 = 18.0;
/// Opacity of each stack slot (Claude, Codex, OpenCode): muted tones of one hue.
const SLOT_OPACITY: [f32; 3] = [1.0, 0.62, 0.36];

/// The tone of stack `slot`: muted for past days, accent for today.
pub fn slot_tone(theme: &Theme, slot: usize, today: bool) -> Hsla {
    let base: Hsla = if today { theme.colors.accent.into() } else { theme.colors.muted.into() };
    base.opacity(SLOT_OPACITY.get(slot).copied().unwrap_or(0.36))
}

/// Geometry of one bar inside a column of `col_w` x `col_h`: `(bar_w, rows)`.
pub fn bar_geometry(col_w: f32, col_h: f32) -> (f32, usize) {
    let bar_w = (col_w - 4.0).clamp(2.0, MAX_BAR_W).floor();
    let rows = ((col_h / PITCH).floor() as usize).max(1);
    (bar_w, rows)
}

/// The chart: columns for `days` (oldest first), `height` px tall. `today` is highlighted.
pub fn bar_chart(
    theme: &Theme,
    days: &[DayUsage],
    today: NaiveDate,
    height: f32,
    day_numbers: bool,
) -> impl IntoElement + use<> {
    let c = theme.colors;
    let max_total = days.iter().map(DayUsage::total).max().unwrap_or(0);
    let tones: Vec<[Hsla; 3]> = [false, true]
        .into_iter()
        .map(|t| [slot_tone(theme, 0, t), slot_tone(theme, 1, t), slot_tone(theme, 2, t)])
        .collect();
    let baseline = c.surface_4;

    let columns = days.iter().enumerate().map(|(i, day)| {
        let is_today = day.date == today;
        let tip = day_tooltip(day, today);
        let values = day.per_agent;
        let tone = tones[usize::from(is_today)];
        div()
            .id(("usage-day", i))
            .flex_1()
            .min_w_0()
            .h_full()
            .tooltip(Tooltip::text(theme, tip))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
                        paint_bar(window, bounds, &values, max_total, tone, baseline);
                    },
                )
                .size_full(),
            )
    });

    let numbers = day_numbers.then(|| {
        div().flex().flex_row().children(days.iter().map(|day| {
            let color = if day.date == today { c.text } else { c.muted };
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_center()
                .text_size(px(10.0))
                .text_color(color)
                .child(chrono::Datelike::day(&day.date).to_string())
        }))
    });

    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(div().flex().flex_row().h(px(height)).children(columns))
        .children(numbers)
}

fn paint_bar(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[u64; AGENT_ORDER.len()],
    max_total: u64,
    tone: [Hsla; 3],
    baseline: theme::Rgba,
) {
    let (col_w, col_h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (bar_w, rows) = bar_geometry(col_w, col_h);
    let x = bounds.origin.x + px(((col_w - bar_w) / 2.0).floor());
    let bottom = bounds.origin.y + bounds.size.height;
    if values.iter().sum::<u64>() == 0 {
        // A thin baseline so quiet days still read as columns.
        window.paint_quad(fill(Bounds::new(point(x, bottom - px(2.0)), size(px(bar_w), px(2.0))), baseline));
        return;
    }
    let cells = stack_cells(values, max_total, rows);
    let mut level = 0usize;
    for (slot, n) in cells.iter().enumerate() {
        for _ in 0..*n {
            let y = bottom - px(CELL_H + PITCH * level as f32);
            window.paint_quad(fill(Bounds::new(point(x, y), size(px(bar_w), px(CELL_H))), tone[slot]));
            level += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_adapts_to_the_column() {
        // Wide column: capped bar, rows from the height.
        assert_eq!(bar_geometry(40.0, 96.0), (18.0, 19));
        // Thin column keeps a 2 px minimum.
        assert_eq!(bar_geometry(5.0, 20.0), (2.0, 4));
        assert_eq!(bar_geometry(20.0, 1.0), (16.0, 1));
    }

    #[test]
    fn today_uses_the_accent_and_the_past_the_muted_tone() {
        let t = Theme::dark();
        let accent: Hsla = t.colors.accent.into();
        let muted: Hsla = t.colors.muted.into();
        assert_eq!(slot_tone(&t, 0, true), accent);
        assert_eq!(slot_tone(&t, 0, false), muted);
        assert!(slot_tone(&t, 1, false).a < slot_tone(&t, 0, false).a);
    }
}
