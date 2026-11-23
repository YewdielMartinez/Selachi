#![allow(clippy::unwrap_used, clippy::expect_used)]
//! chat_core end to end with scripted agents: recorded fixtures replayed through a fake driver
//! into `ThreadState`, persistence, worktrees, checkpoints, handoff and deletion. No real model
//! is ever called.

mod common;

use std::sync::Arc;
use std::time::Duration;

use agents_driver::{AgentEvent, AgentKind, AgentStatus, PermissionMode, TaskState, ToolKind};
use chat_core::{
    ApprovalDecision, ApprovalState, Block, ChatDb, ChatError, CostSource, DeleteOptions, Role,
    ThreadFilter, ThreadGroup, ThreadSpec, ThreadStatus, ToolStatus,
};
use common::*;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

fn spec(project: &std::path::Path, agent: AgentKind) -> ThreadSpec {
    ThreadSpec::new(project, agent)
}

fn done_script() -> Arc<FakeFactory> {
    FakeFactory::new(|_, _| {
        vec![
            AgentEvent::Status(AgentStatus::Working),
            AgentEvent::Text { text: "ok".into(), delta: false },
            AgentEvent::Status(AgentStatus::Done),
        ]
    })
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[test]
fn claude_basic_fixture_into_state_hooks_and_db() {
    let dir = tempfile::tempdir().unwrap();
    let events = claude_events("claude_basic.jsonl");
    let factory = FakeFactory::new(move |_, _| events.clone());
    let hooks = Arc::new(FakeHooks::default());
    let store = store_at(dir.path(), &factory, Some(hooks.clone()));
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();

    rt.send("Reply ok", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done), "turn did not finish");

    rt.with_state(|s| {
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].role, Role::User);
        let a = &s.messages[1];
        assert_eq!((a.role, a.plain_text().as_str(), a.streaming), (Role::Assistant, "ok", false));
        let u = a.usage.unwrap();
        assert_eq!((u.input, u.output, u.cache_create, u.cache_read), (2, 4, 52063, 0));
        let c = a.cost.unwrap();
        assert_eq!(c.source, CostSource::Reported);
        assert!((c.usd - 0.2084219).abs() < 1e-9);
        assert!(s.usage.cost_is_exact() && (s.usage.cost_usd() - 0.2084219).abs() < 1e-9);
        assert_eq!(s.usage.tokens(), 2 + 4 + 52063);
        let ctx = s.context.unwrap();
        assert_eq!(ctx.window_tokens, 1_000_000);
        assert!((ctx.percent - 5.2069).abs() < 0.01, "{}", ctx.percent);
        assert!(s.rate_limits.iter().any(|w| w.name == "five_hour" && w.used_percent == 31.0));
        assert_eq!(s.session_id.as_deref(), Some("00000000-0000-4000-8000-000000000001"));
        assert_eq!(s.model.as_deref(), Some("claude-haiku-5-5"));
    });
    // Title derived from the first message.
    assert_eq!(rt.record().display_title(), "Reply ok");

    // Process ownership: one owner per thread, the agent pid registered under it.
    assert_eq!(hooks.opened.lock().len(), 1);
    assert_eq!(hooks.opened.lock()[0].1.agent, "claude");
    assert_eq!(factory.rec.lock().spawns[0].2, Some(777), "job handle passed to the spawner");
    assert_eq!(*hooks.roots.lock(), vec![(rec.id, 4242)]);

    rt.close();
    assert_eq!(*hooks.closed.lock(), vec![rec.id]);
    assert!(rt.send("again", vec![]).is_err());
    drop(store);

    // Reopen from disk: same conversation, same totals, resume id used on the next send.
    let store = store_at(dir.path(), &factory, None);
    let rt = store.open(rec.id).unwrap();
    rt.with_state(|s| {
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[1].plain_text(), "ok");
        assert!((s.usage.cost_usd() - 0.2084219).abs() < 1e-9);
        assert_eq!(s.status, ThreadStatus::Done);
        assert_eq!(s.session_id.as_deref(), Some("00000000-0000-4000-8000-000000000001"));
    });
    rt.send("second", vec![]).unwrap();
    factory.wait("second spawn", |r| r.spawns.len() == 2);
    assert_eq!(
        factory.rec.lock().spawns[1].1.resume_session.as_deref(),
        Some("00000000-0000-4000-8000-000000000001")
    );
    rt.close();
}

