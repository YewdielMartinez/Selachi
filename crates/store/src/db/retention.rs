//! Retention: age and row-count limits for command history. Paths, projects,
//! favorites and the layout are kept indefinitely. `usage_raw_days` belongs to
//! `agents_svc`'s database and is not applied here.

use super::StateDb;
use crate::config::RetentionConfig;
use crate::error::Result;
use rusqlite::params;

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneStats {
    pub history_by_age: usize,
    pub history_by_cap: usize,
}

impl StateDb {
    /// Delete history older than `history_days` and trim to `history_max_rows`
    /// (oldest first). `now` is unix ms. A value of 0 disables that limit.
    pub fn prune(&self, now: i64, cfg: &RetentionConfig) -> Result<PruneStats> {
        let days = cfg.history_days;
        let max_rows = cfg.history_max_rows;
        self.exec_sync(move |conn| {
            let mut stats = PruneStats::default();
            if days > 0 {
                let cutoff = now - i64::from(days) * DAY_MS;
                stats.history_by_age =
                    conn.execute("DELETE FROM command_history WHERE ts < ?1", params![cutoff])?;
            }
            if max_rows > 0 {
                stats.history_by_cap = conn.execute(
                    "DELETE FROM command_history WHERE id IN (
                         SELECT id FROM command_history ORDER BY ts DESC, id DESC LIMIT -1 OFFSET ?1)",
                    params![i64::from(max_rows)],
                )?;
            }
            if stats.history_by_age + stats.history_by_cap > 0 {
                // Keep the FTS b-tree compact after bulk deletes.
                conn.execute("INSERT INTO command_fts(command_fts) VALUES ('optimize')", [])?;
            }
            Ok(stats)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{temp_db, NewCommand};

    fn add(db: &StateDb, c: &str, ts: i64) {
        db.record_command(NewCommand {
            ts: Some(ts),
            ..NewCommand::new(c, "C:\\a")
        })
        .unwrap();
    }

    #[test]
    fn prune_by_age_and_cap() {
        let (_d, db) = temp_db();
        let now = 1_000 * DAY_MS;
        add(&db, "ancient one", now - 200 * DAY_MS);
        add(&db, "ancient two", now - 91 * DAY_MS);
        add(&db, "kept a", now - 89 * DAY_MS);
        add(&db, "kept b", now - 2 * DAY_MS);
        add(&db, "kept c", now - DAY_MS);
        db.flush().unwrap();

        let cfg = RetentionConfig {
            history_days: 90,
            usage_raw_days: 30,
            history_max_rows: 0,
        };
        let s = db.prune(now, &cfg).unwrap();
        assert_eq!(
            s,
            PruneStats {
                history_by_age: 2,
                history_by_cap: 0
            }
        );
        assert!(db.search("ancient", None, 10).unwrap().is_empty()); // FTS cleaned too
        assert_eq!(db.search("", None, 10).unwrap().len(), 3);

        let cfg = RetentionConfig {
            history_max_rows: 2,
            ..cfg
        };
        let s = db.prune(now, &cfg).unwrap();
        assert_eq!(
            s,
            PruneStats {
                history_by_age: 0,
                history_by_cap: 1
            }
        );
        let left: Vec<String> = db
            .search("", None, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.command)
            .collect();
        assert_eq!(left.len(), 2);
        assert!(!left.contains(&"kept a".to_string()));

        // 0 = keep forever; other tables untouched.
        db.record_visit("C:\\keep").unwrap();
        let off = RetentionConfig {
            history_days: 0,
            usage_raw_days: 0,
            history_max_rows: 0,
        };
        assert_eq!(db.prune(now, &off).unwrap(), PruneStats::default());
        assert_eq!(db.frecent("", 5).unwrap().len(), 1);
    }
}
