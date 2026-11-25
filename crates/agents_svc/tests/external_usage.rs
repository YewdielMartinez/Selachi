#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `Store::usage_for_external`: which ingested session an external agent run belongs to, from
//! synthetic fixtures only (no real session content).

use agents_svc::model::{TokenCounts, UsageEvent, UsageExtras};
use agents_svc::{Agent, IngestConfig, Ingestor, Store};
use chrono::{DateTime, Utc};
use std::fs;
use std::io::Write;
use std::path::Path;

fn ms(ts: &str) -> i64 {
    DateTime::parse_from_rfc3339(ts)
        .unwrap()
        .with_timezone(&Utc)
        .timestamp_millis()
}

fn claude_line(ts: &str, session: &str, msg: &str, out: u64, cwd: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"{session}","cwd":"{}","gitBranch":"main","requestId":"r-{msg}","uuid":"u-{msg}","message":{{"id":"{msg}","model":"claude-opus-5-5","usage":{{"input_tokens":10,"output_tokens":{out},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"content":[]}}}}"#,
        cwd.replace('\\', "\\\\")
    )
}

fn write_lines(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = fs::File::create(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn ingest(dir: &Path) -> Store {
    let mut ing = Ingestor::open(
        &dir.join("db").join("t.db"),
        IngestConfig {
            claude_projects: Some(dir.join("claude")),
            codex_sessions: Some(dir.join("codex")),
            ..Default::default()
        },
    )
    .unwrap();
    ing.scan().unwrap();
    ing.into_store()
}

#[test]
fn finds_the_claude_session_of_that_cwd_inside_the_window() {
    let tmp = tempfile::tempdir().unwrap();
    write_lines(
        &tmp.path().join("claude/p/a.jsonl"),
        &[
            // before the window
            claude_line("2026-05-01T10:00:00Z", "sess-a", "m1", 100, r"C:\work\app"),
            // inside
            claude_line("2026-05-01T11:00:00Z", "sess-a", "m2", 200, r"C:\work\app"),
            claude_line("2026-05-01T11:05:00Z", "sess-a", "m3", 300, r"C:\work\app"),
            // after
            claude_line("2026-05-01T13:00:00Z", "sess-a", "m4", 400, r"C:\work\app"),
        ],
    );
    write_lines(
        &tmp.path().join("claude/q/b.jsonl"),
        &[
            // another project in the same window: must not be picked for C:\work\app
            claude_line(
                "2026-05-01T11:01:00Z",
                "sess-b",
                "m5",
                9000,
                r"C:\work\other",
            ),
            // a session started in a subfolder counts as that cwd
            claude_line(
                "2026-05-01T11:02:00Z",
                "sess-c",
                "m6",
                50,
                r"C:\work\app\sub",
            ),
        ],
    );
    let store = ingest(tmp.path());
    let est = store
        .usage_for_external(
            Agent::Claude,
            r"C:\work\app",
            ms("2026-05-01T10:30:00Z"),
            ms("2026-05-01T12:00:00Z"),
        )
        .unwrap();
    // sess-a used the most inside the window: 2 responses, 10 + 10 input, 200 + 300 output
    assert_eq!(est.session_id, "sess-a");
    assert_eq!(est.events, 2);
    assert_eq!(est.tokens.output, 500);
    assert_eq!(est.tokens.input, 20);
    assert_eq!(est.total_tokens(), 520);
    assert_eq!(est.billable_tokens(), 520);
    assert!(est.cwd_matched);
    assert!(est.cost.is_some_and(|c| c > 0.0), "{:?}", est.cost);
    assert_eq!(est.first_ts_ms, ms("2026-05-01T11:00:00Z"));
    assert_eq!(est.last_ts_ms, ms("2026-05-01T11:05:00Z"));

    // case and slash style do not matter
    assert!(store
        .usage_for_external(
            Agent::Claude,
            "c:/WORK/app/",
            ms("2026-05-01T10:30:00Z"),
            ms("2026-05-01T12:00:00Z")
        )
        .is_some());
    // a window with no events, another agent, another folder, an inverted window: nothing
    let w = (ms("2026-05-01T10:30:00Z"), ms("2026-05-01T12:00:00Z"));
    assert!(store
        .usage_for_external(
            Agent::Claude,
            r"C:\work\app",
            ms("2026-05-02T00:00:00Z"),
            ms("2026-05-02T01:00:00Z")
        )
        .is_none());
    assert!(store
        .usage_for_external(Agent::Codex, r"C:\work\app", w.0, w.1)
        .is_none());
    assert!(store
        .usage_for_external(Agent::Claude, r"C:\work\nothing", w.0, w.1)
        .is_none());
    assert!(store
        .usage_for_external(Agent::Claude, r"C:\work\app", w.1, w.0)
        .is_none());
    // `C:\work\ap` is not a parent of `C:\work\app` (segment aware)
    assert!(store
        .usage_for_external(Agent::Claude, r"C:\work\ap", w.0, w.1)
        .is_none());
}

fn event(
    agent: Agent,
    session: &str,
    ts: i64,
    cwd: Option<&str>,
    key: &str,
    out: u64,
) -> UsageEvent {
    UsageEvent {
        agent,
        session_id: session.into(),
        ts_ms: ts,
        model: "some-model".into(),
        tokens: TokenCounts {
            input: 100,
            output: out,
            ..Default::default()
        },
        cwd: cwd.map(str::to_string),
        git_branch: None,
        msg_key: key.into(),
    }
}

#[test]
fn reported_cost_wins_and_cwdless_agents_need_an_unambiguous_session() {
    let store = Store::open_in_memory().unwrap();
    // OpenCode reports its own cost
    store
        .upsert_usage_replace(
            &event(Agent::OpenCode, "oc1", 1_000, Some(r"C:\w\x"), "oc-1", 10),
            &UsageExtras {
                reasoning: 0,
                reported_cost_usd: Some(0.25),
                cost_source: Some("opencode".into()),
                provider: None,
            },
        )
        .unwrap();
    store
        .upsert_usage_replace(
            &event(Agent::OpenCode, "oc1", 2_000, Some(r"C:\w\x"), "oc-2", 20),
            &UsageExtras {
                reasoning: 0,
                reported_cost_usd: Some(0.5),
                cost_source: Some("opencode".into()),
                provider: None,
            },
        )
        .unwrap();
    let est = store
        .usage_for_external(Agent::OpenCode, r"C:\w\x", 0, 5_000)
        .unwrap();
    assert_eq!(est.session_id, "oc1");
    assert_eq!(est.cost, Some(0.75));
    assert_eq!(est.tokens.output, 30);

    // Antigravity logs no folder: a single session in the window is taken, flagged as not matched
    store
        .upsert_usage(&event(Agent::Antigravity, "ag1", 1_000, None, "ag-1", 5))
        .unwrap();
    let est = store
        .usage_for_external(Agent::Antigravity, r"C:\anything", 0, 5_000)
        .unwrap();
    assert_eq!(est.session_id, "ag1");
    assert!(!est.cwd_matched);
    // unknown model: tokens yes, cost no (never a guess)
    assert_eq!(est.cost, None);
    // two sessions at once: ambiguous, nothing
    store
        .upsert_usage(&event(Agent::Antigravity, "ag2", 1_500, None, "ag-2", 7))
        .unwrap();
    assert!(store
        .usage_for_external(Agent::Antigravity, r"C:\anything", 0, 5_000)
        .is_none());
}

#[test]
fn the_free_function_never_creates_the_database() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("none.db");
    assert!(agents_svc::usage_for_external(&db, Agent::Claude, "C:/x", 0, 10).is_none());
    assert!(!db.exists());
    {
        let store = Store::open(&db).unwrap();
        store
            .upsert_usage(&event(Agent::Codex, "c1", 5, Some("C:/x"), "k1", 3))
            .unwrap();
    }
    let est = agents_svc::usage_for_external(&db, Agent::Codex, "C:/x", 0, 10).unwrap();
    assert_eq!(est.session_id, "c1");
}
