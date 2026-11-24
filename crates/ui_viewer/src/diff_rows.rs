//! Flattens `viewer_svc` hunks into the rows the diff views paint (unified and side by side) and
//! summarises them for the minimap and the header.

use viewer_svc::{DiffLine, Hunk, LineKind};

/// A row of the unified diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnifiedRow {
    /// `@@ -1,3 +1,4 @@`
    Hunk(String),
    Line(DiffLine),
}

/// A row of the side-by-side diff: old on the left, new on the right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitRow {
    Hunk(String),
    Pair {
        left: Option<DiffLine>,
        right: Option<DiffLine>,
    },
}

pub fn unified_rows(hunks: &[Hunk]) -> Vec<UnifiedRow> {
    let mut rows = Vec::with_capacity(hunks.iter().map(|h| h.lines.len() + 1).sum());
    for h in hunks {
        rows.push(UnifiedRow::Hunk(h.header()));
        rows.extend(h.lines.iter().cloned().map(UnifiedRow::Line));
    }
    rows
}

/// Pairs removed and added lines of the same change block one to one (the leftovers sit opposite an
/// empty cell); context lines appear on both sides.
pub fn split_rows(hunks: &[Hunk]) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    for h in hunks {
        rows.push(SplitRow::Hunk(h.header()));
        let mut removed: Vec<&DiffLine> = Vec::new();
        let mut added: Vec<&DiffLine> = Vec::new();
        let flush =
            |rows: &mut Vec<SplitRow>, removed: &mut Vec<&DiffLine>, added: &mut Vec<&DiffLine>| {
                let n = removed.len().max(added.len());
                for i in 0..n {
                    rows.push(SplitRow::Pair {
                        left: removed.get(i).map(|l| (*l).clone()),
                        right: added.get(i).map(|l| (*l).clone()),
                    });
                }
                removed.clear();
                added.clear();
            };
        for line in &h.lines {
            match line.kind {
                LineKind::Context => {
                    flush(&mut rows, &mut removed, &mut added);
                    rows.push(SplitRow::Pair {
                        left: Some(line.clone()),
                        right: Some(line.clone()),
                    });
                }
                LineKind::Removed => removed.push(line),
                LineKind::Added => added.push(line),
            }
        }
        flush(&mut rows, &mut removed, &mut added);
    }
    rows
}

/// Length in bytes of each unified row (hunk headers count as short) and whether it is a change.
/// This is what the minimap draws in diff mode: changed rows in the accent.
pub fn unified_minimap_source(rows: &[UnifiedRow]) -> (Vec<u16>, Vec<usize>) {
    let mut lens = Vec::with_capacity(rows.len());
    let mut changed = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        match r {
            UnifiedRow::Hunk(_) => lens.push(10),
            UnifiedRow::Line(l) => {
                lens.push(l.text.len().min(usize::from(u16::MAX)) as u16);
                if l.kind != LineKind::Context {
                    changed.push(i);
                }
            }
        }
    }
    (lens, changed)
}

/// Same for the split layout: a pair's length is the longer side.
pub fn split_minimap_source(rows: &[SplitRow]) -> (Vec<u16>, Vec<usize>) {
    let mut lens = Vec::with_capacity(rows.len());
    let mut changed = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        match r {
            SplitRow::Hunk(_) => lens.push(10),
            SplitRow::Pair { left, right } => {
                let len = left
                    .iter()
                    .chain(right.iter())
                    .map(|l| l.text.len())
                    .max()
                    .unwrap_or(0);
                lens.push(len.min(usize::from(u16::MAX)) as u16);
                let is_change = left.as_ref().is_some_and(|l| l.kind != LineKind::Context)
                    || right.as_ref().is_some_and(|l| l.kind != LineKind::Context);
                if is_change {
                    changed.push(i);
                }
            }
        }
    }
    (lens, changed)
}

/// Totals for the header chips: `(added, removed)` lines.
pub fn totals(hunks: &[Hunk]) -> (usize, usize) {
    hunks
        .iter()
        .fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()))
}

/// Digits needed for the widest line number of the diff (gutter width).
pub fn gutter_digits(hunks: &[Hunk]) -> usize {
    let max = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .flat_map(|l| [l.old_line, l.new_line])
        .flatten()
        .max()
        .unwrap_or(1);
    max.to_string().len().max(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewer_svc::diff_text;

    const OLD: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";
    const NEW: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\n";

    #[test]
    fn unified_has_a_header_per_hunk() {
        let hunks = diff_text(OLD, NEW);
        let rows = unified_rows(&hunks);
        let headers = rows
            .iter()
            .filter(|r| matches!(r, UnifiedRow::Hunk(_)))
            .count();
        assert_eq!(headers, hunks.len());
        assert_eq!(
            rows.len(),
            hunks.iter().map(|h| h.lines.len()).sum::<usize>() + hunks.len()
        );
    }

    #[test]
    fn split_pairs_replaced_lines() {
        let hunks = diff_text("one\ntwo\nthree\n", "one\nTWO\nthree\nfour\n");
        let rows = split_rows(&hunks);
        // header, context(one), pair(two|TWO), context(three), pair(-|four)
        let pairs: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                SplitRow::Pair { left, right } => Some((
                    left.as_ref().map(|l| l.text.as_str()),
                    right.as_ref().map(|l| l.text.as_str()),
                )),
                SplitRow::Hunk(_) => None,
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Some("one"), Some("one")),
                (Some("two"), Some("TWO")),
                (Some("three"), Some("three")),
                (None, Some("four")),
            ]
        );
    }

    #[test]
    fn split_leaves_the_short_side_empty() {
        let hunks = diff_text("a\nb\nc\n", "a\nc\n");
        let rows = split_rows(&hunks);
        let removed_only = rows
            .iter()
            .any(|r| matches!(r, SplitRow::Pair { left: Some(l), right: None } if l.text == "b"));
        assert!(removed_only, "{rows:?}");
    }

    #[test]
    fn minimap_source_marks_only_changes() {
        let hunks = diff_text(OLD, NEW);
        let rows = unified_rows(&hunks);
        let (lens, changed) = unified_minimap_source(&rows);
        assert_eq!(lens.len(), rows.len());
        assert!(!changed.is_empty());
        for i in &changed {
            assert!(matches!(&rows[*i], UnifiedRow::Line(l) if l.kind != LineKind::Context));
        }
        let split = split_rows(&hunks);
        let (slens, schanged) = split_minimap_source(&split);
        assert_eq!(slens.len(), split.len());
        assert!(!schanged.is_empty());
    }

    #[test]
    fn totals_and_gutter() {
        let hunks = diff_text(OLD, NEW);
        let (a, r) = totals(&hunks);
        assert_eq!((a, r), (3, 1));
        assert_eq!(gutter_digits(&hunks), 2);
        assert_eq!(gutter_digits(&[]), 2);
        assert!(totals(&[]) == (0, 0));
    }
}
