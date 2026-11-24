#![allow(clippy::unwrap_used, clippy::expect_used)]
//! OpenCode ingestion against synthetic storage that mirrors the real schema
//! (opencode 1.14.48: `message.data` JSON, `session.directory`, `workspace.branch`; plus the
//! legacy `storage/message/<session>/<message>.json` layout). No real content is used.

use agents_svc::{Agent, IngestConfig, Ingestor, PricingTable, Range};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The subset of OpenCode's real DDL the reader touches (copied from `sqlite_master`).
const OC_SCHEMA: &str = r#"
CREATE TABLE `project` (`id` text PRIMARY KEY, `worktree` text NOT NULL, `time_created` integer NOT NULL, `time_updated` integer NOT NULL);
CREATE TABLE `workspace` (`id` text PRIMARY KEY, `type` text NOT NULL, `name` text DEFAULT '' NOT NULL, `branch` text, `directory` text, `extra` text, `project_id` text NOT NULL);
CREATE TABLE `session` (`id` text PRIMARY KEY, `project_id` text NOT NULL, `parent_id` text, `slug` text NOT NULL, `directory` text NOT NULL, `title` text NOT NULL, `version` text NOT NULL, `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `workspace_id` text);
CREATE TABLE `message` (`id` text PRIMARY KEY, `session_id` text NOT NULL, `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL);
"#;

struct Oc {
    data_dir: PathBuf,
    conn: Connection,
}

impl Oc {
    fn new(root: &Path) -> Oc {
        let data_dir = root.join("opencode");
        fs::create_dir_all(&data_dir).unwrap();
        let conn = Connection::open(data_dir.join("opencode.db")).unwrap();
        let _: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch(OC_SCHEMA).unwrap();
        Oc { data_dir, conn }
    }

    fn session(&self, id: &str, dir: &str, workspace_branch: Option<&str>) {
        let ws = workspace_branch.map(|_| format!("wrk_{id}"));
        if let (Some(w), Some(b)) = (&ws, workspace_branch) {
            self.conn
                .execute(
                    "INSERT INTO workspace(id, type, branch, directory, project_id) VALUES (?1,'worktree',?2,?3,'prj')",
                    params![w, b, dir],
                )
                .unwrap();
        }
        self.conn
            .execute(
                "INSERT INTO session(id, project_id, slug, directory, title, version, time_created, time_updated, workspace_id) \
                 VALUES (?1,'prj','s',?2,'t','1.14.48',0,0,?3)",
                params![id, dir, ws],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn assistant(
        &self,
        id: &str,
        session: &str,
        ts: i64,
        updated: i64,
        model: &str,
        provider: &str,
        cost: f64,
        tok: [u64; 5],
    ) {
        let data = json!({
            "role": "assistant",
            "time": {"created": ts, "completed": ts + 10},
            "parentID": "msg_user", "modelID": model, "providerID": provider, "mode": "build", "agent": "build",
            "path": {"cwd": "C:\\work\\proj", "root": "C:\\work\\proj"},
            "cost": cost,
            "tokens": {"input": tok[0], "output": tok[1], "reasoning": tok[2], "cache": {"read": tok[3], "write": tok[4]}},
            "finish": "stop"
        });
        self.put(id, session, ts, updated, &data);
    }

    fn put(&self, id: &str, session: &str, created: i64, updated: i64, data: &Value) {
        self.conn
            .execute(
                "INSERT INTO message(id, session_id, time_created, time_updated, data) VALUES (?1,?2,?3,?4,?5) \
                 ON CONFLICT(id) DO UPDATE SET time_updated=?4, data=?5",
                params![id, session, created, updated, data.to_string()],
            )
            .unwrap();
    }

    fn user(&self, id: &str, session: &str, ts: i64) {
        self.put(id, session, ts, ts, &json!({"role":"user","time":{"created":ts},"agent":"build","model":{"providerID":"anthropic","modelID":"x"}}));
    }
}

fn ingestor(tmp: &Path, oc: &Path) -> Ingestor {
    let mut ing = Ingestor::open(
        &tmp.join("selachi.db"),
        IngestConfig {
            opencode_data: Some(oc.to_path_buf()),
            ..Default::default()
        },
    )
    .unwrap();
    // deterministic: nothing in the embedded table is agent-less, so OpenCode events are only
    // priced by their reported cost
    ing.store_mut().set_pricing(PricingTable::embedded());
    ing
}

const T0: i64 = 1_760_000_000_000;

#[test]
fn reads_assistant_messages_with_session_context_and_reported_cost() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = Oc::new(tmp.path());
    oc.session("ses_a", "C:\\work\\proj", Some("feature/x"));
    oc.session("ses_b", "C:\\work\\proj", None);
    oc.user("msg_u1", "ses_a", T0);
    oc.assistant(
        "msg_1",
        "ses_a",
        T0 + 1,
        T0 + 20,
        "claude-sonnet-4-5",
        "anthropic",
        0.5,
        [100, 200, 50, 1000, 20],
    );
    oc.assistant(
        "msg_2",
        "ses_b",
        T0 + 2,
        T0 + 21,
        "big-pickle",
        "opencode",
        0.0,
        [10, 20, 0, 0, 0],
    );

    let mut ing = ingestor(tmp.path(), &oc.data_dir);
    let rep = ing.scan().unwrap();
    assert_eq!(rep.opencode_files, 1);
    assert_eq!(rep.events_inserted, 2, "{rep:?}");
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);

