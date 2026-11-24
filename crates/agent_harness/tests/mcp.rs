#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `selachi-agent mcp` end to end: the real binary is started, spoken to over stdio, and the delegated
//! runs it starts use the scripted fake agent (`SELACHI_AGENT_FAKE`), so no model, CLI or network is
//! involved. `USERNAME` is always set to a name nobody serves, so a real Selachi window on this
//! machine can never answer a test's confirmation request.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use agent_harness::{DelegationConfig, Permission, Registry, RunRequest, RunStatus};
use common::*;
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(90);

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    stash: Vec<Value>,
    next_id: i64,
}

#[derive(Default)]
struct Opts<'a> {
    env: Vec<(&'a str, String)>,
    script: Option<Value>,
}

impl Mcp {
    fn start(data_dir: &Path, opts: Opts) -> Mcp {
        std::fs::create_dir_all(data_dir).unwrap();
        let mut c = Command::new(env!("CARGO_BIN_EXE_selachi-agent"));
        c.arg("mcp")
            .env("SELACHI_DATA_DIR", data_dir)
            .env(
                "USERNAME",
                format!("mcp-test-nobody-{}", std::process::id()),
            )
            .env_remove("SELACHI_PARENT_PERMISSION")
            .env_remove("SELACHI_DELEGATION_DEPTH")
            .env_remove("SELACHI_PARENT_THREAD")
            .env_remove("SELACHI_PARENT_TAB")
            .env_remove("SELACHI_CHANNEL")
            .env_remove("SELACHI_AGENT_FAKE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(script) = &opts.script {
            let p = data_dir.join("fake-agent.json");
            std::fs::write(&p, script.to_string()).unwrap();
            c.env("SELACHI_AGENT_FAKE", p);
        }
        for (k, v) in &opts.env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("stdout carried a non-JSON line ({e}): {line:?}"));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Mcp {
            child,
            stdin,
            rx,
            stash: Vec::new(),
            next_id: 1,
        }
    }

    fn write_raw(&mut self, bytes: &[u8]) {
        let s = self.stdin.as_mut().unwrap();
        s.write_all(bytes).unwrap();
        s.flush().unwrap();
    }

    fn send(&mut self, msg: &Value) {
        let mut line = msg.to_string();
        line.push('\n');
        self.write_raw(line.as_bytes());
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Wait for the message for which `pred` holds.
    fn wait_for(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.stash.iter().position(&pred) {
            return self.stash.remove(i);
        }
        let end = Instant::now() + WAIT;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(v) if pred(&v) => return v,
                Ok(v) => self.stash.push(v),
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for {what}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the server closed stdout while waiting for {what}")
                }
            }
        }
    }

    /// Send a request, return the whole response message.
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.wait_for(method, |v| v["id"] == id)
    }

    fn call_raw(&mut self, tool: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": tool, "arguments": args}))
    }

    /// `tools/call` result (asserts it is not a protocol error).
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call_raw(tool, args);
        assert!(r["error"].is_null(), "{tool}: protocol error {r}");
        r["result"].clone()
    }

    fn initialize(&mut self) -> Value {
        let r = self.rpc(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        self.notify("notifications/initialized", json!({}));
        r["result"].clone()
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    fn exit_code(&mut self) -> Option<i32> {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_err(r: &Value) -> bool {
    r["isError"] == true
}

fn text_of(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn data_dir(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("data")
}

fn accept_edits_env() -> Vec<(&'static str, String)> {
    vec![("SELACHI_PARENT_PERMISSION", "accept-edits".to_string())]
}

fn wait_status(m: &mut Mcp, id: &str, want: &str) -> Value {
    let end = Instant::now() + WAIT;
    loop {
        let r = m.call("agent_status", json!({"run_id": id}));
        if r["structuredContent"]["status"] == want {
            return r["structuredContent"].clone();
        }
        assert!(Instant::now() < end, "run {id} never became {want}: {r}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// handshake and protocol
// ---------------------------------------------------------------------------

#[test]
fn legacy_handshake_and_tool_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::start(&data_dir(&dir), Opts::default());
    let init = m.initialize();
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "selachi");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["instructions"]
        .as_str()
        .unwrap()
        .contains("never secrets"));

    let list = m.rpc("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "list_agents",
            "run_agent",
            "agent_status",
            "agent_result",
            "cancel_agent",
            "get_diff",
            "apply_changes"
        ]
    );
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["description"].as_str().unwrap().contains("ES:"));
    }
    let run = tools.iter().find(|t| t["name"] == "run_agent").unwrap();
    assert_eq!(
        run["inputSchema"]["required"],
        json!(["agent", "prompt", "cwd"])
    );
    assert_eq!(
        run["inputSchema"]["properties"]["permission"]["enum"],
        json!(["read_only", "ask", "accept_edits"])
    );
    assert!(
        list["result"].get("resultType").is_none(),
        "legacy results carry no resultType"
    );

    assert_eq!(m.rpc("ping", json!({}))["result"], json!({}));
    // older revisions are echoed back, unknown ones get the newest legacy one
    let old = m.rpc("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
    assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
    let future = m.rpc("initialize", json!({"protocolVersion": "2999-01-01"}));
    assert_eq!(future["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn modern_discover_and_version_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::start(&data_dir(&dir), Opts::default());
    let meta = |v: &str| json!({"_meta": {"io.modelcontextprotocol/protocolVersion": v, "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"}, "io.modelcontextprotocol/clientCapabilities": {}}});
    let d = m.rpc("server/discover", meta("2026-07-28"));
    assert_eq!(d["result"]["resultType"], "complete");
    assert_eq!(d["result"]["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(
        d["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "selachi"
    );
    assert!(d["result"]["capabilities"]["tools"].is_object());

    let l = m.rpc("tools/list", meta("2026-07-28"));
    assert_eq!(l["result"]["resultType"], "complete");
    assert_eq!(l["result"]["tools"].as_array().unwrap().len(), 7);

    let bad = m.rpc("tools/list", meta("1900-01-01"));
    assert_eq!(bad["error"]["code"], -32022);
    assert_eq!(bad["error"]["data"]["requested"], "1900-01-01");
    assert!(bad["error"]["data"]["supported"]
        .as_array()
        .unwrap()
        .contains(&json!("2026-07-28")));

    // a modern tools/call works without any handshake
    let mut p = meta("2026-07-28");
    p["name"] = json!("list_agents");
    p["arguments"] = json!({});
    let c = m.rpc("tools/call", p);
    assert_eq!(c["result"]["resultType"], "complete");
    assert_eq!(c["result"]["isError"], false);
}

#[test]
fn protocol_errors_do_not_kill_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::start(&data_dir(&dir), Opts::default());
    assert_eq!(m.rpc("resources/list", json!({}))["error"]["code"], -32601);
    assert_eq!(m.rpc("prompts/list", json!({}))["error"]["code"], -32601);
    let unknown = m.call_raw("rm_rf", json!({}));
    assert_eq!(unknown["error"]["code"], -32602);
    assert!(unknown["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));
    assert_eq!(m.rpc("tools/call", json!({}))["error"]["code"], -32602);

    // garbage, a batch, a bad id: errors with id null, never a crash
    m.write_raw(b"this is not json\n");
    let e = m.wait_for("parse error", |v| v["error"]["code"] == -32700);
    assert!(e["id"].is_null());
    m.write_raw(b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n");
    m.wait_for("batch error", |v| v["error"]["code"] == -32600);
    m.write_raw(b"{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"ping\"}\n");
    m.wait_for("bad id", |v| v["error"]["code"] == -32600);
    m.write_raw(&[0xff, 0xfe, b'\n']);
    m.wait_for("utf8 error", |v| v["error"]["code"] == -32700);
    // notifications never get an answer; the next request does
    m.notify("notifications/whatever", json!({}));
    assert_eq!(m.rpc("ping", json!({}))["result"], json!({}));
}

// ---------------------------------------------------------------------------
// framing
// ---------------------------------------------------------------------------

#[test]
fn partial_lines_crlf_and_blank_lines() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::start(&data_dir(&dir), Opts::default());
    // one request delivered in three pieces with pauses in between
    let req = json!({"jsonrpc": "2.0", "id": 100, "method": "ping"}).to_string();
    let (a, rest) = req.split_at(7);
    let (b, c) = rest.split_at(11);
    for piece in [a, b] {
        m.write_raw(piece.as_bytes());
        std::thread::sleep(Duration::from_millis(150));
    }
    m.write_raw(format!("{c}\r\n\r\n\n").as_bytes()); // CRLF + blank lines
    let r = m.wait_for("ping 100", |v| v["id"] == 100);
    assert_eq!(r["result"], json!({}));
    // two requests in one write
    m.write_raw(
        format!(
            "{}\n{}\n",
            json!({"jsonrpc":"2.0","id":101,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":102,"method":"tools/list"})
        )
        .as_bytes(),
    );
    m.wait_for("101", |v| v["id"] == 101);
    let l = m.wait_for("102", |v| v["id"] == 102);
    assert_eq!(l["result"]["tools"].as_array().unwrap().len(), 7);
    // a UTF-8 BOM and multibyte text survive
    m.write_raw("\u{feff}".as_bytes());
    let r = m.call("agent_status", json!({"run_id": "ñandú-✓"}));
    assert!(is_err(&r));
}

#[test]
fn large_payloads_in_and_out_and_oversized_messages_are_survivable() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: Some(json!({"reply": "got it", "echo_prompt_len": true})),
            ..Opts::default()
        },
    );

    // a 1 MiB prompt goes through the child's stdin and back
    let n = 1024 * 1024;
    let prompt: String = "lorem ipsum dolor sit amet "
        .repeat(n / 27 + 1)
        .chars()
        .take(n)
        .collect();
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": prompt, "cwd": repo}),
    );
    assert!(!is_err(&r), "{r}");
    assert!(
        r["structuredContent"]["summary"]
            .as_str()
            .unwrap()
            .contains(&format!("prompt_len={n}")),
        "{r}"
    );

    // 9 MiB on one line is over the limit: discarded with an error, the server keeps serving
    let mut big = vec![b'x'; 9 * 1024 * 1024];
    big.push(b'\n');
    m.write_raw(&big);
    let e = m.wait_for("too large", |v| v["error"]["code"] == -32600);
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("larger than"),
        "{e}"
    );
    assert_eq!(m.rpc("ping", json!({}))["result"], json!({}));

    // a big diff comes back whole (under the cap) and the stat stays correct
    let body: String = (0..14_000).map(|i| format!("line {i}\n")).collect();
    let s = setup_free_run(&dir, &body);
    let r = m.call("get_diff", json!({"run_id": s}));
    assert!(
        r["structuredContent"]["diff"].as_str().unwrap().len() > 150_000,
        "{}",
        r["structuredContent"]["diff_stat"]
    );
    assert_eq!(r["structuredContent"]["truncated"], false);
}

/// A finished worktree run (created in-process with a scripted agent) in `dir`'s data folder; returns its id.
fn setup_free_run(dir: &tempfile::TempDir, body: &str) -> String {
    let body = body.to_string();
    let s = setup_in(dir, move |cfg, _, _| {
        std::fs::write(cfg.cwd.join("big.txt"), &body).unwrap();
        vec![working(), text("edited"), usage(10, 20, 0, 0), done()]
    });
    let repo = dir.path().join("proj2");
    if !repo.exists() {
        init_repo(dir.path(), "proj2");
    }
    let h = s.harness(caller(Permission::AcceptEdits), DelegationConfig::default());
    let mut req = RunRequest::new("codex", "edit things", &repo);
    req.permission = Permission::AcceptEdits;
    req.worktree = true;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    r.run_id.unwrap()
}

/// `common::setup` but sharing an existing temp dir (its `data` folder is `dir/data`).
fn setup_in(
    dir: &tempfile::TempDir,
    script: impl Fn(&agents_driver::ThreadConfig, usize, &str) -> Vec<agents_driver::AgentEvent>
        + Send
        + Sync
        + 'static,
) -> SetupIn {
    SetupIn {
        dir: dir.path().to_path_buf(),
        factory: FakeFactory::new(script),
    }
}

struct SetupIn {
    dir: PathBuf,
    factory: std::sync::Arc<FakeFactory>,
}

impl SetupIn {
    fn harness(
        &self,
        caller: agent_harness::Caller,
        cfg: DelegationConfig,
    ) -> agent_harness::Harness {
        agent_harness::Harness::builder()
            .data_dir(self.dir.join("data"))
            .config(cfg)
            .caller(caller)
            .factory(std::sync::Arc::new(FactoryHandle(self.factory.clone())))
            .resolver(std::sync::Arc::new(FakeResolver))
            .usage_probe(std::sync::Arc::new(FixedUsage(Vec::new())))
            .broker(std::sync::Arc::new(agent_harness::DenyAll))
            .build()
            .unwrap()
    }
}

// ---------------------------------------------------------------------------
// tools
// ---------------------------------------------------------------------------

#[test]
fn list_agents_reports_agents_and_the_callers_limits() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            env: vec![
                ("SELACHI_PARENT_PERMISSION", "ask".into()),
                ("SELACHI_DELEGATION_DEPTH", "1".into()),
            ],
            ..Opts::default()
        },
    );
    let r = m.call("list_agents", json!({}));
    assert!(!is_err(&r), "{r}");
    let s = &r["structuredContent"];
    let names: Vec<&str> = s["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["claude", "codex", "opencode", "gemini", "agy"]);
    assert_eq!(s["delegation"]["caller_permission"], "ask");
    assert_eq!(s["delegation"]["effective_max_permission"], "ask");
    assert_eq!(s["delegation"]["caller_depth"], 1);
    assert!(text_of(&r).contains("delegation:"));
}

#[test]
fn run_agent_blocking_then_status_and_result() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: Some(json!({"reply": "the answer is 42", "usage": [100, 50]})),
            ..Opts::default()
        },
    );
    m.initialize();
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "what is the answer?", "cwd": repo}),
    );
    assert!(!is_err(&r), "{r}");
    let s = &r["structuredContent"];
    assert_eq!(s["status"], "done");
    assert_eq!(s["summary"], "the answer is 42");
    assert_eq!(s["usage"]["total"], 150);
    assert_eq!(s["permission"], "read-only");
    assert_eq!(s["async"], false);
    assert!(s["files_changed"].as_array().unwrap().is_empty());
    assert!(text_of(&r).contains("the answer is 42") && text_of(&r).contains("done"));
    let id = s["run_id"].as_str().unwrap().to_string();

    let st = m.call("agent_status", json!({"run_id": id}));
    assert_eq!(st["structuredContent"]["status"], "done");
    assert_eq!(st["structuredContent"]["agent"], "codex");
    assert_eq!(
        st["structuredContent"]["depth"], 1,
        "a top-level caller's run is depth 1"
    );
    let res = m.call("agent_result", json!({"run_id": id}));
    assert!(!is_err(&res));
    assert_eq!(res["structuredContent"]["summary"], "the answer is 42");
    assert_eq!(res["structuredContent"]["run_id"], id.as_str());

    // unknown ids are tool errors with the reason
    let e = m.call("agent_result", json!({"run_id": "99999"}));
    assert!(is_err(&e) && text_of(&e).contains("no run 99999"), "{e}");
    let e = m.call("agent_status", json!({"run_id": "..\\evil"}));
    assert!(is_err(&e) && text_of(&e).contains("not a run id"), "{e}");
}

