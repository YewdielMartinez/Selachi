//! Ctrl+click paths in terminal output (PLAN section 13).
//!
//! While Ctrl is held and the pointer moves, the hovered row is scanned with
//! `viewer_svc::find_paths`; the matches are cached by the row's content hash (plus the working
//! directory they were resolved against), and whether a candidate exists on disk is asked lazily
//! with one `stat` the first time the pointer is over it. Nothing runs without Ctrl, so the idle
//! and the normal-typing cost is zero.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use term_core::CellView;
use viewer_svc::{PathMatch, Resolved};

/// Most rows kept in the cache (a screen is ~50; scrolling through history stays bounded).
const CACHE_ROWS: usize = 512;

/// A path under the pointer that exists on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkHit {
    /// Columns to underline, `start..end` (end exclusive).
    pub cols: Range<usize>,
    pub path: PathBuf,
    pub is_dir: bool,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

struct RowMatch {
    cols: Range<usize>,
    found: PathMatch,
    /// `None` = not asked yet; `Some(None)` = does not exist.
    resolved: Option<Option<Resolved>>,
}

/// The text of a row and where each character sits.
struct RowText {
    text: String,
    /// `(byte offset, column)` of every character, in order.
    starts: Vec<(usize, usize)>,
    /// Column right after the last character.
    end_col: usize,
}

impl RowText {
    fn of(cells: &[CellView]) -> Self {
        let mut text = String::with_capacity(cells.len());
        let mut starts = Vec::with_capacity(cells.len());
        let mut end_col = 0;
        for (col, cell) in cells.iter().enumerate() {
            if cell.flags.wide_spacer {
                continue;
            }
            starts.push((text.len(), col));
            text.push(if cell.ch.is_control() { ' ' } else { cell.ch });
            end_col = col + if cell.flags.wide { 2 } else { 1 };
        }
        Self {
            text,
            starts,
            end_col,
        }
    }

    /// Column of the character that starts at (or first after) byte `b`.
    fn col_at(&self, b: usize) -> usize {
        let i = self.starts.partition_point(|(off, _)| *off < b);
        self.starts.get(i).map_or(self.end_col, |(_, c)| *c)
    }

    fn cols(&self, bytes: &Range<usize>) -> Range<usize> {
        self.col_at(bytes.start)..self.col_at(bytes.end)
    }
}

/// Per-row match cache of one terminal.
#[derive(Default)]
pub struct LinkCache {
    rows: HashMap<u64, Vec<RowMatch>>,
}

impl LinkCache {
    /// The existing path under column `col` of a row whose cells are `cells`.
    pub fn hit(
        &mut self,
        row_hash: u64,
        cells: &[CellView],
        col: usize,
        cwd: &Path,
    ) -> Option<LinkHit> {
        if self.rows.len() > CACHE_ROWS {
            self.rows.clear();
        }
        let key = row_hash ^ cwd_hash(cwd).rotate_left(17);
        let matches = self.rows.entry(key).or_insert_with(|| {
            let row = RowText::of(cells);
            viewer_svc::find_paths(&row.text, cwd)
                .into_iter()
                .map(|m| RowMatch {
                    cols: row.cols(&m.range),
                    found: m,
                    resolved: None,
                })
                .collect()
        });
        let m = matches.iter_mut().find(|m| m.cols.contains(&col))?;
        let resolved = m
            .resolved
            .get_or_insert_with(|| m.found.resolve())
            .clone()?;
        Some(LinkHit {
            cols: m.cols.clone(),
            path: resolved.path,
            is_dir: resolved.is_dir,
            line: m.found.line,
            col: m.found.col,
        })
    }
}

fn cwd_hash(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use term_core::{CellFlags, Rgb8};

    use super::*;

    fn cells(s: &str) -> Vec<CellView> {
        let mut out = Vec::new();
        for ch in s.chars() {
            let wide = ch as u32 > 0x2E80;
            out.push(CellView {
                ch,
                fg: Rgb8::default(),
                bg: Rgb8::default(),
                flags: CellFlags {
                    wide,
                    ..CellFlags::default()
                },
            });
            if wide {
                out.push(CellView {
                    ch: ' ',
                    fg: Rgb8::default(),
                    bg: Rgb8::default(),
                    flags: CellFlags {
                        wide_spacer: true,
                        ..CellFlags::default()
                    },
                });
            }
        }
        out
    }

    #[test]
    fn finds_an_existing_file_with_line_and_columns() {
        let dir = std::env::temp_dir().join(format!("selachi-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
        let line = "error: bad  main.rs:12:5  here";
        let row = cells(line);
        let mut cache = LinkCache::default();
        let start = line.find("main.rs").unwrap();
        let hit = cache.hit(1, &row, start + 2, &dir).expect("hit");
        assert_eq!((hit.line, hit.col, hit.is_dir), (Some(12), Some(5), false));
        assert_eq!(hit.cols, start..start + "main.rs:12:5".len());
        assert_eq!(hit.path, dir.join("main.rs"));
        // Outside the match, and a path that does not exist, give nothing.
        assert!(cache.hit(1, &row, 2, &dir).is_none());
        let missing = cells("see nope.rs:3");
        assert!(cache.hit(2, &missing, 5, &dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn columns_account_for_wide_characters() {
        let row = cells("日本 a.toml");
        let rt = RowText::of(&row);
        // "日本" takes 4 columns, then a space: `a.toml` starts at column 5.
        let b = rt.text.find("a.toml").unwrap();
        assert_eq!(rt.col_at(b), 5);
        assert_eq!(rt.end_col, 11);
    }

    #[test]
    fn a_directory_is_reported_as_such() {
        let dir = std::env::temp_dir();
        let row = cells(&format!("cd {}", dir.display()));
        let mut cache = LinkCache::default();
        if let Some(hit) = cache.hit(9, &row, 5, &dir) {
            assert!(hit.is_dir);
        }
    }
}
