//! Pure logic of the block UI (PLAN A2/A3): which viewport row belongs to which command block,
//! which absolute scrollback line a row shows, which rows a folded block hides, and the runs the
//! gutter marker is painted over. No gpui here, so it is unit tested.
//!
//! `term_core` reports blocks in absolute scrollback lines and answers "which block is at viewport
//! row N" ([`term_core::Session::block_at_line`]) but does not expose the absolute line of the
//! viewport top. [`viewport_abs`] recovers it from the block boundaries: a run of rows of one block
//! that does not start at the top row starts at the block's prompt line; one that does not reach
//! the last row and belongs to a finished block ends right before the block's `output_end`.

use std::collections::HashSet;

use term_core::{AbsLine, Block, BlockId, BlockState};

/// What one painted row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisRow {
    /// The terminal viewport row painted here (`None` = blank filler or a fold summary).
    pub term_row: Option<usize>,
    /// The block this row belongs to (a fold summary belongs to the folded block).
    pub block: Option<BlockId>,
    /// Absolute scrollback line of `term_row`, when it could be recovered.
    pub abs: Option<AbsLine>,
    /// `Some(n)`: this row is the summary of a folded block that hides `n` lines.
    pub summary: Option<u64>,
}

impl VisRow {
    const fn blank() -> Self {
        Self { term_row: None, block: None, abs: None, summary: None }
    }
}

/// The block of every viewport row, from `block_at_line` answers.
pub fn find_block(blocks: &[Block], id: BlockId) -> Option<&Block> {
    blocks.iter().find(|b| b.id == id)
}

/// Absolute scrollback line shown at each viewport row, `None` where it cannot be known
/// (rows outside any block, or a run that fills the screen without a visible boundary).
pub fn viewport_abs(row_block: &[Option<BlockId>], blocks: &[Block]) -> Vec<Option<AbsLine>> {
    let n = row_block.len();
    let mut out = vec![None; n];
    let mut r = 0;
    while r < n {
        let Some(id) = row_block[r] else {
            r += 1;
            continue;
        };
        let mut e = r;
        while e + 1 < n && row_block[e + 1] == Some(id) {
            e += 1;
        }
        if let Some(b) = find_block(blocks, id) {
            let base = if r > 0 {
                // The row above is another block (or none): this run starts at the prompt line.
                Some(b.prompt_line)
            } else if e + 1 < n {
                // The run ends inside the screen: its last row is the block's last line.
                b.output_end.and_then(|end| end.checked_sub(1 + (e - r) as u64))
            } else {
                None
            };
            if let Some(base) = base {
                for (k, slot) in out.iter_mut().enumerate().take(e + 1).skip(r) {
                    *slot = Some(base + (k - r) as u64);
                }
            }
        }
        r = e + 1;
    }
    out
}

/// Can this block be folded? Only finished blocks with output.
pub fn foldable(b: &Block) -> bool {
    b.state != BlockState::Running && b.output_lines() > 0
}

/// Is `abs` one of the output lines of `b`?
fn in_output(b: &Block, abs: AbsLine) -> bool {
    b.output_end.is_some_and(|end| abs >= b.output_start && abs < end)
}

/// Is `abs` the prompt or command line of `b` (the rows the action bar hangs from)?
pub fn in_header(b: &Block, abs: AbsLine) -> bool {
    abs >= b.prompt_line && abs < b.output_start.max(b.prompt_line + 1)
}

/// The rows to paint: folded blocks lose their output rows (one summary row stays), and the freed
/// space is left blank at the top when the view is at the live bottom (the prompt stays on the
/// last row) or at the bottom when scrolled into history.
pub fn layout_rows(
    row_block: &[Option<BlockId>],
    abs: &[Option<AbsLine>],
    blocks: &[Block],
    folded: &HashSet<BlockId>,
    at_bottom: bool,
) -> Vec<VisRow> {
    let n = row_block.len();
    let mut rows: Vec<VisRow> = Vec::with_capacity(n);
    let mut summarized: Option<BlockId> = None;
    for (r, &block) in row_block.iter().enumerate() {
        let line = abs.get(r).copied().flatten();
        let hide = match (block, line) {
            (Some(id), Some(a)) if folded.contains(&id) => find_block(blocks, id).filter(|b| in_output(b, a)),
            _ => None,
        };
        match hide {
            Some(b) => {
                if summarized != Some(b.id) {
                    summarized = Some(b.id);
                    rows.push(VisRow { term_row: None, block, abs: None, summary: Some(b.output_lines()) });
                }
            },
            None => {
                summarized = None;
                rows.push(VisRow { term_row: Some(r), block, abs: line, summary: None });
            },
        }
    }
    let missing = n.saturating_sub(rows.len());
    if missing > 0 {
        let pad = std::iter::repeat_n(VisRow::blank(), missing);
        if at_bottom {
            rows.splice(0..0, pad);
        } else {
            rows.extend(pad);
        }
    }
    rows
}

