//! Core, UI-agnostic types shared by every agent driver.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which agent backend a thread talks to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    /// Any Agent Client Protocol agent (Gemini CLI, ...). `command` is the
    /// executable (and optional leading args) that speaks ACP over stdio.
    Acp { command: String },
}

/// UI-level permission pill. Each driver maps it to its CLI's native knobs.
///
/// | Mode        | Claude Code                                        | Codex (`approvalPolicy` / `sandbox`)     |
/// |-------------|----------------------------------------------------|------------------------------------------|
/// | Ask         | `--permission-mode default` + stdio prompts        | `untrusted` / `workspace-write`          |
/// | AcceptEdits | `--permission-mode acceptEdits` + stdio prompts    | `on-request` / `workspace-write`         |
/// | Full        | `--dangerously-skip-permissions`                   | `never` / `danger-full-access`           |
/// | ReadOnly    | `--permission-mode plan` + stdio prompts           | `on-request` / `read-only`               |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum PermissionMode {
    #[default]
    Ask,
    AcceptEdits,
    Full,
    ReadOnly,
}

/// Everything needed to start (or resume) one chat thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadConfig {
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    /// Claude: `--resume <session_id>`. Codex: `thread/resume` with this thread id.
    pub resume_session: Option<String>,
    /// Extra CLI args appended verbatim (Claude: after our flags; Codex: after `app-server`).
    pub extra_args: Vec<String>,
}

impl ThreadConfig {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            model: None,
            permission_mode: PermissionMode::default(),
            resume_session: None,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolKind {
    Read,
    Search,
    Edit,
    Write,
    Command,
    Web,
    Task,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskState {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// A turn is running.
    Working,
    /// The agent is blocked on the user (e.g. an approval).
    WaitingForInput,
    /// The turn finished; the agent is idle and ready for the next message.
    Done,
    Error(String),
}

/// One usage window (session / weekly / ...), as reported by the CLI itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    /// Stable name: `five_hour`, `seven_day` (Claude) or `primary` / `secondary` (Codex).
    pub name: String,
    /// 0.0 ..= 100.0
    pub used_percent: f64,
    pub window_mins: Option<u64>,
    /// Unix seconds.
    pub resets_at: Option<i64>,
}

/// Normalized event stream, identical for every backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        model: Option<String>,
    },
    /// `delta == true`: append to the current message. `false`: complete text of a message.
    Text { text: String, delta: bool },
    Thinking { text: String, delta: bool },
    ToolCall {
        id: String,
        kind: ToolKind,
        title: String,
        input_json: serde_json::Value,
    },
    ToolResult {
        id: String,
        ok: bool,
        summary: String,
    },
    Plan { tasks: Vec<(String, TaskState)> },
    /// Answer with [`AgentDriver::approve`] using `id`.
    ApprovalRequest {
        id: String,
        tool: String,
        detail: String,
    },
    /// Token usage **increment** (sum them per thread). `input` excludes cache reads.
    Usage {
        input: u64,
        output: u64,
        cache_create: u64,
        cache_read: u64,
        /// Claude only: `total_cost_usd` as reported by the CLI at the end of the turn.
        cost_usd: Option<f64>,
        context_window: Option<u64>,
    },
    /// Subscription/rate-limit windows pushed by the CLI (no estimation).
    RateLimits { windows: Vec<RateWindow> },
    Status(AgentStatus),
    /// The agent process exited (stream closed).
    Exited { code: Option<i32> },
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("executable not found: {0}")]
    NotFound(String),
    #[error("agent process is not running")]
    Closed,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DriverError>;

/// Control surface of one running agent thread. Events arrive on the receiver
/// returned by [`crate::spawn`]. All methods are non-blocking (they write to the
/// child's stdin); they may be called from any thread.
pub trait AgentDriver: Send {
    /// Send a user message (starts a turn; emits `Status(Working)`).
    fn send(&mut self, text: &str) -> Result<()>;
    /// Abort the current turn but keep the session alive (see each driver for how).
    fn interrupt(&mut self) -> Result<()>;
    /// Answer an [`AgentEvent::ApprovalRequest`].
    fn approve(&mut self, id: &str, allow: bool) -> Result<()>;
    /// Stop the process (close stdin, then kill the process tree). Idempotent.
    fn shutdown(&mut self);
}
