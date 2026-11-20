//! Directory history with zoxide-style frecency.

use super::{now_ms, StateDb};
use crate::error::Result;
use crate::paths::{clean_display, normalize_key};
use rusqlite::params;

#[derive(Debug, Clone, PartialEq)]
pub struct PathEntry {
    pub path: String,
    pub visits: u32,
    /// Unix ms.
    pub last_visit: i64,
    /// Frecency (times the match bonus when a query was given).
    pub score: f64,
}

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// zoxide's scheme: visits weighted by how recently the directory was used.
pub fn frecency(visits: u32, last_visit: i64, now: i64) -> f64 {
    let age = (now - last_visit).max(0);
    let mult = if age < HOUR {
        4.0
    } else if age < DAY {
        2.0
    } else if age < WEEK {
        0.5
    } else {
        0.25
    };
    f64::from(visits) * mult
}

impl StateDb {
    /// Count a visit to `path` (queued; returns immediately).
    pub fn record_visit(&self, path: &str) -> Result<()> {
        self.record_visit_at(path, now_ms())
    }

    /// [`StateDb::record_visit`] with an explicit time (unix ms).
    pub fn record_visit_at(&self, path: &str, ts: i64) -> Result<()> {
        let display = clean_display(path);
        if display.is_empty() {
            return Ok(());
        }
        let key = normalize_key(&display);
        self.exec_async("record_visit", move |conn| {
            conn.prepare_cached(
                "INSERT INTO path_history(key, path, visits, last_visit) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(key) DO UPDATE SET visits = visits + 1,
                                                last_visit = max(last_visit, excluded.last_visit),
                                                path = excluded.path",
            )?
            .execute(params![key, display, ts])?;
            Ok(())
        })
    }

    /// Directories ranked by frecency. `query` is split on whitespace; every
    /// keyword must appear in the path, in order (case-insensitive substring), with
    /// a bonus when the last keyword hits the final folder name (x3 prefix, x2
    /// contained). If nothing matches that way, falls back to a fuzzy subsequence
    /// match at reduced weight. An empty query lists everything.
    pub fn frecent(&self, query: &str, limit: usize) -> Result<Vec<PathEntry>> {
        let now = now_ms();
        let rows: Vec<(String, String, u32, i64)> = self.read(|conn| {
            let mut stmt = conn.prepare_cached("SELECT key, path, visits, last_visit FROM path_history")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u32, r.get(3)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
        })?;

        let keywords: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let to_entry = |(_, path, visits, last): &(String, String, u32, i64), factor: f64| PathEntry {
            path: path.clone(),
            visits: *visits,
            last_visit: *last,
            score: frecency(*visits, *last, now) * factor,
        };

        let mut out: Vec<PathEntry> = if keywords.is_empty() {
            rows.iter().map(|r| to_entry(r, 1.0)).collect()
        } else {
            let strict: Vec<PathEntry> = rows
                .iter()
                .filter_map(|r| keyword_bonus(&r.0, &keywords).map(|f| to_entry(r, f)))
                .collect();
            if strict.is_empty() {
                let needle: String = keywords.concat();
                rows.iter()
                    .filter(|r| is_subsequence(&needle, &r.0))
                    .map(|r| to_entry(r, 0.2))
                    .collect()
            } else {
                strict
            }
        };
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.last_visit.cmp(&a.last_visit))
                .then_with(|| a.path.cmp(&b.path))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Drop a path from the history.
    pub fn forget_path(&self, path: &str) -> Result<()> {
        let key = normalize_key(path);
        self.exec_sync(move |conn| {
            conn.execute("DELETE FROM path_history WHERE key = ?1", params![key])?;
            Ok(())
        })
    }

    /// Delete entries whose directory no longer exists; returns how many.
    /// Touches the filesystem (a dead network drive can stall): call from a
    /// background thread.
    pub fn remove_missing_paths(&self) -> Result<usize> {
        let rows: Vec<(String, String)> = self.read(|conn| {
            let mut stmt = conn.prepare_cached("SELECT key, path FROM path_history")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
        })?;
        let missing: Vec<String> = rows
            .into_iter()
            .filter(|(_, p)| !std::path::Path::new(p).exists())
            .map(|(k, _)| k)
            .collect();
        let n = missing.len();
        if n > 0 {
            self.exec_sync(move |conn| {
                let mut stmt = conn.prepare_cached("DELETE FROM path_history WHERE key = ?1")?;
                for k in &missing {
                    stmt.execute(params![k])?;
                }
                Ok(())
            })?;
        }
        Ok(n)
    }
}

