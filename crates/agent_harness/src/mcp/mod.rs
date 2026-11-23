//! `selachi-agent mcp`: an MCP server over **stdio only** (no sockets, no ports) that exposes the agent
//! harness as typed tools: `list_agents`, `run_agent`, `agent_status`, `agent_result`, `cancel_agent`,
//! `get_diff`, `apply_changes`.
//!
//! # Protocol
//!
//! Hand-written JSON-RPC 2.0, newline-delimited UTF-8 (one message per line, no embedded newlines),
//! stdout carries only protocol messages, diagnostics go to stderr. Checked against
//! modelcontextprotocol.io on 2026-11-23:
//!
//! * **Latest revision `2026-07-28` ("modern")** has no handshake: every request carries
//!   `_meta["io.modelcontextprotocol/protocolVersion"]`, the server answers `server/discover` and
//!   results carry `resultType: "complete"`. An unsupported version gets error `-32022`
//!   (`UnsupportedProtocolVersionError`, `data: {supported, requested}`).
//! * **Legacy revisions `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`** use `initialize` /
//!   `notifications/initialized`; the server echoes the client's version when it knows it, else answers
//!   with `2025-11-25`. Claude Code and Antigravity speak this today.
//!
//! The server is dual-era: a request with the modern `_meta` is served statelessly, anything else follows
//! the legacy rules. Requests are accepted without a prior `initialize` (stateless, harmless: every call
//! is checked on its own).
//!
//! # Why no `rmcp`
//!
//! The official Rust SDK is async (tokio) and pulls schemars/tower-style layers into a binary that is
//! otherwise synchronous and small; this server needs seven tools, one transport and about 600 lines of
//! plain `serde_json`. No new dependency was added (`futures` is only used by the test driver).
//!
//! # Concurrency and cancellation
//!
//! `tools/call` runs on its own thread (at most [`MAX_IN_FLIGHT`]), so a blocking `run_agent` never
//! stops `agent_status` or `cancel_agent` from being served. `notifications/cancelled` stops the work
//! and suppresses the response. When the client closes stdin, in-flight blocking runs are cancelled;
//! `async` runs keep going (their result stays in the registry).
//!
//! # Limits inherited from the caller
//!
//! `SELACHI_PARENT_PERMISSION`, `SELACHI_DELEGATION_DEPTH`, `SELACHI_PARENT_THREAD`, `SELACHI_PARENT_TAB`
//! of this server's own environment apply to every call (see [`runs`] for how they reach each run).
//!
//! Not supported (answered with `-32600` / `-32601`): JSON-RPC batches, resources, prompts, logging,
//! sampling, elicitation, tasks, progress notifications.

pub mod runs;
pub mod schema;
pub mod tools;

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Map, Value};

pub use tools::{Ctx, ToolOutput};

/// Latest protocol revision (stateless, `server/discover`).
pub const LATEST_VERSION: &str = "2026-07-28";
/// Revisions that use the `initialize` handshake, newest first.
pub const LEGACY_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Largest accepted message (one line). Bigger ones are discarded with an error and the server keeps going.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// Tool calls served at once.
pub const MAX_IN_FLIGHT: usize = 32;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

const INSTRUCTIONS: &str = "Selachi delegates work to other coding agents (Codex, Gemini, OpenCode, Claude, Antigravity) under limits. \
Use list_agents first, then run_agent with a self-contained prompt (never secrets). Default permission is read_only; for edits use \
permission=accept_edits with worktree=true, review with get_diff, then apply_changes (the user must confirm; it never applies silently). \
Long task: run_agent with async=true, then agent_status / agent_result. Respect refusals: they say what to change.";

/// A JSON-RPC error.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into(), data: None }
    }
}

/// Per-call state shared between the worker and the reader (cancellation).
#[derive(Default)]
pub struct CallState {
    cancelled: AtomicBool,
}

