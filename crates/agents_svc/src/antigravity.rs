//! Antigravity usage: reads the local conversation databases of the Antigravity CLI (`agy`) and IDE
//! (read-only, offline). Structure only was inspected to write this module; no message text, tool
//! output or credential is ever read.
//!
//! # Where the data lives (agy 1.5.15 + Antigravity IDE, Windows 11, 2026-11-23)
//!
//! The shared Gemini directory `~/.gemini` (`--gemini-dir`) holds one **app data dir** per product:
//!
//! * `antigravity-cli/` (the `agy` CLI; `--app-data-dir`/`--cli_dir_name` default) and
//! * `antigravity/` (Antigravity IDE / desktop app),
//!
//! each with the same layout: `conversations/<uuid>.db` (SQLite, one per conversation),
//! `conversation_summaries.db` (index with `workspace_uris`, `parent_conversation_id`, ...), `brain/`,
//! `knowledge/`, `log/`, `mcp_config.json`, `cache/`, ... Older conversations are
//! `conversations/<uuid>.pb`: high-entropy blobs (encrypted or compressed, ~8 bits per byte), **not
//! readable and not ingested**. `~/.antigravity/` only holds `argv.json` and
//! `%APPDATA%\Antigravity` is the IDE's Electron profile (no sessions or usage): neither is read.
//!
//! # Conversation database schema (tables of `conversations/<uuid>.db`)
//!
//! `trajectory_meta`, `steps(idx, step_type, status, metadata blob, step_payload blob, ...)`,
//! `gen_metadata(idx, data blob, size)` (one row per **model generation**), `executor_metadata`,
//! `parent_references`, `trajectory_metadata_blob`, `battle_mode_infos`. The blobs are protocol
//! buffers **without a published schema**: the field numbers below were reverse-engineered from 1,421
//! generations in 14 local databases and are checked by an identity that held in every row
//! (`output == thinking + response`). Treat them as a best effort: any row that does not decode is
//! skipped and counted, never guessed.
//!
//! `gen_metadata.data`, message `/1` (the generation):
//!
//! | Path        | Meaning                                                              |
//! |-------------|----------------------------------------------------------------------|
//! | `/1/4/1`    | model enum (an id, not a name)                                       |
//! | `/1/4/2`    | input tokens, **not** cached (cache reads are separate and larger)   |
//! | `/1/4/3`    | output tokens, thinking included                                     |
//! | `/1/4/4`    | cache write tokens (never seen non-zero; read when present)          |
//! | `/1/4/5`    | cache read tokens (absent = 0)                                       |
//! | `/1/4/9`    | thinking tokens (a part of `/1/4/3`; `/1/4/3 == /1/4/9 + /1/4/10`)   |
//! | `/1/4/10`   | response tokens                                                      |
//! | `/1/19`     | model name string (`claude-opus-5-5-medium`, `gemini-4-argon`, ...)  |
//! | `/1/9/4/1`  | unix seconds of the generation (only in older conversations)         |
//!
//! `steps.metadata` of a model-response step carries the same usage message at `/9` and the step's
//! unix seconds at `/1/1`: that is where the time of newer generations comes from. Generations are
//! matched to steps in order by their usage tuple.
//!
//! # Mapping to [`UsageEvent`]
//!
//! agent `antigravity`; session = conversation uuid (the file stem); key `antigravity:<uuid>:<idx>`
//! (generation index, stable, rewritten in place: last write wins); `input` -> `input`;
//! `/1/4/3` -> `output` (reasoning in [`UsageExtras::reasoning`]); cache read/write -> `cache_read` /
//! `cache_create`; model = the name, else `antigravity-model-<enum>`; `cwd` = first `workspace_uris`
//! entry of the app dir's `conversation_summaries.db`. **No cost**: Antigravity reports none and its
//! models are not in the price table, so only tokens are shown (never an invented figure).
//!
//! Limits: no rate-limit / quota data is stored locally (the tier is only in the live ACP handshake).
//! The agy CLI had no conversation of its own on the machine inspected (only the IDE had), so the
//! CLI directory is covered by the shared layout, not by a recorded sample.

use crate::model::{Agent, TokenCounts, UsageEvent, UsageExtras};
use std::path::{Path, PathBuf};

/// Names of the app data dirs under the Gemini dir that hold `conversations/`.
pub const APP_DIRS: [&str; 2] = ["antigravity-cli", "antigravity"];

