//! The multi-file diff a review is made of: where it comes from ([`ReviewSource`]), one
//! [`FileDiff`] per changed file (hunks from `viewer_svc`, flattened into rows) and the loader that
//! runs on a background thread.
//!
//! Zero idle cost: nothing watches the repository. The host asks for a refresh when something
//! happened (the agent finished a turn, the user pressed "Actualizar").

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use git_svc::{ChangeKind, DeltaKind, Repo};
use viewer_svc::{DiffLine, GitBase, Hunk, LineKind, diff_against_git};

use crate::note::{Side, SideLines};

/// Files bigger than this are listed but not diffed.
pub const MAX_DIFF_BYTES: u64 = 1_500_000;
/// Untracked directories are expanded up to this many files.
const MAX_UNTRACKED_FILES: usize = 200;

/// What a review shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewSource {
    /// Staged + unstaged + untracked changes of the worktree against HEAD.
    Worktree { root: PathBuf },
    /// What changed since a checkpoint (A10): "Qué tocó el agente".
    SinceCheckpoint { root: PathBuf, id: String },
    /// Specific files against `base`.
    Files { root: PathBuf, base: GitBase, paths: Vec<PathBuf> },
}

impl ReviewSource {
    pub fn root(&self) -> &Path {
        match self {
            ReviewSource::Worktree { root } | ReviewSource::SinceCheckpoint { root, .. } | ReviewSource::Files { root, .. } => root,
        }
    }

    /// Stable key to store drafts under (together with the root).
    pub fn key(&self) -> String {
        match self {
            ReviewSource::Worktree { .. } => "worktree".to_string(),
            ReviewSource::SinceCheckpoint { id, .. } => format!("checkpoint:{id}"),
            ReviewSource::Files { base, paths, .. } => {
                let base = match base {
                    GitBase::Head => "HEAD".to_string(),
                    GitBase::Commit(c) => c.clone(),
                };
                let mut p: Vec<String> = paths.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
                p.sort();
                format!("files:{base}:{}", crate::note::lines_hash(p.iter().map(String::as_str)))
            }
        }
    }

    /// Phrase for the message to the agent ("los cambios del worktree").
    pub fn phrase(&self) -> String {
        match self {
            ReviewSource::Worktree { .. } => "los cambios del worktree".to_string(),
            ReviewSource::SinceCheckpoint { .. } => "lo que cambió desde el checkpoint".to_string(),
            ReviewSource::Files { paths, .. } => format!("{} archivo(s) concretos", paths.len()),
        }
    }

    /// Short title for the header and the tab subtitle.
    pub fn title(&self) -> String {
        match self {
            ReviewSource::Worktree { .. } => "Cambios del worktree".to_string(),
            ReviewSource::SinceCheckpoint { .. } => "Qué tocó el agente".to_string(),
            ReviewSource::Files { base, .. } => match base {
                GitBase::Head => "Archivos contra HEAD".to_string(),
                GitBase::Commit(c) => format!("Archivos contra {}", c.chars().take(8).collect::<String>()),
            },
        }
    }
}

/// Kind of change of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

impl FileStatus {
    /// One-letter mark (A, M, D).
    pub const fn mark(self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
        }
    }
}

/// Whether the file has a text diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileBody {
    Text,
    Binary,
    TooLarge,
}

/// A row of the flattened diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    /// `@@ -1,3 +1,4 @@`
    Hunk(String),
    Line(DiffLine),
}

/// One changed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// Repository-relative, forward slashes.
    pub path: String,
    pub status: FileStatus,
    pub body: FileBody,
    pub rows: Vec<DiffRow>,
    pub added: usize,
    pub removed: usize,
    new_index: HashMap<usize, usize>,
    old_index: HashMap<usize, usize>,
}