impl CallState {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// One line read from the client.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Eof,
    Line(Vec<u8>),
    /// The line exceeded the limit; it was consumed and dropped.
    TooLong,
}

/// Read one `\n`-terminated message. Partial reads are fine (it just keeps reading); a line longer than
/// `max` is skipped without being buffered.
pub fn read_frame<R: BufRead>(r: &mut R, max: usize) -> io::Result<Frame> {
    let mut buf: Vec<u8> = Vec::new();
    let mut too_long = false;
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(if too_long {
                Frame::TooLong
            } else if buf.is_empty() {
                Frame::Eof
            } else {
                Frame::Line(buf)
            });
        }
        let (take, found) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if buf.len() + take > max {
                too_long = true;
                buf = Vec::new();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        r.consume(take + usize::from(found));
        if found {
            return Ok(if too_long { Frame::TooLong } else { Frame::Line(buf) });
        }
    }
}

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

struct Server {
    ctx: Ctx,
    out: Out,
    inflight: Mutex<HashMap<String, Arc<CallState>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

impl Server {
    fn send(&self, msg: &Value) {
        // serde_json never emits a raw newline inside a compact document
        let mut line = serde_json::to_string(msg).unwrap_or_else(|_| "{}".into());
        line.push('\n');
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        if out.write_all(line.as_bytes()).and_then(|()| out.flush()).is_err() {
            log::debug!("mcp: the client closed stdout");
        }
    }

    fn respond(&self, id: &Value, result: Result<Value, RpcError>, modern: bool) {
        let msg = match result {
            Ok(mut r) => {
                if modern {
                    if let Some(o) = r.as_object_mut() {
                        o.entry("resultType").or_insert_with(|| json!("complete"));
                    }
                }
                json!({"jsonrpc": "2.0", "id": id, "result": r})
            }
            Err(e) => {
                let mut err = json!({"code": e.code, "message": e.message});
                if let Some(d) = e.data {
                    err["data"] = d;
                }
                json!({"jsonrpc": "2.0", "id": id, "error": err})
            }
        };
        self.send(&msg);
    }

