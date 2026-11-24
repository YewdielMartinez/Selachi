//! Incremental ingestion of Claude Code, Codex, OpenCode and Antigravity session logs into the store.
//!
//! * Plain `.jsonl`: per-file `(size, mtime, byte offset, parser ctx)`; a re-scan only reads bytes
//!   after the last complete line. A shrunk file restarts from 0.
//! * `.jsonl.zst`: not seekable; re-read whole only if size or mtime changed (all writes are
//!   idempotent through unique keys).
//! * A trailing line without `\n` (still being written) is left for the next scan.
//! * OpenCode: its SQLite database is opened READ-ONLY and read with a `(time_updated, id)` cursor
//!   (stored in `ingest_offsets.ctx`; the database+WAL size/mtime fingerprint skips unchanged
//!   databases), legacy `storage/message/*/*.json` files by `(size, mtime)` per file. Rows are
//!   deduplicated by message id (last write wins). See [`crate::opencode`].
//! * Antigravity: every `conversations/<uuid>.db` is opened READ-ONLY and its `gen_metadata` rows are
//!   read from the next generation index (stored in `ingest_offsets.ctx`; the database+WAL size/mtime
//!   fingerprint skips unchanged ones). See [`crate::antigravity`].

use crate::antigravity;
use crate::claude;
use crate::codex::{self, CodexCtx, CodexItem};
use crate::db::{OffsetRow, Store, UpsertOutcome};
use crate::model::Agent;
use crate::opencode::{self, Parsed};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Where to look for logs.
#[derive(Debug, Clone, Default)]
pub struct IngestConfig {
    /// Root of Claude Code projects (`~/.claude/projects`).
    pub claude_projects: Option<PathBuf>,
    /// Root of Codex sessions (`~/.codex/sessions`).
    pub codex_sessions: Option<PathBuf>,
    /// OpenCode data directory (`~/.local/share/opencode`): `opencode.db` and legacy `storage/`.
    pub opencode_data: Option<PathBuf>,
    /// Gemini directory (`~/.gemini`) holding Antigravity's `antigravity-cli/` and `antigravity/`
    /// app dirs with their `conversations/*.db`.
    pub antigravity_data: Option<PathBuf>,
}

impl IngestConfig {
    /// Default locations under the user's home (`CLAUDE_CONFIG_DIR` / `CODEX_HOME` honoured).
    pub fn from_home() -> IngestConfig {
        let home = home_dir();
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".claude")))
            .map(|p| p.join("projects"));
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".codex")))
            .map(|p| p.join("sessions"));
        IngestConfig {
            claude_projects: claude,
            codex_sessions: codex,
            opencode_data: opencode::default_data_dir(),
            antigravity_data: antigravity::default_gemini_dir(),
        }
    }
}

/// Which agent log a changed path belongs to (see [`IngestConfig::classify`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A Claude Code `projects/**/*.jsonl` file.
    Claude,
    /// A Codex `sessions/**/rollout-*.jsonl[.zst]` file.
    Codex,
    /// The OpenCode SQLite database (or its `-wal` / `-shm` companions).
    OpenCodeDb,
    /// A legacy OpenCode `storage/message/*/*.json` file.
    OpenCodeJson,
    /// An Antigravity `conversations/<uuid>.db` (or its `-wal` / `-shm` companions).
    AntigravityDb,
}

impl IngestConfig {
    /// Directories whose changes can mean new usage, with whether to watch them recursively.
    /// OpenCode's data dir is shallow (the database files sit directly in it); its legacy message
    /// tree is only watched if it exists. Missing directories are skipped by the caller.
    pub fn watch_dirs(&self) -> Vec<(PathBuf, bool)> {
        let mut v = vec![];
        if let Some(p) = &self.claude_projects {
            v.push((p.clone(), true));
        }
        if let Some(p) = &self.codex_sessions {
            v.push((p.clone(), true));
        }
        if let Some(p) = &self.opencode_data {
            v.push((p.clone(), false));
            let legacy = p.join("storage").join("message");
            if legacy.is_dir() {
                v.push((legacy, true));
            }
        }
        if let Some(g) = &self.antigravity_data {
            // Shallow: the databases sit directly in `conversations/`.
            v.extend(
                antigravity::conversation_dirs(g)
                    .into_iter()
                    .map(|d| (d, false)),
            );
        }
        v
    }

