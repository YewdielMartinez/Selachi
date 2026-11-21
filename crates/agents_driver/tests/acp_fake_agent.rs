//! End-to-end tests of the ACP client against a scripted fake agent.
//!
//! The fake agent is this very test binary re-spawned with `--exact fake_agent_main`
//! and `FAKE_ACP_AGENT=1` (see [`fake_agent_main`]); it speaks real newline-delimited
//! JSON-RPC over stdio, so the whole process driver (spawn, reader threads, coalescer,
//! stdin writer, shutdown) is exercised, not only the state machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agents_driver::acp::{AcpCommand, AcpDriver};
use agents_driver::*;
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// The fake agent
// ---------------------------------------------------------------------------

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    writeln!(o, "{v}").unwrap();
    o.flush().unwrap();
}

fn update(sid: &str, u: Value) {
    out(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":u}}));
}

fn chunk(sid: &str, text: &str) {
    update(sid, json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}));
}

/// Read stdin lines until a message satisfying `pred` arrives.
fn read_until(lines: &mut impl Iterator<Item = String>, pred: impl Fn(&Value) -> bool) -> Option<Value> {
    for l in lines {
        if let Ok(v) = serde_json::from_str::<Value>(&l) {
            if pred(&v) {
                return Some(v);
            }
        }
    }
    None
}

