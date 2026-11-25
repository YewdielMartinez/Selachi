//! Claude Code driver: one persistent `claude -p` process per thread, speaking
//! `stream-json` in both directions.
//!
//! # Verified facts (Claude Code 2.1.312, Windows)
//!
//! * Command: `claude -p --input-format stream-json --output-format stream-json --verbose`
//!   (`claude --help`; `--output-format stream-json` needs `--verbose`).
//! * User message on stdin, one JSON object per line (verified live):
//!   `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`
//!   (docs: <https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode>).
//! * `--permission-mode` accepts `acceptEdits | auto | bypassPermissions | default |
//!   dontAsk | plan` (`manual` is an alias of `default`).
//! * Output lines are `system` (`init`, `hook_*`, `commands_changed`, ...),
//!   `stream_event` (only with `--include-partial-messages`), `assistant`, `user`
//!   (tool results), `rate_limit_event` and a final `result` per turn with `usage`
//!   and `total_cost_usd`. See `tests/fixtures/claude_basic.jsonl` (recorded live).
//! * Approvals: `--permission-prompt-tool stdio` makes the CLI emit
//!   `{"type":"control_request","request_id":..,"request":{"subtype":"can_use_tool",
//!   "tool_name":..,"input":{..},"tool_use_id":..}}` and wait for
//!   `{"type":"control_response","response":{"subtype":"success","request_id":..,
//!   "response":{"behavior":"allow","updatedInput":{..}}}}` (or `behavior":"deny"` +
//!   `message`). The flag/subtype exist in the shipped binary; the JSON shapes come
//!   from the Agent SDK protocol (not live-tested here to avoid spending quota).
//! * "Always allow" is native: `approve_with(.., Always)` adds `updatedPermissions:
//!   [{"type":"addRules","rules":[{"toolName":..}],"behavior":"allow","destination":"session"}]`
//!   to the allow response (SDK `PermissionResult` / `PermissionUpdate`, docs:
//!   <https://code.claude.com/docs/en/agent-sdk/typescript>; the shipped binary validates the
//!   field). Whole-tool rule, this session only, nothing written to settings files.
//!
//! # Interrupt
//!
//! `interrupt()` sends `{"type":"control_request","request_id":..,"request":
//! {"subtype":"interrupt"}}` (Agent SDK: the turn then ends with a `result` whose
//! subtype is `error_during_execution` and the session stays usable). That is
//! mapped to `Status(Done)`, not an error. If the process is wedged, `shutdown()`
//! closes stdin and then kills the whole process tree (`taskkill /T /F`); a killed
//! thread can be resumed with `resume_session`.
//!
//! # Windows
//!
//! `claude` is usually an npm `.cmd` shim; [`crate::proc::command_for`] resolves the
//! real `claude.exe` the shim points to (falling back to std's `.cmd` handling) and
//! always spawns with `CREATE_NO_WINDOW`.
//!
//! # TODO
//!
//! `AskUserQuestion` arrives as a `can_use_tool` request; `approve(true)` echoes the
//! input without `answers`, so questions cannot be answered yet.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::coalesce::{self, Coalescer};
use crate::proc::{self, truncate};
use crate::types::*;

/// An open `can_use_tool` request: what we need to answer it later.
#[derive(Debug, Clone)]
struct PendingRequest {
    tool: String,
    input: Value,
}

type Pending = Arc<Mutex<HashMap<String, PendingRequest>>>;

// ---------------------------------------------------------------------------
// Tool mapping
// ---------------------------------------------------------------------------

/// Map a Claude Code tool name to a normalized [`ToolKind`].
pub fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" => ToolKind::Read,
        "Grep" | "Glob" | "ToolSearch" => ToolKind::Search,
        "Edit" | "MultiEdit" | "NotebookEdit" => ToolKind::Edit,
        "Write" => ToolKind::Write,
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" => ToolKind::Command,
        "WebFetch" | "WebSearch" => ToolKind::Web,
        "Task" | "Agent" => ToolKind::Task,
        other => ToolKind::Other(other.to_string()),
    }
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// One-line human title for a tool call.
pub fn tool_title(name: &str, input: &Value) -> String {
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| str_field(input, k))
            .map(str::to_string)
    };
    let t = match name {
        "Read" | "Edit" | "MultiEdit" | "Write" => pick(&["file_path"]),
        "NotebookEdit" => pick(&["notebook_path"]),
        "Bash" | "PowerShell" => pick(&["command"]),
        "Grep" | "Glob" => pick(&["pattern"]),
        "WebFetch" => pick(&["url"]),
        "WebSearch" => pick(&["query"]),
        "Task" | "Agent" => pick(&["description", "prompt"]),
        _ => None,
    };
    truncate(&t.unwrap_or_else(|| name.to_string()), 200)
}