#[test]
fn claude_tools_fixture_groups_plan_and_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let all = claude_events("claude_tools_synthetic.jsonl");
    let wait_idx = all.iter().position(|e| matches!(e, AgentEvent::Status(AgentStatus::WaitingForInput))).unwrap();
    let (head, tail) = (all[..=wait_idx].to_vec(), all[wait_idx + 1..].to_vec());
    let factory = FakeFactory::new(move |_, _| head.clone());
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("do things", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::WaitingForInput));

    rt.with_state(|s| {
        // text deltas merged, then the complete text, thinking, tools in order
        let a = s.open_message().expect("turn still open");
        let kinds: Vec<String> = a
            .blocks
            .iter()
            .map(|b| match b {
                Block::Text { text } => format!("text:{text}"),
                Block::Thinking(t) => format!("thinking:{}:{}", t.text, t.collapsed),
                Block::Explored(g) => g.label(),
                Block::Command(r) => format!("command:{}:{:?}", r.title, r.status),
                Block::Tool(r) => format!("tool:{}", r.title),
                Block::Approval(a) => format!("approval:{}:{:?}", a.tool, a.state),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "text:Hello",
                "thinking:hmm:true",
                "text:Done.",
                "Explored · 1 file",
                "command:cargo build:Failed",
                "approval:Bash:Pending",
            ]
        );
        // checklist
        let plan = s.plan.as_ref().unwrap();
        assert_eq!(plan.progress(), (1, 3));
        assert_eq!(plan.items[1].state, TaskState::InProgress);
        // approvals
        let pending = s.pending_approvals();
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].request_id.as_str(), pending[0].detail.as_str()), ("req_1", "rm -rf build"));
    });

    // Unknown approvals are rejected; the real one is forwarded with its decision.
    assert!(matches!(rt.approve("nope", ApprovalDecision::Allow), Err(ChatError::NotFound(_))));
    rt.approve("req_1", ApprovalDecision::Always).unwrap();
    factory.wait("approval forwarded", |r| r.approvals.len() == 1);
    assert_eq!(factory.rec.lock().approvals[0], ("req_1".to_string(), ApprovalDecision::Always));
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Working && s.pending_approvals().is_empty()));

    // "Always" is remembered: the next request for the same tool is answered without the user.
    factory.push(vec![AgentEvent::ApprovalRequest { id: "req_2".into(), tool: "Bash".into(), detail: "ls".into() }]);
    factory.wait("auto approval", |r| r.approvals.len() == 2);
    assert_eq!(factory.rec.lock().approvals[1], ("req_2".to_string(), ApprovalDecision::Always));

    // The turn ends with the recorded error.
    factory.push(tail);
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Error));
    rt.with_state(|s| {
        assert_eq!(s.error.as_deref(), Some("boom"));
        let a = s.last_assistant().unwrap();
        let states: Vec<_> = a
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Approval(x) => Some((x.request_id.clone(), x.state)),
                _ => None,
            })
            .collect();
        assert!(states.contains(&("req_1".to_string(), ApprovalState::AlwaysAllowed)));
        // answered "always" for req_2 as well (auto) and the rest is final
        assert!(!states.iter().any(|(_, st)| *st == ApprovalState::Pending));
    });
    rt.close();

    // Persistence: tool runs and approvals landed in the database.
    store.flush().unwrap();
    let db = ChatDb::open(store.db_path()).unwrap();
    let runs = db.load_tool_runs(rec.id).unwrap();
    let kinds: Vec<_> = runs.iter().map(|r| (r.kind.as_str(), r.ok)).collect();
    assert_eq!(kinds, [("read", Some(true)), ("command", Some(false))]);
    let ap = db.load_approvals(rec.id).unwrap();
    assert_eq!(ap[0].request_id, "req_1");
    assert_eq!(ap[0].decision.as_deref(), Some("always"));
    assert!(db.load_plan(rec.id).unwrap().unwrap().contains("Write tests"));
}