/// Contiguous runs `(first vis row, last vis row, block)` of painted rows that belong to the same
/// block: what the gutter marker is drawn over.
pub fn gutter_runs(rows: &[VisRow]) -> Vec<(usize, usize, BlockId)> {
    let mut out: Vec<(usize, usize, BlockId)> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let Some(id) = r.block else { continue };
        match out.last_mut() {
            Some((_, end, last)) if *last == id && *end + 1 == i => *end = i,
            _ => out.push((i, i, id)),
        }
    }
    out
}

/// The block the action bar shows for, given the hovered painted row and whether the pointer is in
/// the gutter: the block of a prompt/command row, or any row of the block when in the gutter.
pub fn hover_target(rows: &[VisRow], blocks: &[Block], vis: usize, in_gutter: bool) -> Option<BlockId> {
    let row = rows.get(vis)?;
    let id = row.block?;
    if in_gutter || row.summary.is_some() {
        return Some(id);
    }
    let b = find_block(blocks, id)?;
    row.abs.filter(|a| in_header(b, *a)).map(|_| id)
}

/// First painted row of block `id` (where the bar is anchored): its prompt row when visible.
pub fn anchor_row(rows: &[VisRow], blocks: &[Block], id: BlockId) -> Option<usize> {
    let b = find_block(blocks, id)?;
    rows.iter()
        .position(|r| r.block == Some(id) && r.abs.is_some_and(|a| in_header(b, a)))
        .or_else(|| rows.iter().position(|r| r.block == Some(id)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Instant, SystemTime};

    use super::*;

    fn block(id: u64, prompt: u64, out_start: u64, out_end: Option<u64>, state: BlockState) -> Block {
        Block {
            id: BlockId(id),
            command: Some(format!("cmd{id}")),
            cwd: Some(PathBuf::from("C:\\p")),
            started_at: Instant::now(),
            started_wall: SystemTime::now(),
            finished_at: None,
            finished_wall: None,
            duration: None,
            exit_code: None,
            state,
            prompt_line: prompt,
            command_line: prompt,
            output_start: out_start,
            output_end: out_end,
        }
    }

    fn ids(v: &[Option<u64>]) -> Vec<Option<BlockId>> {
        v.iter().map(|x| x.map(BlockId)).collect()
    }

    #[test]
    fn abs_from_a_run_that_starts_inside_the_screen() {
        // rows: [none, B1 x4, B2 x3]
        let blocks = [block(1, 10, 11, Some(14), BlockState::Succeeded), block(2, 14, 15, Some(17), BlockState::Failed)];
        let rows = ids(&[None, Some(1), Some(1), Some(1), Some(1), Some(2), Some(2), Some(2)]);
        // B1 has 4 lines (10..14) and B2 starts right after at 14.
        let abs = viewport_abs(&rows, &blocks);
        assert_eq!(abs[0], None);
        assert_eq!(abs[1], Some(10));
        assert_eq!(abs[4], Some(13));
        assert_eq!(abs[5], Some(14));
        assert_eq!(abs[7], Some(16));
    }

    #[test]
    fn abs_from_the_end_when_the_run_starts_above_the_screen() {
        let blocks = [block(1, 0, 1, Some(100), BlockState::Succeeded), block(2, 100, 101, None, BlockState::Running)];
        // Viewport shows the last 3 lines of B1, then B2.
        let rows = ids(&[Some(1), Some(1), Some(1), Some(2), Some(2)]);
        let abs = viewport_abs(&rows, &blocks);
        assert_eq!(&abs[..3], &[Some(97), Some(98), Some(99)]);
        assert_eq!(abs[3], Some(100));
        assert_eq!(abs[4], Some(101));
    }

    #[test]
    fn unknown_when_a_block_fills_the_screen() {
        let blocks = [block(1, 0, 1, Some(100), BlockState::Succeeded)];
        let abs = viewport_abs(&ids(&[Some(1), Some(1), Some(1)]), &blocks);
        assert_eq!(abs, vec![None, None, None]);
    }

    #[test]
    fn folding_keeps_one_summary_row_and_pads_the_bottom_in_history() {
        let blocks = [block(1, 10, 11, Some(15), BlockState::Succeeded), block(2, 15, 16, None, BlockState::Running)];
        let rb = ids(&[Some(1), Some(1), Some(1), Some(1), Some(1), Some(2)]);
        let abs = viewport_abs(&rb, &blocks);
        // Row 0 is B1's prompt, rows 1..=4 its output (11..15), row 5 is B2's prompt at 15.
        assert_eq!(abs[0], Some(10));
        let folded: HashSet<_> = [BlockId(1)].into();
        let rows = layout_rows(&rb, &abs, &blocks, &folded, false);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].term_row, Some(0));
        assert_eq!(rows[1].summary, Some(4));
        assert_eq!(rows[1].block, Some(BlockId(1)));
        assert_eq!(rows[2].term_row, Some(5));
        assert!(rows[3..].iter().all(|r| r.term_row.is_none() && r.summary.is_none()));
    }

    #[test]
    fn folding_at_the_live_bottom_keeps_the_prompt_on_the_last_row() {
        let blocks = [block(1, 10, 11, Some(15), BlockState::Succeeded), block(2, 15, 16, None, BlockState::Running)];
        let rb = ids(&[Some(1), Some(1), Some(1), Some(1), Some(1), Some(2)]);
        let abs = viewport_abs(&rb, &blocks);
        let folded: HashSet<_> = [BlockId(1)].into();
        let rows = layout_rows(&rb, &abs, &blocks, &folded, true);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[5].term_row, Some(5));
        assert!(rows[0].term_row.is_none() && rows[0].summary.is_none());
    }

    #[test]
    fn nothing_folds_when_the_rows_are_unknown_or_the_block_runs() {
        let blocks = [block(1, 0, 1, Some(100), BlockState::Succeeded)];
        let rb = ids(&[Some(1), Some(1)]);
        let abs = viewport_abs(&rb, &blocks);
        let folded: HashSet<_> = [BlockId(1)].into();
        let rows = layout_rows(&rb, &abs, &blocks, &folded, true);
        assert_eq!(rows.iter().filter_map(|r| r.term_row).collect::<Vec<_>>(), vec![0, 1]);
        assert!(foldable(&blocks[0]));
        assert!(!foldable(&block(2, 5, 6, None, BlockState::Running)));
        assert!(!foldable(&block(3, 5, 6, Some(6), BlockState::Succeeded)));
    }

    #[test]
    fn gutter_runs_split_by_block_and_gaps() {
        let r = |b: Option<u64>| VisRow { term_row: Some(0), block: b.map(BlockId), abs: None, summary: None };
        let rows = [r(Some(1)), r(Some(1)), r(None), r(Some(1)), r(Some(2))];
        assert_eq!(
            gutter_runs(&rows),
            vec![(0, 1, BlockId(1)), (3, 3, BlockId(1)), (4, 4, BlockId(2))]
        );
    }

    #[test]
    fn the_bar_hangs_from_the_prompt_row_or_the_gutter() {
        let blocks = [block(1, 10, 12, Some(15), BlockState::Failed)];
        // A leading blank row makes the run start inside the screen, so its rows are exact.
        let rb = ids(&[None, Some(1), Some(1), Some(1), Some(1), Some(1)]);
        let abs = viewport_abs(&rb, &blocks);
        let rows = layout_rows(&rb, &abs, &blocks, &HashSet::new(), true);
        // Rows 1 and 2 are the prompt + command (abs 10, 11), 3.. the output.
        assert_eq!(hover_target(&rows, &blocks, 1, false), Some(BlockId(1)));
        assert_eq!(hover_target(&rows, &blocks, 2, false), Some(BlockId(1)));
        assert_eq!(hover_target(&rows, &blocks, 4, false), None);
        assert_eq!(hover_target(&rows, &blocks, 4, true), Some(BlockId(1)));
        assert_eq!(anchor_row(&rows, &blocks, BlockId(1)), Some(1));
    }
}