/// `~/.gemini` (the default `--gemini-dir`).
pub fn default_gemini_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".gemini"))
}

/// `<gemini>/<app dir>/conversations` directories that exist.
pub fn conversation_dirs(gemini: &Path) -> Vec<PathBuf> {
    APP_DIRS
        .iter()
        .map(|a| gemini.join(a).join("conversations"))
        .filter(|d| d.is_dir())
        .collect()
}

/// Conversation databases (`*.db`) in `dir`, sorted. The `.pb` legacy files are not listed.
pub fn find_dbs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "db") && p.is_file())
        .collect();
    v.sort();
    v
}

/// Is `path` (pure string work) a conversation database file or its `-wal` / `-shm` companion
/// directly inside `<gemini>/<app dir>/conversations`?
pub fn is_conversation_db(gemini: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !(name.ends_with(".db") || name.ends_with(".db-wal") || name.ends_with(".db-shm")) {
        return false;
    }
    let Some(dir) = path.parent() else {
        return false;
    };
    dir.file_name().is_some_and(|n| n == "conversations")
        && dir.parent().is_some_and(|app| {
            app.starts_with(gemini)
                && app.parent() == Some(gemini)
                && APP_DIRS.iter().any(|a| app.ends_with(a))
        })
}

// ---------------------------------------------------------------------------
// Minimal, bounds-checked protobuf wire reader
// ---------------------------------------------------------------------------

enum Val<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

struct Fields<'a> {
    b: &'a [u8],
    i: usize,
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut r = 0u64;
    for shift in (0..70).step_by(7) {
        let x = *b.get(*i)?;
        *i += 1;
        r |= u64::from(x & 0x7f).checked_shl(shift).unwrap_or(0);
        if x & 0x80 == 0 {
            return Some(r);
        }
    }
    None
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u32, Val<'a>);

    /// Ends (rather than panics) at the first malformed byte.
    fn next(&mut self) -> Option<Self::Item> {
        if self.i >= self.b.len() {
            return None;
        }
        let tag = varint(self.b, &mut self.i)?;
        let field = u32::try_from(tag >> 3).ok()?;
        let val = match tag & 7 {
            0 => Val::Varint(varint(self.b, &mut self.i)?),
            1 => {
                self.i = self.i.checked_add(8).filter(|e| *e <= self.b.len())?;
                Val::Fixed
            }
            5 => {
                self.i = self.i.checked_add(4).filter(|e| *e <= self.b.len())?;
                Val::Fixed
            }
            2 => {
                let len = usize::try_from(varint(self.b, &mut self.i)?).ok()?;
                let end = self.i.checked_add(len).filter(|e| *e <= self.b.len())?;
                let s = &self.b[self.i..end];
                self.i = end;
                Val::Bytes(s)
            }
            _ => return None,
        };
        Some((field, val))
    }
}

fn fields(b: &[u8]) -> Fields<'_> {
    Fields { b, i: 0 }
}

/// First length-delimited sub-message at `path` (field numbers, outermost first).
fn msg<'a>(mut b: &'a [u8], path: &[u32]) -> Option<&'a [u8]> {
    for f in path {
        b = fields(b).find_map(|(n, v)| match v {
            Val::Bytes(s) if n == *f => Some(s),
            _ => None,
        })?;
    }
    Some(b)
}