#[test]
fn codex_fixture_replay() {
    let dir = tempfile::tempdir().unwrap();
    let events = codex_events("codex_basic.jsonl");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Text { .. })), "replay produced no text");
    let factory = FakeFactory::new(move |_, _| events.clone());
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::Codex)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("Reply with exactly: ok", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    rt.with_state(|s| {
        assert_eq!(s.last_assistant().unwrap().plain_text(), "ok");
        assert_eq!((s.usage.input, s.usage.output), (19702, 5));
        let ctx = s.context.unwrap();
        assert_eq!(ctx.window_tokens, 258_400);
        assert!((ctx.percent - 7.627).abs() < 0.01, "{}", ctx.percent);
        assert_eq!(s.rate_limits[0].name, "primary");
        assert_eq!(s.rate_limits[0].used_percent, 12.0);
        assert!(s.session_id.as_deref().unwrap().starts_with("01a1fa88"));
        // Codex reports no cost: never presented as reported
        assert!(s.usage.cost_reported_usd == 0.0);
    });
    rt.close();
}

#[test]
fn opencode_handshake_and_acp_tool_calls_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut events = opencode_handshake_events();
    assert!(matches!(&events[0], AgentEvent::SessionStarted { session_id, .. } if session_id.starts_with("ses_e048")));
    let call = |id: &str, kind: &str, title: &str| AgentEvent::ToolCall {
        id: id.into(),
        kind: agents_driver::acp::tool_kind(kind),
        title: title.into(),
        input_json: json!({ "path": "x" }),
    };
    let res = |id: &str| AgentEvent::ToolResult { id: id.into(), ok: true, summary: "done".into() };
    events.extend([
        AgentEvent::Status(AgentStatus::Working),
        call("t1", "other", "list"),
        res("t1"),
        call("t2", "search", "grep foo"),
        res("t2"),
        call("t3", "search", "glob *.rs"),
        res("t3"),
        call("t4", "read", "src/lib.rs"),
        res("t4"),
        call("t5", "execute", "cargo test"),
        AgentEvent::Usage { input: 10, output: 5, cache_create: 0, cache_read: 0, cost_usd: Some(0.25), context_window: Some(200_000) },
        AgentEvent::Status(AgentStatus::Done),
    ]);
    let factory = FakeFactory::new(move |_, _| events.clone());
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::OpenCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("explore", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    rt.with_state(|s| {
        let a = s.last_assistant().unwrap();
        let Block::Explored(g) = &a.blocks[0] else { panic!("expected an explore group first") };
        assert_eq!(g.label(), "Explored · 1 list · 2 searches · 1 file");
        let Block::Command(c) = &a.blocks[1] else { panic!("expected a command row") };
        // announced but never answered: cancelled when the turn ended
        assert_eq!((c.title.as_str(), c.status), ("cargo test", ToolStatus::Cancelled));
        assert_eq!(a.cost.unwrap().source, CostSource::Reported);
        assert_eq!(s.model.as_deref(), Some("opencode/big-pickle"));
    });
    assert_eq!(rt.record().agent_session_id.as_deref(), Some("ses_e048338a7ffeMneVXo2TD5D85l"));
    rt.close();
}

// ---------------------------------------------------------------------------
// Persistence, listing, grouping
// ---------------------------------------------------------------------------

