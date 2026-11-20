//! Converts `store::wireframe` (0..100 float zones) into ui_kit pixel-art blocks on a cell grid.

use store::{ResolvedLayout, Splits, Zone};
use ui_kit::{WireFill, WireRect, Wireframe};
use theme::Theme;

/// Grid of a preset card thumbnail.
pub const THUMB: (u8, u8, f32) = (26, 20, 3.0);
/// Grid of the live preview.
pub const PREVIEW: (u8, u8, f32) = (48, 30, 3.0);

fn edge(v: f32, n: u8) -> u8 {
    ((v / 100.0 * f32::from(n)).round().clamp(0.0, f32::from(n))) as u8
}

/// One zone on the cell grid, half-open `x0..x1` x `y0..y1`.
struct Cells {
    zone: Zone,
    x0: u8,
    y0: u8,
    x1: u8,
    y1: u8,
}

impl Cells {
    fn w(&self) -> u8 {
        self.x1 - self.x0
    }
    fn h(&self) -> u8 {
        self.y1 - self.y0
    }
    /// Horizontal tab strip (as opposed to vertical rows inside the sidebar).
    fn is_tab_strip(&self) -> bool {
        self.zone == Zone::Tabs && self.h() <= 3 && self.w() >= 2 * self.h() + 4
    }
}

/// Pixel-art blocks for `layout` on a `cols` x `rows` grid. Zone edges are rounded, so zones
/// that tile in the 0..100 space also tile on the grid; one cell of gutter is then carved out of
/// the right / bottom of each block so neighbours read as separate pieces.
pub fn blocks(layout: &ResolvedLayout, cols: u8, rows: u8) -> Vec<WireRect> {
    let mut zones: Vec<Cells> = store::wireframe(layout)
        .iter()
        .map(|r| {
            let (x0, x1) = (edge(r.x, cols), edge(r.x + r.w, cols));
            let (y0, y1) = (edge(r.y, rows), edge(r.y + r.h, rows));
            let (w, h) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
            let x0 = x0.min(cols.saturating_sub(w));
            let y0 = y0.min(rows.saturating_sub(h));
            Cells { zone: r.zone, x0, y0, x1: x0 + w, y1: y0 + h }
        })
        .collect();
    let strips: Vec<u8> = zones.iter().filter(|z| z.is_tab_strip()).map(|z| z.y1).collect();
    for z in &mut zones {
        let strip = z.is_tab_strip();
        // The pills of a tab strip keep their height; whatever sits under them moves down.
        if z.zone == Zone::Main && strips.contains(&z.y0) && z.h() > 3 {
            z.y0 += 1;
        }
        if z.x1 < cols && z.w() >= 2 && !strip {
            z.x1 -= 1;
        }
        if z.y1 < rows && z.h() >= 2 && !strip {
            z.y1 -= 1;
        }
    }

    let mut out = Vec::new();
    for z in &zones {
        let (x0, y0, w, h) = (z.x0, z.y0, z.w(), z.h());
        match z.zone {
            Zone::Rail => out.push(WireRect::new(x0, y0, w, h, WireFill::Solid)),
            Zone::Sidebar | Zone::Right => out.push(WireRect::new(x0, y0, w, h, WireFill::Dither)),
            Zone::Main => out.push(WireRect::new(x0, y0, w, h, WireFill::Outline)),
            Zone::Tabs if z.is_tab_strip() => {
                // Tab pills along the top: one active (solid), one idle (dither).
                let pill = (w / 4).max(2);
                out.push(WireRect::new(x0, y0, pill, h, WireFill::Solid));
                if w > 2 * pill + 1 {
                    out.push(WireRect::new(x0 + pill + 1, y0, pill, h, WireFill::Dither));
                }
            }
            Zone::Tabs => {
                // Vertical tabs inside the sidebar: a stack of one-cell rows.
                let mut y = y0 + 1;
                while y < y0 + h {
                    out.push(WireRect::new(x0 + 1, y, w.saturating_sub(2).max(1), 1, WireFill::Solid));
                    y += 2;
                }
            }
            Zone::Status => {
                let chip = (w / 4).max(2);
                out.push(WireRect::new(x0 + 1, y0, chip, h, WireFill::Dither));
                if w > 2 * chip + 3 {
                    out.push(WireRect::new(x0 + chip + 2, y0, chip, h, WireFill::Dither));
                }
            }
        }
    }
    out
}

