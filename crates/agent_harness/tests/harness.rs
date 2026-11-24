#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The harness end to end with a scripted agent (no models, no real CLIs): child thread with
//! lineage, Job Object, worktree + checkpoint, JSON schema, `diff` / `apply`, every refusal,
//! timeout / cancel / budget, redaction and approvals (including a mock Selachi on a real pipe).

mod common;

use std::sync::Arc;
use std::time::Duration;

use agent_harness::registry::RunRecord;
use agent_harness::{
    slots, ApprovalBroker, CancelOutcome, Confirmation, DelegationConfig, Permission, PipeBroker,
    RefusalCode, Risk, RunRequest, RunResult, RunStatus,
};
use agents_driver::{AgentEvent, AgentKind, PermissionMode};
use chat_core::{ApprovalDecision, ChatStore, ThreadId, ThreadSpec};
use common::*;

fn cfg() -> DelegationConfig {
    DelegationConfig::default()
}

/// The standard fake: edits `a.txt`, adds `b.txt`, answers, reports usage.
fn editing_agent() -> Setup {
    setup(|cfg, _, _| {
        std::fs::write(cfg.cwd.join("a.txt"), "alpha\nbeta\n").unwrap();
        std::fs::write(cfg.cwd.join("b.txt"), "new file\n").unwrap();
        vec![
            working(),
            text("I added beta to a.txt and created b.txt"),
            usage(100, 50, 10, 1000),
            done(),
        ]
    })
}

fn refused(r: &RunResult, code: RefusalCode) {
    assert_eq!(r.status, RunStatus::Refused, "{r:?}");
    assert_eq!(r.code, Some(code), "{:?}", r.reason);
    assert!(r.run_id.is_none());
    assert!(r.reason.as_deref().is_some_and(|m| !m.is_empty()));
}

