//! Small pure helpers: durations, money, tokens, paths, branch lookup.

use std::path::{Path, PathBuf};

/// `850 ms`, `3.2 s`, `2 min 5 s`, `1 h 4 min`.
pub fn format_duration_ms(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1_000 {
        return format!("{ms} ms");
    }
    let s = ms as f64 / 1_000.0;
    if s < 10.0 {
        return format!("{s:.1} s");
    }
    let total = ms / 1_000;
    match total {
        0..=59 => format!("{total} s"),
        60..=3_599 => {
            let (m, s) = (total / 60, total % 60);
            if s == 0 { format!("{m} min") } else { format!("{m} min {s} s") }
        }
        _ => {
            let (h, m) = (total / 3_600, (total % 3_600) / 60);
            if m == 0 { format!("{h} h") } else { format!("{h} h {m} min") }
        }
    }
}

/// `$1.23`, `$0.12`, `<$0.01`.
pub fn format_cost(usd: f64) -> String {
    if usd <= 0.0 {
        "$0.00".to_string()
    } else if usd < 0.01 {
        "<$0.01".to_string()
    } else {
        format!("${usd:.2}")
    }
}

/// `842`, `12.3k`, `1.2M`.
pub fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
    }
}

/// Context window label: `1M`, `200k`.
pub fn format_window(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => {
            let m = tokens as f64 / 1_000_000.0;
            if (m - m.round()).abs() < 0.05 { format!("{}M", m.round() as u64) } else { format!("{m:.1}M") }
        }
    }
}

/// Last path component (`""` for an empty path). Handles both slash styles.
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Last `n` components of `path`, `…\a\b` when something was cut. Forward slashes are kept as is.
pub fn tail_path(path: &Path, n: usize) -> String {
    let comps: Vec<String> = path.components().filter_map(|c| c.as_os_str().to_str().map(str::to_string)).collect();
    let comps: Vec<&String> = comps.iter().filter(|c| !matches!(c.as_str(), "\\" | "/")).collect();
    if comps.len() <= n {
        return path.display().to_string();
    }
    let sep = if path.to_string_lossy().contains('\\') { "\\" } else { "/" };
    format!("…{sep}{}", comps[comps.len() - n..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join(sep))
}

/// Folder name of a project root (`"Terminal"`), falling back to the whole path.
pub fn project_name(root: &Path) -> String {
    root.file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_else(|| root.display().to_string())
}

/// Current branch of the repository (or worktree) containing `dir`, read straight from `.git`
/// (no git process). A detached HEAD gives the short hash.
pub fn read_branch(dir: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(dir);
    while let Some(d) = cur {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return head_of(&dot_git);
        }
        if dot_git.is_file() {
            // A linked worktree: `gitdir: <repo>/.git/worktrees/<name>`.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(target).is_absolute() { PathBuf::from(target) } else { d.join(target) };
            return head_of(&gitdir);
        }
        cur = d.parent();
    }
    None
}

fn head_of(gitdir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(r) => Some(r.trim().trim_start_matches("refs/heads/").to_string()),
        None => Some(head.chars().take(7).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(format_duration_ms(-5), "0 ms");
        assert_eq!(format_duration_ms(850), "850 ms");
        assert_eq!(format_duration_ms(3_240), "3.2 s");
        assert_eq!(format_duration_ms(42_000), "42 s");
        assert_eq!(format_duration_ms(125_000), "2 min 5 s");
        assert_eq!(format_duration_ms(120_000), "2 min");
        assert_eq!(format_duration_ms(3_840_000), "1 h 4 min");
    }

    #[test]
    fn money_and_tokens() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(0.004), "<$0.01");
        assert_eq!(format_cost(0.123), "$0.12");
        assert_eq!(format_cost(12.0), "$12.00");
        assert_eq!(format_tokens(842), "842");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(2_000), "2k");
        assert_eq!(format_tokens(1_250_000), "1.2M");
        assert_eq!(format_window(1_000_000), "1M");
        assert_eq!(format_window(200_000), "200k");
    }

    #[test]
    fn paths() {
        assert_eq!(file_name("src/a/b.rs"), "b.rs");
        assert_eq!(file_name("C:\\x\\y.txt"), "y.txt");
        assert_eq!(file_name("dir/"), "dir");
        assert_eq!(project_name(Path::new("C:\\Users\\v\\Terminal")), "Terminal");
        assert_eq!(tail_path(Path::new("a/b"), 3), "a/b");
        assert_eq!(tail_path(Path::new("/a/b/c/d"), 2), "…/c/d");
    }

    #[test]
    fn branch_from_head_file() {
        let dir = std::env::temp_dir().join(format!("ui_chat_branch_{}", std::process::id()));
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feat/chat\n").unwrap();
        let sub = dir.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(read_branch(&sub).as_deref(), Some("feat/chat"));
        std::fs::write(git.join("HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(read_branch(&dir).as_deref(), Some("0123456"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
