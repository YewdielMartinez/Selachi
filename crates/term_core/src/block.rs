//! Command blocks: one [`Block`] per command, built from the shell's OSC 133 marks.
//!
//! # Coordinates
//!
//! Positions are [`AbsLine`]s: absolute scrollback lines that do not move when the terminal
//! scrolls. Line `0` is the first line the session ever printed; every line that scrolls into
//! history keeps its number. When history is trimmed (or cleared) the oldest numbers simply stop
//! existing; blocks that lost all their lines are dropped and partially trimmed ones are clamped.
//!
//! This is exact because the terminal's own history cap is set higher than the user's scrollback
//! and the session trims it itself, counting every dropped line (see [`BlockTracker::observe`]).
//!
//! # State machine
//!
//! `A` prompt start (pending prompt) -> `B` input starts -> `C` command executed (the block is
//! created, [`BlockState::Running`]) -> `D;code` finished. A new `A` or `C` while a block still
//! runs closes it as [`BlockState::Unknown`] (nested shells, interrupted prompts). A `D` with no
//! running block is ignored. `OSC 633;E` carries the exact command line and wins over scraping
//! the grid between `B` and `C`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Term, TermMode};

use crate::osc::{Mark, OscEvent};

/// An absolute scrollback line (see the module docs).
pub type AbsLine = u64;

/// Most blocks kept per session; older ones are forgotten.
pub const MAX_BLOCKS: usize = 500;
/// Longest stored command line, in bytes.
const MAX_COMMAND_BYTES: usize = 8 * 1024;
/// Most lines scraped from the grid for a command line.
const MAX_COMMAND_LINES: i64 = 200;
/// Extra history the terminal may hold above the user's scrollback before the session trims it.
pub(crate) const TRIM_SLACK: usize = 1 << 17;

/// Stable identifier of a block within one session (never reused).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u64);

impl std::fmt::Display for BlockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// How a block ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockState {
    /// The command is still running.
    Running,
    /// Finished with exit code 0.
    Succeeded,
    /// Finished with a non-zero exit code.
    Failed,
    /// Finished without an exit code (interrupted, shell exited, closed by a new prompt).
    Unknown,
}

/// One command: where it lives in the scrollback, what ran, and how it ended.
#[derive(Clone, Debug)]
pub struct Block {
    /// Identifier within the session.
    pub id: BlockId,
    /// The command line, if known (exact via OSC 633;E, otherwise scraped; `None` if empty).
    pub command: Option<String>,
    /// Working directory when the command started (latest OSC 7).
    pub cwd: Option<PathBuf>,
    /// Monotonic start time (the moment the command was executed).
    pub started_at: Instant,
    /// Wall-clock start time.
    pub started_wall: SystemTime,
    /// Monotonic finish time.
    pub finished_at: Option<Instant>,
    /// Wall-clock finish time.
    pub finished_wall: Option<SystemTime>,
    /// `finished_at - started_at`.
    pub duration: Option<Duration>,
    /// Exit code reported by the shell.
    pub exit_code: Option<i32>,
    /// Lifecycle state.
    pub state: BlockState,
    /// First line of the prompt (OSC 133 `A`).
    pub prompt_line: AbsLine,
    /// Line where the typed command starts (OSC 133 `B`).
    pub command_line: AbsLine,
    /// First line of the output (first full line after the command was submitted).
    pub output_start: AbsLine,
    /// End of the output, exclusive; `None` while the command runs.
    pub output_end: Option<AbsLine>,
}

impl Block {
    /// `true` once the command is no longer running.
    pub fn is_finished(&self) -> bool {
        self.state != BlockState::Running
    }

    /// Number of output lines (for a running block: up to its last known line).
    pub fn output_lines(&self) -> u64 {
        self.output_end
            .map_or(0, |e| e.saturating_sub(self.output_start))
    }
}

/// A transition the tracker wants announced.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BlockEvent {
    Started(BlockId),
    Finished {
        id: BlockId,
        exit_code: Option<i32>,
        duration: Duration,
    },
}

/// "Now" in both clocks, injectable for tests.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    pub instant: Instant,
    pub wall: SystemTime,
}

