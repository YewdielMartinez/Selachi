//! A file-system watcher over the agents' log directories that only *remembers* what changed.
//!
//! The callback runs on notify's own thread and does a few string comparisons and one hash-set
//! insert per event; it never touches the database, never wakes a UI thread and has no timers.
//! The host decides when (and whether) to ingest by calling [`LogWatcher::take_changes`]: a
//! window that is not focused simply never asks, and the set of changed paths keeps deduplicating
//! in the meantime. If too many distinct paths pile up, or the OS reports an error / overflow,
//! the watcher degrades to a single "everything may have changed" flag, which maps to one
//! [`crate::Ingestor::scan`].

use crate::ingest::IngestConfig;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// More distinct changed paths than this collapse into [`Changes::all`].
pub const MAX_PENDING_PATHS: usize = 4096;

/// What changed since the last [`LogWatcher::take_changes`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    /// A full scan is needed (initial state, overflow, watcher error).
    pub all: bool,
    /// Changed log files (at most the `max` asked for; the rest stays queued).
    pub paths: Vec<PathBuf>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.all && self.paths.is_empty()
    }
}

#[derive(Default)]
struct Shared {
    paths: Mutex<HashSet<PathBuf>>,
    all: AtomicBool,
    /// Cheap "anything pending?" check that avoids the lock.
    any: AtomicBool,
}

impl Shared {
    fn mark_all(&self) {
        self.all.store(true, Ordering::Release);
        self.any.store(true, Ordering::Release);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
        self.paths.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Keeps watching until dropped.
pub struct LogWatcher {
    shared: Arc<Shared>,
    _watcher: Option<RecommendedWatcher>,
}

impl LogWatcher {
    /// Watches every directory of `cfg` that exists. Starts with [`Changes::all`] set (nothing
    /// is known about the state of the logs yet). If no watch could be installed the flag simply
    /// stays set, so the host keeps working with full scans.
    pub fn start(cfg: &IngestConfig) -> LogWatcher {
        let shared = Arc::new(Shared::default());
        shared.mark_all();
        let cb_shared = Arc::clone(&shared);
        let cb_cfg = cfg.clone();
        let handler = move |res: notify::Result<notify::Event>| match res {
            Err(_) => cb_shared.mark_all(),
            Ok(ev) => {
                if matches!(ev.kind, EventKind::Access(_)) {
                    return;
                }
                let mut set = None;
                for p in &ev.paths {
                    if cb_cfg.classify(p).is_none() {
                        continue;
                    }
                    let set = set.get_or_insert_with(|| cb_shared.lock());
                    if set.len() >= MAX_PENDING_PATHS {
                        set.clear();
                        cb_shared.all.store(true, Ordering::Release);
                    } else {
                        set.insert(p.clone());
                    }
                    cb_shared.any.store(true, Ordering::Release);
                }
            }
        };
        let watcher = match notify::recommended_watcher(handler) {
            Ok(mut w) => {
                let mut watched = 0;
                for (dir, recursive) in cfg.watch_dirs() {
                    if !dir.is_dir() {
                        continue;
                    }
                    let mode = if recursive {
                        RecursiveMode::Recursive
                    } else {
                        RecursiveMode::NonRecursive
                    };
                    match w.watch(&dir, mode) {
                        Ok(()) => watched += 1,
                        Err(e) => log::warn!("usage: cannot watch {}: {e}", dir.display()),
                    }
                }
                (watched > 0).then_some(w)
            }
            Err(e) => {
                log::warn!("usage: file watcher unavailable: {e}");
                None
            }
        };
        LogWatcher {
            shared,
            _watcher: watcher,
        }
    }

    /// Is anything pending? Lock-free.
    pub fn has_changes(&self) -> bool {
        self.shared.any.load(Ordering::Acquire)
    }

    /// Takes the pending state: the `all` flag and up to `max` paths (sorted, so batches are
    /// deterministic). Paths beyond `max` stay queued.
    pub fn take_changes(&self, max: usize) -> Changes {
        let all = self.shared.all.swap(false, Ordering::AcqRel);
        let mut set = self.shared.lock();
        let mut paths: Vec<PathBuf> = if set.len() <= max {
            set.drain().collect()
        } else {
            let mut all_paths: Vec<PathBuf> = set.iter().cloned().collect();
            all_paths.sort();
            all_paths.truncate(max);
            for p in &all_paths {
                set.remove(p);
            }
            all_paths
        };
        paths.sort();
        if set.is_empty() && !self.shared.all.load(Ordering::Acquire) {
            self.shared.any.store(false, Ordering::Release);
        }
        Changes { all, paths }
    }

    /// Marks everything as possibly changed (e.g. after a failed scan).
    pub fn mark_all(&self) {
        self.shared.mark_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, Instant};

    fn wait_for(w: &LogWatcher, mut ok: impl FnMut(&LogWatcher) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if ok(w) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn marks_only_log_files_and_dedupes() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        std::fs::create_dir_all(claude.join("proj")).unwrap();
        let cfg = IngestConfig {
            claude_projects: Some(claude.clone()),
            ..Default::default()
        };
        let w = LogWatcher::start(&cfg);
        // Initial state: everything unknown.
        let first = w.take_changes(10);
        assert!(first.all && first.paths.is_empty());
        assert!(!w.has_changes());

        let log = claude.join("proj").join("s.jsonl");
        let noise = claude.join("proj").join("notes.txt");
        std::fs::write(&noise, "x").unwrap();
        for _ in 0..3 {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .unwrap();
            writeln!(f, "{{}}").unwrap();
        }
        assert!(wait_for(&w, LogWatcher::has_changes));
        std::thread::sleep(Duration::from_millis(300));
        let c = w.take_changes(10);
        assert!(!c.all);
        assert_eq!(c.paths, vec![log]);
        assert!(!w.has_changes());
    }

    #[test]
    fn take_changes_caps_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        std::fs::create_dir_all(&claude).unwrap();
        let cfg = IngestConfig {
            claude_projects: Some(claude.clone()),
            ..Default::default()
        };
        let w = LogWatcher::start(&cfg);
        w.take_changes(1);
        for i in 0..5 {
            std::fs::write(claude.join(format!("s{i}.jsonl")), "{}\n").unwrap();
        }
        assert!(wait_for(&w, |w| w.shared.lock().len() >= 5));
        let a = w.take_changes(2);
        assert_eq!(a.paths.len(), 2);
        assert!(w.has_changes());
        let b = w.take_changes(2);
        assert_eq!(b.paths.len(), 2);
        let c = w.take_changes(2);
        assert_eq!(c.paths.len(), 1);
        assert!(!w.has_changes());
    }
}