impl FileDiff {
    pub fn new(path: &str, status: FileStatus, body: FileBody, hunks: &[Hunk]) -> Self {
        let mut rows = Vec::with_capacity(hunks.iter().map(|h| h.lines.len() + 1).sum());
        let (mut added, mut removed) = (0, 0);
        let mut new_index = HashMap::new();
        let mut old_index = HashMap::new();
        for h in hunks {
            rows.push(DiffRow::Hunk(h.header()));
            for l in &h.lines {
                match l.kind {
                    LineKind::Added => added += 1,
                    LineKind::Removed => removed += 1,
                    LineKind::Context => {}
                }
                if let Some(n) = l.new_line {
                    new_index.insert(n, rows.len());
                }
                if let Some(n) = l.old_line {
                    old_index.insert(n, rows.len());
                }
                rows.push(DiffRow::Line(l.clone()));
            }
        }
        Self { path: path.replace('\\', "/"), status, body, rows, added, removed, new_index, old_index }
    }

    /// A diff of two texts (for hosts that already have both sides, and for tests).
    pub fn from_texts(path: &str, old: &str, new: &str) -> Self {
        let status = if old.is_empty() && !new.is_empty() {
            FileStatus::Added
        } else if new.is_empty() && !old.is_empty() {
            FileStatus::Deleted
        } else {
            FileStatus::Modified
        };
        Self::new(path, status, FileBody::Text, &viewer_svc::diff_text(old, new))
    }

    /// Row index of `line` on `side`.
    pub fn row_of(&self, side: Side, line: usize) -> Option<usize> {
        match side {
            Side::New => self.new_index.get(&line),
            Side::Old => self.old_index.get(&line),
        }
        .copied()
    }

    /// The visible lines of `side` (changes plus context), by line number.
    pub fn side_lines(&self, side: Side) -> SideLines {
        let mut out = SideLines::new();
        for r in &self.rows {
            if let DiffRow::Line(l) = r {
                let n = match side {
                    Side::New => l.new_line,
                    Side::Old => l.old_line,
                };
                if let Some(n) = n {
                    out.insert(n, l.text.clone());
                }
            }
        }
        out
    }

    /// Number of text lines (rows that are not hunk headers).
    pub fn line_count(&self) -> usize {
        self.rows.iter().filter(|r| matches!(r, DiffRow::Line(_))).count()
    }

    /// File name without its directory.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Directory part (empty for a root file).
    pub fn dir(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(d, _)| d)
    }
}

/// The side a clicked row belongs to, and its number there: removed lines live on the old side,
/// everything else on the new one.
pub fn anchor_of(line: &DiffLine) -> Option<(Side, usize)> {
    match line.kind {
        LineKind::Removed => line.old_line.map(|n| (Side::Old, n)),
        _ => line.new_line.map(|n| (Side::New, n)),
    }
}

fn rel_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Text, binary (a NUL in the first 8 KB) or too large, judged from the file on disk.
fn probe(path: &Path) -> FileBody {
    use std::io::Read as _;
    let Ok(meta) = std::fs::metadata(path) else {
        return FileBody::Text; // deleted: the old text is shown
    };
    if meta.len() > MAX_DIFF_BYTES {
        return FileBody::TooLarge;
    }
    let mut buf = [0u8; 8192];
    match std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) if buf[..n].contains(&0) => FileBody::Binary,
        _ => FileBody::Text,
    }
}

fn diff_one(root: &Path, rel: &Path, status: FileStatus, base: GitBase) -> FileDiff {
    let abs = root.join(rel);
    let name = rel_string(rel);
    let body = probe(&abs);
    if body != FileBody::Text {
        return FileDiff::new(&name, status, body, &[]);
    }
    match diff_against_git(root, &abs, base) {
        Ok(hunks) => {
            // `git show` of a missing path reads as empty: classify from the disk.
            let status = if !abs.exists() { FileStatus::Deleted } else { status };
            FileDiff::new(&name, status, FileBody::Text, &hunks)
        }
        Err(_) => FileDiff::new(&name, status, FileBody::Text, &[]),
    }
}

/// Expands an untracked directory entry (`dir/`) into its files.
fn expand_untracked(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    let abs = root.join(rel);
    if !abs.is_dir() {
        out.push(rel.to_path_buf());
        return;
    }
    let mut stack = vec![abs];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if out.len() >= MAX_UNTRACKED_FILES {
                return;
            }
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(r) = p.strip_prefix(root) {
                out.push(r.to_path_buf());
            }
        }
    }
}

