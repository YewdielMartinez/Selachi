//! Incremental ingestion of Claude Code and Codex session logs into the store.
//!
//! * Plain `.jsonl`: per-file `(size, mtime, byte offset, parser ctx)`; a re-scan only reads bytes
//!   after the last complete line. A shrunk file restarts from 0.
//! * `.jsonl.zst`: not seekable; re-read whole only if size or mtime changed (all writes are
//!   idempotent through unique keys).
//! * A trailing line without `\n` (still being written) is left for the next scan.

use crate::claude;
use crate::codex::{self, CodexCtx, CodexItem};
use crate::db::{OffsetRow, Store, UpsertOutcome};
use crate::model::Agent;
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
        }
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
        Ok(rep)
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
