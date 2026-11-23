#![allow(clippy::unwrap_used, clippy::expect_used)]
use agents_svc::{Agent, IngestConfig, Ingestor, PricingTable, Range, ScanReport, Store};
use chrono::{FixedOffset, TimeZone, Utc, Weekday};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

// ---------- synthetic fixtures (no real content) ----------

#[allow(clippy::too_many_arguments)]
fn claude_line(
    ts: &str,
    session: &str,
    msg: &str,
    req: &str,
    out: u64,
    branch: &str,
    cwd: &str,
    tool: Option<(&str, &str, &str)>,
) -> String {
    let content = match tool {
        Some((id, name, path)) => format!(
            r#"[{{"type":"tool_use","id":"{id}","name":"{name}","input":{{"file_path":"{}"}}}}]"#,
            path.replace('\\', "\\\\")
        ),
        None => "[]".to_string(),
    };
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"{session}","cwd":"{}","gitBranch":"{branch}","requestId":"{req}","uuid":"u-{msg}-{out}","message":{{"id":"{msg}","model":"claude-opus-5-5","usage":{{"input_tokens":10,"output_tokens":{out},"cache_creation_input_tokens":1000,"cache_read_input_tokens":2000,"cache_creation":{{"ephemeral_1h_input_tokens":1000,"ephemeral_5m_input_tokens":0}}}},"content":{content}}}}}"#,
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

