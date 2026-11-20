//! SQLite storage: connection setup (WAL), versioned migrations, row writers.

use crate::model::{normalize_path, FileTouch, RateLimitSnapshot, UsageEvent};
use crate::pricing::PricingTable;
use anyhow::{Context, Result};
use chrono::Weekday;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::time::Duration;

/// Ordered migrations; `PRAGMA user_version` holds the number applied.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
CREATE TABLE usage_events(
    id              INTEGER PRIMARY KEY,
    agent           TEXT    NOT NULL,
    session_id      TEXT    NOT NULL,
    ts              INTEGER NOT NULL,           -- unix ms (UTC)
    model           TEXT    NOT NULL,
    input           INTEGER NOT NULL DEFAULT 0, -- non-cached input
    output          INTEGER NOT NULL DEFAULT 0, -- includes reasoning/thinking
    cache_create    INTEGER NOT NULL DEFAULT 0,
    cache_create_1h INTEGER NOT NULL DEFAULT 0, -- part of cache_create written with 1h TTL
    cache_read      INTEGER NOT NULL DEFAULT 0,
    cwd             TEXT,
    cwd_norm        TEXT,                       -- lowercase, '/' separators (prefix queries)
    git_branch      TEXT,
    msg_key         TEXT    NOT NULL UNIQUE
);
CREATE INDEX idx_usage_ts      ON usage_events(ts);
CREATE INDEX idx_usage_agent   ON usage_events(agent, ts);
CREATE INDEX idx_usage_session ON usage_events(session_id);
CREATE INDEX idx_usage_cwd     ON usage_events(cwd_norm);

CREATE TABLE file_touches(
    id         INTEGER PRIMARY KEY,
    agent      TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    ts         INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    tool       TEXT    NOT NULL,
    touch_key  TEXT    NOT NULL UNIQUE
);
CREATE INDEX idx_touch_session ON file_touches(session_id, ts);

CREATE TABLE ingest_offsets(
    path       TEXT PRIMARY KEY,
    agent      TEXT    NOT NULL,
    size       INTEGER NOT NULL,
    mtime_ms   INTEGER NOT NULL,
    byte_offset INTEGER NOT NULL,
    ctx        TEXT,                            -- parser state (JSON), e.g. Codex session context
    updated_ms INTEGER NOT NULL
);

CREATE TABLE rate_limit_snapshots(
    id           INTEGER PRIMARY KEY,
    agent        TEXT    NOT NULL,
    ts           INTEGER NOT NULL,
    window       TEXT    NOT NULL,
    used_percent REAL    NOT NULL,
    resets_at    INTEGER,                       -- unix ms
    source       TEXT    NOT NULL,
    UNIQUE(agent, ts, window, source)
);
CREATE INDEX idx_rl_lookup ON rate_limit_snapshots(agent, window, ts);

CREATE TABLE statusline_snapshots(
    id                  INTEGER PRIMARY KEY,
    ts                  INTEGER NOT NULL,
    session_id          TEXT,
    model               TEXT,
    cost_usd            REAL,
    context_used_percent REAL,
    context_window_size INTEGER
);
CREATE INDEX idx_sl_session ON statusline_snapshots(session_id, ts);
"#,
];

/// Offsets row for one source file.
#[derive(Debug, Clone)]
pub struct OffsetRow {
    pub size: i64,
    pub mtime_ms: i64,
    pub byte_offset: i64,
    pub ctx: Option<String>,
}

/// Outcome of writing one usage event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Inserted,
    /// An existing row for the same `msg_key` was replaced by a later streamed chunk.
    Updated,
    /// Identical or earlier chunk of an already-counted message.
    Duplicate,
}

/// Handle to the database plus query-time settings (pricing, week start).
pub struct Store {
    pub(crate) conn: Connection,
    pub(crate) pricing: PricingTable,
    pub(crate) week_start: Weekday,
}

