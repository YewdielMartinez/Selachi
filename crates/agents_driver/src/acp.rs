//! Agent Client Protocol (ACP) client: JSON-RPC 2.0 over the agent's stdio, one JSON
//! object per line. Works with OpenCode (`opencode acp`, see [`crate::opencode`]), Gemini CLI
//! and any other ACP agent. Spec: <https://agentclientprotocol.com>.
//!
//! # What is implemented (protocol version 1)
//!
//! * `initialize` with `clientCapabilities{fs{readTextFile,writeTextFile}, terminal:false}`.
//! * `session/new {cwd, mcpServers:[]}`; with [`ThreadConfig::resume_session`]: `session/resume`
//!   when the agent advertises `sessionCapabilities.resume`, else `session/load` (the replayed
//!   history is swallowed), else a fresh session.
//! * Model / mode selection right after the session exists, one request at a time:
//!   `session/set_config_option` (`model`, `mode`) when the agent lists such config options,
//!   else `session/set_model` / `session/set_mode`. `ReadOnly` selects the agent mode `plan`.
//! * `session/prompt` (text content block), one prompt in flight; further `send`s queue and are
//!   dispatched when the previous prompt returns.
//! * `session/update` -> [`AgentEvent`]: `agent_message_chunk` -> `Text` delta,
//!   `agent_thought_chunk` -> `Thinking` delta, `tool_call` / `tool_call_update` -> `ToolCall` /
//!   `ToolResult` (kind mapping in [`tool_kind`]), `plan` -> `Plan`, `usage_update` (context size
//!   and cumulative cost) folded into the turn's `Usage`. `user_message_chunk`,
//!   `available_commands_update`, `current_mode_update`, `config_option_update` are ignored.
//! * `session/prompt` response: `stopReason` -> `Status(Done)` (`refusal` -> `Error`), the optional
//!   `usage{inputTokens,outputTokens,thoughtTokens,cachedReadTokens,cachedWriteTokens}` -> `Usage`
//!   (thinking tokens are added to `output`; `cost_usd` is the increment of the agent's cumulative
//!   `usage_update.cost` when it is in USD).
//! * `session/request_permission` -> `ApprovalRequest` (or auto-answered, see
//!   [`PermissionMode`]); [`AgentDriver::approve`] picks the `allow_once` / `reject_once` option,
//!   [`AcpDriver::approve_always`] the `allow_always` one. Cancelling the turn answers every open
//!   request with `outcome: cancelled`, as the spec requires.
//! * `session/cancel` for [`AgentDriver::interrupt`].
//! * Client methods: `fs/read_text_file` (absolute path **inside the thread's cwd only**, optional
//!   `line` / `limit`, 8 MiB cap) and `fs/write_text_file` (inside cwd; surfaces an
//!   `ApprovalRequest` unless the mode is `Full`, or the same path was just approved through a
//!   `session/request_permission`, which is what OpenCode does for its edit tool). Everything else
//!   (`terminal/*`, ...) is answered with `-32601`.
//!
//! Not implemented: terminals, `authenticate` (agents that require it surface an `Error` status
//! naming the auth methods), MCP servers (an empty list is sent), non-text prompt content.
//!
//! The protocol logic lives in [`AcpSession`], a pure state machine (messages in, events and
//! outgoing messages out, plus the file reads/writes above); [`AcpDriver`] only moves lines
//! between the child process and the session, like [`crate::codex::CodexDriver`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::coalesce::{self, Coalescer};
use crate::opencode;
use crate::proc::{self, truncate};
use crate::types::*;

/// ACP protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// Largest file `fs/read_text_file` will return.
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;
const ERR_INTERNAL: i64 = -32603;
const ERR_AUTH_REQUIRED: i64 = -32000;
const ERR_NOT_FOUND: i64 = -32002;