    let sum = ing.store().usage_summary(&Range::All).unwrap();
    assert_eq!(sum.len(), 2);
    let sonnet = sum.iter().find(|u| u.model == "claude-sonnet-4-5").unwrap();
    assert_eq!(sonnet.agent, Agent::OpenCode);
    assert_eq!(
        (
            sonnet.input,
            sonnet.output,
            sonnet.cache_read,
            sonnet.cache_create
        ),
        (100, 250, 1000, 20),
        "reasoning folded into output"
    );
    assert_eq!(sonnet.est_cost_usd, Some(0.5), "reported cost preferred");
    let pickle = sum.iter().find(|u| u.model == "big-pickle").unwrap();
    assert_eq!(
        pickle.est_cost_usd, None,
        "free/unknown model: never guessed"
    );

    let rc = ing.store().reported_costs(&Range::All).unwrap();
    assert_eq!(rc.len(), 1);
    assert_eq!(
        (
            rc[0].agent,
            rc[0].source.as_str(),
            rc[0].cost_usd,
            rc[0].events
        ),
        (Agent::OpenCode, "opencode", 0.5, 1)
    );

    // extras landed in the new columns
    let (reasoning, provider, source): (i64, String, String) = ing
        .store()
        .conn()
        .query_row(
            "SELECT reasoning, provider, cost_source FROM usage_events WHERE msg_key='opencode:msg_1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (reasoning, provider.as_str(), source.as_str()),
        (50, "anthropic", "opencode")
    );

    // per branch: the workspace branch where the session has one, NULL otherwise
    let br = ing.store().usage_by_branch("C:/work", &Range::All).unwrap();
    assert_eq!(br.len(), 2);
    let feature = br
        .iter()
        .find(|b| b.git_branch.as_deref() == Some("feature/x"))
        .unwrap();
    assert_eq!(feature.est_cost_usd, Some(0.5));
    assert!(feature.cost_complete);
    let none = br.iter().find(|b| b.git_branch.is_none()).unwrap();
    assert_eq!(none.est_cost_usd, None);
    assert!(!none.cost_complete);

    // burn rate counts OpenCode tokens (all rows are far in the past: window anchored there)
    let rate = ing
        .store()
        .burn_rate_at(
            Some(Agent::OpenCode),
            Duration::from_secs(3600),
            T0 + 60_000,
        )
        .unwrap();
    assert_eq!(rate.tokens, 100 + 250 + 20 + 1000 + 10 + 20);
    assert_eq!(rate.billable_tokens, 100 + 250 + 20 + 10 + 20);
}