/// Loads the diff of `source`. Blocking: call it from a background executor.
pub fn load_source(source: &ReviewSource) -> Result<Vec<FileDiff>, String> {
    let root = source.root();
    let mut files: Vec<FileDiff> = match source {
        ReviewSource::Worktree { .. } => {
            let repo = Repo::discover(root).map_err(|e| e.to_string())?.ok_or_else(|| format!("{} no es un repositorio git", root.display()))?;
            let repo_root = repo.root.clone();
            let st = repo.status().map_err(|e| e.to_string())?;
            let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
            let mut list: Vec<(PathBuf, FileStatus)> = Vec::new();
            for c in st.staged.iter().chain(st.unstaged.iter()) {
                let status = match c.kind {
                    ChangeKind::Added | ChangeKind::Copied => FileStatus::Added,
                    ChangeKind::Deleted => FileStatus::Deleted,
                    _ => FileStatus::Modified,
                };
                if seen.insert(c.path.clone()) {
                    list.push((c.path.clone(), status));
                }
            }
            let mut untracked = Vec::new();
            for u in &st.untracked {
                expand_untracked(&repo_root, u, &mut untracked);
            }
            for u in untracked {
                if seen.insert(u.clone()) {
                    list.push((u, FileStatus::Added));
                }
            }
            list.into_iter().map(|(p, s)| diff_one(&repo_root, &p, s, GitBase::Head)).collect()
        }
        ReviewSource::SinceCheckpoint { id, .. } => {
            let repo = Repo::discover(root).map_err(|e| e.to_string())?.ok_or_else(|| format!("{} no es un repositorio git", root.display()))?;
            let cp = repo
                .checkpoints()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("el checkpoint {id} ya no existe"))?;
            let changes = repo.changed_files_since(id).map_err(|e| e.to_string())?;
            changes
                .into_iter()
                .map(|c| {
                    let status = match c.kind {
                        DeltaKind::Added => FileStatus::Added,
                        DeltaKind::Deleted => FileStatus::Deleted,
                        _ => FileStatus::Modified,
                    };
                    diff_one(&repo.root, &c.path, status, GitBase::Commit(cp.commit.clone()))
                })
                .collect()
        }
        ReviewSource::Files { base, paths, .. } => paths
            .iter()
            .map(|p| {
                let rel = p.strip_prefix(root).unwrap_or(p);
                diff_one(root, rel, FileStatus::Modified, base.clone())
            })
            .collect(),
    };
    // Drop files whose diff is empty (touched and reverted) unless they are binary / huge.
    files.retain(|f| f.body != FileBody::Text || !f.rows.is_empty());
    files.sort_by_cached_key(|f| f.path.to_lowercase());
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn rows_and_indexes() {
        let f = FileDiff::from_texts("src/a.rs", "a\nb\nc\nd\n", "a\nB\nc\nd\ne\n");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!((f.added, f.removed), (2, 1));
        assert!(matches!(f.rows[0], DiffRow::Hunk(_)));
        let new = f.side_lines(Side::New);
        assert_eq!(new.get(&2).map(String::as_str), Some("B"));
        assert_eq!(new.get(&5).map(String::as_str), Some("e"));
        let old = f.side_lines(Side::Old);
        assert_eq!(old.get(&2).map(String::as_str), Some("b"));
        // Row lookups.
        let r = f.row_of(Side::New, 2).unwrap();
        assert!(matches!(&f.rows[r], DiffRow::Line(l) if l.text == "B"));
        let r = f.row_of(Side::Old, 2).unwrap();
        assert!(matches!(&f.rows[r], DiffRow::Line(l) if l.text == "b"));
        assert!(f.row_of(Side::New, 99).is_none());
        assert_eq!((f.name(), f.dir()), ("a.rs", "src"));
    }

    #[test]
    fn status_from_texts() {
        assert_eq!(FileDiff::from_texts("a", "", "x\n").status, FileStatus::Added);
        assert_eq!(FileDiff::from_texts("a", "x\n", "").status, FileStatus::Deleted);
    }

    #[test]
    fn click_side() {
        let f = FileDiff::from_texts("a", "a\nb\n", "a\nc\n");
        let lines: Vec<&DiffLine> = f.rows.iter().filter_map(|r| if let DiffRow::Line(l) = r { Some(l) } else { None }).collect();
        let removed = lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
        let added = lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
        assert_eq!(anchor_of(removed), Some((Side::Old, 2)));
        assert_eq!(anchor_of(added), Some((Side::New, 2)));
    }

    #[test]
    fn source_keys_differ() {
        let w = ReviewSource::Worktree { root: "r".into() };
        let c = ReviewSource::SinceCheckpoint { root: "r".into(), id: "123-abc".into() };
        let f1 = ReviewSource::Files { root: "r".into(), base: GitBase::Head, paths: vec!["a".into(), "b".into()] };
        let f2 = ReviewSource::Files { root: "r".into(), base: GitBase::Head, paths: vec!["b".into(), "a".into()] };
        let f3 = ReviewSource::Files { root: "r".into(), base: GitBase::Head, paths: vec!["a".into()] };
        assert_ne!(w.key(), c.key());
        assert_eq!(f1.key(), f2.key(), "path order does not matter");
        assert_ne!(f1.key(), f3.key());
        assert_eq!(c.key(), "checkpoint:123-abc");
    }

    #[test]
    fn worktree_source_lists_modified_added_deleted_and_untracked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("keep.txt"), "uno\ndos\ntres\n").unwrap();
        std::fs::write(root.join("gone.txt"), "adios\n").unwrap();
        std::fs::write(root.join("same.txt"), "igual\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "init"]);

        std::fs::write(root.join("keep.txt"), "uno\nDOS\ntres\ncuatro\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::create_dir_all(root.join("new_dir")).unwrap();
        std::fs::write(root.join("new_dir").join("fresh.txt"), "hola\n").unwrap();
        std::fs::write(root.join("blob.bin"), [0u8, 1, 2, 3, 0]).unwrap();

        let files = load_source(&ReviewSource::Worktree { root: root.to_path_buf() }).unwrap();
        let by: HashMap<&str, &FileDiff> = files.iter().map(|f| (f.path.as_str(), f)).collect();
        assert!(!by.contains_key("same.txt"), "unchanged files are not listed");
        assert_eq!(by["keep.txt"].status, FileStatus::Modified);
        assert_eq!((by["keep.txt"].added, by["keep.txt"].removed), (2, 1));
        assert_eq!(by["gone.txt"].status, FileStatus::Deleted);
        assert_eq!(by["gone.txt"].removed, 1);
        assert_eq!(by["new_dir/fresh.txt"].status, FileStatus::Added);
        assert_eq!(by["new_dir/fresh.txt"].added, 1);
        assert_eq!(by["blob.bin"].body, FileBody::Binary);
        // Sorted by path.
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|s| s.to_lowercase());
        assert_eq!(names, sorted);
    }

    #[test]
    fn not_a_repository_is_an_error_message() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_source(&ReviewSource::Worktree { root: dir.path().to_path_buf() }).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn since_checkpoint_shows_only_what_changed_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "uno\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "init"]);
        // Dirty before the checkpoint: must not show up.
        std::fs::write(root.join("b.txt"), "b editado antes\n").unwrap();
        let repo = Repo::discover(root).unwrap().unwrap();
        let cp = repo.create_checkpoint("antes del agente").unwrap();
        std::fs::write(root.join("a.txt"), "uno\ndos\n").unwrap();
        std::fs::write(root.join("c.txt"), "nuevo\n").unwrap();

        let files = load_source(&ReviewSource::SinceCheckpoint { root: root.to_path_buf(), id: cp.id }).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "c.txt"], "{files:?}");
        assert_eq!(files[0].added, 1);
        assert_eq!(files[1].status, FileStatus::Added);

        let err = load_source(&ReviewSource::SinceCheckpoint { root: root.to_path_buf(), id: "nope".into() }).unwrap_err();
        assert!(err.contains("nope"));
    }

    #[test]
    fn files_source_diffs_against_a_base() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "uno\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "init"]);
        std::fs::write(root.join("a.txt"), "uno\ndos\n").unwrap();
        let files = load_source(&ReviewSource::Files { root: root.to_path_buf(), base: GitBase::Head, paths: vec![root.join("a.txt")] }).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].added, 1);
    }
}