fn no_threads_created(s: &Setup) {
    let chat = s.data_dir().join("chat.db");
    if chat.exists() {
        let store = ChatStore::open_at(chat).unwrap();
        assert!(
            store.delegated_threads().unwrap().is_empty(),
            "a refused run must not create a thread"
        );
    }
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[test]
fn worktree_run_end_to_end() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    std::fs::create_dir_all(s.data_dir()).unwrap();
    // the delegating chat thread
    let parent = {
        let store = ChatStore::open_at(s.data_dir().join("chat.db")).unwrap();
        let t = store
            .create_thread(ThreadSpec::new(&repo, AgentKind::ClaudeCode))
            .unwrap();
        store.flush().unwrap();
        t
    };
    let mut c = caller(Permission::AcceptEdits);
    c.parent_thread = Some(parent.id.0);
    c.parent_tab = Some("tab-9".into());
    let h = s.harness(c, cfg());

    let mut req = RunRequest::new("codex", "review and fix a.txt", &repo);
    req.permission = Permission::AcceptEdits;
    req.worktree = true;
    let r = h.run(req);

    // --- the documented JSON shape
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    for k in [
        "run_id",
        "status",
        "summary",
        "files_changed",
        "diff_stat",
        "usage",
        "cost",
        "worktree_path",
    ] {
        assert!(v.get(k).is_some(), "missing {k} in {v}");
    }
    assert_eq!(v["status"], "done");
    let run_id = r.run_id.clone().unwrap();
    assert!(v["summary"].as_str().unwrap().contains("created b.txt"));
    let mut files = r.files_changed.clone();
    files.sort();
    assert_eq!(files, vec!["a.txt", "b.txt"]);
    assert_eq!(
        (
            r.diff_stat.files,
            r.diff_stat.insertions,
            r.diff_stat.deletions
        ),
        (2, 2, 0)
    );
    assert!(r.diff_stat.text.contains("2 files changed"));
    assert_eq!(
        (
            r.usage.input,
            r.usage.output,
            r.usage.cache_create,
            r.usage.cache_read
        ),
        (100, 50, 10, 1000)
    );
    assert_eq!((r.usage.total, r.usage.billable), (1160, 160));
    assert!(v["cost"]["usd"].is_number() && v["cost"]["source"].is_string());

    // --- isolation: the project is untouched, the work is in a sibling worktree
    let wt = std::path::PathBuf::from(r.worktree_path.clone().unwrap());
    assert!(wt.is_dir() && wt != repo);
    assert_eq!(
        std::fs::read_to_string(wt.join("b.txt")).unwrap(),
        "new file\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!repo.join("b.txt").exists());
    assert!(git(&repo, &["status", "--porcelain"]).trim().is_empty());

    // --- the agent got what it should: redacted prompt, the worktree, the permission, inside a Job Object
    {
        let rec = s.factory.rec.lock();
        assert_eq!(rec.spawns.len(), 1);
        let (kind, cfg, job) = &rec.spawns[0];
        assert_eq!(*kind, AgentKind::Codex);
        assert!(
            agent_harness::policy::Permission::from_driver(cfg.permission_mode)
                == Permission::AcceptEdits
        );
        assert!(same_dir(&cfg.cwd, &wt));
        assert!(
            job.is_some_and(|j| j != 0),
            "agent must be created inside the Job Object"
        );
        assert_eq!(rec.sent, vec!["review and fix a.txt".to_string()]);
        assert_eq!(rec.shutdowns, 1, "the agent is stopped when the run ends");
    }
    assert_eq!(h.jobs().jobs_opened(), 1);

    // --- the child thread is persisted and linked to its parent
    let store = h.store().unwrap();
    let kids = store.children(parent.id).unwrap();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].id.0.to_string(), run_id);
    let link = store.thread_link(kids[0].id).unwrap().unwrap();
    assert_eq!(link.parent_thread_id, Some(parent.id));
    assert_eq!(
        (
            link.parent_tab.as_deref(),
            link.depth,
            link.delegated_by.as_deref()
        ),
        (Some("tab-9"), 1, Some("cli"))
    );
    assert_eq!(kids[0].permission, PermissionMode::AcceptEdits);
    assert!(same_dir(kids[0].worktree_path.as_ref().unwrap(), &wt));
    assert_eq!(kids[0].agent, AgentKind::Codex);
    let rt = store.open(kids[0].id).unwrap();
    assert_eq!(
        rt.with_state(|st| st.messages.len()),
        2,
        "prompt + answer are in the thread"
    );

    // --- status / result / diff replay from the registry
    let st = h.status(&run_id).unwrap();
    assert_eq!(
        (st.status, st.agent.as_str(), st.depth),
        (RunStatus::Done, "codex", 1)
    );
    assert_eq!(h.result(&run_id).unwrap(), r);
    let diff = h.diff(&run_id).unwrap();
    assert!(diff.contains("+beta") && diff.contains("b.txt"), "{diff}");

    // --- apply: plan first (read-only), then a confirmed apply, never twice
    let plan = h.apply_plan(&run_id).unwrap();
    assert!(
        git(&repo, &["status", "--porcelain"]).trim().is_empty(),
        "planning must not touch the project"
    );
    assert_eq!(plan.risk, Risk::Normal);
    assert!(
        plan.can_apply
            && plan.applies_cleanly
            && plan.requires_confirmation
            && !plan.already_applied
    );
    let mut changes: Vec<_> = plan
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.change))
        .collect();
    changes.sort();
    assert_eq!(changes, vec![("a.txt", "modified"), ("b.txt", "added")]);
    assert!(plan.to_text().contains("b.txt"));

    let report = h.apply(&run_id, Confirmation::Flag).unwrap();
    assert_eq!(report.files.len(), 2);
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("b.txt")).unwrap(),
        "new file\n"
    );
    assert!(h.status(&run_id).unwrap().applied);
    assert!(h
        .apply(&run_id, Confirmation::Flag)
        .unwrap_err()
        .contains("already applied"));
    assert!(h.apply_plan(&run_id).unwrap().already_applied);
}

#[test]
fn read_only_run_in_place_changes_nothing_and_has_no_worktree() {
    let s = setup(|_, _, _| {
        vec![
            working(),
            text("looks fine to me"),
            usage(5, 5, 0, 0),
            done(),
        ]
    });
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let r = h.run(RunRequest::new("claude", "is this ok?", &repo));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert!(r.files_changed.is_empty() && r.worktree_path.is_none());
    assert_eq!(r.diff_stat.files, 0);
    assert_eq!(r.permission.as_deref(), Some("read-only"));
    {
        let rec = s.factory.rec.lock();
        assert_eq!(rec.spawns[0].1.permission_mode, PermissionMode::ReadOnly);
        assert!(same_dir(&rec.spawns[0].1.cwd, &repo));
    }
    // nothing to apply for a run that worked in place
    let e = h.apply_plan(r.run_id.as_deref().unwrap()).unwrap_err();
    assert!(e.contains("did not use --worktree"), "{e}");
}

#[test]
fn a_run_in_place_reports_what_the_agent_changed_not_what_was_already_dirty() {
    let s = setup(|cfg, _, _| {
        std::fs::write(cfg.cwd.join("b.txt"), "by the agent\n").unwrap();
        vec![working(), text("done"), done()]
    });
    let repo = init_repo(s.dir.path(), "proj");
    std::fs::write(repo.join("a.txt"), "alpha\nmy own wip\n").unwrap(); // dirty before the run
    let h = s.harness(caller(Permission::AcceptEdits), cfg());
    let mut req = RunRequest::new("codex", "add b", &repo);
    req.permission = Permission::AcceptEdits;
    let r = h.run(req);
    assert_eq!(
        r.files_changed,
        vec!["b.txt"],
        "the checkpoint was taken after the parent's own edits: {r:?}"
    );
}