    fn handle_line(self: &Arc<Self>, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return self.respond(&Value::Null, Err(RpcError::new(-32700, "parse error: the message is not valid UTF-8")), false);
        };
        let text = text.trim().trim_start_matches('\u{feff}');
        if text.is_empty() {
            return;
        }
        let msg: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return self.respond(&Value::Null, Err(RpcError::new(-32700, format!("parse error: {e}"))), false),
        };
        let Some(obj) = msg.as_object() else {
            let what = if msg.is_array() { "JSON-RPC batches are not supported; send one message per line" } else { "a message must be a JSON object" };
            return self.respond(&Value::Null, Err(RpcError::new(-32600, format!("invalid request: {what}"))), false);
        };
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // a response from the client (we never send requests) or garbage: nothing to do
            if !obj.contains_key("result") && !obj.contains_key("error") {
                self.respond(&Value::Null, Err(RpcError::new(-32600, "invalid request: no method")), false);
            }
            return;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        match obj.get("id") {
            None => self.notification(method, &params),
            Some(id) if id.is_string() || id.is_number() => self.request(id.clone(), method, params),
            Some(_) => self.respond(&Value::Null, Err(RpcError::new(-32600, "invalid request: id must be a string or a number")), false),
        }
    }

    fn notification(&self, method: &str, params: &Value) {
        if method == "notifications/cancelled" {
            if let Some(rid) = params.get("requestId") {
                let state = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).get(&id_key(rid)).cloned();
                if let Some(s) = state {
                    s.cancel();
                }
            }
        }
        // notifications/initialized, progress, roots/list_changed...: nothing to do
    }

    fn request(self: &Arc<Self>, id: Value, method: &str, params: Value) {
        // Which era? The modern per-request `_meta` wins; its version must be one we serve.
        let requested = params.get("_meta").and_then(|m| m.get(META_VERSION)).and_then(Value::as_str);
        let modern = requested.is_some();
        if let Some(v) = requested {
            if v != LATEST_VERSION {
                let mut supported = vec![LATEST_VERSION];
                supported.extend(LEGACY_VERSIONS);
                let mut e = RpcError::new(-32022, "Unsupported protocol version");
                e.data = Some(json!({"supported": supported, "requested": v}));
                return self.respond(&id, Err(e), true);
            }
        }
        match method {
            "tools/call" => self.tools_call(id, params, modern),
            "initialize" => self.respond(&id, Ok(initialize(&params)), false),
            "server/discover" => self.respond(&id, Ok(discover()), true),
            "ping" => self.respond(&id, Ok(json!({})), modern),
            "tools/list" => self.respond(&id, Ok(json!({"tools": schema::tools()})), modern),
            other => self.respond(&id, Err(RpcError::new(-32601, format!("method not found: {other}"))), modern),
        }
    }

    fn tools_call(self: &Arc<Self>, id: Value, params: Value, modern: bool) {
        let Some(name) = params.get("name").and_then(Value::as_str).map(str::to_string) else {
            return self.respond(&id, Err(RpcError::new(-32602, "invalid params: `name` is required")), modern);
        };
        if !schema::TOOL_NAMES.contains(&name.as_str()) {
            return self.respond(&id, Err(RpcError::new(-32602, format!("unknown tool: {name}"))), modern);
        }
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let state = Arc::new(CallState::default());
        {
            let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if inflight.len() >= MAX_IN_FLIGHT {
                drop(inflight);
                return self.respond(&id, Err(RpcError::new(-32603, format!("too many calls in flight (max {MAX_IN_FLIGHT})"))), modern);
            }
            inflight.insert(id_key(&id), state.clone());
        }
        let this = self.clone();
        let spawned = std::thread::Builder::new().name(format!("mcp-{name}")).spawn({
            let id = id.clone();
            move || {
                let out = tools::call(&this.ctx, &name, args, &state);
                this.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&id_key(&id));
                if state.is_cancelled() {
                    return; // the client gave up on this request: send nothing more for it
                }
                let result = match out {
                    Some(o) => Ok(tool_result(o)),
                    None => Err(RpcError::new(-32602, format!("unknown tool: {name}"))),
                };
                this.respond(&id, result, modern);
            }
        });
        match spawned {
            Ok(handle) => {
                let mut w = self.workers.lock().unwrap_or_else(|e| e.into_inner());
                w.retain(|h| !h.is_finished());
                w.push(handle);
            }
            Err(e) => {
                self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&id_key(&id));
                self.respond(&id, Err(RpcError::new(-32603, format!("cannot start a worker thread: {e}"))), modern);
            }
        }
    }

    /// Client closed stdin: stop blocking work and wait for the workers.
    fn shutdown(&self) {
        for s in self.inflight.lock().unwrap_or_else(|e| e.into_inner()).values() {
            s.cancel();
        }
        let workers: Vec<_> = std::mem::take(&mut *self.workers.lock().unwrap_or_else(|e| e.into_inner()));
        for w in workers {
            let _ = w.join();
        }
    }
}

fn server_info() -> Value {
    json!({"name": "selachi", "title": "Selachi agent harness", "version": env!("CARGO_PKG_VERSION")})
}

fn initialize(params: &Value) -> Value {
    let wanted = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = if LEGACY_VERSIONS.contains(&wanted) { wanted } else { LEGACY_VERSIONS[0] };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": server_info(),
        "instructions": INSTRUCTIONS,
    })
}

fn discover() -> Value {
    let mut meta = Map::new();
    meta.insert(META_SERVER_INFO.into(), server_info());
    json!({
        "supportedVersions": [LATEST_VERSION],
        "capabilities": {"tools": {}},
        "_meta": meta,
        "instructions": INSTRUCTIONS,
    })
}

