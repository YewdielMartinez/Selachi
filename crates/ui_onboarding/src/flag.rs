//! The "first run done" flag.
//!
//! `state.db` has no generic key/value table, so the flag is a small marker file next to it,
//! `%APPDATA%\Selachi\onboarding.done` (the directory of the `StateDb` that is passed in, which
//! honours `SELACHI_DATA_DIR`). Preferred store change: a `meta(key, value)` table, see the report.

use std::path::{Path, PathBuf};

use store::StateDb;

/// Name of the marker file.
pub const FLAG_FILE: &str = "onboarding.done";

/// Where the flag lives for a data directory.
pub fn flag_path(dir: &Path) -> PathBuf {
    dir.join(FLAG_FILE)
}

/// Directory that holds `db`'s files.
pub fn data_dir_of(db: &StateDb) -> PathBuf {
    db.path().parent().map_or_else(PathBuf::new, Path::to_path_buf)
}

/// `true` until the wizard has been finished or skipped once in `dir`.
pub fn needs_onboarding_in(dir: &Path) -> bool {
    !flag_path(dir).is_file()
}

/// Records that the wizard is done in `dir` (idempotent).
pub fn mark_done_in(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let stamp = store::now_ms();
    std::fs::write(flag_path(dir), format!("selachi onboarding v1 done at {stamp}\n"))
}

/// Forgets the flag (the settings screen's "Volver a configurar" does not need this: the wizard
/// can always be opened by hand; it exists so tests and support can force a first run).
pub fn reset_in(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(flag_path(dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whether the first-run wizard must be shown before the main window.
pub fn needs_onboarding(db: &StateDb) -> bool {
    needs_onboarding_in(&data_dir_of(db))
}

/// Records that the wizard was finished or skipped. Failures are logged, never fatal.
pub fn mark_done(db: &StateDb) {
    if let Err(e) = mark_done_in(&data_dir_of(db)) {
        log::warn!("onboarding: cannot write {FLAG_FILE}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_flag_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        assert!(needs_onboarding(&db), "fresh data dir");
        mark_done(&db);
        assert!(!needs_onboarding(&db));
        assert!(flag_path(dir.path()).is_file());
        mark_done(&db); // idempotent
        assert!(!needs_onboarding(&db));
        reset_in(dir.path()).unwrap();
        assert!(needs_onboarding(&db));
        reset_in(dir.path()).unwrap(); // already gone: fine
    }

    #[test]
    fn flag_is_per_data_dir() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        mark_done_in(a.path()).unwrap();
        assert!(!needs_onboarding_in(a.path()));
        assert!(needs_onboarding_in(b.path()));
    }

    #[test]
    fn creates_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        mark_done_in(&nested).unwrap();
        assert!(!needs_onboarding_in(&nested));
    }
}
