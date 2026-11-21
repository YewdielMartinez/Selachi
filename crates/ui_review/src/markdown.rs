//! The message that goes to the agent: Spanish Markdown, notes grouped by file, each with its
//! `path:line`, the note text and a +-3 line code excerpt. Everything that leaves the machine goes
//! through `term_core::redact` (A13): code excerpts and note text alike.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::note::Note;

/// The built message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewMessage {
    pub markdown: String,
    /// Secrets hidden in the excerpts and texts.
    pub redactions: usize,
}

/// Markdown language tag for a path (empty = no tag).
pub fn fence_language(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e).to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "py" => "python",
        "cs" => "csharp",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "md" | "markdown" => "markdown",
        "ps1" | "psm1" => "powershell",
        "sh" | "bash" => "bash",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "sql" => "sql",
        "xml" | "csproj" | "sln" => "xml",
        _ => "",
    }
}

/// A fence longer than any backtick run in `body`.
fn fence_for(body: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// The excerpt of `note` as a fenced block. Marks the anchored lines with `>`.
fn excerpt_block(note: &Note) -> (String, usize) {
    if note.snippet.is_empty() {
        return (String::new(), 0);
    }
    let raw: String = note.snippet.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
    let (clean, hits) = term_core::redact(&raw);
    let clean_lines: Vec<&str> = clean.split('\n').collect();
    let numbered = clean_lines.len() == note.snippet.len();
    let last = note.snippet.last().map_or(1, |(n, _)| *n);
    let width = last.to_string().len();
    let mut body = String::new();
    if numbered {
        for ((n, _), text) in note.snippet.iter().zip(&clean_lines) {
            let mark = if (note.start..=note.end).contains(n) { '>' } else { ' ' };
            let _ = writeln!(body, "{mark} {n:>width$} | {text}");
        }
    } else {
        // Redaction collapsed lines (a private key block): numbers would lie, so leave them out.
        for text in &clean_lines {
            let _ = writeln!(body, "{text}");
        }
    }
    let fence = fence_for(&body);
    let lang = fence_language(&note.path);
    (format!("{fence}{lang}\n{body}{fence}\n"), hits)
}

/// Builds the review message for `notes` (typically the drafts). `label` says what was reviewed
/// ("los cambios del worktree"). Notes are grouped by file (path order) and sorted by line.
pub fn build_markdown(label: &str, notes: &[&Note]) -> ReviewMessage {
    let mut out = String::new();
    let mut redactions = 0usize;
    let n = notes.len();
    let _ = writeln!(
        out,
        "He revisado {label} y tengo {n} {}. Atiéndelas una por una y dime qué cambiaste en cada caso.\n",
        if n == 1 { "nota" } else { "notas" }
    );

    let mut by_path: BTreeMap<&str, Vec<&Note>> = BTreeMap::new();
    for note in notes {
        by_path.entry(note.path.as_str()).or_default().push(note);
    }
    for (path, mut group) in by_path {
        group.sort_by_key(|n| (n.start, n.end, n.id));
        let _ = writeln!(out, "## `{path}`\n");
        for note in group {
            let _ = writeln!(out, "### `{}`\n", note.location());
            let (text, hits) = term_core::redact(&note.text);
            redactions += hits;
            for line in text.lines() {
                let _ = writeln!(out, "> {line}");
            }
            out.push('\n');
            if note.stale {
                let _ = writeln!(out, "_Las líneas pudieron cambiar desde que escribí la nota; busca el fragmento._\n");
            }
            let (block, hits) = excerpt_block(note);
            redactions += hits;
            if !block.is_empty() {
                out.push_str(&block);
                out.push('\n');
            }
        }
    }
    if redactions > 0 {
        let _ = writeln!(out, "_Se ocultaron {redactions} secreto(s) antes de enviar._");
    }
    ReviewMessage { markdown: out.trim_end().to_string() + "\n", redactions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{NoteId, Side, SideLines};

    fn file(n: usize) -> SideLines {
        (1..=n).map(|i| (i, format!("linea {i}"))).collect()
    }

    fn note(id: u64, path: &str, start: usize, end: usize, text: &str) -> Note {
        Note::new(NoteId(id), path, Side::New, start, end, text, &file(30), 0)
    }

    #[test]
    fn groups_by_file_and_sorts_by_line() {
        let a = note(1, "src/b.rs", 20, 20, "segunda");
        let b = note(2, "src/a.rs", 9, 9, "primera");
        let c = note(3, "src/b.rs", 3, 3, "tercera");
        let m = build_markdown("los cambios del worktree", &[&a, &b, &c]);
        let md = &m.markdown;
        assert!(md.starts_with("He revisado los cambios del worktree y tengo 3 notas."), "{md}");
        let (ia, ib) = (md.find("## `src/a.rs`").unwrap(), md.find("## `src/b.rs`").unwrap());
        assert!(ia < ib, "files in path order");
        assert_eq!(md.matches("\n## `src/").count(), 2, "one header per file");
        let (i3, i20) = (md.find("### `src/b.rs:3`").unwrap(), md.find("### `src/b.rs:20`").unwrap());
        assert!(i3 < i20, "notes in line order");
        assert!(md.contains("> primera") && md.contains("> segunda") && md.contains("> tercera"));
        assert_eq!(m.redactions, 0);
    }

    #[test]
    fn excerpt_is_plus_minus_three_and_marks_the_range() {
        let n = note(1, "src/lib.rs", 10, 11, "extrae esto");
        let md = build_markdown("x", &[&n]).markdown;
        assert!(md.contains("```rust\n"), "{md}");
        for l in 7..=14 {
            assert!(md.contains(&format!("linea {l}\n")), "line {l} missing in {md}");
        }
        assert!(!md.contains("linea 6\n") && !md.contains("linea 15\n"));
        assert!(md.contains("> 10 | linea 10"));
        assert!(md.contains("> 11 | linea 11"));
        assert!(md.contains("   9 | linea 9") || md.contains("  9 | linea 9"), "{md}");
        assert!(md.contains("### `src/lib.rs:10-11`"));
    }

    #[test]
    fn excerpt_at_the_top_of_the_file_is_clipped() {
        let n = note(1, "a.txt", 1, 1, "x");
        let md = build_markdown("x", &[&n]).markdown;
        assert!(md.contains("> 1 | linea 1"));
        assert!(md.contains("4 | linea 4"));
        assert!(!md.contains("5 | linea 5"));
        assert!(md.contains("```\n"), "no language tag for .txt: {md}");
    }

    #[test]
    fn secrets_are_redacted_in_excerpts_and_texts() {
        let mut lines = file(10);
        lines.insert(5, "let key = \"sk-abcdefghijklmnopqrstuvwxyz0123456789\";".to_string());
        let n = Note::new(NoteId(1), "src/cfg.rs", Side::New, 5, 5, "no subas ghp_abcdefghijklmnopqrstuvwxyz0123456789 al repo", &lines, 0);
        let m = build_markdown("x", &[&n]);
        assert!(!m.markdown.contains("sk-abcdef"), "{}", m.markdown);
        assert!(!m.markdown.contains("ghp_abcdef"), "{}", m.markdown);
        assert!(m.markdown.contains("[REDACTED]"));
        assert!(m.redactions >= 2, "{}", m.redactions);
        assert!(m.markdown.contains("Se ocultaron"));
    }

    #[test]
    fn multi_line_secret_blocks_do_not_break_the_excerpt() {
        let mut lines = file(3);
        lines.insert(2, "-----BEGIN RSA PRIVATE KEY-----".to_string());
        lines.insert(3, "MIIEpAIBAAKCAQEA".to_string());
        lines.insert(4, "-----END RSA PRIVATE KEY-----".to_string());
        let n = Note::new(NoteId(1), "k.pem", Side::New, 3, 3, "esto es una clave", &lines, 0);
        let m = build_markdown("x", &[&n]);
        assert!(!m.markdown.contains("MIIEpAIBAAKCAQEA"));
        assert!(m.redactions >= 1);
    }

    #[test]
    fn fence_grows_when_the_code_has_backticks() {
        let mut lines = file(5);
        lines.insert(3, "let s = \"```\";".to_string());
        let n = Note::new(NoteId(1), "a.md", Side::New, 3, 3, "x", &lines, 0);
        let md = build_markdown("x", &[&n]).markdown;
        assert!(md.contains("````markdown\n"), "{md}");
    }

    #[test]
    fn singular_and_stale_wording() {
        let mut n = note(1, "a.rs", 2, 2, "una sola");
        n.stale = true;
        let md = build_markdown("el diff", &[&n]).markdown;
        assert!(md.contains("tengo 1 nota."), "{md}");
        assert!(md.contains("pudieron cambiar"));
    }

    #[test]
    fn old_side_notes_say_so() {
        let lines: SideLines = (1..=5).map(|i| (i, format!("vieja {i}"))).collect();
        let n = Note::new(NoteId(1), "a.rs", Side::Old, 2, 2, "por qué se borra", &lines, 0);
        let md = build_markdown("x", &[&n]).markdown;
        assert!(md.contains("a.rs:2 (versión anterior)"), "{md}");
    }

    #[test]
    fn multi_line_note_text_is_quoted() {
        let n = note(1, "a.rs", 2, 2, "línea uno\nlínea dos");
        let md = build_markdown("x", &[&n]).markdown;
        assert!(md.contains("> línea uno\n> línea dos\n"), "{md}");
    }

    #[test]
    fn language_tags() {
        assert_eq!(fence_language("a/b.rs"), "rust");
        assert_eq!(fence_language("a/b.CS"), "csharp");
        assert_eq!(fence_language("Makefile"), "");
    }
}
