//! The file tree model: a lazily loaded directory tree of the project root. Pure (no gpui), so the
//! laziness and the ignore rules are unit-tested.
//!
//! * Directories are listed **one level at a time**, only when expanded ([`FileTree::toggle`]
//!   reports which directory needs a listing; the panel runs [`list_dir`] on a background thread
//!   and hands the result to [`FileTree::set_children`]).
//! * `.gitignore` (and `.git/info/exclude`, the global ignore file) is honoured with the `ignore`
//!   crate's walker restricted to depth 1: it only reads the ignore files that apply to that one
//!   directory instead of walking the project. `.git` itself is never listed.
//! * A quick filter walks the whole project once, on demand, and yields a flat list of hits.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Directory entries listed per level at most (a folder with 100 000 files must not stall the UI).
pub const MAX_ENTRIES: usize = 5_000;

/// One directory entry as listed from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

fn skip_git(entry: &ignore::DirEntry) -> bool {
    entry.file_name() != ".git"
}

/// Lists `dir` (one level): directories first, then files, each group case-insensitively sorted.
/// With `respect_ignore`, entries hidden by `.gitignore` rules are left out. Unreadable
/// directories yield an empty list.
pub fn list_dir(dir: &Path, respect_ignore: bool) -> Vec<Entry> {
    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(respect_ignore)
        .git_global(respect_ignore)
        .git_exclude(respect_ignore)
        .ignore(false)
        .parents(respect_ignore)
        .require_git(false)
        .filter_entry(skip_git)
        .build();
    let mut entries: Vec<Entry> = walker
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .take(MAX_ENTRIES)
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: e.file_type().is_some_and(|t| t.is_dir()),
        })
        .collect();
    sort_entries(&mut entries);
    entries
}

/// Directories first, then names compared case-insensitively.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// One node of the tree (an entry of the arena).
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// 0 for the entries directly under the root.
    pub depth: u16,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub expanded: bool,
    /// The children were listed.
    pub loaded: bool,
    /// A listing is in flight.
    pub loading: bool,
}

/// What a toggle asks of the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// Nothing to do (a file, or an out-of-range index).
    None,
    /// The visible rows changed.
    Changed,
    /// The directory must be listed: call [`list_dir`] for `path` and feed [`FileTree::set_children`].
    Load { index: usize, path: PathBuf },
}

/// Lazily loaded tree. Node `0` is the root (never shown as a row, always expanded).
pub struct FileTree {
    nodes: Vec<TreeNode>,
    rows: Vec<usize>,
    pub respect_ignore: bool,
}

