#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The per-run events log (`agent-runs/<id>.events.jsonl`) and «Permitir siempre en este perfil»
//! (`always_profile`) through the harness with a scripted agent and a counting broker.

mod common;

use std::path::Path;
use std::sync::Arc;

use agent_harness::events::{ApprovalOutcome, EventBody, EventToolKind, RunEvent};
use agent_harness::{
    ApprovalBroker, ApprovalMode, ApprovalRequest, DelegationConfig, Permission, RunRequest,
    RunStatus, Verdict,
};
use agents_driver::{AgentEvent, ToolKind};
use chat_core::ApprovalDecision;
use common::*;
use parking_lot::Mutex;
use serde_json::json;

const SECRET: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";

struct Counting {
    answer: agent_harness::Decision,
    seen: Mutex<Vec<ApprovalRequest>>,
}

impl Counting {
    fn new(answer: agent_harness::Decision) -> Arc<Counting> {
        Arc::new(Counting {
            answer,
            seen: Mutex::new(Vec::new()),
        })
    }
    fn details(&self) -> Vec<String> {
        self.seen.lock().iter().map(|r| r.detail.clone()).collect()
    }
}

impl ApprovalBroker for Counting {
    fn decide(&self, req: &ApprovalRequest) -> Verdict {
        self.seen.lock().push(req.clone());
        Verdict {
            decision: self.answer,
            reason: None,
            forwarded: true,
        }
    }
}

fn cfg(mode: ApprovalMode) -> DelegationConfig {
    DelegationConfig {
        approvals: mode,
        ..DelegationConfig::default()
    }
}

fn events_of(h: &agent_harness::Harness, r: &agent_harness::RunResult) -> Vec<RunEvent> {
    let path = h.registry().events_file(r.run_id.as_deref().unwrap());
    let text = std::fs::read_to_string(&path).unwrap();
    // every line is a complete, known event
    assert!(
        text.lines().all(|l| RunEvent::parse_line(l).is_some()),
        "{text}"
    );
    RunEvent::parse_all(&text)
}

fn tool_call(id: &str, kind: ToolKind, title: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        kind,
        title: title.into(),
        input_json: input,
    }
}

fn tool_result(id: &str, ok: bool, summary: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        id: id.into(),
        ok,
        summary: summary.into(),
    }
}