/// What a state-machine call produced.
#[derive(Debug, Default)]
pub struct Step {
    pub events: Vec<AgentEvent>,
    /// JSON-RPC messages to write to the agent's stdin.
    pub outgoing: Vec<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Req {
    Initialize,
    OpenSession,
    Setup,
    Prompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Auto {
    Allow,
    Reject,
    Ask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PermOption {
    id: String,
    kind: String,
}

struct PendingPermission {
    rpc_id: Value,
    options: Vec<PermOption>,
    paths: Vec<String>,
}

struct PendingWrite {
    rpc_id: Value,
    path: PathBuf,
    content: String,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// ACP tool kind (`read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`,
/// `switch_mode`, `other`) -> [`ToolKind`].
pub fn tool_kind(kind: &str) -> ToolKind {
    match kind {
        "read" => ToolKind::Read,
        "search" => ToolKind::Search,
        "edit" | "delete" | "move" => ToolKind::Edit,
        "execute" => ToolKind::Command,
        "fetch" => ToolKind::Web,
        other => ToolKind::Other(other.to_string()),
    }
}

/// Split a command line into program and arguments. Double quotes group (and are removed);
/// there is no escaping, which is enough for Windows paths.
pub fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for ch in cmd.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

fn path_key(p: &str) -> String {
    let k = p.replace('\\', "/");
    if cfg!(windows) {
        k.to_lowercase()
    } else {
        k
    }
}

/// Drop `.` and resolve `..` lexically.
fn lexical_clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Pure ACP client state machine.
pub struct AcpSession {
    cwd: PathBuf,
    cwd_canon: PathBuf,
    model: Option<String>,
    mode: PermissionMode,
    resume: Option<String>,
    next_id: i64,
    reqs: HashMap<i64, Req>,
    // agent capabilities
    can_load: bool,
    can_resume: bool,
    auth_methods: Vec<String>,
    // session
    session_id: Option<String>,
    replaying: bool,
    ready: bool,
    busy: bool,
    current_model: Option<String>,
    has_model_option: bool,
    has_models: bool,
    has_mode_option: bool,
    modes: Vec<String>,
    current_mode: Option<String>,
    setup: VecDeque<(&'static str, Value)>,
    queue: VecDeque<String>,
    tools: HashMap<String, bool>,
    permissions: HashMap<String, PendingPermission>,
    writes: HashMap<String, PendingWrite>,
    approved_paths: HashSet<String>,
    // usage bookkeeping
    cost_total: f64,
    cost_reported: f64,
    cost_seen: bool,
    context_size: Option<u64>,
}

impl AcpSession {
    pub fn new(cfg: &ThreadConfig) -> Self {
        let cwd_canon = std::fs::canonicalize(&cfg.cwd).unwrap_or_else(|_| cfg.cwd.clone());
        Self {
            cwd: cfg.cwd.clone(),
            cwd_canon,
            model: cfg.model.clone(),
            mode: cfg.permission_mode,
            resume: cfg.resume_session.clone(),
            next_id: 0,
            reqs: HashMap::new(),
            can_load: false,
            can_resume: false,
            auth_methods: Vec::new(),
            session_id: None,
            replaying: false,
            ready: false,
            busy: false,
            current_model: None,
            has_model_option: false,
            has_models: false,
            has_mode_option: false,
            modes: Vec::new(),
            current_mode: None,
            setup: VecDeque::new(),
            queue: VecDeque::new(),
            tools: HashMap::new(),
            permissions: HashMap::new(),
            writes: HashMap::new(),
            approved_paths: HashSet::new(),
            cost_total: 0.0,
            cost_reported: 0.0,
            cost_seen: false,
            context_size: None,
        }
    }

    /// The ACP session id, once the session exists.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn request(&mut self, kind: Req, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.reqs.insert(id, kind);
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn notification(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "method": method, "params": params })
    }

    fn reply(id: &Value, result: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    }

    fn reply_err(id: &Value, code: i64, message: &str) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
    }

    /// First message(s) to send right after spawning the agent.
    pub fn start(&mut self) -> Vec<Value> {
        vec![self.request(
            Req::Initialize,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "selachi", "title": "Selachi", "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )]
    }

    // -- user-facing calls ---------------------------------------------------

    /// User message: dispatched now, or queued until the session is ready / the previous
    /// prompt has returned.
    pub fn user_message(&mut self, text: &str) -> Step {
        let mut step = Step::default();
        step.events.push(AgentEvent::Status(AgentStatus::Working));
        if self.ready && !self.busy {
            self.dispatch(text, &mut step);
        } else {
            self.queue.push_back(text.to_string());
        }
        step
    }

    fn dispatch(&mut self, text: &str, step: &mut Step) {
        let Some(sid) = self.session_id.clone() else {
            self.queue.push_back(text.to_string());
            return;
        };
        self.busy = true;
        let m = self.request(
            Req::Prompt,
            "session/prompt",
            json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": text }] }),
        );
        step.outgoing.push(m);
    }

    /// Abort the running turn (`session/cancel`) and answer every open request as cancelled.
    pub fn interrupt(&mut self) -> Result<Step> {
        let mut step = Step::default();
        self.queue.clear();
        for (_, p) in self.permissions.drain() {
            step.outgoing
                .push(Self::reply(&p.rpc_id, json!({ "outcome": { "outcome": "cancelled" } })));
        }
        for (_, w) in self.writes.drain() {
            step.outgoing
                .push(Self::reply_err(&w.rpc_id, ERR_INTERNAL, "cancelled"));
        }
        if self.busy {
            if let Some(sid) = &self.session_id {
                step.outgoing
                    .push(Self::notification("session/cancel", json!({ "sessionId": sid })));
            }
        }
        Ok(step)
    }

    /// Answer an [`AgentEvent::ApprovalRequest`] (once-only grants).
    pub fn approve(&mut self, id: &str, allow: bool) -> Result<Step> {
        self.approve_with(id, allow, false)
    }

    /// Like [`approve`](Self::approve); `always` picks the agent's `allow_always` option.
    pub fn approve_with(&mut self, id: &str, allow: bool, always: bool) -> Result<Step> {
        let mut step = Step::default();
        if let Some(p) = self.permissions.remove(id) {
            let outcome = match pick_option(&p.options, allow, always) {
                Some(opt) => json!({ "outcome": "selected", "optionId": opt }),
                None => json!({ "outcome": "cancelled" }),
            };
            if allow {
                for path in &p.paths {
                    self.approved_paths.insert(path_key(path));
                }
            }
            step.outgoing.push(Self::reply(&p.rpc_id, json!({ "outcome": outcome })));
            step.events.push(AgentEvent::Status(AgentStatus::Working));
            return Ok(step);
        }
        if let Some(w) = self.writes.remove(id) {
            if allow {
                match std::fs::write(&w.path, w.content.as_bytes()) {
                    Ok(()) => step.outgoing.push(Self::reply(&w.rpc_id, Value::Null)),
                    Err(e) => step
                        .outgoing
                        .push(Self::reply_err(&w.rpc_id, ERR_INTERNAL, &format!("write failed: {e}"))),
                }
            } else {
                step.outgoing
                    .push(Self::reply_err(&w.rpc_id, ERR_AUTH_REQUIRED, "write denied by the user"));
            }
            step.events.push(AgentEvent::Status(AgentStatus::Working));
            return Ok(step);
        }
        Err(DriverError::Protocol(format!("unknown approval id {id}")))
    }

    // -- incoming ------------------------------------------------------------

    /// Feed one message received from the agent.
    pub fn on_message(&mut self, v: &Value) -> Step {
        let mut step = Step::default();
        match (v.get("method").and_then(Value::as_str), v.get("id")) {
            (Some(m), Some(id)) => self.on_agent_request(m, id, v.get("params").unwrap_or(&Value::Null), &mut step),
            (Some(m), None) => self.on_notification(m, v.get("params").unwrap_or(&Value::Null), &mut step),
            (None, Some(id)) => self.on_response(id, v, &mut step),
            _ => {}
        }
        step
    }

    /// Parse a raw stdout line (tolerant: bad lines are logged and ignored).
    pub fn on_line(&mut self, line: &str) -> Step {
        match serde_json::from_str::<Value>(line.trim()) {
            Ok(v) => self.on_message(&v),
            Err(e) => {
                if !line.trim().is_empty() {
                    log::debug!("acp: non-JSON stdout line ({e}): {}", truncate(line, 120));
                }
                Step::default()
            }
        }
    }

    // -- responses -----------------------------------------------------------

    fn on_response(&mut self, id: &Value, v: &Value, step: &mut Step) {
        let Some(kind) = id.as_i64().and_then(|i| self.reqs.remove(&i)) else {
            return;
        };
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let msg = s(err, "message").unwrap_or("request failed").to_string();
            match kind {
                Req::Setup => {
                    log::warn!("acp: session setup request failed: {msg}");
                    self.next_setup(step);
                }
                Req::Prompt => {
                    self.busy = false;
                    self.queue.clear();
                    step.events.push(AgentEvent::Status(AgentStatus::Error(truncate(&msg, 300))));
                }
                Req::OpenSession | Req::Initialize => {
                    self.replaying = false;
                    let text = if code == ERR_AUTH_REQUIRED || msg.to_lowercase().contains("auth") {
                        format!(
                            "authentication required ({}): sign in with the agent's own CLI first",
                            if self.auth_methods.is_empty() {
                                "no auth method advertised".to_string()
                            } else {
                                self.auth_methods.join(", ")
                            }
                        )
                    } else {
                        truncate(&msg, 300)
                    };
                    step.events.push(AgentEvent::Status(AgentStatus::Error(text)));
                }
            }
            return;
        }
        let result = v.get("result").unwrap_or(&Value::Null);
        match kind {
            Req::Initialize => self.on_initialized(result, step),
            Req::OpenSession => self.on_session_opened(result, step),
            Req::Setup => self.next_setup(step),
            Req::Prompt => self.on_prompt_done(result, step),
        }
    }

    fn on_initialized(&mut self, result: &Value, step: &mut Step) {
        let caps = result.get("agentCapabilities").unwrap_or(&Value::Null);
        self.can_load = caps.get("loadSession").and_then(Value::as_bool).unwrap_or(false);
        self.can_resume = caps.pointer("/sessionCapabilities/resume").is_some_and(|r| !r.is_null());
        self.auth_methods = result
            .get("authMethods")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|m| s(m, "id").map(str::to_string)).collect())
            .unwrap_or_default();
        let cwd = self.cwd.to_string_lossy().into_owned();
        let base = json!({ "cwd": cwd, "mcpServers": [] });
        let (method, params) = match self.resume.clone() {
            Some(sid) if self.can_resume => {
                let mut p = base;
                p["sessionId"] = json!(sid);
                ("session/resume", p)
            }
            Some(sid) if self.can_load => {
                self.replaying = true;
                let mut p = base;
                p["sessionId"] = json!(sid);
                ("session/load", p)
            }
            other => {
                if other.is_some() {
                    log::warn!("acp: agent can neither resume nor load sessions; starting a new one");
                }
                ("session/new", base)
            }
        };
        let m = self.request(Req::OpenSession, method, params);
        step.outgoing.push(m);
    }

    fn on_session_opened(&mut self, result: &Value, step: &mut Step) {
        self.replaying = false;
        let Some(sid) = s(result, "sessionId")
            .map(str::to_string)
            .or_else(|| self.resume.clone())
        else {
            step.events.push(AgentEvent::Status(AgentStatus::Error(
                "agent returned no sessionId".into(),
            )));
            return;
        };
        self.session_id = Some(sid.clone());

        // Models: `models` (unstable ACP) and/or the `model` config option.
        if let Some(models) = result.get("models") {
            self.has_models = true;
            self.current_model = s(models, "currentModelId").map(str::to_string);
        }
        if let Some(opts) = result.get("configOptions").and_then(Value::as_array) {
            for o in opts {
                match s(o, "id") {
                    Some("model") => {
                        self.has_model_option = true;
                        if let Some(c) = s(o, "currentValue") {
                            self.current_model = Some(c.to_string());
                        }
                    }
                    Some("mode") => self.has_mode_option = true,
                    _ => {}
                }
            }
        }
        if let Some(modes) = result.get("modes") {
            self.modes = modes
                .get("availableModes")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|m| s(m, "id").map(str::to_string)).collect())
                .unwrap_or_default();
            self.current_mode = s(modes, "currentModeId").map(str::to_string);
        }
        step.events.push(AgentEvent::SessionStarted {
            session_id: sid.clone(),
            model: self.current_model.clone(),
        });

