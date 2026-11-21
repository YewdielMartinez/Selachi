//! The bar minimap (PLAN section 13): every line (or group of lines) is a horizontal pixel bar whose
//! width follows the line length, painted like the shark logo. Changed lines are `accent`.
//!
//! This module is the pure part: bucket data -> pixel rectangles, and click -> row mapping. The
//! painting lives in `views::minimap`.

use std::ops::Range;

/// Geometry of the strip, all in logical px. Bars sit on a 1 px grid (no antialiasing).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarSpec {
    pub bar_h: f32,
    pub gap: f32,
    /// Width of the strip available to bars.
    pub max_w: f32,
    /// A line this long (in bytes) fills the strip; longer ones are clipped to it.
    pub full_len: u16,
    /// Shortest visible bar for a non-empty line.
    pub min_w: f32,
}

impl Default for BarSpec {
    fn default() -> Self {
        Self { bar_h: 2.0, gap: 1.0, max_w: 56.0, full_len: 100, min_w: 2.0 }
    }
}

impl BarSpec {
    /// Height of one bar slot.
    pub fn pitch(&self) -> f32 {
        self.bar_h + self.gap
    }

    /// How many bars fit in a strip `height` px tall.
    pub fn buckets_for_height(&self, height: f32) -> usize {
        if height <= 0.0 { 0 } else { (height / self.pitch()).floor() as usize }
    }

    /// Width in px of the bar for a bucket whose longest line is `len` bytes. Empty lines draw
    /// nothing; widths are whole, even pixels (the pixel-art grid).
    pub fn bar_width(&self, len: u16) -> f32 {
        if len == 0 {
            return 0.0;
        }
        let frac = f32::from(len.min(self.full_len)) / f32::from(self.full_len.max(1));
        let w = (frac * self.max_w).round().max(self.min_w).min(self.max_w);
        // snap to even px so bars line up like the logo's
        (w / 2.0).round().max(1.0) * 2.0
    }
}

/// Width available to bars in a strip `strip_w` wide with `pad_x` of padding on each side
/// (whole, even pixels).
pub fn minimap_geometry_width(strip_w: f32, pad_x: f32) -> f32 {
    (((strip_w - 2.0 * pad_x) / 2.0).floor() * 2.0).max(8.0)
}

/// One bar to paint, in strip-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub y: f32,
    pub w: f32,
    pub changed: bool,
}

/// Bars for `buckets` (longest line per bucket) with `changed[i]` marking changed buckets (missing
/// entries count as unchanged). Rows without width are skipped unless changed (a changed empty
/// line still shows as a short bar so the change is visible).
pub fn bars(buckets: &[u16], changed: &[bool], spec: &BarSpec) -> Vec<Bar> {
    buckets
        .iter()
        .enumerate()
        .filter_map(|(i, &len)| {
            let is_changed = changed.get(i).copied().unwrap_or(false);
            let mut w = spec.bar_width(len);
            if w == 0.0 && is_changed {
                w = spec.min_w.max(2.0);
            }
            (w > 0.0).then_some(Bar { y: i as f32 * spec.pitch(), w, changed: is_changed })
        })
        .collect()
}

/// Bucket that row `row` of `n_rows` falls in when split into `buckets` groups. Same mapping as
/// `viewer_svc::minimap_from_lengths`.
pub fn bucket_of(row: usize, n_rows: usize, buckets: usize) -> usize {
    if n_rows == 0 || buckets == 0 {
        return 0;
    }
    let out_len = n_rows.min(buckets);
    (row as u128 * out_len as u128 / n_rows as u128) as usize
}

/// Which buckets contain at least one of `changed_rows`.
pub fn changed_flags(changed_rows: impl IntoIterator<Item = usize>, n_rows: usize, buckets: usize) -> Vec<bool> {
    let out_len = n_rows.min(buckets);
    let mut flags = vec![false; out_len];
    for row in changed_rows {
        if row < n_rows
            && let Some(f) = flags.get_mut(bucket_of(row, n_rows, buckets))
        {
            *f = true;
        }
    }
    flags
}

/// Row at vertical position `y` (px from the strip top) of a strip `height` tall showing `n_rows`.
pub fn row_at(y: f32, height: f32, n_rows: usize) -> usize {
    if n_rows == 0 || height <= 0.0 {
        return 0;
    }
    let frac = (y / height).clamp(0.0, 1.0);
    ((frac * n_rows as f32) as usize).min(n_rows - 1)
}

/// The viewport indicator: `(top, height)` px for `first..first+visible` of `n_rows`. At least 4 px
/// tall so it stays grabbable on huge files.
pub fn viewport_rect(first: usize, visible: usize, n_rows: usize, height: f32) -> (f32, f32) {
    if n_rows == 0 || height <= 0.0 {
        return (0.0, 0.0);
    }
    let per_row = height / n_rows as f32;
    let h = (visible as f32 * per_row).clamp(4.0f32.min(height), height);
    let top = (first as f32 * per_row).min(height - h).max(0.0);
    (top, h)
}