#[test]
fn a_run_leaves_an_ordered_redacted_events_log() {
    let s = setup(|_, _, _| {
        vec![
            working(),
            text(&format!("Voy a probar con la clave {SECRET} y sigo.")),
            tool_call(
                "c1",
                ToolKind::Command,
                "cargo test -p store",
                json!({"command": "cargo test -p store"}),
            ),
            tool_result("c1", true, "test result: ok. 3 passed"),
            tool_call(
                "c2",
                ToolKind::Command,
                "cargo build",
                json!({"command": "cargo build"}),
            ),
            tool_result("c2", false, "exit 101"),
            tool_call(
                "e1",
                ToolKind::Edit,
                "Edit",
                json!({"file_path": "a.txt", "old_string": "alpha", "new_string": "alpha\nbeta"}),
            ),
            tool_result("e1", true, "edited"),
            tool_call("r1", ToolKind::Read, "Read", json!({"file_path": "a.txt"})),
            tool_result("r1", true, "alpha"),
            AgentEvent::ApprovalRequest {
                id: "req_1".into(),
                tool: "Bash".into(),
                detail: "cargo fmt".into(),
            },
        ]
    });
    s.factory.on_approve(|_, _| vec![text("listo"), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness_with(
        caller(Permission::AcceptEdits),
        cfg(ApprovalMode::Ask),
        Counting::new(agent_harness::Decision::Allow),
    );
    let mut req = RunRequest::new("codex", "go", &repo);
    req.permission = Permission::Ask;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");

    let evs = events_of(&h, &r);
    let kinds: Vec<&str> = evs
        .iter()
        .map(|e| match &e.body {
            EventBody::MessageDelta { .. } => "delta",
            EventBody::ToolStart { .. } => "start",
            EventBody::ToolEnd { .. } => "end",
            EventBody::FileEdit { .. } => "file",
            EventBody::Approval { .. } => "approval",
            EventBody::Status { .. } => "status",
            EventBody::Truncated { .. } => "truncated",
            EventBody::FollowUp { .. } => "follow_up",
        })
        .collect();
    assert_eq!(kinds.first(), Some(&"status"), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"status"), "{kinds:?}");
    // the text came before the first tool; start precedes end for each tool
    let pos = |k: &str| kinds.iter().position(|x| *x == k).unwrap();
    assert!(
        pos("delta") < pos("start") && pos("start") < pos("end"),
        "{kinds:?}"
    );

    // nothing secret in the file
    let raw =
        std::fs::read_to_string(h.registry().events_file(r.run_id.as_deref().unwrap())).unwrap();
    assert!(!raw.contains(SECRET), "{raw}");

    let ends: Vec<_> = evs
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ToolEnd {
                id,
                exit_code,
                ok,
                output_tail,
                ..
            } => Some((id.as_str(), *exit_code, *ok, output_tail.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(ends[0], ("c1", Some(0), true, "test result: ok. 3 passed"));
    assert_eq!(ends[1], ("c2", Some(101), false, "exit 101"));

    let starts: Vec<_> = evs
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ToolStart {
                id,
                kind,
                command,
                path,
                ..
            } => Some((id.as_str(), *kind, command.clone(), path.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts[0],
        (
            "c1",
            EventToolKind::Command,
            Some("cargo test -p store".to_string()),
            None
        )
    );
    assert_eq!(
        starts[2],
        ("e1", EventToolKind::Edit, None, Some("a.txt".to_string()))
    );
    assert_eq!(
        starts[3],
        ("r1", EventToolKind::Read, None, Some("a.txt".to_string()))
    );
    assert!(evs.iter().any(|e| matches!(&e.body,
        EventBody::FileEdit { path, added: 1, removed: 0 } if path == "a.txt")));
    let approval = evs
        .iter()
        .find_map(|e| match &e.body {
            EventBody::Approval {
                request,
                decision,
                mode,
                ..
            } => Some((request.clone(), *decision, mode.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        approval,
        (
            "cargo fmt".to_string(),
            ApprovalOutcome::Allowed,
            "ask".to_string()
        )
    );
    match &evs.last().unwrap().body {
        EventBody::Status { status } => assert_eq!(status, "done"),
        _ => unreachable!(),
    }
    // the file is not a run record: the registry listing is untouched
    assert_eq!(h.registry().list().len(), 1);
}

#[test]
fn blocked_and_auto_approvals_are_logged_with_their_mode() {
    let cmds: &'static [&'static str] = &["git push origin main", "cargo build"];
    let s = setup(move |_, n, _| {
        if n > 0 {
            // the harness's follow-up after the refusal: the agent reports
            return vec![working(), text("Listo; el push fue rechazado."), done()];
        }
        vec![
            working(),
            AgentEvent::ApprovalRequest {
                id: "req_1".into(),
                tool: "Bash".into(),
                detail: cmds[0].into(),
            },
        ]
    });
    s.factory.on_approve(move |id, _| match id {
        "req_1" => vec![AgentEvent::ApprovalRequest {
            id: "req_2".into(),
            tool: "Bash".into(),
            detail: cmds[1].into(),
        }],
        _ => vec![done()],
    });
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness_with(
        caller(Permission::AcceptEdits),
        cfg(ApprovalMode::Auto),
        Counting::new(agent_harness::Decision::Allow),
    );
    let mut req = RunRequest::new("codex", "go", &repo);
    req.permission = Permission::Ask;
    let r = h.run(req);
    let evs = events_of(&h, &r);
    let approvals: Vec<_> = evs
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::Approval {
                request,
                decision,
                mode,
                reason,
            } => Some((request.clone(), *decision, mode.clone(), reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[0].1, ApprovalOutcome::Blocked);
    assert!(approvals[0].3.is_some());
    assert_eq!(approvals[0].2, "auto");
    assert_eq!(approvals[1].1, ApprovalOutcome::Auto);
    assert_eq!(approvals[1].0, "cargo build");
}

fn write_profile(repo: &Path, toml: &str) -> std::path::PathBuf {
    let dir = repo.join(".selachi").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("p.toml");
    std::fs::write(&p, toml).unwrap();
    p
}

fn agent_running(commands: &'static [&'static str]) -> Setup {
    let s = setup(move |_, _, _| vec![working(), ask(1, commands[0])]);
    s.factory.on_approve(move |id, _| {
        let n: usize = id.trim_start_matches("req_").parse().unwrap();
        match commands.get(n) {
            Some(c) => vec![ask(n + 1, c)],
            None => vec![text("all done"), done()],
        }
    });
    s
}

fn ask(n: usize, command: &str) -> AgentEvent {
    AgentEvent::ApprovalRequest {
        id: format!("req_{n}"),
        tool: "Bash".into(),
        detail: command.into(),
    }
}

const PROFILE: &str = "# tests profile\nname = \"Tests\"\nagent = \"codex\"\npermission = \"ask\"\n\n# commands approved on their own\nallowed_commands = [\n  \"cargo build *\", # build\n]\n";

#[test]
fn always_profile_saves_the_pattern_and_approves_the_matching_rest() {
    let s = agent_running(&[
        "cargo test -p store",
        "cargo test -p other --release",
        "npm install",
        "git push origin main",
    ]);
    let repo = init_repo(s.dir.path(), "proj");
    let file = write_profile(&repo, PROFILE);
    let broker = Counting::new(agent_harness::Decision::AlwaysProfile);
    let h = s.harness_with(
        caller(Permission::AcceptEdits),
        cfg(ApprovalMode::Ask),
        broker.clone(),
    );
    let mut req = RunRequest::new("", "go", &repo);
    req.profile = Some("Tests".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");

    // the profile got the pattern, comments intact
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("# tests profile") && after.contains("# build"),
        "{after}"
    );
    assert!(after.contains("\"cargo test *\""), "{after}");

    // 1st asked, 2nd matched the new pattern, 3rd asked again (the broker allows everything it sees);
    // git push never reached it
    assert_eq!(broker.details(), ["cargo test -p store", "npm install"]);
    // `npm install` was answered AlwaysProfile as well, but `npm` ... is saved too (a plain word)
    let after_npm = std::fs::read_to_string(&file).unwrap();
    assert!(after_npm.contains("npm install *"), "{after_npm}");
    let decisions = s.factory.rec.lock().approvals.clone();
    assert_eq!(
        decisions,
        [
            ("req_1".to_string(), ApprovalDecision::Allow),
            ("req_2".to_string(), ApprovalDecision::Allow),
            ("req_3".to_string(), ApprovalDecision::Allow),
            ("req_4".to_string(), ApprovalDecision::Deny),
        ]
    );
    let l = h
        .registry()
        .load(r.run_id.as_deref().unwrap())
        .unwrap()
        .limits
        .unwrap();
    assert_eq!(l.approvals, "allowlist");
    assert!(l.allowed_commands.iter().any(|c| c == "cargo test *"));

    let evs = events_of(&h, &r);
    let outcomes: Vec<_> = evs
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::Approval { decision, mode, .. } => Some((*decision, mode.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (ApprovalOutcome::AlwaysProfile, "ask".to_string()),
            (ApprovalOutcome::Auto, "allowlist".to_string()),
            (ApprovalOutcome::AlwaysProfile, "allowlist".to_string()),
            (ApprovalOutcome::Blocked, "allowlist".to_string()),
        ]
    );
}

#[test]
fn always_profile_without_a_profile_is_a_single_allow_with_a_note() {
    let s = agent_running(&["cargo test -p store", "cargo test -p store"]);
    let repo = init_repo(s.dir.path(), "proj");
    let broker = Counting::new(agent_harness::Decision::AlwaysProfile);
    let h = s.harness_with(
        caller(Permission::AcceptEdits),
        cfg(ApprovalMode::Ask),
        broker.clone(),
    );
    let mut req = RunRequest::new("codex", "go", &repo);
    req.permission = Permission::Ask;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    // both asked: nothing was remembered
    assert_eq!(broker.details().len(), 2);
    assert!(
        r.notes.iter().any(|n| n.contains("has no profile")),
        "{:?}",
        r.notes
    );
    assert!(!repo.join(".selachi").exists());
}

#[test]
fn always_profile_never_saves_what_cannot_be_generalized() {
    let s = agent_running(&["cargo test && rm -rf target", "python x.py"]);
    let repo = init_repo(s.dir.path(), "proj");
    let file = write_profile(&repo, PROFILE);
    let broker = Counting::new(agent_harness::Decision::AlwaysProfile);
    let h = s.harness_with(
        caller(Permission::AcceptEdits),
        cfg(ApprovalMode::Ask),
        broker.clone(),
    );
    let mut req = RunRequest::new("", "go", &repo);
    req.profile = Some("Tests".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), PROFILE);
    assert_eq!(broker.details().len(), 2);
    assert!(
        r.notes.iter().any(|n| n.contains("cannot be saved")),
        "{:?}",
        r.notes
    );
}
