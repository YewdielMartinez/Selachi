//! Pinned directories shown in the sidebar, in a user-defined order.

use super::StateDb;
use crate::error::Result;
use crate::paths::{clean_display, normalize_key};
use rusqlite::params;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favorite {
    pub path: String,
    pub label: String,
    pub order: i64,
}

impl StateDb {
    /// Add (or relabel) a favorite. New favorites go to the end. An empty label
    /// defaults to the folder name.
    pub fn add_favorite(&self, path: &str, label: &str) -> Result<()> {
        let display = clean_display(path);
        let key = normalize_key(&display);
        let label = if label.trim().is_empty() {
            folder_name(&display)
        } else {
            label.trim().to_string()
        };
        self.exec_sync(move |conn| {
            conn.execute(
                "INSERT INTO favorites(key, path, label, ord)
                 VALUES (?1, ?2, ?3, (SELECT coalesce(max(ord), -1) + 1 FROM favorites))
                 ON CONFLICT(key) DO UPDATE SET label = excluded.label, path = excluded.path",
                params![key, display, label],
            )?;
            Ok(())
        })
    }

    pub fn remove_favorite(&self, path: &str) -> Result<()> {
        let key = normalize_key(path);
        self.exec_sync(move |conn| {
            conn.execute("DELETE FROM favorites WHERE key = ?1", params![key])?;
            Ok(())
        })
    }

    /// Favorites in display order.
    pub fn favorites(&self) -> Result<Vec<Favorite>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare_cached("SELECT path, label, ord FROM favorites ORDER BY ord, key")?;
            let rows = stmt.query_map([], |r| {
                Ok(Favorite {
                    path: r.get(0)?,
                    label: r.get(1)?,
                    order: r.get(2)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
    }

    /// Re-order: `paths` lists favorites in the new order; any not listed keep
    /// their relative order after the listed ones.
    pub fn reorder_favorites(&self, paths: &[String]) -> Result<()> {
        let keys: Vec<String> = paths.iter().map(|p| normalize_key(p)).collect();
        self.exec_sync(move |conn| {
            let mut rest: Vec<String> = {
                let mut stmt = conn.prepare("SELECT key FROM favorites ORDER BY ord, key")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            rest.retain(|k| !keys.contains(k));
            let mut upd = conn.prepare("UPDATE favorites SET ord = ?1 WHERE key = ?2")?;
            for (n, k) in keys.iter().chain(rest.iter()).enumerate() {
                upd.execute(params![n as i64, k])?;
            }
            Ok(())
        })
    }
}

fn folder_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use crate::db::temp_db;

    #[test]
    fn add_order_relabel_remove() {
        let (_d, db) = temp_db();
        db.add_favorite("C:\\a\\one", "").unwrap();
        db.add_favorite("C:\\a\\two", "Second").unwrap();
        db.add_favorite("C:\\a\\three\\", "").unwrap();
        let labels = |db: &crate::StateDb| -> Vec<String> {
            db.favorites()
                .unwrap()
                .into_iter()
                .map(|f| f.label)
                .collect()
        };
        assert_eq!(labels(&db), ["one", "Second", "three"]);
        db.add_favorite("c:/a/one", "Uno").unwrap(); // same key: relabel, keep position
        assert_eq!(labels(&db), ["Uno", "Second", "three"]);

        db.reorder_favorites(&["C:\\a\\three".to_string()]).unwrap();
        assert_eq!(labels(&db), ["three", "Uno", "Second"]);
        db.remove_favorite("C:\\A\\TWO").unwrap();
        assert_eq!(labels(&db), ["three", "Uno"]);
    }
}
