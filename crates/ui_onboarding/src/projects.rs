//! Step 3: root folders, streaming project discovery and the checkable project list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc::UnboundedSender;
use store::paths::normalize_key;
use store::{DiscoveredProject, ProjectKind, detect_project, discover_projects};

/// Folders offered by default when they exist.
pub fn default_roots(home: &Path) -> Vec<PathBuf> {
    ["Desktop", "source\\repos", "Projects", "dev"]
        .iter()
        .map(|rel| home.join(rel))
        .filter(|p| p.is_dir())
        .collect()
}

/// The user's home directory (`USERPROFILE`, then `HOME`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Depth (below each sub-folder of a root) searched by [`scan_streaming`].
pub const SCAN_DEPTH: usize = 2;

/// Messages of the background scan.
#[derive(Debug)]
pub enum ScanMsg {
    /// A batch of projects found under one folder.
    Found(Vec<DiscoveredProject>),
    /// The scan finished (or was cancelled).
    Done,
}

/// Folders never worth entering from a root (hidden ones are skipped by name).
fn skip_dir(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with('.')
        || matches!(
            n.as_str(),
            "node_modules"
                | "target"
                | "bin"
                | "obj"
                | "dist"
                | "build"
                | "out"
                | "venv"
                | "appdata"
        )
}

/// Scan `roots` and stream the results: one [`ScanMsg::Found`] per immediate sub-folder of a
/// root, so the list fills while the disk is still being read. Uses `store::discover_projects`
/// for each sub-folder. Blocking: call it from the background executor. `cancel` is checked
/// between folders; a final `Done` is always sent.
pub fn scan_streaming(roots: &[PathBuf], cancel: &AtomicBool, tx: &UnboundedSender<ScanMsg>) {
    let mut seen = std::collections::HashSet::new();
    let mut send = |batch: Vec<DiscoveredProject>| {
        let fresh: Vec<_> = batch
            .into_iter()
            .filter(|p| seen.insert(normalize_key(&p.path.to_string_lossy())))
            .collect();
        if !fresh.is_empty() {
            let _ = tx.unbounded_send(ScanMsg::Found(fresh));
        }
    };
    'roots: for root in roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(p) = detect_project(root) {
            send(vec![p]);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
            .filter(|e| !skip_dir(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        subdirs.sort();
        for sub in subdirs {
            if cancel.load(Ordering::Relaxed) {
                break 'roots;
            }
            send(discover_projects(std::slice::from_ref(&sub), SCAN_DEPTH));
        }
    }
    let _ = tx.unbounded_send(ScanMsg::Done);
}

/// Shared cancel flag for a scan; setting it stops the blocking walker at the next folder.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn as_atomic(&self) -> &AtomicBool {
        &self.0
    }
}

/// Short badge text of a project kind.
pub const fn kind_label(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Git => "Git",
        ProjectKind::Node => "Node",
        ProjectKind::Rust => "Rust",
        ProjectKind::Dotnet => ".NET",
        ProjectKind::Python => "Python",
        ProjectKind::Other => "Otro",
    }
}

/// One discovered project plus the user's choices for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectItem {
    pub project: DiscoveredProject,
    pub checked: bool,
    pub favorite: bool,
}

/// The project list of step 3: arrives in batches, every row checkable, a star per row.
#[derive(Clone, Debug, Default)]
pub struct ProjectList {
    items: Vec<ProjectItem>,
    index: HashMap<String, usize>,
    /// Choices made on rows that are not in the list right now (a root was switched off and on).
    remembered: HashMap<String, (bool, bool)>,
}

fn key_of(p: &DiscoveredProject) -> String {
    normalize_key(&p.path.to_string_lossy())
}