impl Clock {
    pub(crate) fn now() -> Self {
        Self {
            instant: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

#[derive(Debug)]
struct Pending {
    prompt_line: AbsLine,
    /// Where `B` put the cursor: `(line, column)`.
    cmd_start: Option<(AbsLine, usize)>,
    /// Exact command line from OSC 633;E.
    exact: Option<String>,
}

/// Builds and queries the block list. Lives behind a mutex in the session; mutated only while the
/// terminal lock is held so grid and tracker never disagree.
#[derive(Debug)]
pub(crate) struct BlockTracker {
    scrollback: usize,
    /// Lines that no longer exist (trimmed or cleared from the top of history).
    dropped: u64,
    /// History length at the last observation.
    last_history: usize,
    blocks: VecDeque<Block>,
    pending: Option<Pending>,
    cwd: Option<PathBuf>,
    next_id: u64,
    /// `true` right after `C`: an `E` arriving now belongs to the running block.
    just_started: bool,
    /// The running block's command came from OSC 633;E.
    running_exact: bool,
}

/// History length the terminal should be created with for a user scrollback of `scrollback`.
pub(crate) fn term_history_cap(scrollback: usize) -> usize {
    scrollback.saturating_add(TRIM_SLACK)
}

impl BlockTracker {
    pub(crate) fn new(scrollback: usize) -> Self {
        Self {
            scrollback,
            dropped: 0,
            last_history: 0,
            blocks: VecDeque::new(),
            pending: None,
            cwd: None,
            next_id: 1,
            just_started: false,
            running_exact: false,
        }
    }

    // ---- coordinates -------------------------------------------------------------------

    /// Absolute number of a grid line (`line` is relative to the viewport top; negative = history).
    fn abs(&self, hist: usize, line: i64) -> AbsLine {
        u64::try_from(self.dropped as i64 + hist as i64 + line).unwrap_or(0)
    }

    /// Grid line of an absolute line, for the current history length.
    fn grid_line(&self, hist: usize, abs: AbsLine) -> i64 {
        abs as i64 - self.dropped as i64 - hist as i64
    }

    /// Absolute line shown at viewport row `row` given the scroll offset.
    pub(crate) fn abs_of_viewport_row(
        &self,
        hist: usize,
        display_offset: usize,
        row: usize,
    ) -> AbsLine {
        self.abs(hist, row as i64 - display_offset as i64)
    }

    /// Display offset that puts `abs` on the first viewport row (clamped by the terminal).
    pub(crate) fn offset_for_top(&self, hist: usize, abs: AbsLine) -> i64 {
        -self.grid_line(hist, abs)
    }

    /// Records the current history length; every decrease means lines left the top of the
    /// scrollback (trimmed by the session, cleared by `CSI 3 J`, ...), so the numbering shifts.
    pub(crate) fn observe(&mut self, hist: usize) {
        if hist < self.last_history {
            self.dropped += (self.last_history - hist) as u64;
            self.expire();
        }
        self.last_history = hist;
    }

    /// Drops blocks whose lines are all gone and clamps the rest.
    fn expire(&mut self) {
        let dropped = self.dropped;
        self.blocks
            .retain(|b| b.output_end.is_none_or(|end| end > dropped));
        for b in &mut self.blocks {
            b.prompt_line = b.prompt_line.max(dropped);
            b.command_line = b.command_line.max(dropped);
            b.output_start = b.output_start.max(dropped);
        }
        if let Some(p) = &mut self.pending {
            p.prompt_line = p.prompt_line.max(dropped);
            if p.cmd_start.is_some_and(|(l, _)| l < dropped) {
                p.cmd_start = None;
            }
        }
    }

    /// Called by the session after it resized the terminal. Height changes preserve absolute
    /// numbers; a column change reflows the text, which invalidates every stored line.
    pub(crate) fn after_resize(&mut self, hist: usize, cols_changed: bool) {
        self.last_history = hist;
        if cols_changed {
            self.blocks.clear();
            self.pending = None;
            self.just_started = false;
            self.running_exact = false;
        }
    }

    // ---- queries -----------------------------------------------------------------------

    pub(crate) fn blocks(&self) -> Vec<Block> {
        self.blocks.iter().cloned().collect()
    }

    pub(crate) fn get(&self, id: BlockId) -> Option<&Block> {
        // Ids grow monotonically, so a binary search works.
        let i = self.blocks.binary_search_by_key(&id.0, |b| b.id.0).ok()?;
        self.blocks.get(i)
    }

    /// The block whose lines (prompt, command and output) contain `abs`.
    pub(crate) fn block_at_abs(&self, abs: AbsLine) -> Option<BlockId> {
        self.blocks
            .iter()
            .rev()
            .find(|b| b.prompt_line <= abs && abs < b.output_end.unwrap_or(u64::MAX))
            .map(|b| b.id)
    }

    /// Previous block: before `from`, or (with `None`) the last one starting above `top`.
    pub(crate) fn prev(&self, from: Option<BlockId>, top: AbsLine) -> Option<BlockId> {
        match from {
            Some(id) => {
                let i = self
                    .blocks
                    .binary_search_by_key(&id.0, |b| b.id.0)
                    .unwrap_or_else(|i| i);
                i.checked_sub(1)
                    .and_then(|i| self.blocks.get(i))
                    .map(|b| b.id)
            }
            None => self
                .blocks
                .iter()
                .rev()
                .find(|b| b.prompt_line < top)
                .map(|b| b.id),
        }
    }

    /// Next block: after `from`, or (with `None`) the first one starting below `top`.
    pub(crate) fn next(&self, from: Option<BlockId>, top: AbsLine) -> Option<BlockId> {
        match from {
            Some(id) => match self.blocks.binary_search_by_key(&id.0, |b| b.id.0) {
                Ok(i) => self.blocks.get(i + 1).map(|b| b.id),
                Err(i) => self.blocks.get(i).map(|b| b.id),
            },
            None => self
                .blocks
                .iter()
                .find(|b| b.prompt_line > top)
                .map(|b| b.id),
        }
    }

    /// Plain text of the grid lines `[from, to)` (absolute), trailing whitespace trimmed.
    pub(crate) fn text_between<L: EventListener>(
        &self,
        term: &Term<L>,
        from: AbsLine,
        to: AbsLine,
    ) -> String {
        let hist = term.grid().history_size();
        let first = -(hist as i64);
        let last = term.screen_lines() as i64 - 1;
        let start = self.grid_line(hist, from).max(first);
        let end = (self.grid_line(hist, to) - 1).min(last);
        if start > end {
            return String::new();
        }
        let raw = term.bounds_to_string(
            Point::new(Line(start as i32), Column(0)),
            Point::new(Line(end as i32), Column(term.columns() - 1)),
        );
        clean_lines(&raw)
    }

    /// Last line (exclusive) a block currently covers: its end, or the cursor line while running.
    pub(crate) fn output_limit<L: EventListener>(&self, term: &Term<L>, b: &Block) -> AbsLine {
        b.output_end.unwrap_or_else(|| {
            let cursor = term.grid().cursor.point.line.0 as i64;
            self.abs(term.grid().history_size(), cursor) + 1
        })
    }

    // ---- feeding -----------------------------------------------------------------------

    fn cursor<L: EventListener>(&mut self, term: &Term<L>) -> (AbsLine, usize) {
        let hist = term.grid().history_size();
        self.observe(hist);
        let p = term.grid().cursor.point;
        (self.abs(hist, i64::from(p.line.0)), p.column.0)
    }

    /// Applies one OSC event to the model; `term` has parsed everything up to that sequence.
    pub(crate) fn on_osc<L: EventListener>(
        &mut self,
        term: &Term<L>,
        event: OscEvent,
        clock: Clock,
    ) -> Vec<BlockEvent> {
        let mut out = Vec::new();
        if let OscEvent::Cwd(path) = event {
            self.cwd = Some(path);
            return out;
        }
        // Full-screen apps (alternate screen) have no scrollback; their marks mean nothing here.
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return out;
        }
        let (line, col) = self.cursor(term);
        // First line after the cursor's content: the cursor line itself if it is still empty.
        let excl = if col == 0 { line } else { line + 1 };
        match event {
            OscEvent::Cwd(_) => {}
            OscEvent::Mark(Mark::PromptStart) => {
                self.just_started = false;
                self.finish_running(None, excl, clock, &mut out);
                // A prompt at or above an old block means the screen was cleared over it.
                self.blocks.retain(|b| b.prompt_line < line);
                self.pending = Some(Pending {
                    prompt_line: line,
                    cmd_start: None,
                    exact: None,
                });
            }
            OscEvent::Mark(Mark::CommandStart) => {
                self.just_started = false;
                let p = self.pending.get_or_insert(Pending {
                    prompt_line: line,
                    cmd_start: None,
                    exact: None,
                });
                p.cmd_start = Some((line, col));
            }
            OscEvent::Command(text) => {
                let text = cap_command(text.trim());
                if self.just_started
                    && self
                        .blocks
                        .back()
                        .is_some_and(|b| b.state == BlockState::Running)
                {
                    if let Some(b) = self.blocks.back_mut() {
                        b.command = non_empty(text);
                    }
                    self.running_exact = true;
                } else {
                    let p = self.pending.get_or_insert(Pending {
                        prompt_line: line,
                        cmd_start: None,
                        exact: None,
                    });
                    p.exact = Some(text);
                }
                self.just_started = false;
            }
            OscEvent::Mark(Mark::Executed) => self.start_block(term, line, col, clock, &mut out),
            OscEvent::Mark(Mark::Finished(code)) => {
                self.just_started = false;
                self.finish_running(code, excl, clock, &mut out);
            }
        }
        out
    }

    fn start_block<L: EventListener>(
        &mut self,
        term: &Term<L>,
        line: AbsLine,
        col: usize,
        clock: Clock,
        out: &mut Vec<BlockEvent>,
    ) {
        let pending = self.pending.take().unwrap_or(Pending {
            prompt_line: line,
            cmd_start: None,
            exact: None,
        });
        self.finish_running(None, if col == 0 { line } else { line + 1 }, clock, out);

        let exact = pending.exact.is_some();
        let command = match pending.exact {
            Some(text) => non_empty(text),
            None => pending
                .cmd_start
                .and_then(|start| self.scrape_command(term, start, (line, col)))
                .and_then(non_empty),
        };
        let id = BlockId(self.next_id);
        self.next_id += 1;
        let command_line = pending.cmd_start.map_or(pending.prompt_line, |(l, _)| l);
        self.blocks.push_back(Block {
            id,
            command,
            cwd: self.cwd.clone(),
            started_at: clock.instant,
            started_wall: clock.wall,
            finished_at: None,
            finished_wall: None,
            duration: None,
            exit_code: None,
            state: BlockState::Running,
            prompt_line: pending.prompt_line,
            command_line,
            output_start: if col == 0 { line } else { line + 1 },
            output_end: None,
        });
        while self.blocks.len() > MAX_BLOCKS {
            self.blocks.pop_front();
        }
        self.just_started = true;
        self.running_exact = exact;
        out.push(BlockEvent::Started(id));
    }

    /// Text typed between `B` and `C`, joined across wrapped lines.
    fn scrape_command<L: EventListener>(
        &self,
        term: &Term<L>,
        start: (AbsLine, usize),
        cursor: (AbsLine, usize),
    ) -> Option<String> {
        let hist = term.grid().history_size();
        let cols = term.columns();
        let first = -(hist as i64);
        let (sl, sc) = (self.grid_line(hist, start.0), start.1.min(cols - 1));
        // The last typed cell is the one before the cursor (or the end of the previous line).
        let (el, ec) = if cursor.1 > 0 {
            (self.grid_line(hist, cursor.0), cursor.1 - 1)
        } else {
            (self.grid_line(hist, cursor.0) - 1, cols - 1)
        };
        let (sl, sc) = if sl < first { (first, 0) } else { (sl, sc) };
        if sl > el || el - sl > MAX_COMMAND_LINES || el >= term.screen_lines() as i64 {
            return None;
        }
        let raw = term.bounds_to_string(
            Point::new(Line(sl as i32), Column(sc)),
            Point::new(Line(el as i32), Column(ec.min(cols - 1))),
        );
        Some(cap_command(raw.trim()))
    }

    fn finish_running(
        &mut self,
        exit: Option<i32>,
        end: AbsLine,
        clock: Clock,
        out: &mut Vec<BlockEvent>,
    ) {
        let Some(b) = self
            .blocks
            .back_mut()
            .filter(|b| b.state == BlockState::Running)
        else {
            return;
        };
        let duration = clock.instant.saturating_duration_since(b.started_at);
        b.finished_at = Some(clock.instant);
        b.finished_wall = Some(clock.wall);
        b.duration = Some(duration);
        b.exit_code = exit;
        b.state = match exit {
            Some(0) => BlockState::Succeeded,
            Some(_) => BlockState::Failed,
            None => BlockState::Unknown,
        };
        b.output_end = Some(end.max(b.output_start));
        self.running_exact = false;
        out.push(BlockEvent::Finished {
            id: b.id,
            exit_code: exit,
            duration,
        });
    }

    /// Called after every parsed slice, with the terminal locked: keeps the grid's history at
    /// the user's scrollback (the terminal itself is allowed [`TRIM_SLACK`] more) and tracks
    /// what was dropped.
    pub(crate) fn after_slice<L: EventListener>(&mut self, term: &mut Term<L>) {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let hist = term.grid().history_size();
        if hist > self.scrollback {
            let grid = term.grid_mut();
            grid.update_history(self.scrollback);
            // `update_history` also resets the cap; restore the roomy one.
            grid.update_history(term_history_cap(self.scrollback));
        }
        self.observe(term.grid().history_size());
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.trim().is_empty()).then_some(s)
}

fn cap_command(s: &str) -> String {
    if s.len() <= MAX_COMMAND_BYTES {
        return s.to_string();
    }
    let mut end = MAX_COMMAND_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Trims trailing whitespace of every line and trailing empty lines.
fn clean_lines(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (i, line) in raw.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    out.truncate(out.trim_end().len());
    out
}

/// Shortens `text` to about `max_bytes`, keeping the head and the tail and replacing the middle
/// with a `… N líneas omitidas …` marker.
pub(crate) fn truncate_middle(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let marker_for = |n: usize| format!("… {n} líneas omitidas …");
    // Reserve room for the marker; the result may exceed `max_bytes` only when `max_bytes` is
    // smaller than the marker itself.
    let budget = max_bytes.saturating_sub(marker_for(lines.len()).len() + 2);
    let (head_budget, tail_budget) = (budget / 2, budget - budget / 2);

    let mut head: Vec<&str> = Vec::new();
    let mut used = 0;
    for l in &lines {
        let cost = l.len() + usize::from(!head.is_empty());
        if used + cost > head_budget {
            break;
        }
        used += cost;
        head.push(l);
    }
    let mut tail: Vec<&str> = Vec::new();
    used = 0;
    for l in lines[head.len()..].iter().rev() {
        let cost = l.len() + usize::from(!tail.is_empty());
        if used + cost > tail_budget {
            break;
        }
        used += cost;
        tail.push(l);
    }
    tail.reverse();

    let omitted = lines.len() - head.len() - tail.len();
    // A single giant line fits neither side: cut it on char boundaries instead.
    if head.is_empty() && tail.is_empty() {
        let first = lines[0];
        let mut end = head_budget.min(first.len());
        while !first.is_char_boundary(end) {
            end -= 1;
        }
        let last = lines[lines.len() - 1];
        let mut start = last.len().saturating_sub(tail_budget);
        while !last.is_char_boundary(start) {
            start += 1;
        }
        let omitted = lines.len().saturating_sub(2);
        return format!(
            "{}\n{}\n{}",
            &first[..end],
            marker_for(omitted),
            &last[start..]
        );
    }
    let mut out = head.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&marker_for(omitted));
    if !tail.is_empty() {
        out.push('\n');
        out.push_str(&tail.join("\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use alacritty_terminal::event::EventListener;
    use alacritty_terminal::term::Config;

    use super::*;
    use crate::osc::escape_command;
    use crate::pty_loop::{Hooks, Ingest};

    struct Inner {
        tracker: BlockTracker,
        events: Vec<BlockEvent>,
    }

    #[derive(Clone)]
    struct TestHooks(Arc<Mutex<Inner>>);

    impl EventListener for TestHooks {}

    impl Hooks for TestHooks {
        fn osc(&self, term: &mut Term<Self>, event: OscEvent) {
            let mut g = self.0.lock().unwrap();
            let ev = g.tracker.on_osc(term, event, Clock::now());
            g.events.extend(ev);
        }

        fn after_slice(&self, term: &mut Term<Self>) {
            self.0.lock().unwrap().tracker.after_slice(term);
        }
    }

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }

        fn screen_lines(&self) -> usize {
            self.1
        }

        fn columns(&self) -> usize {
            self.0
        }
    }

    /// A terminal fed through the same ingest path the session uses.
    struct Harness {
        term: Term<TestHooks>,
        ingest: Ingest,
        hooks: TestHooks,
    }

    impl Harness {
        fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
            let hooks = TestHooks(Arc::new(Mutex::new(Inner {
                tracker: BlockTracker::new(scrollback),
                events: Vec::new(),
            })));
            let cfg = Config {
                scrolling_history: term_history_cap(scrollback),
                ..Config::default()
            };
            let term = Term::new(cfg, &Size(cols, rows), hooks.clone());
            Self {
                term,
                ingest: Ingest::default(),
                hooks,
            }
        }

        fn feed(&mut self, s: &str) {
            let hooks = self.hooks.clone();
            self.ingest.feed(&mut self.term, &hooks, s.as_bytes());
        }

        fn blocks(&self) -> Vec<Block> {
            self.hooks.0.lock().unwrap().tracker.blocks()
        }

        fn events(&self) -> Vec<BlockEvent> {
            self.hooks.0.lock().unwrap().events.clone()
        }

        fn output(&self, id: BlockId) -> String {
            let g = self.hooks.0.lock().unwrap();
            let b = g.tracker.get(id).unwrap();
            let end = g.tracker.output_limit(&self.term, b);
            g.tracker.text_between(&self.term, b.output_start, end)
        }

        fn history(&self) -> usize {
            self.term.grid().history_size()
        }

        fn dropped(&self) -> u64 {
            self.hooks.0.lock().unwrap().tracker.dropped
        }
    }

    const A: &str = "\x1b]133;A\x07";
    const B: &str = "\x1b]133;B\x07";
    const C: &str = "\x1b]133;C\x07";

    fn d(code: i32) -> String {
        format!("\x1b]133;D;{code}\x07")
    }

    /// One whole interaction: prompt, typed command, output, finish.
    fn run(h: &mut Harness, cmd: &str, output: &str, code: i32) {
        h.feed(&format!("{A}PS> {B}{cmd}\r\n{C}{output}{}", d(code)));
    }

    #[test]
    fn a_finished_command_becomes_a_block() {
        let mut h = Harness::new(40, 10, 100);
        run(&mut h, "echo hi", "hi\r\n", 0);
        h.feed(&format!("{A}PS> {B}"));
        let blocks = h.blocks();
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert_eq!(b.command.as_deref(), Some("echo hi"));
        assert_eq!(b.state, BlockState::Succeeded);
        assert_eq!(b.exit_code, Some(0));
        assert!(b.duration.is_some() && b.finished_at.is_some() && b.finished_wall.is_some());
        assert_eq!(
            (b.prompt_line, b.command_line, b.output_start, b.output_end),
            (0, 0, 1, Some(2))
        );
        assert_eq!(h.output(b.id), "hi");
        assert_eq!(
            h.events()
                .iter()
                .map(|e| matches!(e, BlockEvent::Started(_)))
                .collect::<Vec<_>>(),
            vec![true, false]
        );
    }

    #[test]
    fn failing_command_is_marked_failed() {
        let mut h = Harness::new(40, 10, 100);
        run(&mut h, "cmd /c exit 3", "", 3);
        let b = &h.blocks()[0];
        assert_eq!((b.state, b.exit_code), (BlockState::Failed, Some(3)));
        assert!(matches!(
            h.events()[1],
            BlockEvent::Finished {
                exit_code: Some(3),
                ..
            }
        ));
        assert_eq!(h.output(b.id), "");
    }

    #[test]
    fn wrapped_command_lines_are_joined() {
        let mut h = Harness::new(20, 10, 100);
        let cmd = "git commit -m \"a quite long message that wraps around\"";
        run(&mut h, cmd, "ok\r\n", 0);
        let b = &h.blocks()[0];
        assert_eq!(b.command.as_deref(), Some(cmd));
        // The output starts after the wrapped command lines.
        assert_eq!(b.output_start, 3);
        assert_eq!(h.output(b.id), "ok");
    }

    #[test]
    fn empty_command_has_no_text() {
        let mut h = Harness::new(40, 10, 100);
        run(&mut h, "", "", 0);
        let b = &h.blocks()[0];
        assert_eq!(b.command, None);
        assert_eq!(b.state, BlockState::Succeeded);
    }

    #[test]
    fn exact_command_from_osc633_wins_over_the_grid() {
        let mut h = Harness::new(40, 10, 100);
        let exact = "echo \"a;b\" # ñ\tx";
        h.feed(&format!(
            "{A}PS> {B}ec\r\n\x1b]633;E;{}\x07{C}out\r\n{}",
            escape_command(exact),
            d(0)
        ));
        assert_eq!(h.blocks()[0].command.as_deref(), Some(exact.trim()));
        // E arriving after C (other integrations) also works.
        h.feed(&format!(
            "{A}PS> {B}x\r\n{C}\x1b]633;E;late\x07out\r\n{}",
            d(0)
        ));
        assert_eq!(h.blocks()[1].command.as_deref(), Some("late"));
        // An E without any following C does not leak into the next block.
        h.feed(&format!("{A}PS> {B}y\r\n{C}z\r\n{}", d(0)));
        assert_eq!(h.blocks()[2].command.as_deref(), Some("y"));
    }

    #[test]
    fn interrupted_commands() {
        let mut h = Harness::new(40, 10, 100);
        // Ctrl+C: D without a code, then with 130.
        h.feed(&format!("{A}PS> {B}sleep 9\r\n{C}^C\r\n\x1b]133;D\x07"));
        h.feed(&format!("{A}PS> {B}sleep 9\r\n{C}^C\r\n{}", d(130)));
        let blocks = h.blocks();
        assert_eq!(
            (blocks[0].state, blocks[0].exit_code),
            (BlockState::Unknown, None)
        );
        assert_eq!(
            (blocks[1].state, blocks[1].exit_code),
            (BlockState::Failed, Some(130))
        );
    }

    #[test]
    fn nested_and_overlapping_prompts() {
        let mut h = Harness::new(40, 10, 100);
        // An outer `pwsh` never reports D before the inner shell prints its own prompt.
        h.feed(&format!("{A}PS> {B}pwsh\r\n{C}"));
        h.feed(&format!("{A}inner> {B}echo 1\r\n{C}1\r\n{}", d(0)));
        h.feed(&format!("{A}inner> {B}"));
        // A second prompt without a command (empty Enter) leaves no block.
        h.feed(&format!("\r\n{A}inner> {B}"));
        // A stray D with nothing running is ignored.
        h.feed(&d(0));
        let blocks = h.blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].command.as_deref(), Some("pwsh"));
        assert_eq!(blocks[0].state, BlockState::Unknown);
        assert_eq!(blocks[1].command.as_deref(), Some("echo 1"));
        assert_eq!(blocks[1].state, BlockState::Succeeded);
        // A new C while running closes the previous block as unknown.
        h.feed(&format!("{A}p> {B}one\r\n{C}{A}p> {B}two\r\n{C}"));
        let blocks = h.blocks();
        assert_eq!(blocks[2].state, BlockState::Unknown);
        assert_eq!(blocks[3].state, BlockState::Running);
        assert_eq!(blocks[3].output_end, None);
    }

