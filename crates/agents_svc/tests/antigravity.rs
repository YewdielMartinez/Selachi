//! Antigravity ingestion against synthetic `~/.gemini` trees that mirror the real schema
//! (conversation databases with `gen_metadata` / `steps` protobuf blobs, `conversation_summaries.db`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use agents_svc::{Agent, IngestConfig, Ingestor, Range, SourceKind};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

fn varint(mut v: u64, out: &mut Vec<u8>) {
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
fn f_int(f: u32, v: u64, out: &mut Vec<u8>) {
    varint(u64::from(f) << 3, out);
    varint(v, out);
}
fn f_bytes(f: u32, b: &[u8], out: &mut Vec<u8>) {
    varint((u64::from(f) << 3) | 2, out);
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}
/// `[model enum, input, output, cache_read, thinking, response]`
fn usage(u: [u64; 6]) -> Vec<u8> {
    let mut o = vec![];
    f_int(1, u[0], &mut o);
    f_int(2, u[1], &mut o);
    f_int(3, u[2], &mut o);
    if u[3] > 0 {
        f_int(5, u[3], &mut o);
    }
    f_int(6, 26, &mut o);
    if u[4] > 0 {
        f_int(9, u[4], &mut o);
    }
    f_int(10, u[5], &mut o);
    o
}
fn gen_blob(model: &str, u: [u64; 6]) -> Vec<u8> {
    let mut g = vec![];
    f_bytes(4, &usage(u), &mut g);
    f_bytes(19, model.as_bytes(), &mut g);
    let mut top = vec![];
    f_bytes(1, &g, &mut top);
    top
}
fn step_blob(ts: u64, u: Option<[u64; 6]>) -> Vec<u8> {
    let mut t = vec![];
    f_int(1, ts, &mut t);
    f_int(2, 1, &mut t);
    let mut top = vec![];
    f_bytes(1, &t, &mut top);
    if let Some(u) = u {
        f_bytes(9, &usage(u), &mut top);
    }
    top
}

struct Tree {
    gemini: PathBuf,
    app: PathBuf,
}

impl Tree {
    fn new(root: &Path, app: &str) -> Tree {
        let gemini = root.join(".gemini");
        let app = gemini.join(app);
        std::fs::create_dir_all(app.join("conversations")).unwrap();
        let s = Connection::open(app.join("conversation_summaries.db")).unwrap();
        s.execute_batch("CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY, workspace_uris text NOT NULL)").unwrap();
        Tree { gemini, app }
    }
    fn workspace(&self, id: &str, uri: &str) {
        let s = Connection::open(self.app.join("conversation_summaries.db")).unwrap();
        s.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2)",
            params![id, format!("[\"{uri}\"]")],
        )
        .unwrap();
    }
    fn db(&self, id: &str) -> PathBuf {
        self.app.join("conversations").join(format!("{id}.db"))
    }
    fn conv(&self, id: &str) -> Connection {
        let c = Connection::open(self.db(id)).unwrap();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS gen_metadata (idx integer PRIMARY KEY, data blob, size integer NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS steps (idx integer PRIMARY KEY, step_type integer NOT NULL DEFAULT 0, metadata blob);",
        )
        .unwrap();
        c
    }
    fn cfg(&self) -> IngestConfig {
        IngestConfig { antigravity_data: Some(self.gemini.clone()), ..Default::default() }
    }
}

fn add_gen(c: &Connection, idx: i64, model: &str, u: [u64; 6], ts: u64) {
    c.execute("INSERT INTO gen_metadata(idx, data) VALUES (?1, ?2)", params![idx, gen_blob(model, u)]).unwrap();
    // a plain step first, then the model-response step carrying the same usage and the time
    c.execute("INSERT INTO steps(idx, metadata) VALUES (?1, ?2)", params![idx * 2, step_blob(ts - 1, None)]).unwrap();
    c.execute("INSERT INTO steps(idx, metadata) VALUES (?1, ?2)", params![idx * 2 + 1, step_blob(ts, Some(u))]).unwrap();
}

/// `agent, model, ts, input, output, cache_read, reasoning, cwd, reported_cost`
type Row = (String, String, i64, i64, i64, i64, i64, Option<String>, Option<f64>);

const T0: u64 = 1_795_000_000;
const A: [u64; 6] = [1401, 11_276, 221, 8_401, 0, 221];
const B: [u64; 6] = [1401, 1_965, 1_830, 18_459, 803, 1_027];

fn ingestor(tmp: &Path, cfg: IngestConfig) -> Ingestor {
    Ingestor::open(&tmp.join("selachi.db"), cfg).unwrap()
}

