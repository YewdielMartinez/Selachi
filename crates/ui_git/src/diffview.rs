//! Inline diff preview: unified-diff parsing, syntax highlight through `viewer_svc` and the flat,
//! virtualisable row list the view paints.
//!
//! Tracked files take their text from `git diff` (exactly the index / worktree sides the user
//! is looking at) and untracked files from `viewer_svc::diff_against_git` (a new file diffs as
//! all-added). Both produce `viewer_svc::Hunk`s. Highlights come from the file's grammar when
//! one is available (compiled in or an installed plugin); without one the diff is plain.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use viewer_svc::{Document, GitBase, Hunk, HighlightKind, Limits, LineKind};

/// Rows kept for one preview; the rest is summarised in a "…N líneas más" row.
pub const MAX_ROWS: usize = 2_500;
/// Files bigger than this are not highlighted (the diff stays plain).
const MAX_HIGHLIGHT_BYTES: u64 = 1_000_000;
/// Columns a tab is shown as.
const TAB: &str = "    ";

/// One file of a unified diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// Path relative to the repo, forward slashes.
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
}

/// Parses the output of `git diff` (any number of files) into hunks.
pub fn parse_unified(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    // Lines still expected in the current hunk, so `--- x` inside a hunk is content.
    let (mut old_left, mut new_left) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0usize, 0usize);

    fn flush(cur: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        if let (Some(f), Some(h)) = (cur.as_mut(), hunk.take()) {
            f.hunks.push(h);
        }
    }

    for line in text.lines() {
        let in_hunk = old_left > 0 || new_left > 0;
        if !in_hunk {
            if let Some(rest) = line.strip_prefix("diff --git ") {
                flush(&mut cur, &mut hunk);
                if let Some(f) = cur.take() {
                    files.push(f);
                }
                let path = rest.rsplit_once(" b/").map_or("", |(_, b)| b).to_string();
                cur = Some(FileDiff { path, hunks: Vec::new(), binary: false });
                continue;
            }
            if let Some(f) = cur.as_mut() {
                if let Some(p) = line.strip_prefix("+++ ") {
                    if p.trim() != "/dev/null" {
                        f.path = clean_path(p);
                    }
                    continue;
                }
                if let Some(p) = line.strip_prefix("--- ") {
                    if f.path.is_empty() && p.trim() != "/dev/null" {
                        f.path = clean_path(p);
                    }
                    continue;
                }
                if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                    f.binary = true;
                    continue;
                }
            }
            if let Some(h) = parse_hunk_header(line) {
                if cur.is_none() {
                    cur = Some(FileDiff { path: String::new(), hunks: Vec::new(), binary: false });
                }
                flush(&mut cur, &mut hunk);
                old_left = h.old_len;
                new_left = h.new_len;
                old_no = h.old_start;
                new_no = h.new_start;
                hunk = Some(h);
                continue;
            }
            continue;
        }
        let Some(h) = hunk.as_mut() else { continue };
        let (kind, body) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            Some('\\') => continue,
            None => (LineKind::Context, ""),
            Some(_) => (LineKind::Context, line),
        };
        let (old_line, new_line) = match kind {
            LineKind::Added => {
                new_left = new_left.saturating_sub(1);
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                old_left = old_left.saturating_sub(1);
                old_no += 1;
                (Some(old_no - 1), None)
            }
            LineKind::Context => {
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
        };
        h.lines.push(viewer_svc::DiffLine { kind, old_line, new_line, text: body.to_string(), inline: Vec::new() });
    }
    flush(&mut cur, &mut hunk);
    if let Some(f) = cur.take() {
        files.push(f);
    }
    files
}

fn clean_path(p: &str) -> String {
    let p = p.split('\t').next().unwrap_or(p).trim();
    let p = p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p);
    p.trim_matches('"').to_string()
}

/// `@@ -12,3 +14,4 @@ fn x` -> an empty hunk with the numbers.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some(Hunk { old_start, old_len, new_start, new_len, lines: Vec::new() })
}

// ---------------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------------

/// Highlight spans of a line (byte ranges of its displayed text).
pub type Spans = Vec<(Range<usize>, HighlightKind)>;