    #[test]
    fn cwd_is_captured_when_the_command_starts() {
        let mut h = Harness::new(40, 10, 100);
        h.feed("\x1b]7;file:///C:/one\x07");
        h.feed(&format!(
            "{A}PS> {B}cd two\r\n{C}\x1b]7;file:///C:/two\x07{}",
            d(0)
        ));
        h.feed(&format!("{A}PS> {B}ls\r\n{C}"));
        let blocks = h.blocks();
        let norm = |p: &Option<PathBuf>| p.as_ref().map(|p| p.to_string_lossy().replace('\\', "/"));
        assert_eq!(norm(&blocks[0].cwd).as_deref(), Some("C:/one"));
        assert_eq!(norm(&blocks[1].cwd).as_deref(), Some("C:/two"));
    }

    #[test]
    fn running_block_output_follows_the_cursor() {
        let mut h = Harness::new(40, 10, 100);
        h.feed(&format!(
            "{A}PS> {B}build\r\n{C}line 1\r\nline 2\r\npartial"
        ));
        let b = &h.blocks()[0];
        assert_eq!(b.state, BlockState::Running);
        assert_eq!(h.output(b.id), "line 1\nline 2\npartial");
    }

    #[test]
    fn coordinates_survive_scrolling() {
        let mut h = Harness::new(40, 5, 1000);
        for i in 0..30 {
            run(&mut h, &format!("cmd{i}"), &format!("out{i}\r\n"), 0);
        }
        assert!(h.history() > 20, "the screen only holds 5 lines");
        let blocks = h.blocks();
        assert_eq!(blocks.len(), 30);
        for (i, b) in blocks.iter().enumerate() {
            assert_eq!(b.prompt_line, 2 * i as u64);
            assert_eq!(h.output(b.id), format!("out{i}"), "block {i}");
        }
    }

