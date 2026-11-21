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
    // v2: snippets (A12). Six useful defaults are seeded here, i.e. once per database.
    r#"
CREATE TABLE snippets(
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    template    TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    tags        TEXT    NOT NULL DEFAULT '',      -- comma separated, lowercase
    scope       TEXT    NOT NULL DEFAULT 'global',-- global|project
    scope_path  TEXT    NOT NULL DEFAULT '',      -- project folder (display form)
    scope_key   TEXT    NOT NULL DEFAULT '',      -- normalized scope_path
    created     INTEGER NOT NULL,                 -- unix ms
    last_used   INTEGER,                          -- unix ms
    uses        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_snippets_scope ON snippets(scope, scope_key);

INSERT INTO snippets(name, template, description, tags, created) VALUES
 ('EF: nueva migración', 'dotnet ef migrations add {nombre}', 'Crea una migración de Entity Framework Core', 'dotnet,ef,migracion', CAST(strftime('%s','now') AS INTEGER) * 1000),
 ('Git: nueva rama', 'git switch -c {rama}', 'Crea una rama y cambia a ella', 'git,rama', CAST(strftime('%s','now') AS INTEGER) * 1000),
 ('npm: instalar paquete', 'npm i {paquete}', 'Instala un paquete en el proyecto', 'npm,node', CAST(strftime('%s','now') AS INTEGER) * 1000),
 ('Docker: logs de un servicio', 'docker compose logs -f {servicio}', 'Sigue los logs de un servicio de compose', 'docker,compose,logs', CAST(strftime('%s','now') AS INTEGER) * 1000),
 ('Cargo: test de un crate', 'cargo test -p {crate}', 'Ejecuta los tests de un crate del workspace', 'cargo,rust,test', CAST(strftime('%s','now') AS INTEGER) * 1000),
 ('Git: log corto', 'git log --oneline -n {n:20}', 'Últimos commits en una línea', 'git,log', CAST(strftime('%s','now') AS INTEGER) * 1000);
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