#[test]
fn list_filter_group_rename_archive() {
    let dir = tempfile::tempdir().unwrap();
    let proj_a = dir.path().join("a");
    let proj_b = dir.path().join("b");
    std::fs::create_dir_all(&proj_a).unwrap();
    std::fs::create_dir_all(&proj_b).unwrap();
    // `t_work` stays Working (no Done), `t_done` finishes
    let factory = FakeFactory::new(|_, text| {
        if text.contains("work") {
            vec![AgentEvent::Status(AgentStatus::Working)]
        } else {
            vec![AgentEvent::Text { text: "ok".into(), delta: false }, AgentEvent::Status(AgentStatus::Done)]
        }
    });
    let store = store_at(dir.path(), &factory, None);
    let mut s = spec(&proj_a, AgentKind::ClaudeCode);
    s.title = Some("Trabajo en curso".into());
    let t_work = store.create_thread(s).unwrap();
    let mut s = spec(&proj_a, AgentKind::Codex);
    s.title = Some("Terminado".into());
    let t_done = store.create_thread(s).unwrap();
    let t_other = store.create_thread(spec(&proj_b, AgentKind::OpenCode)).unwrap();
    assert_eq!(t_other.display_title(), "Nuevo hilo");

    let rw = store.open(t_work.id).unwrap();
    rw.send("work please", vec![]).unwrap();
    assert!(rw.wait_for(T, |s| s.status == ThreadStatus::Working));
    let rd = store.open(t_done.id).unwrap();
    rd.send("finish", vec![]).unwrap();
    assert!(rd.wait_for(T, |s| s.status == ThreadStatus::Done));

    // live statuses overlay the database
    let all = store.list_threads(&ThreadFilter::default()).unwrap();
    assert_eq!(all.len(), 3);
    let groups = store.grouped_threads(&ThreadFilter::default()).unwrap();
    assert_eq!(groups.working.iter().map(|t| t.id).collect::<Vec<_>>(), [t_work.id]);
    assert!(groups.waiting.is_empty());
    assert_eq!(groups.settled.len(), 2);
    assert_eq!(groups.busy_count(), 1);

    let in_a = store.list_threads(&ThreadFilter { project: Some(proj_a), ..Default::default() }).unwrap();
    assert_eq!(in_a.len(), 2);
    let only_done = store
        .list_threads(&ThreadFilter { status: Some(ThreadStatus::Done), ..Default::default() })
        .unwrap();
    assert_eq!(only_done.iter().map(|t| t.id).collect::<Vec<_>>(), [t_done.id]);
    let settled = store
        .list_threads(&ThreadFilter { group: Some(ThreadGroup::Settled), ..Default::default() })
        .unwrap();
    assert_eq!(settled.len(), 2);

    // rename + archive
    store.rename_thread(t_other.id, "Otro proyecto").unwrap();
    store.archive_thread(t_done.id, true).unwrap();
    assert_eq!(store.list_threads(&ThreadFilter::default()).unwrap().len(), 2);
    let archived = store
        .list_threads(&ThreadFilter { archived: chat_core::Archived::Only, ..Default::default() })
        .unwrap();
    assert_eq!(archived.iter().map(|t| t.id).collect::<Vec<_>>(), [t_done.id]);
    assert!(rd.is_closed(), "archiving stops the agent");
    assert_eq!(store.get_thread(t_other.id).unwrap().unwrap().title, "Otro proyecto");

    let by_project = chat_core::group_by_project(&store.list_threads(&ThreadFilter::default()).unwrap());
    assert_eq!(by_project.len(), 2);

    // a thread left busy by a dead process is idle after a restart
    store.shutdown();
    drop(store);
    let store = store_at(dir.path(), &factory, None);
    let back = store.get_thread(t_work.id).unwrap().unwrap();
    assert_eq!(back.status, ThreadStatus::Idle);
}

#[test]
fn secrets_are_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let factory = FakeFactory::new(move |_, _| {
        vec![
            AgentEvent::ToolCall {
                id: "c".into(),
                kind: ToolKind::Command,
                title: "env".into(),
                input_json: json!({ "command": format!("echo {secret}") }),
            },
            AgentEvent::Status(AgentStatus::Done),
        ]
    });
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send(format!("my token is {secret}"), vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    rt.with_state(|s| assert!(s.messages[0].plain_text().contains(secret), "memory keeps the real text"));
    rt.close();
    store.flush().unwrap();
    let db = ChatDb::open(store.db_path()).unwrap();
    for m in db.load_messages(rec.id).unwrap() {
        assert!(!m.content_json.contains(secret));
    }
    for r in db.load_tool_runs(rec.id).unwrap() {
        assert!(!r.input_json.contains(secret));
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn spawn_failure_is_reported_and_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let factory = done_script();
    factory.fail_spawn.store(true, std::sync::atomic::Ordering::SeqCst);
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("hola", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Error));
    rt.with_state(|s| {
        assert!(s.error.as_deref().unwrap().contains("No se encontró"));
        assert_eq!(s.messages.last().unwrap().role, Role::System);
    });
    factory.fail_spawn.store(false, std::sync::atomic::Ordering::SeqCst);
    rt.send("otra vez", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    rt.close();
}

#[test]
fn interrupt_reaches_the_driver() {
    let dir = tempfile::tempdir().unwrap();
    let factory = FakeFactory::new(|_, _| vec![AgentEvent::Status(AgentStatus::Working)]);
    *factory.on_interrupt.lock() = vec![AgentEvent::Status(AgentStatus::Done)];
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("long task", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Working));
    rt.interrupt().unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    assert_eq!(factory.rec.lock().interrupts, 1);
    rt.close();
}