    #[test]
    fn trimming_drops_and_clamps_blocks() {
        let mut h = Harness::new(40, 5, 20);
        for i in 0..40 {
            run(&mut h, &format!("cmd{i}"), &format!("out{i}\r\n"), 0);
        }
        // 80 lines printed, 5 on screen, at most 20 in history.
        assert!(h.history() <= 20);
        assert_eq!(
            h.dropped() + h.history() as u64 + 5,
            81,
            "numbering stays exact (cursor on line 80)"
        );
        let blocks = h.blocks();
        assert!(blocks.len() < 40 && blocks.len() >= 10, "{}", blocks.len());
        // The surviving blocks are the newest ones and their text is still right.
        let last = blocks.last().unwrap();
        assert_eq!(last.command.as_deref(), Some("cmd39"));
        assert_eq!(h.output(last.id), "out39");
        let first = &blocks[0];
        assert!(
            first.prompt_line >= h.dropped(),
            "clamped into the surviving range"
        );
        let n: usize = first
            .command
            .as_deref()
            .unwrap()
            .trim_start_matches("cmd")
            .parse()
            .unwrap();
        assert_eq!(h.output(first.id), format!("out{n}"));
    }

    #[test]
    fn flooding_output_keeps_the_grid_bounded_and_the_numbering_exact() {
        let mut h = Harness::new(40, 5, 50);
        h.feed(&format!("{A}PS> {B}yes\r\n{C}"));
        let flood: String = (0..30_000).map(|i| format!("{i}\r\n")).collect();
        h.feed(&flood);
        assert!(h.history() <= 50);
        let b = h.blocks()[0].clone();
        assert_eq!(b.state, BlockState::Running);
        assert!(b.output_start >= h.dropped(), "clamped");
        h.feed(&d(0));
        let b = h.blocks()[0].clone();
        assert_eq!((b.state, b.exit_code), (BlockState::Succeeded, Some(0)));
        assert!(b.output_end.is_some_and(|e| e >= b.output_start));
        let text = h.output(b.id);
        assert!(text.ends_with("29999"), "{text}");
    }