    /// Is `path` a log file this config ingests? Pure string work (no I/O), so it is cheap enough
    /// to run on every file-system event.
    pub fn classify(&self, path: &Path) -> Option<SourceKind> {
        let name = path.file_name()?.to_str()?;
        if let Some(root) = &self.claude_projects {
            if path.starts_with(root) {
                return path
                    .extension()
                    .is_some_and(|e| e == "jsonl")
                    .then_some(SourceKind::Claude);
            }
        }
        if let Some(root) = &self.codex_sessions {
            if path.starts_with(root) {
                let ok = name.starts_with("rollout-")
                    && (name.ends_with(".jsonl.zst") || name.ends_with(".jsonl"));
                return ok.then_some(SourceKind::Codex);
            }
        }
        if let Some(root) = &self.opencode_data {
            if path.starts_with(root) {
                if path.starts_with(root.join("storage").join("message")) {
                    return path
                        .extension()
                        .is_some_and(|e| e == "json")
                        .then_some(SourceKind::OpenCodeJson);
                }
                let db = name.starts_with("opencode")
                    && (name.ends_with(".db")
                        || name.ends_with(".db-wal")
                        || name.ends_with(".db-shm"));
                return (db && path.parent() == Some(root.as_path()))
                    .then_some(SourceKind::OpenCodeDb);
            }
        }
        if let Some(root) = &self.antigravity_data {
            if path.starts_with(root) && antigravity::is_conversation_db(root, path) {
                return Some(SourceKind::AntigravityDb);
            }
        }
        None
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// What one `scan()` did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanReport {
    pub files_seen: usize,
    pub files_scanned: usize,
    pub files_unchanged: usize,
    pub claude_files: usize,
    pub codex_files: usize,
    /// OpenCode databases + legacy JSON message files seen.
    pub opencode_files: usize,
    /// Antigravity conversation databases seen.
    pub antigravity_files: usize,
    /// Decoded bytes read (for `.zst`, the decompressed size).
    pub bytes_read: u64,
    pub lines: u64,
    pub malformed_lines: u64,
    pub events_inserted: u64,
    /// Existing events replaced by a later streamed chunk with more output tokens.
    pub events_updated: u64,
    /// Repeated chunks / repeated cumulative totals that were not counted again.
    pub duplicates_skipped: u64,
    /// `<synthetic>` / zero-usage entries not counted.
    pub synthetic_skipped: u64,
    pub touches_inserted: u64,
    pub rate_snapshots_inserted: u64,
    /// Per-file problems (unreadable file, truncated zstd stream...). The scan continues.
    pub errors: Vec<String>,
}

pub struct Ingestor {
    store: Store,
    cfg: IngestConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Zst,
}

struct SourceFile {
    path: PathBuf,
    agent: Agent,
    kind: Kind,
}

impl Ingestor {
    pub fn new(store: Store, cfg: IngestConfig) -> Ingestor {
        Ingestor { store, cfg }
    }