impl Store {
    /// Open (creating if needed) the database at `path`, enable WAL, run migrations.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.busy_timeout(Duration::from_secs(10))?;
        // journal_mode returns a row, so query it.
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Store {
            conn,
            pricing: PricingTable::embedded(),
            week_start: Weekday::Mon,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: usize =
            self.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        if current > MIGRATIONS.len() {
            anyhow::bail!(
                "database schema v{current} is newer than this build (v{})",
                MIGRATIONS.len()
            );
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)
                .with_context(|| format!("migration v{}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Replace the pricing table used for cost estimates.
    pub fn set_pricing(&mut self, p: PricingTable) {
        self.pricing = p;
    }

    pub fn pricing(&self) -> &PricingTable {
        &self.pricing
    }

    /// First day of the week for `Range::ThisWeek` (default Monday).
    pub fn set_week_start(&mut self, w: Weekday) {
        self.week_start = w;
    }

    pub fn week_start(&self) -> Weekday {
        self.week_start
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---- writers (callers wrap batches in a transaction) ----

    /// Insert a usage event, or replace an existing row of the same `msg_key` when the new chunk
    /// has strictly more output tokens (streamed chunks grow monotonically; keep the final one).
    pub fn upsert_usage(&self, e: &UsageEvent) -> Result<UpsertOutcome> {
        let existing: Option<i64> = self
            .conn
            .prepare_cached("SELECT output FROM usage_events WHERE msg_key = ?1")?
            .query_row([&e.msg_key], |r| r.get(0))
            .optional()?;
        let t = &e.tokens;
        let cwd_norm = e.cwd.as_deref().map(normalize_path);
        match existing {
            None => {
                self.conn
                    .prepare_cached(
                        "INSERT INTO usage_events(agent, session_id, ts, model, input, output, \
                         cache_create, cache_create_1h, cache_read, cwd, cwd_norm, git_branch, msg_key) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    )?
                    .execute(params![
                        e.agent.as_str(),
                        e.session_id,
                        e.ts_ms,
                        e.model,
                        t.input as i64,
                        t.output as i64,
                        t.cache_create as i64,
                        t.cache_create_1h as i64,
                        t.cache_read as i64,
                        e.cwd,
                        cwd_norm,
                        e.git_branch,
                        e.msg_key
                    ])?;
                Ok(UpsertOutcome::Inserted)
            }
            Some(old_out) if (t.output as i64) > old_out => {
                self.conn
                    .prepare_cached(
                        "UPDATE usage_events SET ts=?2, model=?3, input=?4, output=?5, cache_create=?6, \
                         cache_create_1h=?7, cache_read=?8, cwd=?9, cwd_norm=?10, git_branch=?11 \
                         WHERE msg_key=?1",
                    )?
                    .execute(params![
                        e.msg_key,
                        e.ts_ms,
                        e.model,
                        t.input as i64,
                        t.output as i64,
                        t.cache_create as i64,
                        t.cache_create_1h as i64,
                        t.cache_read as i64,
                        e.cwd,
                        cwd_norm,
                        e.git_branch
                    ])?;
                Ok(UpsertOutcome::Updated)
            }
            Some(_) => Ok(UpsertOutcome::Duplicate),
        }
    }

    /// Returns true if a new row was inserted.
    pub fn insert_touch(&self, t: &FileTouch) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached(
                "INSERT OR IGNORE INTO file_touches(agent, session_id, ts, path, tool, touch_key) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )?
            .execute(params![
                t.agent.as_str(),
                t.session_id,
                t.ts_ms,
                t.path,
                t.tool,
                t.touch_key
            ])?;
        Ok(n > 0)
    }

    /// Returns true if a new row was inserted.
    pub fn insert_rate_limit(&self, s: &RateLimitSnapshot) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached(
                "INSERT OR IGNORE INTO rate_limit_snapshots(agent, ts, window, used_percent, resets_at, source) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )?
            .execute(params![
                s.agent.as_str(),
                s.ts_ms,
                s.window,
                s.used_percent,
                s.resets_at_ms,
                s.source
            ])?;
        Ok(n > 0)
    }

    pub fn get_offset(&self, path: &str) -> Result<Option<OffsetRow>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT size, mtime_ms, byte_offset, ctx FROM ingest_offsets WHERE path = ?1",
            )?
            .query_row([path], |r| {
                Ok(OffsetRow {
                    size: r.get(0)?,
                    mtime_ms: r.get(1)?,
                    byte_offset: r.get(2)?,
                    ctx: r.get(3)?,
                })
            })
            .optional()?)
    }

    pub fn put_offset(
        &self,
        path: &str,
        agent: crate::model::Agent,
        row: &OffsetRow,
        now_ms: i64,
    ) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO ingest_offsets(path, agent, size, mtime_ms, byte_offset, ctx, updated_ms) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7) \
                 ON CONFLICT(path) DO UPDATE SET agent=?2, size=?3, mtime_ms=?4, byte_offset=?5, ctx=?6, updated_ms=?7",
            )?
            .execute(params![
                path,
                agent.as_str(),
                row.size,
                row.mtime_ms,
                row.byte_offset,
                row.ctx,
                now_ms
            ])?;
        Ok(())
    }
}
