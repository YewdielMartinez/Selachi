//! The Changes list: `RepoStatus` + `git diff --numstat` grouped into the four sections.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git_svc::RepoStatus;

use crate::fmt::{change_letter, slashes};

/// A group of the Changes list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Conflicts,
    Staged,
    Changes,
    Untracked,
}

impl Section {
    pub const fn title(self) -> &'static str {
        match self {
            Section::Conflicts => "Conflictos",
            Section::Staged => "Staged",
            Section::Changes => "Cambios",
            Section::Untracked => "Sin seguimiento",
        }
    }
}

/// Added / deleted lines of a file (`None` = binary).
pub type LineCounts = Option<(u32, u32)>;

/// `+N -M` per path for the staged and the unstaged side.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub staged: HashMap<PathBuf, LineCounts>,
    pub unstaged: HashMap<PathBuf, LineCounts>,
}

/// One file row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub section: Section,
    pub path: PathBuf,
    /// Forward-slash path for display.
    pub text: String,
    /// `M`, `A`, `D`, `R`, `C`, `T`, `U` (untracked) or `!` (conflict).
    pub letter: char,
    pub orig: Option<PathBuf>,
    pub counts: LineCounts,
    /// `numstat` had an entry for this file (distinguishes "binary" from "unknown").
    pub counted: bool,
}

/// Non-empty sections in display order: conflicts (urgent) first, then Staged, Cambios and
/// Sin seguimiento.
pub fn build_sections(status: &RepoStatus, counts: &Counts) -> Vec<(Section, Vec<FileRow>)> {
    let tracked =
        |section: Section, list: &[git_svc::FileChange], map: &HashMap<PathBuf, LineCounts>| {
            list.iter()
                .map(|c| {
                    let found = map.get(&c.path);
                    FileRow {
                        section,
                        path: c.path.clone(),
                        text: slashes(&c.path),
                        letter: change_letter(c.kind),
                        orig: c.orig_path.clone(),
                        counts: found.copied().flatten(),
                        counted: found.is_some(),
                    }
                })
                .collect::<Vec<_>>()
        };
    let plain = |section: Section, list: &[PathBuf], letter: char| {
        list.iter()
            .map(|p| FileRow {
                section,
                path: p.clone(),
                text: slashes(p),
                letter,
                orig: None,
                counts: None,
                counted: false,
            })
            .collect::<Vec<_>>()
    };
    let groups = [
        (
            Section::Conflicts,
            plain(Section::Conflicts, &status.conflicted, '!'),
        ),
        (
            Section::Staged,
            tracked(Section::Staged, &status.staged, &counts.staged),
        ),
        (
            Section::Changes,
            tracked(Section::Changes, &status.unstaged, &counts.unstaged),
        ),
        (
            Section::Untracked,
            plain(Section::Untracked, &status.untracked, 'U'),
        ),
    ];
    groups
        .into_iter()
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// Parses `git diff --numstat -z` output (`added\tdeleted\tpath\0`; a rename is
/// `added\tdeleted\t\0old\0new\0`; binary files report `-`).
pub fn parse_numstat(bytes: &[u8]) -> HashMap<PathBuf, LineCounts> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = HashMap::new();
    let mut fields = text.split('\0');
    while let Some(head) = fields.next() {
        if head.is_empty() {
            continue;
        }
        let mut parts = head.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let counts = match (a.parse::<u32>(), d.parse::<u32>()) {
            (Ok(a), Ok(d)) => Some((a, d)),
            _ => None,
        };
        let path = if path.is_empty() {
            // Rename: the next two fields are the old and the new path.
            let _old = fields.next();
            match fields.next() {
                Some(new) => new,
                None => break,
            }
        } else {
            path
        };
        out.insert(PathBuf::from(path), counts);
    }
    out
}

/// Staged / unstaged / total letter counts for the header summary, e.g. `3 cambios`.
pub fn total_changes(status: &RepoStatus) -> usize {
    status.staged.len() + status.unstaged.len() + status.untracked.len() + status.conflicted.len()
}

/// Whether `path` (relative to the repo) is still listed in `section` of `status`.
pub fn contains(status: &RepoStatus, section: Section, path: &Path) -> bool {
    match section {
        Section::Conflicts => status.conflicted.iter().any(|p| p == path),
        Section::Staged => status.staged.iter().any(|c| c.path == path),
        Section::Changes => status.unstaged.iter().any(|c| c.path == path),
        Section::Untracked => status.untracked.iter().any(|p| p == path),
    }
}

#[cfg(test)]
mod tests {
    use git_svc::{ChangeKind, FileChange};

    use super::*;

    fn change(p: &str, kind: ChangeKind) -> FileChange {
        FileChange {
            path: p.into(),
            orig_path: None,
            kind,
        }
    }

    #[test]
    fn sections_are_ordered_and_empty_ones_dropped() {
        let st = RepoStatus {
            staged: vec![change("a.rs", ChangeKind::Added)],
            unstaged: vec![
                change("src\\b.rs", ChangeKind::Modified),
                change("c.rs", ChangeKind::Deleted),
            ],
            untracked: vec!["new.txt".into()],
            conflicted: vec!["x.rs".into()],
            ..RepoStatus::default()
        };
        let mut counts = Counts::default();
        counts.unstaged.insert("src\\b.rs".into(), Some((5, 2)));
        counts.unstaged.insert("c.rs".into(), None);
        let s = build_sections(&st, &counts);
        let order: Vec<Section> = s.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            order,
            [
                Section::Conflicts,
                Section::Staged,
                Section::Changes,
                Section::Untracked
            ]
        );
        let changes = &s[2].1;
        assert_eq!(changes[0].letter, 'M');
        assert_eq!(changes[0].text, "src/b.rs");
        assert_eq!(changes[0].counts, Some((5, 2)));
        assert_eq!(changes[1].letter, 'D');
        assert!(changes[1].counted && changes[1].counts.is_none(), "binary");
        assert_eq!(s[0].1[0].letter, '!');
        assert_eq!(s[3].1[0].letter, 'U');

        let only_untracked = RepoStatus {
            untracked: vec!["n".into()],
            ..RepoStatus::default()
        };
        assert_eq!(build_sections(&only_untracked, &Counts::default()).len(), 1);
        assert!(build_sections(&RepoStatus::default(), &Counts::default()).is_empty());
    }

    #[test]
    fn numstat_parsing() {
        let raw = b"3\t1\tsrc/a.rs\0-\t-\timg/logo.png\0";
        let m = parse_numstat(raw);
        assert_eq!(m[Path::new("src/a.rs")], Some((3, 1)));
        assert_eq!(m[Path::new("img/logo.png")], None);
        // Rename: empty path then old and new.
        let raw = b"2\t0\t\0old.rs\0new.rs\0 4\t4\tz.rs\0";
        let m = parse_numstat(raw);
        assert_eq!(m[Path::new("new.rs")], Some((2, 0)));
        assert!(parse_numstat(b"").is_empty());
    }

    #[test]
    fn membership_and_totals() {
        let st = RepoStatus {
            staged: vec![change("a", ChangeKind::Added)],
            untracked: vec!["u".into()],
            ..RepoStatus::default()
        };
        assert_eq!(total_changes(&st), 2);
        assert!(contains(&st, Section::Staged, Path::new("a")));
        assert!(!contains(&st, Section::Changes, Path::new("a")));
    }
}
