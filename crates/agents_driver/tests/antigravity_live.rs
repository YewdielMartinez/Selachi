//! Live smoke tests against the installed Antigravity CLI (`agy`). Run with
//! `cargo test -p agents_driver --test antigravity_live -- --ignored --nocapture`.
//!
//! Only the ACP handshake (`initialize` + `session/new`) and `agy models` / `agy agents` are
//! performed: **no prompt is ever sent**, so no model runs and nothing is billed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use agents_driver::*;
use futures::channel::mpsc::unbounded;

#[test]
#[ignore = "needs agy installed; handshake only, no model call"]
fn live_antigravity_handshake_without_prompt() {
    let exe = agents_driver::antigravity::resolve()
        .expect("agy not found (PATH or LOCALAPPDATA agy bin)");
    eprintln!("agy executable: {}", exe.display());
    let cwd = std::env::temp_dir();
    let mut cfg = ThreadConfig::new(&cwd);
    cfg.effort = Some("max".into());
    let kind = AgentKind::antigravity(&exe, None, &[]);
    let AgentKind::Acp { command } = &kind else {
        panic!("not an ACP preset")
    };
    let (tx, mut rx) = unbounded();
    let mut d = agents_driver::acp::AcpDriver::spawn_command_line(command, &cfg, tx)
        .expect("spawn agy acp");
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut started, mut models) = (None, None);
    while Instant::now() < deadline && (started.is_none() || models.is_none()) {
        match rx.try_recv() {
            Ok(AgentEvent::SessionStarted { session_id, model }) => {
                started = Some((session_id, model))
            }
            Ok(AgentEvent::Models { models: m, current }) => models = Some((m, current)),
            Ok(AgentEvent::Status(AgentStatus::Error(e))) => panic!("agent error: {e}"),
            Ok(other) => eprintln!("event: {other:?}"),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    // Let the setup requests (thinking level for `max`) complete before closing.
    std::thread::sleep(Duration::from_millis(800));
    d.shutdown();
    let (sid, model) = started.expect("no SessionStarted within 60 s");
    let (models, current) = models.expect("no Models event");
    eprintln!(
        "session {sid} model {model:?} ({} models, current {current:?})",
        models.len()
    );
    for m in &models {
        eprintln!(
            "  {} | {} | {:?} | default {:?}",
            m.id, m.label, m.efforts, m.default_effort
        );
    }
    assert!(!sid.is_empty());
    assert!(
        models.iter().any(|m| !m.efforts.is_empty()),
        "no model with thinking levels"
    );
}

#[test]
#[ignore = "needs agy installed"]
fn live_antigravity_models_and_agents() {
    let models =
        agents_driver::antigravity::list_models(Duration::from_secs(30)).expect("agy models");
    eprintln!(
        "{} models via `agy models`: {:?}",
        models.len(),
        models.iter().map(|m| &m.id).collect::<Vec<_>>()
    );
    assert!(models.iter().any(|m| m.efforts.len() > 1));
    let agents =
        agents_driver::antigravity::list_agents(Duration::from_secs(30)).expect("agy agents");
    eprintln!("{} agents via `agy agents`: {agents:?}", agents.len());
    let via_probe = probe_models(
        AgentKind::antigravity(&agents_driver::antigravity::resolve().unwrap(), None, &[]),
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .expect("probe_models");
    assert_eq!(via_probe.len(), models.len());
}

#[test]
#[ignore = "needs agy installed"]
fn live_detect_finds_antigravity() {
    let found = agents_driver::detect();
    let agy = found
        .iter()
        .find(|a| matches!(&a.kind, AgentKind::Acp { command } if is_antigravity(command)))
        .expect("agy not detected");
    eprintln!("{agy:?}");
    assert!(agy.path.is_file());
    assert!(agy
        .version
        .as_deref()
        .is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())));
}
