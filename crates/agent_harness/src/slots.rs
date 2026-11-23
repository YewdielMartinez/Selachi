//! Concurrency limit across processes: at most N delegated runs in flight.
//!
//! One lock file per slot in `<data dir>\agent-locks`, opened with `share_mode(0)` (exclusive).
//! The kernel releases the handle when the owner exits or crashes, so a dead run never leaks a
//! slot and there is no stale-lock cleanup. Nothing polls: a run either gets a slot now or is refused.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;
const ERROR_ACCESS_DENIED: i32 = 5;

/// A held slot. Dropping it frees the slot.
#[derive(Debug)]
pub struct SlotGuard {
    _file: File,
    index: usize,
    path: PathBuf,
}

impl SlotGuard {
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Directory that holds the slot files.
pub fn lock_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-locks")
}

/// Take a free slot out of `max`. `Ok(None)`: all of them are held by live runs.
pub fn acquire(data_dir: &Path, max: usize) -> std::io::Result<Option<SlotGuard>> {
    let dir = lock_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    for index in 0..max {
        let path = dir.join(format!("slot-{index}.lock"));
        match OpenOptions::new().read(true).write(true).create(true).truncate(true).share_mode(0).open(&path) {
            Ok(mut file) => {
                // Diagnostics only: who holds it.
                let _ = write!(file, "{}", std::process::id());
                return Ok(Some(SlotGuard { _file: file, index, path }));
            }
            Err(e) if matches!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_ACCESS_DENIED)) => {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// How many of the `max` slots are held right now (opens and releases each one briefly).
pub fn in_use(data_dir: &Path, max: usize) -> usize {
    let dir = lock_dir(data_dir);
    (0..max)
        .filter(|index| {
            let path = dir.join(format!("slot-{index}.lock"));
            if !path.exists() {
                return false;
            }
            matches!(
                OpenOptions::new().read(true).share_mode(0).open(&path),
                Err(e) if matches!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_ACCESS_DENIED))
            )
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_out_exactly_max_slots_and_frees_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let a = acquire(dir.path(), 3).unwrap().unwrap();
        let b = acquire(dir.path(), 3).unwrap().unwrap();
        let c = acquire(dir.path(), 3).unwrap().unwrap();
        let mut idx = [a.index(), b.index(), c.index()];
        idx.sort_unstable();
        assert_eq!(idx, [0, 1, 2]);
        assert!(acquire(dir.path(), 3).unwrap().is_none(), "the fourth run must be refused");
        assert_eq!(in_use(dir.path(), 3), 3);
        drop(b);
        assert_eq!(in_use(dir.path(), 3), 2);
        let again = acquire(dir.path(), 3).unwrap().unwrap();
        assert_eq!(again.index(), 1);
    }

    #[test]
    fn lowering_the_limit_only_counts_the_first_slots() {
        let dir = tempfile::tempdir().unwrap();
        let _a = acquire(dir.path(), 1).unwrap().unwrap();
        assert!(acquire(dir.path(), 1).unwrap().is_none());
    }
}