/// A thumbnail/preview `Wireframe` of a resolved layout.
pub fn wireframe(theme: &Theme, layout: &ResolvedLayout, grid: (u8, u8, f32)) -> Wireframe {
    Wireframe::new(theme).grid(grid.0, grid.1).cell(grid.2).rects(blocks(layout, grid.0, grid.1))
}

/// The tiny split icons of the "Splits" row (grid 10 x 7).
pub fn split_blocks(s: Splits) -> Vec<WireRect> {
    use WireFill::Outline;
    let r = |x, y, w, h| WireRect::new(x, y, w, h, Outline);
    match s {
        Splits::Single => vec![r(0, 0, 10, 7)],
        Splits::TwoVertical => vec![r(0, 0, 5, 7), r(5, 0, 5, 7)],
        Splits::TwoHorizontal => vec![r(0, 0, 10, 4), r(0, 3, 10, 4)],
        Splits::OneAndTwoStacked => vec![r(0, 0, 5, 7), r(5, 0, 5, 4), r(5, 3, 5, 4)],
        Splits::Grid2x2 => vec![r(0, 0, 5, 4), r(5, 0, 5, 4), r(0, 3, 5, 4), r(5, 3, 5, 4)],
    }
}

pub fn split_icon(theme: &Theme, s: Splits, selected: bool) -> Wireframe {
    Wireframe::new(theme).grid(10, 7).cell(3.0).rects(split_blocks(s)).selected(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::BuiltinPreset;

    #[test]
    fn every_preset_fits_both_grids_and_has_a_main_card() {
        for b in BuiltinPreset::ALL {
            let l = ResolvedLayout::builtin(b);
            for (cols, rows, _) in [THUMB, PREVIEW] {
                let rects = blocks(&l, cols, rows);
                assert!(!rects.is_empty(), "{b:?}");
                assert!(
                    rects.iter().any(|r| r.fill == WireFill::Outline),
                    "{b:?} has no main card"
                );
                for r in &rects {
                    assert!(u16::from(r.x) + u16::from(r.w) <= u16::from(cols), "{b:?} {r:?}");
                    assert!(u16::from(r.y) + u16::from(r.h) <= u16::from(rows), "{b:?} {r:?}");
                    assert!(r.w >= 1 && r.h >= 1);
                }
            }
        }
    }

    #[test]
    fn zen_is_just_the_terminal() {
        let l = ResolvedLayout::builtin(BuiltinPreset::Zen);
        let rects = blocks(&l, THUMB.0, THUMB.1);
        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0].x, rects[0].y, rects[0].w, rects[0].h), (0, 0, THUMB.0, THUMB.1));
    }

    #[test]
    fn different_presets_draw_differently() {
        let a = blocks(&ResolvedLayout::builtin(BuiltinPreset::Terminal), 48, 30);
        let b = blocks(&ResolvedLayout::builtin(BuiltinPreset::Orquestador), 48, 30);
        assert_ne!(a, b);
    }

    #[test]
    fn split_icons_fit_their_grid() {
        for s in [
            Splits::Single,
            Splits::TwoVertical,
            Splits::TwoHorizontal,
            Splits::OneAndTwoStacked,
            Splits::Grid2x2,
        ] {
            for r in split_blocks(s) {
                assert!(r.x + r.w <= 10 && r.y + r.h <= 7, "{s:?} {r:?}");
            }
        }
    }
}
