//! Review notes: the data model (pure, no gpui) behind the line notes of a diff review.
//!
//! A note is anchored to a **side** of a file diff (the new file for added / context lines, the old
//! one for removed lines) and a 1-based inclusive line range, plus a content hash of the anchored
//! lines. When the diff is refreshed (the agent kept working) line numbers shift; [`Note::reanchor`]
//! finds the lines again by hash inside a search window instead of trusting the old numbers.
//!
//! States: [`NoteState::Draft`] (Borrador) -> [`NoteState::Sent`] (Enviada, after the host calls
//! `mark_sent`) -> [`NoteState::Resolved`] (Resuelta).

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Lines of context kept around a note for the Markdown excerpt.
pub const EXCERPT_RADIUS: usize = 3;
/// How far (in lines, each direction) [`Note::reanchor`] looks for moved lines.
pub const SEARCH_WINDOW: usize = 60;

/// Identity of a note inside a [`NoteSet`] (stable across export / import).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NoteId(pub u64);

/// Lifecycle of a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteState {
    /// Written, not sent yet.
    Draft,
    /// Included in a message the host delivered to the agent.
    Sent,
    /// The agent (or you) dealt with it.
    Resolved,
}

impl NoteState {
    /// UI label (Spanish).
    pub const fn label(self) -> &'static str {
        match self {
            NoteState::Draft => "Borrador",
            NoteState::Sent => "Enviada",
            NoteState::Resolved => "Resuelta",
        }
    }
}

/// Which file of the diff the line numbers refer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The old file (removed lines).
    Old,
    /// The new file (added and context lines).
    New,
}

/// Lines of one side of a file diff that are visible in the diff (changes plus context).
pub type SideLines = BTreeMap<usize, String>;

/// FNV-1a 64 over `bytes` (stable across runs and Rust versions, unlike `DefaultHasher`).
fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Hash of a line ignoring surrounding whitespace (re-indenting must not lose the anchor).
pub fn line_hash(text: &str) -> u64 {
    fnv(FNV_OFFSET, text.trim().as_bytes())
}

/// Hash of consecutive lines.
pub fn lines_hash<'a>(lines: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut h = FNV_OFFSET;
    for l in lines {
        h = fnv(h, &line_hash(l).to_le_bytes());
        h = fnv(h, b"\n");
    }
    h
}

/// Hash of `start..=end` of `lines`; `None` when any of those lines is not visible.
pub fn range_hash(lines: &SideLines, start: usize, end: usize) -> Option<u64> {
    if start == 0 || end < start {
        return None;
    }
    let mut texts = Vec::with_capacity(end - start + 1);
    for n in start..=end {
        texts.push(lines.get(&n)?.as_str());
    }
    Some(lines_hash(texts))
}

/// The lines around `start..=end` (at most [`EXCERPT_RADIUS`] each way) that are visible.
pub fn excerpt(lines: &SideLines, start: usize, end: usize) -> Vec<(usize, String)> {
    let from = start.saturating_sub(EXCERPT_RADIUS).max(1);
    let to = end + EXCERPT_RADIUS;
    lines
        .range(from..=to)
        .map(|(n, t)| (*n, t.clone()))
        .collect()
}

/// What [`Note::reanchor`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reanchor {
    /// The lines are still where the note says.
    Kept,
    /// The lines moved by this many lines (positive = down).
    Moved(isize),
    /// The lines are gone or changed: the note stays at its last position, marked outdated.
    Stale,
}

/// A line note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    /// Path relative to the repository root, with forward slashes.
    pub path: String,
    pub side: Side,
    /// First anchored line (1-based, inclusive) on `side`.
    pub start: usize,
    /// Last anchored line (inclusive).
    pub end: usize,
    pub text: String,
    pub state: NoteState,
    /// [`lines_hash`] of the anchored lines at the time they were last seen.
    pub anchor_hash: u64,
    /// Lines around the anchor (numbered), kept so the message still has its excerpt when the
    /// lines are no longer in the diff.
    pub snippet: Vec<(usize, String)>,
    /// The anchored lines are not in the latest diff.
    pub stale: bool,
    /// Unix seconds.
    pub created: u64,
}

