//! Approvals of a delegated agent. The child never approves itself and the parent cannot approve
//! for it: every request goes to a Selachi window (the attention queue) or is denied.
//!
//! # IPC protocol (version 1)
//!
//! Transport: the named pipe `\\.\pipe\selachi-approvals-<user>` (`<user>` is `%USERNAME%` reduced to
//! `[A-Za-z0-9_-]`), a byte-mode duplex pipe served by the Selachi GUI (the server side is not
//! written yet; tests use a mock). **One connection = one request**: the client opens the pipe, writes
//! one JSON object followed by `\n`, reads one JSON line back and closes. Messages are UTF-8 JSON
//! with a `"type"` tag and a `"v"` protocol version.
//!
//! ```text
//! client -> {"type":"ping","v":1}
//! server -> {"type":"pong","v":1,"accepts_approvals":true}
//!
//! client -> {"type":"approval_request","v":1,"request_id":"req_2","run_id":"12","thread_id":12,
//!            "parent_thread_id":7,"agent":"codex","tool":"Bash","detail":"rm -rf build",
//!            "permission":"ask","depth":1,"timeout_secs":120}
//! server -> {"type":"approval_response","v":1,"request_id":"req_2","decision":"allow","reason":null}
//!           decision: "allow" | "deny" | "always" | "always_run"
//!             always     = also for the rest of this run (the agent's own "always allow")
//!             always_run = allow, and answer the rest of this run's requests without asking
//!                          (approval mode `auto`; the hard blocklist still denies). Additive in
//!                          protocol 1: a server that never sends it changes nothing.
//! server -> {"type":"error","v":1,"reason":"..."}   (any request; treated as a denial)
//! ```
//!
//! Confirmation of `apply_changes` (MCP) travels as an ordinary `approval_request` whose `tool` is
//! `selachi:apply_changes` and whose `detail` is the apply plan (files, diff stat, warnings); `request_id`
//! starts with `apply-`. The window shows it as "apply these changes to the project?" and answers `allow` or
//! `deny` (`always` counts as a single `allow`). Full-risk plans are never sent: they need a terminal.
//!
//! The server holds the connection open while the user decides; the client waits up to
//! `timeout_secs` and then denies. `tool` and `detail` are redacted by the harness before they are
//! sent. No Selachi window (pipe missing) or no answer means **deny**, with the reason recorded in the
//! run's `notes`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Protocol version spoken by this build.
pub const PROTOCOL_VERSION: u32 = 1;

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;

/// Pipe path for `user` (`\\.\pipe\selachi-approvals-<user>`).
pub fn pipe_path_for(user: &str) -> String {
    let clean: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!(
        r"\\.\pipe\selachi-approvals-{}",
        if clean.is_empty() { "user" } else { &clean }
    )
}

/// Pipe path for the current user (`%USERNAME%`).
pub fn default_pipe_path() -> String {
    pipe_path_for(&std::env::var("USERNAME").unwrap_or_default())
}

/// What the user decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    /// Allow now and for the rest of this run (the agent's own "always allow this tool").
    Always,
    /// Allow now and switch the rest of this run to the `auto` approval mode: no more questions
    /// (the hard blocklist still denies). Sent by "No volver a preguntar en esta ejecución".
    AlwaysRun,
}

/// A request to approve one action of a delegated agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub v: u32,
    /// Id the agent gave the request (echoed in the response).
    pub request_id: String,
    pub run_id: String,
    /// The child thread (what the attention queue should open).
    pub thread_id: i64,
    /// The thread that delegated, when it is a chat thread.
    pub parent_thread_id: Option<i64>,
    pub agent: String,
    pub tool: String,
    pub detail: String,
    pub permission: String,
    pub depth: u32,
    /// Seconds the harness will wait before denying.
    pub timeout_secs: u64,
}

/// Messages the client sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Ping { v: u32 },
    ApprovalRequest(ApprovalRequest),
}