/// `Some(multiplier)` if all keywords occur in `key` in order.
fn keyword_bonus(key: &str, keywords: &[String]) -> Option<f64> {
    let mut pos = 0;
    for kw in keywords {
        let idx = key[pos..].find(kw.as_str())?;
        pos += idx + kw.len();
    }
    let last = keywords.last()?;
    let leaf = key.trim_end_matches('/').rsplit('/').next().unwrap_or(key);
    Some(if leaf.starts_with(last.as_str()) {
        3.0
    } else if leaf.contains(last.as_str()) {
        2.0
    } else {
        1.0
    })
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut it = haystack.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;

    #[test]
    fn frecency_buckets() {
        let now = 10 * WEEK;
        assert_eq!(frecency(2, now - 10, now), 8.0);
        assert_eq!(frecency(2, now - 2 * HOUR, now), 4.0);
        assert_eq!(frecency(2, now - 2 * DAY, now), 1.0);
        assert_eq!(frecency(2, now - 3 * WEEK, now), 0.5);
    }

    #[test]
    fn visits_accumulate_and_normalize() {
        let (_d, db) = temp_db();
        db.record_visit("C:\\Users\\Me\\Proj").unwrap();
        db.record_visit("c:/users/me/proj/").unwrap();
        db.record_visit("C:\\Users\\Me\\Proj\\").unwrap();
        db.flush().unwrap();
        let r = db.frecent("", 10).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].visits, 3);
    }

    #[test]
    fn frecency_ordering() {
        let (_d, db) = temp_db();
        let now = now_ms();
        // old but very frequent: 20 * 0.25 = 5
        for i in 0..20 {
            db.record_visit_at("C:\\code\\legacy", now - 30 * DAY + i).unwrap();
        }
        // recent, few visits: 2 * 4 = 8
        db.record_visit_at("C:\\code\\fresh", now - 1000).unwrap();
        db.record_visit_at("C:\\code\\fresh", now - 500).unwrap();
        // yesterday-ish, once: 1 * 2 = 2
        db.record_visit_at("C:\\code\\mid", now - 5 * HOUR).unwrap();
        db.flush().unwrap();
        let paths: Vec<String> = db.frecent("", 10).unwrap().into_iter().map(|e| e.path).collect();
        assert_eq!(paths, ["C:\\code\\fresh", "C:\\code\\legacy", "C:\\code\\mid"]);
        assert_eq!(db.frecent("", 2).unwrap().len(), 2);
    }

    #[test]
    fn query_matching() {
        let (_d, db) = temp_db();
        let now = now_ms();
        for (p, n) in [
            ("C:\\work\\selachi", 3),
            ("C:\\work\\selachi\\crates\\store", 1),
            ("C:\\play\\other", 5),
            ("C:\\work\\shell-tools", 2),
        ] {
            for i in 0..n {
                db.record_visit_at(p, now - i).unwrap();
            }
        }
        db.flush().unwrap();
        let top = |q: &str| db.frecent(q, 5).unwrap().into_iter().map(|e| e.path).collect::<Vec<_>>();
        assert_eq!(top("sela")[0], "C:\\work\\selachi");
        assert_eq!(top("selachi store"), ["C:\\work\\selachi\\crates\\store"]);
        assert_eq!(top("WORK")[0], "C:\\work\\selachi");
        // Keywords out of order do not match strictly (and are not a subsequence either).
        assert!(top("store selachi").is_empty());
        // Fuzzy fallback: subsequence only.
        assert_eq!(top("slch")[0], "C:\\work\\selachi");
        assert!(top("qqq").is_empty());
    }

    #[test]
    fn forget_and_remove_missing() {
        let (dir, db) = temp_db();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        db.record_visit(real.to_str().unwrap()).unwrap();
        db.record_visit(dir.path().join("gone").to_str().unwrap()).unwrap();
        db.record_visit("C:\\zzz\\nope").unwrap();
        db.flush().unwrap();
        assert_eq!(db.remove_missing_paths().unwrap(), 2);
        assert_eq!(db.frecent("", 10).unwrap().len(), 1);
        db.forget_path(real.to_str().unwrap()).unwrap();
        assert!(db.frecent("", 10).unwrap().is_empty());
    }
}
