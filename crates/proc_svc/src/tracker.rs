//! Attribution model: which tab / agent owns which processes, and which of
//! them were left behind.
//!
//! * Each owner (tab / thread, opaque `OwnerId`) has a [`JobGroup`].
//! * The shell / agent process launched for the owner is registered as a
//!   *root* (see [`Tracker::spawn`] / [`Tracker::register_root`]).
//! * A process in the job is [`ProcState::Active`] while a root is alive,
//!   [`ProcState::Orphan`] once every root has exited, and
//!   [`ProcState::PortZombie`] when an orphan still holds a listening port.
//! * Processes outside our jobs are attributed heuristically by walking the
//!   parent chain ([`Tracker::heuristic_untracked`]) and are only ever
//!   [`Attribution::Probable`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::SystemTime;

use crate::actions::{TerminateOutcome, TerminateOptions, terminate_job_with, terminate_tree_with};
use crate::error::{Error, Result};
use crate::job::{JobGroup, JobOptions};
use crate::ports::{PortInfo, listening_ports, ports_by_pid};
use crate::runtime::{Runtime, RuntimeKind, classify};
use crate::snapshot::{ProcInfo, snapshot, start_time_of};

/// Opaque owner id (tab / thread), chosen by the app.
pub type OwnerId = u64;

/// Metadata about an owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnerMeta {
    /// Agent running there (`claude`, `codex`, ...), if any.
    pub agent: Option<String>,
    /// Human label (tab title).
    pub label: String,
    /// Working directory / project.
    pub cwd: Option<PathBuf>,
}

/// Lifecycle state of a tracked process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcState {
    /// The owner's root process is still alive (or no root was registered).
    Active,
    /// Every registered root exited; this process survived it.
    Orphan,
    /// Orphan that still holds a listening / bound port.
    PortZombie,
}

/// How sure we are about the attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Attribution {
    /// Member of one of our jobs.
    Certain,
    /// Inferred from the parent chain.
    Probable,
}

/// A process attributed to an owner.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedProc {
    /// Snapshot data.
    pub info: ProcInfo,
    /// Ports it holds.
    pub ports: Vec<PortInfo>,
    /// State.
    pub state: ProcState,
    /// Runtime classification (icon / tag).
    pub runtime: Runtime,
    /// True for a registered root (the shell / agent itself).
    pub is_root: bool,
}

/// Report for one owner.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerReport {
    /// Owner id.
    pub owner: OwnerId,
    /// Its metadata.
    pub meta: OwnerMeta,
    /// Processes in its job, roots first, then by pid.
    pub processes: Vec<TrackedProc>,
}

impl OwnerReport {
    /// Processes that are orphans or port zombies.
    pub fn orphans(&self) -> impl Iterator<Item = &TrackedProc> {
        self.processes.iter().filter(|p| p.state != ProcState::Active)
    }
    /// Sum of working sets of orphans, in bytes.
    pub fn orphan_bytes(&self) -> u64 {
        self.orphans().map(|p| p.info.working_set).sum()
    }
}

/// A process outside our jobs attributed by heuristic.
#[derive(Debug, Clone, PartialEq)]
pub struct UntrackedProc {
    /// Snapshot data.
    pub info: ProcInfo,
    /// Ports it holds.
    pub ports: Vec<PortInfo>,
    /// Matched agent name (as given in `agent_names`).
    pub agent: String,
    /// Pid of the ancestor that matched.
    pub agent_pid: u32,
    /// Always [`Attribution::Probable`].
    pub attribution: Attribution,
    /// Runtime classification.
    pub runtime: Runtime,
}

#[derive(Debug, Clone, Copy)]
struct Root {
    pid: u32,
    start: Option<SystemTime>,
}

struct Entry {
    job: JobGroup,
    meta: OwnerMeta,
    roots: Vec<Root>,
}

/// Holds one job per owner and produces attribution reports.
#[derive(Default)]
pub struct Tracker {
    owners: HashMap<OwnerId, Entry>,
}

impl Tracker {
    /// Empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a job for `owner` (replacing nothing: errors if it exists is
    /// not enforced; an existing owner is replaced and its job dropped).
    pub fn create_owner(
        &mut self,
        owner: OwnerId,
        meta: OwnerMeta,
        options: JobOptions,
    ) -> Result<&JobGroup> {
        let job = JobGroup::with_options(meta.label.clone(), options)?;
        Ok(self.add_owner(owner, meta, job))
    }

    /// Registers an existing job for `owner`.
    pub fn add_owner(&mut self, owner: OwnerId, meta: OwnerMeta, job: JobGroup) -> &JobGroup {
        &self.owners.entry(owner).insert_entry(Entry { job, meta, roots: Vec::new() }).into_mut().job
    }

    /// The job of `owner`.
    pub fn job(&self, owner: OwnerId) -> Option<&JobGroup> {
        self.owners.get(&owner).map(|e| &e.job)
    }

