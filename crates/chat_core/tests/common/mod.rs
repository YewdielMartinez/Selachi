#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Test doubles: a scripted agent driver (no processes, no models) and recording process hooks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agents_driver::acp::AcpSession;
use agents_driver::claude::ClaudeParser;
use agents_driver::codex::CodexSession;
use agents_driver::{AgentEvent, AgentKind, DriverError, ThreadConfig};
use chat_core::{
    AgentControl, ApprovalDecision, ChatStore, DriverFactory, JobHandle, OwnerInfo, ProcessHooks, SpawnedAgent,
    ThreadId,
};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use parking_lot::Mutex;
use serde_json::{json, Value};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../agents_driver/tests/fixtures");

pub fn fixture_lines(name: &str) -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURES}/{name}"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Events the real Claude parser produces for a recorded stream.
pub fn claude_events(name: &str) -> Vec<AgentEvent> {
    let mut p = ClaudeParser::default();
    fixture_lines(name).iter().flat_map(|l| p.parse_line(l)).collect()
}

/// Events the real Codex state machine produces for the recorded `codex_basic.jsonl`. The
/// fixture was recorded with its own request ids, so responses are re-addressed to the ids the
/// session actually used (matched by method).
pub fn codex_events(name: &str) -> Vec<AgentEvent> {
    let mut s = CodexSession::new(&ThreadConfig::new("."));
    let mut ids: HashMap<String, i64> = HashMap::new();
    let mut events = Vec::new();
    let track = |ids: &mut HashMap<String, i64>, out: &[Value]| {
        for m in out {
            if let (Some(method), Some(id)) = (m["method"].as_str(), m["id"].as_i64()) {
                ids.insert(method.to_string(), id);
            }
        }
    };
    let first = s.start();
    track(&mut ids, &first);
    let step = s.user_message("Reply with exactly: ok");
    events.extend(step.events);
    track(&mut ids, &step.outgoing);
    for line in fixture_lines(name) {
        let mut v: Value = serde_json::from_str(&line).unwrap();
        if v.get("method").is_none() {
            let fixture_id = v["id"].as_i64();
            let method = match fixture_id {
                Some(0) => Some("initialize"),
                Some(2) => Some("account/rateLimits/read"),
                Some(3) => Some("thread/start"),
                Some(4) => Some("turn/start"),
                _ => None,
            };
            let Some(real) = method.and_then(|m| ids.get(m)) else { continue };
            v["id"] = json!(real);
        }
        let step = s.on_message(&v);
        track(&mut ids, &step.outgoing);
        events.extend(step.events);
    }
    events
}

/// `SessionStarted` produced by the real ACP client for the recorded OpenCode handshake.
pub fn opencode_handshake_events() -> Vec<AgentEvent> {
    let mut sess = AcpSession::new(&ThreadConfig::new("."));
    sess.start();
    let mut events = Vec::new();
    for line in fixture_lines("opencode_acp_handshake.jsonl") {
        let v: Value = serde_json::from_str(&line).unwrap();
        events.extend(sess.on_message(&v).events);
    }
    events
}

// ---------------------------------------------------------------------------
// Fake agent
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Rec {
    pub spawns: Vec<(AgentKind, ThreadConfig, Option<JobHandle>)>,
    pub sent: Vec<String>,
    pub approvals: Vec<(String, ApprovalDecision)>,
    pub interrupts: usize,
    pub shutdowns: usize,
    txs: Vec<UnboundedSender<AgentEvent>>,
}

pub type Script = Box<dyn Fn(usize, &str) -> Vec<AgentEvent> + Send + Sync>;

pub struct FakeFactory {
    pub rec: Arc<Mutex<Rec>>,
    script: Arc<Script>,
    pub fail_spawn: AtomicBool,
    /// Emit these when `interrupt` is called.
    pub on_interrupt: Mutex<Vec<AgentEvent>>,
}

