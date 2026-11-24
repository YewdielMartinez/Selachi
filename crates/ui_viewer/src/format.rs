//! Small text formatting helpers shared by the views.

use std::path::{Path, PathBuf};

/// `1.4 MB`, `812 B`, `3.2 KB`.
pub fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f < KB {
        format!("{n} B")
    } else if f < KB * KB {
        format!("{:.1} KB", f / KB)
    } else if f < KB * KB * KB {
        format!("{:.1} MB", f / (KB * KB))
    } else {
        format!("{:.2} GB", f / (KB * KB * KB))
    }
}

/// `1 234`-style thousands grouping for token counts (`12 345 678`), `1.2 M` above a million.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1} M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1} k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// `path` relative to `root` with forward slashes, or the path itself when it is outside.
pub fn display_path(root: Option<&Path>, path: &Path) -> String {
    let shown = match root.and_then(|r| path.strip_prefix(r).ok()) {
        Some(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
        _ => path.to_path_buf(),
    };
    shown.to_string_lossy().replace('\\', "/")
}

/// Breadcrumb segments of a display path (`crates/ui_viewer/src/lib.rs` -> 4 segments).
pub fn breadcrumb(display: &str) -> Vec<String> {
    display
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Walks up from `start` looking for a `.git` entry; the directory that holds it.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn tokens() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(12_345), "12.3 k");
        assert_eq!(format_tokens(2_500_000), "2.5 M");
    }

    #[test]
    fn display_and_breadcrumb() {
        let root = Path::new("C:/proj");
        assert_eq!(
            display_path(Some(root), Path::new("C:/proj/src/a.rs")),
            "src/a.rs"
        );
        assert_eq!(
            display_path(Some(root), Path::new("D:/x/a.rs")),
            "D:/x/a.rs"
        );
        assert_eq!(display_path(None, Path::new("a.rs")), "a.rs");
        assert_eq!(breadcrumb("src/a/b.rs"), ["src", "a", "b.rs"]);
    }
}