#[test]
fn ingests_generations_with_model_time_cwd_and_no_cost() {
    let tmp = tempfile::tempdir().unwrap();
    let t = Tree::new(tmp.path(), "antigravity-cli");
    t.workspace("conv-1", "file:///c%3A/work/my%20proj");
    let c = t.conv("conv-1");
    add_gen(&c, 0, "claude-opus-5-5-medium", A, T0);
    add_gen(&c, 1, "claude-opus-5-5-medium", B, T0 + 60);
    // a legacy encrypted blob next to it is ignored, not an error
    std::fs::write(t.app.join("conversations").join("old.pb"), [7u8; 64]).unwrap();
    drop(c);

    let mut ing = ingestor(tmp.path(), t.cfg());
    let rep = ing.scan().unwrap();
    assert_eq!(rep.antigravity_files, 1);
    assert_eq!(rep.events_inserted, 2);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);

    let rows: Vec<Row> = ing
        .store()
        .conn()
        .prepare("SELECT agent, model, ts, input, output, cache_read, reasoning, cwd, reported_cost FROM usage_events ORDER BY ts")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 2);
    let (agent, model, ts, input, output, cache_read, reasoning, cwd, cost) = rows[1].clone();
    assert_eq!((agent.as_str(), model.as_str()), ("antigravity", "claude-opus-5-5-medium"));
    assert_eq!(ts, (T0 as i64 + 60) * 1000);
    assert_eq!((input, output, cache_read, reasoning), (1_965, 1_830, 18_459, 803));
    assert_eq!(cwd.as_deref(), Some("c:/work/my proj"));
    assert_eq!(cost, None, "Antigravity reports no cost");

    // the summary groups it under its own agent, tokens only, no estimated cost
    let sum = ing.store().usage_summary(&Range::All).unwrap();
    let agy = sum.iter().find(|u| u.agent == Agent::Antigravity).unwrap();
    assert_eq!(agy.events, 2);
    assert_eq!(agy.est_cost_usd, None);
    assert_eq!(agy.model, "claude-opus-5-5-medium");
}

#[test]
fn second_scan_is_incremental_and_picks_up_appended_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let t = Tree::new(tmp.path(), "antigravity");
    let c = t.conv("conv-2");
    add_gen(&c, 0, "gemini-4-argon", A, T0);
    let mut ing = ingestor(tmp.path(), t.cfg());
    assert_eq!(ing.scan().unwrap().events_inserted, 1);

    let r = ing.scan().unwrap();
    assert_eq!((r.files_unchanged, r.files_scanned, r.events_inserted), (1, 0, 0), "unchanged db is skipped");

    add_gen(&c, 1, "gemini-4-argon", B, T0 + 5);
    let r = ing.scan().unwrap();
    assert_eq!(r.files_scanned, 1);
    assert_eq!(r.events_inserted, 1, "only the new generation is new");
    assert_eq!(r.duplicates_skipped, 1, "the boundary generation is re-read idempotently");
    let n: i64 = ing.store().conn().query_row("SELECT count(*) FROM usage_events WHERE agent='antigravity'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);

    // a generation rewritten in place (streaming) is replaced, not duplicated
    c.execute("UPDATE gen_metadata SET data = ?1 WHERE idx = 1", params![gen_blob("gemini-4-argon", [1401, 1_965, 2_500, 18_459, 803, 1_697])]).unwrap();
    let r = ing.scan().unwrap();
    assert_eq!(r.events_updated, 1);
    let out: i64 = ing.store().conn().query_row("SELECT output FROM usage_events WHERE msg_key LIKE '%conv-2:1'", [], |r| r.get(0)).unwrap();
    assert_eq!(out, 2_500);
}