fn append(path: &Path, text: &str) {
    let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

fn ing(dir: &Path) -> Ingestor {
    Ingestor::open(
        &dir.join("db").join("t.db"),
        IngestConfig {
            claude_projects: Some(dir.join("claude")),
            codex_sessions: Some(dir.join("codex")),
            ..Default::default()
        },
    )
    .unwrap()
}

fn codex_lines(thread: &str, root: &str, base_ts: &str) -> Vec<String> {
    vec![
        format!(r#"{{"timestamp":"{base_ts}","type":"session_meta","payload":{{"id":"{thread}","session_id":"{root}","cwd":"C:\\proj\\x"}}}}"#),
        format!(r#"{{"timestamp":"{base_ts}","type":"turn_context","payload":{{"model":"gpt-6.1-sol","cwd":"C:\\proj\\x"}}}}"#),
        format!(r#"{{"timestamp":"{base_ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":1500}},"last_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":500,"reasoning_output_tokens":100,"total_tokens":1500}}}},"rate_limits":{{"primary":{{"used_percent":20.0,"window_minutes":10080,"resets_at":1893456000}},"secondary":null}}}}}}"#),
        // repeated event (only rate limit changed) -> must not double count
        format!(r#"{{"timestamp":"{base_ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":1500}},"last_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":500,"reasoning_output_tokens":100,"total_tokens":1500}}}},"rate_limits":{{"primary":{{"used_percent":21.0,"window_minutes":10080,"resets_at":1893456000}},"secondary":null}}}}}}"#),
        r#"{"timestamp":"2026-03-02T10:00:00.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","call_id":"c1","input":"*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** End Patch"}}"#.to_string(),
    ]
}

// ---------- tests ----------

#[test]
fn claude_dedupe_malformed_touches_and_streamed_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("claude/proj/s1.jsonl");
    write_lines(
        &f,
        &[
            // three streamed chunks of one response: 5 -> 40 -> 90 output tokens
            claude_line("2026-03-02T10:00:00.000Z", "s1", "m1", "r1", 5, "main", "C:\\proj\\x", Some(("tu1", "Edit", "C:\\proj\\x\\a.rs"))),
            claude_line("2026-03-02T10:00:01.000Z", "s1", "m1", "r1", 40, "main", "C:\\proj\\x", Some(("tu1", "Edit", "C:\\proj\\x\\a.rs"))),
            claude_line("2026-03-02T10:00:02.000Z", "s1", "m1", "r1", 90, "main", "C:\\proj\\x", Some(("tu1", "Edit", "C:\\proj\\x\\a.rs"))),
            "this is not json".to_string(),
            "{\"type\":\"assistant\", \"broken".to_string(),
            r#"{"type":"user","message":{"content":"hi"}}"#.to_string(),
            claude_line("2026-03-02T11:00:00.000Z", "s1", "m2", "r2", 7, "feat/y", "C:\\proj\\x", Some(("tu2", "Write", "C:\\proj\\x\\b.rs"))),
            r#"{"type":"assistant","timestamp":"2026-03-02T11:00:01.000Z","sessionId":"s1","message":{"id":"ms","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#.to_string(),
        ],
    );
    let mut ing = ing(tmp.path());
    let rep = ing.scan().unwrap();
    assert_eq!(rep.claude_files, 1);
    assert_eq!(rep.events_inserted, 2, "{rep:?}");
    assert_eq!(rep.events_updated, 2, "{rep:?}");
    assert_eq!(rep.malformed_lines, 2, "{rep:?}");
    assert_eq!(rep.synthetic_skipped, 1);
    assert_eq!(rep.touches_inserted, 2);
    assert!(rep.errors.is_empty());

    let s = ing.store();
    let all = s.usage_summary(&Range::All).unwrap();
    assert_eq!(all.len(), 1);
    // m1 counted once with the FINAL chunk's 90 output tokens, plus m2's 7.
    assert_eq!(all[0].output, 97);
    assert_eq!(all[0].input, 20);
    assert_eq!(all[0].cache_create, 2000);
    assert_eq!(all[0].cache_read, 4000);
    assert_eq!(all[0].events, 2);
    // opus-5-5: 20*4 + 97*20 + 2000*8(1h) + 4000*0.2 = 80+1940+16000+800 = 18820 / 1e6
    let c = all[0].est_cost_usd.unwrap();
    assert!((c - 0.01882).abs() < 1e-9, "{c}");

    let touched = s.files_touched("s1").unwrap();
    assert_eq!(touched.len(), 2);
    assert_eq!(touched[0].tool, "Edit");
    assert_eq!(touched[1].path, "C:\\proj\\x\\b.rs");

    // per-branch cost with a Windows-style, differently cased prefix
    let br = s.usage_by_branch("c:/PROJ/x", &Range::All).unwrap();
    assert_eq!(br.len(), 2);
    assert!(br.iter().any(|b| b.git_branch.as_deref() == Some("feat/y")));
    assert!(s
        .usage_by_branch("C:\\proj\\xother", &Range::All)
        .unwrap()
        .is_empty());
}

#[test]
fn unknown_model_has_no_cost() {
    let tmp = tempfile::tempdir().unwrap();
    let line = claude_line(
        "2026-03-02T10:00:00.000Z",
        "s",
        "m",
        "r",
        5,
        "b",
        "C:\\p",
        None,
    )
    .replace("claude-opus-5-5", "claude-mystery-1");
    write_lines(&tmp.path().join("claude/p/s.jsonl"), &[line]);
    let mut ing = ing(tmp.path());
    ing.scan().unwrap();
    let u = ing.store().usage_summary(&Range::All).unwrap();
    assert_eq!(u[0].est_cost_usd, None);
}

#[test]
fn incremental_offsets_read_only_new_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("claude/p/s.jsonl");
    write_lines(
        &f,
        &[claude_line(
            "2026-03-02T10:00:00.000Z",
            "s",
            "m1",
            "r1",
            5,
            "b",
            "C:\\p",
            None,
        )],
    );
    let mut ing = ing(tmp.path());
    let r1 = ing.scan().unwrap();
    assert_eq!(r1.events_inserted, 1);
    let size1 = fs::metadata(&f).unwrap().len();
    assert_eq!(r1.bytes_read, size1);

    // nothing changed -> file untouched
    let r2 = ing.scan().unwrap();
    assert_eq!(r2.files_unchanged, 1);
    assert_eq!(r2.files_scanned, 0);
    assert_eq!(r2.bytes_read, 0);

    // append one full line + a partial line (still being written)
    let second = claude_line(
        "2026-03-02T10:05:00.000Z",
        "s",
        "m2",
        "r2",
        9,
        "b",
        "C:\\p",
        None,
    );
    let partial = &claude_line(
        "2026-03-02T10:06:00.000Z",
        "s",
        "m3",
        "r3",
        11,
        "b",
        "C:\\p",
        None,
    );
    let (head, tail) = partial.split_at(partial.len() / 2);
    append(&f, &format!("{second}\n{head}"));
    let r3 = ing.scan().unwrap();
    assert_eq!(r3.events_inserted, 1, "{r3:?}");
    assert_eq!(r3.malformed_lines, 0);
    assert_eq!(
        r3.bytes_read,
        second.len() as u64 + 1,
        "only the new complete line is read"
    );

    // finish the partial line: it is now parsed (and only it)
    append(&f, &format!("{tail}\n"));
    let r4 = ing.scan().unwrap();
    assert_eq!(r4.events_inserted, 1, "{r4:?}");
    assert_eq!(r4.malformed_lines, 0);
    assert_eq!(ing.store().usage_summary(&Range::All).unwrap()[0].events, 3);

    // a brand-new Ingestor on the same DB must not re-read anything (offsets persisted)
    let db = tmp.path().join("db/t.db");
    drop(ing);
    let mut again = Ingestor::open(
        &db,
        IngestConfig {
            claude_projects: Some(tmp.path().join("claude")),
            codex_sessions: None,
            ..Default::default()
        },
    )
    .unwrap();
    let r5 = again.scan().unwrap();
    assert_eq!(r5.files_unchanged, 1);
    assert_eq!(r5.events_inserted, 0);
}

#[test]
fn shrunk_file_restarts_without_double_counting() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("claude/p/s.jsonl");
    let l1 = claude_line(
        "2026-03-02T10:00:00.000Z",
        "s",
        "m1",
        "r1",
        5,
        "b",
        "C:\\p",
        None,
    );
    let l2 = claude_line(
        "2026-03-02T10:01:00.000Z",
        "s",
        "m2",
        "r2",
        5,
        "b",
        "C:\\p",
        None,
    );
    write_lines(&f, &[l1.clone(), l2]);
    let mut ing = ing(tmp.path());
    ing.scan().unwrap();
    write_lines(&f, &[l1]); // truncated/rewritten
    let r = ing.scan().unwrap();
    assert_eq!(r.events_inserted, 0);
    assert_eq!(ing.store().usage_summary(&Range::All).unwrap()[0].events, 2);
}

#[test]
fn codex_zstd_and_plain_with_rate_limits() {
    let tmp = tempfile::tempdir().unwrap();
    // zstd-compressed rollout
    let zdir = tmp.path().join("codex/2026/03/02");
    fs::create_dir_all(&zdir).unwrap();
    let body = codex_lines("th1", "root1", "2026-03-02T10:00:00.000Z").join("\n") + "\nnot json\n";
    let zst = zstd::encode_all(body.as_bytes(), 3).unwrap();
    let zpath = zdir.join("rollout-2026-03-02T10-00-00-th1.jsonl.zst");
    fs::write(&zpath, &zst).unwrap();
    // plain rollout (incremental with persisted ctx)
    let ppath = zdir.join("rollout-2026-03-02T11-00-00-th2.jsonl");
    write_lines(
        &ppath,
        &codex_lines("th2", "root2", "2026-03-02T11:00:00.000Z")[..3],
    );

    let mut ing = ing(tmp.path());
    let r = ing.scan().unwrap();
    assert_eq!(r.codex_files, 2);
    assert_eq!(r.events_inserted, 2, "{r:?}");
    assert_eq!(r.malformed_lines, 1);
    assert_eq!(r.touches_inserted, 1);
    assert!(r.errors.is_empty(), "{:?}", r.errors);

    let s = ing.store();
    let u = s.usage_summary(&Range::Session("root1".into())).unwrap();
    assert_eq!(u.len(), 1);
    assert_eq!((u[0].input, u[0].cache_read, u[0].output), (600, 400, 500));
    // gpt-6.1-sol: 600*2 + 400*0.1 + 500*10 = 1200+40+5000 = 6240 / 1e6
    assert!((u[0].est_cost_usd.unwrap() - 0.00624).abs() < 1e-9);
    assert_eq!(s.files_touched("root1").unwrap()[0].path, "src/a.rs");

    // rate limits: real snapshots, recorded when changed (20% then 21% for th1)
    let rl = s.latest_rate_limits().unwrap();
    assert_eq!(rl[0].agent, Agent::Codex);
    assert_eq!(rl[0].window, "seven_day");
    assert_eq!(rl[0].resets_at_ms, Some(1_893_456_000_000));

    // zst unchanged -> skipped; modified -> whole file re-read but nothing double counted
    let r2 = ing.scan().unwrap();
    assert_eq!(r2.files_unchanged, 2);
    let body2 = body + &codex_lines("th1", "root1", "2026-03-02T10:00:00.000Z")[0] + "\n";
    fs::write(&zpath, zstd::encode_all(body2.as_bytes(), 3).unwrap()).unwrap();
    let r3 = ing.scan().unwrap();
    assert_eq!(r3.files_scanned, 1);
    assert_eq!(r3.events_inserted, 0);
    assert_eq!(
        ing.store()
            .usage_summary(&Range::All)
            .unwrap()
            .iter()
            .map(|u| u.events)
            .sum::<u64>(),
        2
    );

    // plain codex file grows: new turn appended, ctx (model/cwd/thread) restored from offsets
    append(
        &ppath,
        concat!(
            r#"{"timestamp":"2026-03-02T11:30:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":2500},"last_token_usage":{"input_tokens":800,"cached_input_tokens":0,"output_tokens":200,"reasoning_output_tokens":0,"total_tokens":1000}},"rate_limits":null}}"#,
            "\n"
        ),
    );
    let r4 = ing.scan().unwrap();
    assert_eq!(r4.events_inserted, 1, "{r4:?}");
    let u2 = ing
        .store()
        .usage_summary(&Range::Session("root2".into()))
        .unwrap();
    assert_eq!(u2.len(), 1);
    assert_eq!(u2[0].model, "gpt-6.1-sol");
    assert_eq!(u2[0].events, 2);
}

fn insert_ts(s: &Store, ts_ms: i64, tag: &str) {
    s.upsert_usage(&agents_svc::UsageEvent {
        agent: Agent::Claude,
        session_id: "s".into(),
        ts_ms,
        model: "claude-opus-5-5".into(),
        tokens: agents_svc::TokenCounts {
            input: 1,
            output: 1,
            ..Default::default()
        },
        cwd: None,
        git_branch: None,
        msg_key: tag.into(),
    })
    .unwrap();
}

#[test]
fn week_and_day_ranges_respect_timezone_and_week_start() {
    // UTC-5 (e.g. a US east-coast user). "now" = Wed 2026-03-04 02:00 local = 07:00 UTC.
    let tz = FixedOffset::west_opt(5 * 3600).unwrap();
    let now = tz.with_ymd_and_hms(2026, 3, 4, 2, 0, 0).unwrap();
    let mut s = Store::open_in_memory().unwrap();
    let ms = |y, mo, d, h, mi| {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    };

    // Monday 2026-03-02 00:00 local == 05:00 UTC; week is [Mon 05:00Z, next Mon 05:00Z)
    insert_ts(&s, ms(2026, 3, 2, 4, 59), "before-monday-local"); // Sunday 23:59 local -> previous week
    insert_ts(&s, ms(2026, 3, 2, 5, 0), "monday-start");
    insert_ts(&s, ms(2026, 3, 4, 6, 59), "today-1"); // Tue 01:59 local... still yesterday? 06:59Z = 01:59 local Wed
    insert_ts(&s, ms(2026, 3, 4, 4, 59), "tue-late"); // Tue 23:59 local
    insert_ts(&s, ms(2026, 3, 9, 4, 59), "sunday-late"); // Sun 23:59 local, still this week
    insert_ts(&s, ms(2026, 3, 9, 5, 0), "next-monday"); // next week

    let count = |s: &Store, r: &Range| -> u64 {
        s.usage_summary_at(r, &now)
            .unwrap()
            .iter()
            .map(|u| u.events)
            .sum()
    };
    // Monday-start week: monday-start, today-1, tue-late, sunday-late
    assert_eq!(count(&s, &Range::ThisWeek), 4);
    // Today (Wed local 00:00 = 05:00Z .. next 05:00Z): only today-1 (06:59Z)
    assert_eq!(count(&s, &Range::Today), 1);

    // Exact boundaries in UTC-5: Monday 00:00 local == 05:00Z.
    let wb = Range::ThisWeek.resolve(&now, Weekday::Mon);
    assert_eq!(wb.start_ms, Some(ms(2026, 3, 2, 5, 0)));
    assert_eq!(wb.end_ms, Some(ms(2026, 3, 9, 5, 0)));
    let tb = Range::Today.resolve(&now, Weekday::Mon);
    assert_eq!(tb.start_ms, Some(ms(2026, 3, 4, 5, 0)));
    assert_eq!(tb.end_ms, Some(ms(2026, 3, 5, 5, 0)));

    // Sunday-start week containing Wed Mar 4 is Sun Mar 1 05:00Z .. Sun Mar 8 05:00Z
    s.set_week_start(Weekday::Sun);
    // before-monday-local (Sun Mar 1 23:59 local), monday-start, today-1, tue-late; sunday-late is next week
    assert_eq!(count(&s, &Range::ThisWeek), 4);
    let sb = Range::ThisWeek.resolve(&now, Weekday::Sun);
    assert_eq!(sb.start_ms, Some(ms(2026, 3, 1, 5, 0)));
    assert_eq!(sb.end_ms, Some(ms(2026, 3, 8, 5, 0)));

    // Same wall clock, far-east timezone (UTC+9): Wed Mar 4 16:00 local. Monday 00:00 local is Sun 15:00Z.
    let tz2 = FixedOffset::east_opt(9 * 3600).unwrap();
    let now2 = tz2.with_ymd_and_hms(2026, 3, 4, 16, 0, 0).unwrap();
    s.set_week_start(Weekday::Mon);
    let wb2 = Range::ThisWeek.resolve(&now2, Weekday::Mon);
    assert_eq!(wb2.start_ms, Some(ms(2026, 3, 1, 15, 0)));
    assert_eq!(wb2.end_ms, Some(ms(2026, 3, 8, 15, 0)));
    // before-monday-local(UTC-5 view) is inside this week for UTC+9; sunday-late and next-monday are not.
    let c2 = s
        .usage_summary_at(&Range::ThisWeek, &now2)
        .unwrap()
        .iter()
        .map(|u| u.events)
        .sum::<u64>();
    assert_eq!(c2, 4);

    // Last7Days is rolling
    assert_eq!(count(&s, &Range::Last7Days), 4); // events within 7d before now (07:00Z Mar 4): Feb 25..Mar 4
                                                 // Custom
    let custom = Range::Custom {
        start_ms: ms(2026, 3, 2, 5, 0),
        end_ms: ms(2026, 3, 2, 5, 1),
    };
    assert_eq!(count(&s, &custom), 1);
}

#[test]
fn burn_rate_and_forecast_require_real_snapshot() {
    let s = Store::open_in_memory().unwrap();
    let now: i64 = 1_800_000_000_000;
    let h = 3_600_000;
    // 1000 billable tokens (in+out+cache_create) each hour for the last 3h
    for i in 0..3 {
        s.upsert_usage(&agents_svc::UsageEvent {
            agent: Agent::Codex,
            session_id: "s".into(),
            ts_ms: now - i * h - 1000,
            model: "gpt-6.1-sol".into(),
            tokens: agents_svc::TokenCounts {
                input: 600,
                output: 400,
                ..Default::default()
            },
            cwd: None,
            git_branch: None,
            msg_key: format!("k{i}"),
        })
        .unwrap();
    }
    // plus one event inside the 5h window before the snapshot (calibration)
    s.upsert_usage(&agents_svc::UsageEvent {
        agent: Agent::Codex,
        session_id: "s".into(),
        ts_ms: now - 45 * 60_000,
        model: "gpt-6.1-sol".into(),
        tokens: agents_svc::TokenCounts {
            input: 500,
            output: 500,
            ..Default::default()
        },
        cwd: None,
        git_branch: None,
        msg_key: "calib".into(),
    })
    .unwrap();
    let br = s
        .burn_rate_at(Some(Agent::Codex), Duration::from_secs(3 * 3600), now)
        .unwrap();
    assert_eq!(br.billable_tokens, 4000);
    assert!((br.billable_tokens_per_hour - 4000.0 / 3.0).abs() < 1e-6);

    // No snapshot -> never a forecast.
    assert!(s
        .forecast_at(
            Agent::Codex,
            "five_hour",
            Duration::from_secs(3 * 3600),
            now
        )
        .unwrap()
        .is_none());

    // Real snapshot: 30% used 30 minutes ago, window resets in 4h (5h window started 1h ago).
    s.insert_rate_limit(&agents_svc::RateLimitSnapshot {
        agent: Agent::Codex,
        ts_ms: now - h / 2,
        window: "five_hour".into(),
        used_percent: 30.0,
        resets_at_ms: Some(now + 4 * h),
        source: "codex-rollout".into(),
    })
    .unwrap();
    let f = s
        .forecast_at(
            Agent::Codex,
            "five_hour",
            Duration::from_secs(3 * 3600),
            now,
        )
        .unwrap()
        .unwrap();
    // Window started now-1h; only the 45-minute-old event (1000 tokens) precedes the snapshot at 30%.
    assert!((f.tokens_per_percent - 1000.0 / 30.0).abs() < 1e-6);
    assert!(f.eta_ms > 0);
    assert_eq!(f.label, "estimated");
    assert!(f.est_used_percent_now >= 30.0);

    // Expired snapshot -> None
    assert!(s
        .forecast_at(
            Agent::Codex,
            "five_hour",
            Duration::from_secs(3 * 3600),
            now + 5 * h
        )
        .unwrap()
        .is_none());
}

#[test]
fn pricing_override_and_week_start_are_configurable() {
    let mut s = Store::open_in_memory().unwrap();
    let t = PricingTable::from_toml(
        "version = \"t\"\n[[model]]\nid = \"claude-opus-5-5\"\ninput = 1.0\noutput = 1.0\ncache_write = 1.0\ncache_read = 1.0\n",
    )
    .unwrap();
    s.set_pricing(t);
    insert_ts(&s, 1_700_000_000_000, "a");
    let u = s.usage_summary(&Range::All).unwrap();
    assert!((u[0].est_cost_usd.unwrap() - 2.0 / 1e6).abs() < 1e-12);
    assert_eq!(s.pricing().version, "t");
}

// Smoke test against the real home directories. Run with `-- --ignored --nocapture`.
#[test]
#[ignore]
fn smoke_real_home() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ing = Ingestor::open(&tmp.path().join("smoke.db"), IngestConfig::from_home()).unwrap();
    let rep: ScanReport = ing.scan().unwrap();
    println!(
        "SMOKE files_seen={} claude_files={} codex_files={} lines={} events_inserted={} events_updated={} dups={} synthetic={} malformed={} touches={} rate_snapshots={} errors={}",
        rep.files_seen, rep.claude_files, rep.codex_files, rep.lines, rep.events_inserted, rep.events_updated,
        rep.duplicates_skipped, rep.synthetic_skipped, rep.malformed_lines, rep.touches_inserted,
        rep.rate_snapshots_inserted, rep.errors.len()
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    let r2 = ing.scan().unwrap();
    // Agents may be writing right now (live sessions), so allow a few changed files.
    println!(
        "SMOKE rescan files_scanned={} bytes_read={} events_inserted={}",
        r2.files_scanned, r2.bytes_read, r2.events_inserted
    );
    assert!(r2.errors.is_empty(), "{:?}", r2.errors);
    assert!(r2.files_scanned <= 5 && r2.files_unchanged + r2.files_scanned == r2.files_seen);
    for range in [Range::Today, Range::ThisWeek, Range::Last7Days, Range::All] {
        let rows = ing.store().usage_summary(&range).unwrap();
        for a in [Agent::Claude, Agent::Codex] {
            let (mut i, mut o, mut cc, mut cr, mut ev) = (0, 0, 0, 0, 0);
            let mut cost = 0.0;
            let mut unpriced = 0u64;
            for u in rows.iter().filter(|u| u.agent == a) {
                i += u.input;
                o += u.output;
                cc += u.cache_create;
                cr += u.cache_read;
                ev += u.events;
                match u.est_cost_usd {
                    Some(c) => cost += c,
                    None => unpriced += u.events,
                }
            }
            println!("SMOKE {range:?} {a}: events={ev} input={i} output={o} cache_create={cc} cache_read={cr} est_cost_usd={cost:.2} unpriced_events={unpriced}");
        }
    }
    for rl in ing.store().latest_rate_limits().unwrap() {
        println!(
            "SMOKE ratelimit {} {} used={}% source={}",
            rl.agent, rl.window, rl.used_percent, rl.source
        );
    }
}

#[test]
fn scan_paths_ingests_only_the_given_files() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("claude/p/a.jsonl");
    let b = tmp.path().join("claude/p/b.jsonl");
    let line = |m: &str, out: u64| claude_line("2026-03-02T10:00:00.000Z", "s", m, &format!("r-{m}"), out, "main", "C:/p", None);
    write_lines(&a, &[line("m1", 10)]);
    write_lines(&b, &[line("m2", 20)]);
    let mut ing = ing(tmp.path());

    // Only `a` is named: `b` is not even looked at.
    let rep = ing.scan_paths(std::slice::from_ref(&a)).unwrap();
    assert_eq!((rep.files_seen, rep.files_scanned, rep.events_inserted), (1, 1, 1));

    // Unchanged file: stat only. Unknown / vanished paths are ignored.
    let rep = ing.scan_paths(&[a.clone(), tmp.path().join("claude/p/gone.jsonl"), tmp.path().join("claude/p/x.txt")]).unwrap();
    assert_eq!((rep.files_seen, rep.files_scanned, rep.files_unchanged, rep.events_inserted), (1, 0, 1, 0));

    // Append: only the tail is read.
    append(&a, &format!("{}\n", line("m3", 30)));
    let rep = ing.scan_paths(std::slice::from_ref(&a)).unwrap();
    assert_eq!((rep.files_scanned, rep.events_inserted), (1, 1));

    // A later full scan picks up the file that was never named, without double counting `a`.
    let rep = ing.scan().unwrap();
    assert_eq!(rep.events_inserted, 1);
    let total: u64 = ing.store().usage_summary(&Range::All).unwrap().iter().map(|u| u.output).sum();
    assert_eq!(total, 60);
}

#[test]
fn classify_matches_ingested_files_only() {
    use agents_svc::SourceKind::*;
    let cfg = IngestConfig {
        claude_projects: Some("C:/h/.claude/projects".into()),
        codex_sessions: Some("C:/h/.codex/sessions".into()),
        opencode_data: Some("C:/h/.local/share/opencode".into()),
    };
    assert_eq!(cfg.classify(Path::new("C:/h/.claude/projects/p/s.jsonl")), Some(Claude));
    assert_eq!(cfg.classify(Path::new("C:/h/.claude/projects/p/s.txt")), None);
    assert_eq!(cfg.classify(Path::new("C:/h/.codex/sessions/2026/rollout-1.jsonl.zst")), Some(Codex));
    assert_eq!(cfg.classify(Path::new("C:/h/.codex/sessions/2026/other.jsonl")), None);
    assert_eq!(cfg.classify(Path::new("C:/h/.local/share/opencode/opencode.db-wal")), Some(OpenCodeDb));
    assert_eq!(cfg.classify(Path::new("C:/h/.local/share/opencode/log/x.log")), None);
    assert_eq!(cfg.classify(Path::new("C:/h/.local/share/opencode/storage/message/s/m.json")), Some(OpenCodeJson));
    assert_eq!(cfg.classify(Path::new("C:/elsewhere/s.jsonl")), None);
}