impl Note {
    /// A new draft anchored to `start..=end` of `lines`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: NoteId,
        path: &str,
        side: Side,
        start: usize,
        end: usize,
        text: &str,
        lines: &SideLines,
        created: u64,
    ) -> Self {
        let (start, end) = if end < start {
            (end, start)
        } else {
            (start, end)
        };
        Self {
            id,
            path: path.to_string(),
            side,
            start,
            end,
            text: text.trim().to_string(),
            state: NoteState::Draft,
            anchor_hash: range_hash(lines, start, end).unwrap_or(0),
            snippet: excerpt(lines, start, end),
            stale: false,
            created,
        }
    }

    /// Number of anchored lines.
    pub fn len(&self) -> usize {
        self.end + 1 - self.start
    }

    /// Never empty (at least one line).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// `path:12`, `path:12-15`, plus `(antes)` for the old side.
    pub fn location(&self) -> String {
        let range = if self.start == self.end {
            self.start.to_string()
        } else {
            format!("{}-{}", self.start, self.end)
        };
        match self.side {
            Side::New => format!("{}:{range}", self.path),
            Side::Old => format!("{}:{range} (versión anterior)", self.path),
        }
    }

    /// Finds the anchored lines again in `lines` (the visible lines of this note's side after a
    /// refresh): first at the same numbers, then by content hash in a +-[`SEARCH_WINDOW`] window,
    /// nearest first (ties go down). Updates the numbers, the excerpt and the stale flag.
    pub fn reanchor(&mut self, lines: &SideLines) -> Reanchor {
        let len = self.len();
        let matches_at = |s: usize| range_hash(lines, s, s + len - 1) == Some(self.anchor_hash);
        let found = if self.anchor_hash == 0 {
            None
        } else {
            (0..=SEARCH_WINDOW).find_map(|d| {
                let down = self.start + d;
                if matches_at(down) {
                    return Some(down);
                }
                let up = self.start.checked_sub(d).filter(|s| *s >= 1)?;
                (d > 0 && matches_at(up)).then_some(up)
            })
        };
        match found {
            Some(s) => {
                let delta = s as isize - self.start as isize;
                self.start = s;
                self.end = s + len - 1;
                self.stale = false;
                self.snippet = excerpt(lines, self.start, self.end);
                if delta == 0 {
                    Reanchor::Kept
                } else {
                    Reanchor::Moved(delta)
                }
            }
            None => {
                self.stale = true;
                Reanchor::Stale
            }
        }
    }
}

/// What the host gets back with `SendToAgent` to mark the notes as sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteRef {
    pub id: NoteId,
    pub path: String,
    /// First line of the note.
    pub line: usize,
}

impl From<&Note> for NoteRef {
    fn from(n: &Note) -> Self {
        Self {
            id: n.id,
            path: n.path.clone(),
            line: n.start,
        }
    }
}

/// The notes of one review, in creation order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteSet {
    notes: Vec<Note>,
    next_id: u64,
}

/// JSON envelope of [`NoteSet::to_json`].
#[derive(Serialize, Deserialize)]
struct Export {
    version: u32,
    notes: Vec<Note>,
}