#[test]
fn attachments_are_appended_to_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let factory = done_script();
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    let file = dir.path().join("spec.md");
    rt.send("lee esto", vec![file.clone()]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    let sent = factory.sent();
    assert!(sent[0].starts_with("lee esto") && sent[0].contains(&file.display().to_string()));
    rt.with_state(|s| {
        assert_eq!(s.messages[0].plain_text(), "lee esto", "the bubble shows only what the user typed");
        assert_eq!(s.messages[0].attachments, vec![file.clone()]);
    });
    assert!(rt.send("  ", vec![]).is_err());
    rt.close();
}

#[test]
fn updates_are_coalesced() {
    let dir = tempfile::tempdir().unwrap();
    let factory = FakeFactory::new(|_, _| {
        let mut ev = vec![AgentEvent::Status(AgentStatus::Working)];
        ev.extend((0..500).map(|i| AgentEvent::Text { text: format!("{i} "), delta: true }));
        ev.push(AgentEvent::Status(AgentStatus::Done));
        ev
    });
    let store = store_at(dir.path(), &factory, None);
    let mut rx = store.updates();
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("go", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    rt.close();
    let mut updates = Vec::new();
    while let Ok(u) = rx.try_recv() {
        if u.thread_id == rec.id {
            updates.push(u);
        }
    }
    assert!(!updates.is_empty());
    assert!(updates.len() < 25, "{} updates for 500 deltas", updates.len());
    assert_eq!(updates.last().unwrap().status, ThreadStatus::Done);
}

// ---------------------------------------------------------------------------
// Worktrees, checkpoints, handoff, delete
// ---------------------------------------------------------------------------

#[test]
fn worktree_thread_runs_in_its_own_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "my repo");
    let factory = done_script();
    let store = store_at(dir.path(), &factory, None);

    let mut s = spec(&repo, AgentKind::ClaudeCode);
    s.title = Some("Arregla el parser".into());
    s.use_worktree = true;
    let rec = store.create_thread(s.clone()).unwrap();
    let wt = rec.worktree_path.clone().expect("worktree path");
    assert!(wt.is_dir());
    assert!(same_dir(&wt, &dir.path().join("my repo.worktrees").join("arregla-el-parser")), "{wt:?}");
    assert!(git(&repo, &["branch", "--list", "selachi/arregla-el-parser"]).contains("selachi/arregla-el-parser"));
    assert_eq!(rec.cwd(), wt.as_path());

    // a second thread with the same title gets another name
    let rec2 = store.create_thread(s).unwrap();
    assert!(rec2.worktree_path.unwrap().ends_with("arregla-el-parser-2"));

    // the agent starts inside the worktree
    let rt = store.open(rec.id).unwrap();
    rt.send("go", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));
    assert_eq!(factory.rec.lock().spawns[0].1.cwd, wt);
    rt.close();

    // not a repo -> clear error, nothing created
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut s = spec(&plain, AgentKind::ClaudeCode);
    s.use_worktree = true;
    assert!(matches!(store.create_thread(s), Err(ChatError::NotAGitRepo(_))));
    assert_eq!(store.list_threads(&ThreadFilter::default()).unwrap().len(), 2);
}