    /// Open the database at `db_path` (own connection) with the default home locations.
    pub fn open(db_path: &Path, cfg: IngestConfig) -> Result<Ingestor> {
        Ok(Ingestor::new(Store::open(db_path)?, cfg))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub fn into_store(self) -> Store {
        self.store
    }

    fn discover(&self) -> Vec<SourceFile> {
        let mut out = vec![];
        if let Some(root) = &self.cfg.claude_projects {
            let mut files = vec![];
            walk(root, &mut files);
            for p in files {
                if p.extension().is_some_and(|e| e == "jsonl") {
                    out.push(SourceFile {
                        path: p,
                        agent: Agent::Claude,
                        kind: Kind::Plain,
                    });
                }
            }
        }
        if let Some(root) = &self.cfg.codex_sessions {
            let mut files = vec![];
            walk(root, &mut files);
            for p in files {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.starts_with("rollout-") {
                    continue;
                }
                if name.ends_with(".jsonl.zst") {
                    out.push(SourceFile {
                        path: p,
                        agent: Agent::Codex,
                        kind: Kind::Zst,
                    });
                } else if name.ends_with(".jsonl") {
                    out.push(SourceFile {
                        path: p,
                        agent: Agent::Codex,
                        kind: Kind::Plain,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Scan all known locations, ingesting only what is new. `Err` only for database failures;
    /// per-file I/O problems are collected in [`ScanReport::errors`].
    pub fn scan(&mut self) -> Result<ScanReport> {
        let mut rep = ScanReport::default();
        for f in self.discover() {
            rep.files_seen += 1;
            match f.agent {
                Agent::Claude => rep.claude_files += 1,
                Agent::Codex => rep.codex_files += 1,
                Agent::OpenCode | Agent::Antigravity => {}
            }
            match self.scan_file(&f, &mut rep) {
                Ok(()) => {}
                Err(e) => {
                    if is_db_error(&e) {
                        return Err(e);
                    }
                    rep.errors.push(format!("{}: {e:#}", f.path.display()));
                }
            }
        }
        self.scan_opencode(&mut rep)?;
        self.scan_antigravity(&mut rep)?;
        Ok(rep)
    }

    /// Ingest only `paths` (typically what a file watcher reported as changed): no directory
    /// walk, one `stat` + one offset lookup per path, and only the bytes after the stored offset
    /// of each plain `.jsonl`. Paths that are not agent logs, or that no longer exist, are
    /// ignored. An OpenCode database path (or its `-wal`) triggers one cursor read of that
    /// database. `Err` only for database failures, like [`Ingestor::scan`].
    pub fn scan_paths(&mut self, paths: &[PathBuf]) -> Result<ScanReport> {
        let mut rep = ScanReport::default();
        let mut oc_dbs: Vec<PathBuf> = vec![];
        let mut agy_dbs: Vec<PathBuf> = vec![];
        for p in paths {
            let Some(kind) = self.cfg.classify(p) else {
                continue;
            };
            match kind {
                SourceKind::Claude | SourceKind::Codex => {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let (agent, kind) = if kind == SourceKind::Claude {
                        (Agent::Claude, Kind::Plain)
                    } else if name.ends_with(".zst") {
                        (Agent::Codex, Kind::Zst)
                    } else {
                        (Agent::Codex, Kind::Plain)
                    };
                    if !p.is_file() {
                        continue;
                    }
                    rep.files_seen += 1;
                    match agent {
                        Agent::Claude => rep.claude_files += 1,
                        _ => rep.codex_files += 1,
                    }
                    let f = SourceFile {
                        path: p.clone(),
                        agent,
                        kind,
                    };
                    if let Err(e) = self.scan_file(&f, &mut rep) {
                        if is_db_error(&e) {
                            return Err(e);
                        }
                        rep.errors.push(format!("{}: {e:#}", p.display()));
                    }
                }
                SourceKind::OpenCodeDb => {
                    let s = p.to_string_lossy();
                    let base = s
                        .strip_suffix("-wal")
                        .or_else(|| s.strip_suffix("-shm"))
                        .unwrap_or(&s);
                    let db = PathBuf::from(base);
                    if db.is_file() && !oc_dbs.contains(&db) {
                        oc_dbs.push(db);
                    }
                }
                SourceKind::AntigravityDb => {
                    let s = p.to_string_lossy();
                    let base = s
                        .strip_suffix("-wal")
                        .or_else(|| s.strip_suffix("-shm"))
                        .unwrap_or(&s);
                    let db = PathBuf::from(base);
                    if db.is_file() && !agy_dbs.contains(&db) {
                        agy_dbs.push(db);
                    }
                }
                SourceKind::OpenCodeJson => {
                    if !p.is_file() {
                        continue;
                    }
                    rep.files_seen += 1;
                    rep.opencode_files += 1;
                    match self.scan_opencode_json(p, &mut rep) {
                        Ok(()) => {}
                        Err(OcError::Store(e)) => return Err(e),
                        Err(OcError::Source(m)) => rep.errors.push(format!("{}: {m}", p.display())),
                    }
                }
            }
        }
        for db in oc_dbs {
            rep.files_seen += 1;
            rep.opencode_files += 1;
            match self.scan_opencode_db(&db, &mut rep) {
                Ok(()) => {}
                Err(OcError::Store(e)) => return Err(e),
                Err(OcError::Source(m)) => rep.errors.push(format!("{}: {m}", db.display())),
            }
        }
        for db in agy_dbs {
            rep.files_seen += 1;
            rep.antigravity_files += 1;
            match self.scan_antigravity_db(&db, &mut rep) {
                Ok(()) => {}
                Err(OcError::Store(e)) => return Err(e),
                Err(OcError::Source(m)) => rep.errors.push(format!("{}: {m}", db.display())),
            }
        }
        Ok(rep)
    }

    /// Antigravity: every conversation database under the Gemini dir (generation cursor).
    fn scan_antigravity(&mut self, rep: &mut ScanReport) -> Result<()> {
        let Some(root) = self.cfg.antigravity_data.clone() else {
            return Ok(());
        };
        for dir in antigravity::conversation_dirs(&root) {
            for db in antigravity::find_dbs(&dir) {
                rep.files_seen += 1;
                rep.antigravity_files += 1;
                match self.scan_antigravity_db(&db, rep) {
                    Ok(()) => {}
                    Err(OcError::Store(e)) => return Err(e),
                    Err(OcError::Source(m)) => rep.errors.push(format!("{}: {m}", db.display())),
                }
            }
        }
        Ok(())
    }

    fn scan_antigravity_db(
        &mut self,
        db: &Path,
        rep: &mut ScanReport,
    ) -> std::result::Result<(), OcError> {
        let (size, mtime_ms) =
            db_fingerprint(db).map_err(|e| OcError::Source(format!("stat: {e}")))?;
        let key = db.to_string_lossy().to_string();
        let stored = self.store.get_offset(&key).map_err(OcError::Store)?;
        let mut cursor = stored
            .as_ref()
            .and_then(|s| s.ctx.as_deref())
            .and_then(|c| serde_json::from_str::<AgyCursor>(c).ok())
            .unwrap_or_default();
        if let Some(s) = &stored {
            if s.size == size && s.mtime_ms == mtime_ms {
                rep.files_unchanged += 1;
                return Ok(());
            }
            if size < s.size {
                cursor = AgyCursor::default(); // database replaced / vacuumed: start over (idempotent)
            }
        }
        rep.files_scanned += 1;
        // Re-read the last generation seen (rewritten in place while a turn streams): idempotent.
        let from = cursor.next.saturating_sub(1).max(0);
        let read = antigravity::read_conversation(db, from, mtime_ms).map_err(OcError::Source)?;
        rep.bytes_read += read.bytes;
        rep.synthetic_skipped += read.skipped;
        let tx = self
            .store
            .conn
            .unchecked_transaction()
            .map_err(|e| OcError::Store(e.into()))?;
        for (e, x) in &read.events {
            rep.lines += 1;
            let out = self
                .store
                .upsert_usage_replace(e, x)
                .map_err(OcError::Store)?;
            count_upsert(out, rep);
        }
        self.store
            .put_offset(
                &key,
                Agent::Antigravity,
                &OffsetRow {
                    size,
                    mtime_ms,
                    byte_offset: 0,
                    ctx: Some(
                        serde_json::to_string(&AgyCursor {
                            next: read.next_idx,
                        })
                        .unwrap_or_default(),
                    ),
                },
                now_ms(),
            )
            .map_err(OcError::Store)?;
        tx.commit().map_err(|e| OcError::Store(e.into()))?;
        Ok(())
    }

    /// OpenCode: SQLite databases (cursor) and legacy JSON messages (per-file fingerprint).
    fn scan_opencode(&mut self, rep: &mut ScanReport) -> Result<()> {
        let Some(root) = self.cfg.opencode_data.clone() else {
            return Ok(());
        };
        for db in opencode::find_dbs(&root) {
            rep.files_seen += 1;
            rep.opencode_files += 1;
            match self.scan_opencode_db(&db, rep) {
                Ok(()) => {}
                Err(OcError::Store(e)) => return Err(e),
                Err(OcError::Source(m)) => rep.errors.push(format!("{}: {m}", db.display())),
            }
        }
        for f in opencode::legacy_message_files(&root) {
            rep.files_seen += 1;
            rep.opencode_files += 1;
            match self.scan_opencode_json(&f, rep) {
                Ok(()) => {}
                Err(OcError::Store(e)) => return Err(e),
                Err(OcError::Source(m)) => rep.errors.push(format!("{}: {m}", f.display())),
            }
        }
        Ok(())
    }

    fn scan_opencode_json(
        &mut self,
        path: &Path,
        rep: &mut ScanReport,
    ) -> std::result::Result<(), OcError> {
        let md = std::fs::metadata(path).map_err(|e| OcError::Source(format!("stat: {e}")))?;
        let (size, mtime_ms) = fingerprint(&md);
        let key = path.to_string_lossy().to_string();
        if let Some(s) = self.store.get_offset(&key).map_err(OcError::Store)? {
            if s.size == size && s.mtime_ms == mtime_ms {
                rep.files_unchanged += 1;
                return Ok(());
            }
        }
        rep.files_scanned += 1;
        let text =
            std::fs::read_to_string(path).map_err(|e| OcError::Source(format!("read: {e}")))?;
        rep.bytes_read += text.len() as u64;
        rep.lines += 1;
        let tx = self
            .store
            .conn
            .unchecked_transaction()
            .map_err(|e| OcError::Store(e.into()))?;
        match serde_json::from_str::<serde_json::Value>(&text) {
            Err(_) => rep.malformed_lines += 1,
            Ok(v) => {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or(stem);
                let session = v
                    .get("sessionID")
                    .and_then(|i| i.as_str())
                    .or_else(|| {
                        path.parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                    })
                    .unwrap_or("unknown");
                self.store_opencode(
                    opencode::parse_message(id, session, None, &v, None, None),
                    rep,
                )?;
            }
        }
        self.store
            .put_offset(
                &key,
                Agent::OpenCode,
                &OffsetRow {
                    size,
                    mtime_ms,
                    byte_offset: size,
                    ctx: None,
                },
                now_ms(),
            )
            .map_err(OcError::Store)?;
        tx.commit().map_err(|e| OcError::Store(e.into()))?;
        Ok(())
    }

    fn store_opencode(&self, p: Parsed, rep: &mut ScanReport) -> std::result::Result<(), OcError> {
        match p {
            Parsed::NotAssistant => {}
            Parsed::Empty => rep.synthetic_skipped += 1,
            Parsed::Usage(b) => {
                let (e, x) = *b;
                let out = self
                    .store
                    .upsert_usage_replace(&e, &x)
                    .map_err(OcError::Store)?;
                count_upsert(out, rep);
            }
        }
        Ok(())
    }

    fn scan_opencode_db(
        &mut self,
        db: &Path,
        rep: &mut ScanReport,
    ) -> std::result::Result<(), OcError> {
        let (size, mtime_ms) =
            db_fingerprint(db).map_err(|e| OcError::Source(format!("stat: {e}")))?;
        let key = db.to_string_lossy().to_string();
        let stored = self.store.get_offset(&key).map_err(OcError::Store)?;
        let mut cursor = stored
            .as_ref()
            .and_then(|s| s.ctx.as_deref())
            .and_then(|c| serde_json::from_str::<OcCursor>(c).ok())
            .unwrap_or_default();
        if let Some(s) = &stored {
            if s.size == size && s.mtime_ms == mtime_ms {
                rep.files_unchanged += 1;
                return Ok(());
            }
            if size < s.size {
                cursor = OcCursor::default(); // database replaced / vacuumed: start over (idempotent)
            }
        }
        rep.files_scanned += 1;
        let reader = OcReader::open(db).map_err(OcError::Source)?;
        let tx = self
            .store
            .conn
            .unchecked_transaction()
            .map_err(|e| OcError::Store(e.into()))?;
        // First pass of a scan re-reads the boundary millisecond (idempotent upsert); later
        // chunks use a strict keyset so every row is read once.
        let mut strict = false;
        loop {
            let rows = reader
                .next_chunk(&cursor, strict)
                .map_err(OcError::Source)?;
            if rows.is_empty() {
                break;
            }
            for r in &rows {
                rep.lines += 1;
                rep.bytes_read += r.data.len() as u64;
                match serde_json::from_str::<serde_json::Value>(&r.data) {
                    Err(_) => rep.malformed_lines += 1,
                    Ok(v) => self.store_opencode(
                        opencode::parse_message(
                            &r.id,
                            &r.session_id,
                            Some(r.time_created),
                            &v,
                            r.directory.as_deref(),
                            r.branch.as_deref(),
                        ),
                        rep,
                    )?,
                }
            }
            if let Some(last) = rows.last() {
                cursor = OcCursor {
                    t: last.time_updated,
                    id: last.id.clone(),
                };
            }
            strict = true;
            if rows.len() < OcReader::CHUNK {
                break;
            }
        }
        // Next scan restarts at the boundary millisecond, whatever id the strict cursor stopped on.
        let saved = OcCursor {
            t: cursor.t,
            id: String::new(),
        };
        self.store
            .put_offset(
                &key,
                Agent::OpenCode,
                &OffsetRow {
                    size,
                    mtime_ms,
                    byte_offset: 0,
                    ctx: Some(serde_json::to_string(&saved).unwrap_or_default()),
                },
                now_ms(),
            )
            .map_err(OcError::Store)?;
        tx.commit().map_err(|e| OcError::Store(e.into()))?;
        Ok(())
    }

    fn scan_file(&mut self, f: &SourceFile, rep: &mut ScanReport) -> Result<()> {
        let md = std::fs::metadata(&f.path).with_context(|| "stat")?;
        let size = md.len() as i64;
        let mtime_ms = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let key = f.path.to_string_lossy().to_string();
        let stored = self.store.get_offset(&key)?;

        if let Some(s) = &stored {
            if s.size == size && s.mtime_ms == mtime_ms {
                rep.files_unchanged += 1;
                return Ok(());
            }
        }
        rep.files_scanned += 1;

        let fallback_session = f
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .trim_end_matches(".jsonl")
            .to_string();

        let (start, mut ctx) = match (&stored, f.kind) {
            (Some(s), Kind::Plain) if s.byte_offset <= size => (
                s.byte_offset as u64,
                s.ctx
                    .as_deref()
                    .and_then(|c| serde_json::from_str::<CodexCtx>(c).ok())
                    .unwrap_or_default(),
            ),
            _ => (0, CodexCtx::default()),
        };

        let file = File::open(&f.path).with_context(|| "open")?;
        let tx = self.store.conn.unchecked_transaction()?;
        let mut consumed: u64 = start;
        let mut read_err: Option<anyhow::Error> = None;

        {
            let mut reader: Box<dyn BufRead> = match f.kind {
                Kind::Plain => {
                    let mut file = file;
                    file.seek(SeekFrom::Start(start))?;
                    Box::new(BufReader::with_capacity(1 << 20, file))
                }
                Kind::Zst => Box::new(BufReader::with_capacity(
                    1 << 20,
                    zstd::stream::read::Decoder::new(file).with_context(|| "zstd init")?,
                )),
            };
            let mut buf: Vec<u8> = Vec::with_capacity(8192);
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let complete = buf.last() == Some(&b'\n');
                        if !complete && f.kind == Kind::Plain {
                            break; // partial line still being written
                        }
                        consumed += n as u64;
                        rep.bytes_read += n as u64;
                        let text = String::from_utf8_lossy(&buf);
                        let line = text.trim_end_matches(['\n', '\r']);
                        if line.trim().is_empty() {
                            continue;
                        }
                        rep.lines += 1;
                        process_line(&self.store, f.agent, line, &fallback_session, &mut ctx, rep)?;
                    }
                    Err(e) => {
                        read_err = Some(anyhow::Error::new(e).context("read"));
                        break;
                    }
                }
            }
        }

        if read_err.is_none() {
            let (byte_offset, ctx_json) = match f.kind {
                Kind::Plain => (
                    consumed as i64,
                    (f.agent == Agent::Codex)
                        .then(|| serde_json::to_string(&ctx).unwrap_or_default()),
                ),
                Kind::Zst => (size, None),
            };
            self.store.put_offset(
                &key,
                f.agent,
                &OffsetRow {
                    size,
                    mtime_ms,
                    byte_offset,
                    ctx: ctx_json,
                },
                now_ms(),
            )?;
        }
        tx.commit()?;
        match read_err {
            // Events parsed before the failure are kept (idempotent); offsets stay old so the
            // file is retried on the next scan.
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn process_line(
    store: &Store,
    agent: Agent,
    line: &str,
    fallback_session: &str,
    ctx: &mut CodexCtx,
    rep: &mut ScanReport,
) -> Result<()> {
    match agent {
        Agent::Claude => match claude::parse_line(line) {
            Err(_) => rep.malformed_lines += 1,
            Ok(None) => {}
            Ok(Some(l)) => {
                if l.skipped_synthetic {
                    rep.synthetic_skipped += 1;
                }
                if let Some(e) = &l.usage {
                    count_upsert(store.upsert_usage(e)?, rep);
                }
                for t in &l.touches {
                    if store.insert_touch(t)? {
                        rep.touches_inserted += 1;
                    }
                }
                if let Some(r) = &l.rate_limit {
                    if store.insert_rate_limit(r)? {
                        rep.rate_snapshots_inserted += 1;
                    }
                }
            }
        },
        Agent::OpenCode | Agent::Antigravity => {} // not line-based: see `scan_opencode` / `scan_antigravity`
        Agent::Codex => match codex::parse_line(ctx, line, fallback_session) {
            Err(_) => rep.malformed_lines += 1,
            Ok(items) => {
                for it in items {
                    match it {
                        CodexItem::Usage(e) => count_upsert(store.upsert_usage(&e)?, rep),
                        CodexItem::Touch(t) => {
                            if store.insert_touch(&t)? {
                                rep.touches_inserted += 1;
                            }
                        }
                        CodexItem::RateLimit(r) => {
                            if store.insert_rate_limit(&r)? {
                                rep.rate_snapshots_inserted += 1;
                            }
                        }
                        CodexItem::NoNewUsage => rep.duplicates_skipped += 1,
                    }
                }
            }
        },
    }
    Ok(())
}

/// Failure while ingesting OpenCode data: `Store` (our database, fatal for the scan) vs `Source`
/// (OpenCode's files: reported per file, the scan continues).
enum OcError {
    Store(anyhow::Error),
    Source(String),
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct OcCursor {
    /// `message.time_updated` (ms) of the last row read.
    t: i64,
    /// `message.id` of the last row read ("" = start of that millisecond).
    id: String,
}

/// Per-database cursor of an Antigravity conversation.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct AgyCursor {
    /// First `gen_metadata.idx` not ingested yet.
    next: i64,
}

struct OcRow {
    id: String,
    session_id: String,
    time_created: i64,
    time_updated: i64,
    data: String,
    directory: Option<String>,
    branch: Option<String>,
}

/// Read-only view of an OpenCode SQLite database.
struct OcReader {
    conn: rusqlite::Connection,
    joined: bool,
}

impl OcReader {
    const CHUNK: usize = 2000;

    fn open(path: &Path) -> std::result::Result<Self, String> {
        use rusqlite::OpenFlags;
        let conn = rusqlite::Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open read-only: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        let has = |t: &str| -> bool {
            conn.query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [t],
                |_| Ok(()),
            )
            .is_ok()
        };
        if !has("message") {
            return Err("no `message` table (unsupported OpenCode schema)".into());
        }
        let joined = has("session") && has("workspace");
        Ok(Self { conn, joined })
    }

    fn next_chunk(&self, c: &OcCursor, strict: bool) -> std::result::Result<Vec<OcRow>, String> {
        let from = if self.joined {
            "FROM message m LEFT JOIN session s ON s.id = m.session_id \
             LEFT JOIN workspace w ON w.id = s.workspace_id"
        } else {
            "FROM message m"
        };
        let sel = if self.joined {
            "s.directory, w.branch"
        } else {
            "NULL, NULL"
        };
        // Non-strict: `time_updated >= t` (id ignored); strict: keyset after (t, id).
        let sql = format!(
            "SELECT m.id, m.session_id, m.time_created, m.time_updated, m.data, {sel} {from} \
             WHERE json_extract(m.data, '$.role') = 'assistant' \
               AND ((?3 = 0 AND m.time_updated >= ?1) \
                 OR (?3 = 1 AND (m.time_updated > ?1 OR (m.time_updated = ?1 AND m.id > ?2)))) \
             ORDER BY m.time_updated, m.id LIMIT {}",
            Self::CHUNK
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![c.t, c.id, strict as i64], |r| {
                Ok(OcRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    time_created: r.get(2)?,
                    time_updated: r.get(3)?,
                    data: r.get(4)?,
                    directory: r.get(5)?,
                    branch: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

fn fingerprint(md: &std::fs::Metadata) -> (i64, i64) {
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (md.len() as i64, mtime)
}

/// Size and mtime of the database combined with its `-wal` (WAL writes do not touch the main file).
fn db_fingerprint(db: &Path) -> std::io::Result<(i64, i64)> {
    let (mut size, mut mtime) = fingerprint(&std::fs::metadata(db)?);
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    if let Ok(md) = std::fs::metadata(PathBuf::from(wal)) {
        let (s, m) = fingerprint(&md);
        size += s;
        mtime = mtime.max(m);
    }
    Ok((size, mtime))
}

fn count_upsert(o: UpsertOutcome, rep: &mut ScanReport) {
    match o {
        UpsertOutcome::Inserted => rep.events_inserted += 1,
        UpsertOutcome::Updated => rep.events_updated += 1,
        UpsertOutcome::Duplicate => rep.duplicates_skipped += 1,
    }
}

fn is_db_error(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<rusqlite::Error>().is_some())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(&p, out);
        } else if ft.is_file() {
            out.push(p);
        }
    }
}
