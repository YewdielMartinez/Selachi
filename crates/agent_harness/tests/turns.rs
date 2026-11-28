#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Turns that end early and model selection, with scripted agents (no processes, no models). The
//! scripts reproduce what the real `agy` runs in `%APPDATA%\Selachi\agent-runs` did:
//!
//! * runs 15 and 17: the final text was «…esperando a que termine…» and the run was reported `done`;
//! * run 26 (and 10, 16, 17): a refused command ended the turn within a second with no answer.

mod common;

use std::sync::Arc;

use agent_harness::events::{EventBody, RunEvent};
use agent_harness::{
    ApprovalMode, DelegationConfig, FixedModels, Permission, RefusalCode, RunRequest, RunResult,
    RunStatus,
};
use agents_driver::catalog::{AgentModels, CatalogModel};
use agents_driver::{AgentEvent, ToolKind};
use common::*;

/// The whole final answer of run 15 (agy, accept-edits, status `done`).
const RUN_15: &str = "Ejecutando `lint`, `test` y `build` en `@violet-code/admin` (`oxlint` y las 16 suites / 69 pruebas ya pasaron; esperando a que termine `tsc -b && vite build`).";
/// The whole final answer of run 17.
const RUN_17: &str = "Ejecutando verificación (`lint`, `test` y `build`); esperando a que finalice el comando en segundo plano.";
const REPORT: &str = "Archivos cambiados: apps/admin/src/a.tsx.\n\nResultados: `npm run lint`: exit 0; `npm run test`: exit 0; `npm run build`: exit 0.\n\nNo quedó nada sin hacer.";

fn tool(id: &str, command: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        kind: ToolKind::Command,
        title: command.into(),
        input_json: serde_json::json!({ "command": command }),
    }
}

fn tool_result(id: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        id: id.into(),
        ok: true,
        summary: "Command exited with code 0.".into(),
    }
}

fn events_of(h: &agent_harness::Harness, r: &RunResult) -> Vec<RunEvent> {
    let path = h.registry().events_file(r.run_id.as_deref().unwrap());
    RunEvent::parse_all(&std::fs::read_to_string(path).unwrap())
}

fn follow_ups(evs: &[RunEvent]) -> Vec<(String, String)> {
    evs.iter()
        .filter_map(|e| match &e.body {
            EventBody::FollowUp { reason, text } => Some((reason.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

fn run_agy(s: &Setup, req: impl FnOnce(&mut RunRequest)) -> (agent_harness::Harness, RunResult) {
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), DelegationConfig::default());
    let mut r = RunRequest::new("agy", "arregla los tipos y verifica", &repo);
    r.permission = Permission::AcceptEdits;
    req(&mut r);
    let out = h.run(r);
    (h, out)
}

// ---------------------------------------------------------------------------
// A background command is not a finished task
// ---------------------------------------------------------------------------

#[test]
fn run_15_text_gets_one_follow_up_and_the_final_report_becomes_the_result() {
    let s = setup(|_, n, _| {
        if n == 0 {
            vec![
                working(),
                tool("c1", "npm run lint; npm run test; npm run build"),
                tool_result("c1"),
                text(RUN_15),
                done(),
            ]
        } else {
            vec![working(), text(REPORT), done()]
        }
    });
    let (h, r) = run_agy(&s, |_| {});
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.summary, REPORT, "the final report, not the progress line");
    let sent = s.factory.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(
        sent[1].split(". ").next().unwrap(),
        "Espera a que termine el comando y responde con el resultado final"
    );
    assert!(
        r.notes.iter().any(|n| n.contains("esperar")),
        "{:?}",
        r.notes
    );
    let f = follow_ups(&events_of(&h, &r));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].0, "background");
}

#[test]
fn run_17_text_that_stays_unfinished_is_incomplete_not_done() {
    let s = setup(|_, _, _| vec![working(), text(RUN_17), done()]);
    let (h, r) = run_agy(&s, |_| {});
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert_eq!(r.status.exit_code(), 1);
    let reason = r.reason.clone().unwrap();
    assert!(reason.starts_with("Incompleto"), "{reason}");
    assert!(
        r.summary.contains(RUN_17),
        "the last message is kept: {}",
        r.summary
    );
    // asked once, not in a loop
    assert_eq!(s.factory.sent().len(), 2);
    assert_eq!(follow_ups(&events_of(&h, &r)).len(), 1);
}