#[test]
fn incremental_scans_skip_unchanged_and_replace_grown_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = Oc::new(tmp.path());
    oc.session("ses_a", "C:\\work\\proj", None);
    oc.assistant(
        "msg_1",
        "ses_a",
        T0,
        T0 + 5,
        "claude-sonnet-4-5",
        "anthropic",
        0.1,
        [10, 10, 0, 0, 0],
    );

    let mut ing = ingestor(tmp.path(), &oc.data_dir);
    assert_eq!(ing.scan().unwrap().events_inserted, 1);

    // nothing changed: the database fingerprint short-circuits
    let r2 = ing.scan().unwrap();
    assert_eq!(
        (r2.files_unchanged, r2.events_inserted, r2.lines),
        (1, 0, 0),
        "{r2:?}"
    );

    // OpenCode rewrites the same message in place as steps finish, and adds a new one
    oc.assistant(
        "msg_1",
        "ses_a",
        T0,
        T0 + 50,
        "claude-sonnet-4-5",
        "anthropic",
        0.3,
        [30, 40, 0, 0, 0],
    );
    oc.assistant(
        "msg_2",
        "ses_a",
        T0 + 100,
        T0 + 120,
        "claude-sonnet-4-5",
        "anthropic",
        0.2,
        [5, 5, 0, 0, 0],
    );
    let r3 = ing.scan().unwrap();
    assert_eq!((r3.events_inserted, r3.events_updated), (1, 1), "{r3:?}");
    let sum = ing.store().usage_summary(&Range::All).unwrap();
    assert_eq!(sum.len(), 1);
    assert_eq!(
        (sum[0].input, sum[0].output, sum[0].events),
        (35, 45, 2),
        "no double counting"
    );
    assert!((sum[0].est_cost_usd.unwrap() - 0.5).abs() < 1e-9);

    // a fresh Ingestor on the same database keeps the cursor
    drop(ing);
    let mut again = ingestor(tmp.path(), &oc.data_dir);
    let r4 = again.scan().unwrap();
    assert_eq!(
        (r4.files_unchanged, r4.events_inserted, r4.events_updated),
        (1, 0, 0),
        "{r4:?}"
    );

    // re-reading the boundary millisecond is a no-op, never a double count
    oc.user("msg_u", "ses_a", T0 + 200);
    let r5 = again.scan().unwrap();
    assert_eq!((r5.events_inserted, r5.events_updated), (0, 0), "{r5:?}");
    assert_eq!(
        again.store().usage_summary(&Range::All).unwrap()[0].events,
        2
    );
}

#[test]
fn large_databases_are_read_in_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = Oc::new(tmp.path());
    oc.session("ses_a", "C:\\work\\proj", None);
    oc.conn.execute_batch("BEGIN").unwrap();
    for i in 0..4500 {
        // many rows share a millisecond to exercise the (time_updated, id) keyset
        oc.assistant(
            &format!("msg_{i:05}"),
            "ses_a",
            T0 + i / 10,
            T0 + i / 10,
            "m",
            "p",
            0.0,
            [1, 1, 0, 0, 0],
        );
    }
    oc.conn.execute_batch("COMMIT").unwrap();
    let mut ing = ingestor(tmp.path(), &oc.data_dir);
    let rep = ing.scan().unwrap();
    assert_eq!(rep.events_inserted, 4500, "{rep:?}");
    let sum = ing.store().usage_summary(&Range::All).unwrap();
    assert_eq!((sum[0].events, sum[0].input), (4500, 4500));
}

#[test]
fn legacy_json_storage_and_db_dedupe_by_message_id() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = Oc::new(tmp.path());
    oc.session("ses_a", "C:\\work\\proj", None);
    oc.assistant(
        "msg_1",
        "ses_a",
        T0,
        T0 + 5,
        "claude-sonnet-4-5",
        "anthropic",
        0.25,
        [10, 20, 0, 0, 0],
    );

    let msg_dir = oc.data_dir.join("storage").join("message").join("ses_a");
    fs::create_dir_all(&msg_dir).unwrap();
    let legacy = |id: &str, ts: i64, out: u64| {
        json!({
            "id": id, "sessionID": "ses_a", "role": "assistant", "time": {"created": ts, "completed": ts + 1},
            "parentID": "msg_u", "modelID": "claude-sonnet-4-5", "providerID": "anthropic", "mode": "build", "agent": "build",
            "path": {"cwd": "C:\\work\\proj", "root": "C:\\work\\proj"}, "cost": 0.25,
            "tokens": {"input": 10, "output": out, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        })
        .to_string()
    };
    // the same message as in the database (the SQLite migration keeps the JSON), plus an old one
    fs::write(msg_dir.join("msg_1.json"), legacy("msg_1", T0, 20)).unwrap();
    fs::write(
        msg_dir.join("msg_old.json"),
        legacy("msg_old", T0 - 1000, 5),
    )
    .unwrap();
    fs::write(msg_dir.join("garbage.json"), "{not json").unwrap();

    let mut ing = ingestor(tmp.path(), &oc.data_dir);
    let rep = ing.scan().unwrap();
    assert_eq!(rep.opencode_files, 4);
    assert_eq!(rep.events_inserted, 2, "{rep:?}");
    assert_eq!(rep.malformed_lines, 1);
    let sum = ing.store().usage_summary(&Range::All).unwrap();
    assert_eq!(sum[0].events, 2);
    assert_eq!(sum[0].output, 25);

    // unchanged files are skipped by (size, mtime)
    let r2 = ing.scan().unwrap();
    assert_eq!(r2.files_unchanged, 4, "{r2:?}");
    assert_eq!(r2.events_inserted + r2.events_updated, 0);
}

