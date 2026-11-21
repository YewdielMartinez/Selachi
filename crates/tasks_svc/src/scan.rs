//! Scan context shared by the detectors: records every file or folder consulted
//! (so the cache knows what to stat) and builds [`TaskDef`]s.

use crate::model::{DiscoverOptions, TaskDef, TaskKind};
use std::path::{Path, PathBuf};

/// Directories never entered while looking for sub-projects.
pub(crate) const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "bin",
    "obj",
    "dist",
    "build",
    "out",
    "venv",
    ".venv",
    "__pycache__",
    "vendor",
];

pub(crate) struct Scan<'a> {
    pub root: &'a Path,
    pub opts: &'a DiscoverOptions,
    pub tasks: Vec<TaskDef>,
    /// Everything looked at, present or not (a missing file appearing later must
    /// invalidate the cache too).
    pub deps: Vec<PathBuf>,
}

impl<'a> Scan<'a> {
    pub fn new(root: &'a Path, opts: &'a DiscoverOptions) -> Self {
        Self {
            root,
            opts,
            tasks: Vec::new(),
            deps: Vec::new(),
        }
    }

    /// Read a text file (recorded as a dependency even when missing).
    pub fn read(&mut self, path: &Path) -> Option<String> {
        self.deps.push(path.to_path_buf());
        let bytes = std::fs::read(path).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        Some(text.trim_start_matches('\u{feff}').to_string())
    }

    pub fn exists(&mut self, path: &Path) -> bool {
        self.deps.push(path.to_path_buf());
        path.exists()
    }

    /// `(name, is_dir)` of the entries of a folder, sorted by name; empty when it
    /// cannot be read. The folder is recorded as a dependency.
    pub fn list_dir(&mut self, dir: &Path) -> Vec<(String, bool)> {
        self.deps.push(dir.to_path_buf());
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<(String, bool)> = rd
            .flatten()
            .map(|e| {
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                (e.file_name().to_string_lossy().into_owned(), is_dir)
            })
            .collect();
        out.sort();
        out
    }

    /// Path relative to the project root, with `/` separators.
    pub fn rel(&self, path: &Path) -> String {
        let p = path.strip_prefix(self.root).unwrap_or(path);
        p.to_string_lossy().replace('\\', "/")
    }

    pub fn push(
        &mut self,
        kind: TaskKind,
        label: impl Into<String>,
        command: impl Into<String>,
        cwd: &Path,
        source: &Path,
        description: Option<String>,
    ) {
        let label = label.into();
        let id = format!("{}:{}:{}", kind.as_str(), self.rel(source), label);
        self.tasks.push(TaskDef {
            id,
            label,
            kind,
            command: command.into(),
            cwd: cwd.to_path_buf(),
            source_file: source.to_path_buf(),
            description: description.filter(|d| !d.trim().is_empty()),
        });
    }

    /// Make ids unique (same label twice in one source) and dedupe dependencies.
    pub fn finish(mut self) -> (Vec<TaskDef>, Vec<PathBuf>) {
        let mut seen = std::collections::HashMap::<String, u32>::new();
        for t in &mut self.tasks {
            let n = seen.entry(t.id.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                t.id = format!("{}#{}", t.id, n);
            }
        }
        self.deps.sort();
        self.deps.dedup();
        (self.tasks, self.deps)
    }
}

/// Wrap an argument for PowerShell when it is not a plain word.
pub(crate) fn quote_arg(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '\\' | ':' | '+' | '=' | '-' | '@')
        })
        && !s.starts_with('@');
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{}…", cut.trim_end())
    }
}

/// Simple glob for one path segment: `*` anywhere (one wildcard is enough for
/// workspace patterns like `pkg-*`).
pub(crate) fn segment_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((pre, post)) => {
            name.len() >= pre.len() + post.len() && name.starts_with(pre) && name.ends_with(post)
        }
    }
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

/// Expand workspace-style patterns (`packages/*`, `apps/*/web`, `tools`) into
/// folders. `**` counts as a single `*` (one level deep). At most `max` folders.
pub(crate) fn expand_dirs(s: &mut Scan<'_>, patterns: &[String], max: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for pat in patterns {
        if pat.starts_with('!') {
            continue;
        }
        let norm = pat.replace('\\', "/");
        let segs: Vec<&str> = norm
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        if segs.is_empty() {
            continue;
        }
        let root = s.root.to_path_buf();
        expand_rec(s, &root, &segs, &mut out, max);
        if out.len() >= max {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn expand_rec(s: &mut Scan<'_>, base: &Path, segs: &[&str], out: &mut Vec<PathBuf>, max: usize) {
    if out.len() >= max {
        return;
    }
    let Some((seg, rest)) = segs.split_first() else {
        out.push(base.to_path_buf());
        return;
    };
    if seg.contains('*') {
        let seg = if *seg == "**" { "*" } else { seg };
        for (name, is_dir) in s.list_dir(base) {
            if is_dir && !skip_dir(&name) && segment_matches(seg, &name) {
                expand_rec(s, &base.join(&name), rest, out, max);
            }
        }
    } else {
        expand_rec(s, &base.join(seg), rest, out, max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_arg("src/Api/Api.csproj"), "src/Api/Api.csproj");
        assert_eq!(quote_arg("my proj/x.csproj"), "'my proj/x.csproj'");
        assert_eq!(quote_arg("it's"), "'it''s'");
        assert_eq!(quote_arg("@x"), "'@x'");
        assert_eq!(quote_arg(""), "''");
    }

    #[test]
    fn segment_globs() {
        assert!(segment_matches("*", "anything"));
        assert!(segment_matches("pkg-*", "pkg-a"));
        assert!(!segment_matches("pkg-*", "other"));
        assert!(segment_matches("*-web", "x-web"));
        assert!(segment_matches("exact", "exact"));
        assert!(!segment_matches("exact", "exactly"));
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefgh", 3), "abc…");
    }
}