#[test]
fn secrets_are_redacted_going_in_and_coming_out() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let s = setup(move |_, _, _| {
        vec![
            working(),
            text(&format!("the token is {secret}, keep it safe")),
            done(),
        ]
    });
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let r = h.run(RunRequest::new(
        "codex",
        format!("use {secret} to call the api"),
        &repo,
    ));
    assert_eq!(r.status, RunStatus::Done);
    let sent = s.factory.sent();
    assert!(
        !sent[0].contains(secret),
        "the agent must never see the secret: {sent:?}"
    );
    assert!(!r.summary.contains(secret), "{}", r.summary);
    assert!(!r.to_json().contains(secret) && !r.to_text().contains(secret));
    assert!(
        r.notes.iter().any(|n| n.contains("hidden")),
        "{:?}",
        r.notes
    );
    let rec = h.registry().load(r.run_id.as_deref().unwrap()).unwrap();
    assert!(!rec.prompt.contains(secret));
}

#[test]
fn an_agent_that_cannot_start_is_an_error_result_not_a_hang() {
    let s = editing_agent();
    s.factory
        .fail_spawn
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let r = h.run(RunRequest::new("codex", "x", &repo));
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert!(
        r.reason.as_deref().unwrap().contains("ejecutable"),
        "{:?}",
        r.reason
    );
    assert!(r.run_id.is_some(), "it did start a thread, so it has an id");
}

// ---------------------------------------------------------------------------
// Refusals (nothing runs, nothing is created)
// ---------------------------------------------------------------------------

#[test]
fn permission_never_exceeds_the_parent() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    // no SELACHI_PARENT_PERMISSION at all: read-only
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.permission = Permission::AcceptEdits;
    refused(&h.run(req.clone()), RefusalCode::PermissionExceedsParent);
    req.permission = Permission::Ask;
    refused(&h.run(req), RefusalCode::PermissionExceedsParent);
    assert_eq!(s.factory.spawns(), 0);
    no_threads_created(&s);

    // an `ask` parent can delegate `ask` and `read-only`, not `accept-edits`
    let h = s.harness(caller(Permission::Ask), cfg());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.permission = Permission::Ask;
    assert_eq!(h.run(req.clone()).status, RunStatus::Done);
    req.permission = Permission::AcceptEdits;
    refused(&h.run(req), RefusalCode::PermissionExceedsParent);
}

#[test]
fn full_is_never_delegable_even_for_a_full_parent() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::Full), cfg());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.permission = Permission::Full;
    refused(&h.run(req), RefusalCode::PermissionFull);
    assert_eq!(s.factory.spawns(), 0);
    no_threads_created(&s);
}

#[test]
fn depth_is_limited() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let at = |depth| {
        let mut c = caller(Permission::ReadOnly);
        c.depth = depth;
        c
    };
    // default max_depth 1 (PLAN §20): a top-level session may delegate, the child may not
    assert_eq!(
        s.harness(at(0), cfg())
            .run(RunRequest::new("codex", "x", &repo))
            .status,
        RunStatus::Done
    );
    refused(
        &s.harness(at(1), cfg())
            .run(RunRequest::new("codex", "x", &repo)),
        RefusalCode::DepthExceeded,
    );
    // raising max_depth to 2 lets a child delegate once more, and stops at depth 2
    let two = DelegationConfig {
        max_depth: 2,
        ..cfg()
    };
    let r = s
        .harness(at(1), two.clone())
        .run(RunRequest::new("codex", "x", &repo));
    assert_eq!(r.status, RunStatus::Done);
    let rec = s
        .harness(at(1), two.clone())
        .registry()
        .load(r.run_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.depth, 2);
    refused(
        &s.harness(at(2), two)
            .run(RunRequest::new("codex", "x", &repo)),
        RefusalCode::DepthExceeded,
    );
    assert_eq!(s.factory.spawns(), 2);
}

