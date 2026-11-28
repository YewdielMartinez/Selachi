//! Codex driver: `codex app-server` (JSON-RPC 2.0 over stdio, one JSON object per
//! line, the `"jsonrpc":"2.0"` member is omitted on the wire).
//!
//! # Verified facts (codex-cli 0.171.0-alpha.13, Windows)
//!
//! Sources: <https://developers.openai.com/codex/app-server> (transport, handshake,
//! method names), the JSON Schema emitted by the binary itself
//! (`codex app-server generate-json-schema --out <dir>`: parameter / notification /
//! approval shapes) and two live sessions recorded in `tests/fixtures/codex_*.jsonl`
//! (`initialize`, `account/read`, `account/rateLimits/read`, `model/list`,
//! `thread/start`, `turn/start`, streaming and the failed-turn path).
//!
//! * Handshake: `initialize {clientInfo:{name,title,version}}` -> response, then the
//!   notification `initialized`.
//! * `thread/start {cwd, model?, approvalPolicy, sandbox}` -> `{thread:{id}, model, ..}`;
//!   `approvalPolicy` is `untrusted | on-request | never`, `sandbox` is
//!   `read-only | workspace-write | danger-full-access`. `thread/resume {threadId, ..}`.
//! * `turn/start {threadId, input:[{type:"text",text}]}` -> `{turn:{id}}`; while a turn
//!   runs, `turn/steer {threadId, expectedTurnId, input}` appends input;
//!   `turn/interrupt {threadId, turnId}` ends it with `status:"interrupted"`.
//! * Notifications used: `turn/started`, `turn/completed` (`turn.status`
//!   `completed|interrupted|failed`, `turn.error.message`), `item/started`,
//!   `item/completed` (types `agentMessage`, `reasoning`, `commandExecution`,
//!   `fileChange`, `mcpToolCall`, `dynamicToolCall`, `webSearch`), `item/agentMessage/delta`,
//!   `item/reasoning/{summaryTextDelta,textDelta}`, `turn/plan/updated`
//!   (`plan:[{step,status:pending|inProgress|completed}]`), `thread/tokenUsage/updated`
//!   (`tokenUsage.{last,total}.{inputTokens,cachedInputTokens,cacheWriteInputTokens,outputTokens}`,
//!   `modelContextWindow`), `account/rateLimits/updated`.
//! * Approvals are *server requests* (they carry an `id`): `item/commandExecution/requestApproval`
//!   and `item/fileChange/requestApproval`; the client answers `{id, result:{decision}}` with
//!   `decision` in `accept | acceptForSession | decline | cancel`.
//! * `account/rateLimits/read` -> `rateLimits.{primary,secondary}.{usedPercent,windowDurationMins,resetsAt}`.
//!
//! Not live-tested (shapes taken from the schema only): approvals, `turn/steer`,
//! `turn/interrupt`, `thread/resume`, tool items.
//!
//! # TODO
//!
//! * `item/permissions/requestApproval`, `item/tool/requestUserInput`,
//!   `mcpServer/elicitation/request`, `item/tool/call` and the legacy
//!   `applyPatchApproval` / `execCommandApproval` server requests are answered with a
//!   JSON-RPC error (`-32601`) so the turn cannot hang; they need real UI.
//! * No `codex` on PATH fallback to the copy bundled in the VS Code extension.
//!
//! The protocol logic lives in [`CodexSession`], a pure state machine (messages in,
//! events + outgoing messages out) so it is fully unit-testable; [`CodexDriver`]
//! only moves lines between the child process and the session.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::coalesce::{self, Coalescer};
use crate::proc::{self, truncate};
use crate::types::*;

/// `(approvalPolicy, sandbox)` for a UI permission mode.
pub fn policy_for(mode: PermissionMode) -> (&'static str, &'static str) {
    match mode {
        PermissionMode::Ask => ("untrusted", "workspace-write"),
        PermissionMode::AcceptEdits => ("on-request", "workspace-write"),
        PermissionMode::ReadOnly => ("on-request", "read-only"),
        PermissionMode::Full => ("never", "danger-full-access"),
    }
}

