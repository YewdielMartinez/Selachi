#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Test doubles: a scripted agent (no processes, no models; copied from `chat_core`'s tests and
//! extended so the script sees the thread config and approvals can drive it), a fake resolver,
//! git helpers and a mock Selachi approvals server on a real named pipe.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_harness::{
    AgentResolver, ApprovalRequest, Caller, ClientMessage, DelegationConfig, Harness, Permission, Refusal, RefusalCode,
    ServerMessage, UsageProbe,
};
use agents_driver::{AgentEvent, AgentKind, DriverError, ThreadConfig};
use chat_core::{AgentControl, ApprovalDecision, DriverFactory, JobHandle, SpawnedAgent};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use parking_lot::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

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

pub type Script = Box<dyn Fn(&ThreadConfig, usize, &str) -> Vec<AgentEvent> + Send + Sync>;
pub type OnApprove = Box<dyn Fn(&str, ApprovalDecision) -> Vec<AgentEvent> + Send + Sync>;

pub struct FakeFactory {
    pub rec: Arc<Mutex<Rec>>,
    script: Script,
    pub on_approve: Mutex<Option<OnApprove>>,
    pub fail_spawn: AtomicBool,
}

impl FakeFactory {
    pub fn new(script: impl Fn(&ThreadConfig, usize, &str) -> Vec<AgentEvent> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            rec: Arc::new(Mutex::new(Rec::default())),
            script: Box::new(script),
            on_approve: Mutex::new(None),
            fail_spawn: AtomicBool::new(false),
        })
    }

    pub fn on_approve(&self, f: impl Fn(&str, ApprovalDecision) -> Vec<AgentEvent> + Send + Sync + 'static) {
        *self.on_approve.lock() = Some(Box::new(f));
    }

    pub fn sent(&self) -> Vec<String> {
        self.rec.lock().sent.clone()
    }

    pub fn spawns(&self) -> usize {
        self.rec.lock().spawns.len()
    }
}

struct FakeControl {
    f: Arc<FakeFactory>,
    cfg: ThreadConfig,
    tx: UnboundedSender<AgentEvent>,
}

impl AgentControl for FakeControl {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        let n = {
            let mut r = self.f.rec.lock();
            r.sent.push(text.to_string());
            r.sent.len() - 1
        };
        for e in (self.f.script)(&self.cfg, n, text) {
            self.tx.unbounded_send(e).map_err(|_| DriverError::Closed)?;
        }
        Ok(())
    }

    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.f.rec.lock().interrupts += 1;
        Ok(())
    }

    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()> {
        self.f.rec.lock().approvals.push((id.to_string(), decision));
        let events = self.f.on_approve.lock().as_ref().map(|f| f(id, decision)).unwrap_or_default();
        for e in events {
            let _ = self.tx.unbounded_send(e);
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        self.f.rec.lock().shutdowns += 1;
        let _ = self.tx.unbounded_send(AgentEvent::Exited { code: Some(0) });
    }
}

/// `Arc<FakeFactory>` as a `DriverFactory`.
pub struct FactoryHandle(pub Arc<FakeFactory>);

impl DriverFactory for FactoryHandle {
    fn spawn(&self, kind: &AgentKind, cfg: &ThreadConfig, job: Option<JobHandle>) -> agents_driver::Result<SpawnedAgent> {
        let f = &self.0;
        if f.fail_spawn.load(Ordering::SeqCst) {
            return Err(DriverError::NotFound("codex".into()));
        }
        let (tx, events) = unbounded();
        {
            let mut r = f.rec.lock();
            r.spawns.push((kind.clone(), cfg.clone(), job));
            r.txs.push(tx.clone());
        }
        Ok(SpawnedAgent { control: Box::new(FakeControl { f: f.clone(), cfg: cfg.clone(), tx }), events, pid: Some(4242) })
    }
}

pub fn working() -> AgentEvent {
    AgentEvent::Status(agents_driver::AgentStatus::Working)
}
pub fn done() -> AgentEvent {
    AgentEvent::Status(agents_driver::AgentStatus::Done)
}
pub fn text(t: &str) -> AgentEvent {
    AgentEvent::Text { text: t.into(), delta: false }
}
pub fn usage(input: u64, output: u64, cache_create: u64, cache_read: u64) -> AgentEvent {
    AgentEvent::Usage { input, output, cache_create, cache_read, cost_usd: None, context_window: None }
}

// ---------------------------------------------------------------------------
// Resolver / usage probe / harness
// ---------------------------------------------------------------------------

/// Maps the known names to kinds without looking at the machine.
pub struct FakeResolver;

impl AgentResolver for FakeResolver {
    fn resolve(&self, name: &str) -> Result<AgentKind, Refusal> {
        match agent_harness::canonical_name(name) {
            Some("claude") => Ok(AgentKind::ClaudeCode),
            Some("codex") => Ok(AgentKind::Codex),
            Some("opencode") => Ok(AgentKind::OpenCode),
            Some(other) => Ok(AgentKind::Acp { command: format!("{other} --acp") }),
            None => Err(Refusal::new(RefusalCode::UnknownAgent, format!("unknown agent {name}"))),
        }
    }
}

pub struct FixedUsage(pub Vec<agent_harness::LimitRow>);

impl UsageProbe for FixedUsage {
    fn limits(&self, _now_ms: i64) -> Vec<agent_harness::LimitRow> {
        self.0.clone()
    }
}

