//! `agents_driver`: UI-agnostic process drivers for coding agents.
//!
//! One thread = one agent process. [`spawn`] returns a control handle
//! ([`AgentDriver`]) and a stream of normalized [`AgentEvent`]s:
//!
//! ```no_run
//! use agents_driver::*;
//! # fn demo() -> Result<()> {
//! let cfg = ThreadConfig::new(".");
//! let (mut driver, mut events) = spawn(AgentKind::ClaudeCode, &cfg)?;
//! driver.send("hello")?;
//! // `events` is a futures UnboundedReceiver: poll it from your executor / UI loop.
//! # Ok(()) }
//! ```
//!
//! Backends: Claude Code (`stream-json`, [`claude`]), Codex (`app-server`
//! JSON-RPC, [`codex`]), and any Agent Client Protocol agent ([`acp`]): OpenCode
//! (`opencode acp`, [`opencode`]), the Antigravity CLI (`agy acp`, preset
//! [`AgentKind::antigravity`], [`antigravity`]) and, with `AgentKind::Acp { command }`, Gemini CLI
//! and friends. No API keys: they use the user's own CLI login. Plan reference:
//! `docs/PLAN.md` section 14.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod acp;
pub mod antigravity;
pub mod catalog;
pub mod claude;
pub mod coalesce;
pub mod codex;
pub mod detect;
pub mod images;
#[cfg(windows)]
mod job;
pub mod opencode;
pub mod proc;
pub mod shell_safety;
mod types;

use futures::channel::mpsc::{unbounded, UnboundedReceiver};

pub use detect::{detect, DetectedAgent};
pub use types::*;

/// Start an agent process for `kind` and return its driver and event stream.
///
/// The receiver ends (`None`) after the final [`AgentEvent::Exited`].
pub fn spawn(
    kind: AgentKind,
    cfg: &ThreadConfig,
) -> Result<(Box<dyn AgentDriver>, UnboundedReceiver<AgentEvent>)> {
    spawn_with(kind, cfg, SpawnOpts::default())
}

/// [`spawn`] with [`SpawnOpts`]: e.g. create the agent inside the host's Job Object.
pub fn spawn_with(
    kind: AgentKind,
    cfg: &ThreadConfig,
    opts: SpawnOpts,
) -> Result<(Box<dyn AgentDriver>, UnboundedReceiver<AgentEvent>)> {
    let (tx, rx) = unbounded();
    let driver: Box<dyn AgentDriver> = match kind {
        AgentKind::ClaudeCode => Box::new(claude::ClaudeDriver::spawn_with(cfg, tx, &opts)?),
        AgentKind::Codex => Box::new(codex::CodexDriver::spawn_with(cfg, tx, &opts)?),
        AgentKind::OpenCode => Box::new(acp::AcpDriver::spawn_opencode_with(cfg, tx, &opts)?),
        AgentKind::Acp { command } => Box::new(acp::AcpDriver::spawn_command_line_with(
            &command, cfg, tx, &opts,
        )?),
    };
    Ok((driver, rx))
}

/// Which models does this agent offer? Starts the agent (no prompt is ever sent), waits for its
/// [`AgentEvent::Models`] (Codex `model/list`, ACP `session/new`) for at most `timeout` and shuts it
/// down. Blocking: call it from a background thread. Claude Code has no such call (its aliases are
/// documented), so it returns `Unsupported`. The Antigravity CLI is answered by `agy models` (no session
/// is opened), falling back to the ACP handshake. See [`catalog`] for the cached, per-agent view.
pub fn probe_models(
    kind: AgentKind,
    cwd: impl Into<std::path::PathBuf>,
    timeout: std::time::Duration,
) -> Result<Vec<ModelInfo>> {
    probe_models_with_current(kind, cwd, timeout).map(|(models, _)| models)
}

/// [`probe_models`] plus the model the agent says it currently uses (its default), when it says.
pub fn probe_models_with_current(
    kind: AgentKind,
    cwd: impl Into<std::path::PathBuf>,
    timeout: std::time::Duration,
) -> Result<(Vec<ModelInfo>, Option<String>)> {
    use futures::StreamExt;
    if kind == AgentKind::ClaudeCode {
        return Err(DriverError::Unsupported(
            "Claude Code does not list models".into(),
        ));
    }
    if let AgentKind::Acp { command } = &kind {
        if is_antigravity(command) {
            if let Ok(models) = antigravity::list_models(timeout) {
                return Ok((models, None));
            }
        }
    }
    let (mut driver, mut rx) = spawn(kind, &ThreadConfig::new(cwd))?;
    let (tx, out) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("model-probe".into())
        .spawn(move || {
            futures::executor::block_on(async move {
                while let Some(ev) = rx.next().await {
                    if let AgentEvent::Models { models, current } = ev {
                        let _ = tx.send((models, current));
                        break;
                    }
                }
            });
        })?;
    let got = out.recv_timeout(timeout);
    driver.shutdown();
    got.map_err(|_| DriverError::Protocol("the agent did not list its models in time".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_acp_with_missing_executable_is_not_found() {
        let r = spawn(
            AgentKind::Acp {
                command: "definitely-not-an-acp-agent --acp".into(),
            },
            &ThreadConfig::new("."),
        );
        assert!(matches!(r, Err(DriverError::NotFound(_))));
        let r = spawn(
            AgentKind::Acp {
                command: "  ".into(),
            },
            &ThreadConfig::new("."),
        );
        assert!(matches!(r, Err(DriverError::NotFound(_))));
    }

    #[test]
    fn events_roundtrip_through_serde() {
        let e = AgentEvent::Usage {
            input: 1,
            output: 2,
            cache_create: 3,
            cache_read: 4,
            cost_usd: Some(0.5),
            context_window: None,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), e);
    }
}