    /// Metadata of `owner`.
    pub fn meta(&self, owner: OwnerId) -> Option<&OwnerMeta> {
        self.owners.get(&owner).map(|e| &e.meta)
    }

    /// Mutable metadata of `owner` (e.g. the agent started later).
    pub fn meta_mut(&mut self, owner: OwnerId) -> Option<&mut OwnerMeta> {
        self.owners.get_mut(&owner).map(|e| &mut e.meta)
    }

    /// Known owner ids.
    pub fn owners(&self) -> Vec<OwnerId> {
        self.owners.keys().copied().collect()
    }

    /// Removes an owner and returns its job so the caller decides what to do
    /// (dropping it does not kill members unless `kill_on_close` was set).
    pub fn remove_owner(&mut self, owner: OwnerId) -> Option<JobGroup> {
        self.owners.remove(&owner).map(|e| e.job)
    }

    /// Registers `pid` as a root of `owner` (the shell / agent process). The
    /// process start time is recorded so a reused pid is not mistaken for it.
    /// The process should already be in the job (see [`Tracker::spawn`]).
    pub fn register_root(&mut self, owner: OwnerId, pid: u32) -> Result<()> {
        let e = self.owners.get_mut(&owner).ok_or(Error::UnknownOwner(owner))?;
        if !e.roots.iter().any(|r| r.pid == pid) {
            e.roots.push(Root { pid, start: start_time_of(pid) });
        }
        Ok(())
    }

    /// Spawns `cmd` inside the owner's job (suspended, assigned, resumed: no
    /// escapes) and registers it as a root.
    pub fn spawn(&mut self, owner: OwnerId, cmd: &mut Command) -> Result<Child> {
        let e = self.owners.get(&owner).ok_or(Error::UnknownOwner(owner))?;
        let child = e.job.spawn_command(cmd, 0)?;
        self.register_root(owner, child.id())?;
        Ok(child)
    }

    /// Full attribution report (one snapshot + one port table; call it only
    /// while the panel is open or when orphans are suspected).
    pub fn report(&self) -> Vec<OwnerReport> {
        let procs = snapshot();
        let ports = ports_by_pid(&listening_ports());
        self.report_from(&procs, &ports)
    }

    /// [`report`](Self::report) over data the caller already collected.
    pub fn report_from(
        &self,
        procs: &[ProcInfo],
        ports: &HashMap<u32, Vec<PortInfo>>,
    ) -> Vec<OwnerReport> {
        let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
        let mut out: Vec<OwnerReport> = self
            .owners
            .iter()
            .map(|(owner, e)| {
                let members = e.job.members();
                let root_alive = e.roots.is_empty()
                    || e.roots.iter().any(|r| {
                        by_pid.get(&r.pid).is_some_and(|p| r.start.is_none() || p.start_time == r.start)
                    });
                let mut processes: Vec<TrackedProc> = members
                    .iter()
                    .filter_map(|pid| by_pid.get(pid))
                    .map(|info| {
                        let p_ports = ports.get(&info.pid).cloned().unwrap_or_default();
                        let is_root = e.roots.iter().any(|r| r.pid == info.pid);
                        TrackedProc {
                            state: state_for(root_alive, !p_ports.is_empty()),
                            runtime: classify(&info.name, info.cmdline.as_deref(), info.exe_path.as_deref()),
                            ports: p_ports,
                            is_root,
                            info: (*info).clone(),
                        }
                    })
                    .collect();
                processes.sort_by_key(|p| (!p.is_root, p.info.pid));
                OwnerReport { owner: *owner, meta: e.meta.clone(), processes }
            })
            .collect();
        out.sort_by_key(|r| r.owner);
        out
    }

    /// Processes outside every one of our jobs whose ancestor chain contains
    /// one of `agent_names` (e.g. `["claude", "codex"]`). Marked
    /// [`Attribution::Probable`].
    pub fn heuristic_untracked(&self, agent_names: &[&str]) -> Vec<UntrackedProc> {
        let procs = snapshot();
        let ports = ports_by_pid(&listening_ports());
        let tracked: HashSet<u32> =
            self.owners.values().flat_map(|e| e.job.members()).collect();
        heuristic_from(&procs, &ports, &tracked, agent_names, std::process::id())
    }

    /// Terminates the orphans / port zombies of `owner` (each orphan's tree).
    /// Roots and active processes are left alone.
    pub fn terminate_orphans(
        &self,
        owner: OwnerId,
        opts: &TerminateOptions,
    ) -> Result<TerminateOutcome> {
        let report = self
            .report()
            .into_iter()
            .find(|r| r.owner == owner)
            .ok_or(Error::UnknownOwner(owner))?;
        let mut total = TerminateOutcome::default();
        for p in report.orphans() {
            if total.killed.contains(&p.info.pid) || total.exited_gracefully.contains(&p.info.pid) {
                continue; // already taken down as part of another orphan's tree
            }
            let o = terminate_tree_with(p.info.pid, opts)?;
            total.targeted.extend(o.targeted);
            total.exited_gracefully.extend(o.exited_gracefully);
            total.killed.extend(o.killed);
            total.failed.extend(o.failed);
        }
        Ok(total)
    }