fn int(b: &[u8], field: u32) -> Option<u64> {
    fields(b).find_map(|(n, v)| match v {
        Val::Varint(x) if n == field => Some(x),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

/// One model generation as stored in `gen_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub idx: i64,
    pub model: Option<String>,
    pub model_enum: Option<u64>,
    pub tokens: TokenCounts,
    pub reasoning: u64,
    /// Unix seconds, when the generation itself carries one (older conversations).
    pub ts_s: Option<i64>,
    key: [u64; 6],
}

fn usage_key(u: &[u8]) -> [u64; 6] {
    [1, 2, 3, 5, 9, 10].map(|f| int(u, f).unwrap_or(0))
}

fn tokens_of(u: &[u8]) -> (TokenCounts, u64) {
    let n = |f| int(u, f).unwrap_or(0);
    let counts = TokenCounts {
        input: n(2),
        output: n(3),
        cache_create: n(4),
        cache_create_1h: 0,
        cache_read: n(5),
    };
    (counts, n(9).min(counts.output))
}

/// Decode one `gen_metadata.data` blob; `None` when it has no usage message.
pub fn parse_generation(idx: i64, data: &[u8]) -> Option<Generation> {
    let g = msg(data, &[1])?;
    let u = msg(g, &[4])?;
    let (tokens, reasoning) = tokens_of(u);
    let model = msg(g, &[19])
        .and_then(|m| std::str::from_utf8(m).ok())
        .map(str::trim)
        .filter(|m| !m.is_empty() && m.len() <= 80 && m.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_string);
    let ts_s = msg(g, &[9, 4])
        .and_then(|t| int(t, 1))
        .and_then(|s| i64::try_from(s).ok())
        .filter(|s| *s > 1_000_000_000);
    Some(Generation {
        idx,
        model,
        model_enum: int(u, 1),
        tokens,
        reasoning,
        ts_s,
        key: usage_key(u),
    })
}

/// Usage tuple and unix seconds of a model-response step (`steps.metadata`), if it has usage.
pub fn parse_step(meta: &[u8]) -> Option<([u64; 6], i64)> {
    let u = msg(meta, &[9])?;
    let ts = msg(meta, &[1])
        .and_then(|t| int(t, 1))
        .and_then(|s| i64::try_from(s).ok())?;
    Some((usage_key(u), ts))
}

/// `file:///c%3A/Users/x/proj` -> `c:/Users/x/proj` (percent-decoded; UNC `file://host/share` kept).
pub fn uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///c%3A/x`: the slash before a drive letter is not part of the path.
    let rest = match rest.strip_prefix('/') {
        Some(r)
            if r.as_bytes()
                .get(1)
                .is_some_and(|c| matches!(c, b':' | b'%')) =>
        {
            r
        }
        _ => rest,
    };
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| {
                rest.get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .flatten();
        match hex {
            Some(x) => {
                out.push(x);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.is_empty()).then_some(s)
}

/// First workspace of a `workspace_uris` JSON array (`["file:///c%3A/x"]`).
pub fn first_workspace(workspace_uris_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(workspace_uris_json).ok()?;
    v.as_array()?
        .iter()
        .find_map(|u| u.as_str().and_then(uri_to_path))
}

// ---------------------------------------------------------------------------
// Reading a conversation database
// ---------------------------------------------------------------------------

/// What reading one conversation database produced.
#[derive(Debug, Default)]
pub struct ConvRead {
    /// Events for generations at or after the requested index (zero-usage rows are left out).
    pub events: Vec<(UsageEvent, UsageExtras)>,
    /// First generation index not read yet (the next incremental call starts here).
    pub next_idx: i64,
    /// `gen_metadata` rows that did not decode or had no tokens.
    pub skipped: u64,
    /// Bytes of blobs read.
    pub bytes: u64,
}

fn open_ro(path: &Path) -> Result<rusqlite::Connection, String> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open read-only: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn has_table(conn: &rusqlite::Connection, t: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |_| Ok(()),
    )
    .is_ok()
}

/// Workspace folder of `conversation_id` from the app dir's `conversation_summaries.db`.
pub fn workspace_of(summaries_db: &Path, conversation_id: &str) -> Option<String> {
    if !summaries_db.is_file() {
        return None;
    }
    let conn = open_ro(summaries_db).ok()?;
    let json: String = conn
        .query_row(
            "SELECT workspace_uris FROM conversation_summaries WHERE conversation_id = ?1",
            [conversation_id],
            |r| r.get(0),
        )
        .ok()?;
    first_workspace(&json)
}

/// Read the generations of `db` with `idx >= from_idx`. `fallback_ts_ms` (the file's mtime) is used
/// only for a generation whose time cannot be found anywhere.
pub fn read_conversation(
    db: &Path,
    from_idx: i64,
    fallback_ts_ms: i64,
) -> Result<ConvRead, String> {
    let conn = open_ro(db)?;
    if !has_table(&conn, "gen_metadata") {
        return Err("no `gen_metadata` table (unsupported Antigravity schema)".into());
    }
    let session = db
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let cwd = db
        .parent()
        .and_then(Path::parent)
        .map(|app| app.join("conversation_summaries.db"))
        .and_then(|s| workspace_of(&s, &session));

    let mut out = ConvRead {
        next_idx: from_idx,
        ..Default::default()
    };
    let mut stmt = conn
        .prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([from_idx], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut gens = Vec::new();
    for r in rows {
        let (idx, data) = r.map_err(|e| e.to_string())?;
        out.bytes += data.len() as u64;
        out.next_idx = out.next_idx.max(idx + 1);
        match parse_generation(idx, &data) {
            Some(g) if !g.tokens.is_zero() => gens.push(g),
            _ => out.skipped += 1,
        }
    }
    if gens.is_empty() {
        return Ok(out);
    }

    // Times of generations that do not carry one: the matching model-response steps, in order.
    let steps: Vec<([u64; 6], i64)> = if has_table(&conn, "steps") {
        let mut s = conn
            .prepare("SELECT metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx")
            .map_err(|e| e.to_string())?;
        let rows = s
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(Result::ok)
            .filter_map(|m| parse_step(&m))
            .collect()
    } else {
        Vec::new()
    };
    let mut cursor = 0usize;
    for g in gens {
        let step_ts = steps[cursor..]
            .iter()
            .position(|(k, _)| *k == g.key)
            .map(|p| {
                let ts = steps[cursor + p].1;
                cursor += p + 1;
                ts
            });
        let ts_ms = g.ts_s.or(step_ts).map_or(fallback_ts_ms, |s| s * 1000);
        let model = g
            .model
            .clone()
            .or_else(|| g.model_enum.map(|e| format!("antigravity-model-{e}")))
            .unwrap_or_else(|| "unknown".to_string());
        let event = UsageEvent {
            agent: Agent::Antigravity,
            session_id: session.clone(),
            ts_ms,
            model,
            tokens: g.tokens,
            cwd: cwd.clone(),
            git_branch: None,
            msg_key: format!("antigravity:{session}:{}", g.idx),
        };
        let extras = UsageExtras {
            reasoning: g.reasoning,
            reported_cost_usd: None,
            cost_source: None,
            provider: None,
        };
        out.events.push((event, extras));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod testutil {
    //! Synthetic conversation databases mirroring the real schema (protobuf blobs built by hand).

    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    pub fn field_int(f: u32, v: u64, out: &mut Vec<u8>) {
        varint(u64::from(f) << 3, out);
        varint(v, out);
    }
    pub fn field_bytes(f: u32, b: &[u8], out: &mut Vec<u8>) {
        varint((u64::from(f) << 3) | 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }
    /// `[model enum, input, output, cache_read, thinking, response]`.
    pub fn usage(u: [u64; 6]) -> Vec<u8> {
        let mut o = vec![];
        field_int(1, u[0], &mut o);
        field_int(2, u[1], &mut o);
        field_int(3, u[2], &mut o);
        if u[3] > 0 {
            field_int(5, u[3], &mut o);
        }
        field_int(6, 26, &mut o);
        if u[4] > 0 {
            field_int(9, u[4], &mut o);
        }
        field_int(10, u[5], &mut o);
        o
    }
    /// A `gen_metadata.data` blob; `ts` (unix seconds) only for the old layout.
    pub fn gen_blob(model: Option<&str>, u: [u64; 6], ts: Option<u64>) -> Vec<u8> {
        let mut g = vec![];
        field_int(3, 1026, &mut g);
        field_bytes(4, &usage(u), &mut g);
        if let Some(ts) = ts {
            let mut t = vec![];
            field_int(1, ts, &mut t);
            field_int(2, 47_318_200, &mut t);
            let mut nine = vec![];
            field_bytes(4, &t, &mut nine);
            field_bytes(9, &nine, &mut g);
        }
        if let Some(m) = model {
            field_bytes(19, m.as_bytes(), &mut g);
        }
        let mut top = vec![];
        field_bytes(2, b"ab", &mut top);
        field_bytes(1, &g, &mut top);
        top
    }
    /// A `steps.metadata` blob of a model-response step (or a plain step when `u` is `None`).
    pub fn step_blob(ts: u64, u: Option<[u64; 6]>) -> Vec<u8> {
        let mut t = vec![];
        field_int(1, ts, &mut t);
        field_int(2, 439_851_200, &mut t);
        let mut top = vec![];
        field_bytes(1, &t, &mut top);
        field_int(3, 2, &mut top);
        if let Some(u) = u {
            field_bytes(9, &usage(u), &mut top);
        }
        top
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    const U1: [u64; 6] = [1401, 11_276, 221, 8_401, 0, 221];
    const U2: [u64; 6] = [1401, 1_965, 1_830, 18_459, 803, 1_027];

    #[test]
    fn generation_decodes_tokens_model_and_time() {
        let g = parse_generation(
            7,
            &gen_blob(Some("claude-opus-5-5-medium"), U2, Some(1_780_969_402)),
        )
        .unwrap();
        assert_eq!(g.idx, 7);
        assert_eq!(g.model.as_deref(), Some("claude-opus-5-5-medium"));
        assert_eq!(g.model_enum, Some(1401));
        assert_eq!(
            g.tokens,
            TokenCounts {
                input: 1_965,
                output: 1_830,
                cache_create: 0,
                cache_create_1h: 0,
                cache_read: 18_459
            }
        );
        assert_eq!(g.reasoning, 803);
        assert_eq!(g.ts_s, Some(1_780_969_402));
        // no cache field -> 0, no thinking field -> 0, no timestamp / name -> None
        let g = parse_generation(0, &gen_blob(None, [1318, 5, 6, 0, 0, 6], None)).unwrap();
        assert_eq!(
            (g.tokens.cache_read, g.reasoning, g.ts_s, g.model.clone()),
            (0, 0, None, None)
        );
        assert_eq!(g.model_enum, Some(1318));
    }

    #[test]
    fn reader_never_panics_on_garbage() {
        for bad in [
            &b""[..],
            &[0xff; 16],
            &[0x0a, 0x7f],
            &[0x08],
            &[0x0a, 0x03, 1, 2],
            &[0x0b],
        ] {
            assert!(parse_generation(0, bad).is_none(), "{bad:?}");
            assert!(parse_step(bad).is_none());
        }
        // a thinking count above the output total cannot make the subset exceed its parent
        let g = parse_generation(0, &gen_blob(None, [1, 1, 5, 0, 99, 0], None)).unwrap();
        assert_eq!(g.reasoning, 5);
    }

    #[test]
    fn step_carries_the_time_and_the_same_usage_tuple() {
        let g = parse_generation(0, &gen_blob(None, U1, None)).unwrap();
        let (key, ts) = parse_step(&step_blob(1_795_222_788, Some(U1))).unwrap();
        assert_eq!((key, ts), (g.key, 1_795_222_788));
        assert!(parse_step(&step_blob(1_795_222_788, None)).is_none());
    }

    #[test]
    fn workspace_uris_become_paths() {
        assert_eq!(
            uri_to_path("file:///c%3A/Users/a%20b/proj").as_deref(),
            Some("c:/Users/a b/proj")
        );
        assert_eq!(uri_to_path("file:///c:/x").as_deref(), Some("c:/x"));
        assert_eq!(
            uri_to_path("file:///home/u/p").as_deref(),
            Some("/home/u/p")
        );
        assert_eq!(uri_to_path("file:///c%3A/100%").as_deref(), Some("c:/100%"));
        assert_eq!(uri_to_path("https://x"), None);
        assert_eq!(
            first_workspace(r#"["file:///c%3A/w/p","file:///c%3A/other"]"#).as_deref(),
            Some("c:/w/p")
        );
        assert_eq!(first_workspace("[]"), None);
        assert_eq!(first_workspace("nope"), None);
    }

    #[test]
    fn classifies_only_conversation_dbs_of_the_two_app_dirs() {
        let g = Path::new("C:/h/.gemini");
        for ok in [
            "C:/h/.gemini/antigravity-cli/conversations/a.db",
            "C:/h/.gemini/antigravity/conversations/a.db-wal",
            "C:/h/.gemini/antigravity/conversations/a.db-shm",
        ] {
            assert!(is_conversation_db(g, Path::new(ok)), "{ok}");
        }
        for no in [
            "C:/h/.gemini/antigravity/conversations/a.pb",
            "C:/h/.gemini/antigravity/conversation_summaries.db",
            "C:/h/.gemini/antigravity/brain/a.db",
            "C:/h/.gemini/other/conversations/a.db",
            "C:/h/.gemini/antigravity/conversations/sub/a.db",
            "C:/elsewhere/antigravity/conversations/a.db",
        ] {
            assert!(!is_conversation_db(g, Path::new(no)), "{no}");
        }
    }
}
