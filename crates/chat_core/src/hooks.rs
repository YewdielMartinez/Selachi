//! Seams to the outside world: how agent processes are spawned ([`DriverFactory`]) and how the
//! host tracks their process trees ([`ProcessHooks`]). Both exist so `chat_core` stays free of
//! gpui, `proc_svc` and real processes in tests.

use std::path::PathBuf;

use agents_driver::acp::AcpDriver;
use agents_driver::claude::ClaudeDriver;
use agents_driver::codex::CodexDriver;
use agents_driver::{AgentDriver, AgentEvent, AgentKind, DriverError, ThreadConfig};
use futures::channel::mpsc::{unbounded, UnboundedReceiver};

use crate::model::{ApprovalDecision, ThreadId};

/// Raw job handle as an integer (a Windows `HANDLE`, i.e. `proc_svc` / `ui_procs::OwnerJob.job_handle
/// as usize`). Owned by the host's tracker: never closed here.
pub type JobHandle = usize;

/// What the host needs to label the process owner of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerInfo {
    /// `claude`, `codex`, `opencode` or the ACP command's program name.
    pub agent: String,
    /// Thread title (may be empty for an untitled thread).
    pub label: String,
    /// Directory the agent runs in (worktree or project root).
    pub cwd: PathBuf,
}

/// Process-ownership callbacks, implemented by the host on top of `proc_svc::Tracker` (see
/// `ui_procs::open_owner`). Every method may be called from `chat_core` worker threads.
///
/// Lifecycle per thread runtime: `open_owner` once, right before the first agent spawn;
/// `register_root(pid)` for every agent process that reports a pid (a restarted agent registers
/// its new pid under the same owner); `close_owner` when the runtime is closed (the host decides
/// what to do with leftovers: Stop / Keep / orphans, PLAN section 15).
pub trait ProcessHooks: Send + Sync {
    /// Create the owner (job) of `thread_id`. Returns the job handle if the host wants the
    /// agent process created inside it (see [`DriverFactory::spawn`]).
    fn open_owner(&self, thread_id: ThreadId, info: &OwnerInfo) -> Option<JobHandle>;
    /// The agent process of the thread has this pid. Implementations should assign it to the job
    /// (`JobGroup::assign`) when it was not created inside it, and `Tracker::register_root` it.
    fn register_root(&self, thread_id: ThreadId, pid: u32);
    /// The thread runtime is closed.
    fn close_owner(&self, thread_id: ThreadId);
}

/// Control surface of one running agent: [`AgentDriver`] plus the "always allow" answer.
pub trait AgentControl: Send {
    fn send(&mut self, text: &str) -> agents_driver::Result<()>;
    fn interrupt(&mut self) -> agents_driver::Result<()>;
    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()>;
    fn shutdown(&mut self);
}

/// A freshly started agent.
pub struct SpawnedAgent {
    pub control: Box<dyn AgentControl>,
    pub events: UnboundedReceiver<AgentEvent>,
    /// Pid of the agent process when the spawner knows it (`agents_driver` does not expose it
    /// yet, so [`DefaultDriverFactory`] reports `None`).
    pub pid: Option<u32>,
}

/// Starts agent processes. The default talks to `agents_driver`; tests inject a fake.
pub trait DriverFactory: Send + Sync {
    /// `job`: the handle returned by [`ProcessHooks::open_owner`], for spawners able to create
    /// the process inside that Job Object. [`DefaultDriverFactory`] ignores it.
    fn spawn(
        &self,
        kind: &AgentKind,
        cfg: &ThreadConfig,
        job: Option<JobHandle>,
    ) -> agents_driver::Result<SpawnedAgent>;
}

/// Spawns the real CLIs through `agents_driver`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultDriverFactory;

struct Generic(Box<dyn AgentDriver>);

impl AgentControl for Generic {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        self.0.send(text)
    }
    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.0.interrupt()
    }
    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()> {
        // Claude / Codex have no native "always"; chat_core remembers the grant per tool.
        self.0.approve(id, decision != ApprovalDecision::Deny)
    }
    fn shutdown(&mut self) {
        self.0.shutdown()
    }
}

struct Acp(AcpDriver);

impl AgentControl for Acp {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        self.0.send(text)
    }
    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.0.interrupt()
    }
    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()> {
        match decision {
            ApprovalDecision::Always => self.0.approve_always(id),
            ApprovalDecision::Allow => self.0.approve(id, true),
            ApprovalDecision::Deny => self.0.approve(id, false),
        }
    }
    fn shutdown(&mut self) {
        self.0.shutdown()
    }
}

impl DriverFactory for DefaultDriverFactory {
    fn spawn(
        &self,
        kind: &AgentKind,
        cfg: &ThreadConfig,
        _job: Option<JobHandle>,
    ) -> agents_driver::Result<SpawnedAgent> {
        let (tx, events) = unbounded();
        let control: Box<dyn AgentControl> = match kind {
            AgentKind::ClaudeCode => Box::new(Generic(Box::new(ClaudeDriver::spawn(cfg, tx)?))),
            AgentKind::Codex => Box::new(Generic(Box::new(CodexDriver::spawn(cfg, tx)?))),
            AgentKind::OpenCode => Box::new(Acp(AcpDriver::spawn_opencode(cfg, tx)?)),
            AgentKind::Acp { command } => Box::new(Acp(AcpDriver::spawn_command_line(command, cfg, tx)?)),
        };
        Ok(SpawnedAgent { control, events, pid: None })
    }
}

/// Short name for [`OwnerInfo::agent`].
pub(crate) fn owner_agent_name(kind: &AgentKind) -> String {
    match kind {
        AgentKind::ClaudeCode => "claude".into(),
        AgentKind::Codex => "codex".into(),
        AgentKind::OpenCode => "opencode".into(),
        AgentKind::Acp { command } => {
            let l = crate::model::agent_label(&AgentKind::Acp { command: command.clone() });
            l.trim_end_matches(" (ACP)").to_string()
        }
    }
}

pub(crate) fn driver_err_text(e: &DriverError) -> String {
    match e {
        DriverError::NotFound(what) => format!("No se encontró el ejecutable del agente ({what}). ¿Está instalado y en el PATH?"),
        other => format!("No se pudo iniciar el agente: {other}"),
    }
}