/// Not a real test: when `FAKE_ACP_AGENT=1` it turns the process into a scripted ACP agent.
#[test]
fn fake_agent_main() {
    if std::env::var("FAKE_ACP_AGENT").as_deref() != Ok("1") {
        return;
    }
    // libtest printed `test fake_agent_main ... ` without a newline: end that line so the first
    // JSON message starts on a fresh one.
    out(Value::Null);
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines().map_while(std::result::Result::ok);
    let mut cwd = String::new();
    let mut model = "m1".to_string();
    let sid = "fake-session";
    while let Some(line) = lines.next() {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let id = msg.get("id").cloned();
        match (msg["method"].as_str(), id) {
            (Some("initialize"), Some(id)) => out(json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":1,
                "agentCapabilities":{"loadSession":false,"promptCapabilities":{}},
                "authMethods":[],
                "agentInfo":{"name":"fake","version":"0"}}})),
            (Some("session/new"), Some(id)) => {
                cwd = msg["params"]["cwd"].as_str().unwrap().to_string();
                out(json!({"jsonrpc":"2.0","id":id,"result":{
                    "sessionId":sid,
                    "configOptions":[{"id":"model","currentValue":"m1","options":[]}],
                    "models":{"currentModelId":"m1","availableModels":[]}}}));
            }
            (Some("session/set_config_option"), Some(id)) => {
                model = msg["params"]["value"].as_str().unwrap().to_string();
                out(json!({"jsonrpc":"2.0","id":id,"result":{"configOptions":[]}}));
            }
            (Some("session/prompt"), Some(id)) => {
                let text = msg["params"]["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                let mut stop = "end_turn";
                match text.as_str() {
                    "hello" => {
                        update(sid, json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}));
                        for w in ["Hello", " ", "world"] {
                            chunk(sid, w);
                        }
                        update(sid, json!({"sessionUpdate":"plan","entries":[
                            {"content":"step one","priority":"high","status":"completed"},
                            {"content":"step two","priority":"high","status":"pending"}]}));
                        update(sid, json!({"sessionUpdate":"usage_update","used":10,"size":1000,"cost":{"amount":0.5,"currency":"USD"}}));
                    }
                    "tool" => {
                        update(sid, json!({"sessionUpdate":"tool_call","toolCallId":"c1","title":"read","kind":"read","status":"pending","rawInput":{"filePath":"a.txt"}}));
                        update(sid, json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"in_progress"}));
                        update(sid, json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed",
                            "content":[{"type":"content","content":{"type":"text","text":"file contents"}}]}));
                        update(sid, json!({"sessionUpdate":"tool_call","toolCallId":"c2","title":"bash","kind":"execute","status":"pending"}));
                        update(sid, json!({"sessionUpdate":"tool_call_update","toolCallId":"c2","status":"failed","rawOutput":{"error":"exit 1"}}));
                    }
                    "perm" => {
                        out(json!({"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{
                            "sessionId":sid,
                            "toolCall":{"toolCallId":"p1","title":"bash","kind":"execute","rawInput":{"command":"cargo build"}},
                            "options":[
                                {"optionId":"once","kind":"allow_once","name":"Allow once"},
                                {"optionId":"always","kind":"allow_always","name":"Always allow"},
                                {"optionId":"reject","kind":"reject_once","name":"Reject"}]}}));
                        let resp = read_until(&mut lines, |v| v["id"] == 100 && v.get("method").is_none()).unwrap();
                        let outcome = &resp["result"]["outcome"];
                        let chosen = outcome["optionId"].as_str().unwrap_or(outcome["outcome"].as_str().unwrap_or("?"));
                        chunk(sid, &format!("chosen:{chosen}"));
                        let ok = chosen != "reject" && chosen != "cancelled";
                        update(sid, json!({"sessionUpdate":"tool_call_update","toolCallId":"p1","title":"bash",
                            "kind":"execute","status": if ok {"completed"} else {"failed"}}));
                    }
                    "fsread" => {
                        out(json!({"jsonrpc":"2.0","id":200,"method":"fs/read_text_file","params":{
                            "sessionId":sid,"path":Path::new(&cwd).join("hello.txt")}}));
                        let r = read_until(&mut lines, |v| v["id"] == 200).unwrap();
                        chunk(sid, &format!("inside:{}", r["result"]["content"].as_str().unwrap_or("ERR")));
                        let outside = Path::new(&cwd).parent().unwrap().join("outside-secret.txt");
                        out(json!({"jsonrpc":"2.0","id":201,"method":"fs/read_text_file","params":{
                            "sessionId":sid,"path":outside}}));
                        let r = read_until(&mut lines, |v| v["id"] == 201).unwrap();
                        chunk(sid, &format!("outside:{}", if r.get("error").is_some() { "denied" } else { "LEAKED" }));
                    }
                    "fswrite" => {
                        out(json!({"jsonrpc":"2.0","id":300,"method":"fs/write_text_file","params":{
                            "sessionId":sid,"path":Path::new(&cwd).join("written.txt"),"content":"from agent"}}));
                        let r = read_until(&mut lines, |v| v["id"] == 300).unwrap();
                        chunk(sid, &format!("write:{}", if r.get("error").is_some() { "denied" } else { "ok" }));
                    }
                    "model" => chunk(sid, &format!("model:{model}")),
                    "slow" => {
                        chunk(sid, "working");
                        let cancelled = read_until(&mut lines, |v| v["method"] == "session/cancel");
                        assert!(cancelled.is_some());
                        stop = "cancelled";
                    }
                    other => chunk(sid, &format!("echo:{other}")),
                }
                let usage = if text == "hello" {
                    json!({"totalTokens":40,"inputTokens":10,"outputTokens":20,"thoughtTokens":5,"cachedReadTokens":4,"cachedWriteTokens":1})
                } else {
                    Value::Null
                };
                let mut result = json!({"stopReason":stop});
                if !usage.is_null() {
                    result["usage"] = usage;
                }
                out(json!({"jsonrpc":"2.0","id":id,"result":result}));
            }
            (Some(other), Some(id)) => out(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("no {other}")}})),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn fake_command() -> AcpCommand {
    let exe = std::env::current_exe().unwrap();
    let mut c = AcpCommand::new(
        exe,
        vec!["--exact".into(), "fake_agent_main".into(), "--nocapture".into(), "--test-threads=1".into()],
    );
    c.env.push(("FAKE_ACP_AGENT".into(), "1".into()));
    c.label = "fake-acp".into();
    c
}

fn workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("agents_driver_acp_e2e_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("hello.txt"), "hi there").unwrap();
    std::fs::write(root.join("outside-secret.txt"), "secret").unwrap();
    ws
}

fn start(cfg: &ThreadConfig) -> (AcpDriver, UnboundedReceiver<AgentEvent>) {
    let (tx, rx) = unbounded();
    (AcpDriver::spawn(&fake_command(), cfg, tx).unwrap(), rx)
}