#[test]
fn checkpoint_before_each_turn_in_full_and_accept_edits_only() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let factory = done_script();
    let store = store_at(dir.path(), &factory, None);

    let mk = |mode: PermissionMode, flag: bool| {
        let mut s = spec(&repo, AgentKind::ClaudeCode);
        s.permission = mode;
        s.checkpoint_before_turn = flag;
        store.create_thread(s).unwrap()
    };
    let turn = |rec: &chat_core::ThreadRecord, text: &str, n: usize| {
        let rt = store.open(rec.id).unwrap();
        rt.send(text, vec![]).unwrap();
        assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done && s.messages.len() == n * 2));
        rt
    };
    let count = || git_svc::Repo::discover(&repo).unwrap().unwrap().checkpoints().unwrap().len();

    // read-only and ask: none; flag off: none
    turn(&mk(PermissionMode::ReadOnly, true), "a", 1);
    turn(&mk(PermissionMode::Ask, true), "a", 1);
    turn(&mk(PermissionMode::Full, false), "a", 1);
    assert_eq!(count(), 0);

    // full: one checkpoint per turn, labelled with the turn number, linked from the user message
    std::fs::write(repo.join("a.txt"), "changed\n").unwrap();
    let rec = mk(PermissionMode::Full, true);
    let rt = turn(&rec, "first", 1);
    assert_eq!(count(), 1);
    let rt2 = turn(&rec, "second", 2);
    assert_eq!(count(), 2);
    let cps = git_svc::Repo::discover(&repo).unwrap().unwrap().checkpoints().unwrap();
    let labels: Vec<_> = cps.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"antes del turno 1") && labels.contains(&"antes del turno 2"), "{labels:?}");
    rt2.with_state(|s| {
        let refs: Vec<_> = s.messages.iter().filter_map(|m| m.checkpoint.as_ref()).map(|c| c.label.clone()).collect();
        assert_eq!(refs, ["antes del turno 1", "antes del turno 2"]);
    });
    drop(rt);

    // accept-edits too
    turn(&mk(PermissionMode::AcceptEdits, true), "x", 1);
    assert_eq!(count(), 3);

    // not a git repo: the turn runs, silently without a checkpoint
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut s = spec(&plain, AgentKind::ClaudeCode);
    s.permission = PermissionMode::Full;
    s.checkpoint_before_turn = true;
    let r = store.create_thread(s).unwrap();
    turn(&r, "hi", 1);
}

#[test]
fn switching_agent_sends_a_spanish_handoff_and_resumes_on_model_change() {
    let dir = tempfile::tempdir().unwrap();
    let factory = FakeFactory::new(|n, _| {
        if n == 0 {
            vec![
                AgentEvent::SessionStarted { session_id: "sess-1".into(), model: Some("claude-sonnet-5-5".into()) },
                AgentEvent::Status(AgentStatus::Working),
                AgentEvent::ToolCall {
                    id: "e1".into(),
                    kind: ToolKind::Edit,
                    title: "src/parser.rs".into(),
                    input_json: json!({ "file_path": "src/parser.rs", "old_string": "a", "new_string": "b" }),
                },
                AgentEvent::ToolResult { id: "e1".into(), ok: true, summary: "edited".into() },
                AgentEvent::Plan {
                    tasks: vec![
                        ("Arreglar parser".into(), TaskState::Done),
                        ("Escribir tests".into(), TaskState::InProgress),
                        ("Publicar".into(), TaskState::Pending),
                    ],
                },
                AgentEvent::Text { text: "listo".into(), delta: false },
                AgentEvent::Status(AgentStatus::Done),
            ]
        } else {
            vec![AgentEvent::Status(AgentStatus::Working), AgentEvent::Status(AgentStatus::Done)]
        }
    });
    let store = store_at(dir.path(), &factory, None);
    let rec = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    let rt = store.open(rec.id).unwrap();
    rt.send("Arregla el parser", vec![]).unwrap();
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done));

    // preview == what is sent
    let preview = rt.handoff_preview(&AgentKind::Codex, Some("gpt-6")).unwrap().unwrap();
    assert!(preview.starts_with("Traspaso de hilo: hasta ahora trabajaba Claude Code; ahora continúas tú (Codex · gpt-6)."));

    rt.switch_agent_or_model(AgentKind::Codex, Some("gpt-6".into())).unwrap();
    factory.wait("handoff delivered", |r| r.sent.len() == 2);
    assert!(rt.wait_for(T, |s| s.status == ThreadStatus::Done), "handoff turn finished");
    let (spawns, shutdowns) = {
        let r = factory.rec.lock();
        (r.spawns.clone(), r.shutdowns)
    };
    assert_eq!(spawns.len(), 2);
    assert_eq!(spawns[1].0, AgentKind::Codex);
    assert_eq!(spawns[1].1.model.as_deref(), Some("gpt-6"));
    assert_eq!(spawns[1].1.resume_session, None, "a different agent cannot resume the old session");
    assert_eq!(shutdowns, 1, "the old driver was shut down");
    let handoff = factory.sent()[1].clone();
    assert_eq!(handoff, preview);
    assert!(handoff.contains("> Arregla el parser"));
    assert!(handoff.contains("- src/parser.rs"));
    assert!(handoff.contains("- [en curso] Escribir tests") && handoff.contains("- [pendiente] Publicar"));
    assert!(!handoff.contains("] Arreglar parser"), "done items are not repeated");

    // metadata persisted, system notes in the conversation, no error from the dying old process
    let r = rt.record();
    assert_eq!((r.agent.clone(), r.model.as_deref(), r.agent_session_id.clone()), (AgentKind::Codex, Some("gpt-6"), None));
    rt.with_state(|s| {
        assert_ne!(s.status, ThreadStatus::Error);
        let notes: Vec<String> = s
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.plain_text())
            .collect();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0], "Cambiado a Codex · gpt-6");
        assert!(notes[1].starts_with("Traspaso enviado:"));
    });
    store.flush().unwrap();
    let db_rec = store.get_thread(rec.id).unwrap().unwrap();
    assert_eq!(db_rec.agent, AgentKind::Codex);

    // Same agent, other model: resume the session, no handoff message.
    factory.push(vec![AgentEvent::SessionStarted { session_id: "sess-codex".into(), model: None }]);
    assert!(rt.wait_for(T, |s| s.session_id.as_deref() == Some("sess-codex")));
    let before = factory.sent().len();
    rt.switch_agent_or_model(AgentKind::Codex, Some("gpt-6-mini".into())).unwrap();
    rt.send("sigue", vec![]).unwrap();
    factory.wait("third spawn", |r| r.spawns.len() == 3 && r.sent.len() == before + 1);
    let third = factory.rec.lock().spawns[2].clone();
    assert_eq!(third.1.model.as_deref(), Some("gpt-6-mini"));
    assert_eq!(third.1.resume_session.as_deref(), Some("sess-codex"));
    assert_eq!(factory.sent().last().unwrap(), "sigue");
    rt.close();
}

