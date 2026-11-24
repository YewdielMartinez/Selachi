//! Immutable copies of the visible terminal viewport, for UI rendering.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};

use crate::color::{Palette, Rgb8, dim, resolve_color};

/// Text attributes of a rendered cell.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CellFlags {
    /// Bold weight.
    pub bold: bool,
    /// Italic style.
    pub italic: bool,
    /// Any kind of underline.
    pub underline: bool,
    /// Strikethrough.
    pub strikethrough: bool,
    /// Dim (already applied to the foreground color; informational).
    pub dim: bool,
    /// First cell of a double-width character.
    pub wide: bool,
    /// Trailing spacer cell of a double-width character (draw nothing).
    pub wide_spacer: bool,
}

/// One rendered terminal cell with colors fully resolved.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CellView {
    /// Character (`' '` when empty or hidden).
    pub ch: char,
    /// Foreground color; inverse video and dim are already applied.
    pub fg: Rgb8,
    /// Background color; inverse video is already applied.
    pub bg: Rgb8,
    /// Text attributes.
    pub flags: CellFlags,
}

/// A copy of the visible viewport.
///
/// Reuse one instance across frames with [`crate::Session::snapshot_into`]: the row buffers keep
/// their capacity, so steady-state frames allocate nothing.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Number of columns.
    pub cols: usize,
    /// Number of rows.
    pub rows: usize,
    /// `rows` lines of `cols` cells each.
    pub lines: Vec<Vec<CellView>>,
    /// One content hash per row (characters, colors and attributes). Equal hashes mean the row
    /// paints identically, so a renderer can cache shaped text by this value.
    pub row_hash: Vec<u64>,
    /// Cursor position as `(row, col)` in the viewport; `None` if hidden or scrolled out of view.
    pub cursor: Option<(usize, usize)>,
    /// Lines scrolled up into history (0 = bottom).
    pub display_offset: usize,
    /// Whether application cursor keys mode (DECCKM) is active.
    pub app_cursor: bool,
}

/// Mixes `x` into the running hash `h` (multiply-rotate; cheap and good enough for cache keys).
#[inline]
fn mix(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29)
}

#[inline]
fn pack(c: Rgb8) -> u64 {
    u64::from(c.r) << 16 | u64::from(c.g) << 8 | u64::from(c.b)
}

/// Copies the visible viewport of `term` into `out`, reusing its buffers.
pub(crate) fn build_snapshot_into<L: EventListener>(
    term: &Term<L>,
    palette: &Palette,
    out: &mut Snapshot,
) {
    let grid = term.grid();
    let cols = term.columns();
    let rows = term.screen_lines();
    let offset = grid.display_offset();
    let overrides = term.colors();

    out.lines.truncate(rows);
    out.lines.resize_with(rows, Vec::new);
    out.row_hash.clear();
    for r in 0..rows {
        let row = &grid[Line(r as i32 - offset as i32)];
        let line = &mut out.lines[r];
        line.clear();
        line.reserve(cols);
        let mut hash = cols as u64;
        for c in 0..cols {
            let cell = &row[Column(c)];
            let mut fg = resolve_color(cell.fg, palette, overrides);
            let mut bg = resolve_color(cell.bg, palette, overrides);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let is_dim = cell.flags.contains(Flags::DIM);
            if is_dim {
                fg = dim(fg);
            }
            let ch = if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            let flags = CellFlags {
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                strikethrough: cell.flags.contains(Flags::STRIKEOUT),
                dim: is_dim,
                wide: cell.flags.contains(Flags::WIDE_CHAR),
                wide_spacer: cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            };
            let attrs = u64::from(flags.bold)
                | u64::from(flags.italic) << 1
                | u64::from(flags.underline) << 2
                | u64::from(flags.strikethrough) << 3
                | u64::from(flags.wide) << 4
                | u64::from(flags.wide_spacer) << 5;
            hash = mix(hash, u64::from(ch) | attrs << 32 | pack(fg) << 40);
            hash = mix(hash, pack(bg));
            line.push(CellView { ch, fg, bg, flags });
        }
        out.row_hash.push(hash);
    }

    let mode = term.mode();
    out.cursor = if mode.contains(TermMode::SHOW_CURSOR) {
        let p = grid.cursor.point;
        let row = p.line.0 + offset as i32;
        let mut col = p.column.0;
        // Show the cursor on the head of a wide character, not on its spacer.
        if col > 0
            && grid[p.line][p.column]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        {
            col -= 1;
        }
        if row >= 0 && (row as usize) < rows && col < cols {
            Some((row as usize, col))
        } else {
            None
        }
    } else {
        None
    };
    out.cols = cols;
    out.rows = rows;
    out.display_offset = offset;
    out.app_cursor = mode.contains(TermMode::APP_CURSOR);
}

impl Snapshot {
    /// Returns the text of viewport row `row` with trailing spaces trimmed.
    pub fn line_text(&self, row: usize) -> String {
        self.lines
            .get(row)
            .map(|l| {
                let s: String = l
                    .iter()
                    .filter(|c| !c.flags.wide_spacer)
                    .map(|c| c.ch)
                    .collect();
                s.trim_end().to_string()
            })
            .unwrap_or_default()
    }

    /// Returns the whole viewport as text, one line per row, trailing spaces trimmed.
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|r| self.line_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    struct Dims(usize, usize);
    impl Dimensions for Dims {
        fn total_lines(&self) -> usize {
            self.0
        }
        fn screen_lines(&self) -> usize {
            self.0
        }
        fn columns(&self) -> usize {
            self.1
        }
    }

    fn term_with(text: &str) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &Dims(4, 10), VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::default()
            .advance(&mut term, text.as_bytes());
        term
    }

    #[test]
    fn row_hash_tracks_content_and_style() {
        let palette = Palette::default();
        let mut snap = Snapshot::default();
        build_snapshot_into(
            &term_with("abc\r\nabc\r\n\x1b[31mabc\x1b[0m"),
            &palette,
            &mut snap,
        );
        assert_eq!(snap.row_hash.len(), 4);
        assert_eq!(
            snap.row_hash[0], snap.row_hash[1],
            "identical rows hash equally"
        );
        assert_ne!(
            snap.row_hash[1], snap.row_hash[2],
            "a color change changes the hash"
        );
        assert_ne!(
            snap.row_hash[0], snap.row_hash[3],
            "blank row differs from text row"
        );
    }

    #[test]
    fn snapshot_into_reuses_and_resizes_buffers() {
        let palette = Palette::default();
        let mut snap = Snapshot::default();
        build_snapshot_into(&term_with("hello"), &palette, &mut snap);
        assert_eq!((snap.rows, snap.cols), (4, 10));
        assert_eq!(snap.line_text(0), "hello");
        let cap = snap.lines[0].capacity();
        build_snapshot_into(&term_with("bye"), &palette, &mut snap);
        assert_eq!(snap.line_text(0), "bye");
        assert_eq!(snap.lines[0].capacity(), cap, "row buffer capacity is kept");
        assert_eq!(snap.row_hash.len(), snap.rows);
    }
}