/// Messages the server sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Pong {
        v: u32,
        accepts_approvals: bool,
    },
    ApprovalResponse {
        v: u32,
        request_id: String,
        decision: Decision,
        reason: Option<String>,
    },
    Error {
        v: u32,
        reason: String,
    },
}

/// The answer the harness acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    /// Why, when it is a denial that did not come from the user.
    pub reason: Option<String>,
    /// A Selachi window answered (the user decided), as opposed to the harness refusing.
    pub forwarded: bool,
}

impl Verdict {
    pub fn deny(reason: impl Into<String>) -> Verdict {
        Verdict {
            decision: Decision::Deny,
            reason: Some(reason.into()),
            forwarded: false,
        }
    }
}

/// Decides approvals of a delegated agent.
pub trait ApprovalBroker: Send + Sync {
    fn decide(&self, req: &ApprovalRequest) -> Verdict;
}

/// No Selachi window: everything is refused with a clear reason.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAll;

const NO_GUI: &str = "no Selachi window is running to review this approval (delegated agents never approve themselves); \
                      open Selachi, or run the delegation with --permission read-only";

impl ApprovalBroker for DenyAll {
    fn decide(&self, _req: &ApprovalRequest) -> Verdict {
        Verdict::deny(NO_GUI)
    }
}

/// Forwards approvals to the Selachi GUI over the named pipe; denies when it is not there.
#[derive(Debug, Clone)]
pub struct PipeBroker {
    pub path: String,
    /// How long to retry while every pipe instance is busy.
    pub connect_timeout: Duration,
}

impl PipeBroker {
    pub fn new(path: impl Into<String>) -> PipeBroker {
        PipeBroker {
            path: path.into(),
            connect_timeout: Duration::from_secs(2),
        }
    }

    /// The broker for the current user's pipe.
    pub fn for_current_user() -> PipeBroker {
        PipeBroker::new(default_pipe_path())
    }

    fn connect(&self) -> std::io::Result<File> {
        let end = Instant::now() + self.connect_timeout;
        loop {
            match OpenOptions::new().read(true).write(true).open(&self.path) {
                Ok(f) => return Ok(f),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One request, one reply line. `Err` carries the reason it failed.
    fn round_trip(&self, msg: &ClientMessage, wait: Duration) -> Result<ServerMessage, String> {
        let mut file = self.connect().map_err(|e| match e.raw_os_error() {
            Some(ERROR_FILE_NOT_FOUND) => NO_GUI.to_string(),
            _ => format!("cannot reach the Selachi approvals pipe {}: {e}", self.path),
        })?;
        let mut line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|e| format!("writing to Selachi failed: {e}"))?;
        // Synchronous pipe handles cannot be read and cancelled: read on a helper thread and give up
        // on the timeout (the thread ends when the server closes or the process exits).
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("approvals-reader".into())
            .spawn(move || {
                let mut reply = String::new();
                let r = BufReader::new(file).read_line(&mut reply).map(|_| reply);
                let _ = tx.send(r);
            })
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(wait) {
            Ok(Ok(reply)) if !reply.trim().is_empty() => serde_json::from_str(reply.trim())
                .map_err(|e| format!("Selachi sent an unreadable reply: {e}")),
            Ok(Ok(_)) => Err("Selachi closed the connection without answering".into()),
            // the server dropped the pipe instance before replying
            Ok(Err(e))
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED)
                ) =>
            {
                Err("Selachi closed the connection without answering".into())
            }
            Ok(Err(e)) => Err(format!("reading from Selachi failed: {e}")),
            Err(_) => Err(format!(
                "the user did not answer within {} s",
                wait.as_secs()
            )),
        }
    }

    /// Is a Selachi window serving approvals right now?
    pub fn ping(&self) -> bool {
        matches!(
            self.round_trip(
                &ClientMessage::Ping {
                    v: PROTOCOL_VERSION
                },
                Duration::from_secs(2)
            ),
            Ok(ServerMessage::Pong {
                accepts_approvals: true,
                ..
            })
        )
    }
}