impl FileTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string_lossy().into_owned());
        let node = TreeNode {
            name,
            path: root,
            is_dir: true,
            depth: 0,
            parent: None,
            children: Vec::new(),
            expanded: true,
            loaded: false,
            loading: false,
        };
        Self { nodes: vec![node], rows: Vec::new(), respect_ignore: true }
    }

    pub fn root(&self) -> &Path {
        &self.nodes[0].path
    }

    /// Display name of the root folder.
    pub fn root_name(&self) -> &str {
        &self.nodes[0].name
    }

    pub fn node(&self, index: usize) -> Option<&TreeNode> {
        self.nodes.get(index)
    }

    /// Node indices of the visible rows, top to bottom.
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Index of the node at `path`, if it has been listed.
    pub fn find(&self, path: &Path) -> Option<usize> {
        self.nodes.iter().position(|n| n.path == path)
    }

    /// Directories that are expanded but not listed yet (and not being listed): at start, the root.
    pub fn pending_loads(&self) -> Vec<(usize, PathBuf)> {
        let mut out = Vec::new();
        self.collect_pending(0, &mut out);
        out
    }

    fn collect_pending(&self, idx: usize, out: &mut Vec<(usize, PathBuf)>) {
        let n = &self.nodes[idx];
        if !n.is_dir || !n.expanded {
            return;
        }
        if !n.loaded {
            if !n.loading {
                out.push((idx, n.path.clone()));
            }
            return;
        }
        for &c in &n.children {
            self.collect_pending(c, out);
        }
    }

    /// Marks a listing as in flight so [`FileTree::pending_loads`] does not return it again.
    pub fn mark_loading(&mut self, index: usize) {
        if let Some(n) = self.nodes.get_mut(index) {
            n.loading = true;
        }
    }

    /// Expands or collapses a directory row.
    pub fn toggle(&mut self, index: usize) -> Toggle {
        let Some(node) = self.nodes.get_mut(index) else { return Toggle::None };
        if !node.is_dir || index == 0 {
            return Toggle::None;
        }
        node.expanded = !node.expanded;
        let need_load = node.expanded && !node.loaded && !node.loading;
        let path = node.path.clone();
        if need_load {
            self.nodes[index].loading = true;
        }
        self.rebuild();
        if need_load { Toggle::Load { index, path } } else { Toggle::Changed }
    }

    /// Installs the listing of directory `index`. Children that already existed (a refresh) keep
    /// their expansion state.
    pub fn set_children(&mut self, index: usize, entries: Vec<Entry>) {
        let Some(parent) = self.nodes.get(index) else { return };
        let (parent_path, depth) = (parent.path.clone(), parent.depth + u16::from(index != 0));
        let previous: HashMap<String, bool> = parent
            .children
            .iter()
            .filter_map(|&c| self.nodes.get(c))
            .map(|n| (n.name.clone(), n.expanded))
            .collect();
        let mut children = Vec::with_capacity(entries.len());
        for e in entries {
            let expanded = e.is_dir && previous.get(&e.name).copied().unwrap_or(false);
            let id = self.nodes.len();
            self.nodes.push(TreeNode {
                path: parent_path.join(&e.name),
                name: e.name,
                is_dir: e.is_dir,
                depth,
                parent: Some(index),
                children: Vec::new(),
                expanded,
                loaded: false,
                loading: false,
            });
            children.push(id);
        }
        let node = &mut self.nodes[index];
        node.children = children;
        node.loaded = true;
        node.loading = false;
        self.rebuild();
    }

    /// Forgets every listing (keeping which folders are expanded) so the next
    /// [`FileTree::pending_loads`] re-lists them from disk.
    pub fn invalidate(&mut self) {
        for n in &mut self.nodes {
            n.loaded = false;
            n.loading = false;
        }
        self.rows.clear();
    }

    /// Expands every ancestor of `path` (which must be under the root) so the file is visible.
    /// Returns the directories that still need a listing.
    pub fn reveal(&mut self, path: &Path) -> Vec<(usize, PathBuf)> {
        let Ok(rel) = path.strip_prefix(self.root()) else { return Vec::new() };
        let mut cur = 0usize;
        for comp in rel.components() {
            let name = comp.as_os_str().to_string_lossy();
            let Some(next) = self.nodes[cur].children.iter().copied().find(|&c| self.nodes[c].name == name) else {
                break;
            };
            if self.nodes[next].is_dir {
                self.nodes[next].expanded = true;
            }
            cur = next;
        }
        self.rebuild();
        self.pending_loads()
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        self.walk(0, &mut rows);
        self.rows = rows;
    }

    fn walk(&self, idx: usize, out: &mut Vec<usize>) {
        let n = &self.nodes[idx];
        if !n.expanded {
            return;
        }
        for &c in &n.children {
            out.push(c);
            self.walk(c, out);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Git status markers (fed by the host)
// ---------------------------------------------------------------------------------------------

/// A file's git state, as the host reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitMark {
    Modified,
    Added,
    Untracked,
    Deleted,
    Conflict,
}

impl GitMark {
    /// The one-letter marker (`M`, `A`, `?`, `D`, `!`).
    pub fn letter(self) -> &'static str {
        match self {
            GitMark::Modified => "M",
            GitMark::Added => "A",
            GitMark::Untracked => "?",
            GitMark::Deleted => "D",
            GitMark::Conflict => "!",
        }
    }
}

/// Resolves host paths (absolute, or relative to `root`) to absolute ones.
pub fn normalize_marks(root: &Path, marks: HashMap<PathBuf, GitMark>) -> HashMap<PathBuf, GitMark> {
    marks.into_iter().map(|(p, m)| (if p.is_relative() { root.join(p) } else { p }, m)).collect()
}