#[test]
fn disabled_and_allowed_agents() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let off = DelegationConfig {
        enabled: false,
        ..cfg()
    };
    refused(
        &s.harness(caller(Permission::ReadOnly), off)
            .run(RunRequest::new("codex", "x", &repo)),
        RefusalCode::Disabled,
    );
    let only_claude = DelegationConfig {
        allowed_agents: vec!["claude".into()],
        ..cfg()
    };
    let h = s.harness(caller(Permission::ReadOnly), only_claude);
    refused(
        &h.run(RunRequest::new("codex", "x", &repo)),
        RefusalCode::AgentNotAllowed,
    );
    assert_eq!(
        h.run(RunRequest::new("claude", "x", &repo)).status,
        RunStatus::Done
    );
    refused(
        &h.run(RunRequest::new("gpt-9", "x", &repo)),
        RefusalCode::AgentNotAllowed,
    );
}

#[test]
fn bad_requests() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    refused(
        &h.run(RunRequest::new("codex", "   ", &repo)),
        RefusalCode::BadRequest,
    );
    refused(
        &h.run(RunRequest::new("codex", "x", s.dir.path().join("nope"))),
        RefusalCode::BadRequest,
    );
    let h2 = s.harness(caller(Permission::ReadOnly), cfg());
    refused(
        &h2.run(RunRequest::new("not-an-agent", "x", &repo)),
        RefusalCode::UnknownAgent,
    );
    // --worktree outside a repository
    let plain = s.dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut req = RunRequest::new("codex", "x", &plain);
    req.worktree = true;
    refused(&h.run(req), RefusalCode::NotAGitRepo);
    assert_eq!(s.factory.spawns(), 0);
    no_threads_created(&s);
}

#[test]
fn at_most_three_runs_at_once() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let held: Vec<_> = (0..3)
        .map(|_| slots::acquire(&s.data_dir(), 3).unwrap().unwrap())
        .collect();
    let r = h.run(RunRequest::new("codex", "x", &repo));
    refused(&r, RefusalCode::Concurrency);
    assert!(r.reason.unwrap().contains("3 delegated runs"));
    assert_eq!(s.factory.spawns(), 0);
    no_threads_created(&s);
    // one finishes -> the next run gets the slot, and gives it back
    drop(held);
    assert_eq!(
        h.run(RunRequest::new("codex", "x", &repo)).status,
        RunStatus::Done
    );
    assert_eq!(slots::in_use(&s.data_dir(), 3), 0);

    // the limit is configurable
    let one = DelegationConfig {
        max_concurrent: 1,
        ..cfg()
    };
    let h = s.harness(caller(Permission::ReadOnly), one);
    let _held = slots::acquire(&s.data_dir(), 1).unwrap().unwrap();
    refused(
        &h.run(RunRequest::new("codex", "x", &repo)),
        RefusalCode::Concurrency,
    );
}

fn seeded(id: &str, started_ms: i64, billable: u64) -> RunRecord {
    let mut result = RunResult::empty(RunStatus::Done);
    result.usage = agent_harness::UsageOut::new(billable, 0, 0, 0);
    RunRecord {
        run_id: id.into(),
        thread_id: id.parse().unwrap(),
        agent: "codex".into(),
        model: None,
        permission: "read-only".into(),
        cwd: "C:/p".into(),
        project_root: "C:/p".into(),
        worktree_path: None,
        checkpoint_id: None,
        prompt: "old".into(),
        parent_thread_id: None,
        depth: 1,
        started_ms,
        finished_ms: Some(started_ms + 1),
        status: RunStatus::Done,
        runner_pid: 1,
        runner_start_ms: None,
        deadline_ms: started_ms + 1000,
        result: Some(result),
        applied_ms: None,
    }
}

#[test]
fn daily_token_budget() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let budget = DelegationConfig {
        daily_token_budget: 1000,
        ..cfg()
    };
    let h = s.harness(caller(Permission::ReadOnly), budget);
    let now = chat_core::now_ms();
    // yesterday's usage does not count
    h.registry()
        .save(&seeded("901", now - 3 * 86_400_000, 5_000_000))
        .unwrap();
    assert_eq!(
        h.run(RunRequest::new("codex", "x", &repo)).status,
        RunStatus::Done
    );
    // today's does
    h.registry().save(&seeded("902", now, 1_000)).unwrap();
    let r = h.run(RunRequest::new("codex", "x", &repo));
    refused(&r, RefusalCode::DailyBudget);
    assert!(r.reason.unwrap().contains("daily"));
    // 0 = unlimited
    let unlimited = DelegationConfig {
        daily_token_budget: 0,
        ..cfg()
    };
    assert_eq!(
        s.harness(caller(Permission::ReadOnly), unlimited)
            .run(RunRequest::new("codex", "x", &repo))
            .status,
        RunStatus::Done
    );
}

fn limit(agent: &str, window: &str, pct: f64) -> agent_harness::LimitRow {
    agent_harness::LimitRow {
        agent: agent.into(),
        window: window.into(),
        used_percent: pct,
        resets_at_ms: None,
        observed_ms: 0,
        source: "codex-rollout".into(),
    }
}

