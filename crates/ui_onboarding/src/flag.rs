//! The "first run done" flag.
//!
//! Stored in `state.db`'s `meta` table under [`store::db::ONBOARDING_DONE`]. Older builds wrote a
//! marker file, `onboarding.done`, next to the database: [`needs_onboarding`] migrates it (sets
//! the meta key and deletes the file) the first time it finds one. The marker file is still
//! written when there is no database at all, so a broken `state.db` never re-shows the wizard.

use std::path::{Path, PathBuf};

use store::StateDb;
use store::db::ONBOARDING_DONE;

/// Name of the legacy marker file.
pub const FLAG_FILE: &str = "onboarding.done";

/// Where the legacy marker lives for a data directory.
pub fn flag_path(dir: &Path) -> PathBuf {
    dir.join(FLAG_FILE)
}

/// Directory that holds `db`'s files.
pub fn data_dir_of(db: &StateDb) -> PathBuf {
    db.path().parent().map_or_else(PathBuf::new, Path::to_path_buf)
}

/// `true` until the wizard has been finished or skipped once in `dir` (marker file only).
pub fn needs_onboarding_in(dir: &Path) -> bool {
    !flag_path(dir).is_file()
}

fn write_marker(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(flag_path(dir), format!("selachi onboarding v1 done at {}\n", store::now_ms()))
}

/// Whether the first-run wizard must be shown before the main window. A legacy marker file in
/// the database's directory counts as "done" and is migrated into `meta`.
pub fn needs_onboarding(db: &StateDb) -> bool {
    if db.meta_get(ONBOARDING_DONE).ok().flatten().is_some() {
        return false;
    }
    let dir = data_dir_of(db);
    if flag_path(&dir).is_file() {
        if db.meta_set(ONBOARDING_DONE, "migrated").is_ok() {
            let _ = std::fs::remove_file(flag_path(&dir));
        }
        return false;
    }
    true
}

/// Records that the wizard is done: in `db` when there is one, else as the marker file in `dir`.
pub fn mark_done_with(db: Option<&StateDb>, dir: &Path) -> Result<(), String> {
    match db {
        Some(db) => db.meta_set(ONBOARDING_DONE, &format!("done at {}", store::now_ms())).map_err(|e| e.to_string()),
        None => write_marker(dir).map_err(|e| e.to_string()),
    }
}

/// Records that the wizard was finished or skipped. Failures are logged, never fatal.
pub fn mark_done(db: &StateDb) {
    if let Err(e) = mark_done_with(Some(db), &data_dir_of(db)) {
        log::warn!("onboarding: cannot record the first-run flag: {e}");
    }
}

/// Forgets the flag (tests and support: forces a first run).
pub fn reset(db: &StateDb) {
    if let Err(e) = db.meta_remove(ONBOARDING_DONE) {
        log::warn!("onboarding: cannot reset the first-run flag: {e}");
    }
    let _ = std::fs::remove_file(flag_path(&data_dir_of(db)));
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
        assert!(!flag_path(dir.path()).exists(), "no marker file any more");
        mark_done(&db); // idempotent
        assert!(!needs_onboarding(&db));
        reset(&db);
        assert!(needs_onboarding(&db));
        reset(&db); // already gone: fine
    }

    #[test]
    fn a_legacy_marker_file_is_migrated_into_meta() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        write_marker(dir.path()).unwrap();
        assert!(!needs_onboarding(&db));
        assert!(!flag_path(dir.path()).exists(), "marker removed after migrating");
        assert!(db.meta_get(ONBOARDING_DONE).unwrap().is_some());
        assert!(!needs_onboarding(&db));
    }

    #[test]
    fn flag_is_per_database() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let da = StateDb::open(&a.path().join("state.db")).unwrap();
        let db = StateDb::open(&b.path().join("state.db")).unwrap();
        mark_done(&da);
        assert!(!needs_onboarding(&da));
        assert!(needs_onboarding(&db));
    }

    #[test]
    fn without_a_database_the_marker_file_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        assert!(needs_onboarding_in(&nested));
        mark_done_with(None, &nested).unwrap();
        assert!(!needs_onboarding_in(&nested));
    }
}