        if let Some(want) = self.model.clone().filter(|m| Some(m) != self.current_model.as_ref()) {
            if self.has_model_option {
                self.setup.push_back((
                    "session/set_config_option",
                    json!({ "sessionId": sid, "configId": "model", "value": want }),
                ));
            } else if self.has_models {
                self.setup
                    .push_back(("session/set_model", json!({ "sessionId": sid, "modelId": want })));
            } else {
                log::warn!("acp: agent offers no model selection; ignoring model {want}");
            }
        }
        if self.mode == PermissionMode::ReadOnly
            && self.modes.iter().any(|m| m == "plan")
            && self.current_mode.as_deref() != Some("plan")
        {
            if self.has_mode_option {
                self.setup.push_back((
                    "session/set_config_option",
                    json!({ "sessionId": sid, "configId": "mode", "value": "plan" }),
                ));
            } else {
                self.setup
                    .push_back(("session/set_mode", json!({ "sessionId": sid, "modeId": "plan" })));
            }
        }
        self.next_setup(step);
    }

    /// Send the next setup request, or finish setup and flush queued messages.
    fn next_setup(&mut self, step: &mut Step) {
        if let Some((method, params)) = self.setup.pop_front() {
            let m = self.request(Req::Setup, method, params);
            step.outgoing.push(m);
            return;
        }
        if !self.ready {
            self.ready = true;
            if let Some(next) = self.queue.pop_front() {
                self.dispatch(&next, step);
            }
        }
    }

    fn on_prompt_done(&mut self, result: &Value, step: &mut Step) {
        self.busy = false;
        let usage = result.get("usage").filter(|u| u.is_object());
        let cost_delta = if self.cost_seen {
            let d = (self.cost_total - self.cost_reported).max(0.0);
            self.cost_reported = self.cost_total;
            (d > 0.0).then_some(d)
        } else {
            None
        };
        if usage.is_some() || cost_delta.is_some() {
            let n = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
            step.events.push(AgentEvent::Usage {
                input: n("inputTokens"),
                output: n("outputTokens") + n("thoughtTokens"),
                cache_create: n("cachedWriteTokens"),
                cache_read: n("cachedReadTokens"),
                cost_usd: cost_delta,
                context_window: self.context_size,
            });
        }
        match s(result, "stopReason") {
            Some("refusal") => step
                .events
                .push(AgentEvent::Status(AgentStatus::Error("the agent refused to continue".into()))),
            _ => {
                if let Some(next) = self.queue.pop_front() {
                    self.dispatch(&next, step);
                } else {
                    step.events.push(AgentEvent::Status(AgentStatus::Done));
                }
            }
        }
    }

    // -- notifications -------------------------------------------------------

    fn on_notification(&mut self, method: &str, params: &Value, step: &mut Step) {
        if method != "session/update" || self.replaying {
            return;
        }
        let Some(u) = params.get("update") else { return };
        match s(u, "sessionUpdate") {
            Some("agent_message_chunk") => {
                if let Some(t) = chunk_text(u) {
                    step.events.push(AgentEvent::Text { text: t, delta: true });
                }
            }
            Some("agent_thought_chunk") => {
                if let Some(t) = chunk_text(u) {
                    step.events.push(AgentEvent::Thinking { text: t, delta: true });
                }
            }
            Some("tool_call") => self.on_tool_call(u, step),
            Some("tool_call_update") => self.on_tool_update(u, step),
            Some("plan") => {
                let tasks = u
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|e| {
                                let state = match s(e, "status") {
                                    Some("completed") => TaskState::Done,
                                    Some("in_progress") => TaskState::InProgress,
                                    _ => TaskState::Pending,
                                };
                                (s(e, "content").unwrap_or("").to_string(), state)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                step.events.push(AgentEvent::Plan { tasks });
            }
            Some("usage_update") => {
                if let Some(size) = u.get("size").and_then(Value::as_u64) {
                    self.context_size = Some(size);
                }
                let amount = u.pointer("/cost/amount").and_then(Value::as_f64);
                let usd = u.pointer("/cost/currency").and_then(Value::as_str).is_none_or(|c| c == "USD");
                if let (Some(a), true) = (amount, usd) {
                    if !self.cost_seen && self.resume.is_some() {
                        // Resumed session: the cumulative total includes earlier turns.
                        self.cost_reported = a;
                    }
                    self.cost_seen = true;
                    self.cost_total = a;
                }
            }
            other => log::trace!("acp: ignored session update {other:?}"),
        }
    }

    fn on_tool_call(&mut self, u: &Value, step: &mut Step) {
        let Some(id) = s(u, "toolCallId") else { return };
        if self.tools.contains_key(id) {
            return self.on_tool_update(u, step);
        }
        self.tools.insert(id.to_string(), false);
        step.events.push(AgentEvent::ToolCall {
            id: id.to_string(),
            kind: tool_kind(s(u, "kind").unwrap_or("other")),
            title: s(u, "title").unwrap_or(id).to_string(),
            input_json: u.get("rawInput").cloned().unwrap_or(Value::Null),
        });
        self.finish_tool_if_done(id, u, step);
    }

    fn on_tool_update(&mut self, u: &Value, step: &mut Step) {
        let Some(id) = s(u, "toolCallId") else { return };
        if !self.tools.contains_key(id) {
            // Update for a call we never saw announced: synthesize the announcement.
            self.tools.insert(id.to_string(), false);
            step.events.push(AgentEvent::ToolCall {
                id: id.to_string(),
                kind: tool_kind(s(u, "kind").unwrap_or("other")),
                title: s(u, "title").unwrap_or(id).to_string(),
                input_json: u.get("rawInput").cloned().unwrap_or(Value::Null),
            });
        }
        self.finish_tool_if_done(id, u, step);
    }

    fn finish_tool_if_done(&mut self, id: &str, u: &Value, step: &mut Step) {
        let ok = match s(u, "status") {
            Some("completed") => true,
            Some("failed") => false,
            _ => return,
        };
        if self.tools.get(id).copied().unwrap_or(false) {
            return;
        }
        self.tools.insert(id.to_string(), true);
        step.events.push(AgentEvent::ToolResult {
            id: id.to_string(),
            ok,
            summary: tool_summary(u, ok),
        });
    }

    // -- requests from the agent ---------------------------------------------

    fn on_agent_request(&mut self, method: &str, id: &Value, params: &Value, step: &mut Step) {
        match method {
            "session/request_permission" => self.on_permission(id, params, step),
            "fs/read_text_file" => {
                let m = match self.read_text_file(params) {
                    Ok(content) => Self::reply(id, json!({ "content": content })),
                    Err((code, msg)) => Self::reply_err(id, code, &msg),
                };
                step.outgoing.push(m);
            }
            "fs/write_text_file" => self.on_write(id, params, step),
            other => step.outgoing.push(Self::reply_err(
                id,
                ERR_METHOD_NOT_FOUND,
                &format!("method not supported by this client: {other}"),
            )),
        }
    }

    fn auto_decision(&self, kind: &str) -> Auto {
        let editish = matches!(kind, "edit" | "delete" | "move");
        match self.mode {
            PermissionMode::Full => Auto::Allow,
            PermissionMode::AcceptEdits if editish => Auto::Allow,
            PermissionMode::ReadOnly if editish || kind == "execute" => Auto::Reject,
            _ => Auto::Ask,
        }
    }

    fn on_permission(&mut self, id: &Value, params: &Value, step: &mut Step) {
        let tc = params.get("toolCall").unwrap_or(&Value::Null);
        let kind = s(tc, "kind").unwrap_or("other");
        let title = s(tc, "title").unwrap_or("tool").to_string();
        let options: Vec<PermOption> = params
            .get("options")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some(PermOption {
                            id: s(o, "optionId")?.to_string(),
                            kind: s(o, "kind").unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let paths = self.permission_paths(tc);
        match self.auto_decision(kind) {
            Auto::Ask => {
                let key = format!("acp-perm-{}", id_key(id));
                step.events.push(AgentEvent::ApprovalRequest {
                    id: key.clone(),
                    tool: title,
                    detail: permission_detail(tc),
                });
                step.events.push(AgentEvent::Status(AgentStatus::WaitingForInput));
                self.permissions.insert(
                    key,
                    PendingPermission { rpc_id: id.clone(), options, paths },
                );
            }
            decision => {
                let allow = decision == Auto::Allow;
                let outcome = match pick_option(&options, allow, false) {
                    Some(opt) => json!({ "outcome": "selected", "optionId": opt }),
                    None => json!({ "outcome": "cancelled" }),
                };
                if allow {
                    for p in &paths {
                        self.approved_paths.insert(path_key(p));
                    }
                }
                step.outgoing.push(Self::reply(id, json!({ "outcome": outcome })));
            }
        }
    }

    /// Absolute-ish paths mentioned by a permission's tool call (for the fs write pre-approval).
    fn permission_paths(&self, tc: &Value) -> Vec<String> {
        let mut raw: Vec<String> = Vec::new();
        if let Some(locs) = tc.get("locations").and_then(Value::as_array) {
            raw.extend(locs.iter().filter_map(|l| s(l, "path").map(str::to_string)));
        }
        if let Some(inp) = tc.get("rawInput") {
            for k in ["filepath", "filePath", "file_path", "path"] {
                if let Some(p) = s(inp, k) {
                    raw.push(p.to_string());
                }
            }
        }
        raw.into_iter()
            .map(|p| {
                let pb = PathBuf::from(&p);
                let abs = if pb.is_absolute() { pb } else { self.cwd.join(pb) };
                lexical_clean(&abs).to_string_lossy().into_owned()
            })
            .collect()
    }

    /// Resolve `p` (must be absolute) to a path inside the cwd. `must_exist`: the file itself
    /// must exist (reads); otherwise only its parent directory must.
    fn resolve_in_cwd(&self, p: &str, must_exist: bool) -> std::result::Result<PathBuf, (i64, String)> {
        let path = Path::new(p);
        if !path.is_absolute() {
            return Err((ERR_INVALID_PARAMS, "path must be absolute".into()));
        }
        let resolved = if must_exist {
            std::fs::canonicalize(path).map_err(|e| (ERR_NOT_FOUND, format!("cannot open file: {e}")))?
        } else {
            let clean = lexical_clean(path);
            let (Some(parent), Some(name)) = (clean.parent(), clean.file_name()) else {
                return Err((ERR_INVALID_PARAMS, "invalid file path".into()));
            };
            std::fs::canonicalize(parent)
                .map_err(|e| (ERR_NOT_FOUND, format!("parent directory not found: {e}")))?
                .join(name)
        };
        if resolved.starts_with(&self.cwd_canon) {
            Ok(resolved)
        } else {
            Err((ERR_INVALID_PARAMS, "path is outside the workspace".into()))
        }
    }

    fn read_text_file(&self, params: &Value) -> std::result::Result<String, (i64, String)> {
        let p = s(params, "path").ok_or((ERR_INVALID_PARAMS, "missing path".to_string()))?;
        let path = self.resolve_in_cwd(p, true)?;
        let md = std::fs::metadata(&path).map_err(|e| (ERR_NOT_FOUND, e.to_string()))?;
        if !md.is_file() {
            return Err((ERR_INVALID_PARAMS, "not a regular file".into()));
        }
        if md.len() > MAX_READ_BYTES {
            return Err((ERR_INVALID_PARAMS, format!("file larger than {MAX_READ_BYTES} bytes")));
        }
        let bytes = std::fs::read(&path).map_err(|e| (ERR_INTERNAL, e.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| (ERR_INVALID_PARAMS, "file is not valid UTF-8".to_string()))?;
        let line = params.get("line").and_then(Value::as_u64);
        let limit = params.get("limit").and_then(Value::as_u64);
        if line.is_none() && limit.is_none() {
            return Ok(text);
        }
        let skip = line.map_or(0, |l| l.saturating_sub(1)) as usize;
        let take = limit.map_or(usize::MAX, |l| l as usize);
        Ok(text.split_inclusive('\n').skip(skip).take(take).collect())
    }

    fn on_write(&mut self, id: &Value, params: &Value, step: &mut Step) {
        let (Some(p), Some(content)) = (s(params, "path"), s(params, "content")) else {
            step.outgoing
                .push(Self::reply_err(id, ERR_INVALID_PARAMS, "missing path or content"));
            return;
        };
        let path = match self.resolve_in_cwd(p, false) {
            Ok(path) => path,
            Err((code, msg)) => {
                step.outgoing.push(Self::reply_err(id, code, &msg));
                return;
            }
        };
        let pre_approved = self.mode == PermissionMode::Full
            || self.approved_paths.contains(&path_key(p))
            || self.approved_paths.contains(&path_key(&path.to_string_lossy()));
        if self.mode == PermissionMode::ReadOnly {
            step.outgoing
                .push(Self::reply_err(id, ERR_AUTH_REQUIRED, "writes are disabled in read-only mode"));
            return;
        }
        if pre_approved {
            let m = match std::fs::write(&path, content.as_bytes()) {
                Ok(()) => Self::reply(id, Value::Null),
                Err(e) => Self::reply_err(id, ERR_INTERNAL, &format!("write failed: {e}")),
            };
            step.outgoing.push(m);
            return;
        }
        let key = format!("acp-fs-write-{}", id_key(id));
        step.events.push(AgentEvent::ApprovalRequest {
            id: key.clone(),
            tool: "write_text_file".into(),
            detail: truncate(&path.to_string_lossy(), 400),
        });
        step.events.push(AgentEvent::Status(AgentStatus::WaitingForInput));
        self.writes.insert(
            key,
            PendingWrite { rpc_id: id.clone(), path, content: content.to_string() },
        );
    }
}

/// `allow_once` / `reject_once` first, then the `*_always` variant, then any option of that
/// polarity. With `always`, the `*_always` option is preferred.
fn pick_option(options: &[PermOption], allow: bool, always: bool) -> Option<String> {
    let (once, forever, prefix) = if allow {
        ("allow_once", "allow_always", "allow")
    } else {
        ("reject_once", "reject_always", "reject")
    };
    let order = if always { [forever, once] } else { [once, forever] };
    for k in order {
        if let Some(o) = options.iter().find(|o| o.kind == k) {
            return Some(o.id.clone());
        }
    }
    options.iter().find(|o| o.kind.starts_with(prefix)).map(|o| o.id.clone())
}

fn chunk_text(u: &Value) -> Option<String> {
    let c = u.get("content")?;
    match s(c, "type") {
        Some("text") => s(c, "text").map(str::to_string),
        Some("resource_link") => s(c, "name").or_else(|| s(c, "uri")).map(|n| format!("[{n}]")),
        _ => None,
    }
}

/// One-line, content-free-ish description of what a permission is about.
fn permission_detail(tc: &Value) -> String {
    let title = s(tc, "title").unwrap_or("");
    if let Some(inp) = tc.get("rawInput").filter(|v| v.is_object()) {
        for k in ["command", "cmd", "filepath", "filePath", "file_path", "path", "pattern", "url", "query"] {
            if let Some(v) = s(inp, k) {
                return truncate(&format!("{title}: {v}"), 400);
            }
        }
    }
    if let Some(p) = tc
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .and_then(|l| s(l, "path"))
    {
        return truncate(&format!("{title}: {p}"), 400);
    }
    truncate(title, 400)
}

fn tool_summary(u: &Value, ok: bool) -> String {
    if let Some(items) = u.get("content").and_then(Value::as_array) {
        for it in items {
            match s(it, "type") {
                Some("content") => {
                    if let Some(t) = it.get("content").and_then(|c| s(c, "text")).filter(|t| !t.trim().is_empty()) {
                        return truncate(t.trim(), 300);
                    }
                }
                Some("diff") => {
                    if let Some(p) = s(it, "path") {
                        return truncate(&format!("edited {p}"), 300);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(out) = u.get("rawOutput") {
        if let Some(t) = out.as_str() {
            return truncate(t.trim(), 300);
        }
        for k in ["output", "stdout", "error", "message"] {
            if let Some(t) = s(out, k).filter(|t| !t.trim().is_empty()) {
                return truncate(t.trim(), 300);
            }
        }
    }
    if let Some(t) = s(u, "title") {
        return truncate(t, 300);
    }
    (if ok { "completed" } else { "failed" }).to_string()
}

// ---------------------------------------------------------------------------
// Process driver
// ---------------------------------------------------------------------------

/// How to start an ACP agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Label for thread names / logs.
    pub label: String,
}

impl AcpCommand {
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        let program = program.into();
        let label = program
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("acp")
            .to_string();
        Self { program, args, env: Vec::new(), label }
    }

    /// `opencode acp --cwd <cwd> [extra_args]`.
    pub fn opencode(cfg: &ThreadConfig) -> Result<Self> {
        let exe = opencode::resolve().ok_or_else(|| DriverError::NotFound("opencode".into()))?;
        Ok(Self { label: "opencode".into(), ..Self::new(exe, opencode::acp_args(cfg)) })
    }

    /// From a command line (`[AgentKind::Acp]`): program is looked up on PATH, `cfg.extra_args`
    /// are appended.
    pub fn from_command_line(command: &str, cfg: &ThreadConfig) -> Result<Self> {
        let mut parts = split_command(command);
        if parts.is_empty() {
            return Err(DriverError::NotFound("(empty ACP command)".into()));
        }
        let prog = parts.remove(0);
        let exe = proc::resolve_executable(&prog).ok_or(DriverError::NotFound(prog))?;
        parts.extend(cfg.extra_args.iter().cloned());
        Ok(Self::new(exe, parts))
    }
}

pub struct AcpDriver {
    session: Arc<Mutex<AcpSession>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Child>>,
    tx: UnboundedSender<AgentEvent>,
    pid: u32,
}

fn write_all(stdin: &Mutex<Option<ChildStdin>>, msgs: &[Value]) -> Result<()> {
    let mut g = stdin.lock();
    let w = g.as_mut().ok_or(DriverError::Closed)?;
    for m in msgs {
        let mut line = m.to_string();
        line.push('\n');
        w.write_all(line.as_bytes()).map_err(|_| DriverError::Closed)?;
    }
    w.flush().map_err(|_| DriverError::Closed)
}

fn wait_exit(child: &Mutex<Child>, grace: Duration) -> Option<i32> {
    let deadline = Instant::now() + grace;
    loop {
        {
            let mut c = child.lock();
            if let Ok(Some(st)) = c.try_wait() {
                return st.code();
            }
            if Instant::now() >= deadline {
                let _ = c.kill();
                return c.wait().ok().and_then(|s| s.code());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl AcpDriver {
    /// Start `AgentKind::Acp { command }`.
    pub fn spawn_command_line(command: &str, cfg: &ThreadConfig, tx: UnboundedSender<AgentEvent>) -> Result<Self> {
        Self::spawn_command_line_with(command, cfg, tx, &SpawnOpts::default())
    }

    /// [`spawn_command_line`](Self::spawn_command_line) with [`SpawnOpts`].
    pub fn spawn_command_line_with(
        command: &str,
        cfg: &ThreadConfig,
        tx: UnboundedSender<AgentEvent>,
        opts: &SpawnOpts,
    ) -> Result<Self> {
        Self::spawn_with(&AcpCommand::from_command_line(command, cfg)?, cfg, tx, opts)
    }

    /// Start `opencode acp`.
    pub fn spawn_opencode(cfg: &ThreadConfig, tx: UnboundedSender<AgentEvent>) -> Result<Self> {
        Self::spawn_opencode_with(cfg, tx, &SpawnOpts::default())
    }

    /// [`spawn_opencode`](Self::spawn_opencode) with [`SpawnOpts`].
    pub fn spawn_opencode_with(cfg: &ThreadConfig, tx: UnboundedSender<AgentEvent>, opts: &SpawnOpts) -> Result<Self> {
        Self::spawn_with(&AcpCommand::opencode(cfg)?, cfg, tx, opts)
    }

    /// Start any ACP agent process and perform the handshake in the background; events
    /// (`SessionStarted`, ...) arrive on the receiver paired with `tx`.
    pub fn spawn(cmd: &AcpCommand, cfg: &ThreadConfig, tx: UnboundedSender<AgentEvent>) -> Result<Self> {
        Self::spawn_with(cmd, cfg, tx, &SpawnOpts::default())
    }

    /// [`spawn`](Self::spawn) with [`SpawnOpts`] (Job Object membership).
    pub fn spawn_with(
        cmd: &AcpCommand,
        cfg: &ThreadConfig,
        tx: UnboundedSender<AgentEvent>,
        opts: &SpawnOpts,
    ) -> Result<Self> {
        let mut c = proc::command_for(&cmd.program);
        c.args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = proc::spawn_in_job(&mut c, opts.job)?;
        let pid = child.id();
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take().ok_or(DriverError::Closed)?;
        let stderr = child.stderr.take().ok_or(DriverError::Closed)?;
        let child = Arc::new(Mutex::new(child));

        let mut session = AcpSession::new(cfg);
        let first = session.start();
        let session = Arc::new(Mutex::new(session));
        write_all(&stdin, &first)?;

        let label = cmd.label.clone();
        {
            let label = label.clone();
            std::thread::Builder::new().name("acp-stderr".into()).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                    log::debug!("{label} stderr: {line}");
                }
            })?;
        }

        {
            let (session, stdin, child, tx) = (session.clone(), stdin.clone(), child.clone(), tx.clone());
            let lines = coalesce::spawn_line_reader("acp-stdout-io", stdout)?;
            std::thread::Builder::new().name("acp-stdout".into()).spawn(move || {
                let mut co = Coalescer::new(tx.clone());
                let alive = coalesce::pump(&lines, &mut co, |line| {
                    let step = session.lock().on_line(line);
                    if !step.outgoing.is_empty() {
                        if let Err(e) = write_all(&stdin, &step.outgoing) {
                            log::warn!("{label}: write failed: {e}");
                        }
                    }
                    step.events
                });
                if !alive {
                    return;
                }
                let code = wait_exit(&child, Duration::from_secs(5));
                let _ = tx.unbounded_send(AgentEvent::Exited { code });
            })?;
        }

        Ok(Self { session, stdin, child, tx, pid })
    }

    fn apply(&self, step: Step) -> Result<()> {
        if !step.outgoing.is_empty() {
            write_all(&self.stdin, &step.outgoing)?;
        }
        for ev in step.events {
            let _ = self.tx.unbounded_send(ev);
        }
        Ok(())
    }

    /// Approve and ask the agent to remember the grant (`allow_always`), when it offers that.
    pub fn approve_always(&mut self, id: &str) -> Result<()> {
        let step = self.session.lock().approve_with(id, true, true)?;
        self.apply(step)
    }

    /// The ACP session id, once `session/new` returned.
    pub fn session_id(&self) -> Option<String> {
        self.session.lock().session_id().map(str::to_string)
    }
}

impl AgentDriver for AcpDriver {
    fn send(&mut self, text: &str) -> Result<()> {
        let step = self.session.lock().user_message(text);
        self.apply(step)
    }

    fn interrupt(&mut self) -> Result<()> {
        let step = self.session.lock().interrupt()?;
        self.apply(step)
    }

    fn approve(&mut self, id: &str, allow: bool) -> Result<()> {
        let step = self.session.lock().approve(id, allow)?;
        self.apply(step)
    }

    fn approve_with(&mut self, id: &str, decision: ApprovalDecision) -> Result<()> {
        let step = self
            .session
            .lock()
            .approve_with(id, decision.allows(), decision == ApprovalDecision::Always)?;
        self.apply(step)
    }

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn shutdown(&mut self) {
        self.stdin.lock().take();
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            {
                let mut c = self.child.lock();
                if matches!(c.try_wait(), Ok(Some(_))) {
                    return;
                }
                if Instant::now() >= deadline {
                    proc::kill_tree(self.pid);
                    let _ = c.kill();
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for AcpDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<Value> {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn cfg() -> ThreadConfig {
        ThreadConfig::new(std::env::temp_dir())
    }

    /// Drive a session through the initialize + session/new responses.
    fn ready(cfg: &ThreadConfig) -> (AcpSession, Vec<AgentEvent>) {
        let mut sess = AcpSession::new(cfg);
        let first = sess.start();
        assert_eq!(first[0]["method"], "initialize");
        let mut events = Vec::new();
        let init = sess.on_message(&json!({"jsonrpc":"2.0","id":0,"result":{
            "protocolVersion":1,
            "agentCapabilities":{"loadSession":true,"sessionCapabilities":{"resume":{}}},
            "authMethods":[{"id":"login"}]
        }}));
        assert_eq!(init.outgoing[0]["method"], "session/new");
        let opened = sess.on_message(&json!({"jsonrpc":"2.0","id":1,"result":{"sessionId":"s1"}}));
        events.extend(opened.events);
        (sess, events)
    }

    fn update(sess: &mut AcpSession, u: Value) -> Step {
        sess.on_message(&json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":u}}))
    }

    #[test]
    fn split_command_handles_quotes() {
        assert_eq!(split_command(r#""C:\Program Files\x\g.exe" --acp  -v"#), vec![r"C:\Program Files\x\g.exe", "--acp", "-v"]);
        assert_eq!(split_command("gemini --experimental-acp"), vec!["gemini", "--experimental-acp"]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn kind_mapping() {
        assert_eq!(tool_kind("read"), ToolKind::Read);
        assert_eq!(tool_kind("search"), ToolKind::Search);
        assert_eq!(tool_kind("delete"), ToolKind::Edit);
        assert_eq!(tool_kind("execute"), ToolKind::Command);
        assert_eq!(tool_kind("fetch"), ToolKind::Web);
        assert_eq!(tool_kind("think"), ToolKind::Other("think".into()));
    }

    #[test]
    fn handshake_advertises_capabilities_and_opens_session() {
        let mut sess = AcpSession::new(&cfg());
        let first = sess.start();
        let p = &first[0]["params"];
        assert_eq!(p["protocolVersion"], 1);
        assert_eq!(p["clientCapabilities"]["fs"]["readTextFile"], true);
        assert_eq!(p["clientCapabilities"]["fs"]["writeTextFile"], true);
        assert_eq!(p["clientCapabilities"]["terminal"], false);
        let (_, ev) = ready(&cfg());
        assert_eq!(ev, vec![AgentEvent::SessionStarted { session_id: "s1".into(), model: None }]);
    }

    #[test]
    fn recorded_opencode_handshake() {
        let msgs = fixture("opencode_acp_handshake.jsonl");
        let mut sess = AcpSession::new(&cfg());
        sess.start();
        let a = sess.on_message(&msgs[0]);
        assert_eq!(a.outgoing[0]["method"], "session/new");
        let b = sess.on_message(&msgs[1]);
        assert_eq!(
            b.events,
            vec![AgentEvent::SessionStarted {
                session_id: "ses_e048338a7ffeMneVXo2TD5D85l".into(),
                model: Some("opencode/big-pickle".into())
            }]
        );
        assert_eq!(sess.session_id(), Some("ses_e048338a7ffeMneVXo2TD5D85l"));
        assert_eq!(sess.modes, vec!["build", "plan"]);
        assert!(sess.has_model_option && sess.has_models && sess.has_mode_option);
    }

    #[test]
    fn model_and_read_only_mode_are_set_sequentially_before_first_prompt() {
        let mut c = cfg();
        c.model = Some("anthropic/claude-sonnet-5-5".into());
        c.permission_mode = PermissionMode::ReadOnly;
        let mut sess = AcpSession::new(&c);
        sess.start();
        sess.on_message(&json!({"id":0,"result":{"agentCapabilities":{}}}));
        let user = sess.user_message("hi"); // before ready: queued
        assert!(user.outgoing.is_empty());
        let opened = sess.on_message(&json!({"id":1,"result":{
            "sessionId":"s1",
            "configOptions":[{"id":"model","currentValue":"opencode/x"},{"id":"mode","currentValue":"build"}],
            "modes":{"availableModes":[{"id":"build"},{"id":"plan"}],"currentModeId":"build"}
        }}));
        assert_eq!(opened.outgoing.len(), 1, "one setup request at a time");
        assert_eq!(opened.outgoing[0]["method"], "session/set_config_option");
        assert_eq!(opened.outgoing[0]["params"]["configId"], "model");
        assert_eq!(opened.outgoing[0]["params"]["value"], "anthropic/claude-sonnet-5-5");
        let id = opened.outgoing[0]["id"].clone();
        let after_model = sess.on_message(&json!({"id":id,"result":{}}));
        assert_eq!(after_model.outgoing[0]["params"]["configId"], "mode");
        assert_eq!(after_model.outgoing[0]["params"]["value"], "plan");
        let id = after_model.outgoing[0]["id"].clone();
        let after_mode = sess.on_message(&json!({"id":id,"error":{"code":-32602,"message":"nope"}}));
        // a failing setup request does not block the thread: the queued prompt goes out
        assert_eq!(after_mode.outgoing[0]["method"], "session/prompt");
        assert_eq!(after_mode.outgoing[0]["params"]["prompt"][0]["text"], "hi");
    }

    #[test]
    fn set_model_fallback_without_config_options() {
        let mut c = cfg();
        c.model = Some("m2".into());
        let mut sess = AcpSession::new(&c);
        sess.start();
        sess.on_message(&json!({"id":0,"result":{}}));
        let opened = sess.on_message(&json!({"id":1,"result":{"sessionId":"s","models":{"currentModelId":"m1","availableModels":[]}}}));
        assert_eq!(opened.outgoing[0]["method"], "session/set_model");
        assert_eq!(opened.outgoing[0]["params"]["modelId"], "m2");
    }

    #[test]
    fn streaming_updates_map_to_events() {
        let (mut sess, _) = ready(&cfg());
        let sent = sess.user_message("go");
        assert_eq!(sent.events, vec![AgentEvent::Status(AgentStatus::Working)]);
        assert_eq!(sent.outgoing[0]["method"], "session/prompt");
        let thought = update(&mut sess, json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hm"}}));
        assert_eq!(thought.events, vec![AgentEvent::Thinking { text: "hm".into(), delta: true }]);
        let msg = update(&mut sess, json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hel"}}));
        assert_eq!(msg.events, vec![AgentEvent::Text { text: "Hel".into(), delta: true }]);
        let user = update(&mut sess, json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"echo"}}));
        assert!(user.events.is_empty());
        let plan = update(&mut sess, json!({"sessionUpdate":"plan","entries":[
            {"content":"a","priority":"high","status":"completed"},
            {"content":"b","priority":"low","status":"in_progress"},
            {"content":"c","priority":"low","status":"pending"}]}));
        assert_eq!(
            plan.events,
            vec![AgentEvent::Plan { tasks: vec![
                ("a".into(), TaskState::Done), ("b".into(), TaskState::InProgress), ("c".into(), TaskState::Pending)] }]
        );
    }

    #[test]
    fn tool_call_lifecycle() {
        let (mut sess, _) = ready(&cfg());
        let c = update(&mut sess, json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"bash","kind":"execute","status":"pending","rawInput":{"command":"ls"}}));
        assert_eq!(
            c.events,
            vec![AgentEvent::ToolCall { id: "t1".into(), kind: ToolKind::Command, title: "bash".into(), input_json: json!({"command":"ls"}) }]
        );
        let run = update(&mut sess, json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress"}));
        assert!(run.events.is_empty());
        let done = update(&mut sess, json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":"a.txt\nb.txt"}}]}));
        assert_eq!(done.events, vec![AgentEvent::ToolResult { id: "t1".into(), ok: true, summary: "a.txt\nb.txt".into() }]);
        let dup = update(&mut sess, json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}));
        assert!(dup.events.is_empty(), "result only once");
        // update without a prior announcement synthesizes the call, then fails it
        let orphan = update(&mut sess, json!({"sessionUpdate":"tool_call_update","toolCallId":"t2","status":"failed","kind":"read","title":"read","rawOutput":{"error":"boom"}}));
        assert!(matches!(&orphan.events[0], AgentEvent::ToolCall { id, kind: ToolKind::Read, .. } if id == "t2"));
        assert_eq!(orphan.events[1], AgentEvent::ToolResult { id: "t2".into(), ok: false, summary: "boom".into() });
    }

    #[test]
    fn prompt_response_emits_usage_with_cost_delta_and_done() {
        let (mut sess, _) = ready(&cfg());
        let sent = sess.user_message("go");
        let pid = sent.outgoing[0]["id"].clone();
        update(&mut sess, json!({"sessionUpdate":"usage_update","used":100,"size":200000,"cost":{"amount":0.25,"currency":"USD"}}));
        let done = sess.on_message(&json!({"id":pid,"result":{"stopReason":"end_turn","usage":{
            "totalTokens":50,"inputTokens":10,"outputTokens":20,"thoughtTokens":5,"cachedReadTokens":7,"cachedWriteTokens":3}}}));
        assert_eq!(
            done.events,
            vec![
                AgentEvent::Usage { input: 10, output: 25, cache_create: 3, cache_read: 7, cost_usd: Some(0.25), context_window: Some(200000) },
                AgentEvent::Status(AgentStatus::Done),
            ]
        );
        // second turn: cumulative 0.40 -> increment 0.15
        let sent = sess.user_message("again");
        let pid = sent.outgoing[0]["id"].clone();
        update(&mut sess, json!({"sessionUpdate":"usage_update","used":1,"size":200000,"cost":{"amount":0.40,"currency":"USD"}}));
        let done = sess.on_message(&json!({"id":pid,"result":{"stopReason":"end_turn"}}));
        match &done.events[0] {
            AgentEvent::Usage { cost_usd: Some(c), input: 0, .. } => assert!((c - 0.15).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn non_usd_cost_is_ignored_and_refusal_is_error() {
        let (mut sess, _) = ready(&cfg());
        let sent = sess.user_message("go");
        let pid = sent.outgoing[0]["id"].clone();
        update(&mut sess, json!({"sessionUpdate":"usage_update","used":1,"size":2,"cost":{"amount":9.0,"currency":"EUR"}}));
        let done = sess.on_message(&json!({"id":pid,"result":{"stopReason":"refusal"}}));
        assert!(matches!(&done.events[..], [AgentEvent::Status(AgentStatus::Error(_))]));
    }

    #[test]
    fn messages_sent_while_busy_are_queued_in_order() {
        let (mut sess, _) = ready(&cfg());
        let first = sess.user_message("one");
        let pid = first.outgoing[0]["id"].clone();
        let second = sess.user_message("two");
        assert!(second.outgoing.is_empty());
        let done = sess.on_message(&json!({"id":pid,"result":{"stopReason":"end_turn"}}));
        assert_eq!(done.outgoing[0]["params"]["prompt"][0]["text"], "two");
        assert!(!done.events.contains(&AgentEvent::Status(AgentStatus::Done)), "still working on the queued message");
    }

    fn perm_request(id: i64, kind: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"session/request_permission","params":{
            "sessionId":"s1",
            "toolCall":{"toolCallId":"t1","title":"bash","kind":kind,"rawInput":{"command":"rm -rf x"}},
            "options":[
                {"optionId":"once","kind":"allow_once","name":"Allow once"},
                {"optionId":"always","kind":"allow_always","name":"Always"},
                {"optionId":"reject","kind":"reject_once","name":"Reject"}]}})
    }

    #[test]
    fn permission_round_trip() {
        let (mut sess, _) = ready(&cfg());
        let req = sess.on_message(&perm_request(7, "execute"));
        assert!(req.outgoing.is_empty());
        let AgentEvent::ApprovalRequest { id, tool, detail } = &req.events[0] else { panic!("{:?}", req.events) };
        assert_eq!(tool, "bash");
        assert_eq!(detail, "bash: rm -rf x");
        assert_eq!(req.events[1], AgentEvent::Status(AgentStatus::WaitingForInput));
        let ans = sess.approve(id, true).unwrap();
        assert_eq!(ans.outgoing[0]["id"], 7);
        assert_eq!(ans.outgoing[0]["result"]["outcome"], json!({"outcome":"selected","optionId":"once"}));
        assert!(sess.approve(id, true).is_err(), "answered once");

        let req = sess.on_message(&perm_request(8, "execute"));
        let AgentEvent::ApprovalRequest { id, .. } = &req.events[0] else { panic!() };
        let ans = sess.approve(id, false).unwrap();
        assert_eq!(ans.outgoing[0]["result"]["outcome"]["optionId"], "reject");

        let req = sess.on_message(&perm_request(9, "execute"));
        let AgentEvent::ApprovalRequest { id, .. } = &req.events[0] else { panic!() };
        let ans = sess.approve_with(id, true, true).unwrap();
        assert_eq!(ans.outgoing[0]["result"]["outcome"]["optionId"], "always");
    }

    #[test]
    fn permission_without_matching_option_is_cancelled() {
        let (mut sess, _) = ready(&cfg());
        let mut r = perm_request(3, "execute");
        r["params"]["options"] = json!([{"optionId":"o","kind":"allow_once"}]);
        let req = sess.on_message(&r);
        let AgentEvent::ApprovalRequest { id, .. } = &req.events[0] else { panic!() };
        let ans = sess.approve(id, false).unwrap();
        assert_eq!(ans.outgoing[0]["result"]["outcome"], json!({"outcome":"cancelled"}));
    }

    #[test]
    fn permission_modes_auto_answer() {
        for (mode, kind, expect) in [
            (PermissionMode::Full, "execute", Some("once")),
            (PermissionMode::AcceptEdits, "edit", Some("once")),
            (PermissionMode::AcceptEdits, "execute", None),
            (PermissionMode::ReadOnly, "edit", Some("reject")),
            (PermissionMode::ReadOnly, "execute", Some("reject")),
            (PermissionMode::ReadOnly, "read", None),
            (PermissionMode::Ask, "edit", None),
        ] {
            let mut c = cfg();
            c.permission_mode = mode;
            let (mut sess, _) = ready(&c);
            let req = sess.on_message(&perm_request(1, kind));
            match expect {
                Some(opt) => {
                    assert!(req.events.is_empty(), "{mode:?}/{kind}");
                    assert_eq!(req.outgoing[0]["result"]["outcome"]["optionId"], opt, "{mode:?}/{kind}");
                }
                None => assert!(matches!(req.events[0], AgentEvent::ApprovalRequest { .. }), "{mode:?}/{kind}"),
            }
        }
    }

    #[test]
    fn interrupt_cancels_turn_and_open_requests() {
        let (mut sess, _) = ready(&cfg());
        sess.user_message("go");
        sess.on_message(&perm_request(5, "execute"));
        sess.user_message("queued");
        let step = sess.interrupt().unwrap();
        assert!(step
            .outgoing
            .iter()
            .any(|m| m.get("method").and_then(Value::as_str) == Some("session/cancel")));
        let cancelled = step.outgoing.iter().find(|m| m["id"] == 5).unwrap();
        assert_eq!(cancelled["result"]["outcome"], json!({"outcome":"cancelled"}));
        // the agent ends the prompt with `cancelled`; the dropped queue is not dispatched
        let done = sess.on_message(&json!({"id":2,"result":{"stopReason":"cancelled"}}));
        assert!(done.outgoing.is_empty());
        assert_eq!(done.events, vec![AgentEvent::Status(AgentStatus::Done)]);
    }

    #[test]
    fn idle_interrupt_sends_nothing() {
        let (mut sess, _) = ready(&cfg());
        assert!(sess.interrupt().unwrap().outgoing.is_empty());
    }

    #[test]
    fn auth_required_is_reported() {
        let mut sess = AcpSession::new(&cfg());
        sess.start();
        sess.on_message(&json!({"id":0,"result":{"authMethods":[{"id":"opencode-login"}]}}));
        let r = sess.on_message(&json!({"id":1,"error":{"code":-32000,"message":"Authentication required"}}));
        match &r.events[0] {
            AgentEvent::Status(AgentStatus::Error(m)) => assert!(m.contains("opencode-login"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resume_prefers_resume_then_load_then_new() {
        let mut c = cfg();
        c.resume_session = Some("old".into());
        // resume capability
        let mut sess = AcpSession::new(&c);
        sess.start();
        let r = sess.on_message(&json!({"id":0,"result":{"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"resume":{}}}}}));
        assert_eq!(r.outgoing[0]["method"], "session/resume");
        assert_eq!(r.outgoing[0]["params"]["sessionId"], "old");
        // load only: replayed history is swallowed until the response
        let mut sess = AcpSession::new(&c);
        sess.start();
        let r = sess.on_message(&json!({"id":0,"result":{"agentCapabilities":{"loadSession":true}}}));
        assert_eq!(r.outgoing[0]["method"], "session/load");
        let replay = update(&mut sess, json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old"}}));
        assert!(replay.events.is_empty());
        let opened = sess.on_message(&json!({"id":1,"result":{}}));
        assert_eq!(opened.events[0], AgentEvent::SessionStarted { session_id: "old".into(), model: None });
        // neither
        let mut sess = AcpSession::new(&c);
        sess.start();
        let r = sess.on_message(&json!({"id":0,"result":{}}));
        assert_eq!(r.outgoing[0]["method"], "session/new");
    }

    #[test]
    fn unknown_agent_requests_get_method_not_found() {
        let (mut sess, _) = ready(&cfg());
        let r = sess.on_message(&json!({"jsonrpc":"2.0","id":"x1","method":"terminal/create","params":{}}));
        assert_eq!(r.outgoing[0]["id"], "x1");
        assert_eq!(r.outgoing[0]["error"]["code"], ERR_METHOD_NOT_FOUND);
    }

    // -- fs ------------------------------------------------------------------

    fn workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agents_driver_acp_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        dir
    }

    #[test]
    fn fs_read_is_confined_to_cwd() {
        let ws = workspace("read");
        std::fs::write(ws.join("a.txt"), "l1\nl2\nl3\n").unwrap();
        let outside = std::env::temp_dir().join("agents_driver_acp_read_outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let (mut sess, _) = ready(&ThreadConfig::new(&ws));
        let ask = |sess: &mut AcpSession, params: Value| {
            sess.on_message(&json!({"jsonrpc":"2.0","id":99,"method":"fs/read_text_file","params":params})).outgoing.remove(0)
        };
        let ok = ask(&mut sess, json!({"sessionId":"s1","path":ws.join("a.txt")}));
        assert_eq!(ok["result"]["content"], "l1\nl2\nl3\n");
        let slice = ask(&mut sess, json!({"sessionId":"s1","path":ws.join("a.txt"),"line":2,"limit":1}));
        assert_eq!(slice["result"]["content"], "l2\n");
        let out = ask(&mut sess, json!({"sessionId":"s1","path":outside}));
        assert_eq!(out["error"]["code"], ERR_INVALID_PARAMS);
        let dotdot = ask(&mut sess, json!({"sessionId":"s1","path":ws.join("sub").join("..").join("..").join("agents_driver_acp_read_outside.txt")}));
        assert!(dotdot.get("error").is_some(), "{dotdot}");
        let rel = ask(&mut sess, json!({"sessionId":"s1","path":"a.txt"}));
        assert_eq!(rel["error"]["code"], ERR_INVALID_PARAMS);
        let missing = ask(&mut sess, json!({"sessionId":"s1","path":ws.join("nope.txt")}));
        assert_eq!(missing["error"]["code"], ERR_NOT_FOUND);
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn fs_write_needs_approval_unless_pre_approved_or_full() {
        let ws = workspace("write");
        let target = ws.join("out.txt");
        let write_req = |id: i64, path: &Path, content: &str| {
            json!({"jsonrpc":"2.0","id":id,"method":"fs/write_text_file","params":{"sessionId":"s1","path":path,"content":content}})
        };
        // 1. surfaced as ApprovalRequest, written only after approve(true)
        let (mut sess, _) = ready(&ThreadConfig::new(&ws));
        let r = sess.on_message(&write_req(20, &target, "hello"));
        assert!(r.outgoing.is_empty());
        let AgentEvent::ApprovalRequest { id, tool, .. } = &r.events[0] else { panic!("{:?}", r.events) };
        assert_eq!(tool, "write_text_file");
        assert!(!target.exists());
        let ans = sess.approve(id, true).unwrap();
        assert_eq!(ans.outgoing[0]["id"], 20);
        assert!(ans.outgoing[0].get("error").is_none());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        // 2. denied
        let r = sess.on_message(&write_req(21, &ws.join("denied.txt"), "x"));
        let AgentEvent::ApprovalRequest { id, .. } = &r.events[0] else { panic!() };
        let ans = sess.approve(id, false).unwrap();
        assert!(ans.outgoing[0].get("error").is_some());
        assert!(!ws.join("denied.txt").exists());
        // 3. outside cwd: rejected outright, no prompt
        let outside = std::env::temp_dir().join("agents_driver_acp_write_outside.txt");
        let r = sess.on_message(&write_req(22, &outside, "x"));
        assert!(r.events.is_empty());
        assert_eq!(r.outgoing[0]["error"]["code"], ERR_INVALID_PARAMS);
        assert!(!outside.exists());
        // 4. a path approved via session/request_permission is written without a second prompt
        let mut p = perm_request(30, "edit");
        p["params"]["toolCall"]["rawInput"] = json!({"filepath": ws.join("sub").join("e.txt")});
        let r = sess.on_message(&p);
        let AgentEvent::ApprovalRequest { id, .. } = &r.events[0] else { panic!() };
        sess.approve(id, true).unwrap();
        let r = sess.on_message(&write_req(31, &ws.join("sub").join("e.txt"), "edited"));
        assert!(r.events.is_empty());
        assert!(r.outgoing[0].get("error").is_none());
        assert_eq!(std::fs::read_to_string(ws.join("sub").join("e.txt")).unwrap(), "edited");
        // 5. Full mode writes straight away; ReadOnly refuses
        let mut c = ThreadConfig::new(&ws);
        c.permission_mode = PermissionMode::Full;
        let (mut full, _) = ready(&c);
        let r = full.on_message(&write_req(40, &ws.join("full.txt"), "f"));
        assert!(r.events.is_empty() && ws.join("full.txt").exists());
        c.permission_mode = PermissionMode::ReadOnly;
        let (mut ro, _) = ready(&c);
        let r = ro.on_message(&write_req(41, &ws.join("ro.txt"), "f"));
        assert!(r.outgoing[0].get("error").is_some() && !ws.join("ro.txt").exists());
        let _ = std::fs::remove_dir_all(&ws);
    }
}
