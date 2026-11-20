//! `Wireframe`: a layout preset drawn as 1-bit pixel-art blocks (thumbnails of the layout panel).

use gpui::{App, Bounds, IntoElement, RenderOnce, Window, canvas, div, fill, point, prelude::*, px, size};
use theme::Theme;

/// How a block is filled. The thumbnails are 1-bit: a cell is either ink or paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFill {
    /// Every cell is ink (rail, tab pills, status chips).
    Solid,
    /// Checkerboard (sidebar, side panels: "there is content here").
    Dither,
    /// One-cell frame (the main work card).
    Outline,
}

/// A block on the wireframe grid, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireRect {
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
    pub fill: WireFill,
}

impl WireRect {
    pub const fn new(x: u8, y: u8, w: u8, h: u8, fill: WireFill) -> Self {
        Self { x, y, w, h, fill }
    }
}

/// The five layout presets of PLAN section 17, as block descriptions on the default 32 x 20
/// grid. Zones are: rail, sidebar, tabs, main, right panel and status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirePreset {
    Terminal,
    Spaces,
    Orchestrator,
    Chat,
    Zen,
}

impl WirePreset {
    pub const ALL: [WirePreset; 5] = [
        WirePreset::Terminal,
        WirePreset::Spaces,
        WirePreset::Orchestrator,
        WirePreset::Chat,
        WirePreset::Zen,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            WirePreset::Terminal => "Terminal",
            WirePreset::Spaces => "Espacios",
            WirePreset::Orchestrator => "Orquestador",
            WirePreset::Chat => "Chat",
            WirePreset::Zen => "Zen",
        }
    }

    /// The blocks of this preset.
    pub fn rects(self) -> Vec<WireRect> {
        use WireFill::{Dither, Outline, Solid};
        let r = WireRect::new;
        match self {
            WirePreset::Terminal => vec![
                r(0, 0, 2, 20, Solid),
                r(3, 0, 6, 2, Solid),
                r(10, 0, 6, 2, Dither),
                r(3, 3, 29, 14, Outline),
                r(3, 18, 7, 2, Dither),
                r(11, 18, 5, 2, Dither),
            ],
            WirePreset::Spaces => vec![
                r(0, 0, 2, 20, Solid),
                r(3, 0, 8, 17, Dither),
                r(12, 0, 6, 2, Solid),
                r(19, 0, 6, 2, Dither),
                r(12, 3, 20, 14, Outline),
                r(3, 18, 7, 2, Dither),
                r(11, 18, 5, 2, Dither),
            ],
            WirePreset::Orchestrator => vec![
                r(0, 0, 2, 20, Solid),
                r(3, 0, 9, 17, Dither),
                r(4, 3, 7, 1, Solid),
                r(4, 6, 7, 1, Solid),
                r(4, 9, 7, 1, Solid),
                r(13, 3, 9, 14, Outline),
                r(23, 3, 9, 14, Outline),
                r(3, 18, 12, 2, Dither),
                r(16, 18, 8, 2, Dither),
            ],
            WirePreset::Chat => vec![
                r(0, 0, 2, 20, Solid),
                r(3, 0, 8, 20, Dither),
                r(12, 1, 12, 14, Outline),
                r(13, 15, 10, 3, Solid),
                r(25, 1, 7, 17, Dither),
            ],
            WirePreset::Zen => vec![r(1, 1, 30, 18, Outline)],
        }
    }
}

/// Thumbnail of a layout. 32 x 20 cells of 4 px by default (128 x 80 plus padding).
///
/// ```ignore
/// Wireframe::preset(&theme, WirePreset::Orchestrator).selected(true)
/// Wireframe::new(&theme).rect(WireRect::new(0, 0, 2, 20, WireFill::Solid))
/// ```
#[derive(IntoElement)]
pub struct Wireframe {
    theme: Theme,
    cols: u8,
    rows: u8,
    cell: f32,
    rects: Vec<WireRect>,
    selected: bool,
}

