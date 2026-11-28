#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Runs that "finish without doing anything": an agent that ends its turn with no answer must not be
//! reported as a success, every automatic refusal is visible, and the delegation never has more
//! permission than the chat thread that made it has *now*.

mod common;

use agent_harness::events::{ApprovalOutcome, EventBody, RunEvent};
use agent_harness::{DelegationConfig, Permission, RefusalCode, RunRequest, RunResult, RunStatus};
use agents_driver::{AgentEvent, AgentKind, PermissionMode};
use chat_core::ThreadSpec;
use common::*;

fn denied(cmd: &str) -> AgentEvent {
    AgentEvent::PolicyDenied {
        tool: "bash".into(),
        command: cmd.into(),
        reason: "solo lectura: `Remove-Item` no es un comando de consulta permitido".into(),
        blocked: false,
    }
}

fn events_of(h: &agent_harness::Harness, r: &RunResult) -> Vec<RunEvent> {
    let path = h.registry().events_file(r.run_id.as_deref().unwrap());
    RunEvent::parse_all(&std::fs::read_to_string(path).unwrap())
}

fn run_with(events: Vec<AgentEvent>) -> (Setup, agent_harness::Harness, RunResult) {
    let s = setup(move |_, _, _| events.clone());
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(RunRequest::new("agy", "mira el proyecto", &repo));
    (s, h, r)
}

#[test]
fn an_agent_that_only_got_denied_commands_is_an_error_that_says_so() {
    let (_s, h, r) = run_with(vec![
        working(),
        denied("Get-ChildItem > out.txt"),
        denied("Remove-Item x"),
        done(),
    ]);
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert_eq!(r.status.exit_code(), 1);
    let reason = r.reason.clone().unwrap();
    assert!(reason.contains("sin ninguna respuesta"), "{reason}");
    assert!(reason.contains("2 comandos"), "{reason}");
    assert!(reason.contains("Remove-Item x"), "the last one: {reason}");
    assert!(reason.contains("solo lectura"), "{reason}");
    assert!(
        reason.contains("permission") || reason.contains("permiso"),
        "how to fix it: {reason}"
    );
    assert_eq!(
        r.summary, reason,
        "the summary is the explanation, not blank"
    );
    // the events log has one approval line per denial, with the reason, and ends as an error
    let ev = events_of(&h, &r);
    let lines: Vec<_> = ev
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::Approval {
                request,
                decision,
                reason,
                ..
            } => Some((request.clone(), *decision, reason.clone())),
            _ => None,
        })
        .collect();
    // the harness told the agent why and asked it to continue once; the scripted agent repeats the
    // same two refusals in the follow-up turn (and is not asked again)
    assert_eq!(lines.len(), 4, "{ev:?}");
    assert_eq!(lines[0].0, "Get-ChildItem > out.txt");
    assert!(lines.iter().all(|(_, d, r)| *d == ApprovalOutcome::Denied
        && r.as_deref().is_some_and(|r| r.contains("solo lectura"))));
    assert!(matches!(
        &ev.last().unwrap().body,
        EventBody::Status { status } if status == "error"
    ));
    assert!(
        r.notes.iter().any(|n| n.contains("2 comandos")),
        "{:?}",
        r.notes
    );
}

#[test]
fn a_credential_denial_is_logged_as_blocked() {
    let (_s, h, r) = run_with(vec![
        working(),
        AgentEvent::PolicyDenied {
            tool: "bash".into(),
            command: "Get-Content .env".into(),
            reason: "solo lectura: toca `.env`".into(),
            blocked: true,
        },
        text("no pude leer ese archivo"),
        done(),
    ]);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let ev = events_of(&h, &r);
    assert!(ev.iter().any(|e| matches!(
        &e.body,
        EventBody::Approval {
            decision: ApprovalOutcome::Blocked,
            ..
        }
    )));
}

#[test]
fn an_answer_with_denials_is_still_done_with_a_note() {
    let (_s, _h, r) = run_with(vec![
        working(),
        denied("Remove-Item x"),
        text("Revisé los archivos y todo está bien."),
        done(),
    ]);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.status.exit_code(), 0);
    assert!(r.summary.contains("todo está bien"));
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("1 comando") && n.contains("solo lectura")),
        "{:?}",
        r.notes
    );
}

#[test]
fn whitespace_is_not_an_answer() {
    let (_s, _h, r) = run_with(vec![working(), text("  \n\t "), done()]);
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert!(r.reason.unwrap().contains("sin ninguna respuesta"));
}

#[test]
fn no_answer_and_no_denials_is_still_an_error() {
    let (_s, _h, r) = run_with(vec![working(), usage(10, 0, 0, 0), done()]);
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    let reason = r.reason.unwrap();
    assert!(reason.contains("sin ninguna respuesta"), "{reason}");
    assert!(!reason.contains("comandos"), "{reason}");
}