impl FakeFactory {
    pub fn new(script: impl Fn(usize, &str) -> Vec<AgentEvent> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            rec: Arc::new(Mutex::new(Rec::default())),
            script: Arc::new(Box::new(script)),
            fail_spawn: AtomicBool::new(false),
            on_interrupt: Mutex::new(Vec::new()),
        })
    }

    /// Push events into the most recently started agent, as if it produced them now.
    pub fn push(&self, events: Vec<AgentEvent>) {
        let rec = self.rec.lock();
        let tx = rec.txs.last().expect("no agent running");
        for e in events {
            tx.unbounded_send(e).unwrap();
        }
    }

    pub fn sent(&self) -> Vec<String> {
        self.rec.lock().sent.clone()
    }

    pub fn wait(&self, what: &str, mut pred: impl FnMut(&Rec) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if pred(&self.rec.lock()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for: {what}");
    }
}

struct FakeControl {
    rec: Arc<Mutex<Rec>>,
    script: Arc<Script>,
    tx: UnboundedSender<AgentEvent>,
    owner: Arc<FakeFactory>,
}

impl AgentControl for FakeControl {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        let n = {
            let mut r = self.rec.lock();
            r.sent.push(text.to_string());
            r.sent.len() - 1
        };
        for e in (self.script)(n, text) {
            self.tx.unbounded_send(e).map_err(|_| DriverError::Closed)?;
        }
        Ok(())
    }

    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.rec.lock().interrupts += 1;
        for e in self.owner.on_interrupt.lock().clone() {
            let _ = self.tx.unbounded_send(e);
        }
        Ok(())
    }

    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()> {
        self.rec.lock().approvals.push((id.to_string(), decision));
        Ok(())
    }

    fn shutdown(&mut self) {
        self.rec.lock().shutdowns += 1;
        // Real drivers announce the end of the stream.
        let _ = self.tx.unbounded_send(AgentEvent::Exited { code: Some(0) });
    }
}

/// Wrapper so the control can reach the factory's `on_interrupt`.
pub struct FactoryHandle(pub Arc<FakeFactory>);

impl DriverFactory for FactoryHandle {
    fn spawn(&self, kind: &AgentKind, cfg: &ThreadConfig, job: Option<JobHandle>) -> agents_driver::Result<SpawnedAgent> {
        let f = &self.0;
        if f.fail_spawn.load(Ordering::SeqCst) {
            return Err(DriverError::NotFound("claude".into()));
        }
        let (tx, events) = unbounded();
        {
            let mut r = f.rec.lock();
            r.spawns.push((kind.clone(), cfg.clone(), job));
            r.txs.push(tx.clone());
        }
        Ok(SpawnedAgent {
            control: Box::new(FakeControl { rec: f.rec.clone(), script: f.script.clone(), tx, owner: f.clone() }),
            events,
            pid: Some(4242),
        })
    }
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct FakeHooks {
    pub opened: Mutex<Vec<(ThreadId, OwnerInfo)>>,
    pub roots: Mutex<Vec<(ThreadId, u32)>>,
    pub closed: Mutex<Vec<ThreadId>>,
}

impl ProcessHooks for FakeHooks {
    fn open_owner(&self, id: ThreadId, info: &OwnerInfo) -> Option<JobHandle> {
        self.opened.lock().push((id, info.clone()));
        Some(777)
    }
    fn register_root(&self, id: ThreadId, pid: u32) {
        self.roots.lock().push((id, pid));
    }
    fn close_owner(&self, id: ThreadId) {
        self.closed.lock().push(id);
    }
}

pub fn store_at(dir: &Path, factory: &Arc<FakeFactory>, hooks: Option<Arc<FakeHooks>>) -> ChatStore {
    let mut b = ChatStore::builder().path(dir.join("chat.db")).factory(Arc::new(FactoryHandle(factory.clone())));
    if let Some(h) = hooks {
        b = b.hooks(h);
    }
    b.open().unwrap()
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with one commit at `<tmp>/<name>` (so sibling worktrees land in the temp dir).
pub fn init_repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Test User"]);
    git(&root, &["config", "user.email", "test@example.com"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    git(&root, &["add", "a.txt"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}