impl NoteSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_notes(notes: Vec<Note>) -> Self {
        let next_id = notes.iter().map(|n| n.id.0 + 1).max().unwrap_or(0);
        Self { notes, next_id }
    }

    /// Allocates an id (monotonic; never reused inside the set).
    pub fn next_id(&mut self) -> NoteId {
        let id = NoteId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn get(&self, id: NoteId) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn get_mut(&mut self, id: NoteId) -> Option<&mut Note> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    pub fn push(&mut self, note: Note) {
        self.next_id = self.next_id.max(note.id.0 + 1);
        self.notes.push(note);
    }

    pub fn remove(&mut self, id: NoteId) -> bool {
        let before = self.notes.len();
        self.notes.retain(|n| n.id != id);
        self.notes.len() != before
    }

    /// Edits the text; an empty text is refused (returns `false`). A note that was already sent
    /// goes back to a draft: the agent has not seen the new wording.
    pub fn set_text(&mut self, id: NoteId, text: &str) -> bool {
        let text = text.trim();
        match self.get_mut(id) {
            Some(n) if !text.is_empty() => {
                if n.text != text {
                    n.text = text.to_string();
                    if n.state == NoteState::Sent {
                        n.state = NoteState::Draft;
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn set_state(&mut self, id: NoteId, state: NoteState) -> bool {
        match self.get_mut(id) {
            Some(n) => {
                n.state = state;
                true
            }
            None => false,
        }
    }

    /// `Draft` -> `Sent` for each id (other states are left alone). Returns how many changed.
    pub fn mark_sent(&mut self, ids: &[NoteId]) -> usize {
        let mut n = 0;
        for note in &mut self.notes {
            if ids.contains(&note.id) && note.state == NoteState::Draft {
                note.state = NoteState::Sent;
                n += 1;
            }
        }
        n
    }

    pub fn drafts(&self) -> impl Iterator<Item = &Note> {
        self.notes.iter().filter(|n| n.state == NoteState::Draft)
    }

    pub fn count(&self, state: NoteState) -> usize {
        self.notes.iter().filter(|n| n.state == state).count()
    }

    /// Notes of `path`, in line order.
    pub fn for_path(&self, path: &str) -> Vec<&Note> {
        let mut v: Vec<&Note> = self.notes.iter().filter(|n| n.path == path).collect();
        v.sort_by_key(|n| (n.side == Side::New, n.start, n.id));
        v
    }

    /// Paths that have at least one note.
    pub fn paths(&self) -> BTreeSet<&str> {
        self.notes.iter().map(|n| n.path.as_str()).collect()
    }

    /// Re-anchors every note of `path` against the refreshed lines of both sides.
    pub fn reanchor_path(&mut self, path: &str, old: &SideLines, new: &SideLines) {
        for n in self.notes.iter_mut().filter(|n| n.path == path) {
            n.reanchor(match n.side {
                Side::Old => old,
                Side::New => new,
            });
        }
    }

    /// Marks the notes of files that are no longer part of the review as stale.
    pub fn mark_missing_stale(&mut self, present: &BTreeSet<String>) {
        for n in &mut self.notes {
            if !present.contains(&n.path) {
                n.stale = true;
            }
        }
    }

    /// `{"version":1,"notes":[...]}`.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&Export {
            version: 1,
            notes: self.notes.clone(),
        })
        .unwrap_or_else(|_| "{}".to_string())
    }

    /// Merges notes exported with [`NoteSet::to_json`]: a note that already exists (same place,
    /// text and anchor) is skipped, the rest get fresh ids. Returns how many were added.
    pub fn merge_json(&mut self, json: &str) -> Result<usize, String> {
        let export: Export =
            serde_json::from_str(json).map_err(|e| format!("notas ilegibles: {e}"))?;
        if export.version != 1 {
            return Err(format!("versión de notas no soportada: {}", export.version));
        }
        let mut added = 0;
        for mut note in export.notes {
            let dup = self.notes.iter().any(|n| {
                n.path == note.path
                    && n.side == note.side
                    && n.start == note.start
                    && n.end == note.end
                    && n.text == note.text
            });
            if dup || note.text.trim().is_empty() || note.start == 0 || note.end < note.start {
                continue;
            }
            note.id = self.next_id();
            self.notes.push(note);
            added += 1;
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(range: std::ops::RangeInclusive<usize>, f: impl Fn(usize) -> String) -> SideLines {
        range.map(|n| (n, f(n))).collect()
    }

    fn base() -> SideLines {
        lines(1..=30, |n| format!("let value_{n} = compute({n});"))
    }

    /// `base()` with `k` new lines inserted before line `at` (everything after shifts down).
    fn shifted(at: usize, k: usize) -> SideLines {
        let mut out = SideLines::new();
        for n in 1..=30 {
            let new_n = if n >= at { n + k } else { n };
            out.insert(new_n, format!("let value_{n} = compute({n});"));
        }
        for i in 0..k {
            out.insert(at + i, format!("// inserted {i}"));
        }
        out
    }

    #[test]
    fn hashes_ignore_indentation_and_are_stable() {
        assert_eq!(line_hash("  foo();"), line_hash("foo();   "));
        assert_ne!(line_hash("foo();"), line_hash("bar();"));
        // FNV-1a of "a" is a published constant: guards against accidental algorithm changes.
        assert_eq!(fnv(FNV_OFFSET, b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn range_hash_needs_every_line() {
        let l = lines(5..=6, |n| n.to_string());
        assert!(range_hash(&l, 5, 6).is_some());
        assert!(range_hash(&l, 5, 7).is_none());
        assert!(range_hash(&l, 0, 1).is_none());
    }

    #[test]
    fn new_note_captures_anchor_and_excerpt() {
        let n = Note::new(
            NoteId(1),
            "a.rs",
            Side::New,
            10,
            10,
            "  usa ?  ",
            &base(),
            7,
        );
        assert_eq!(n.text, "usa ?");
        assert_eq!(n.state, NoteState::Draft);
        assert_eq!(n.snippet.first().map(|s| s.0), Some(7));
        assert_eq!(n.snippet.last().map(|s| s.0), Some(13));
        assert_ne!(n.anchor_hash, 0);
        // A reversed range is normalised.
        let r = Note::new(NoteId(2), "a.rs", Side::New, 12, 10, "x", &base(), 0);
        assert_eq!((r.start, r.end), (10, 12));
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn anchor_survives_lines_shifting_down() {
        let mut n = Note::new(NoteId(1), "a.rs", Side::New, 20, 21, "ojo", &base(), 0);
        let out = n.reanchor(&shifted(5, 4));
        assert_eq!(out, Reanchor::Moved(4));
        assert_eq!((n.start, n.end), (24, 25));
        assert!(!n.stale);
        assert_eq!(
            n.snippet
                .iter()
                .find(|(l, _)| *l == 24)
                .map(|(_, t)| t.as_str()),
            Some("let value_20 = compute(20);")
        );
    }

    #[test]
    fn anchor_survives_lines_shifting_up() {
        let mut after = SideLines::new();
        for n in 1..=30usize {
            if n <= 3 {
                continue; // three lines deleted at the top
            }
            after.insert(n - 3, format!("let value_{n} = compute({n});"));
        }
        let mut n = Note::new(NoteId(1), "a.rs", Side::New, 12, 12, "x", &base(), 0);
        assert_eq!(n.reanchor(&after), Reanchor::Moved(-3));
        assert_eq!(n.start, 9);
    }

    #[test]
    fn unchanged_lines_are_kept() {
        let mut n = Note::new(NoteId(1), "a.rs", Side::New, 8, 9, "x", &base(), 0);
        assert_eq!(n.reanchor(&base()), Reanchor::Kept);
        assert!(!n.stale);
    }

    #[test]
    fn edited_or_distant_lines_make_the_note_stale_and_it_recovers() {
        let mut n = Note::new(NoteId(1), "a.rs", Side::New, 10, 10, "x", &base(), 0);
        let mut edited = base();
        edited.insert(10, "let value_10 = something_else();".to_string());
        assert_eq!(n.reanchor(&edited), Reanchor::Stale);
        assert!(n.stale);
        assert_eq!(n.start, 10, "a stale note keeps its last position");
        // Outside the search window it is also lost...
        let far = shifted(1, SEARCH_WINDOW + 10);
        assert_eq!(n.reanchor(&far), Reanchor::Stale);
        // ...and found again when the lines come back.
        assert_eq!(n.reanchor(&base()), Reanchor::Kept);
        assert!(!n.stale);
    }

    #[test]
    fn duplicate_lines_resolve_to_the_nearest() {
        let mut l = SideLines::new();
        for n in 1..=20usize {
            l.insert(
                n,
                if n % 5 == 0 {
                    "}".to_string()
                } else {
                    format!("line {n}")
                },
            );
        }
        let mut note = Note::new(NoteId(1), "a.rs", Side::New, 10, 10, "x", &l, 0);
        // One line inserted at the top: every `}` moves down by one (6, 11, 16, 21).
        let mut after = SideLines::new();
        for (n, t) in &l {
            after.insert(n + 1, t.clone());
        }
        after.insert(1, "new".to_string());
        // The candidates are 6, 11, 16 and 21: the nearest to the old position (11) wins.
        assert_eq!(note.reanchor(&after), Reanchor::Moved(1));
    }

    #[test]
    fn removed_side_anchors_independently() {
        let old = lines(1..=5, |n| format!("old {n}"));
        let new = lines(1..=5, |n| format!("new {n}"));
        let mut note = Note::new(
            NoteId(1),
            "a.rs",
            Side::Old,
            3,
            3,
            "por que se borra?",
            &old,
            0,
        );
        let mut set = NoteSet::new();
        set.push(note.clone());
        set.reanchor_path("a.rs", &old, &new);
        assert!(!set.notes()[0].stale);
        // Against the new side the same hash does not exist.
        assert_eq!(note.reanchor(&new), Reanchor::Stale);
    }

    #[test]
    fn state_machine() {
        let mut set = NoteSet::new();
        let (a, b) = (set.next_id(), set.next_id());
        set.push(Note::new(a, "x.rs", Side::New, 1, 1, "uno", &base(), 0));
        set.push(Note::new(b, "x.rs", Side::New, 2, 2, "dos", &base(), 0));
        assert_eq!(set.count(NoteState::Draft), 2);
        assert_eq!(set.mark_sent(&[a]), 1);
        assert_eq!(set.mark_sent(&[a]), 0, "only drafts become sent");
        assert_eq!(set.count(NoteState::Sent), 1);
        assert!(set.set_state(a, NoteState::Resolved));
        assert_eq!(set.mark_sent(&[a]), 0, "resolved stays resolved");
        // Editing a sent note makes it a draft again.
        assert!(set.set_state(b, NoteState::Sent));
        assert!(set.set_text(b, "dos, mejor dicho"));
        assert_eq!(set.get(b).map(|n| n.state), Some(NoteState::Draft));
        assert!(!set.set_text(b, "   "), "empty text is refused");
        assert!(set.remove(a));
        assert!(!set.remove(a));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn json_round_trip_and_merge() {
        let mut set = NoteSet::new();
        let id = set.next_id();
        set.push(Note::new(
            id,
            "src/a.rs",
            Side::New,
            4,
            5,
            "revisa esto",
            &base(),
            99,
        ));
        let json = set.to_json();

        let mut other = NoteSet::new();
        assert_eq!(other.merge_json(&json), Ok(1));
        assert_eq!(other.notes()[0].text, "revisa esto");
        assert_eq!(other.notes()[0].anchor_hash, set.notes()[0].anchor_hash);
        // Importing twice does not duplicate.
        assert_eq!(other.merge_json(&json), Ok(0));
        // Fresh ids never collide with existing ones.
        let mut third = NoteSet::from_notes(vec![Note::new(
            NoteId(0),
            "z",
            Side::New,
            1,
            1,
            "z",
            &base(),
            0,
        )]);
        assert_eq!(third.merge_json(&json), Ok(1));
        assert_ne!(third.notes()[0].id, third.notes()[1].id);

        assert!(NoteSet::new().merge_json("no es json").is_err());
        assert!(
            NoteSet::new()
                .merge_json("{\"version\":9,\"notes\":[]}")
                .is_err()
        );
    }

    #[test]
    fn locations() {
        let n = Note::new(NoteId(1), "src/a.rs", Side::New, 12, 12, "x", &base(), 0);
        assert_eq!(n.location(), "src/a.rs:12");
        let m = Note::new(NoteId(1), "src/a.rs", Side::New, 12, 15, "x", &base(), 0);
        assert_eq!(m.location(), "src/a.rs:12-15");
        let o = Note::new(
            NoteId(1),
            "src/a.rs",
            Side::Old,
            3,
            3,
            "x",
            &lines(1..=5, |n| n.to_string()),
            0,
        );
        assert_eq!(o.location(), "src/a.rs:3 (versión anterior)");
    }

    #[test]
    fn missing_files_become_stale() {
        let mut set = NoteSet::from_notes(vec![
            Note::new(NoteId(0), "a.rs", Side::New, 1, 1, "x", &base(), 0),
            Note::new(NoteId(1), "b.rs", Side::New, 1, 1, "y", &base(), 0),
        ]);
        set.mark_missing_stale(&["a.rs".to_string()].into_iter().collect());
        assert!(!set.notes()[0].stale);
        assert!(set.notes()[1].stale);
    }
}