    #[test]
    fn clearing_the_history_expires_old_blocks() {
        let mut h = Harness::new(40, 5, 100);
        for i in 0..10 {
            run(&mut h, &format!("c{i}"), "o\r\n", 0);
        }
        assert!(h.history() > 0);
        let before = h.blocks().len();
        h.feed("\x1b[3J");
        assert_eq!(h.history(), 0);
        let after = h.blocks();
        assert!(
            after.len() < before,
            "blocks living only in the cleared history are gone"
        );
        // New work after the clear still lines up.
        run(&mut h, "fresh", "new\r\n", 0);
        let last = h.blocks().last().unwrap().clone();
        assert_eq!(last.command.as_deref(), Some("fresh"));
        assert_eq!(h.output(last.id), "new");
    }

    #[test]
    fn clearing_the_screen() {
        // `ESC[2J` scrolls the old screen into history: its blocks stay valid.
        let mut h = Harness::new(40, 10, 100);
        run(&mut h, "one", "1\r\n", 0);
        run(&mut h, "two", "2\r\n", 0);
        h.feed("\x1b[H\x1b[2J");
        h.feed(&format!("{A}PS> {B}"));
        assert_eq!(h.blocks().len(), 2);
        run(&mut h, "three", "3\r\n", 0);
        let blocks = h.blocks();
        assert_eq!(blocks.len(), 3);
        assert_eq!(h.output(blocks[0].id), "1");
        assert_eq!(h.output(blocks[2].id), "3");

        // `ESC[H ESC[0J` erases in place: blocks painted over are discarded.
        let mut h = Harness::new(40, 10, 100);
        run(&mut h, "one", "1\r\n", 0);
        run(&mut h, "two", "2\r\n", 0);
        h.feed("\x1b[H\x1b[0J");
        h.feed(&format!("{A}PS> {B}"));
        assert!(h.blocks().is_empty());
        run(&mut h, "three", "3\r\n", 0);
        assert_eq!(h.blocks().len(), 1);
        assert_eq!(h.blocks()[0].command.as_deref(), Some("three"));
    }
    #[test]
    fn marks_inside_the_alternate_screen_are_ignored() {
        let mut h = Harness::new(40, 10, 100);
        run(&mut h, "vim", "", 0);
        h.feed("\x1b[?1049h");
        h.feed(&format!("{A}x{B}y{C}z{}", d(1)));
        h.feed("\x1b[?1049l");
        assert_eq!(h.blocks().len(), 1);
    }

