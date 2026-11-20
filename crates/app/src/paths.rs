//! Display helpers for working directories: `~` for home, middle truncation, tab labels.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// Longest breadcrumb path (in characters) before the middle is elided.
pub const BREADCRUMB_MAX_CHARS: usize = 44;

fn home_dir() -> Option<&'static Path> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from))
        .as_deref()
}

/// Strips the `\\?\` verbatim prefix that some APIs add.
fn plain(path: &Path) -> String {
    let s = path.display().to_string();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Components of `path` as comparable strings (case-insensitive on Windows).
fn keys(path: &Path) -> Vec<String> {
    plain(path)
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .map(|c| if cfg!(windows) { c.to_lowercase() } else { c.to_string() })
        .collect()
}

/// True if `path` is `root` or lies below it (separator- and, on Windows, case-insensitive).
pub fn is_within(path: &Path, root: &Path) -> bool {
    let (p, r) = (keys(path), keys(root));
    p.len() >= r.len() && p[..r.len()] == r[..]
}

/// `C:\Users\me\src` -> `~\src` when `home` is `C:\Users\me`; otherwise the plain path.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        let (p, h) = (keys(path), keys(home));
        if !h.is_empty() && p.len() >= h.len() && p[..h.len()] == h[..] {
            let original = plain(path);
            let rest: Vec<&str> = original
                .split(['\\', '/'])
                .filter(|c| !c.is_empty())
                .skip(h.len())
                .collect();
            return if rest.is_empty() { "~".to_string() } else { format!("~\\{}", rest.join("\\")) };
        }
    }
    plain(path)
}

/// Elides the middle of a `\`-separated path so it fits in `max` characters:
/// `~\Desktop\Projects\selachi\crates\app` -> `~\…\crates\app`.
pub fn middle_truncate(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let segs: Vec<&str> = path.split('\\').collect();
    if segs.len() <= 2 {
        return path.to_string();
    }
    let first = segs[0];
    // Keep as many trailing segments as fit next to `first\…`.
    let mut used = first.chars().count() + 2; // "first\" + "…"
    let mut tail: Vec<&str> = Vec::new();
    for seg in segs[1..].iter().rev() {
        let cost = seg.chars().count() + 1;
        if used + cost > max && !tail.is_empty() {
            break;
        }
        used += cost;
        tail.push(seg);
    }
    tail.reverse();
    if tail.len() == segs.len() - 1 {
        return path.to_string();
    }
    format!("{first}\\…\\{}", tail.join("\\"))
}

/// Breadcrumb text for a working directory.
pub fn breadcrumb(path: &Path) -> String {
    middle_truncate(&tilde(path, home_dir()), BREADCRUMB_MAX_CHARS)
}

/// Tab label: the last path segment (`~` for home, the drive for a drive root).
pub fn tab_label(path: &Path) -> String {
    let shown = tilde(path, home_dir());
    if shown == "~" {
        return shown;
    }
    shown
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&shown)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn home_becomes_tilde() {
        let home = Path::new(r"C:\Users\venta");
        assert_eq!(tilde(Path::new(r"C:\Users\venta\Desktop\Terminal\crates"), Some(home)), r"~\Desktop\Terminal\crates");
        assert_eq!(tilde(Path::new(r"c:\users\VENTA"), Some(home)), "~");
        assert_eq!(tilde(Path::new(r"C:\Users\venta2\x"), Some(home)), r"C:\Users\venta2\x");
        assert_eq!(tilde(Path::new(r"D:\work"), Some(home)), r"D:\work");
        assert_eq!(tilde(Path::new(r"\\?\C:\Users\venta\x"), Some(home)), r"~\x");
    }

    #[test]
    fn containment() {
        assert!(is_within(Path::new("C:/a/b/c"), Path::new("C:/a/b")));
        assert!(is_within(Path::new("C:/a/b"), Path::new("C:/a/b")));
        assert!(!is_within(Path::new("C:/a/bc"), Path::new("C:/a/b")));
        assert!(!is_within(Path::new("C:/a"), Path::new("C:/a/b")));
    }

    #[test]
    fn truncates_in_the_middle() {
        let long = r"~\Desktop\Projects\selachi\crates\app\src";
        let t = middle_truncate(long, 24);
        assert!(t.starts_with(r"~\…\"), "{t}");
        assert!(t.ends_with(r"app\src"), "{t}");
        assert!(t.chars().count() <= 24, "{t}");
        assert_eq!(middle_truncate(r"~\a\b", 24), r"~\a\b");
        // Always keeps the last segment, even if it alone is too long.
        assert_eq!(middle_truncate(r"~\a\very-long-directory-name", 10), r"~\…\very-long-directory-name");
    }

    #[test]
    fn labels() {
        assert_eq!(tab_label(Path::new(r"C:\Windows\System32")), "System32");
        assert_eq!(tab_label(Path::new(r"C:\")), "C:");
        assert_eq!(tab_label(Path::new(r"C:\Windows\")), "Windows");
    }
}