#[test]
fn a_command_still_running_when_the_turn_ends_is_followed_up_whatever_the_text() {
    let s = setup(|_, n, _| {
        if n == 0 {
            // tool_start without tool_end: the agent never saw the result
            vec![
                working(),
                tool("c1", "npm run build"),
                text("Listo."),
                done(),
            ]
        } else {
            vec![working(), tool_result("c1"), text(REPORT), done()]
        }
    });
    let (_h, r) = run_agy(&s, |_| {});
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(s.factory.sent().len(), 2);
}

#[test]
fn a_command_that_never_ends_is_incomplete() {
    let s = setup(|_, n, _| {
        vec![
            working(),
            tool(&format!("c{n}"), "npm run build"),
            text("Listo."),
            done(),
        ]
    });
    let (_h, r) = run_agy(&s, |_| {});
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert!(r.reason.unwrap().contains("en ejecución"));
}

#[test]
fn wait_for_background_false_keeps_the_old_behaviour() {
    let s = setup(|_, _, _| vec![working(), text(RUN_15), done()]);
    let (_h, r) = run_agy(&s, |req| req.wait_for_background = Some(false));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(s.factory.sent().len(), 1);
}

#[test]
fn a_normal_report_is_not_followed_up() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let (_h, r) = run_agy(&s, |_| {});
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(s.factory.sent().len(), 1);
}

// ---------------------------------------------------------------------------
// A refused command is explained to the agent
// ---------------------------------------------------------------------------

fn credential_read() -> AgentEvent {
    AgentEvent::ApprovalRequest {
        id: "req_1".into(),
        tool: "Bash".into(),
        detail: "Test-Path C:\\p\\apps\\admin\\.env.local; if (Test-Path C:\\p\\apps\\admin\\.env.local) { Get-Content C:\\p\\apps\\admin\\.env.local }".into(),
    }
}

fn auto_cfg() -> DelegationConfig {
    DelegationConfig {
        approvals: ApprovalMode::Auto,
        ..DelegationConfig::default()
    }
}

#[test]
fn run_26_a_blocked_command_then_silence_gets_the_reason_and_continues() {
    let s = setup(|_, n, _| {
        if n == 0 {
            vec![working(), credential_read()]
        } else {
            vec![working(), text(REPORT), done()]
        }
    });
    // the agent ends its turn as soon as the refusal arrives
    s.factory.on_approve(|_, _| vec![done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), auto_cfg());
    let mut req = RunRequest::new("agy", "mira la configuración", &repo);
    req.permission = Permission::Ask;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.summary, REPORT);
    let sent = s.factory.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    let follow = &sent[1];
    assert!(follow.contains("Selachi rechazó"), "{follow}");
    assert!(follow.contains(".env.local"), "the command: {follow}");
    assert!(
        follow.contains("credencial") || follow.contains("credentials"),
        "the reason: {follow}"
    );
    assert!(follow.contains("Continúa sin ese comando"), "{follow}");
    assert!(follow.contains("qué quedó sin hacer"), "{follow}");
    let f = follow_ups(&events_of(&h, &r));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].0, "denial");
}

#[test]
fn a_second_silent_refusal_is_not_followed_up_again() {
    // every turn: refused command, then silence
    let s = setup(|_, _, _| vec![working(), credential_read()]);
    s.factory.on_approve(|_, _| vec![done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), auto_cfg());
    let mut req = RunRequest::new("agy", "mira la configuración", &repo);
    req.permission = Permission::Ask;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert_eq!(
        s.factory.sent().len(),
        2,
        "one follow-up, then the run ends with the explanation"
    );
    assert!(r.reason.unwrap().contains("sin ninguna respuesta"));
}