/// A line of a preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewLine {
    pub kind: LineKind,
    /// New-side number for context / added lines, old-side for removed ones.
    pub num: Option<usize>,
    /// Text with tabs expanded.
    pub text: String,
    pub spans: Spans,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Hunk(String),
    Line(PreviewLine),
    /// `N` lines were not loaded.
    More(usize),
}

/// A loaded preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffPreview {
    pub path: String,
    pub rows: Arc<Vec<DiffRow>>,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    pub highlighted: bool,
    /// New-side line of the first change, for "Abrir".
    pub first_line: Option<usize>,
}

impl DiffPreview {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Expands tabs and remaps the spans to the new byte offsets.
fn expand_tabs(text: &str, spans: Spans) -> (String, Spans) {
    if !text.contains('\t') {
        return (text.to_string(), spans);
    }
    let tabs_before = |off: usize| text.as_bytes()[..off.min(text.len())].iter().filter(|b| **b == b'\t').count();
    let extra = TAB.len() - 1;
    let spans = spans
        .into_iter()
        .map(|(r, k)| (r.start + extra * tabs_before(r.start)..r.end + extra * tabs_before(r.end), k))
        .collect();
    (text.replace('\t', TAB), spans)
}

/// Spans of `line` that are valid for `text` (char boundaries, in range, sorted, no overlap).
fn sanitize(text: &str, spans: Spans) -> Spans {
    let mut out: Spans = Vec::with_capacity(spans.len());
    let mut at = 0;
    for (r, k) in spans {
        if r.start >= r.end || r.end > text.len() || r.start < at {
            continue;
        }
        if !text.is_char_boundary(r.start) || !text.is_char_boundary(r.end) {
            continue;
        }
        at = r.end;
        out.push((r, k));
    }
    out
}

/// Highlights for the new-side lines the hunks show: `new line number (1-based) -> spans`.
/// `None` when the file has no grammar, is too big, or cannot be read.
pub fn highlight_new_lines(file: &Path, hunks: &[Hunk]) -> Option<HashMap<usize, Spans>> {
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let doc = Document::open(file, Limits::default()).ok()?;
    doc.grammar_id()?;
    doc.highlight_blocking();
    let mut out: HashMap<usize, Spans> = HashMap::new();
    for h in hunks {
        let start = h.new_start.saturating_sub(1);
        let range = start..start + h.new_len;
        let spans = doc.highlight_lines(range.clone());
        let texts = doc.lines(range.clone());
        for (i, (spans, text)) in spans.into_iter().zip(texts).enumerate() {
            let n = start + i + 1;
            // Only trust a line whose text is what the diff shows (a staged diff's new side is
            // the index, which may differ from the worktree file).
            if let Some(line) = h.lines.iter().find(|l| l.new_line == Some(n))
                && line.text == text
            {
                out.insert(n, spans);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Builds the preview rows. `highlights` are the new-side spans from [`highlight_new_lines`].
pub fn build_preview(
    path: &str,
    hunks: &[Hunk],
    binary: bool,
    highlights: Option<&HashMap<usize, Spans>>,
) -> DiffPreview {
    let mut rows: Vec<DiffRow> = Vec::new();
    let (mut added, mut removed, mut hidden) = (0usize, 0usize, 0usize);
    let mut first_line = None;
    for h in hunks {
        let full = rows.len() >= MAX_ROWS;
        if !full {
            rows.push(DiffRow::Hunk(h.header()));
        }
        for l in &h.lines {
            match l.kind {
                LineKind::Added => added += 1,
                LineKind::Removed => removed += 1,
                LineKind::Context => {}
            }
            if first_line.is_none() && l.kind != LineKind::Context {
                first_line = l.new_line.or(l.old_line);
            }
            if rows.len() >= MAX_ROWS {
                hidden += 1;
                continue;
            }
            let spans = match (l.kind, l.new_line, highlights) {
                (LineKind::Removed, _, _) | (_, None, _) | (_, _, None) => Vec::new(),
                (_, Some(n), Some(map)) => map.get(&n).cloned().unwrap_or_default(),
            };
            let (text, spans) = expand_tabs(&l.text, spans);
            let spans = sanitize(&text, spans);
            let num = match l.kind {
                LineKind::Removed => l.old_line,
                _ => l.new_line,
            };
            rows.push(DiffRow::Line(PreviewLine { kind: l.kind, num, text, spans }));
        }
    }
    if hidden > 0 {
        rows.push(DiffRow::More(hidden));
    }
    DiffPreview {
        path: path.to_string(),
        rows: Arc::new(rows),
        added,
        removed,
        binary,
        highlighted: highlights.is_some(),
        first_line,
    }
}

/// Preview of one file from a parsed diff, highlighting from `worktree_file` when given.
pub fn preview_of(file: &FileDiff, worktree_file: Option<&Path>) -> DiffPreview {
    let highlights = worktree_file.and_then(|p| highlight_new_lines(p, &file.hunks));
    build_preview(&file.path, &file.hunks, file.binary, highlights.as_ref())
}

/// Files listed for an untracked directory.
const DIR_LIST_MAX: usize = 200;

/// Preview of an untracked directory: its files, as plain rows.
fn preview_directory(abs: &Path, rel_text: &str) -> DiffPreview {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            if out.len() > DIR_LIST_MAX {
                return;
            }
            let path = e.path();
            if path.is_dir() {
                if e.file_name() != ".git" {
                    walk(&path, base, out);
                }
            } else if let Ok(r) = path.strip_prefix(base) {
                out.push(r.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut files = Vec::new();
    walk(abs, abs, &mut files);
    let more = files.len().saturating_sub(DIR_LIST_MAX);
    files.truncate(DIR_LIST_MAX);
    let mut rows = vec![DiffRow::Hunk(format!("carpeta sin seguimiento · {}", crate::fmt::plural(files.len() + more, "archivo", "archivos")))];
    rows.extend(files.into_iter().map(|f| {
        DiffRow::Line(PreviewLine { kind: LineKind::Context, num: None, text: f, spans: Vec::new() })
    }));
    if more > 0 {
        rows.push(DiffRow::More(more));
    }
    DiffPreview {
        path: rel_text.to_string(),
        rows: Arc::new(rows),
        added: 0,
        removed: 0,
        binary: false,
        highlighted: false,
        first_line: None,
    }
}

/// Preview of an untracked (or otherwise new) file: everything is added.
pub fn preview_untracked(repo_root: &Path, rel: &Path) -> Result<DiffPreview, String> {
    let abs = repo_root.join(rel);
    if abs.is_dir() {
        return Ok(preview_directory(&abs, &crate::fmt::slashes(rel)));
    }
    let hunks = viewer_svc::diff_against_git(repo_root, &abs, GitBase::Head).map_err(|e| e.to_string())?;
    let path = crate::fmt::slashes(rel);
    let highlights = highlight_new_lines(&abs, &hunks);
    Ok(build_preview(&path, &hunks, false, highlights.as_ref()))
}

/// Markdown for "Preguntar al agente": the file, where it comes from and the diff (capped).
pub fn agent_markdown(context: &str, preview: &DiffPreview) -> String {
    const MAX_LINES: usize = 300;
    let mut out = format!("## Revisión de cambios\n\nArchivo: `{}` ({context})\n\n```diff\n", preview.path);
    let mut n = 0;
    for row in preview.rows.iter() {
        match row {
            DiffRow::Hunk(h) => out.push_str(&format!("{h}\n")),
            DiffRow::Line(l) => {
                let sign = match l.kind {
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                    LineKind::Context => ' ',
                };
                out.push_str(&format!("{sign}{}\n", l.text));
            }
            DiffRow::More(k) => out.push_str(&format!("… {k} líneas más\n")),
        }
        n += 1;
        if n >= MAX_LINES {
            out.push_str("… (recortado)\n");
            break;
        }
    }
    out.push_str("```\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 111..222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,5 @@\n fn a() {}\n-fn b() {}\n+fn b() -> u8 { 1 }\n+fn c() {}\n fn d() {}\n \n@@ -20,2 +21,2 @@ mod x\n-// --- not a header\n+// +++ not a header\n tail\ndiff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n\\ No newline at end of file\n";

    #[test]
    fn parses_files_hunks_and_numbers() {
        let files = parse_unified(SAMPLE);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].hunks.len(), 2);
        let h = &files[0].hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 4, 1, 5));
        assert_eq!(h.lines.len(), 6);
        assert_eq!(h.lines[1].kind, LineKind::Removed);
        assert_eq!((h.lines[1].old_line, h.lines[1].new_line), (Some(2), None));
        assert_eq!((h.lines[2].old_line, h.lines[2].new_line), (None, Some(2)));
        assert_eq!(h.lines[5].text, "", "a bare context line");
        // Content that looks like file headers stays content inside a hunk.
        let h2 = &files[0].hunks[1];
        assert_eq!(h2.lines[0].text, "// --- not a header");
        assert_eq!(h2.lines.len(), 3);
        assert!(files[1].binary && files[1].hunks.is_empty());
        assert_eq!(files[2].path, "new.txt");
        assert_eq!(files[2].hunks[0].lines.len(), 1);
    }

    #[test]
    fn rows_count_changes_and_cap() {
        let files = parse_unified(SAMPLE);
        let p = build_preview("src/lib.rs", &files[0].hunks, false, None);
        assert_eq!((p.added, p.removed), (3, 2));
        assert_eq!(p.first_line, Some(2));
        assert!(matches!(p.rows[0], DiffRow::Hunk(_)));
        assert!(!p.highlighted);

        let big = Hunk {
            old_start: 1,
            old_len: MAX_ROWS + 10,
            new_start: 1,
            new_len: 0,
            lines: (0..MAX_ROWS + 10)
                .map(|i| viewer_svc::DiffLine {
                    kind: LineKind::Removed,
                    old_line: Some(i + 1),
                    new_line: None,
                    text: "x".into(),
                    inline: Vec::new(),
                })
                .collect(),
        };
        let p = build_preview("big", &[big], false, None);
        assert_eq!(p.rows.len(), MAX_ROWS + 1);
        assert_eq!(p.rows.last(), Some(&DiffRow::More(11)));
        assert_eq!(p.removed, MAX_ROWS + 10, "totals count every line, not only the loaded rows");
    }

    #[test]
    fn tabs_expand_and_spans_follow() {
        let (t, s) = expand_tabs("\tfn x", vec![(1..3, HighlightKind::Keyword)]);
        assert_eq!(t, "    fn x");
        assert_eq!(s, vec![(4..6, HighlightKind::Keyword)]);
        let (t, s) = expand_tabs("no tabs", vec![(0..2, HighlightKind::Plain)]);
        assert_eq!((t.as_str(), s.len()), ("no tabs", 1));
    }

    #[test]
    fn sanitize_drops_invalid_spans() {
        let spans = vec![
            (0..2, HighlightKind::Keyword),
            (1..3, HighlightKind::String), // overlaps
            (4..99, HighlightKind::Number), // out of range
            (3..4, HighlightKind::Comment),
        ];
        let out = sanitize("abcdef", spans);
        assert_eq!(out, vec![(0..2, HighlightKind::Keyword), (3..4, HighlightKind::Comment)]);
        // Inside a multibyte char.
        assert!(sanitize("ñ", vec![(1..2, HighlightKind::Keyword)]).is_empty());
    }

    #[test]
    fn highlights_apply_only_to_added_and_context_lines() {
        let files = parse_unified(SAMPLE);
        let mut map = HashMap::new();
        map.insert(1, vec![(0..2, HighlightKind::Keyword)]);
        map.insert(2, vec![(0..2, HighlightKind::Keyword)]);
        let p = build_preview("src/lib.rs", &files[0].hunks, false, Some(&map));
        assert!(p.highlighted);
        let lines: Vec<&PreviewLine> =
            p.rows.iter().filter_map(|r| if let DiffRow::Line(l) = r { Some(l) } else { None }).collect();
        assert_eq!(lines[0].spans.len(), 1, "context line 1");
        assert!(lines[1].spans.is_empty(), "removed lines are plain");
        assert_eq!(lines[2].spans.len(), 1, "added line 2");
    }

    #[test]
    fn agent_markdown_has_a_fenced_diff() {
        let files = parse_unified(SAMPLE);
        let p = preview_of(&files[0], None);
        let md = agent_markdown("staged", &p);
        assert!(md.starts_with("## Revisión de cambios"));
        assert!(md.contains("`src/lib.rs` (staged)"));
        assert!(md.contains("-fn b() {}\n+fn b() -> u8 { 1 }"));
        assert!(md.trim_end().ends_with("```"));
    }
}
