//! `@file` mentions: finding the token under the caret, walking a project for candidates and
//! ranking them. Pure (the walk is blocking: run it on the background executor).

use std::ops::Range;
use std::path::Path;

use ignore::WalkBuilder;

/// Files listed per project at most.
pub const MAX_FILES: usize = 30_000;
/// Deepest folder level walked.
pub const MAX_DEPTH: usize = 12;

/// The `@query` token the caret is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionQuery {
    /// Byte range of the whole token, `@` included.
    pub range: Range<usize>,
    pub query: String,
}

/// The mention being typed at `caret`: an `@` that starts a word, followed by non-space text up
/// to the caret.
pub fn mention_at(text: &str, caret: usize) -> Option<MentionQuery> {
    let caret = caret.min(text.len());
    if !text.is_char_boundary(caret) {
        return None;
    }
    let head = &text[..caret];
    let start = head.rfind(char::is_whitespace).map_or(0, |i| i + head[i..].chars().next().map_or(1, char::len_utf8));
    let token = &head[start..];
    let query = token.strip_prefix('@')?;
    Some(MentionQuery { range: start..caret, query: query.to_string() })
}

/// All files under `root` (respecting `.gitignore`, skipping hidden entries), as paths relative to
/// `root` with forward slashes, sorted. Blocking.
pub fn walk_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .max_depth(Some(MAX_DEPTH))
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|n| it.any(|h| h == n))
}

/// Score of `path` for `query` (higher is better), `None` when it does not match.
pub fn score(path: &str, query: &str) -> Option<i32> {
    let p = path.to_lowercase();
    let q = query.to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let base = p.rsplit('/').next().unwrap_or(&p);
    let s = if base == q {
        1000
    } else if base.starts_with(&q) {
        800 - (base.len() - q.len()).min(100) as i32
    } else if base.contains(&q) {
        600
    } else if p.contains(&q) {
        400
    } else if is_subsequence(&q, &p) {
        200
    } else {
        return None;
    };
    Some(s)
}

/// The best `limit` files for `query`. An empty query lists the shallowest files first.
pub fn filter_files(files: &[String], query: &str, limit: usize) -> Vec<String> {
    let mut scored: Vec<(i32, usize, &String)> = files
        .iter()
        .filter_map(|f| {
            let depth = f.matches('/').count();
            score(f, query).map(|s| (s, if query.is_empty() { depth * 1000 + f.len() } else { f.len() }, f))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
    scored.into_iter().take(limit).map(|(_, _, f)| f.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_mention_under_the_caret() {
        assert_eq!(mention_at("@", 1), Some(MentionQuery { range: 0..1, query: String::new() }));
        let t = "mira @src/ma";
        assert_eq!(mention_at(t, t.len()), Some(MentionQuery { range: 5..12, query: "src/ma".into() }));
        // not a word start
        assert_eq!(mention_at("a@b", 3), None);
        // a space ended the token
        assert_eq!(mention_at("@a b", 4), None);
        // caret in the middle of the token: the query stops at the caret
        assert_eq!(mention_at("@abcdef", 3), Some(MentionQuery { range: 0..3, query: "ab".into() }));
        // newlines also start a word
        assert_eq!(mention_at("hola\n@x", 7), Some(MentionQuery { range: 5..7, query: "x".into() }));
        assert_eq!(mention_at("sin arroba", 4), None);
        // multi-byte neighbours do not panic
        assert_eq!(mention_at("ñ @é", 6), Some(MentionQuery { range: 3..6, query: "é".into() }));
    }

    #[test]
    fn ranks_names_before_paths() {
        let files: Vec<String> =
            ["src/lib.rs", "src/main.rs", "docs/main-notes.md", "README.md", "crates/app/src/main.rs"].map(String::from).to_vec();
        let top = filter_files(&files, "main", 10);
        assert_eq!(top[0], "src/main.rs");
        assert!(top.contains(&"docs/main-notes.md".to_string()));
        assert!(!top.contains(&"README.md".to_string()));
        assert_eq!(filter_files(&files, "readme", 3), vec!["README.md".to_string()]);
        // subsequence fallback
        assert_eq!(filter_files(&files, "cap", 3), vec!["crates/app/src/main.rs".to_string()]);
        // empty query: shallow first
        assert_eq!(filter_files(&files, "", 2), vec!["README.md".to_string(), "src/lib.rs".to_string()]);
        assert!(filter_files(&files, "zzz", 5).is_empty());
    }

    #[test]
    fn walk_respects_gitignore_and_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("src")).unwrap();
        std::fs::create_dir_all(r.join("target")).unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join("src/a.rs"), "x").unwrap();
        std::fs::write(r.join("b.md"), "x").unwrap();
        std::fs::write(r.join("target/junk.o"), "x").unwrap();
        std::fs::write(r.join(".git/HEAD"), "x").unwrap();
        std::fs::write(r.join(".gitignore"), "target/\n").unwrap();
        let files = walk_files(r, 100);
        assert_eq!(files, vec!["b.md".to_string(), "src/a.rs".to_string()]);
        assert_eq!(walk_files(r, 1).len(), 1);
    }
}
