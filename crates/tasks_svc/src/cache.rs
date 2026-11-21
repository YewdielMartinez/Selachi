//! Scan cache keyed by the mtimes of every file and folder a scan consulted.

use crate::model::{DiscoverOptions, TaskDef};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// `None` = did not exist.
type Stamp = Option<(SystemTime, u64)>;

struct Entry {
    stamps: Vec<(PathBuf, Stamp)>,
    tasks: Arc<Vec<TaskDef>>,
}

/// Remembers the tasks of each project root until one of the files the scan
/// read changes (edited, created, deleted, or a consulted folder gained or lost
/// an entry). Validation is one `stat` per consulted path, no parsing.
#[derive(Default)]
pub struct TaskCache {
    entries: Mutex<HashMap<PathBuf, Entry>>,
    opts: DiscoverOptions,
}

impl std::fmt::Debug for TaskCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCache").finish_non_exhaustive()
    }
}

impl TaskCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(opts: DiscoverOptions) -> Self {
        Self {
            entries: Mutex::default(),
            opts,
        }
    }

    /// Cached tasks of `root`, rescanning only when something changed.
    /// Blocking (a few `stat`s on a hit, a scan on a miss): background thread.
    pub fn get_or_scan(&self, root: &Path) -> Arc<Vec<TaskDef>> {
        if let Some(hit) = self.lookup(root) {
            return hit;
        }
        let (tasks, deps) = crate::scan_project(root, &self.opts);
        let tasks = Arc::new(tasks);
        let stamps = deps
            .into_iter()
            .map(|p| {
                let st = stamp(&p);
                (p, st)
            })
            .collect();
        self.lock().insert(
            root.to_path_buf(),
            Entry {
                stamps,
                tasks: tasks.clone(),
            },
        );
        tasks
    }

    /// Drop one root (or everything with `None`).
    pub fn invalidate(&self, root: Option<&Path>) {
        let mut g = self.lock();
        match root {
            Some(r) => {
                g.remove(r);
            }
            None => g.clear(),
        }
    }

    fn lookup(&self, root: &Path) -> Option<Arc<Vec<TaskDef>>> {
        let g = self.lock();
        let e = g.get(root)?;
        e.stamps
            .iter()
            .all(|(p, st)| stamp(p) == *st)
            .then(|| e.tasks.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Entry>> {
        match self.entries.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }
}

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn cache() -> TaskCache {
        TaskCache::with_options(DiscoverOptions {
            use_just_cli: false,
        })
    }

    #[test]
    fn hit_returns_the_same_allocation() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"b"}}"#);
        let c = cache();
        let a = c.get_or_scan(d.path());
        let b = c.get_or_scan(d.path());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn editing_a_consulted_file_rescans() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"b"}}"#);
        let c = cache();
        let a = c.get_or_scan(d.path());
        // Different length: detected even with a coarse mtime clock.
        write(
            d.path(),
            "package.json",
            r#"{"scripts":{"a":"b","extra":"c"}}"#,
        );
        let b = c.get_or_scan(d.path());
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn a_new_file_appearing_rescans() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"b"}}"#);
        let c = cache();
        assert_eq!(c.get_or_scan(d.path()).len(), 1);
        write(d.path(), "Makefile", "all:\n\techo\n");
        let t = c.get_or_scan(d.path());
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn a_deleted_file_rescans() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"b"}}"#);
        write(d.path(), "Makefile", "all:\n\techo\n");
        let c = cache();
        assert_eq!(c.get_or_scan(d.path()).len(), 2);
        std::fs::remove_file(d.path().join("Makefile")).unwrap();
        assert_eq!(c.get_or_scan(d.path()).len(), 1);
    }

    #[test]
    fn invalidate_forces_a_rescan() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"b"}}"#);
        let c = cache();
        let a = c.get_or_scan(d.path());
        c.invalidate(Some(d.path()));
        assert!(!Arc::ptr_eq(&a, &c.get_or_scan(d.path())));
        c.invalidate(None);
        assert!(!Arc::ptr_eq(&a, &c.get_or_scan(d.path())));
    }
}