/// Directories under `root` that contain at least one marked file (they get a dot in the tree).
pub fn dirty_dirs(root: &Path, marks: &HashMap<PathBuf, GitMark>) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for path in marks.keys() {
        let mut cur = path.parent();
        while let Some(dir) = cur {
            if dir == root || !dir.starts_with(root) {
                break;
            }
            if !out.insert(dir.to_path_buf()) {
                break;
            }
            cur = dir.parent();
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Quick filter
// ---------------------------------------------------------------------------------------------

/// A file that matched the quick filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterHit {
    pub path: PathBuf,
    /// Path relative to the root, forward slashes.
    pub rel: String,
    pub name: String,
    /// The query matched the file name (ranks above a match in a folder name).
    pub in_name: bool,
}

/// Results kept at most.
pub const MAX_HITS: usize = 500;
/// Entries visited at most (a safety net on monsters like an unignored `node_modules`).
pub const MAX_VISITED: usize = 200_000;

/// Walks `root` (honouring ignore rules) and returns the files whose path contains every word of
/// `query` (ASCII case-insensitive), file-name matches first, then shorter paths. Meant for a
/// background thread. An empty query returns nothing.
pub fn filter_files(root: &Path, query: &str, respect_ignore: bool) -> Vec<FilterHit> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(respect_ignore)
        .git_global(respect_ignore)
        .git_exclude(respect_ignore)
        .ignore(false)
        .parents(respect_ignore)
        .require_git(false)
        .filter_entry(skip_git)
        .build();
    let mut hits = Vec::new();
    for entry in walker.filter_map(Result::ok).take(MAX_VISITED) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let rel_lc = rel.to_lowercase();
        if !words.iter().all(|w| rel_lc.contains(w.as_str())) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let name_lc = name.to_lowercase();
        let in_name = words.iter().all(|w| name_lc.contains(w.as_str()));
        hits.push(FilterHit { path: path.to_path_buf(), rel, name, in_name });
    }
    hits.sort_by(|a, b| b.in_name.cmp(&a.in_name).then(a.rel.len().cmp(&b.rel.len())).then_with(|| a.rel.cmp(&b.rel)));
    hits.truncate(MAX_HITS);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("src/inner")).unwrap();
        fs::create_dir_all(r.join("target/debug")).unwrap();
        fs::create_dir_all(r.join(".git/objects")).unwrap();
        fs::write(r.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(r.join("Cargo.toml"), "x").unwrap();
        fs::write(r.join("README.md"), "x").unwrap();
        fs::write(r.join("debug.log"), "x").unwrap();
        fs::write(r.join("src/main.rs"), "x").unwrap();
        fs::write(r.join("src/trace.log"), "x").unwrap();
        fs::write(r.join("src/inner/deep.rs"), "x").unwrap();
        fs::write(r.join("target/debug/out.exe"), "x").unwrap();
        dir
    }

    fn names(e: &[Entry]) -> Vec<&str> {
        e.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_one_level_dirs_first_and_skips_ignored_and_git() {
        let p = project();
        let top = list_dir(p.path(), true);
        assert_eq!(names(&top), ["src", ".gitignore", "Cargo.toml", "README.md"]);
        assert!(top[0].is_dir && !top[1].is_dir);
        let src = list_dir(&p.path().join("src"), true);
        // the parent's `*.log` rule applies inside the subdirectory
        assert_eq!(names(&src), ["inner", "main.rs"]);
    }

    #[test]
    fn ignore_can_be_turned_off() {
        let p = project();
        let top = list_dir(p.path(), false);
        assert!(names(&top).contains(&"target"));
        assert!(names(&top).contains(&"debug.log"));
        assert!(!names(&top).contains(&".git"));
    }

    #[test]
    fn missing_directory_lists_nothing() {
        assert!(list_dir(Path::new("C:/definitely/not/here"), true).is_empty());
    }

    #[test]
    fn sorting_is_case_insensitive_dirs_first() {
        let mut e = vec![
            Entry { name: "b.txt".into(), is_dir: false },
            Entry { name: "Zeta".into(), is_dir: true },
            Entry { name: "A.txt".into(), is_dir: false },
            Entry { name: "alpha".into(), is_dir: true },
        ];
        sort_entries(&mut e);
        assert_eq!(names(&e), ["alpha", "Zeta", "A.txt", "b.txt"]);
    }

    fn loaded_tree(p: &Path) -> FileTree {
        let mut t = FileTree::new(p);
        assert_eq!(t.pending_loads().len(), 1);
        t.set_children(0, list_dir(p, true));
        t
    }

    #[test]
    fn expanding_loads_one_level_lazily() {
        let p = project();
        let mut t = loaded_tree(p.path());
        // Only the top level is known: src, .gitignore, Cargo.toml, README.md
        assert_eq!(t.len(), 4);
        let src = t.rows()[0];
        assert_eq!(t.node(src).unwrap().name, "src");
        assert!(!t.node(src).unwrap().loaded);
        // Expanding asks for a listing and does not touch deeper directories.
        let Toggle::Load { index, path } = t.toggle(src) else { panic!("expected a load") };
        assert_eq!(index, src);
        assert_eq!(path, p.path().join("src"));
        assert!(t.pending_loads().is_empty(), "already in flight");
        t.set_children(index, list_dir(&path, true));
        assert_eq!(t.len(), 4 + 2);
        assert_eq!(t.node(t.rows()[1]).unwrap().name, "inner");
        assert_eq!(t.node(t.rows()[1]).unwrap().depth, 1);
        assert!(!t.node(t.rows()[1]).unwrap().loaded, "inner was not listed");
        // Collapse and expand again: no second listing.
        assert_eq!(t.toggle(src), Toggle::Changed);
        assert_eq!(t.len(), 4);
        assert_eq!(t.toggle(src), Toggle::Changed);
        assert_eq!(t.len(), 6);
    }

    #[test]
    fn toggling_a_file_or_the_root_does_nothing() {
        let p = project();
        let mut t = loaded_tree(p.path());
        let file = t.rows()[3];
        assert_eq!(t.toggle(file), Toggle::None);
        assert_eq!(t.toggle(0), Toggle::None);
        assert_eq!(t.toggle(999), Toggle::None);
    }

    #[test]
    fn refresh_keeps_expanded_folders() {
        let p = project();
        let mut t = loaded_tree(p.path());
        let src = t.rows()[0];
        if let Toggle::Load { index, path } = t.toggle(src) {
            t.set_children(index, list_dir(&path, true));
        }
        t.invalidate();
        let pending = t.pending_loads();
        assert_eq!(pending.len(), 1, "only the root: children are discovered by its listing");
        t.set_children(0, list_dir(p.path(), true));
        let pending = t.pending_loads();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, p.path().join("src"));
    }

    #[test]
    fn reveal_expands_ancestors() {
        let p = project();
        let mut t = loaded_tree(p.path());
        let need = t.reveal(&p.path().join("src/inner/deep.rs"));
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].1, p.path().join("src"));
        t.mark_loading(need[0].0);
        t.set_children(need[0].0, list_dir(&need[0].1, true));
        let need = t.reveal(&p.path().join("src/inner/deep.rs"));
        assert_eq!(need[0].1, p.path().join("src/inner"));
        t.set_children(need[0].0, list_dir(&need[0].1, true));
        let names: Vec<_> = t.rows().iter().map(|&i| t.node(i).unwrap().name.clone()).collect();
        assert!(names.contains(&"deep.rs".to_string()), "{names:?}");
        // Paths outside the root are ignored.
        assert!(t.reveal(Path::new("C:/elsewhere/x.rs")).is_empty());
    }

    #[test]
    fn marks_normalize_and_bubble_up_to_folders() {
        let root = Path::new("C:/proj");
        let marks: HashMap<PathBuf, GitMark> = [
            (PathBuf::from("src/inner/deep.rs"), GitMark::Modified),
            (PathBuf::from("C:/proj/README.md"), GitMark::Untracked),
        ]
        .into();
        let marks = normalize_marks(root, marks);
        assert_eq!(marks.get(&root.join("src/inner/deep.rs")), Some(&GitMark::Modified));
        let dirty = dirty_dirs(root, &marks);
        assert!(dirty.contains(&root.join("src")));
        assert!(dirty.contains(&root.join("src/inner")));
        assert!(!dirty.contains(root));
        assert_eq!(dirty.len(), 2);
        assert_eq!(GitMark::Modified.letter(), "M");
        assert_eq!(GitMark::Added.letter(), "A");
        assert_eq!(GitMark::Untracked.letter(), "?");
    }

    #[test]
    fn filter_ranks_name_matches_first_and_respects_ignore() {
        let p = project();
        let hits = filter_files(p.path(), "main", true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel, "src/main.rs");
        // `out.exe` lives in the ignored target/ dir
        assert!(filter_files(p.path(), "out", true).is_empty());
        assert_eq!(filter_files(p.path(), "out", false).len(), 1);
        // a folder-name match ranks after a file-name match
        let hits = filter_files(p.path(), "inner", true);
        assert_eq!(hits[0].rel, "src/inner/deep.rs");
        assert!(!hits[0].in_name);
        let hits = filter_files(p.path(), "rs", true);
        assert!(hits.iter().all(|h| h.rel.contains("rs")));
        // multi-word: every word must be in the path
        assert_eq!(filter_files(p.path(), "src deep", true).len(), 1);
        assert!(filter_files(p.path(), "   ", true).is_empty());
    }
}