#[test]
fn worktree_run_diff_and_apply_changes_returns_the_plan_only() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            env: accept_edits_env(),
            script: Some(
                json!({"reply": "edited", "write": {"a.txt": "alpha\nbeta\n", "new.txt": "n\n"}}),
            ),
        },
    );
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "edit", "cwd": repo, "permission": "accept_edits", "worktree": true}),
    );
    assert!(!is_err(&r), "{r}");
    let s = &r["structuredContent"];
    assert_eq!(s["status"], "done");
    assert_eq!(s["permission"], "accept-edits");
    let mut files: Vec<&str> = s["files_changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["a.txt", "new.txt"]);
    assert!(Path::new(s["worktree_path"].as_str().unwrap()).is_dir());
    assert!(text_of(&r).contains("apply_changes"));
    let id = s["run_id"].as_str().unwrap().to_string();

    let d = m.call("get_diff", json!({"run_id": id}));
    assert!(d["structuredContent"]["diff"]
        .as_str()
        .unwrap()
        .contains("+beta"));
    assert_eq!(d["structuredContent"]["diff_stat"]["files"], 2);
    let d = m.call("get_diff", json!({"run_id": id, "stat": true}));
    assert!(d["structuredContent"].get("diff").is_none());
    assert!(text_of(&d).contains("2 files changed"), "{}", text_of(&d));

    // the plan, never the change
    let a = m.call("apply_changes", json!({"run_id": id}));
    assert!(!is_err(&a), "{a}");
    let s = &a["structuredContent"];
    assert_eq!(s["applied"], false);
    assert_eq!(s["confirmation"], "required");
    assert_eq!(s["plan"]["can_apply"], true);
    assert_eq!(s["plan"]["files"].as_array().unwrap().len(), 2);
    assert!(s["how_to_confirm"].as_str().unwrap().contains("apply"));
    let t = text_of(&a);
    assert!(
        t.contains("NOT applied") && t.contains(&format!("apply {id}")),
        "{t}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\n",
        "nothing may be applied silently"
    );
    assert!(!repo.join("new.txt").exists());
    // asking again changes nothing either
    m.call("apply_changes", json!({"run_id": id}));
    assert!(!repo.join("new.txt").exists());
}