impl ProjectList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[ProjectItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Add a batch. New rows start checked; a row seen before keeps the user's earlier choices.
    /// Duplicates (same normalized path) are ignored. The list stays sorted by path.
    pub fn push_batch(&mut self, batch: impl IntoIterator<Item = DiscoveredProject>) {
        let mut added = false;
        for project in batch {
            let key = key_of(&project);
            if self.index.contains_key(&key) {
                continue;
            }
            let (checked, favorite) = self.remembered.get(&key).copied().unwrap_or((true, false));
            self.items.push(ProjectItem {
                project,
                checked,
                favorite,
            });
            self.index.insert(key, self.items.len() - 1);
            added = true;
        }
        if added {
            self.items
                .sort_by(|a, b| a.project.path.cmp(&b.project.path));
            self.index = self
                .items
                .iter()
                .enumerate()
                .map(|(i, it)| (key_of(&it.project), i))
                .collect();
        }
    }

    /// Empty the list for a new scan, remembering the choices already made.
    pub fn clear_keep_choices(&mut self) {
        for it in &self.items {
            self.remembered
                .insert(key_of(&it.project), (it.checked, it.favorite));
        }
        self.items.clear();
        self.index.clear();
    }

    pub fn toggle_checked(&mut self, ix: usize) {
        if let Some(it) = self.items.get_mut(ix) {
            it.checked = !it.checked;
        }
    }

    /// Star a row. Starring also checks it (a favorite is always imported); unchecking a
    /// favorite removes the star.
    pub fn toggle_favorite(&mut self, ix: usize) {
        if let Some(it) = self.items.get_mut(ix) {
            it.favorite = !it.favorite;
            if it.favorite {
                it.checked = true;
            }
        }
    }

    pub fn set_checked(&mut self, ix: usize, checked: bool) {
        if let Some(it) = self.items.get_mut(ix) {
            it.checked = checked;
            if !checked {
                it.favorite = false;
            }
        }
    }

    pub fn set_all(&mut self, checked: bool) {
        for it in &mut self.items {
            it.checked = checked;
            if !checked {
                it.favorite = false;
            }
        }
    }

    pub fn checked_count(&self) -> usize {
        self.items.iter().filter(|i| i.checked).count()
    }

    pub fn favorite_count(&self) -> usize {
        self.items.iter().filter(|i| i.favorite).count()
    }

    /// The projects to import.
    pub fn selected(&self) -> Vec<DiscoveredProject> {
        self.items
            .iter()
            .filter(|i| i.checked)
            .map(|i| i.project.clone())
            .collect()
    }

    /// Favorite paths, in list order.
    pub fn favorites(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter(|i| i.favorite && i.checked)
            .map(|i| i.project.path.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(path: &str, kind: ProjectKind) -> DiscoveredProject {
        let path = PathBuf::from(path);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        DiscoveredProject {
            path,
            name,
            kind,
            is_git: kind == ProjectKind::Git,
        }
    }

    #[test]
    fn batches_dedupe_sort_and_start_checked() {
        let mut l = ProjectList::new();
        l.push_batch([
            proj("C:/p/b", ProjectKind::Rust),
            proj("C:/p/a", ProjectKind::Node),
        ]);
        l.push_batch([
            proj("c:\\P\\B", ProjectKind::Rust),
            proj("C:/p/c", ProjectKind::Git),
        ]);
        let names: Vec<_> = l.items().iter().map(|i| i.project.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"], "sorted, no duplicate b");
        assert_eq!(l.checked_count(), 3);
        assert_eq!(l.selected().len(), 3);
    }

    #[test]
    fn toggling_and_favorites() {
        let mut l = ProjectList::new();
        l.push_batch([
            proj("C:/p/a", ProjectKind::Rust),
            proj("C:/p/b", ProjectKind::Rust),
        ]);
        l.toggle_checked(0);
        assert_eq!(l.selected().len(), 1);
        l.toggle_favorite(0); // starring an unchecked row checks it
        assert!(l.items()[0].checked && l.items()[0].favorite);
        l.toggle_favorite(1);
        assert_eq!(
            l.favorites(),
            [PathBuf::from("C:/p/a"), PathBuf::from("C:/p/b")]
        );
        l.set_checked(0, false); // unchecking drops the star
        assert_eq!(l.favorites(), [PathBuf::from("C:/p/b")]);
        l.set_all(false);
        assert!(l.selected().is_empty() && l.favorites().is_empty());
        l.set_all(true);
        assert_eq!(l.checked_count(), 2);
        assert_eq!(l.favorite_count(), 0);
        l.toggle_checked(99); // out of range: ignored
    }

    #[test]
    fn choices_survive_a_rescan() {
        let mut l = ProjectList::new();
        l.push_batch([
            proj("C:/p/a", ProjectKind::Rust),
            proj("C:/p/b", ProjectKind::Rust),
        ]);
        l.toggle_checked(1);
        l.toggle_favorite(0);
        l.clear_keep_choices();
        assert!(l.is_empty());
        l.push_batch([
            proj("C:/p/b", ProjectKind::Rust),
            proj("C:/p/a", ProjectKind::Rust),
            proj("C:/p/z", ProjectKind::Node),
        ]);
        assert!(l.items()[0].favorite && l.items()[0].checked, "a: starred");
        assert!(!l.items()[1].checked, "b: stays unchecked");
        assert!(l.items()[2].checked, "z: new, checked");
    }

    #[test]
    fn scan_streams_batches_then_done() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, marker) in [
            ("alpha", "Cargo.toml"),
            ("beta", "package.json"),
            ("gamma", "x.sln"),
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join(marker), "").unwrap();
        }
        std::fs::create_dir_all(root.join("plain").join("deep").join("proj")).unwrap();
        std::fs::write(
            root.join("plain")
                .join("deep")
                .join("proj")
                .join("pyproject.toml"),
            "",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("node_modules").join("junk")).unwrap();
        std::fs::write(
            root.join("node_modules").join("junk").join("package.json"),
            "",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join(".hidden").join("Cargo.toml"), "").unwrap();

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let flag = CancelFlag::default();
        scan_streaming(&[root.to_path_buf()], flag.as_atomic(), &tx);
        drop(tx);
        let mut batches = 0;
        let mut names = Vec::new();
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMsg::Found(b) => {
                    batches += 1;
                    names.extend(b.into_iter().map(|p| p.name));
                }
                ScanMsg::Done => done = true,
            }
        }
        names.sort();
        assert_eq!(names, ["alpha", "beta", "gamma", "proj"]);
        assert!(batches >= 3, "streamed per folder, got {batches} batches");
        assert!(done);
    }

    #[test]
    fn cancelled_scan_stops_early_but_still_reports_done() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a").join("Cargo.toml"), "").unwrap();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let flag = CancelFlag::default();
        flag.cancel();
        scan_streaming(&[dir.path().to_path_buf()], flag.as_atomic(), &tx);
        drop(tx);
        assert!(matches!(rx.try_recv(), Ok(ScanMsg::Done)));
    }

    #[test]
    fn default_roots_keep_only_existing_folders() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("Desktop")).unwrap();
        std::fs::create_dir_all(home.path().join("dev")).unwrap();
        let roots = default_roots(home.path());
        assert_eq!(
            roots,
            [home.path().join("Desktop"), home.path().join("dev")]
        );
        assert!(default_roots(&home.path().join("nope")).is_empty());
    }
}
