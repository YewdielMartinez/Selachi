//! Agent Client Protocol (ACP) adapter: **stub**.
//!
//! ACP is the open JSON-RPC-over-stdio standard (by Zed) that lets one client talk
//! to any compatible agent (Gemini CLI, ...). Spec and schema:
//! <https://agentclientprotocol.com> (repo: <https://github.com/zed-industries/agent-client-protocol>).
//!
//! Not implemented yet: every call returns [`DriverError::Unsupported`]. The shape
//! of the future implementation is the same as [`crate::codex`]: a pure state
//! machine mapping `session/update` notifications and `session/request_permission`
//! requests to [`crate::AgentEvent`], driven by a thin stdio reader thread.

use crate::types::*;

pub struct AcpDriver {
    #[expect(dead_code, reason = "reserved for the future ACP implementation (stub driver)")]
    command: String,
}

impl AcpDriver {
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into() }
    }
}

fn unsupported() -> DriverError {
    DriverError::Unsupported("ACP agents are not implemented yet (https://agentclientprotocol.com)".into())
}

impl AgentDriver for AcpDriver {
    fn send(&mut self, _text: &str) -> Result<()> {
        Err(unsupported())
    }
    fn interrupt(&mut self) -> Result<()> {
        Err(unsupported())
    }
    fn approve(&mut self, _id: &str, _allow: bool) -> Result<()> {
        Err(unsupported())
    }
    fn shutdown(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_is_unsupported() {
        let mut d = AcpDriver::new("gemini");
        assert!(matches!(d.send("hi"), Err(DriverError::Unsupported(_))));
        assert!(matches!(d.interrupt(), Err(DriverError::Unsupported(_))));
        assert!(matches!(d.approve("x", true), Err(DriverError::Unsupported(_))));
        d.shutdown();
    }
}