#[test]
fn async_run_is_polled_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    // 1) a slow run that finishes
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: Some(json!({"reply": "slow but done", "delay_ms": 1500})),
            ..Opts::default()
        },
    );
    let t0 = Instant::now();
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo, "async": true}),
    );
    assert!(!is_err(&r), "{r}");
    assert_eq!(r["structuredContent"]["status"], "running");
    assert_eq!(r["structuredContent"]["async"], true);
    let id = r["structuredContent"]["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        t0.elapsed() < Duration::from_millis(1400),
        "async must not wait for the agent ({:?})",
        t0.elapsed()
    );
    let st = m.call("agent_status", json!({"run_id": id}));
    assert_eq!(st["structuredContent"]["status"], "running");
    let res = m.call("agent_result", json!({"run_id": id}));
    assert!(!is_err(&res));
    assert_eq!(res["structuredContent"]["status"], "running");
    assert!(text_of(&res).contains("still running"));
    wait_status(&mut m, &id, "done");
    let res = m.call("agent_result", json!({"run_id": id}));
    assert_eq!(res["structuredContent"]["summary"], "slow but done");
    drop(m);

    // 2) one that never answers, cancelled
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: Some(json!({"hang": true})),
            ..Opts::default()
        },
    );
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo, "async": true}),
    );
    let id = r["structuredContent"]["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = m.call("cancel_agent", json!({"run_id": id}));
    assert!(!is_err(&c), "{c}");
    assert_eq!(c["structuredContent"]["outcome"], "cancelled");
    let st = wait_status(&mut m, &id, "cancelled");
    assert_eq!(st["run_id"], id.as_str());
    let c = m.call("cancel_agent", json!({"run_id": id}));
    assert_eq!(c["structuredContent"]["outcome"], "already_finished");
}