#[test]
fn delete_preview_and_delete_with_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let factory = done_script();
    let store = store_at(dir.path(), &factory, None);

    let mut s = spec(&repo, AgentKind::ClaudeCode);
    s.title = Some("Rama de prueba".into());
    s.use_worktree = true;
    let rec = store.create_thread(s.clone()).unwrap();
    let wt = rec.worktree_path.clone().unwrap();

    // no worktree: nothing to preview
    let plain = store.create_thread(spec(&repo, AgentKind::ClaudeCode)).unwrap();
    let p = store.delete_preview(plain.id).unwrap();
    assert!(p.worktree.is_none());
    assert_eq!(p.message_count, 0);

    // clean worktree
    let p = store.delete_preview(rec.id).unwrap();
    let w = p.worktree.unwrap();
    assert!(same_dir(&w.path, &wt));
    assert_eq!((w.exists, w.dirty), (true, Some(false)));
    assert_eq!(w.branch.as_deref(), Some("selachi/rama-de-prueba"));

    // dirty worktree: warned, needs force
    std::fs::write(wt.join("new.txt"), "wip\n").unwrap();
    let w = store.delete_preview(rec.id).unwrap().worktree.unwrap();
    assert_eq!(w.dirty, Some(true));
    assert!(w.requires_confirmation);
    assert!(w.warnings.iter().any(|m| m.contains("uncommitted")), "{:?}", w.warnings);

    // deleting the thread without the worktree keeps the directory
    let rec_b = store.create_thread(s).unwrap();
    let wt_b = rec_b.worktree_path.clone().unwrap();
    let rep = store.delete_thread(rec_b.id, DeleteOptions::default()).unwrap();
    assert!(!rep.worktree_removed);
    assert!(wt_b.is_dir());
    assert!(store.get_thread(rec_b.id).unwrap().is_none());

    // removing a dirty worktree without force fails and keeps the thread
    assert!(store.delete_thread(rec.id, DeleteOptions { remove_worktree: true, force: false }).is_err());
    assert!(store.get_thread(rec.id).unwrap().is_some());
    assert!(wt.is_dir());

    // with force: worktree gone, branch kept, history deleted
    let rep = store.delete_thread(rec.id, DeleteOptions { remove_worktree: true, force: true }).unwrap();
    assert!(rep.worktree_removed);
    assert_eq!(rep.branch_kept.as_deref(), Some("selachi/rama-de-prueba"));
    assert!(!wt.exists());
    assert!(git(&repo, &["branch", "--list", "selachi/rama-de-prueba"]).contains("selachi/rama-de-prueba"));
    assert!(store.get_thread(rec.id).unwrap().is_none());
    assert!(matches!(store.open(rec.id), Err(ChatError::NotFound(_))));
}