#[test]
fn weekly_usage_over_the_threshold_blocks_that_agent_only() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let limits = vec![
        limit("codex", "seven_day", 95.0),
        limit("claude", "seven_day", 10.0),
        limit("opencode", "five_hour", 99.0),
    ];
    let h = s.harness_full(
        caller(Permission::ReadOnly),
        cfg(),
        Arc::new(agent_harness::DenyAll),
        limits.clone(),
    );
    let r = h.run(RunRequest::new("codex", "x", &repo));
    refused(&r, RefusalCode::WeeklyUsage);
    let reason = r.reason.unwrap();
    assert!(reason.contains("95") && reason.contains("90"), "{reason}");
    assert_eq!(
        h.run(RunRequest::new("claude", "x", &repo)).status,
        RunStatus::Done
    );
    assert_eq!(
        h.run(RunRequest::new("opencode", "x", &repo)).status,
        RunStatus::Done,
        "only the weekly window counts"
    );
    // a higher threshold lets codex through; 0 disables the rule
    let lenient = DelegationConfig {
        weekly_usage_threshold_percent: 97.0,
        ..cfg()
    };
    let h = s.harness_full(
        caller(Permission::ReadOnly),
        lenient,
        Arc::new(agent_harness::DenyAll),
        limits.clone(),
    );
    assert_eq!(
        h.run(RunRequest::new("codex", "x", &repo)).status,
        RunStatus::Done
    );
    let off = DelegationConfig {
        weekly_usage_threshold_percent: 0.0,
        ..cfg()
    };
    let h = s.harness_full(
        caller(Permission::ReadOnly),
        off,
        Arc::new(agent_harness::DenyAll),
        limits,
    );
    assert_eq!(
        h.run(RunRequest::new("codex", "x", &repo)).status,
        RunStatus::Done
    );
}

// ---------------------------------------------------------------------------
// Timeout, cancel, per-run budget
// ---------------------------------------------------------------------------

#[test]
fn timeout_interrupts_the_agent_and_frees_the_slot() {
    let s = setup(|_, _, _| vec![working()]); // never finishes
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let mut req = RunRequest::new("codex", "think forever", &repo);
    req.timeout = Some(Duration::from_secs(1));
    let t = std::time::Instant::now();
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::TimedOut, "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(10));
    assert!(r.reason.unwrap().contains("1 s"));
    assert!(s.factory.rec.lock().interrupts >= 1);
    assert_eq!(s.factory.rec.lock().shutdowns, 1);
    assert_eq!(slots::in_use(&s.data_dir(), 3), 0);
    assert_eq!(RunStatus::TimedOut.exit_code(), 4);
    assert_eq!(
        h.status(r.run_id.as_deref().unwrap()).unwrap().status,
        RunStatus::TimedOut
    );
}

#[test]
fn the_default_timeout_comes_from_the_config() {
    let s = setup(|_, _, _| vec![working()]);
    let repo = init_repo(s.dir.path(), "proj");
    assert_eq!(cfg().timeout_secs, 15 * 60, "default timeout is 15 minutes");
    let h = s.harness(
        caller(Permission::ReadOnly),
        DelegationConfig {
            timeout_secs: 1,
            ..cfg()
        },
    );
    let r = h.run(RunRequest::new("codex", "x", &repo));
    assert_eq!(r.status, RunStatus::TimedOut);
}