#[test]
fn timeouts_and_budgets_come_back_as_results() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: Some(json!({"hang": true})),
            ..Opts::default()
        },
    );
    let r = m.call(
        "run_agent",
        json!({"agent": "claude", "prompt": "x", "cwd": repo, "timeout_s": 1}),
    );
    assert!(is_err(&r), "a timed out run is reported as an error: {r}");
    assert_eq!(r["structuredContent"]["status"], "timed_out");
    assert!(text_of(&r).contains("timed_out"));
    // the finished run is information, not a failure, when read back
    let id = r["structuredContent"]["run_id"].as_str().unwrap();
    assert!(!is_err(&m.call("agent_result", json!({"run_id": id}))));
}

// ---------------------------------------------------------------------------
// refusals and validation
// ---------------------------------------------------------------------------

#[test]
fn refusals_map_to_tool_errors_with_code_and_reason() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let script = Some(json!({"reply": "never runs"}));

    // default caller is read-only: asking for more is refused
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: script.clone(),
            ..Opts::default()
        },
    );
    for (perm, code) in [
        ("accept_edits", "permission_exceeds_parent"),
        ("ask", "permission_exceeds_parent"),
    ] {
        let r = m.call(
            "run_agent",
            json!({"agent": "codex", "prompt": "x", "cwd": repo, "permission": perm}),
        );
        assert!(is_err(&r), "{r}");
        assert_eq!(r["structuredContent"]["status"], "refused");
        assert_eq!(r["structuredContent"]["code"], code);
        assert!(r["structuredContent"]["run_id"].is_null());
        let t = text_of(&r);
        assert!(
            t.starts_with(&format!("refused ({code})")) && t.contains("SELACHI_PARENT_PERMISSION"),
            "{t}"
        );
    }
    // unknown agent
    let r = m.call(
        "run_agent",
        json!({"agent": "gpt", "prompt": "x", "cwd": repo}),
    );
    assert!(is_err(&r));
    assert_eq!(r["structuredContent"]["code"], "unknown_agent");
    // a directory that does not exist
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": dir.path().join("nope")}),
    );
    assert_eq!(r["structuredContent"]["code"], "bad_request");
    // a worktree outside a git repo
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": plain, "worktree": true}),
    );
    assert_eq!(r["structuredContent"]["code"], "not_a_git_repo");
    drop(m);

    // the server's own environment is the caller: a delegated session (depth 1) cannot delegate again
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            env: vec![("SELACHI_DELEGATION_DEPTH", "1".into())],
            script,
        },
    );
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo}),
    );
    assert!(is_err(&r));
    assert_eq!(r["structuredContent"]["code"], "depth_exceeded");
    assert!(text_of(&r).contains("depth"), "{}", text_of(&r));
    // nothing was started
    assert!(Registry::new(&data_dir(&dir)).list().is_empty());
}

