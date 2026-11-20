//! Versioned migrations for `state.db`. `PRAGMA user_version` holds the number
//! applied. This file is a different database from `agents_svc`'s `selachi.db`,
//! so the two migration counters never interact.

use crate::error::{Result, StoreError};
use rusqlite::Connection;

/// Append-only. Never edit a released entry; add a new one.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
CREATE TABLE command_history(
    id          INTEGER PRIMARY KEY,
    ts          INTEGER NOT NULL,            -- unix ms (UTC)
    cwd         TEXT    NOT NULL,
    cwd_key     TEXT    NOT NULL,            -- normalized cwd (lowercase, '/'), for comparisons
    command     TEXT    NOT NULL,
    exit_code   INTEGER,
    duration_ms INTEGER,
    shell       TEXT,
    session_id  TEXT
);
CREATE INDEX idx_hist_ts      ON command_history(ts);
CREATE INDEX idx_hist_session ON command_history(session_id, id);

-- Token-prefix search over the command text (external content: no duplicate storage).
CREATE VIRTUAL TABLE command_fts USING fts5(
    command,
    content='command_history',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2',
    prefix='2 3'
);
CREATE TRIGGER command_history_ai AFTER INSERT ON command_history BEGIN
    INSERT INTO command_fts(rowid, command) VALUES (new.id, new.command);
END;
CREATE TRIGGER command_history_ad AFTER DELETE ON command_history BEGIN
    INSERT INTO command_fts(command_fts, rowid, command) VALUES ('delete', old.id, old.command);
END;

CREATE TABLE path_history(
    key        TEXT PRIMARY KEY,             -- normalized path
    path       TEXT    NOT NULL,             -- display form
    visits     INTEGER NOT NULL DEFAULT 1,
    last_visit INTEGER NOT NULL              -- unix ms
);

CREATE TABLE projects(
    id          INTEGER PRIMARY KEY,
    key         TEXT    NOT NULL UNIQUE,     -- normalized path
    path        TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL DEFAULT 'other',  -- git|node|rust|dotnet|python|other
    pinned      INTEGER NOT NULL DEFAULT 0,
    last_opened INTEGER                      -- unix ms
);

CREATE TABLE favorites(
    key   TEXT PRIMARY KEY,
    path  TEXT    NOT NULL,
    label TEXT    NOT NULL,
    ord   INTEGER NOT NULL
);

-- Single row: the tab/split layout to restore on launch. Never scrollback or secrets.
CREATE TABLE session_layout(
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT    NOT NULL,
    ts   INTEGER NOT NULL
);
"#,
];

/// Latest schema version this build knows.
pub fn latest_version() -> u32 {
    MIGRATIONS.len() as u32
}

/// Apply pending migrations; returns the resulting version. Idempotent.
pub fn migrate(conn: &mut Connection) -> Result<u32> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > latest_version() {
        return Err(StoreError::Invalid(format!(
            "state.db is schema v{current}, newer than this build (v{}); update Selachi",
            latest_version()
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(latest_version())
}
