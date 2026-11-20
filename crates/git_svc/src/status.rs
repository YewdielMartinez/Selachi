//! Typed model and parser for `git status --porcelain=v2 -z --branch`.

use std::path::PathBuf;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

impl ChangeKind {
    fn from_code(c: char) -> Option<Self> {
        match c {
            '.' | ' ' => None,
            'A' => Some(Self::Added),
            'D' => Some(Self::Deleted),
            'R' => Some(Self::Renamed),
            'C' => Some(Self::Copied),
            'T' => Some(Self::TypeChanged),
            // 'M' and anything unforeseen: show as modified rather than hiding it.
            _ => Some(Self::Modified),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    /// Original path for renames / copies.
    pub orig_path: Option<PathBuf>,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStatus {
    /// Branch name; `None` when HEAD is detached.
    pub branch: Option<String>,
    pub detached: bool,
    /// Full HEAD object id, `None` on an unborn branch.
    pub head_oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Changes between HEAD and the index.
    pub staged: Vec<FileChange>,
    /// Changes between the index and the working tree.
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<PathBuf>,
    pub conflicted: Vec<PathBuf>,
}

impl RepoStatus {
    /// True when there is nothing staged, modified or conflicted (untracked files are ignored).
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.conflicted.is_empty()
    }

    /// True when a checkout/switch could clobber work: staged, unstaged or conflicted entries.
    pub fn is_dirty(&self) -> bool {
        !self.is_clean()
    }
}

/// Parse the NUL-separated output of `git status --porcelain=v2 -z --branch`.
pub fn parse_status(bytes: &[u8]) -> Result<RepoStatus> {
    let text = String::from_utf8_lossy(bytes);
    let mut st = RepoStatus::default();
    let mut records = text.split('\0');

    while let Some(rec) = records.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            parse_header(h, &mut st);
        } else if let Some(rest) = rec.strip_prefix("1 ") {
            // <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
            let mut f = rest.splitn(8, ' ');
            let xy = f.next().unwrap_or("..");
            let path = f.nth(6).unwrap_or("");
            if path.is_empty() {
                continue;
            }
            push_changes(&mut st, xy, path, None);
        } else if let Some(rest) = rec.strip_prefix("2 ") {
            // <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path> \0 <origPath>
            let mut f = rest.splitn(9, ' ');
            let xy = f.next().unwrap_or("..");
            let path = f.nth(7).unwrap_or("");
            let orig = records.next().unwrap_or("");
            if path.is_empty() {
                continue;
            }
            push_changes(&mut st, xy, path, (!orig.is_empty()).then_some(orig));
        } else if let Some(rest) = rec.strip_prefix("u ") {
            // <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
            let path = rest.splitn(10, ' ').nth(9).unwrap_or("");
            if !path.is_empty() {
                st.conflicted.push(PathBuf::from(path));
            }
        } else if let Some(p) = rec.strip_prefix("? ") {
            st.untracked.push(PathBuf::from(p));
        }
        // "! " (ignored) and unknown records are skipped.
    }
    Ok(st)
}