impl ApprovalBroker for PipeBroker {
    fn decide(&self, req: &ApprovalRequest) -> Verdict {
        let wait = Duration::from_secs(req.timeout_secs.max(1));
        match self.round_trip(&ClientMessage::ApprovalRequest(req.clone()), wait) {
            Ok(ServerMessage::ApprovalResponse {
                request_id,
                decision,
                reason,
                ..
            }) => {
                if request_id != req.request_id {
                    return Verdict::deny(format!(
                        "Selachi answered a different request ({request_id}, expected {}); denied",
                        req.request_id
                    ));
                }
                Verdict {
                    decision,
                    reason: reason.filter(|_| decision == Decision::Deny),
                    forwarded: true,
                }
            }
            Ok(ServerMessage::Error { reason, .. }) => {
                Verdict::deny(format!("Selachi refused the request: {reason}"))
            }
            Ok(ServerMessage::Pong { .. }) => {
                Verdict::deny("Selachi answered with a pong instead of a decision; denied")
            }
            Err(reason) => Verdict::deny(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_per_user_and_sanitized() {
        assert_eq!(pipe_path_for("venta"), r"\\.\pipe\selachi-approvals-venta");
        assert_eq!(
            pipe_path_for(r"DOM\us er!"),
            r"\\.\pipe\selachi-approvals-DOMuser"
        );
        assert_eq!(pipe_path_for(""), r"\\.\pipe\selachi-approvals-user");
    }

    #[test]
    fn wire_format_matches_the_documentation() {
        let req = ApprovalRequest {
            v: PROTOCOL_VERSION,
            request_id: "req_2".into(),
            run_id: "12".into(),
            thread_id: 12,
            parent_thread_id: Some(7),
            agent: "codex".into(),
            tool: "Bash".into(),
            detail: "rm -rf build".into(),
            permission: "ask".into(),
            depth: 1,
            timeout_secs: 120,
        };
        let json = serde_json::to_value(ClientMessage::ApprovalRequest(req.clone())).unwrap();
        assert_eq!(json["type"], "approval_request");
        assert_eq!(json["request_id"], "req_2");
        assert_eq!(json["parent_thread_id"], 7);
        let back: ClientMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, ClientMessage::ApprovalRequest(req));

        assert_eq!(
            serde_json::to_string(&ClientMessage::Ping { v: 1 }).unwrap(),
            r#"{"type":"ping","v":1}"#
        );
        let r: ServerMessage =
            serde_json::from_str(r#"{"type":"approval_response","v":1,"request_id":"a","decision":"always","reason":null}"#).unwrap();
        assert_eq!(
            r,
            ServerMessage::ApprovalResponse {
                v: 1,
                request_id: "a".into(),
                decision: Decision::Always,
                reason: None
            }
        );
    }

    #[test]
    fn without_a_gui_everything_is_denied_with_a_reason() {
        let req = ApprovalRequest {
            v: 1,
            request_id: "r".into(),
            run_id: "1".into(),
            thread_id: 1,
            parent_thread_id: None,
            agent: "codex".into(),
            tool: "Bash".into(),
            detail: "x".into(),
            permission: "ask".into(),
            depth: 1,
            timeout_secs: 1,
        };
        let v = DenyAll.decide(&req);
        assert_eq!(v.decision, Decision::Deny);
        assert!(!v.forwarded && v.reason.unwrap().contains("Selachi"));
        // a pipe nobody serves behaves the same
        let b = PipeBroker::new(r"\\.\pipe\selachi-approvals-test-nobody-listens");
        let v = b.decide(&req);
        assert_eq!(v.decision, Decision::Deny);
        assert!(v.reason.unwrap().contains("no Selachi window"));
        assert!(!b.ping());
    }
}
