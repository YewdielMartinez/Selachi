//! Seams to the outside world: how agent processes are spawned ([`DriverFactory`]) and how the
//! host tracks their process trees ([`ProcessHooks`]). Both exist so `chat_core` stays free of
//! gpui, `proc_svc` and real processes in tests.

use std::path::PathBuf;

use agents_driver::acp::AcpDriver;
use agents_driver::claude::ClaudeDriver;
use agents_driver::codex::CodexDriver;
use agents_driver::{AgentDriver, AgentEvent, AgentKind, DriverError, SpawnOpts, ThreadConfig};
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
    /// Pid of the agent process when the spawner knows it ([`DefaultDriverFactory`] reports the
    /// real one, so the runtime calls [`ProcessHooks::register_root`]).
    pub pid: Option<u32>,
}

/// Starts agent processes. The default talks to `agents_driver`; tests inject a fake.
pub trait DriverFactory: Send + Sync {
    /// `job`: the handle returned by [`ProcessHooks::open_owner`], for spawners able to create
    /// the process inside that Job Object ([`DefaultDriverFactory`] passes it to
    /// `agents_driver::spawn_with`: suspended spawn, assign, resume).
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

/// Adapter over any [`AgentDriver`]: "always" goes through the driver's native
/// [`AgentDriver::approve_with`] (ACP `allow_always`, Codex `acceptForSession`, Claude
/// `updatedPermissions`). A driver without a native grant falls back to a plain allow and
/// `ThreadRuntime` remembers the grant per tool.
struct Driver(Box<dyn AgentDriver>);

impl AgentControl for Driver {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        self.0.send(text)
    }
    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.0.interrupt()
    }
    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()> {
        self.0.approve_with(id, native_decision(decision))
    }
    fn shutdown(&mut self) {
        self.0.shutdown()
    }
}

fn native_decision(d: ApprovalDecision) -> agents_driver::ApprovalDecision {
    match d {
        ApprovalDecision::Allow => agents_driver::ApprovalDecision::Allow,
        ApprovalDecision::Deny => agents_driver::ApprovalDecision::Deny,
        ApprovalDecision::Always => agents_driver::ApprovalDecision::Always,
    }
}

impl DriverFactory for DefaultDriverFactory {
    fn spawn(
        &self,
        kind: &AgentKind,
        cfg: &ThreadConfig,
        job: Option<JobHandle>,
    ) -> agents_driver::Result<SpawnedAgent> {
        let (tx, events) = unbounded();
        let opts = SpawnOpts { job };
        let driver: Box<dyn AgentDriver> = match kind {
            AgentKind::ClaudeCode => Box::new(ClaudeDriver::spawn_with(cfg, tx, &opts)?),
            AgentKind::Codex => Box::new(CodexDriver::spawn_with(cfg, tx, &opts)?),
            AgentKind::OpenCode => Box::new(AcpDriver::spawn_opencode_with(cfg, tx, &opts)?),
            AgentKind::Acp { command } => {
                Box::new(AcpDriver::spawn_command_line_with(command, cfg, tx, &opts)?)
            }
        };
        let pid = driver.pid();
        Ok(SpawnedAgent {
            control: Box::new(Driver(driver)),
            events,
            pid,
        })
    }
}

/// Short name for [`OwnerInfo::agent`].
pub(crate) fn owner_agent_name(kind: &AgentKind) -> String {
    match kind {
        AgentKind::ClaudeCode => "claude".into(),
        AgentKind::Codex => "codex".into(),
        AgentKind::OpenCode => "opencode".into(),
        AgentKind::Acp { command } => {
            let l = crate::model::agent_label(&AgentKind::Acp {
                command: command.clone(),
            });
            l.trim_end_matches(" (ACP)").to_string()
        }
    }
}

pub(crate) fn driver_err_text(e: &DriverError) -> String {
    match e {
        DriverError::NotFound(what) => format!(
            "No se encontró el ejecutable del agente ({what}). ¿Está instalado y en el PATH?"
        ),
        other => format!("No se pudo iniciar el agente: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_map_one_to_one() {
        use agents_driver::ApprovalDecision as D;
        assert_eq!(native_decision(ApprovalDecision::Allow), D::Allow);
        assert_eq!(native_decision(ApprovalDecision::Deny), D::Deny);
        assert_eq!(native_decision(ApprovalDecision::Always), D::Always);
    }

    /// A driver with no native "always" (trait defaults) degrades to a plain allow.
    #[test]
    fn default_driver_degrades_always_to_allow() {
        struct Fake(Vec<(String, bool)>);
        impl AgentDriver for Fake {
            fn send(&mut self, _: &str) -> agents_driver::Result<()> {
                Ok(())
            }
            fn interrupt(&mut self) -> agents_driver::Result<()> {
                Ok(())
            }
            fn approve(&mut self, id: &str, allow: bool) -> agents_driver::Result<()> {
                self.0.push((id.into(), allow));
                Ok(())
            }
            fn shutdown(&mut self) {}
        }
        let mut d = Fake(Vec::new());
        assert_eq!(d.pid(), None);
        d.approve_with("a", agents_driver::ApprovalDecision::Always)
            .unwrap();
        d.approve_with("b", agents_driver::ApprovalDecision::Deny)
            .unwrap();
        assert_eq!(d.0, vec![("a".to_string(), true), ("b".to_string(), false)]);
    }

    /// Real factory, harmless stand-in agent: the pid is reported and, given a job, the agent
    /// process is a member of it.
    #[cfg(windows)]
    #[test]
    fn default_factory_reports_pid_and_joins_the_job() {
        let job = proc_svc::JobGroup::new("chat_core_factory_test").unwrap();
        let kind = AgentKind::Acp {
            command: "cmd.exe /c ping -n 8 127.0.0.1".into(),
        };
        let mut a = DefaultDriverFactory
            .spawn(
                &kind,
                &ThreadConfig::new("."),
                Some(job.raw_handle() as usize),
            )
            .unwrap();
        let pid = a.pid.expect("real pid");
        assert!(
            job.contains(pid),
            "pid {pid} not in job {:?}",
            job.members()
        );
        a.control.shutdown();
    }
}