#[test]
fn reads_a_database_another_process_is_writing_and_survives_bad_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = Oc::new(tmp.path());
    oc.session("ses_a", "C:\\work\\proj", None);
    // OpenCode is "running": WAL mode, writer connection open, nothing checkpointed
    oc.assistant("msg_1", "ses_a", T0, T0 + 5, "m", "p", 0.0, [7, 7, 0, 0, 0]);
    assert!(oc.data_dir.join("opencode.db-wal").exists());

    // a second database of an unsupported shape must not abort the scan
    let bad = Connection::open(oc.data_dir.join("opencode-bad.db")).unwrap();
    bad.execute_batch("CREATE TABLE other(x)").unwrap();
    drop(bad);

    let mut ing = ingestor(tmp.path(), &oc.data_dir);
    let rep = ing.scan().unwrap();
    assert_eq!(rep.events_inserted, 1, "{rep:?}");
    assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
    assert!(rep.errors[0].contains("opencode-bad.db"));
    // the source database was not modified by reading
    let n: i64 = oc
        .conn
        .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn opencode_and_other_agents_coexist_in_one_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = Oc::new(tmp.path());
    oc.session("ses_a", "C:\\work\\proj", None);
    oc.assistant(
        "msg_1",
        "ses_a",
        T0,
        T0 + 5,
        "claude-opus-5-5",
        "anthropic",
        0.0,
        [1_000_000, 0, 0, 0, 0],
    );
    let claude_dir = tmp.path().join("claude").join("p");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        claude_dir.join("s1.jsonl"),
        r#"{"type":"assistant","timestamp":"2025-10-09T09:00:00Z","sessionId":"s1","cwd":"C:\\work\\proj","gitBranch":"main","requestId":"r1","uuid":"u1","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":1000000,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"content":[]}}
"#
    )
    .unwrap();
    let mut ing = Ingestor::open(
        &tmp.path().join("selachi.db"),
        IngestConfig {
            claude_projects: Some(tmp.path().join("claude")),
            opencode_data: Some(oc.data_dir),
            ..Default::default()
        },
    )
    .unwrap();
    ing.scan().unwrap();
    let sum = ing.store().usage_summary(&Range::All).unwrap();
    assert_eq!(sum.len(), 2);
    let claude = sum.iter().find(|u| u.agent == Agent::Claude).unwrap();
    assert_eq!(claude.est_cost_usd, Some(4.0), "table-priced as before");
    let opencode = sum.iter().find(|u| u.agent == Agent::OpenCode).unwrap();
    assert_eq!(
        opencode.est_cost_usd, None,
        "an OpenCode event without a reported cost is not priced from a Claude-only table entry"
    );
    assert_eq!(Agent::parse("opencode"), Some(Agent::OpenCode));
    assert_eq!(Agent::OpenCode.to_string(), "opencode");
}

/// Reads the real local OpenCode data (read-only) and prints counts only, never content.
/// `cargo test -p agents_svc --test opencode -- --ignored --nocapture`
#[test]
#[ignore = "reads the real ~/.local/share/opencode"]
fn smoke_real_opencode_home() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ing = Ingestor::open(&tmp.path().join("smoke.db"), IngestConfig::from_home()).unwrap();
    let rep = ing.scan().unwrap();
    eprintln!(
        "opencode files {} inserted {} errors {:?}",
        rep.opencode_files, rep.events_inserted, rep.errors
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
}