/// `CallToolResult`: a short text block plus the structured JSON.
fn tool_result(o: ToolOutput) -> Value {
    json!({
        "content": [{"type": "text", "text": o.text}],
        "structuredContent": o.structured,
        "isError": o.is_error,
    })
}

/// Serve MCP on `reader` / `writer` until the client closes the input. Returns the exit code.
pub fn serve<R: BufRead>(mut reader: R, writer: Box<dyn Write + Send>, ctx: Ctx) -> i32 {
    let server = Arc::new(Server {
        ctx,
        out: Arc::new(Mutex::new(writer)),
        inflight: Mutex::new(HashMap::new()),
        workers: Mutex::new(Vec::new()),
    });
    let code = loop {
        match read_frame(&mut reader, MAX_MESSAGE_BYTES) {
            Ok(Frame::Eof) => break 0,
            Ok(Frame::Line(bytes)) => server.handle_line(&bytes),
            Ok(Frame::TooLong) => server.respond(
                &Value::Null,
                Err(RpcError::new(-32600, format!("invalid request: the message is larger than {MAX_MESSAGE_BYTES} bytes and was discarded"))),
                false,
            ),
            Err(e) => {
                log::warn!("mcp: cannot read stdin: {e}");
                break 1;
            }
        }
    };
    server.shutdown();
    code
}

/// `selachi-agent mcp`.
pub fn run_stdio() -> i32 {
    let stdin = io::stdin();
    serve(stdin.lock(), Box::new(io::stdout()), Ctx::from_env())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_split_on_newlines_and_keep_partial_input() {
        let mut r = Cursor::new(b"{\"a\":1}\r\n\n{\"b\":2}".to_vec());
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Line(b"{\"a\":1}\r".to_vec()));
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Line(Vec::new()));
        // a last line without a newline is still delivered
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Line(b"{\"b\":2}".to_vec()));
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Eof);
    }

    #[test]
    fn frames_survive_tiny_reads() {
        // 1-byte BufReader capacity: every message arrives in pieces
        let data = b"{\"x\":\"hello\"}\n{\"y\":2}\n".to_vec();
        let mut r = io::BufReader::with_capacity(1, Cursor::new(data));
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Line(b"{\"x\":\"hello\"}".to_vec()));
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Line(b"{\"y\":2}".to_vec()));
        assert_eq!(read_frame(&mut r, 100).unwrap(), Frame::Eof);
    }

    #[test]
    fn oversized_lines_are_dropped_and_the_next_one_is_read() {
        let mut data = vec![b'x'; 50];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut r = Cursor::new(data);
        assert_eq!(read_frame(&mut r, 10).unwrap(), Frame::TooLong);
        assert_eq!(read_frame(&mut r, 10).unwrap(), Frame::Line(b"ok".to_vec()));
        // too long and no newline before EOF
        let mut r = Cursor::new(vec![b'y'; 50]);
        assert_eq!(read_frame(&mut r, 10).unwrap(), Frame::TooLong);
        assert_eq!(read_frame(&mut r, 10).unwrap(), Frame::Eof);
    }

    #[test]
    fn initialize_negotiates_the_legacy_version() {
        assert_eq!(initialize(&json!({"protocolVersion": "2025-06-18"}))["protocolVersion"], "2025-06-18");
        assert_eq!(initialize(&json!({"protocolVersion": "2024-11-05"}))["protocolVersion"], "2024-11-05");
        assert_eq!(initialize(&json!({"protocolVersion": "1999-01-01"}))["protocolVersion"], "2025-11-25");
        assert_eq!(initialize(&json!({}))["protocolVersion"], "2025-11-25");
        let r = initialize(&json!({"protocolVersion": "2025-11-25"}));
        assert_eq!(r["serverInfo"]["name"], "selachi");
        assert!(r["capabilities"]["tools"].is_object());
    }
}
