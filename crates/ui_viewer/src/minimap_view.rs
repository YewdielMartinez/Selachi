//! The bar minimap element: horizontal pixel bars per line group (PLAN section 13), changed rows in
//! the accent, a translucent viewport marker. Painting reads the list's scroll state at paint time,
//! so it never needs its own timer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{Bounds, IntoElement, Pixels, Styled as _, canvas, fill, point, px, size};
use theme::{Rgba, Theme};
use ui_kit::UniformListScrollHandle;
use viewer_svc::Document;

use crate::minimap::{
    BarSpec, bars, changed_flags, minimap_geometry_width, viewport_rect, visible_rows,
};

/// Width of the strip, including its padding.
pub const STRIP_W: f32 = 72.0;
const PAD_X: f32 = 8.0;

/// Where the bars come from.
#[derive(Clone)]
pub enum MinimapSource {
    /// A document's line lengths (code view).
    Doc(Document),
    /// Precomputed row lengths plus the changed rows (diff view).
    Rows {
        lens: Rc<Vec<u16>>,
        changed: Rc<Vec<usize>>,
    },
}

impl MinimapSource {
    /// Number of rows the strip stands for.
    pub fn total(&self) -> usize {
        match self {
            MinimapSource::Doc(d) => d.line_count().max(d.estimated_line_count()),
            MinimapSource::Rows { lens, .. } => lens.len(),
        }
    }

    fn buckets(&self, n: usize) -> Vec<u16> {
        match self {
            MinimapSource::Doc(d) => d.minimap(0..self.total(), n),
            MinimapSource::Rows { lens, .. } => {
                viewer_svc::minimap_from_lengths(lens.iter().copied(), lens.len(), n)
            }
        }
    }

    fn changed(&self) -> &[usize] {
        match self {
            MinimapSource::Doc(_) => &[],
            MinimapSource::Rows { changed, .. } => changed,
        }
    }
}

struct Cached {
    key: (u64, usize, usize),
    lens: Vec<u16>,
    flags: Vec<bool>,
}

/// State shared between the element (written while painting) and the viewer (read by click handling).
#[derive(Clone, Default)]
pub struct MinimapShared {
    /// Bounds of the strip at the last paint.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    cache: Rc<RefCell<Option<Cached>>>,
}

/// The strip. `generation` changes whenever `source` does (a new diff, a reloaded file) so the
/// cached buckets are dropped.
pub fn minimap(
    theme: Theme,
    source: MinimapSource,
    scroll: UniformListScrollHandle,
    row_h: f32,
    shared: &MinimapShared,
    generation: u64,
) -> impl IntoElement + use<> {
    let shared = shared.clone();
    canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            shared.bounds.set(bounds);
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let spec = BarSpec {
                max_w: minimap_geometry_width(w, PAD_X),
                ..BarSpec::default()
            };
            let n = spec.buckets_for_height(h);
            let total = source.total();
            if n == 0 || total == 0 {
                return;
            }
            let c = theme.colors;
            let mut cache = shared.cache.borrow_mut();
            let key = (generation, n, total);
            if cache.as_ref().is_none_or(|c| c.key != key) {
                let lens = source.buckets(n);
                let flags = changed_flags(source.changed().iter().copied(), total, n);
                *cache = Some(Cached { key, lens, flags });
            }
            let Some(cached) = cache.as_ref() else { return };

            let x0 = bounds.origin.x + px(PAD_X);
            let y0 = bounds.origin.y;
            let calm: Rgba = c.surface_4;
            for bar in bars(&cached.lens, &cached.flags, &spec) {
                let color = if bar.changed { c.accent } else { calm };
                window.paint_quad(fill(
                    Bounds::new(
                        point(x0, y0 + px(bar.y.floor())),
                        size(px(bar.w), px(spec.bar_h)),
                    ),
                    color,
                ));
            }

            // Viewport marker.
            let (offset_y, viewport_h) = {
                let st = scroll.0.borrow();
                let viewport = st.last_item_size.map_or(0.0, |s| f32::from(s.item.height));
                (f32::from(st.base_handle.offset().y), viewport)
            };
            let rows = visible_rows(offset_y, viewport_h, row_h, total);
            if !rows.is_empty() {
                let (top, height) = viewport_rect(rows.start, rows.len(), total, h);
                let mut tint = c.text;
                tint.a = 0.10;
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x, y0 + px(top.floor())),
                        size(bounds.size.width, px(height.ceil())),
                    ),
                    tint,
                ));
            }
        },
    )
    .size_full()
}