#[test]
fn unnamed_models_and_zero_rows_and_foreign_schemas_are_handled() {
    let tmp = tempfile::tempdir().unwrap();
    let t = Tree::new(tmp.path(), "antigravity-cli");
    let c = t.conv("conv-3");
    // no model name -> enum label; all-zero row -> skipped; undecodable row -> skipped
    c.execute("INSERT INTO gen_metadata(idx, data) VALUES (0, ?1)", params![{
        let mut g = vec![];
        f_bytes(4, &usage(A), &mut g);
        let mut top = vec![];
        f_bytes(1, &g, &mut top);
        top
    }]).unwrap();
    c.execute("INSERT INTO gen_metadata(idx, data) VALUES (1, ?1)", params![gen_blob("m", [1, 0, 0, 0, 0, 0])]).unwrap();
    c.execute("INSERT INTO gen_metadata(idx, data) VALUES (2, ?1)", params![vec![0xffu8, 0xff, 0xff]]).unwrap();
    // a database from another product / version: error for that file only
    let other = Connection::open(t.db("foreign")).unwrap();
    other.execute_batch("CREATE TABLE message (id text)").unwrap();
    drop(other);
    drop(c);
    let mut ing = ingestor(tmp.path(), t.cfg());
    let rep = ing.scan().unwrap();
    assert_eq!(rep.events_inserted, 1);
    assert_eq!(rep.synthetic_skipped, 2);
    assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
    assert!(rep.errors[0].contains("foreign"));
    let m: String = ing.store().conn().query_row("SELECT model FROM usage_events", [], |r| r.get(0)).unwrap();
    assert_eq!(m, "antigravity-model-1401");
    // time unknown anywhere -> the database's mtime, never 0
    let ts: i64 = ing.store().conn().query_row("SELECT ts FROM usage_events", [], |r| r.get(0)).unwrap();
    assert!(ts > 1_700_000_000_000);
}

#[test]
fn watch_dirs_classify_and_scan_paths_cover_antigravity() {
    let tmp = tempfile::tempdir().unwrap();
    let t = Tree::new(tmp.path(), "antigravity-cli");
    let c = t.conv("conv-4");
    add_gen(&c, 0, "gemini-4-neon", A, T0);
    drop(c);
    let cfg = t.cfg();
    let dirs = cfg.watch_dirs();
    assert_eq!(dirs, vec![(t.app.join("conversations"), false)]);
    let db = t.db("conv-4");
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    assert_eq!(cfg.classify(&db), Some(SourceKind::AntigravityDb));
    assert_eq!(cfg.classify(&PathBuf::from(&wal)), Some(SourceKind::AntigravityDb));
    assert_eq!(cfg.classify(&t.app.join("conversation_summaries.db")), None);
    assert_eq!(cfg.classify(&t.app.join("conversations").join("x.pb")), None);

    let mut ing = ingestor(tmp.path(), cfg);
    // a -wal event resolves to the database; the same db twice is read once
    let rep = ing.scan_paths(&[PathBuf::from(&wal), db, tmp.path().join("nothing.db")]).unwrap();
    assert_eq!((rep.antigravity_files, rep.events_inserted), (1, 1));
    // absent directory: no watch entry, no scan error
    let none = IngestConfig { antigravity_data: Some(tmp.path().join("missing")), ..Default::default() };
    assert!(none.watch_dirs().is_empty());
    assert!(ingestor(tmp.path(), none).scan().unwrap().errors.is_empty());
}

#[test]
fn agent_name_roundtrips() {
    assert_eq!(Agent::parse("antigravity"), Some(Agent::Antigravity));
    assert_eq!(Agent::Antigravity.to_string(), "antigravity");
}

/// Live: scans the real `~/.gemini` read-only into a temp store and prints totals only (never any
/// message content). `cargo test -p agents_svc --test antigravity -- --ignored --nocapture`
#[test]
#[ignore = "reads the real ~/.gemini conversation databases (read-only)"]
fn live_scan_of_the_real_gemini_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ing = ingestor(tmp.path(), IngestConfig::from_home());
    let t = std::time::Instant::now();
    let rep = ing.scan().unwrap();
    eprintln!("first scan {:?}: {rep:?}", t.elapsed());
    let (n, input, output, cache, reasoning): (i64, i64, i64, i64, i64) = ing
        .store()
        .conn()
        .query_row(
            "SELECT count(*), coalesce(sum(input),0), coalesce(sum(output),0), coalesce(sum(cache_read),0), coalesce(sum(reasoning),0) FROM usage_events WHERE agent='antigravity'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    eprintln!("antigravity events {n}: input {input} output {output} cache_read {cache} reasoning {reasoning}");
    let models: Vec<(String, i64)> = ing
        .store()
        .conn()
        .prepare("SELECT model, count(*) FROM usage_events WHERE agent='antigravity' GROUP BY model ORDER BY 2 DESC LIMIT 8")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    eprintln!("models: {models:?}");
    let t = std::time::Instant::now();
    let again = ing.scan().unwrap();
    eprintln!("second scan {:?}: scanned {} unchanged {} inserted {}", t.elapsed(), again.files_scanned, again.files_unchanged, again.events_inserted);
    // an IDE may be writing right now, so only unrelated failures are asserted
    assert!(rep.errors.is_empty() && again.errors.is_empty(), "{:?} {:?}", rep.errors, again.errors);
}
