//! Debounced, event-driven (no polling) watcher for a repository.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::Result;
use crate::repo::Repo;

/// Directory names whose contents never affect git state we display.
const NOISE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
];

/// Entries directly inside the git dir that matter (HEAD moves, index changes, in-progress ops).
const GIT_DIR_INTERESTING: &[&str] = &[
    "head",
    "index",
    "packed-refs",
    "refs",
    "merge_head",
    "cherry_pick_head",
    "revert_head",
    "rebase_head",
    "rebase-merge",
    "rebase-apply",
];

enum Msg {
    Change,
    Stop,
}

/// Keeps the watcher alive. Dropping it stops the watcher and closes the event channel.
pub struct WatchHandle {
    watcher: Option<RecommendedWatcher>,
    tx: mpsc::Sender<Msg>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // Stop OS notifications first, then wake the debounce thread so it exits.
        self.watcher.take();
        let _ = self.tx.send(Msg::Stop);
    }
}

/// Decides which filesystem paths are worth a refresh.
#[derive(Clone)]
pub(crate) struct PathFilter {
    root: String,
    git_dir: String,
    common_dir: String,
}

impl PathFilter {
    pub(crate) fn new(root: &Path, git_dir: &Path, common_dir: &Path) -> Self {
        Self {
            root: normalize(root),
            git_dir: normalize(git_dir),
            common_dir: normalize(common_dir),
        }
    }

    pub(crate) fn is_relevant(&self, path: &Path) -> bool {
        let n = normalize(path);
        if let Some(rel) = strip_dir(&n, &self.git_dir) {
            return git_dir_entry_relevant(rel, GIT_DIR_INTERESTING);
        }
        if self.common_dir != self.git_dir {
            if let Some(rel) = strip_dir(&n, &self.common_dir) {
                return git_dir_entry_relevant(rel, &["packed-refs", "refs"]);
            }
        }
        if let Some(rel) = strip_dir(&n, &self.root) {
            if rel.is_empty() {
                return false;
            }
            return !rel
                .split('/')
                .any(|c| c == ".git" || NOISE_DIRS.contains(&c));
        }
        false
    }
}

fn git_dir_entry_relevant(rel: &str, allowed: &[&str]) -> bool {
    let mut comps = rel.split('/').filter(|c| !c.is_empty());
    let Some(first) = comps.next() else {
        return false;
    };
    if !allowed.contains(&first) {
        return false;
    }
    // Lock files are transient; the final rename produces the interesting event.
    !rel.split('/').any(|c| c.ends_with(".lock"))
}

/// Normalise to forward slashes, strip the `\\?\` prefix, lowercase on Windows.
fn normalize(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_owned();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

/// If `path` is `dir` or inside it, the remainder (without leading slash).
fn strip_dir<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    if path == dir {
        return Some("");
    }
    let rest = path.strip_prefix(dir)?;
    rest.strip_prefix('/')
}

impl Repo {
    /// Watch `.git/HEAD`, `.git/index`, refs and the working tree, coalescing bursts of events
    /// into a single `()` after `debounce` of quiet. Events inside `.git/objects`, `target/`,
    /// `node_modules/` and similar are ignored. No polling: purely OS notifications.
    ///
    /// Dropping the [`WatchHandle`] stops everything and closes the receiver (it yields `None`).
    pub fn watch(
        &self,
        debounce: Duration,
    ) -> Result<(WatchHandle, UnboundedReceiver<()>)> {
        let (out_tx, out_rx) = unbounded::<()>();
        let (tx, rx) = mpsc::channel::<Msg>();

        let filter = PathFilter::new(&self.root, &self.git_dir, &self.common_dir);
        let handler_tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            if event.paths.iter().any(|p| filter.is_relevant(p)) {
                let _ = handler_tx.send(Msg::Change);
            }
        })?;

        watcher.watch(&self.root, RecursiveMode::Recursive)?;
        let git_inside_root = normalize_starts_with(&self.git_dir, &self.root);
        if !git_inside_root {
            watcher.watch(&self.git_dir, RecursiveMode::NonRecursive)?;
        }
        let refs = self.common_dir.join("refs");
        if !normalize_starts_with(&refs, &self.root) && refs.is_dir() {
            watcher.watch(&refs, RecursiveMode::Recursive)?;
        }
        if self.common_dir != self.git_dir && !normalize_starts_with(&self.common_dir, &self.root) {
            watcher.watch(&self.common_dir, RecursiveMode::NonRecursive)?;
        }

        thread::Builder::new()
            .name("git_svc-debounce".into())
            .spawn(move || debounce_loop(rx, out_tx, debounce))?;

        Ok((
            WatchHandle {
                watcher: Some(watcher),
                tx,
            },
            out_rx,
        ))
    }
}

fn normalize_starts_with(path: &Path, dir: &Path) -> bool {
    strip_dir(&normalize(path), &normalize(dir)).is_some()
}

fn debounce_loop(rx: mpsc::Receiver<Msg>, out: UnboundedSender<()>, debounce: Duration) {
    // Upper bound so a continuous stream of events still produces refreshes.
    let max_wait = debounce.saturating_mul(10).max(Duration::from_secs(1));
    loop {
        match rx.recv() {
            Ok(Msg::Change) => {}
            Ok(Msg::Stop) | Err(_) => return,
        }
        let first = Instant::now();
        loop {
            match rx.recv_timeout(debounce) {
                Ok(Msg::Change) => {
                    if first.elapsed() >= max_wait {
                        break;
                    }
                }
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        if out.unbounded_send(()).is_err() {
            return; // receiver dropped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn filter() -> PathFilter {
        PathFilter::new(
            &PathBuf::from("C:/w/proj"),
            &PathBuf::from("C:/w/proj/.git"),
            &PathBuf::from("C:/w/proj/.git"),
        )
    }

    #[test]
    fn git_dir_filtering() {
        let f = filter();
        assert!(f.is_relevant(Path::new("C:/w/proj/.git/index")));
        assert!(f.is_relevant(Path::new("C:\\w\\proj\\.git\\HEAD")));
        assert!(f.is_relevant(Path::new("C:/w/proj/.git/refs/heads/main")));
        assert!(f.is_relevant(Path::new("C:/w/proj/.git/packed-refs")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/.git/index.lock")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/.git/refs/heads/main.lock")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/.git/objects/ab/cdef")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/.git/logs/HEAD")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/.git")));
    }

    #[test]
    fn worktree_filtering() {
        let f = filter();
        assert!(f.is_relevant(Path::new("C:/w/proj/src/main.rs")));
        assert!(f.is_relevant(Path::new("C:/w/proj/README.md")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/target/debug/x.exe")));
        assert!(!f.is_relevant(Path::new("C:/w/proj/web/node_modules/a/b.js")));
        assert!(!f.is_relevant(Path::new("C:/w/other/file.txt")));
        assert!(!f.is_relevant(Path::new("C:/w/project2/file.txt")));
    }

    #[test]
    fn linked_worktree_common_refs() {
        let f = PathFilter::new(
            &PathBuf::from("C:/w/wt"),
            &PathBuf::from("C:/w/main/.git/worktrees/wt"),
            &PathBuf::from("C:/w/main/.git"),
        );
        assert!(f.is_relevant(Path::new("C:/w/main/.git/refs/heads/x")));
        assert!(f.is_relevant(Path::new("C:/w/main/.git/worktrees/wt/index")));
        assert!(!f.is_relevant(Path::new("C:/w/main/.git/objects/aa/bb")));
        assert!(!f.is_relevant(Path::new("C:/w/main/.git/config")));
    }
}