/// Collect events until `stop` matches one (inclusive); panics after 20 s.
fn collect_until(rx: &mut UnboundedReceiver<AgentEvent>, stop: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut v = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ev) => {
                let done = stop(&ev);
                v.push(ev);
                if done {
                    return v;
                }
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "timed out; events so far: {v:#?}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn is_done(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::Status(AgentStatus::Done))
}

fn text_of(evs: &[AgentEvent]) -> String {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn handshake_streaming_plan_and_usage() {
    let ws = workspace("stream");
    let (mut d, mut rx) = start(&ThreadConfig::new(&ws));
    d.send("hello").unwrap();
    let evs = collect_until(&mut rx, is_done);

    assert!(evs.iter().any(|e| matches!(e, AgentEvent::SessionStarted { session_id, model }
        if session_id == "fake-session" && model.as_deref() == Some("m1"))), "{evs:#?}");
    assert!(evs.contains(&AgentEvent::Status(AgentStatus::Working)));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Thinking { text, .. } if text == "thinking")));
    assert_eq!(text_of(&evs), "Hello world");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Plan { tasks }
        if tasks == &vec![("step one".to_string(), TaskState::Done), ("step two".to_string(), TaskState::Pending)])));
    let usage = evs.iter().find_map(|e| match e {
        AgentEvent::Usage { input, output, cache_create, cache_read, cost_usd, context_window } => {
            Some((*input, *output, *cache_create, *cache_read, *cost_usd, *context_window))
        }
        _ => None,
    });
    assert_eq!(usage, Some((10, 25, 1, 4, Some(0.5), Some(1000))));
    // usage precedes Done (the UI sums usage per turn)
    let ui = evs.iter().position(|e| matches!(e, AgentEvent::Usage { .. })).unwrap();
    assert_eq!(ui, evs.len() - 2);
    assert_eq!(d.session_id().as_deref(), Some("fake-session"));
    d.shutdown();
    let tail = collect_until(&mut rx, |e| matches!(e, AgentEvent::Exited { .. }));
    assert!(matches!(tail.last(), Some(AgentEvent::Exited { .. })));
}

#[test]
fn tool_calls_map_to_call_and_result_events() {
    let ws = workspace("tools");
    let (mut d, mut rx) = start(&ThreadConfig::new(&ws));
    d.send("tool").unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert!(evs.contains(&AgentEvent::ToolCall {
        id: "c1".into(), kind: ToolKind::Read, title: "read".into(), input_json: json!({"filePath":"a.txt"})
    }));
    assert!(evs.contains(&AgentEvent::ToolResult { id: "c1".into(), ok: true, summary: "file contents".into() }));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolCall { id, kind: ToolKind::Command, .. } if id == "c2")));
    assert!(evs.contains(&AgentEvent::ToolResult { id: "c2".into(), ok: false, summary: "exit 1".into() }));
    d.shutdown();
}

fn approval(evs: &[AgentEvent]) -> (String, String, String) {
    evs.iter()
        .find_map(|e| match e {
            AgentEvent::ApprovalRequest { id, tool, detail } => Some((id.clone(), tool.clone(), detail.clone())),
            _ => None,
        })
        .expect("approval request")
}

#[test]
fn permission_round_trip_allow_and_deny() {
    let ws = workspace("perm");
    let (mut d, mut rx) = start(&ThreadConfig::new(&ws));

    d.send("perm").unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, AgentEvent::Status(AgentStatus::WaitingForInput)));
    let (id, tool, detail) = approval(&evs);
    assert_eq!((tool.as_str(), detail.as_str()), ("bash", "bash: cargo build"));
    d.approve(&id, true).unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert_eq!(text_of(&evs), "chosen:once");
    assert!(evs.contains(&AgentEvent::Status(AgentStatus::Working)));
    assert!(d.approve(&id, true).is_err(), "already answered");

    d.send("perm").unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, AgentEvent::Status(AgentStatus::WaitingForInput)));
    let (id, ..) = approval(&evs);
    d.approve(&id, false).unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert_eq!(text_of(&evs), "chosen:reject");

    d.send("perm").unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, AgentEvent::Status(AgentStatus::WaitingForInput)));
    let (id, ..) = approval(&evs);
    d.approve_always(&id).unwrap();
    assert_eq!(text_of(&collect_until(&mut rx, is_done)), "chosen:always");
    d.shutdown();
}

