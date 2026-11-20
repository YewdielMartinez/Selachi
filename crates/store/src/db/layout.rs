//! Session layout (tabs, splits, cwds) restored on launch. Opaque JSON owned by
//! the app: it must contain no scrollback and no secrets.

use super::{now_ms, StateDb};
use crate::error::Result;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLayout {
    pub json: String,
    /// Unix ms when saved.
    pub ts: i64,
}

impl StateDb {
    /// Replace the saved layout (queued; cheap enough to call on every tab change).
    pub fn save_session_layout(&self, json: &str) -> Result<()> {
        let json = json.to_string();
        let ts = now_ms();
        self.exec_async("save_session_layout", move |conn| {
            conn.prepare_cached(
                "INSERT INTO session_layout(id, json, ts) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET json = excluded.json, ts = excluded.ts",
            )?
            .execute(params![json, ts])?;
            Ok(())
        })
    }

    pub fn load_session_layout(&self) -> Result<Option<SessionLayout>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT json, ts FROM session_layout WHERE id = 1",
                    [],
                    |r| {
                        Ok(SessionLayout {
                            json: r.get(0)?,
                            ts: r.get(1)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    pub fn clear_session_layout(&self) -> Result<()> {
        self.exec_sync(|conn| {
            conn.execute("DELETE FROM session_layout", [])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::db::temp_db;

    #[test]
    fn save_replace_load_clear() {
        let (_d, db) = temp_db();
        assert!(db.load_session_layout().unwrap().is_none());
        db.save_session_layout("{\"tabs\":[1]}").unwrap();
        db.save_session_layout("{\"tabs\":[1,2]}").unwrap();
        db.flush().unwrap();
        let l = db.load_session_layout().unwrap().unwrap();
        assert_eq!(l.json, "{\"tabs\":[1,2]}");
        assert!(l.ts > 0);
        db.clear_session_layout().unwrap();
        assert!(db.load_session_layout().unwrap().is_none());
    }
}