#[test]
fn cancel_from_another_thread_stops_the_run() {
    let s = setup(|_, _, _| vec![working()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = Arc::new(s.harness(caller(Permission::ReadOnly), cfg()));
    let runner = {
        let h = h.clone();
        std::thread::spawn(move || h.run(RunRequest::new("codex", "long task", &repo)))
    };
    wait_until("the run is registered", || {
        h.registry()
            .list()
            .iter()
            .any(|r| r.status == RunStatus::Running)
    });
    let id = h.registry().list()[0].run_id.clone();
    assert_eq!(h.status(&id).unwrap().status, RunStatus::Running);
    assert!(h.result(&id).unwrap_err().contains("still running"));
    assert_eq!(h.cancel(&id).unwrap(), CancelOutcome::Cancelled);
    let r = runner.join().unwrap();
    assert_eq!(r.status, RunStatus::Cancelled, "{r:?}");
    assert_eq!(RunStatus::Cancelled.exit_code(), 5);
    assert_eq!(
        h.cancel(&id).unwrap(),
        CancelOutcome::AlreadyFinished {
            status: RunStatus::Cancelled
        }
    );
    assert_eq!(slots::in_use(&s.data_dir(), 3), 0);
}

#[test]
fn a_dead_runner_is_reported_as_an_error_not_running_forever() {
    let s = editing_agent();
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let mut rec = seeded("777", chat_core::now_ms(), 0);
    rec.status = RunStatus::Running;
    rec.finished_ms = None;
    rec.result = None;
    rec.runner_pid = 0x7fff_f000; // no such process
    h.registry().save(&rec).unwrap();
    let st = h.status("777").unwrap();
    assert_eq!(st.status, RunStatus::Error);
    assert!(h
        .result("777")
        .unwrap()
        .reason
        .unwrap()
        .contains("ended without reporting"));
    assert_eq!(
        h.registry().load("777").unwrap().status,
        RunStatus::Error,
        "reconciled on disk"
    );
}

#[test]
fn token_budget_per_run_interrupts_the_agent() {
    let s = setup(|_, _, _| vec![working(), usage(1000, 1000, 0, 0)]); // never finishes
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), cfg());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.budget_tokens = Some(500);
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::BudgetExceeded, "{r:?}");
    assert!(r.reason.as_deref().unwrap().contains("--budget-tokens 500"));
    assert_eq!(r.usage.billable, 2000);
    assert!(s.factory.rec.lock().interrupts >= 1);
    assert_eq!(RunStatus::BudgetExceeded.exit_code(), 6);
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

fn asking_agent() -> Setup {
    let s = setup(|_, _, _| {
        vec![
            working(),
            AgentEvent::ApprovalRequest {
                id: "req_1".into(),
                tool: "Bash".into(),
                detail: "deploy with ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
            },
        ]
    });
    s.factory.on_approve(|_, d| match d {
        ApprovalDecision::Deny => vec![text("ok, I will not do that"), done()],
        _ => vec![text("done after approval"), done()],
    });
    s
}

fn asking_run(
    s: &Setup,
    broker: Arc<dyn ApprovalBroker>,
    cfg: DelegationConfig,
    parent: Option<i64>,
) -> RunResult {
    let repo = init_repo(s.dir.path(), "proj");
    let mut c = caller(Permission::Ask);
    c.parent_thread = parent;
    if parent.is_some() {
        // the delegating chat thread must exist in chat.db
        std::fs::create_dir_all(s.data_dir()).unwrap();
        let store = ChatStore::open_at(s.data_dir().join("chat.db")).unwrap();
        let t = store
            .create_thread(ThreadSpec::new(&repo, AgentKind::ClaudeCode))
            .unwrap();
        assert_eq!(Some(t.id.0), parent, "the first thread of a fresh database");
        store.flush().unwrap();
    }
    let h = s.harness_with(c, cfg, broker);
    let mut req = RunRequest::new("codex", "do the thing", &repo);
    req.permission = Permission::Ask;
    h.run(req)
}

#[test]
fn without_a_selachi_window_child_approvals_are_denied_with_a_reason() {
    let s = asking_agent();
    let r = asking_run(&s, Arc::new(agent_harness::DenyAll), cfg(), None);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Deny)]
    );
    let note = r
        .notes
        .iter()
        .find(|n| n.contains("approval denied"))
        .expect("a note says why");
    assert!(note.contains("no Selachi window"), "{note}");
}

#[test]
fn approvals_with_a_pipe_nobody_serves_are_denied_too() {
    let s = asking_agent();
    let broker = Arc::new(PipeBroker::new(
        r"\\.\pipe\selachi-approvals-test-nobody-home",
    ));
    let r = asking_run(&s, broker, cfg(), None);
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Deny)]
    );
    assert!(
        r.notes.iter().any(|n| n.contains("no Selachi window")),
        "{:?}",
        r.notes
    );
}

#[test]
fn approvals_are_forwarded_to_selachi_over_the_pipe() {
    let s = asking_agent();
    let server = MockServer::answering(agent_harness::Decision::Allow);
    let r = asking_run(&s, Arc::new(PipeBroker::new(&server.path)), cfg(), Some(1));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.summary, "done after approval");
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Allow)]
    );

    let reqs = server.approval_requests();
    assert_eq!(reqs.len(), 1);
    let q = &reqs[0];
    assert_eq!(q.v, agent_harness::PROTOCOL_VERSION);
    assert_eq!(
        (q.request_id.as_str(), q.tool.as_str(), q.agent.as_str()),
        ("req_1", "Bash", "codex")
    );
    assert_eq!(Some(q.run_id.clone()), r.run_id);
    assert_eq!(q.thread_id.to_string(), q.run_id);
    assert_eq!(q.parent_thread_id, Some(1));
    assert_eq!((q.permission.as_str(), q.depth), ("ask", 1));
    assert!(
        !q.detail.contains("ghp_"),
        "the detail shown in Selachi is redacted: {}",
        q.detail
    );
    assert!(q.detail.contains("deploy with"));
}