#[test]
fn full_mode_answers_permissions_without_asking() {
    let ws = workspace("full");
    let mut cfg = ThreadConfig::new(&ws);
    cfg.permission_mode = PermissionMode::Full;
    let (mut d, mut rx) = start(&cfg);
    d.send("perm").unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ApprovalRequest { .. })));
    assert_eq!(text_of(&evs), "chosen:once");
    d.shutdown();
}

#[test]
fn cancel_ends_the_turn_and_keeps_the_session_alive() {
    let ws = workspace("cancel");
    let (mut d, mut rx) = start(&ThreadConfig::new(&ws));
    d.send("slow").unwrap();
    collect_until(&mut rx, |e| matches!(e, AgentEvent::Text { text, .. } if text.contains("working")));
    d.interrupt().unwrap();
    collect_until(&mut rx, is_done);
    d.send("hello").unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert_eq!(text_of(&evs), "Hello world");
    d.shutdown();
}

#[test]
fn client_fs_read_is_sandboxed_and_write_needs_approval() {
    let ws = workspace("fs");
    let (mut d, mut rx) = start(&ThreadConfig::new(&ws));

    d.send("fsread").unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert_eq!(text_of(&evs), "inside:hi thereoutside:denied");

    d.send("fswrite").unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, AgentEvent::Status(AgentStatus::WaitingForInput)));
    let (id, tool, detail) = approval(&evs);
    assert_eq!(tool, "write_text_file");
    assert!(detail.ends_with("written.txt"), "{detail}");
    assert!(!ws.join("written.txt").exists(), "nothing written before approval");
    d.approve(&id, true).unwrap();
    assert_eq!(text_of(&collect_until(&mut rx, is_done)), "write:ok");
    assert_eq!(std::fs::read_to_string(ws.join("written.txt")).unwrap(), "from agent");

    // denied write
    std::fs::remove_file(ws.join("written.txt")).unwrap();
    d.send("fswrite").unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, AgentEvent::Status(AgentStatus::WaitingForInput)));
    let (id, ..) = approval(&evs);
    d.approve(&id, false).unwrap();
    assert_eq!(text_of(&collect_until(&mut rx, is_done)), "write:denied");
    assert!(!ws.join("written.txt").exists());
    d.shutdown();
}

#[test]
fn configured_model_is_selected_before_the_first_prompt() {
    let ws = workspace("model");
    let mut cfg = ThreadConfig::new(&ws);
    cfg.model = Some("provider/m2".into());
    let (mut d, mut rx) = start(&cfg);
    d.send("model").unwrap(); // sent right away: queued until setup finished
    let evs = collect_until(&mut rx, is_done);
    assert_eq!(text_of(&evs), "model:provider/m2");
    d.shutdown();
}

#[test]
fn messages_sent_back_to_back_run_in_order() {
    let ws = workspace("queue");
    let (mut d, mut rx) = start(&ThreadConfig::new(&ws));
    d.send("one").unwrap();
    d.send("two").unwrap();
    let evs = collect_until(&mut rx, is_done);
    assert_eq!(text_of(&evs), "echo:oneecho:two");
    d.shutdown();
}

#[test]
fn command_line_parsing_resolves_quoted_program() {
    let exe = std::env::current_exe().unwrap();
    let mut cfg = ThreadConfig::new(".");
    cfg.extra_args = vec!["--extra".into()];
    let c = AcpCommand::from_command_line(&format!("\"{}\" --flag \"two words\"", exe.display()), &cfg).unwrap();
    assert_eq!(c.program, exe);
    assert_eq!(c.args, vec!["--flag", "two words", "--extra"]);
    assert!(matches!(AcpCommand::from_command_line("no-such-acp-binary-xyz", &cfg), Err(DriverError::NotFound(_))));
}
