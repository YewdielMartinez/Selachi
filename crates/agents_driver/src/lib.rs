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
//! JSON-RPC, [`codex`]), ACP (stub, [`acp`]). No API keys: they use the user's own
//! CLI login. Plan reference: `docs/PLAN.md` section 14.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod acp;
pub mod claude;
pub mod codex;
pub mod coalesce;
pub mod detect;
pub mod proc;
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
    let (tx, rx) = unbounded();
    let driver: Box<dyn AgentDriver> = match kind {
        AgentKind::ClaudeCode => Box::new(claude::ClaudeDriver::spawn(cfg, tx)?),
        AgentKind::Codex => Box::new(codex::CodexDriver::spawn(cfg, tx)?),
        AgentKind::Acp { command } => {
            let _ = acp::AcpDriver::new(command);
            return Err(DriverError::Unsupported(
                "ACP agents are not implemented yet (https://agentclientprotocol.com)".into(),
            ));
        }
    };
    Ok((driver, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_acp_is_unsupported() {
        let r = spawn(AgentKind::Acp { command: "gemini".into() }, &ThreadConfig::new("."));
        assert!(matches!(r, Err(DriverError::Unsupported(_))));
    }

    #[test]
    fn events_roundtrip_through_serde() {
        let e = AgentEvent::Usage {
            input: 1, output: 2, cache_create: 3, cache_read: 4, cost_usd: Some(0.5), context_window: None,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), e);
    }
}
