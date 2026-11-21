//! Pure text helpers: middle truncation, path labels, relative time, plurals.

use std::time::{SystemTime, UNIX_EPOCH};

use git_svc::ChangeKind;

/// `abcdefghij` -> `abcd…ghij` keeping `max` chars in total (`max >= 3`).
pub fn truncate_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let max = max.max(3);
    let keep = max - 1;
    let tail = keep / 2;
    let head = keep - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// `abcdefghij` -> `abcdefg…` keeping `max` chars in total.
pub fn truncate_end(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let keep = max.max(2) - 1;
    let mut out: String = chars[..keep].iter().collect::<String>().trim_end().to_string();
    out.push('…');
    out
}

/// A path split for painting: the directory (muted) and the file name (strong).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathLabel {
    /// Directory part including the trailing `/`, possibly truncated in the middle. Empty for a
    /// root-level file.
    pub dir: String,
    pub name: String,
}

/// Middle-truncates `path` (forward slashes) to about `budget` chars, always keeping as much of
/// the file name as possible: `crates/ui_git/src/view.rs` -> `crates/…/src/view.rs`.
pub fn path_label(path: &str, budget: usize) -> PathLabel {
    let budget = budget.max(8);
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let total = path.chars().count();
    if total <= budget {
        return PathLabel { dir: dir.to_string(), name: name.to_string() };
    }
    let name_len = name.chars().count();
    if name_len + 3 >= budget {
        return PathLabel { dir: String::new(), name: truncate_middle(name, budget) };
    }
    let dir_budget = budget - name_len;
    PathLabel { dir: truncate_middle(dir, dir_budget), name: name.to_string() }
}

/// Status letter of a change (`M`, `A`, `D`, `R`, `C`, `T`).
pub fn change_letter(kind: ChangeKind) -> char {
    match kind {
        ChangeKind::Added => 'A',
        ChangeKind::Modified => 'M',
        ChangeKind::Deleted => 'D',
        ChangeKind::Renamed => 'R',
        ChangeKind::Copied => 'C',
        ChangeKind::TypeChanged => 'T',
    }
}

/// Seconds since the epoch (0 before it).
pub fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Short relative time: `ahora`, `5m`, `3h`, `2d`, `3sem`, `4mes`, `2a`.
pub fn relative_time(ts: i64, now: i64) -> String {
    let secs = now.saturating_sub(ts).max(0) as u64;
    match secs {
        0..=59 => "ahora".to_string(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        604_800..=2_591_999 => format!("{}sem", secs / 604_800),
        2_592_000..=31_535_999 => format!("{}mes", secs / 2_592_000),
        _ => format!("{}a", secs / 31_536_000),
    }
}

/// `1 archivo`, `3 archivos`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// Normalises a path to forward slashes (git's notation).
pub fn slashes(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// First non-empty line of `s`, trimmed.
pub fn first_line(s: &str) -> &str {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_in_the_middle() {
        assert_eq!(truncate_middle("short", 10), "short");
        assert_eq!(truncate_middle("abcdefghij", 7), "abc…hij");
        assert_eq!(truncate_middle("abcdefghij", 6).chars().count(), 6);
        assert_eq!(truncate_middle("ñandú-ñandú-ñandú", 8).chars().count(), 8);
    }

    #[test]
    fn truncates_at_the_end() {
        assert_eq!(truncate_end("short", 10), "short");
        assert_eq!(truncate_end("fix(parser): maneja entrada", 12), "fix(parser)…");
        assert_eq!(truncate_end("abcdefghij", 5).chars().count(), 5);
    }

    #[test]
    fn path_labels_keep_the_file_name() {
        let l = path_label("crates/ui_git/src/view.rs", 60);
        assert_eq!((l.dir.as_str(), l.name.as_str()), ("crates/ui_git/src/", "view.rs"));
        let l = path_label("crates/ui_git/src/deeply/nested/folder/view.rs", 28);
        assert_eq!(l.name, "view.rs");
        assert!(l.dir.contains('…'), "{l:?}");
        assert!(l.dir.chars().count() + l.name.chars().count() <= 28);
        let l = path_label("a-very-long-file-name-that-never-fits.rs", 16);
        assert!(l.dir.is_empty());
        assert!(l.name.contains('…'));
        assert_eq!(l.name.chars().count(), 16);
        let l = path_label("README.md", 20);
        assert_eq!((l.dir.as_str(), l.name.as_str()), ("", "README.md"));
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(100, 100), "ahora");
        assert_eq!(relative_time(0, 300), "5m");
        assert_eq!(relative_time(0, 3 * 3600), "3h");
        assert_eq!(relative_time(0, 2 * 86_400), "2d");
        assert_eq!(relative_time(0, 15 * 86_400), "2sem");
        assert_eq!(relative_time(0, 120 * 86_400), "4mes");
        assert_eq!(relative_time(0, 800 * 86_400), "2a");
        // A commit dated in the future never shows a negative time.
        assert_eq!(relative_time(500, 100), "ahora");
    }

    #[test]
    fn plurals_and_lines() {
        assert_eq!(plural(1, "archivo", "archivos"), "1 archivo");
        assert_eq!(plural(0, "archivo", "archivos"), "0 archivos");
        assert_eq!(first_line("\n  hola \nmundo"), "hola");
    }
}