    #[test]
    fn at_most_500_blocks_are_kept() {
        let mut h = Harness::new(40, 5, 100_000);
        for i in 0..520 {
            h.feed(&format!("{A}{B}c{i}\r\n{C}{}", d(0)));
        }
        let blocks = h.blocks();
        assert_eq!(blocks.len(), MAX_BLOCKS);
        assert_eq!(blocks[0].id, BlockId(21));
        assert_eq!(blocks.last().unwrap().id, BlockId(520));
    }

    #[test]
    fn lookup_helpers() {
        let mut h = Harness::new(40, 6, 100);
        for i in 0..4 {
            run(&mut h, &format!("c{i}"), "o\r\n", 0);
        }
        let g = h.hooks.0.lock().unwrap();
        let t = &g.tracker;
        let hist = h.term.grid().history_size();
        let blocks = t.blocks();
        // Bottom of the screen shows the newest lines: rows 0..6 are abs 3..9 (history 3).
        assert_eq!(hist, 3);
        assert_eq!(t.abs_of_viewport_row(hist, 0, 0), 3);
        assert_eq!(t.block_at_abs(0), Some(blocks[0].id));
        assert_eq!(t.block_at_abs(1), Some(blocks[0].id));
        assert_eq!(t.block_at_abs(2), Some(blocks[1].id));
        assert_eq!(t.block_at_abs(99), None);
        // Scrolled 3 lines up, row 0 is abs 0.
        assert_eq!(t.abs_of_viewport_row(hist, 3, 0), 0);
        assert_eq!(t.offset_for_top(hist, 0), 3);
        assert_eq!(t.offset_for_top(hist, 5), -2);
        // Navigation relative to a block and to the viewport.
        assert_eq!(t.prev(Some(blocks[2].id), 0), Some(blocks[1].id));
        assert_eq!(t.prev(Some(blocks[0].id), 0), None);
        assert_eq!(t.next(Some(blocks[2].id), 0), Some(blocks[3].id));
        assert_eq!(t.next(Some(blocks[3].id), 0), None);
        assert_eq!(
            t.prev(None, 4),
            Some(blocks[1].id),
            "starts at 2 < 4, next at 4 is not < 4"
        );
        assert_eq!(t.next(None, 4), Some(blocks[3].id));
        assert_eq!(t.prev(None, 0), None);
        assert_eq!(t.get(BlockId(3)).map(|b| b.id), Some(BlockId(3)));
        assert!(t.get(BlockId(99)).is_none());
    }