#[test]
fn the_user_can_deny_or_always_allow() {
    let s = asking_agent();
    let server = MockServer::answering(agent_harness::Decision::Deny);
    let r = asking_run(&s, Arc::new(PipeBroker::new(&server.path)), cfg(), None);
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Deny)]
    );
    assert_eq!(r.summary, "ok, I will not do that");

    let s = asking_agent();
    let server = MockServer::answering(agent_harness::Decision::Always);
    asking_run(&s, Arc::new(PipeBroker::new(&server.path)), cfg(), None);
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Always)]
    );
}

#[test]
fn a_server_error_a_silent_server_and_a_slow_user_all_mean_deny() {
    // error reply
    let s = asking_agent();
    let server = MockServer::start(|_| {
        Some(agent_harness::ServerMessage::Error {
            v: 1,
            reason: "queue is full".into(),
        })
    });
    let r = asking_run(&s, Arc::new(PipeBroker::new(&server.path)), cfg(), None);
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Deny)]
    );
    assert!(
        r.notes.iter().any(|n| n.contains("queue is full")),
        "{:?}",
        r.notes
    );

    // closes without answering
    let s = asking_agent();
    let server = MockServer::start(|_| None);
    let r = asking_run(&s, Arc::new(PipeBroker::new(&server.path)), cfg(), None);
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Deny)]
    );
    assert!(
        r.notes.iter().any(|n| n.contains("without answering")),
        "{:?}",
        r.notes
    );

    // the user takes too long
    let s = asking_agent();
    let server = MockServer::start(|m| {
        std::thread::sleep(Duration::from_secs(4));
        match m {
            agent_harness::ClientMessage::ApprovalRequest(q) => {
                Some(agent_harness::ServerMessage::ApprovalResponse {
                    v: 1,
                    request_id: q.request_id.clone(),
                    decision: agent_harness::Decision::Allow,
                    reason: None,
                })
            }
            _ => None,
        }
    });
    let patient = DelegationConfig {
        approval_timeout_secs: 1,
        ..cfg()
    };
    let r = asking_run(&s, Arc::new(PipeBroker::new(&server.path)), patient, None);
    assert_eq!(
        s.factory.rec.lock().approvals,
        vec![("req_1".to_string(), ApprovalDecision::Deny)]
    );
    assert!(
        r.notes.iter().any(|n| n.contains("did not answer")),
        "{:?}",
        r.notes
    );
}

#[test]
fn ipc_ping_and_mismatched_answers() {
    let server = MockServer::answering(agent_harness::Decision::Allow);
    let broker = PipeBroker::new(&server.path);
    assert!(broker.ping());
    assert!(matches!(
        server.seen.lock()[0],
        agent_harness::ClientMessage::Ping { v: 1 }
    ));

    // a server that answers a different request id must not grant anything
    let wrong = MockServer::start(|m| match m {
        agent_harness::ClientMessage::ApprovalRequest(_) => {
            Some(agent_harness::ServerMessage::ApprovalResponse {
                v: 1,
                request_id: "someone-else".into(),
                decision: agent_harness::Decision::Allow,
                reason: None,
            })
        }
        agent_harness::ClientMessage::Ping { v } => Some(agent_harness::ServerMessage::Pong {
            v: *v,
            accepts_approvals: false,
        }),
    });
    let broker = PipeBroker::new(&wrong.path);
    assert!(
        !broker.ping(),
        "a server that does not accept approvals is not a usable queue"
    );
    let req = agent_harness::ApprovalRequest {
        v: 1,
        request_id: "mine".into(),
        run_id: "1".into(),
        thread_id: 1,
        parent_thread_id: None,
        agent: "codex".into(),
        tool: "Bash".into(),
        detail: "x".into(),
        permission: "ask".into(),
        depth: 1,
        timeout_secs: 5,
    };
    let v = broker.decide(&req);
    assert_eq!(v.decision, agent_harness::Decision::Deny);
    assert!(v.reason.unwrap().contains("different request"));
}

// ---------------------------------------------------------------------------
// apply: risk, dry run
// ---------------------------------------------------------------------------