#[test]
fn a_chained_command_names_the_piece_that_was_blocked() {
    let s = setup(|_, n, _| {
        if n == 0 {
            vec![
                working(),
                AgentEvent::ApprovalRequest {
                    id: "req_1".into(),
                    tool: "Bash".into(),
                    detail: "git checkout -- package-lock.json; git status --short".into(),
                },
            ]
        } else {
            vec![working(), text(REPORT), done()]
        }
    });
    s.factory.on_approve(|_, _| vec![done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), auto_cfg());
    let mut req = RunRequest::new("agy", "x", &repo);
    req.permission = Permission::Ask;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let follow = &s.factory.sent()[1];
    assert!(
        follow.contains("La parte rechazada es `git checkout -- package-lock.json`"),
        "{follow}"
    );
    assert!(
        follow.contains("ninguna de sus partes se ejecutó"),
        "{follow}"
    );
    assert!(follow.contains("ni revertir archivos con git"), "{follow}");
}

// ---------------------------------------------------------------------------
// Preamble
// ---------------------------------------------------------------------------

#[test]
fn only_agy_gets_the_preamble_and_a_profile_can_turn_it_off() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let (_h, _r) = run_agy(&s, |_| {});
    let sent = s.factory.sent();
    assert!(
        sent[0].starts_with("=== SELACHI: REGLAS PARA ESTE AGENTE ==="),
        "{}",
        sent[0]
    );
    assert!(sent[0].contains("primer plano"));
    assert!(
        sent[0].ends_with("=== TASK ===\narregla los tipos y verifica"),
        "{}",
        sent[0]
    );

    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(RunRequest::new("codex", "hola", &repo));
    assert_eq!(r.status, RunStatus::Done);
    assert_eq!(s.factory.sent()[0], "hola", "codex gets the task untouched");

    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let dir = repo.join(".selachi").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("q.toml"),
        "name = \"Quiet\"\nagent = \"agy\"\nagent_preamble = false\nwait_for_background = false\n",
    )
    .unwrap();
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let mut req = RunRequest::new("", "hola", &repo);
    req.profile = Some("Quiet".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(s.factory.sent()[0], "hola");
}

// ---------------------------------------------------------------------------
// Model + effort selection against the catalog
// ---------------------------------------------------------------------------

fn model(id: &str, label: &str, efforts: &[&str]) -> CatalogModel {
    CatalogModel {
        id: id.into(),
        label: label.into(),
        efforts: efforts.iter().map(|e| e.to_string()).collect(),
        default_effort: None,
        default: false,
        available: true,
    }
}

fn agy_catalog() -> Arc<FixedModels> {
    FixedModels::new(vec![AgentModels {
        agent: "agy".into(),
        fetched_ms: agents_driver::catalog::now_ms(),
        source: "agy models".into(),
        models: vec![
            model(
                "gemini-4-neon",
                "Gemini 4 Neon",
                &["low", "medium", "high", "xhigh"],
            ),
            model(
                "gemini-4-hydrogen",
                "Gemini 4 Hydrogen",
                &["low", "medium", "high"],
            ),
            model("gemini-pro-orchestrator-neon", "Orchestrator (Neon)", &[]),
        ],
        efforts: Vec::new(),
        open: false,
        error: None,
    }])
}

fn with_catalog(s: &Setup) -> agent_harness::Harness {
    s.harness_models(
        caller(Permission::AcceptEdits),
        DelegationConfig::default(),
        Arc::new(agent_harness::DenyAll),
        Vec::new(),
        agy_catalog(),
    )
}

#[test]
fn model_and_effort_given_separately_reach_agy_as_its_family_and_level() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = with_catalog(&s);
    let mut req = RunRequest::new("agy", "x", &repo);
    req.model = Some("gemini-4-neon".into());
    req.effort = Some("medium".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let spawns = s.factory.rec.lock().spawns.clone();
    let cfg = &spawns[0].1;
    assert_eq!(cfg.model.as_deref(), Some("gemini-4-neon"));
    assert_eq!(cfg.effort.as_deref(), Some("medium"));
    assert!(
        r.notes.iter().any(|n| n.contains("gemini-4-neon-medium")),
        "the real id is told: {:?}",
        r.notes
    );
    let rec = h.registry().load(r.run_id.as_deref().unwrap()).unwrap();
    assert_eq!(rec.model.as_deref(), Some("gemini-4-neon"));
    assert_eq!(rec.effort.as_deref(), Some("medium"));
}