pub fn caller(permission: Permission) -> Caller {
    Caller { permission, ..Caller::default() }
}

pub struct Setup {
    pub dir: tempfile::TempDir,
    pub factory: Arc<FakeFactory>,
}

impl Setup {
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// A harness over this setup's data dir with the fakes injected.
    pub fn harness(&self, caller: Caller, cfg: DelegationConfig) -> Harness {
        self.harness_with(caller, cfg, Arc::new(agent_harness::DenyAll))
    }

    pub fn harness_with(&self, caller: Caller, cfg: DelegationConfig, broker: Arc<dyn agent_harness::ApprovalBroker>) -> Harness {
        self.harness_full(caller, cfg, broker, Vec::new())
    }

    pub fn harness_full(
        &self,
        caller: Caller,
        cfg: DelegationConfig,
        broker: Arc<dyn agent_harness::ApprovalBroker>,
        limits: Vec<agent_harness::LimitRow>,
    ) -> Harness {
        Harness::builder()
            .data_dir(self.data_dir())
            .config(cfg)
            .caller(caller)
            .factory(Arc::new(FactoryHandle(self.factory.clone())))
            .resolver(Arc::new(FakeResolver))
            .usage_probe(Arc::new(FixedUsage(limits)))
            .broker(broker)
            .build()
            .unwrap()
    }
}

pub fn setup(script: impl Fn(&ThreadConfig, usize, &str) -> Vec<AgentEvent> + Send + Sync + 'static) -> Setup {
    Setup { dir: tempfile::tempdir().unwrap(), factory: FakeFactory::new(script) }
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

pub fn wait_until(what: &str, mut pred: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if pred() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

// ---------------------------------------------------------------------------
// Mock Selachi approvals server (a real named pipe)
// ---------------------------------------------------------------------------

static PIPE_SEQ: AtomicUsize = AtomicUsize::new(0);

pub struct MockServer {
    pub path: String,
    /// Every message received, in order.
    pub seen: Arc<Mutex<Vec<ClientMessage>>>,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl MockServer {
    /// Serve `handler` (its `None` = close without answering). One request per connection.
    pub fn start(handler: impl Fn(&ClientMessage) -> Option<ServerMessage> + Send + Sync + 'static) -> MockServer {
        let path = format!(r"\\.\pipe\selachi-approvals-test-{}-{}", std::process::id(), PIPE_SEQ.fetch_add(1, Ordering::SeqCst));
        let seen: Arc<Mutex<Vec<ClientMessage>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let create = |wide: &[u16]| unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                8,
                64 * 1024,
                64 * 1024,
                0,
                None,
            )
        };
        // The first instance exists before `start` returns, so a client can never race the server.
        let first = create(&wide);
        assert!(!first.is_invalid(), "cannot create the test pipe {path}");
        let first_raw = first.0 as usize;
        let accept = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut next = Some(first_raw);
                loop {
                    let raw = match next.take() {
                        Some(r) => r,
                        None => {
                            let h = create(&wide);
                            if h.is_invalid() {
                                return;
                            }
                            h.0 as usize
                        }
                    };
                    let h = HANDLE(raw as *mut std::ffi::c_void);
                    // ERROR_PIPE_CONNECTED (a client got in between create and connect) is fine too.
                    let _ = unsafe { ConnectNamedPipe(h, None) };
                    if stop.load(Ordering::SeqCst) {
                        unsafe {
                            let _ = DisconnectNamedPipe(h);
                            drop(File::from_raw_handle(h.0 as RawHandle));
                        }
                        return;
                    }
                    let (handler, seen) = (handler.clone(), seen.clone());
                    std::thread::spawn(move || {
                        let handle = HANDLE(raw as *mut std::ffi::c_void);
                        let file = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
                        let mut line = String::new();
                        if BufReader::new(&file).read_line(&mut line).is_ok() {
                            if let Ok(msg) = serde_json::from_str::<ClientMessage>(line.trim()) {
                                seen.lock().push(msg.clone());
                                if let Some(reply) = handler(&msg) {
                                    let mut out = serde_json::to_string(&reply).unwrap();
                                    out.push('\n');
                                    let mut w = &file;
                                    let _ = w.write_all(out.as_bytes());
                                }
                            }
                        }
                        unsafe {
                            let _ = FlushFileBuffers(handle);
                            let _ = DisconnectNamedPipe(handle);
                        }
                        drop(file);
                    });
                }
            })
        };
        MockServer { path, seen, stop, accept: Some(accept) }
    }

    /// Answers every approval with `decision` and every ping with a pong.
    pub fn answering(decision: agent_harness::Decision) -> MockServer {
        MockServer::start(move |m| match m {
            ClientMessage::Ping { v } => Some(ServerMessage::Pong { v: *v, accepts_approvals: true }),
            ClientMessage::ApprovalRequest(r) => Some(ServerMessage::ApprovalResponse {
                v: 1,
                request_id: r.request_id.clone(),
                decision,
                reason: None,
            }),
        })
    }

    pub fn approval_requests(&self) -> Vec<ApprovalRequest> {
        self.seen
            .lock()
            .iter()
            .filter_map(|m| match m {
                ClientMessage::ApprovalRequest(r) => Some(r.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock ConnectNamedPipe with a throwaway client.
        let _ = std::fs::OpenOptions::new().read(true).write(true).open(&self.path);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}