fn approval_detail(tool: &str, input: &Value) -> String {
    match tool {
        "Bash" | "PowerShell" | "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit"
        | "Grep" | "Glob" | "WebFetch" | "WebSearch" => tool_title(tool, input),
        _ => truncate(&input.to_string(), 300),
    }
}

fn todo_plan(input: &Value) -> Option<Vec<(String, TaskState)>> {
    let todos = input.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|t| {
                let text = str_field(t, "content")?.to_string();
                let state = match str_field(t, "status") {
                    Some("completed") => TaskState::Done,
                    Some("in_progress") => TaskState::InProgress,
                    _ => TaskState::Pending,
                };
                Some((text, state))
            })
            .collect(),
    )
}

fn result_summary(content: &Value) -> String {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| str_field(p, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    truncate(text.trim(), 300)
}

fn window_mins(name: &str) -> Option<u64> {
    match name {
        "five_hour" => Some(300),
        "seven_day" | "seven_day_opus" | "seven_day_sonnet" => Some(10_080),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parser (pure: line in, events out)
// ---------------------------------------------------------------------------

/// Stateful line parser for the Claude Code `stream-json` output. Tolerant by
/// design: unknown message types/fields are ignored, malformed lines are logged.
pub struct ClaudeParser {
    /// `(message id, "text"|"thinking")` blocks already delivered as deltas, so the
    /// complete `assistant` message does not repeat them.
    streamed: HashSet<(String, &'static str)>,
    cur_msg: String,
    session_announced: bool,
    pending: Pending,
    interrupted: Arc<AtomicBool>,
}

impl Default for ClaudeParser {
    fn default() -> Self {
        Self::new(Pending::default(), Arc::default())
    }
}

impl ClaudeParser {
    fn new(pending: Pending, interrupted: Arc<AtomicBool>) -> Self {
        Self {
            streamed: HashSet::new(),
            cur_msg: String::new(),
            session_announced: false,
            pending,
            interrupted,
        }
    }

    /// Parse one stdout line into zero or more events.
    pub fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                log::debug!(
                    "claude: non-JSON stdout line ({e}): {}",
                    truncate(line, 120)
                );
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        match str_field(&v, "type") {
            Some("system") => self.on_system(&v, &mut out),
            Some("stream_event") => self.on_stream_event(&v, &mut out),
            Some("assistant") => self.on_assistant(&v, &mut out),
            Some("user") => self.on_user(&v, &mut out),
            Some("result") => self.on_result(&v, &mut out),
            Some("rate_limit_event") => self.on_rate_limit(&v, &mut out),
            Some("control_request") => self.on_control_request(&v, &mut out),
            Some("control_cancel_request") => {
                if let Some(id) = str_field(&v, "request_id") {
                    self.pending.lock().remove(id);
                }
            }
            _ => {}
        }
        out
    }

    fn on_system(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        if str_field(v, "subtype") == Some("init") && !self.session_announced {
            if let Some(id) = str_field(v, "session_id") {
                self.session_announced = true;
                out.push(AgentEvent::SessionStarted {
                    session_id: id.to_string(),
                    model: str_field(v, "model").map(str::to_string),
                });
            }
        }
    }

    fn on_stream_event(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        if !v.get("parent_tool_use_id").is_none_or(Value::is_null) {
            return; // sub-agent stream
        }
        let Some(ev) = v.get("event") else { return };
        match str_field(ev, "type") {
            Some("message_start") => {
                self.cur_msg = ev
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
            Some("content_block_delta") => {
                let Some(d) = ev.get("delta") else { return };
                match str_field(d, "type") {
                    Some("text_delta") => {
                        if let Some(t) = str_field(d, "text") {
                            self.streamed.insert((self.cur_msg.clone(), "text"));
                            out.push(AgentEvent::Text {
                                text: t.to_string(),
                                delta: true,
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = str_field(d, "thinking") {
                            self.streamed.insert((self.cur_msg.clone(), "thinking"));
                            out.push(AgentEvent::Thinking {
                                text: t.to_string(),
                                delta: true,
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn on_assistant(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        let msg = v.get("message").unwrap_or(&Value::Null);
        let msg_id = str_field(msg, "id").unwrap_or_default().to_string();
        let is_sub = !v.get("parent_tool_use_id").is_none_or(Value::is_null);
        let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
            return;
        };
        for b in blocks {
            match str_field(b, "type") {
                Some("text") if !is_sub => {
                    if !self.streamed.contains(&(msg_id.clone(), "text")) {
                        if let Some(t) = str_field(b, "text") {
                            out.push(AgentEvent::Text {
                                text: t.to_string(),
                                delta: false,
                            });
                        }
                    }
                }
                Some("thinking") if !is_sub => {
                    if !self.streamed.contains(&(msg_id.clone(), "thinking")) {
                        if let Some(t) = str_field(b, "thinking").filter(|t| !t.is_empty()) {
                            out.push(AgentEvent::Thinking {
                                text: t.to_string(),
                                delta: false,
                            });
                        }
                    }
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (str_field(b, "id"), str_field(b, "name")) else {
                        continue;
                    };
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    out.push(AgentEvent::ToolCall {
                        id: id.to_string(),
                        kind: tool_kind(name),
                        title: tool_title(name, &input),
                        input_json: input.clone(),
                    });
                    if name == "TodoWrite" {
                        if let Some(tasks) = todo_plan(&input) {
                            out.push(AgentEvent::Plan { tasks });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn on_user(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for b in blocks {
            if str_field(b, "type") == Some("tool_result") {
                if let Some(id) = str_field(b, "tool_use_id") {
                    out.push(AgentEvent::ToolResult {
                        id: id.to_string(),
                        ok: !b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        summary: result_summary(b.get("content").unwrap_or(&Value::Null)),
                    });
                }
            }
        }
    }

    fn on_result(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        self.streamed.clear();
        if let Some(u) = v.get("usage") {
            let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            let context_window = v
                .get("modelUsage")
                .and_then(Value::as_object)
                .and_then(|m| {
                    m.values()
                        .filter_map(|e| e.get("contextWindow")?.as_u64())
                        .max()
                });
            out.push(AgentEvent::Usage {
                input: g("input_tokens"),
                output: g("output_tokens"),
                cache_create: g("cache_creation_input_tokens"),
                cache_read: g("cache_read_input_tokens"),
                cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                context_window,
            });
        }
        let was_interrupted = self.interrupted.swap(false, Ordering::SeqCst);
        let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let status = if was_interrupted || !is_error {
            AgentStatus::Done
        } else {
            let msg = str_field(v, "result")
                .filter(|s| !s.is_empty())
                .or_else(|| str_field(v, "subtype"))
                .unwrap_or("error");
            AgentStatus::Error(truncate(msg, 300))
        };
        out.push(AgentEvent::Status(status));
    }

    fn on_rate_limit(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        let Some(wins) = v
            .pointer("/rate_limit_info/unifiedWindows")
            .and_then(Value::as_object)
        else {
            return;
        };
        let mut windows: Vec<RateWindow> = wins
            .iter()
            .filter_map(|(name, w)| {
                Some(RateWindow {
                    name: name.clone(),
                    used_percent: w.get("utilization")?.as_f64()? * 100.0,
                    window_mins: window_mins(name),
                    resets_at: w.get("resetsAt").and_then(Value::as_i64),
                })
            })
            .collect();
        windows.sort_by(|a, b| a.name.cmp(&b.name));
        if !windows.is_empty() {
            out.push(AgentEvent::RateLimits { windows });
        }
    }

    fn on_control_request(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        let Some(req) = v.get("request") else { return };
        if str_field(req, "subtype") != Some("can_use_tool") {
            return;
        }
        let (Some(rid), Some(tool)) = (str_field(v, "request_id"), str_field(req, "tool_name"))
        else {
            return;
        };
        let input = req.get("input").cloned().unwrap_or_else(|| json!({}));
        let detail = approval_detail(tool, &input);
        self.pending.lock().insert(
            rid.to_string(),
            PendingRequest {
                tool: tool.to_string(),
                input,
            },
        );
        out.push(AgentEvent::ApprovalRequest {
            id: rid.to_string(),
            tool: tool.to_string(),
            detail,
        });
        out.push(AgentEvent::Status(AgentStatus::WaitingForInput));
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// CLI arguments for `cfg` (without the executable).
pub fn build_args(cfg: &ThreadConfig) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]
    .map(String::from)
    .to_vec();
    if let Some(m) = &cfg.model {
        a.extend(["--model".into(), m.clone()]);
    }
    if let Some(e) = cfg.effort.as_deref().filter(|e| EFFORT_LEVELS.contains(e)) {
        a.extend(["--effort".into(), e.to_string()]);
    }
    match cfg.permission_mode {
        PermissionMode::Full => a.push("--dangerously-skip-permissions".into()),
        mode => {
            let pm = match mode {
                PermissionMode::Ask => "default",
                PermissionMode::AcceptEdits => "acceptEdits",
                PermissionMode::ReadOnly => "plan",
                PermissionMode::Full => unreachable!(),
            };
            a.extend(["--permission-mode".into(), pm.into()]);
            // Route approvals to us as control_request messages instead of auto-denying.
            a.extend(["--permission-prompt-tool".into(), "stdio".into()]);
        }
    }
    if let Some(s) = &cfg.resume_session {
        a.extend(["--resume".into(), s.clone()]);
    }
    a.extend(cfg.extra_args.iter().cloned());
    a
}

pub struct ClaudeDriver {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Child>>,
    tx: UnboundedSender<AgentEvent>,
    pending: Pending,
    interrupted: Arc<AtomicBool>,
    next_req: AtomicU64,
    pid: u32,
}

impl ClaudeDriver {
    pub fn spawn(cfg: &ThreadConfig, tx: UnboundedSender<AgentEvent>) -> Result<Self> {
        Self::spawn_with(cfg, tx, &SpawnOpts::default())
    }

    /// [`spawn`](Self::spawn) with [`SpawnOpts`] (Job Object membership).
    pub fn spawn_with(
        cfg: &ThreadConfig,
        tx: UnboundedSender<AgentEvent>,
        opts: &SpawnOpts,
    ) -> Result<Self> {
        let exe = proc::resolve_executable("claude")
            .ok_or_else(|| DriverError::NotFound("claude".into()))?;
        let mut cmd = proc::command_for(&exe);
        cmd.args(build_args(cfg))
            .current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = proc::spawn_in_job(&mut cmd, opts.job)?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or(DriverError::Closed)?;
        let stderr = child.stderr.take().ok_or(DriverError::Closed)?;

        let pending = Pending::default();
        let interrupted = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(child));

        std::thread::Builder::new()
            .name("claude-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                    log::warn!("claude stderr: {line}");
                }
            })?;

        {
            let tx = tx.clone();
            let child = child.clone();
            let mut parser = ClaudeParser::new(pending.clone(), interrupted.clone());
            let lines = coalesce::spawn_line_reader("claude-stdout-io", stdout)?;
            std::thread::Builder::new()
                .name("claude-stdout".into())
                .spawn(move || {
                    let mut co = Coalescer::new(tx.clone());
                    if !coalesce::pump(&lines, &mut co, |l| parser.parse_line(l)) {
                        return; // receiver dropped: nobody listens anymore
                    }
                    let code = wait_exit(&child, Duration::from_secs(5));
                    let _ = tx.unbounded_send(AgentEvent::Exited { code });
                })?;
        }

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            child,
            tx,
            pending,
            interrupted,
            next_req: AtomicU64::new(1),
            pid,
        })
    }

    fn write_json(&self, v: &Value) -> Result<()> {
        let mut guard = self.stdin.lock();
        let stdin = guard.as_mut().ok_or(DriverError::Closed)?;
        let mut line = v.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .map_err(|_| DriverError::Closed)?;
        stdin.flush().map_err(|_| DriverError::Closed)
    }

    fn emit(&self, ev: AgentEvent) {
        let _ = self.tx.unbounded_send(ev);
    }
}

/// The `control_response` line answering a `can_use_tool` request.
///
/// `Always` adds `updatedPermissions`, the Agent SDK's native "always allow": a
/// `PermissionUpdate` `{type:"addRules", rules:[{toolName}], behavior:"allow",
/// destination:"session"}` (types: <https://code.claude.com/docs/en/agent-sdk/typescript>,
/// `PermissionResult` / `PermissionUpdate`; the shipped binary validates this field too). The
/// rule covers the whole tool for the rest of the Claude session (`session` destination: nothing
/// is written to the user's settings files), so Claude stops sending `can_use_tool` for it.
fn permission_response(id: &str, req: &PendingRequest, decision: ApprovalDecision) -> Value {
    let body = match decision {
        ApprovalDecision::Deny => json!({ "behavior": "deny", "message": "Denied by the user" }),
        ApprovalDecision::Allow => json!({ "behavior": "allow", "updatedInput": req.input }),
        ApprovalDecision::Always => json!({
            "behavior": "allow",
            "updatedInput": req.input,
            "updatedPermissions": [{
                "type": "addRules",
                "rules": [{ "toolName": req.tool }],
                "behavior": "allow",
                "destination": "session",
            }],
        }),
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id, "response": body }
    })
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

pub(crate) fn user_message_json(text: &str) -> Value {
    let mut content = vec![json!({ "type": "text", "text": text })];
    for img in crate::images::extract_attached_images(text) {
        if matches!(
            img.mime_type,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.mime_type,
                    "data": img.base64,
                }
            }));
        }
    }
    json!({
        "type": "user",
        "message": { "role": "user", "content": content }
    })
}

impl AgentDriver for ClaudeDriver {
    fn send(&mut self, text: &str) -> Result<()> {
        self.interrupted.store(false, Ordering::SeqCst);
        self.write_json(&user_message_json(text))?;
        self.emit(AgentEvent::Status(AgentStatus::Working));
        Ok(())
    }

    fn interrupt(&mut self) -> Result<()> {
        self.interrupted.store(true, Ordering::SeqCst);
        let n = self.next_req.fetch_add(1, Ordering::SeqCst);
        self.write_json(&json!({
            "type": "control_request",
            "request_id": format!("selachi-{n}"),
            "request": { "subtype": "interrupt" }
        }))
    }

    fn approve(&mut self, id: &str, allow: bool) -> Result<()> {
        self.approve_with(
            id,
            if allow {
                ApprovalDecision::Allow
            } else {
                ApprovalDecision::Deny
            },
        )
    }

    fn approve_with(&mut self, id: &str, decision: ApprovalDecision) -> Result<()> {
        let req = self
            .pending
            .lock()
            .remove(id)
            .ok_or_else(|| DriverError::Protocol(format!("unknown approval id {id}")))?;
        self.write_json(&permission_response(id, &req, decision))?;
        self.emit(AgentEvent::Status(AgentStatus::Working));
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn shutdown(&mut self) {
        // Closing stdin lets `claude -p` finish and exit by itself.
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

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fixture(name: &str) -> Vec<AgentEvent> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).unwrap();
        let mut p = ClaudeParser::default();
        text.lines().flat_map(|l| p.parse_line(l)).collect()
    }

    #[test]
    fn recorded_basic_session() {
        let ev = parse_fixture("claude_basic.jsonl");
        assert!(
            matches!(&ev[0], AgentEvent::SessionStarted { session_id, model }
            if session_id.starts_with("00000000") && model.as_deref() == Some("claude-haiku-5-5"))
        );
        assert!(ev.contains(&AgentEvent::Text {
            text: "ok".into(),
            delta: false
        }));
        let usage = ev
            .iter()
            .find_map(|e| match e {
                AgentEvent::Usage {
                    input,
                    output,
                    cache_create,
                    cache_read,
                    cost_usd,
                    context_window,
                } => Some((
                    *input,
                    *output,
                    *cache_create,
                    *cache_read,
                    *cost_usd,
                    *context_window,
                )),
                _ => None,
            })
            .expect("usage");
        assert_eq!((usage.0, usage.1, usage.2, usage.3), (2, 4, 52063, 0));
        assert!((usage.4.unwrap() - 0.2084219).abs() < 1e-9);
        assert_eq!(usage.5, Some(1_000_000));
        assert_eq!(ev.last(), Some(&AgentEvent::Status(AgentStatus::Done)));
        let rl = ev.iter().find_map(|e| match e {
            AgentEvent::RateLimits { windows } => Some(windows.clone()),
            _ => None,
        });
        let rl = rl.expect("rate limits");
        assert_eq!(rl[0].name, "five_hour");
        assert!((rl[0].used_percent - 31.0).abs() < 1e-6);
        assert_eq!(rl[0].window_mins, Some(300));
        // hooks / commands_changed produce nothing
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
                .count(),
            1
        );
    }

    /// Hand-written from the documented SDK message shapes (NOT a live recording).
    #[test]
    fn synthetic_tools_stream() {
        let ev = parse_fixture("claude_tools_synthetic.jsonl");
        let texts: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Text { text, delta } => Some((text.as_str(), *delta)),
                _ => None,
            })
            .collect();
        // two deltas, and the full assistant text is NOT repeated for the streamed message
        assert_eq!(texts, vec![("Hel", true), ("lo", true), ("Done.", false)]);
        assert!(ev.contains(&AgentEvent::Thinking {
            text: "hmm".into(),
            delta: false
        }));
        assert!(ev.iter().any(|e| matches!(e,
            AgentEvent::ToolCall { id, kind: ToolKind::Read, title, .. }
            if id == "toolu_1" && title == "src/main.rs")));
        assert!(ev.contains(&AgentEvent::ToolResult {
            id: "toolu_1".into(),
            ok: true,
            summary: "fn main() {}".into()
        }));
        assert!(ev.contains(&AgentEvent::ToolResult {
            id: "toolu_2".into(),
            ok: false,
            summary: "exit 1".into()
        }));
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolCall { kind: ToolKind::Command, title, .. } if title == "cargo build")));
        assert!(ev.contains(&AgentEvent::Plan {
            tasks: vec![
                ("Write parser".into(), TaskState::Done),
                ("Write tests".into(), TaskState::InProgress),
                ("Ship".into(), TaskState::Pending),
            ]
        }));
        assert!(ev.contains(&AgentEvent::ApprovalRequest {
            id: "req_1".into(),
            tool: "Bash".into(),
            detail: "rm -rf build".into()
        }));
        assert!(ev.contains(&AgentEvent::Status(AgentStatus::WaitingForInput)));
        assert_eq!(
            ev.last(),
            Some(&AgentEvent::Status(AgentStatus::Error("boom".into())))
        );
    }

    #[test]
    fn approval_input_is_remembered_for_the_response() {
        let pending = Pending::default();
        let mut p = ClaudeParser::new(pending.clone(), Arc::default());
        p.parse_line(r#"{"type":"control_request","request_id":"r9","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"a.txt","content":"x"}}}"#);
        assert_eq!(pending.lock()["r9"].input["file_path"], "a.txt");
        assert_eq!(pending.lock()["r9"].tool, "Write");
        p.parse_line(r#"{"type":"control_cancel_request","request_id":"r9"}"#);
        assert!(pending.lock().is_empty());
    }

    #[test]
    fn approve_with_encodes_each_decision() {
        let req = PendingRequest {
            tool: "Bash".into(),
            input: json!({"command":"cargo test"}),
        };
        let r = |d| permission_response("r1", &req, d);
        assert_eq!(
            r(ApprovalDecision::Allow),
            json!({"type":"control_response","response":{"subtype":"success","request_id":"r1",
                "response":{"behavior":"allow","updatedInput":{"command":"cargo test"}}}})
        );
        assert_eq!(
            r(ApprovalDecision::Deny)["response"]["response"],
            json!({"behavior":"deny","message":"Denied by the user"})
        );
        assert_eq!(
            r(ApprovalDecision::Always)["response"]["response"],
            json!({
                "behavior":"allow",
                "updatedInput":{"command":"cargo test"},
                "updatedPermissions":[{"type":"addRules","rules":[{"toolName":"Bash"}],
                    "behavior":"allow","destination":"session"}]
            })
        );
    }

    #[test]
    fn interrupted_turn_is_done_not_error() {
        let flag = Arc::new(AtomicBool::new(true));
        let mut p = ClaudeParser::new(Pending::default(), flag);
        let ev = p.parse_line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":""}"#,
        );
        assert_eq!(ev, vec![AgentEvent::Status(AgentStatus::Done)]);
    }

    #[test]
    fn garbage_lines_are_ignored() {
        let mut p = ClaudeParser::default();
        assert!(p.parse_line("not json").is_empty());
        assert!(p.parse_line("").is_empty());
        assert!(p.parse_line(r#"{"type":"something_new","x":1}"#).is_empty());
    }

    #[test]
    fn args_mapping() {
        let mut cfg = ThreadConfig::new(".");
        cfg.model = Some("haiku".into());
        cfg.resume_session = Some("abc".into());
        cfg.extra_args = vec!["--effort".into(), "low".into()];
        for (mode, flag) in [
            (PermissionMode::Ask, "default"),
            (PermissionMode::AcceptEdits, "acceptEdits"),
            (PermissionMode::ReadOnly, "plan"),
        ] {
            cfg.permission_mode = mode;
            let a = build_args(&cfg);
            let i = a.iter().position(|x| x == "--permission-mode").unwrap();
            assert_eq!(a[i + 1], flag);
            assert!(a
                .windows(2)
                .any(|w| w == ["--permission-prompt-tool", "stdio"]));
            assert!(a.windows(2).any(|w| w == ["--resume", "abc"]));
            assert!(a.windows(2).any(|w| w == ["--model", "haiku"]));
            assert!(a.ends_with(&["--effort".into(), "low".into()]));
        }
        cfg.permission_mode = PermissionMode::Full;
        let a = build_args(&cfg);
        assert!(a.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(!a.contains(&"--permission-prompt-tool".to_string()));
        assert_eq!(a[0], "-p");
    }

    #[test]
    fn tool_kinds() {
        assert_eq!(tool_kind("Grep"), ToolKind::Search);
        assert_eq!(tool_kind("PowerShell"), ToolKind::Command);
        assert_eq!(tool_kind("mcp__x__y"), ToolKind::Other("mcp__x__y".into()));
    }

    /// Live, no model call: the real CLI must accept our full flag set, stay up
    /// while stdin is open, and exit (reporting `Exited`) once `shutdown` closes stdin.
    #[test]
    #[ignore]
    fn claude_live_spawn_and_shutdown_without_model_call() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let dir = std::env::temp_dir().join("agents_driver_live_claude_idle");
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ThreadConfig::new(&dir);
        cfg.permission_mode = PermissionMode::Ask;
        let mut d = ClaudeDriver::spawn(&cfg, tx).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            d.child.lock().try_wait().unwrap().is_none(),
            "claude exited early (bad flags?)"
        );
        d.shutdown();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            match rx.try_recv() {
                Ok(AgentEvent::Exited { .. }) => exited = true,
                Ok(_) => {}
                Err(futures::channel::mpsc::TryRecvError::Closed) => break,
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        assert!(exited, "no Exited event");
    }

    /// Live: spends a few cents of the user's subscription. Run with
    /// `cargo test -p agents_driver -- --ignored claude_live`.
    #[test]
    #[ignore]
    fn claude_live_roundtrip() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let dir = std::env::temp_dir().join("agents_driver_live_claude");
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ThreadConfig::new(&dir);
        cfg.model = Some("haiku".into());
        cfg.permission_mode = PermissionMode::ReadOnly;
        cfg.extra_args = vec!["--no-session-persistence".into()];
        let mut d = ClaudeDriver::spawn(&cfg, tx).unwrap();
        d.send("Reply with exactly: ok").unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut text = String::new();
        let mut done = false;
        while Instant::now() < deadline && !done {
            match rx.try_recv() {
                Ok(AgentEvent::Text { text: t, .. }) => text.push_str(&t),
                Ok(AgentEvent::Status(AgentStatus::Done)) => done = true,
                Ok(_) => {}
                Err(futures::channel::mpsc::TryRecvError::Closed) => break,
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        d.shutdown();
        assert!(done, "turn did not finish");
        assert!(text.to_lowercase().contains("ok"), "got {text:?}");
    }

    #[test]
    fn effort_becomes_a_flag_only_for_known_levels() {
        let mut cfg = ThreadConfig::new(".");
        assert!(!build_args(&cfg).contains(&"--effort".to_string()));
        cfg.effort = Some("xhigh".into());
        assert!(build_args(&cfg)
            .windows(2)
            .any(|w| w == ["--effort", "xhigh"]));
        cfg.effort = Some("turbo".into());
        assert!(!build_args(&cfg).contains(&"--effort".to_string()));
    }
}