#[test]
fn a_full_agy_id_is_split_into_family_and_effort() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = with_catalog(&s);
    let mut req = RunRequest::new("agy", "x", &repo);
    req.model = Some("gemini-4-hydrogen-high".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let cfg = s.factory.rec.lock().spawns[0].1.clone();
    assert_eq!(cfg.model.as_deref(), Some("gemini-4-hydrogen"));
    assert_eq!(cfg.effort.as_deref(), Some("high"));
}

#[test]
fn an_unknown_model_is_refused_before_anything_starts_with_the_valid_ones() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = with_catalog(&s);
    let mut req = RunRequest::new("agy", "x", &repo);
    req.model = Some("gemini-4-neo".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Refused, "{r:?}");
    assert_eq!(r.code, Some(RefusalCode::UnknownModel));
    let why = r.reason.unwrap();
    assert!(why.contains("did you mean gemini-4-neon"), "{why}");
    assert!(why.contains("gemini-4-hydrogen"), "{why}");
    assert_eq!(s.factory.spawns(), 0, "nothing was started");
    assert!(r.run_id.is_none());
}

#[test]
fn an_effort_the_model_does_not_offer_is_refused_with_the_ones_it_does() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let h = with_catalog(&s);
    let mut req = RunRequest::new("agy", "x", &repo);
    req.model = Some("gemini-4-hydrogen".into());
    req.effort = Some("xhigh".into());
    let r = h.run(req);
    assert_eq!(r.code, Some(RefusalCode::UnknownModel), "{r:?}");
    assert!(r.reason.unwrap().contains("low, medium, high"));
    assert_eq!(s.factory.spawns(), 0);
}

#[test]
fn a_profile_model_is_checked_too_and_the_request_overrides_it() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let dir = repo.join(".selachi").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("p.toml"),
        "name = \"P\"\nagent = \"agy\"\nmodel = \"gemini-4-nope\"\neffort = \"medium\"\n",
    )
    .unwrap();
    let h = with_catalog(&s);
    let mut req = RunRequest::new("", "x", &repo);
    req.profile = Some("P".into());
    let r = h.run(req);
    assert_eq!(r.code, Some(RefusalCode::UnknownModel), "{r:?}");
    // a valid model on the request wins over the profile's
    let mut req = RunRequest::new("", "x", &repo);
    req.profile = Some("P".into());
    req.model = Some("gemini-4-neon".into());
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let cfg = s.factory.rec.lock().spawns[0].1.clone();
    assert_eq!(cfg.model.as_deref(), Some("gemini-4-neon"));
    assert_eq!(
        cfg.effort.as_deref(),
        Some("medium"),
        "the profile's effort"
    );
}

#[test]
fn without_a_model_the_catalog_is_never_consulted() {
    let s = setup(|_, _, _| vec![working(), text(REPORT), done()]);
    let repo = init_repo(s.dir.path(), "proj");
    let models = FixedModels::new(Vec::new());
    let h = s.harness_models(
        caller(Permission::ReadOnly),
        DelegationConfig::default(),
        Arc::new(agent_harness::DenyAll),
        Vec::new(),
        models.clone(),
    );
    let r = h.run(RunRequest::new("agy", "x", &repo));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(
        models.refreshes.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[test]
fn many_silent_tool_failures_in_a_row_stop_the_run_with_a_clear_reason() {
    // runs 10 and 17: every read ended ok=false with the path as its only output
    let s = setup(|_, _, _| {
        let mut ev = vec![working()];
        for i in 0..20 {
            let id = format!("r{i}");
            let title = format!("File /C:/p/src/f{i}.rs");
            ev.push(AgentEvent::ToolCall {
                id: id.clone(),
                kind: ToolKind::Read,
                title: title.clone(),
                input_json: serde_json::json!({}),
            });
            ev.push(AgentEvent::ToolResult {
                id,
                ok: false,
                summary: title,
            });
        }
        ev
    });
    let (_h, r) = run_agy(&s, |_| {});
    assert_eq!(r.status, RunStatus::Error, "{r:?}");
    assert!(r.reason.unwrap().starts_with("Detenido"), "{}", r.summary);
}