#[test]
fn invalid_arguments_are_tool_errors_and_full_is_not_even_expressible() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let mut m = Mcp::start(&data_dir(&dir), Opts::default());
    let check = |m: &mut Mcp, tool: &str, args: Value, needle: &str| {
        let r = m.call(tool, args);
        assert!(is_err(&r), "{tool}: {r}");
        assert!(
            text_of(&r).contains(needle),
            "{tool}: expected `{needle}` in {}",
            text_of(&r)
        );
    };
    check(
        &mut m,
        "run_agent",
        json!({"agent": "codex", "cwd": repo}),
        "`prompt` is required",
    );
    check(
        &mut m,
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo, "permission": "full"}),
        "one of",
    );
    check(
        &mut m,
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo, "timeout_s": 0}),
        "at least 1",
    );
    check(
        &mut m,
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo, "worktree": "yes"}),
        "boolean",
    );
    check(
        &mut m,
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo, "sudo": true}),
        "unknown argument `sudo`",
    );
    check(&mut m, "agent_status", json!({}), "`run_id` is required");
    check(&mut m, "agent_status", json!("12"), "must be an object");
    check(
        &mut m,
        "get_diff",
        json!({"run_id": "1", "stat": "no"}),
        "boolean",
    );
    // null arguments are an empty object
    let r = m.rpc("tools/call", json!({"name": "list_agents"}));
    assert_eq!(r["result"]["isError"], false);
}