    #[test]
    fn resize_rules() {
        let mut t = BlockTracker::new(10);
        let mut h = Harness::new(40, 5, 100);
        run(&mut h, "x", "y\r\n", 0);
        {
            let mut g = h.hooks.0.lock().unwrap();
            // Same width: nothing is lost.
            g.tracker.after_resize(0, false);
            assert_eq!(g.tracker.blocks().len(), 1);
            // Reflow: every stored line is stale.
            g.tracker.after_resize(0, true);
            assert!(g.tracker.blocks().is_empty());
        }
        t.after_resize(0, true);
        assert!(t.blocks().is_empty());
    }

    #[test]
    fn truncation_marks_the_omitted_lines() {
        let text: String = (1..=100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(truncate_middle(&text, text.len()), text);
        let out = truncate_middle(&text, 200);
        assert!(out.len() <= 200, "{} bytes", out.len());
        assert!(out.starts_with("line 1\nline 2"));
        assert!(out.ends_with("line 99\nline 100"));
        let marker = out.lines().find(|l| l.contains("omitidas")).unwrap();
        let n: usize = marker
            .trim_start_matches("… ")
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let kept = out.lines().count() - 1;
        assert_eq!(n + kept, 100, "{out}");
        // A single enormous line is cut on char boundaries.
        let giant = "ñ".repeat(5000);
        let out = truncate_middle(&giant, 300);
        assert!(out.len() < 400 && out.contains("omitidas"));
        // A budget smaller than the marker still yields something sane.
        assert!(truncate_middle(&text, 5).contains("omitidas"));
    }

    #[test]
    fn clean_lines_trims_each_line_and_the_end() {
        assert_eq!(clean_lines("a  \n  b \n\n \n"), "a\n  b");
        assert_eq!(clean_lines("   \n"), "");
    }
}