/// Rows of a visible window given the scroll offset (`offset_y <= 0`, as gpui reports it).
pub fn visible_rows(offset_y: f32, viewport_h: f32, row_h: f32, n_rows: usize) -> Range<usize> {
    if row_h <= 0.0 || n_rows == 0 {
        return 0..0;
    }
    let first = ((-offset_y).max(0.0) / row_h).floor() as usize;
    let last = (((-offset_y).max(0.0) + viewport_h) / row_h).ceil() as usize;
    first.min(n_rows)..last.min(n_rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_width_leaves_padding() {
        assert_eq!(minimap_geometry_width(72.0, 8.0), 56.0);
        assert_eq!(minimap_geometry_width(71.0, 8.0), 54.0);
        assert_eq!(minimap_geometry_width(10.0, 8.0), 8.0);
    }

    #[test]
    fn bucket_count_follows_height() {
        let s = BarSpec::default();
        assert_eq!(s.buckets_for_height(300.0), 100);
        assert_eq!(s.buckets_for_height(2.0), 0);
        assert_eq!(s.buckets_for_height(0.0), 0);
    }

    #[test]
    fn widths_are_even_pixels_and_clipped() {
        let s = BarSpec::default();
        assert_eq!(s.bar_width(0), 0.0);
        assert_eq!(s.bar_width(1), 2.0);
        assert_eq!(s.bar_width(100), 56.0);
        assert_eq!(s.bar_width(5_000), 56.0);
        for len in 1..=300u16 {
            let w = s.bar_width(len);
            assert!((2.0..=56.0).contains(&w), "{len} -> {w}");
            assert_eq!(w % 2.0, 0.0, "{len} -> {w}");
        }
        // Monotonic: longer lines never get shorter bars.
        let mut prev = 0.0;
        for len in 0..=200u16 {
            let w = s.bar_width(len);
            assert!(w >= prev);
            prev = w;
        }
    }

    #[test]
    fn bars_skip_empty_lines_but_keep_changed_ones() {
        let s = BarSpec::default();
        let out = bars(&[0, 50, 0, 100], &[false, false, true, true], &s);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], Bar { y: 3.0, w: 28.0, changed: false });
        assert_eq!(out[1], Bar { y: 6.0, w: 2.0, changed: true });
        assert_eq!(out[2], Bar { y: 9.0, w: 56.0, changed: true });
    }

    #[test]
    fn rows_map_to_the_same_buckets_as_the_service() {
        // 10 rows into 5 buckets: two rows per bucket.
        let flags = changed_flags([0, 1, 9], 10, 5);
        assert_eq!(flags, vec![true, false, false, false, true]);
        // fewer rows than buckets: one bucket per row.
        assert_eq!(changed_flags([2], 4, 100), vec![false, false, true, false]);
        assert_eq!(bucket_of(9, 10, 5), 4);
        // agrees with viewer_svc::minimap_from_lengths
        let lens: Vec<u16> = (0..10).map(|i| if i == 7 { 90 } else { 1 }).collect();
        let m = viewer_svc::minimap_from_lengths(lens.iter().copied(), 10, 5);
        assert_eq!(m[bucket_of(7, 10, 5)], 90);
    }

    #[test]
    fn click_maps_to_rows() {
        assert_eq!(row_at(0.0, 300.0, 1_000), 0);
        assert_eq!(row_at(150.0, 300.0, 1_000), 500);
        assert_eq!(row_at(300.0, 300.0, 1_000), 999);
        assert_eq!(row_at(-20.0, 300.0, 1_000), 0);
        assert_eq!(row_at(999.0, 300.0, 1_000), 999);
        assert_eq!(row_at(10.0, 300.0, 0), 0);
    }

    #[test]
    fn viewport_indicator() {
        let (top, h) = viewport_rect(0, 50, 1_000, 300.0);
        assert_eq!(top, 0.0);
        assert!((h - 15.0).abs() < 0.01);
        // never smaller than 4 px, never past the bottom
        let (top, h) = viewport_rect(999_990, 40, 1_000_000, 300.0);
        assert_eq!(h, 4.0);
        assert!(top + h <= 300.0 + 0.001);
        assert_eq!(viewport_rect(0, 10, 0, 300.0), (0.0, 0.0));
    }

    #[test]
    fn visible_row_window() {
        assert_eq!(visible_rows(0.0, 100.0, 20.0, 1_000), 0..5);
        assert_eq!(visible_rows(-45.0, 100.0, 20.0, 1_000), 2..8);
        assert_eq!(visible_rows(-1_000_000.0, 100.0, 20.0, 10), 10..10);
    }
}