// ---------------------------------------------------------------------------
// cancellation and shutdown
// ---------------------------------------------------------------------------

fn wait_for_run(data: &Path, want: RunStatus) -> agent_harness::RunRecord {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(r) = Registry::new(data)
            .list()
            .into_iter()
            .find(|r| r.status == want)
        {
            return r;
        }
        assert!(Instant::now() < end, "no run reached {want:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn cancelled_notification_stops_the_run_and_the_response() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let data = data_dir(&dir);
    let mut m = Mcp::start(
        &data,
        Opts {
            script: Some(json!({"hang": true})),
            ..Opts::default()
        },
    );
    let id = m.next_id;
    m.next_id += 1;
    m.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                   "params": {"name": "run_agent", "arguments": {"agent": "codex", "prompt": "x", "cwd": repo}}}));
    wait_for_run(&data, RunStatus::Running);
    // other calls are served while the blocking one is in flight
    assert_eq!(
        m.rpc("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    m.notify(
        "notifications/cancelled",
        json!({"requestId": id, "reason": "user pressed escape"}),
    );
    let rec = wait_for_run(&data, RunStatus::Cancelled);
    assert_eq!(rec.agent, "codex");
    assert_eq!(m.rpc("ping", json!({}))["result"], json!({}));
    assert!(
        m.stash.iter().all(|v| v["id"] != id),
        "no response may follow a cancelled request: {:?}",
        m.stash
    );
}