#[test]
fn the_stop_reason_is_in_the_explanation() {
    let (_s, _h, r) = run_with(vec![
        working(),
        AgentEvent::TurnEnded {
            stop_reason: "max_tokens".into(),
        },
        done(),
    ]);
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert!(r.reason.unwrap().contains("max_tokens"));
    // with an answer it is only a note
    let (_s, _h, r) = run_with(vec![
        working(),
        text("respuesta parcial"),
        AgentEvent::TurnEnded {
            stop_reason: "max_tokens".into(),
        },
        done(),
    ]);
    assert_eq!(r.status, RunStatus::Done);
    assert!(
        r.notes.iter().any(|n| n.contains("max_tokens")),
        "{:?}",
        r.notes
    );
}

#[test]
fn the_result_says_which_permission_applied_and_why() {
    let (_s, _h, r) = run_with(vec![working(), text("ok"), done()]);
    assert_eq!(r.permission.as_deref(), Some("read-only"));
    let why = r.permission_reason.clone().unwrap();
    assert!(why.contains("por defecto"), "{why}");
    let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert!(json["permission_reason"].is_string());
    assert!(
        r.to_text().contains("permission: read-only"),
        "{}",
        r.to_text()
    );

    // a read-only caller explains the cap
    let s = setup(|_, _, _| vec![working(), text("ok"), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.permission_explicit = true;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done);
    let why = r.permission_reason.unwrap();
    assert!(why.contains("solo lectura"), "{why}");
    assert!(why.contains("sesión") || why.contains("padre"), "{why}");
}

// ---------------------------------------------------------------------------
// The parent thread's permission, now
// ---------------------------------------------------------------------------

fn parent_run(
    parent_permission: Option<PermissionMode>,
    env_permission: Permission,
    asked: Permission,
) -> RunResult {
    let s = setup(|_, _, _| vec![working(), text("ok"), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let mut c = caller(env_permission);
    let h0 = s.harness(c.clone(), DelegationConfig::default());
    c.parent_thread = Some(match parent_permission {
        Some(p) => {
            let mut spec = ThreadSpec::new(&repo, AgentKind::ClaudeCode);
            spec.permission = p;
            h0.store().unwrap().create_thread(spec).unwrap().id.0
        }
        None => 424_242,
    });
    let h = s.harness(c, DelegationConfig::default());
    let mut req = RunRequest::new("codex", "x", &repo);
    req.permission = asked;
    req.permission_explicit = true;
    h.run(req)
}

#[test]
fn a_permission_lowered_after_the_agent_started_caps_the_delegation() {
    // the env said accept-edits when the chat agent was spawned; the pill is now `ask` / read-only
    let r = parent_run(
        Some(PermissionMode::ReadOnly),
        Permission::AcceptEdits,
        Permission::AcceptEdits,
    );
    assert_eq!(r.status, RunStatus::Refused, "{r:?}");
    assert_eq!(r.code, Some(RefusalCode::PermissionExceedsParent));
    let r = parent_run(
        Some(PermissionMode::Ask),
        Permission::AcceptEdits,
        Permission::AcceptEdits,
    );
    assert_eq!(r.code, Some(RefusalCode::PermissionExceedsParent), "{r:?}");
    let r = parent_run(
        Some(PermissionMode::Ask),
        Permission::AcceptEdits,
        Permission::Ask,
    );
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.permission.as_deref(), Some("ask"));
}

#[test]
fn the_parent_threads_permission_never_raises_the_callers() {
    // the thread is accept-edits now but the environment said read-only: the minimum wins
    let r = parent_run(
        Some(PermissionMode::AcceptEdits),
        Permission::ReadOnly,
        Permission::AcceptEdits,
    );
    assert_eq!(r.code, Some(RefusalCode::PermissionExceedsParent), "{r:?}");
    // equal: fine
    let r = parent_run(
        Some(PermissionMode::AcceptEdits),
        Permission::AcceptEdits,
        Permission::AcceptEdits,
    );
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    // a `full` parent delegates as accept-edits at most
    let r = parent_run(
        Some(PermissionMode::Full),
        Permission::AcceptEdits,
        Permission::AcceptEdits,
    );
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
}

#[test]
fn an_unknown_parent_thread_fails_closed_to_read_only() {
    let r = parent_run(None, Permission::AcceptEdits, Permission::AcceptEdits);
    assert_eq!(r.code, Some(RefusalCode::PermissionExceedsParent), "{r:?}");
    let r = parent_run(None, Permission::AcceptEdits, Permission::ReadOnly);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert!(
        r.notes.iter().any(|n| n.contains("424242")),
        "{:?}",
        r.notes
    );
}

#[test]
fn a_lowered_parent_is_explained_in_the_notes() {
    let r = parent_run(
        Some(PermissionMode::ReadOnly),
        Permission::AcceptEdits,
        Permission::ReadOnly,
    );
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("read-only") && n.contains("hilo")),
        "{:?}",
        r.notes
    );
    assert!(r.permission_reason.unwrap().contains("hilo"));
}