/// What a state-machine call produced.
#[derive(Debug, Default)]
pub struct Step {
    pub events: Vec<AgentEvent>,
    /// JSON-RPC messages to write to the server's stdin.
    pub outgoing: Vec<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Req {
    Initialize,
    ThreadStart,
    TurnStart,
    RateLimits,
    ModelList,
    Other,
}

#[derive(Clone, Copy, Debug)]
enum ApprovalKind {
    Command,
    FileChange,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Our effort level as Codex spells it (`max` is Codex's `xhigh`).
fn codex_effort(e: &str) -> &str {
    if e == "max" {
        "xhigh"
    } else {
        e
    }
}

/// `model/list` -> models (hidden ones dropped).
fn parse_model_list(result: &Value) -> Vec<ModelInfo> {
    let Some(data) = result.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter(|m| m.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|m| {
            let id = s(m, "model").or_else(|| s(m, "id"))?.to_string();
            let label = s(m, "displayName")
                .filter(|l| !l.is_empty())
                .unwrap_or(&id)
                .to_string();
            let efforts = m
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|o| s(o, "reasoningEffort").map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(ModelInfo {
                id,
                label,
                efforts,
                default_effort: s(m, "defaultReasoningEffort").map(str::to_string),
            })
        })
        .collect()
}

fn rate_windows(rl: &Value) -> Vec<RateWindow> {
    ["primary", "secondary"]
        .iter()
        .filter_map(|name| {
            let w = rl.get(*name).filter(|w| w.is_object())?;
            Some(RateWindow {
                name: (*name).to_string(),
                used_percent: w.get("usedPercent")?.as_f64()?,
                window_mins: w.get("windowDurationMins").and_then(Value::as_u64),
                resets_at: w.get("resetsAt").and_then(Value::as_i64),
            })
        })
        .collect()
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Pure Codex app-server protocol state machine.
pub struct CodexSession {
    model: Option<String>,
    effort: Option<String>,
    cwd: String,
    approval_policy: &'static str,
    sandbox: &'static str,
    resume: Option<String>,
    next_id: i64,
    reqs: HashMap<i64, Req>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    /// A `turn/start` was sent and the turn has not completed yet.
    busy: bool,
    queue: Vec<String>,
    /// Item ids already delivered as deltas.
    streamed: HashSet<String>,
    /// Tool-like items for which a `ToolCall` was emitted.
    tool_items: HashSet<String>,
    approvals: HashMap<String, (Value, ApprovalKind)>,
    titles: HashMap<String, String>,
}

impl CodexSession {
    pub fn new(cfg: &ThreadConfig) -> Self {
        let (approval_policy, sandbox) = policy_for(cfg.permission_mode);
        Self {
            model: cfg.model.clone(),
            effort: cfg.effort.as_deref().map(codex_effort).map(str::to_string),
            cwd: cfg.cwd.to_string_lossy().into_owned(),
            approval_policy,
            sandbox,
            resume: cfg.resume_session.clone(),
            next_id: 0,
            reqs: HashMap::new(),
            thread_id: None,
            turn_id: None,
            busy: false,
            queue: Vec::new(),
            streamed: HashSet::new(),
            tool_items: HashSet::new(),
            approvals: HashMap::new(),
            titles: HashMap::new(),
        }
    }

    fn request(&mut self, kind: Req, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.reqs.insert(id, kind);
        json!({ "method": method, "id": id, "params": params })
    }

    /// First message(s) to send right after spawning the server.
    pub fn start(&mut self) -> Vec<Value> {
        vec![self.request(
            Req::Initialize,
            "initialize",
            json!({ "clientInfo": {
                "name": "selachi", "title": "Selachi", "version": env!("CARGO_PKG_VERSION")
            }}),
        )]
    }

    fn thread_params(&self) -> Value {
        let mut p = json!({
            "cwd": self.cwd,
            "approvalPolicy": self.approval_policy,
            "sandbox": self.sandbox,
        });
        if let Some(m) = &self.model {
            p["model"] = json!(m);
        }
        if let Some(t) = &self.resume {
            p["threadId"] = json!(t);
        }
        p
    }

    fn input(text: &str) -> Value {
        let mut items = vec![json!({ "type": "text", "text": text, "text_elements": [] })];
        for img in crate::images::extract_attached_images(text) {
            items.push(json!({
                "type": "localImage",
                "path": img.path.to_string_lossy(),
            }));
        }
        Value::Array(items)
    }

    #[cfg(test)]
    pub(crate) fn input_for_test(text: &str) -> Value {
        Self::input(text)
    }

    fn turn_start(&mut self, text: &str, step: &mut Step) {
        let Some(tid) = self.thread_id.clone() else {
            self.queue.push(text.to_string());
            return;
        };
        self.busy = true;
        let mut params = json!({ "threadId": tid, "input": Self::input(text) });
        if let Some(e) = &self.effort {
            params["effort"] = json!(e);
        }
        step.outgoing
            .push(self.request(Req::TurnStart, "turn/start", params));
    }

    /// User message: starts a turn, steers the running one, or queues until ready.
    pub fn user_message(&mut self, text: &str) -> Step {
        let mut step = Step::default();
        step.events.push(AgentEvent::Status(AgentStatus::Working));
        let (thread, turn) = (self.thread_id.clone(), self.turn_id.clone());
        match (thread, turn, self.busy) {
            (None, _, _) => self.queue.push(text.to_string()),
            (Some(t), Some(turn), true) => {
                let params = json!({
                    "threadId": t, "expectedTurnId": turn, "input": Self::input(text)
                });
                let m = self.request(Req::Other, "turn/steer", params);
                step.outgoing.push(m);
            }
            (Some(_), None, true) => self.queue.push(text.to_string()),
            (Some(_), _, false) => self.turn_start(text, &mut step),
        }
        step
    }

    pub fn interrupt(&mut self) -> Result<Step> {
        let (Some(t), Some(turn)) = (self.thread_id.clone(), self.turn_id.clone()) else {
            return Err(DriverError::Protocol("no active turn to interrupt".into()));
        };
        let mut step = Step::default();
        step.outgoing.push(self.request(
            Req::Other,
            "turn/interrupt",
            json!({ "threadId": t, "turnId": turn }),
        ));
        Ok(step)
    }

    pub fn approve(&mut self, id: &str, allow: bool) -> Result<Step> {
        self.approve_with(
            id,
            if allow {
                ApprovalDecision::Allow
            } else {
                ApprovalDecision::Deny
            },
        )
    }

    /// `Always` answers `acceptForSession`: the server stops asking for the same kind of
    /// command / file change for the rest of the thread.
    pub fn approve_with(&mut self, id: &str, decision: ApprovalDecision) -> Result<Step> {
        let (rpc_id, _kind) = self
            .approvals
            .remove(id)
            .ok_or_else(|| DriverError::Protocol(format!("unknown approval id {id}")))?;
        let decision = match decision {
            ApprovalDecision::Allow => "accept",
            ApprovalDecision::Always => "acceptForSession",
            ApprovalDecision::Deny => "decline",
        };
        let mut step = Step::default();
        step.outgoing
            .push(json!({ "id": rpc_id, "result": { "decision": decision } }));
        step.events.push(AgentEvent::Status(AgentStatus::Working));
        Ok(step)
    }

    /// Feed one message received from the server.
    pub fn on_message(&mut self, v: &Value) -> Step {
        let mut step = Step::default();
        match (v.get("method").and_then(Value::as_str), v.get("id")) {
            (Some(m), Some(id)) => self.on_server_request(m, id, v, &mut step),
            (Some(m), None) => {
                self.on_notification(m, v.get("params").unwrap_or(&Value::Null), &mut step)
            }
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
                    log::debug!("codex: non-JSON stdout line ({e}): {}", truncate(line, 120));
                }
                Step::default()
            }
        }
    }

    // -- responses ---------------------------------------------------------

    fn on_response(&mut self, id: &Value, v: &Value, step: &mut Step) {
        let Some(kind) = id.as_i64().and_then(|i| self.reqs.remove(&i)) else {
            return; // response to a request we did not send (or already handled)
        };
        if let Some(err) = v.get("error") {
            let msg = s(err, "message").unwrap_or("request failed").to_string();
            match kind {
                Req::RateLimits => log::debug!("codex: rateLimits unavailable: {msg}"),
                Req::ModelList => log::debug!("codex: model/list unavailable: {msg}"),
                Req::TurnStart => {
                    self.busy = false;
                    step.events
                        .push(AgentEvent::Status(AgentStatus::Error(truncate(&msg, 300))));
                }
                _ => step
                    .events
                    .push(AgentEvent::Status(AgentStatus::Error(truncate(&msg, 300)))),
            }
            return;
        }
        let result = v.get("result").unwrap_or(&Value::Null);
        match kind {
            Req::Initialize => {
                step.outgoing
                    .push(json!({ "method": "initialized", "params": {} }));
                let (method, params) = if self.resume.is_some() {
                    ("thread/resume", self.thread_params())
                } else {
                    ("thread/start", self.thread_params())
                };
                let m = self.request(Req::ThreadStart, method, params);
                step.outgoing.push(m);
                let m = self.request(Req::RateLimits, "account/rateLimits/read", Value::Null);
                step.outgoing.push(m);
                // The catalogue for the model picker (no model call).
                let m = self.request(
                    Req::ModelList,
                    "model/list",
                    json!({ "includeHidden": false }),
                );
                step.outgoing.push(m);
            }
            Req::ModelList => {
                let models = parse_model_list(result);
                if !models.is_empty() {
                    let current = self.model.clone();
                    step.events.push(AgentEvent::Models { models, current });
                }
            }
            Req::ThreadStart => {
                let Some(tid) = result.pointer("/thread/id").and_then(Value::as_str) else {
                    step.events.push(AgentEvent::Status(AgentStatus::Error(
                        "thread/start returned no thread id".into(),
                    )));
                    return;
                };
                self.thread_id = Some(tid.to_string());
                step.events.push(AgentEvent::SessionStarted {
                    session_id: tid.to_string(),
                    model: s(result, "model").map(str::to_string),
                });
                if !self.queue.is_empty() {
                    let first = self.queue.remove(0);
                    self.turn_start(&first, step);
                }
            }
            Req::TurnStart => {
                if let Some(id) = result.pointer("/turn/id").and_then(Value::as_str) {
                    self.turn_id = Some(id.to_string());
                }
            }
            Req::RateLimits => {
                if let Some(rl) = result.get("rateLimits") {
                    let windows = rate_windows(rl);
                    if !windows.is_empty() {
                        step.events.push(AgentEvent::RateLimits { windows });
                    }
                }
            }
            Req::Other => {}
        }
    }

    // -- server requests (approvals) --------------------------------------

    fn on_server_request(&mut self, method: &str, id: &Value, v: &Value, step: &mut Step) {
        let p = v.get("params").unwrap_or(&Value::Null);
        let (tool, detail, kind) = match method {
            "item/commandExecution/requestApproval" => (
                "Command",
                s(p, "command")
                    .or_else(|| s(p, "reason"))
                    .unwrap_or("run a command")
                    .to_string(),
                ApprovalKind::Command,
            ),
            "item/fileChange/requestApproval" => (
                "FileChange",
                s(p, "reason")
                    .map(str::to_string)
                    .or_else(|| s(p, "itemId").and_then(|i| self.titles.get(i).cloned()))
                    .unwrap_or_else(|| "apply file changes".into()),
                ApprovalKind::FileChange,
            ),
            other => {
                log::warn!("codex: unsupported server request {other}; replying with an error");
                step.outgoing.push(json!({
                    "id": id,
                    "error": { "code": -32601, "message": format!("{other} is not supported by this client") }
                }));
                return;
            }
        };
        let key = id_key(id);
        self.approvals.insert(key.clone(), (id.clone(), kind));
        step.events.push(AgentEvent::ApprovalRequest {
            id: key,
            tool: tool.into(),
            detail: truncate(&detail, 300),
        });
        step.events
            .push(AgentEvent::Status(AgentStatus::WaitingForInput));
    }

    // -- notifications -----------------------------------------------------

    fn on_notification(&mut self, method: &str, p: &Value, step: &mut Step) {
        let ev = &mut step.events;
        match method {
            "turn/started" => {
                if let Some(id) = p.pointer("/turn/id").and_then(Value::as_str) {
                    self.turn_id = Some(id.to_string());
                }
                self.busy = true;
                ev.push(AgentEvent::Status(AgentStatus::Working));
            }
            "turn/completed" => {
                let status = match p.pointer("/turn/status").and_then(Value::as_str) {
                    Some("failed") => {
                        let msg = p
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("turn failed");
                        AgentStatus::Error(truncate(msg, 300))
                    }
                    _ => AgentStatus::Done,
                };
                self.turn_id = None;
                self.busy = false;
                self.streamed.clear();
                ev.push(AgentEvent::Status(status));
                if !self.queue.is_empty() {
                    let next = self.queue.remove(0);
                    self.turn_start(&next, step);
                }
            }
            "item/agentMessage/delta" => {
                if let (Some(id), Some(d)) = (s(p, "itemId"), s(p, "delta")) {
                    self.streamed.insert(id.to_string());
                    ev.push(AgentEvent::Text {
                        text: d.to_string(),
                        delta: true,
                    });
                }
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                if let (Some(id), Some(d)) = (s(p, "itemId"), s(p, "delta")) {
                    self.streamed.insert(id.to_string());
                    ev.push(AgentEvent::Thinking {
                        text: d.to_string(),
                        delta: true,
                    });
                }
            }
            "item/started" => {
                if let Some(item) = p.get("item") {
                    self.on_item_started(item, ev);
                }
            }
            "item/completed" => {
                if let Some(item) = p.get("item") {
                    self.on_item_completed(item, ev);
                }
            }
            "turn/plan/updated" => {
                if let Some(plan) = p.get("plan").and_then(Value::as_array) {
                    let tasks = plan
                        .iter()
                        .filter_map(|t| {
                            let state = match s(t, "status") {
                                Some("completed") => TaskState::Done,
                                Some("inProgress") => TaskState::InProgress,
                                _ => TaskState::Pending,
                            };
                            Some((s(t, "step")?.to_string(), state))
                        })
                        .collect();
                    ev.push(AgentEvent::Plan { tasks });
                }
            }
            "thread/tokenUsage/updated" => {
                if let Some(u) = p.pointer("/tokenUsage/last") {
                    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    // OpenAI counts cached tokens inside inputTokens; normalize to
                    // "input excluding cache reads" like Claude.
                    ev.push(AgentEvent::Usage {
                        input: g("inputTokens").saturating_sub(g("cachedInputTokens")),
                        output: g("outputTokens"),
                        cache_create: g("cacheWriteInputTokens"),
                        cache_read: g("cachedInputTokens"),
                        cost_usd: None,
                        context_window: p
                            .pointer("/tokenUsage/modelContextWindow")
                            .and_then(Value::as_u64),
                    });
                }
            }
            "account/rateLimits/updated" => {
                if let Some(rl) = p.get("rateLimits") {
                    let windows = rate_windows(rl);
                    if !windows.is_empty() {
                        ev.push(AgentEvent::RateLimits { windows });
                    }
                }
            }
            _ => {}
        }
    }

    fn on_item_started(&mut self, item: &Value, ev: &mut Vec<AgentEvent>) {
        let (Some(id), Some(ty)) = (s(item, "id"), s(item, "type")) else {
            return;
        };
        let (kind, title, input) = match ty {
            "commandExecution" => (
                ToolKind::Command,
                s(item, "command").unwrap_or("command").to_string(),
                json!({ "command": item.get("command"), "cwd": item.get("cwd") }),
            ),
            "fileChange" => {
                let changes = item.get("changes").cloned().unwrap_or(Value::Null);
                let paths: Vec<&str> = changes
                    .as_array()
                    .map(|a| a.iter().filter_map(|c| s(c, "path")).collect())
                    .unwrap_or_default();
                let title = if paths.is_empty() {
                    "file changes".to_string()
                } else {
                    paths.join(", ")
                };
                (ToolKind::Edit, title, changes)
            }
            "mcpToolCall" => (
                ToolKind::Other(format!(
                    "{}/{}",
                    s(item, "server").unwrap_or("mcp"),
                    s(item, "tool").unwrap_or("tool")
                )),
                s(item, "tool").unwrap_or("tool").to_string(),
                item.get("arguments").cloned().unwrap_or(Value::Null),
            ),
            "dynamicToolCall" => (
                ToolKind::Other(s(item, "tool").unwrap_or("tool").to_string()),
                s(item, "tool").unwrap_or("tool").to_string(),
                item.get("arguments").cloned().unwrap_or(Value::Null),
            ),
            "webSearch" => (
                ToolKind::Web,
                s(item, "query").unwrap_or("web search").to_string(),
                json!({ "query": item.get("query") }),
            ),
            _ => return,
        };
        let title = truncate(&title, 200);
        self.titles.insert(id.to_string(), title.clone());
        self.tool_items.insert(id.to_string());
        ev.push(AgentEvent::ToolCall {
            id: id.to_string(),
            kind,
            title,
            input_json: input,
        });
    }

    fn on_item_completed(&mut self, item: &Value, ev: &mut Vec<AgentEvent>) {
        let (Some(id), Some(ty)) = (s(item, "id"), s(item, "type")) else {
            return;
        };
        let status = s(item, "status").unwrap_or("");
        match ty {
            "agentMessage" => {
                if !self.streamed.remove(id) {
                    if let Some(t) = s(item, "text").filter(|t| !t.is_empty()) {
                        ev.push(AgentEvent::Text {
                            text: t.to_string(),
                            delta: false,
                        });
                    }
                }
            }
            "reasoning" => {
                if !self.streamed.remove(id) {
                    let join = |k: &str| -> String {
                        item.get(k)
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default()
                    };
                    let mut text = join("summary");
                    if text.is_empty() {
                        text = join("content");
                    }
                    if !text.is_empty() {
                        ev.push(AgentEvent::Thinking { text, delta: false });
                    }
                }
            }
            _ if !self.tool_items.remove(id) => {}
            "commandExecution" => {
                let code = item.get("exitCode").and_then(Value::as_i64);
                let ok = status == "completed" && code.unwrap_or(0) == 0;
                let out = s(item, "aggregatedOutput").map(str::trim).unwrap_or("");
                let summary = if out.is_empty() {
                    match code {
                        Some(c) => format!("exit {c}"),
                        None => status.to_string(),
                    }
                } else {
                    truncate(out, 300)
                };
                ev.push(AgentEvent::ToolResult {
                    id: id.to_string(),
                    ok,
                    summary,
                });
            }
            "fileChange" => {
                let n = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                ev.push(AgentEvent::ToolResult {
                    id: id.to_string(),
                    ok: status == "completed",
                    summary: if status == "completed" {
                        format!("{n} file(s) changed")
                    } else {
                        status.to_string()
                    },
                });
            }
            "mcpToolCall" => {
                let err = item.pointer("/error/message").and_then(Value::as_str);
                ev.push(AgentEvent::ToolResult {
                    id: id.to_string(),
                    ok: status == "completed",
                    summary: truncate(err.unwrap_or(status), 300),
                });
            }
            "dynamicToolCall" => {
                let ok = item
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(status == "completed");
                ev.push(AgentEvent::ToolResult {
                    id: id.to_string(),
                    ok,
                    summary: status.to_string(),
                });
            }
            "webSearch" => {
                ev.push(AgentEvent::ToolResult {
                    id: id.to_string(),
                    ok: true,
                    summary: String::new(),
                });
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

pub struct CodexDriver {
    session: Arc<Mutex<CodexSession>>,
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
        w.write_all(line.as_bytes())
            .map_err(|_| DriverError::Closed)?;
    }
    w.flush().map_err(|_| DriverError::Closed)
}

impl CodexDriver {
    pub fn spawn(cfg: &ThreadConfig, tx: UnboundedSender<AgentEvent>) -> Result<Self> {
        Self::spawn_with(cfg, tx, &SpawnOpts::default())
    }

    /// [`spawn`](Self::spawn) with [`SpawnOpts`] (Job Object membership).
    pub fn spawn_with(
        cfg: &ThreadConfig,
        tx: UnboundedSender<AgentEvent>,
        opts: &SpawnOpts,
    ) -> Result<Self> {
        let exe = proc::resolve_executable("codex")
            .ok_or_else(|| DriverError::NotFound("codex".into()))?;
        let mut cmd = proc::command_for(&exe);
        cmd.arg("app-server")
            .args(&cfg.extra_args)
            .current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        proc::apply_env(&mut cmd, &cfg.env);
        let mut child = proc::spawn_in_job(&mut cmd, opts.job)?;
        let pid = child.id();
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take().ok_or(DriverError::Closed)?;
        let stderr = child.stderr.take().ok_or(DriverError::Closed)?;
        let child = Arc::new(Mutex::new(child));

        let mut session = CodexSession::new(cfg);
        let first = session.start();
        let session = Arc::new(Mutex::new(session));
        write_all(&stdin, &first)?;

        std::thread::Builder::new()
            .name("codex-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                    log::debug!("codex stderr: {line}");
                }
            })?;

        {
            let (session, stdin, child, tx) =
                (session.clone(), stdin.clone(), child.clone(), tx.clone());
            let lines = coalesce::spawn_line_reader("codex-stdout-io", stdout)?;
            std::thread::Builder::new()
                .name("codex-stdout".into())
                .spawn(move || {
                    let mut co = Coalescer::new(tx.clone());
                    let alive = coalesce::pump(&lines, &mut co, |line| {
                        let step = session.lock().on_line(line);
                        if !step.outgoing.is_empty() {
                            if let Err(e) = write_all(&stdin, &step.outgoing) {
                                log::warn!("codex: write failed: {e}");
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

        Ok(Self {
            session,
            stdin,
            child,
            tx,
            pid,
        })
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

impl AgentDriver for CodexDriver {
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
        let step = self.session.lock().approve_with(id, decision)?;
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

impl Drop for CodexDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// The fixtures were recorded by a script that used its own request ids
    /// (thread/start = 3, turn/start = 4, rateLimits = 2); register them so the
    /// replay exercises the real response handling.
    fn replay(name: &str) -> Vec<AgentEvent> {
        let mut s = CodexSession::new(&ThreadConfig::new("."));
        s.reqs.insert(2, Req::RateLimits);
        s.reqs.insert(3, Req::ThreadStart);
        s.reqs.insert(4, Req::TurnStart);
        fixture(name)
            .lines()
            .flat_map(|l| s.on_line(l).events)
            .collect()
    }

    #[test]
    fn recorded_basic_turn() {
        let ev = replay("codex_basic.jsonl");
        assert!(matches!(&ev[0], AgentEvent::RateLimits { windows }
            if windows[0].name == "primary" && windows[0].used_percent == 12.0
               && windows[0].window_mins == Some(43200)));
        assert!(ev.iter().any(
            |e| matches!(e, AgentEvent::SessionStarted { session_id, model }
            if session_id.starts_with("01a1fa88") && model.is_some())
        ));
        assert!(ev.contains(&AgentEvent::Text {
            text: "ok".into(),
            delta: true
        }));
        // the completed agentMessage must not repeat the streamed text
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, AgentEvent::Text { .. }))
                .count(),
            1
        );
        let usage = ev.iter().find_map(|e| match e {
            AgentEvent::Usage {
                input,
                output,
                cache_create,
                cache_read,
                context_window,
                ..
            } => Some((*input, *output, *cache_create, *cache_read, *context_window)),
            _ => None,
        });
        assert_eq!(usage, Some((19702, 5, 0, 0, Some(258_400))));
        assert_eq!(ev.last(), Some(&AgentEvent::Status(AgentStatus::Done)));
        assert!(ev.contains(&AgentEvent::Status(AgentStatus::Working)));
    }

    #[test]
    fn recorded_failed_turn() {
        let ev = replay("codex_error.jsonl");
        match ev.last() {
            Some(AgentEvent::Status(AgentStatus::Error(m))) => assert!(m.contains("not supported")),
            other => panic!("expected error status, got {other:?}"),
        }
    }

    #[test]
    fn handshake_sequence() {
        let mut cfg = ThreadConfig::new("C:/work");
        cfg.model = Some("gpt-5.6-luna".into());
        cfg.permission_mode = PermissionMode::ReadOnly;
        let mut s = CodexSession::new(&cfg);
        let first = s.start();
        assert_eq!(first[0]["method"], "initialize");
        assert_eq!(first[0]["params"]["clientInfo"]["name"], "selachi");
        // a message sent before the thread exists is queued, then flushed
        assert!(s.user_message("hi").outgoing.is_empty());
        let step = s.on_message(&json!({"id":0,"result":{"userAgent":"x"}}));
        assert_eq!(step.outgoing[0]["method"], "initialized");
        assert_eq!(step.outgoing[1]["method"], "thread/start");
        assert_eq!(step.outgoing[1]["params"]["sandbox"], "read-only");
        assert_eq!(step.outgoing[1]["params"]["approvalPolicy"], "on-request");
        assert_eq!(step.outgoing[1]["params"]["model"], "gpt-5.6-luna");
        assert_eq!(step.outgoing[2]["method"], "account/rateLimits/read");
        let tid_req = step.outgoing[1]["id"].clone();
        let step = s.on_message(&json!({"id":tid_req,"result":{"thread":{"id":"t1"},"model":"m"}}));
        assert!(
            matches!(&step.events[0], AgentEvent::SessionStarted { session_id, .. } if session_id == "t1")
        );
        assert_eq!(step.outgoing[0]["method"], "turn/start");
        assert_eq!(step.outgoing[0]["params"]["threadId"], "t1");
        assert_eq!(step.outgoing[0]["params"]["input"][0]["text"], "hi");
    }

    #[test]
    fn steer_when_busy_and_interrupt() {
        let mut s = CodexSession::new(&ThreadConfig::new("."));
        s.thread_id = Some("t".into());
        assert!(s.interrupt().is_err());
        s.on_message(
            &json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"u1"}}}),
        );
        let step = s.user_message("more");
        assert_eq!(step.outgoing[0]["method"], "turn/steer");
        assert_eq!(step.outgoing[0]["params"]["expectedTurnId"], "u1");
        let step = s.interrupt().unwrap();
        assert_eq!(step.outgoing[0]["method"], "turn/interrupt");
        assert_eq!(step.outgoing[0]["params"]["turnId"], "u1");
    }

    /// Synthetic: shapes from the generated JSON schema (not a live recording).
    #[test]
    fn command_approval_roundtrip() {
        let mut s = CodexSession::new(&ThreadConfig::new("."));
        let step = s.on_message(&json!({
            "method":"item/commandExecution/requestApproval","id":7,
            "params":{"itemId":"i1","threadId":"t","turnId":"u","command":"git status","cwd":"C:/x","startedAtMs":1}
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::ApprovalRequest {
                id: "7".into(),
                tool: "Command".into(),
                detail: "git status".into()
            }
        );
        let step = s.approve("7", true).unwrap();
        assert_eq!(
            step.outgoing[0],
            json!({"id":7,"result":{"decision":"accept"}})
        );
        assert!(s.approve("7", true).is_err());
        s.on_message(&json!({"method":"item/fileChange/requestApproval","id":8,"params":{"itemId":"i2","reason":"write file"}}));
        let step = s.approve("8", false).unwrap();
        assert_eq!(step.outgoing[0]["result"]["decision"], "decline");
    }

    #[test]
    fn approve_with_maps_each_decision() {
        for (d, want) in [
            (ApprovalDecision::Allow, "accept"),
            (ApprovalDecision::Always, "acceptForSession"),
            (ApprovalDecision::Deny, "decline"),
        ] {
            let mut s = CodexSession::new(&ThreadConfig::new("."));
            s.on_message(
                &json!({"method":"item/commandExecution/requestApproval","id":3,
                "params":{"itemId":"i","command":"ls"}}),
            );
            let step = s.approve_with("3", d).unwrap();
            assert_eq!(step.outgoing[0], json!({"id":3,"result":{"decision":want}}));
        }
    }

    #[test]
    fn unsupported_server_request_gets_an_error_reply() {
        let mut s = CodexSession::new(&ThreadConfig::new("."));
        let step =
            s.on_message(&json!({"method":"item/tool/requestUserInput","id":"abc","params":{}}));
        assert!(step.events.is_empty());
        assert_eq!(step.outgoing[0]["id"], "abc");
        assert_eq!(step.outgoing[0]["error"]["code"], -32601);
    }

    /// Synthetic tool items (schema shapes).
    #[test]
    fn command_item_maps_to_tool_events() {
        let mut s = CodexSession::new(&ThreadConfig::new("."));
        let start = json!({"method":"item/started","params":{"threadId":"t","turnId":"u","startedAtMs":1,
            "item":{"type":"commandExecution","id":"c1","command":"dir","cwd":"C:/x","status":"inProgress","commandActions":[]}}});
        let ev = s.on_message(&start).events;
        assert!(
            matches!(&ev[0], AgentEvent::ToolCall { id, kind: ToolKind::Command, title, .. } if id == "c1" && title == "dir")
        );
        let done = json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","completedAtMs":2,
            "item":{"type":"commandExecution","id":"c1","command":"dir","cwd":"C:/x","status":"completed","exitCode":0,"aggregatedOutput":"a.txt\n","commandActions":[]}}});
        let ev = s.on_message(&done).events;
        assert_eq!(
            ev,
            vec![AgentEvent::ToolResult {
                id: "c1".into(),
                ok: true,
                summary: "a.txt".into()
            }]
        );
        let plan = s.on_message(&json!({"method":"turn/plan/updated","params":{"threadId":"t","turnId":"u",
            "plan":[{"step":"a","status":"completed"},{"step":"b","status":"inProgress"},{"step":"c","status":"pending"}]}})).events;
        assert_eq!(
            plan,
            vec![AgentEvent::Plan {
                tasks: vec![
                    ("a".into(), TaskState::Done),
                    ("b".into(), TaskState::InProgress),
                    ("c".into(), TaskState::Pending)
                ]
            }]
        );
    }

    #[test]
    fn permission_mapping() {
        assert_eq!(
            policy_for(PermissionMode::Ask),
            ("untrusted", "workspace-write")
        );
        assert_eq!(
            policy_for(PermissionMode::Full),
            ("never", "danger-full-access")
        );
    }

    /// Live and non-consuming: handshake + `account/rateLimits/read` through the
    /// real server. Needs `codex` on PATH. `cargo test -p agents_driver -- --ignored codex_live`
    #[test]
    #[ignore]
    fn codex_live_handshake() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let dir = std::env::temp_dir().join("agents_driver_live_codex");
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ThreadConfig::new(&dir);
        cfg.permission_mode = PermissionMode::ReadOnly;
        let mut d = CodexDriver::spawn(&cfg, tx).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let (mut started, mut limits) = (false, false);
        while Instant::now() < deadline && !(started && limits) {
            match rx.try_recv() {
                Ok(AgentEvent::SessionStarted { .. }) => started = true,
                Ok(AgentEvent::RateLimits { .. }) => limits = true,
                Ok(_) => {}
                Err(futures::channel::mpsc::TryRecvError::Closed) => break,
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        d.shutdown();
        assert!(started, "no SessionStarted");
    }

    #[test]
    fn model_list_is_parsed_and_effort_rides_on_turn_start() {
        let r = json!({"data":[
            {"id":"a","model":"gpt-5.6-luna","displayName":"GPT-5.6 Luna","hidden":false,"isDefault":true,
             "defaultReasoningEffort":"medium",
             "supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"high","description":""}]},
            {"id":"b","model":"secret","displayName":"Secret","hidden":true}
        ]});
        let m = parse_model_list(&r);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "gpt-5.6-luna");
        assert_eq!(m[0].label, "GPT-5.6 Luna");
        assert_eq!(m[0].efforts, vec!["low", "high"]);
        assert_eq!(m[0].default_effort.as_deref(), Some("medium"));

        let mut cfg = ThreadConfig::new("C:/work");
        cfg.effort = Some("max".into());
        let mut s = CodexSession::new(&cfg);
        let _ = s.start();
        let step = s.on_message(&json!({"id":0,"result":{}}));
        assert!(step.outgoing.iter().any(|m| m["method"] == "model/list"));
        let tid = step.outgoing[1]["id"].clone();
        s.on_message(&json!({"id":tid,"result":{"thread":{"id":"t1"},"model":"m"}}));
        let step = s.user_message("hi");
        assert_eq!(
            step.outgoing[0]["params"]["effort"], "xhigh",
            "max is Codex's xhigh"
        );
    }
}