    /// Terminates everything in the owner's job.
    pub fn terminate_owner(&self, owner: OwnerId, opts: &TerminateOptions) -> Result<TerminateOutcome> {
        let e = self.owners.get(&owner).ok_or(Error::UnknownOwner(owner))?;
        terminate_job_with(&e.job, opts)
    }
}

/// Pure state rule.
fn state_for(root_alive: bool, has_port: bool) -> ProcState {
    match (root_alive, has_port) {
        (true, _) => ProcState::Active,
        (false, false) => ProcState::Orphan,
        (false, true) => ProcState::PortZombie,
    }
}

fn matches_agent(p: &ProcInfo, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let lower = p.name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    if stem == name {
        return true;
    }
    // node-based agents (`node ...\claude\cli.js`), bun/deno/python wrappers.
    let kind = classify(&p.name, None, None).kind;
    matches!(kind, RuntimeKind::Node | RuntimeKind::Bun | RuntimeKind::Deno | RuntimeKind::Python)
        && p.cmdline.as_deref().is_some_and(|c| c.to_ascii_lowercase().contains(&name))
}

/// Pure core of [`Tracker::heuristic_untracked`] (testable).
pub fn heuristic_from(
    procs: &[ProcInfo],
    ports: &HashMap<u32, Vec<PortInfo>>,
    tracked: &HashSet<u32>,
    agent_names: &[&str],
    self_pid: u32,
) -> Vec<UntrackedProc> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let find_agent = |p: &ProcInfo| agent_names.iter().find(|n| matches_agent(p, n)).copied();
    let mut out = Vec::new();
    for p in procs {
        if tracked.contains(&p.pid) || p.pid == self_pid || p.pid <= 4 || find_agent(p).is_some() {
            continue;
        }
        let mut cur = p;
        let mut hops = 0;
        while hops < 64 {
            hops += 1;
            let Some(parent) = by_pid.get(&cur.ppid).copied() else { break };
            // PID-reuse guard: a real parent started strictly before its child.
            let valid = matches!((parent.start_time, cur.start_time), (Some(a), Some(b)) if a < b);
            if !valid || parent.pid == cur.pid {
                break;
            }
            if let Some(agent) = find_agent(parent) {
                out.push(UntrackedProc {
                    info: p.clone(),
                    ports: ports.get(&p.pid).cloned().unwrap_or_default(),
                    agent: agent.to_string(),
                    agent_pid: parent.pid,
                    attribution: Attribution::Probable,
                    runtime: classify(&p.name, p.cmdline.as_deref(), p.exe_path.as_deref()),
                });
                break;
            }
            cur = parent;
        }
    }
    out.sort_by_key(|u| u.info.pid);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn p(pid: u32, ppid: u32, name: &str, cmd: &str, start: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: name.into(),
            exe_path: None,
            cmdline: Some(cmd.into()),
            start_time: Some(UNIX_EPOCH + Duration::from_secs(start)),
            working_set: 0,
            private_bytes: 0,
            cpu_time: Duration::ZERO,
            threads: 1,
            access_denied: false,
        }
    }

    #[test]
    fn state_rule() {
        assert_eq!(state_for(true, true), ProcState::Active);
        assert_eq!(state_for(false, false), ProcState::Orphan);
        assert_eq!(state_for(false, true), ProcState::PortZombie);
    }

    #[test]
    fn heuristic_walks_chain_and_guards_pid_reuse() {
        let procs = [
            p(100, 1, "node.exe", r"node C:\x\claude\cli.js", 10), // agent (node-based)
            p(101, 100, "cmd.exe", "cmd", 20),
            p(102, 101, "node.exe", "node vite.js", 30), // grandchild: probable
            p(103, 999, "node.exe", "node other.js", 40), // unknown parent
            // parent 100 but older than the agent: pid reuse, ignored
            p(104, 100, "node.exe", "node stale.js", 5),
            p(105, 102, "esbuild.exe", "esbuild", 31), // deeper descendant
            p(200, 1, "codex.exe", "codex", 10),
            p(201, 200, "python.exe", "python -m http.server", 11),
        ];
        let tracked: HashSet<u32> = [105].into();
        let r = heuristic_from(&procs, &HashMap::new(), &tracked, &["claude", "codex"], 9999);
        let pids: Vec<u32> = r.iter().map(|u| u.info.pid).collect();
        assert_eq!(pids, vec![101, 102, 201]);
        assert!(r.iter().all(|u| u.attribution == Attribution::Probable));
        assert_eq!(r[1].agent, "claude");
        assert_eq!(r[1].agent_pid, 100);
        assert_eq!(r[2].agent, "codex");
        assert_eq!(r[1].runtime.tag, Some("vite"));
    }
}