#[test]
fn closing_stdin_cancels_blocking_runs_and_exits() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let data = data_dir(&dir);
    let mut m = Mcp::start(
        &data,
        Opts {
            script: Some(json!({"hang": true})),
            ..Opts::default()
        },
    );
    m.send(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                   "params": {"name": "run_agent", "arguments": {"agent": "codex", "prompt": "x", "cwd": repo}}}));
    wait_for_run(&data, RunStatus::Running);
    m.close_stdin();
    assert_eq!(
        m.exit_code(),
        Some(0),
        "the server exits when the client closes stdin"
    );
    wait_for_run(&data, RunStatus::Cancelled);

    // and an idle server exits at once
    let mut m = Mcp::start(&data_dir(&dir), Opts::default());
    m.initialize();
    m.close_stdin();
    assert_eq!(m.exit_code(), Some(0));
}

// ---------------------------------------------------------------------------
// apply_changes with a Selachi window on the approvals pipe
// ---------------------------------------------------------------------------

/// Finished worktree run with the given files; returns (setup, repo, run id).
fn finished_run(files: &[(&str, &str)]) -> (Setup, PathBuf, String) {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let s = setup(move |cfg, _, _| {
        for (path, body) in &files {
            let p = cfg.cwd.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        vec![working(), text("edited"), usage(10, 20, 0, 0), done()]
    });
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), DelegationConfig::default());
    let mut req = RunRequest::new("codex", "edit things", &repo);
    req.permission = Permission::AcceptEdits;
    req.worktree = true;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let id = r.run_id.unwrap();
    drop(h);
    (s, repo, id)
}

/// `USERNAME` that makes the server's approvals pipe the mock's.
fn user_of(server: &MockServer) -> String {
    server
        .path
        .rsplit("selachi-approvals-")
        .next()
        .unwrap()
        .to_string()
}

fn mcp_with_gui(data: &Path, server: &MockServer) -> Mcp {
    Mcp::start(
        data,
        Opts {
            env: vec![("USERNAME", user_of(server))],
            ..Opts::default()
        },
    )
}