fn parse_header(h: &str, st: &mut RepoStatus) {
    if let Some(oid) = h.strip_prefix("branch.oid ") {
        st.head_oid = (oid != "(initial)").then(|| oid.to_owned());
    } else if let Some(head) = h.strip_prefix("branch.head ") {
        if head == "(detached)" {
            st.detached = true;
            st.branch = None;
        } else {
            st.detached = false;
            st.branch = Some(head.to_owned());
        }
    } else if let Some(up) = h.strip_prefix("branch.upstream ") {
        st.upstream = Some(up.to_owned());
    } else if let Some(ab) = h.strip_prefix("branch.ab ") {
        for tok in ab.split_whitespace() {
            if let Some(n) = tok.strip_prefix('+') {
                st.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = tok.strip_prefix('-') {
                st.behind = n.parse().unwrap_or(0);
            }
        }
    }
}

fn push_changes(st: &mut RepoStatus, xy: &str, path: &str, orig: Option<&str>) {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');
    for (code, list) in [(x, &mut st.staged), (y, &mut st.unstaged)] {
        if let Some(kind) = ChangeKind::from_code(code) {
            let orig_path = match kind {
                ChangeKind::Renamed | ChangeKind::Copied => orig.map(PathBuf::from),
                _ => None,
            };
            list.push(FileChange {
                path: PathBuf::from(path),
                orig_path,
                kind,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn branch_with_upstream_and_ab() {
        let s = "# branch.oid abc123\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +3 -2\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert!(!st.detached);
        assert_eq!(st.head_oid.as_deref(), Some("abc123"));
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!((st.ahead, st.behind), (3, 2));
        assert!(st.is_clean());
    }

    #[test]
    fn detached_head_no_upstream() {
        let s = "# branch.oid deadbeef\0# branch.head (detached)\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert!(st.detached);
        assert_eq!(st.branch, None);
        assert_eq!(st.upstream, None);
        assert_eq!((st.ahead, st.behind), (0, 0));
    }

    #[test]
    fn unborn_branch() {
        let s = "# branch.oid (initial)\0# branch.head main\0? a.txt\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.head_oid, None);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.untracked, vec![p("a.txt")]);
    }

    #[test]
    fn ordinary_entries_staged_and_unstaged() {
        let h = "1 {XY} N... 100644 100644 100644 aaaa bbbb";
        let s = format!(
            "# branch.head main\0{} src/a.rs\0{} src/b.rs\0{} src/c.rs\0{} src/d.rs\0",
            h.replace("{XY}", "M."),
            h.replace("{XY}", ".M"),
            h.replace("{XY}", "AM"),
            h.replace("{XY}", "D."),
        );
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(
            st.staged
                .iter()
                .map(|c| (c.path.clone(), c.kind))
                .collect::<Vec<_>>(),
            vec![
                (p("src/a.rs"), ChangeKind::Modified),
                (p("src/c.rs"), ChangeKind::Added),
                (p("src/d.rs"), ChangeKind::Deleted),
            ]
        );
        assert_eq!(
            st.unstaged
                .iter()
                .map(|c| (c.path.clone(), c.kind))
                .collect::<Vec<_>>(),
            vec![
                (p("src/b.rs"), ChangeKind::Modified),
                (p("src/c.rs"), ChangeKind::Modified),
            ]
        );
        assert!(st.is_dirty());
    }

    #[test]
    fn rename_consumes_orig_path_record() {
        let s = "2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.txt\0old name.txt\0? after.txt\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.staged.len(), 1);
        assert_eq!(st.staged[0].kind, ChangeKind::Renamed);
        assert_eq!(st.staged[0].path, p("new name.txt"));
        assert_eq!(st.staged[0].orig_path, Some(p("old name.txt")));
        assert!(st.unstaged.is_empty());
        // The record after the orig path must still be parsed.
        assert_eq!(st.untracked, vec![p("after.txt")]);
    }

    #[test]
    fn rename_with_worktree_edit_keeps_orig_only_on_staged_side() {
        let s = "2 RM N... 100644 100644 100644 aaaa bbbb R090 b.txt\0a.txt\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.staged[0].orig_path, Some(p("a.txt")));
        assert_eq!(st.unstaged[0].kind, ChangeKind::Modified);
        assert_eq!(st.unstaged[0].orig_path, None);
    }

    #[test]
    fn copied_entry() {
        let s = "2 C. N... 100644 100644 100644 aaaa bbbb C75 copy.txt\0orig.txt\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.staged[0].kind, ChangeKind::Copied);
        assert_eq!(st.staged[0].orig_path, Some(p("orig.txt")));
    }

    #[test]
    fn unmerged_entries() {
        let s = "u UU N... 100644 100644 100644 100644 a1 b2 c3 conflict file.txt\0u AA N... 100644 100644 100644 100644 a1 b2 c3 x.txt\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.conflicted, vec![p("conflict file.txt"), p("x.txt")]);
        assert!(st.staged.is_empty() && st.unstaged.is_empty());
        assert!(st.is_dirty());
    }

    #[test]
    fn untracked_ignored_unicode_and_spaces() {
        let s = "? dir with space/\0? ñandú/日本語.txt\0! target/\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(
            st.untracked,
            vec![p("dir with space/"), p("ñandú/日本語.txt")]
        );
    }

    #[test]
    fn path_with_spaces_in_ordinary_entry() {
        let s = "1 .M N... 100644 100644 100644 aaaa bbbb my dir/my file.rs\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.unstaged[0].path, p("my dir/my file.rs"));
    }

    #[test]
    fn type_change_and_empty_input() {
        let s = "1 T. N... 100644 120000 120000 aaaa bbbb link\0";
        let st = parse_status(s.as_bytes()).unwrap();
        assert_eq!(st.staged[0].kind, ChangeKind::TypeChanged);
        assert_eq!(parse_status(b"").unwrap(), RepoStatus::default());
    }
}
