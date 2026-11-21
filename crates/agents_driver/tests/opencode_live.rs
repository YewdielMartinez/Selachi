//! Live smoke test against the installed `opencode`. Needs the CLI; run with
//! `cargo test -p agents_driver --test opencode_live -- --ignored --nocapture`.
//!
//! It only performs the ACP handshake (`initialize` + `session/new`): **no prompt is ever
//! sent**, so no model is called and nothing is billed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use agents_driver::*;
use futures::channel::mpsc::unbounded;

#[test]
#[ignore = "needs opencode installed; handshake only, no model call"]
fn live_opencode_handshake_without_prompt() {
    let exe = agents_driver::opencode::resolve().expect("opencode not found");
    eprintln!("opencode executable: {}", exe.display());
    let cfg = ThreadConfig::new(env!("CARGO_MANIFEST_DIR"));
    let (tx, mut rx) = unbounded();
    let mut d = agents_driver::acp::AcpDriver::spawn_opencode(&cfg, tx).expect("spawn opencode acp");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut started = None;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(AgentEvent::SessionStarted { session_id, model }) => {
                started = Some((session_id, model));
                break;
            }
            Ok(AgentEvent::Status(AgentStatus::Error(e))) => panic!("agent error: {e}"),
            Ok(other) => eprintln!("event: {other:?}"),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    d.shutdown();
    let (sid, model) = started.expect("no SessionStarted within 60 s");
    eprintln!("session {sid} model {model:?}");
    assert!(sid.starts_with("ses_"));
    assert!(model.is_some());
}

#[test]
#[ignore = "needs opencode installed"]
fn live_detect_finds_opencode() {
    let found = agents_driver::detect();
    let oc = found.iter().find(|a| a.kind == AgentKind::OpenCode).expect("opencode not detected");
    eprintln!("{oc:?}");
    assert!(oc.path.is_file());
    assert!(oc.version.as_deref().is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())));
}