#[test]
fn full_risk_apply_refuses_the_yes_flag_but_accepts_an_interactive_confirmation() {
    let s = setup(|cfg, _, _| {
        std::fs::create_dir_all(cfg.cwd.join(".github/workflows")).unwrap();
        std::fs::write(cfg.cwd.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
        vec![working(), text("added CI"), done()]
    });
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), cfg());
    let mut req = RunRequest::new("codex", "add ci", &repo);
    req.permission = Permission::AcceptEdits;
    req.worktree = true;
    let r = h.run(req);
    let id = r.run_id.unwrap();

    let plan = h.apply_plan(&id).unwrap();
    assert_eq!(plan.risk, Risk::Full);
    assert!(
        plan.risk_reasons[0].contains("workflow"),
        "{:?}",
        plan.risk_reasons
    );
    assert!(plan.to_text().contains("RISK (full)"));
    let e = h.apply(&id, Confirmation::Flag).unwrap_err();
    assert!(e.contains("--yes is not allowed"), "{e}");
    assert!(!repo.join(".github").exists());
    h.apply(&id, Confirmation::Interactive).unwrap();
    assert!(repo.join(".github/workflows/ci.yml").exists());
}

#[test]
fn apply_dry_run_plan_reports_conflicts_and_overlaps_without_touching_anything() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), cfg());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.permission = Permission::AcceptEdits;
    req.worktree = true;
    let id = h.run(req).run_id.unwrap();

    // the parent edited a.txt meanwhile: the patch no longer applies, and the plan says so
    std::fs::write(repo.join("a.txt"), "alpha\nparent edit\n").unwrap();
    let plan = h.apply_plan(&id).unwrap();
    assert!(!plan.applies_cleanly && !plan.can_apply);
    assert!(plan.conflict.is_some());
    assert_eq!(plan.overlaps_uncommitted, vec!["a.txt"]);
    assert!(plan.to_text().contains("BLOCKED"));
    let e = h.apply(&id, Confirmation::Interactive).unwrap_err();
    assert!(e.contains("does not apply cleanly"), "{e}");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\nparent edit\n"
    );
    assert!(!repo.join("b.txt").exists());
}

// ---------------------------------------------------------------------------
// config.toml
// ---------------------------------------------------------------------------

#[test]
fn delegation_settings_are_read_from_config_toml() {
    let s = editing_agent();
    std::fs::create_dir_all(s.data_dir()).unwrap();
    std::fs::write(
        s.data_dir().join("config.toml"),
        "[delegation]\nmax_concurrent = 1\nmax_depth = 1\nmax_permission = \"read_only\"\nallowed_agents = [\"claude\"]\n",
    )
    .unwrap();
    let h = agent_harness::Harness::builder()
        .data_dir(s.data_dir())
        .caller(caller(Permission::AcceptEdits))
        .factory(Arc::new(FactoryHandle(s.factory.clone())))
        .resolver(Arc::new(FakeResolver))
        .build()
        .unwrap();
    assert_eq!(h.config().max_concurrent, 1);
    assert_eq!(h.config().max_permission, Permission::ReadOnly);
    assert!(h.config_warning().is_none());
    let repo = init_repo(s.dir.path(), "proj");
    let mut req = RunRequest::new("claude", "x", &repo);
    req.permission = Permission::Ask;
    refused(&h.run(req), RefusalCode::PermissionExceedsConfig);
    refused(
        &h.run(RunRequest::new("codex", "x", &repo)),
        RefusalCode::AgentNotAllowed,
    );

    // a broken file falls back to the defaults and says so
    std::fs::write(s.data_dir().join("config.toml"), "[delegation\nbroken").unwrap();
    let h = agent_harness::Harness::builder()
        .data_dir(s.data_dir())
        .build()
        .unwrap();
    assert_eq!(h.config(), &DelegationConfig::default());
    assert!(h.config_warning().is_some());
}

#[test]
fn a_stale_parent_thread_id_does_not_sink_the_run() {
    let s = editing_agent();
    let repo = init_repo(s.dir.path(), "proj");
    let mut c = caller(Permission::ReadOnly);
    c.parent_thread = Some(424242);
    let h = s.harness(c, cfg());
    let r = h.run(RunRequest::new("codex", "x", &repo));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert!(
        r.notes.iter().any(|n| n.contains("424242")),
        "{:?}",
        r.notes
    );
    let link = h
        .store()
        .unwrap()
        .thread_link(ThreadId(r.thread_id.unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!((link.parent_thread_id, link.depth), (None, 1));
}

#[test]
fn the_exported_child_environment_matches_the_documented_variables() {
    // the names are a contract with the agents that read them
    assert_eq!(
        agent_harness::policy::ENV_PARENT_PERMISSION,
        "SELACHI_PARENT_PERMISSION"
    );
    assert_eq!(
        agent_harness::policy::ENV_DELEGATION_DEPTH,
        "SELACHI_DELEGATION_DEPTH"
    );
    let _ = ThreadId(1);
}
