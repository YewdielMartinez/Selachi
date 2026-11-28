//! Small key/value state of the app (`meta` table): the first-run flag and similar one-off
//! markers. Not a settings store: user preferences live in `config.toml`.

use super::StateDb;
use crate::error::Result;
use rusqlite::{params, OptionalExtension};

/// `meta` key of the first-run wizard: set once it was finished or skipped.
pub const ONBOARDING_DONE: &str = "onboarding.done";

/// `meta` key of "Novedades": the version whose release notes the user already saw or dismissed.
pub const WHATS_NEW_SEEN: &str = "whatsnew.seen_version";
/// `meta` key: the version that was running when an update was handed to the installer. At the next
/// start, a running version newer than this means "just updated": "Novedades" shows.
pub const UPDATE_FROM: &str = "update.installed_from";
/// `meta` key: JSON with the release notes of the update being installed (so "Novedades" works offline).
pub const UPDATE_NOTES: &str = "update.pending_notes";

/// `meta` key: the project that new terminals and chats open in (its folder, display form).
/// Absent = no project selected (they open in the active tab's folder).
pub const SELECTED_PROJECT: &str = "projects.selected";

impl StateDb {
    /// The value stored under `key`, if any.
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        self.read(move |conn| {
            Ok(conn
                .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?)
        })
    }

    /// Store `value` under `key` (replacing). Blocks until committed.
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.exec_sync(move |conn| {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }

    /// Remove `key` (no-op when absent).
    pub fn meta_remove(&self, key: &str) -> Result<()> {
        let key = key.to_string();
        self.exec_sync(move |conn| {
            conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::db::temp_db;

    #[test]
    fn get_set_replace_remove() {
        let (_d, db) = temp_db();
        assert_eq!(db.meta_get("a").unwrap(), None);
        db.meta_set("a", "1").unwrap();
        assert_eq!(db.meta_get("a").unwrap().as_deref(), Some("1"));
        db.meta_set("a", "2").unwrap();
        assert_eq!(db.meta_get("a").unwrap().as_deref(), Some("2"));
        db.meta_remove("a").unwrap();
        db.meta_remove("a").unwrap();
        assert_eq!(db.meta_get("a").unwrap(), None);
    }

    #[test]
    fn the_selected_project_survives_a_reopen_and_a_removed_favorite_does_not_touch_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let db = crate::StateDb::open(&path).unwrap();
            db.add_favorite(r"C:\dev\Tron", "").unwrap();
            db.meta_set(super::SELECTED_PROJECT, r"C:\dev\Tron")
                .unwrap();
            db.remove_favorite(r"c:\DEV\tron").unwrap();
            assert!(db.favorites().unwrap().is_empty());
        }
        let db = crate::StateDb::open(&path).unwrap();
        assert_eq!(
            db.meta_get(super::SELECTED_PROJECT).unwrap().as_deref(),
            Some(r"C:\dev\Tron")
        );
    }

    #[test]
    fn migrating_a_v2_database_adds_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let mut conn = rusqlite::Connection::open(&path).unwrap();
            // Apply only the first two migrations, like an install from before this change.
            let n = crate::db::latest_version();
            assert!(n >= 3);
            crate::db::migrate(&mut conn).unwrap();
            conn.execute_batch("DROP TABLE meta; PRAGMA user_version = 2")
                .unwrap();
        }
        let db = crate::StateDb::open(&path).unwrap();
        db.meta_set("k", "v").unwrap();
        assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("v"));
    }
}