#[test]
fn apply_changes_forwards_a_confirmation_to_the_selachi_window() {
    let (s, repo, id) = finished_run(&[("a.txt", "alpha\nbeta\n")]);
    let server = MockServer::answering(agent_harness::Decision::Allow);
    let mut m = mcp_with_gui(&s.data_dir(), &server);
    let a = m.call("apply_changes", json!({"run_id": id}));
    assert!(!is_err(&a), "{a}");
    assert_eq!(a["structuredContent"]["applied"], true, "{a}");
    assert_eq!(a["structuredContent"]["confirmed_by"], "selachi_window");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    let reqs = server.approval_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].tool, "selachi:apply_changes");
    assert_eq!(reqs[0].run_id, id);
    assert!(
        reqs[0].detail.contains("a.txt"),
        "the window must be shown the plan: {}",
        reqs[0].detail
    );
    // applying twice is reported, never repeated
    let again = m.call("apply_changes", json!({"run_id": id}));
    assert_eq!(again["structuredContent"]["applied"], false);
    assert!(text_of(&again).contains("already applied"));
    assert_eq!(server.approval_requests().len(), 1);
}

#[test]
fn apply_changes_denied_in_the_window_changes_nothing() {
    let (s, repo, id) = finished_run(&[("a.txt", "alpha\nbeta\n")]);
    let server = MockServer::answering(agent_harness::Decision::Deny);
    let mut m = mcp_with_gui(&s.data_dir(), &server);
    let a = m.call("apply_changes", json!({"run_id": id}));
    assert!(!is_err(&a), "{a}");
    assert_eq!(a["structuredContent"]["applied"], false);
    assert_eq!(a["structuredContent"]["denied"], true);
    assert!(text_of(&a).contains("NOT applied"));
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\n"
    );
}

#[test]
fn apply_changes_with_a_silent_window_denies_on_timeout() {
    let (s, repo, id) = finished_run(&[("a.txt", "alpha\nbeta\n")]);
    // answers the ping but closes the connection on the approval request without deciding
    let server = MockServer::start(|m| match m {
        agent_harness::ClientMessage::Ping { v } => Some(agent_harness::ServerMessage::Pong {
            v: *v,
            accepts_approvals: true,
        }),
        agent_harness::ClientMessage::ApprovalRequest(_) => None,
    });
    let mut m = mcp_with_gui(&s.data_dir(), &server);
    let a = m.call("apply_changes", json!({"run_id": id}));
    assert_eq!(a["structuredContent"]["applied"], false, "{a}");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\n"
    );
}

#[test]
fn full_risk_changes_are_never_forwarded_to_a_window() {
    let (s, repo, id) = finished_run(&[(".github/workflows/ci.yml", "on: push\n")]);
    let server = MockServer::answering(agent_harness::Decision::Allow);
    let mut m = mcp_with_gui(&s.data_dir(), &server);
    let a = m.call("apply_changes", json!({"run_id": id}));
    assert_eq!(a["structuredContent"]["applied"], false, "{a}");
    assert_eq!(a["structuredContent"]["plan"]["risk"], "full");
    assert!(text_of(&a).contains("terminal"), "{}", text_of(&a));
    assert!(
        server.approval_requests().is_empty(),
        "a full-risk plan must not even be offered to the window"
    );
    assert!(!repo.join(".github").exists());
}

#[test]
fn apply_changes_errors_are_tool_errors() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path(), "proj");
    let mut m = Mcp::start(
        &data_dir(&dir),
        Opts {
            script: Some(json!({"reply": "ok"})),
            ..Opts::default()
        },
    );
    // unknown run
    let a = m.call("apply_changes", json!({"run_id": "31337"}));
    assert!(is_err(&a) && text_of(&a).contains("no run 31337"), "{a}");
    // a run that did not use a worktree has nothing to apply
    let r = m.call(
        "run_agent",
        json!({"agent": "codex", "prompt": "x", "cwd": repo}),
    );
    let id = r["structuredContent"]["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let a = m.call("apply_changes", json!({"run_id": id}));
    assert!(
        is_err(&a) && text_of(&a).contains("did not use --worktree"),
        "{a}"
    );
}