impl Wireframe {
    pub fn new(theme: &Theme) -> Self {
        Self { theme: *theme, cols: 32, rows: 20, cell: 4.0, rects: Vec::new(), selected: false }
    }

    pub fn preset(theme: &Theme, preset: WirePreset) -> Self {
        Self::new(theme).rects(preset.rects())
    }

    pub fn rect(mut self, rect: WireRect) -> Self {
        self.rects.push(rect);
        self
    }

    pub fn rects(mut self, rects: impl IntoIterator<Item = WireRect>) -> Self {
        self.rects.extend(rects);
        self
    }

    /// Grid size in cells (default 32 x 20).
    pub fn grid(mut self, cols: u8, rows: u8) -> Self {
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        self
    }

    /// Cell edge in px (default 4).
    pub fn cell(mut self, cell: f32) -> Self {
        self.cell = cell.max(1.0);
        self
    }

    /// Selected thumbnails use a lighter tile and full-strength ink.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

/// Cells (x, y) of a block that are ink, in grid coordinates. Pure so it can be tested.
pub(crate) fn ink_cells(r: WireRect, cols: u8, rows: u8) -> Vec<(u8, u8)> {
    let x1 = r.x.saturating_add(r.w).min(cols);
    let y1 = r.y.saturating_add(r.h).min(rows);
    let mut out = Vec::new();
    for y in r.y..y1 {
        for x in r.x..x1 {
            let on = match r.fill {
                WireFill::Solid => true,
                WireFill::Dither => (x + y) % 2 == 0,
                WireFill::Outline => x == r.x || y == r.y || x == x1 - 1 || y == y1 - 1,
            };
            if on {
                out.push((x, y));
            }
        }
    }
    out
}

impl RenderOnce for Wireframe {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let (tile, ink) = if self.selected { (c.surface_3, c.text) } else { (c.surface_2, c.outline) };
        let (cols, rows, cell) = (self.cols, self.rows, self.cell);
        let rects = self.rects;
        let w = f32::from(cols) * cell;
        let h = f32::from(rows) * cell;
        div()
            .flex_none()
            .p(px(8.0))
            .rounded(px(10.0))
            .bg(tile)
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        for r in &rects {
                            for (x, y) in ink_cells(*r, cols, rows) {
                                let origin = point(
                                    bounds.origin.x + px(f32::from(x) * cell),
                                    bounds.origin.y + px(f32::from(y) * cell),
                                );
                                window.paint_quad(fill(Bounds::new(origin, size(px(cell), px(cell))), ink));
                            }
                        }
                    },
                )
                .w(px(w))
                .h(px(h)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_covers_the_block() {
        assert_eq!(ink_cells(WireRect::new(1, 1, 3, 2, WireFill::Solid), 32, 20).len(), 6);
    }

    #[test]
    fn outline_is_hollow() {
        let cells = ink_cells(WireRect::new(0, 0, 4, 4, WireFill::Outline), 32, 20);
        assert_eq!(cells.len(), 12);
        assert!(!cells.contains(&(1, 1)));
    }

    #[test]
    fn dither_is_a_checkerboard_and_blocks_clip_to_the_grid() {
        assert_eq!(ink_cells(WireRect::new(0, 0, 4, 4, WireFill::Dither), 32, 20).len(), 8);
        assert!(ink_cells(WireRect::new(30, 18, 10, 10, WireFill::Solid), 32, 20).len() <= 4);
    }

    #[test]
    fn presets_fit_the_default_grid() {
        for p in WirePreset::ALL {
            for r in p.rects() {
                assert!(u16::from(r.x) + u16::from(r.w) <= 32, "{p:?} {r:?}");
                assert!(u16::from(r.y) + u16::from(r.h) <= 20, "{p:?} {r:?}");
            }
        }
    }
}