// ---------------------------------------------------------------------------
// Delegation lineage (PLAN section 20)
// ---------------------------------------------------------------------------

#[test]
fn delegation_links_children_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let factory = done_script();
    let store = store_at(dir.path(), &factory, None);

    let parent = store.create_thread(spec(dir.path(), AgentKind::ClaudeCode)).unwrap();
    assert_eq!(store.thread_link(parent.id).unwrap(), Some(chat_core::ThreadLink::default()));
    assert!(store.thread_link(chat_core::ThreadId(9999)).unwrap().is_none());

    let mut child_spec = spec(dir.path(), AgentKind::Codex);
    child_spec.link = Some(chat_core::ThreadLink {
        parent_thread_id: Some(parent.id),
        parent_tab: Some("tab-7".into()),
        depth: 1,
        delegated_by: Some("cli".into()),
    });
    let child = store.create_thread(child_spec.clone()).unwrap();
    // A delegation started from a terminal: no parent thread, still delegated.
    child_spec.link = Some(chat_core::ThreadLink {
        parent_thread_id: None,
        parent_tab: None,
        depth: 1,
        delegated_by: Some("mcp".into()),
    });
    let orphan = store.create_thread(child_spec).unwrap();

    let kids = store.children(parent.id).unwrap();
    assert_eq!(kids.iter().map(|t| t.id).collect::<Vec<_>>(), vec![child.id]);
    let link = store.thread_link(child.id).unwrap().unwrap();
    assert_eq!(link.parent_thread_id, Some(parent.id));
    assert_eq!((link.parent_tab.as_deref(), link.depth, link.delegated_by.as_deref()), (Some("tab-7"), 1, Some("cli")));

    let delegated = store.delegated_threads().unwrap();
    let ids: Vec<_> = delegated.iter().map(|(t, _)| t.id).collect();
    assert!(ids.contains(&child.id) && ids.contains(&orphan.id) && !ids.contains(&parent.id));
    store.flush().unwrap();
    drop(store);

    let store = store_at(dir.path(), &factory, None);
    assert_eq!(store.children(parent.id).unwrap().len(), 1);
    assert_eq!(store.thread_link(orphan.id).unwrap().unwrap().delegated_by.as_deref(), Some("mcp"));

    // Deleting the parent keeps the child (the link is cleared, the depth stays).
    store.delete_thread(parent.id, DeleteOptions::default()).unwrap();
    let link = store.thread_link(child.id).unwrap().unwrap();
    assert_eq!((link.parent_thread_id, link.depth), (None, 1));
}

#[test]
fn schema_v3_migrates_an_existing_v2_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chat.db");
    // Build a v2 database by hand (what shipped before the delegation columns).
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads(id INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT '', project_root TEXT NOT NULL, \
             worktree_path TEXT, agent_kind TEXT NOT NULL, model TEXT, permission_mode TEXT NOT NULL DEFAULT 'ask', \
             status TEXT NOT NULL DEFAULT 'idle', created INTEGER NOT NULL, updated INTEGER NOT NULL, \
             agent_session_id TEXT, archived INTEGER NOT NULL DEFAULT 0, checkpoint_before_turn INTEGER NOT NULL DEFAULT 0, \
             effort TEXT);\
             INSERT INTO threads(title, project_root, agent_kind, created, updated) VALUES ('viejo', 'C:/p', 'claude', 1, 1);\
             PRAGMA user_version = 2;",
        )
        .unwrap();
        // The remaining v1 tables are created by the migration only for fresh databases, so give the
        // old file the ones the store touches.
        conn.execute_batch(
            "CREATE TABLE messages(id INTEGER PRIMARY KEY, thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE, \
             seq INTEGER NOT NULL, role TEXT NOT NULL, created INTEGER NOT NULL, content_json TEXT NOT NULL, usage_json TEXT, \
             cost_usd REAL, cost_source TEXT, UNIQUE(thread_id, seq));",
        )
        .unwrap();
    }
    let db = ChatDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), chat_core::latest_version());
    let t = db.list_threads().unwrap();
    assert_eq!(t.len(), 1);
    let link = db.get_link(t[0].id).unwrap().unwrap();
    assert_eq!((link.parent_thread_id, link.depth, link.delegated_by), (None, 0, None));
}
