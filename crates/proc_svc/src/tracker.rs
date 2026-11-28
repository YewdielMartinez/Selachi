//! Attribution model: which tab / agent owns which processes, and which of
//! them were left behind.
//!
//! * Each owner (tab / thread, opaque `OwnerId`) has a [`JobGroup`].
//! * The shell / agent process launched for the owner is registered as a
//!   *root* (see [`Tracker::spawn`] / [`Tracker::register_root`]).
//! * A process in the job is [`ProcState::Active`] while a root is alive,
//!   [`ProcState::Orphan`] once every root has exited, and
//!   [`ProcState::PortZombie`] when an orphan still holds a listening port.
//! * Owners whose shell cannot be put in a Job Object (Microsoft Store `pwsh`:
//!   `AssignProcessToJobObject` is denied) use [`TrackingMode::TreeOnly`]: members
//!   are the live descendants of the root, remembered across reports so they turn
//!   into orphans when the root dies (see [`Tracker::register_root_untracked_job`]).
//! * Processes outside our jobs are attributed heuristically by walking the
//!   parent chain ([`Tracker::heuristic_untracked`]) and are only ever
//!   [`Attribution::Probable`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use crate::actions::{terminate_job_with, terminate_tree_with, TerminateOptions, TerminateOutcome};
use crate::error::{Error, Result};
use crate::job::{JobGroup, JobOptions};
use crate::ports::{listening_ports, ports_by_pid, PortInfo};
use crate::runtime::{classify, Runtime, RuntimeKind};
use crate::snapshot::{snapshot, snapshot_with, start_time_of, ProcInfo, SnapshotOptions};

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
    /// Descendant of the owner's root found by walking the process tree
    /// ([`TrackingMode::TreeOnly`]): exact enough, weaker than a job.
    Tree,
}

/// How an owner's processes are tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackingMode {
    /// Job Object membership: exact, survives parent death.
    Job,
    /// No usable job: live descendant tree of the root + remembered members.
    TreeOnly,
}

impl TrackingMode {
    /// Attribution level of processes tracked in this mode.
    pub fn attribution(self) -> Attribution {
        match self {
            Self::Job => Attribution::Certain,
            Self::TreeOnly => Attribution::Tree,
        }
    }

    /// Short UI label: `"exacta"` (job) or `"árbol"` (tree).
    pub fn attribution_label(self) -> &'static str {
        match self {
            Self::Job => "exacta",
            Self::TreeOnly => "árbol",
        }
    }

    /// One-line hint for the doctor / procs UI.
    pub fn hint_es(self) -> &'static str {
        match self {
            Self::Job => "seguimiento exacto (Job Object)",
            Self::TreeOnly => {
                "seguimiento por árbol (sin Job Object, p. ej. pwsh de Microsoft Store)"
            }
        }
    }
}

/// Recommended interval for the host's background tick while
/// [`Tracker::needs_polling`] is true (never poll faster).
pub const TREE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long a dead member is remembered so children of it can still be adopted.
const TOMBSTONE_TTL: Duration = Duration::from_secs(120);

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
        self.processes
            .iter()
            .filter(|p| p.state != ProcState::Active)
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

/// A process identity remembered by a tree-only owner.
#[derive(Debug, Clone, Copy)]
struct Known {
    pid: u32,
    start: Option<SystemTime>,
    /// When we first saw it gone (None while alive).
    died_seen: Option<SystemTime>,
}

#[derive(Default)]
struct TreeState {
    known: Vec<Known>,
    /// Some known member was alive at the last update.
    active: bool,
}

struct Entry {
    /// For tree-only owners this is an empty, unassigned job (kept so
    /// [`Tracker::job`] stays total).
    job: JobGroup,
    meta: OwnerMeta,
    roots: Vec<Root>,
    tree: Option<Mutex<TreeState>>,
}

fn lock(m: &Mutex<TreeState>) -> MutexGuard<'_, TreeState> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
        &self.add_owner_entry(owner, meta, job).job
    }

    fn add_owner_entry(&mut self, owner: OwnerId, meta: OwnerMeta, job: JobGroup) -> &mut Entry {
        self.owners
            .entry(owner)
            .insert_entry(Entry {
                job,
                meta,
                roots: Vec::new(),
                tree: None,
            })
            .into_mut()
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

    /// Takes `owner` out of the tracker so its job can be terminated **without holding the
    /// tracker's lock** (a graceful stop waits for seconds). Only that owner's job (or, for a
    /// tree-only owner, its remembered tree) is ever affected: nothing else is reachable through
    /// the returned handle. Hand it back with [`Tracker::reattach`] when the stop failed, so the
    /// leftovers stay attributed. `None` for an unknown owner.
    pub fn detach_for_stop(&mut self, owner: OwnerId) -> Option<DetachedOwner> {
        let entry = self.owners.remove(&owner)?;
        Some(DetachedOwner { owner, entry })
    }

    /// Puts an owner taken by [`Tracker::detach_for_stop`] back.
    pub fn reattach(&mut self, d: DetachedOwner) {
        self.owners.insert(d.owner, d.entry);
    }

    /// Registers `pid` as a root of `owner` (the shell / agent process). The
    /// process start time is recorded so a reused pid is not mistaken for it.
    /// The process should already be in the job (see [`Tracker::spawn`]).
    pub fn register_root(&mut self, owner: OwnerId, pid: u32) -> Result<()> {
        let e = self
            .owners
            .get_mut(&owner)
            .ok_or(Error::UnknownOwner(owner))?;
        if !e.roots.iter().any(|r| r.pid == pid) {
            let root = Root {
                pid,
                start: start_time_of(pid),
            };
            e.roots.push(root);
            if let Some(t) = &e.tree {
                remember_root(&mut lock(t), root);
            }
        }
        Ok(())
    }

    /// Registers `pid` as a root of `owner` **without a usable Job Object**
    /// ([`TrackingMode::TreeOnly`]). Use it when job attachment failed or was
    /// skipped (`JobAttachment::AssignFailed` / `None`), e.g. Microsoft Store
    /// `pwsh`, which Windows refuses to assign to a job.
    ///
    /// * Creates the owner (default metadata, empty job) if it is unknown; if it
    ///   exists (e.g. via [`create_owner`](Self::create_owner)) it is switched to
    ///   tree-only mode and keeps its metadata.
    /// * [`report`](Self::report) then computes its members as the live
    ///   descendants of the root(s) (ToolHelp `ppid` validated by start time, so
    ///   a reused pid is never followed) plus every previously seen member still
    ///   alive. When the root dies those survivors become
    ///   [`ProcState::Orphan`] / [`ProcState::PortZombie`]; new processes whose
    ///   (possibly dead) parent is a remembered member are adopted too.
    /// * Cost: nothing runs by itself. Members are refreshed on every
    ///   [`report`](Self::report) and on [`poll_tree`](Self::poll_tree). Poll
    ///   only while [`needs_polling`](Self::needs_polling) is true, at
    ///   [`TREE_POLL_INTERVAL`] or slower: it narrows the window in which a
    ///   short-lived intermediate (or a pid reuse) could hide a descendant.
    pub fn register_root_untracked_job(&mut self, owner: OwnerId, pid: u32) -> Result<()> {
        if !self.owners.contains_key(&owner) {
            let job = JobGroup::new("tree-only")?;
            self.add_owner(owner, OwnerMeta::default(), job);
        }
        if let Some(e) = self.owners.get_mut(&owner) {
            e.tree
                .get_or_insert_with(|| Mutex::new(TreeState::default()));
        }
        self.register_root(owner, pid)
    }

    /// Creates a job-less (tree-only) owner without a root yet.
    pub fn add_owner_tree_only(&mut self, owner: OwnerId, meta: OwnerMeta) -> Result<()> {
        let job = JobGroup::new(meta.label.clone())?;
        self.add_owner_entry(owner, meta, job).tree = Some(Mutex::new(TreeState::default()));
        Ok(())
    }

    /// How `owner` is tracked, for the doctor / procs UI (see
    /// [`TrackingMode::hint_es`]). Unknown owners report [`TrackingMode::Job`].
    pub fn attachment_hint(&self, owner: OwnerId) -> TrackingMode {
        match self.owners.get(&owner) {
            Some(Entry { tree: Some(_), .. }) => TrackingMode::TreeOnly,
            _ => TrackingMode::Job,
        }
    }

    /// True while at least one tree-only owner still has something to follow
    /// (a live remembered member). The host should run a background
    /// [`poll_tree`](Self::poll_tree) every [`TREE_POLL_INTERVAL`] (>= 5 s)
    /// **only** while this is true and a consumer (procs panel, orphan pill)
    /// needs fresh data; otherwise do nothing: zero cost at rest. Job owners
    /// never need polling.
    pub fn needs_polling(&self) -> bool {
        self.owners
            .values()
            .any(|e| e.tree.as_ref().is_some_and(|t| lock(t).active))
    }

    /// Cheap refresh of the remembered members of every tree-only owner (one
    /// ToolHelp snapshot, no command lines, no port table). Returns
    /// [`needs_polling`](Self::needs_polling).
    pub fn poll_tree(&self) -> bool {
        if self.owners.values().all(|e| e.tree.is_none()) {
            return false;
        }
        let procs = snapshot_with(SnapshotOptions { cmdline: false });
        self.update_trees(&procs);
        self.needs_polling()
    }

    /// Attribution level of `owner`'s processes (`None` for unknown owners).
    pub fn attribution(&self, owner: OwnerId) -> Option<Attribution> {
        self.owners
            .contains_key(&owner)
            .then(|| self.attachment_hint(owner).attribution())
    }

    /// Updates every tree-only owner against `procs`; returns alive members.
    fn update_trees(&self, procs: &[ProcInfo]) -> HashMap<OwnerId, HashSet<u32>> {
        let now = SystemTime::now();
        self.owners
            .iter()
            .filter_map(|(id, e)| {
                let t = e.tree.as_ref()?;
                Some((*id, update_tree(&mut lock(t), &e.roots, procs, now)))
            })
            .collect()
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
        let tree_members = self.update_trees(procs);
        let mut out: Vec<OwnerReport> = self
            .owners
            .iter()
            .map(|(owner, e)| {
                let mut members = e.job.members();
                if let Some(extra) = tree_members.get(owner) {
                    members.extend(extra.iter().copied());
                    members.sort_unstable();
                    members.dedup();
                }
                let root_alive = e.roots.is_empty()
                    || e.roots.iter().any(|r| {
                        by_pid
                            .get(&r.pid)
                            .is_some_and(|p| r.start.is_none() || p.start_time == r.start)
                    });
                let mut processes: Vec<TrackedProc> = members
                    .iter()
                    .filter_map(|pid| by_pid.get(pid))
                    .map(|info| {
                        let p_ports = ports.get(&info.pid).cloned().unwrap_or_default();
                        let is_root = e.roots.iter().any(|r| r.pid == info.pid);
                        TrackedProc {
                            state: state_for(root_alive, !p_ports.is_empty()),
                            runtime: classify(
                                &info.name,
                                info.cmdline.as_deref(),
                                info.exe_path.as_deref(),
                            ),
                            ports: p_ports,
                            is_root,
                            info: (*info).clone(),
                        }
                    })
                    .collect();
                processes.sort_by_key(|p| (!p.is_root, p.info.pid));
                OwnerReport {
                    owner: *owner,
                    meta: e.meta.clone(),
                    processes,
                }
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
        let mut tracked: HashSet<u32> =
            self.owners.values().flat_map(|e| e.job.members()).collect();
        tracked.extend(self.update_trees(&procs).into_values().flatten());
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
    pub fn terminate_owner(
        &self,
        owner: OwnerId,
        opts: &TerminateOptions,
    ) -> Result<TerminateOutcome> {
        let e = self.owners.get(&owner).ok_or(Error::UnknownOwner(owner))?;
        let mut total = terminate_job_with(&e.job, opts)?;
        if e.tree.is_some() {
            // Tree-only: take down each live remembered member's tree.
            let report = self.report().into_iter().find(|r| r.owner == owner);
            for p in report.iter().flat_map(|r| r.processes.iter()) {
                if total.killed.contains(&p.info.pid)
                    || total.exited_gracefully.contains(&p.info.pid)
                {
                    continue;
                }
                let o = terminate_tree_with(p.info.pid, opts)?;
                total.targeted.extend(o.targeted);
                total.exited_gracefully.extend(o.exited_gracefully);
                total.killed.extend(o.killed);
                total.failed.extend(o.failed);
            }
        }
        Ok(total)
    }
}

/// An owner taken out of a [`Tracker`] to be stopped (see [`Tracker::detach_for_stop`]).
pub struct DetachedOwner {
    owner: OwnerId,
    entry: Entry,
}

impl DetachedOwner {
    pub fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn label(&self) -> &str {
        &self.entry.meta.label
    }

    /// Live processes of the owner's job right now.
    pub fn live_members(&self) -> Vec<u32> {
        self.entry.job.members()
    }

    /// Terminates what is in the owner's job: the polite request first, the force after
    /// `opts.grace`. Safety checks (system processes, other users) apply as everywhere.
    /// Blocking: call it off the UI thread.
    pub fn terminate(&self, opts: &TerminateOptions) -> Result<TerminateOutcome> {
        terminate_job_with(&self.entry.job, opts)
    }
}

fn remember_root(t: &mut TreeState, r: Root) {
    if let Some(k) = t
        .known
        .iter_mut()
        .find(|k| k.pid == r.pid && k.start.is_none())
    {
        k.start = r.start; // learned the start time later
    }
    if !t.known.iter().any(|k| k.pid == r.pid && k.start == r.start) {
        t.known.push(Known {
            pid: r.pid,
            start: r.start,
            died_seen: None,
        });
        t.active = true;
    }
}

fn is_alive(k: &Known, by_pid: &HashMap<u32, &ProcInfo>) -> bool {
    by_pid
        .get(&k.pid)
        .is_some_and(|p| k.start.is_none() || p.start_time == k.start)
}

/// Pure core of tree-only tracking: refreshes `t.known` against `procs` and
/// returns the pids of the live members.
///
/// A process `c` is adopted when its parent identity is a known member:
/// * the live process with pid `c.ppid` started strictly before `c` (so it is
///   the real parent, not a reused pid) and is known; or
/// * no such live parent exists and a **dead** known member with that pid
///   started before `c` and was last seen at or after `c` started (a parent
///   must be alive when its child is created). A live process that reused the
///   pid and started after `c` therefore cannot capture `c`'s ancestry.
fn update_tree(
    t: &mut TreeState,
    roots: &[Root],
    procs: &[ProcInfo],
    now: SystemTime,
) -> HashSet<u32> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    for r in roots {
        // Only a live root is (re-)remembered; a dead one lives on as a tombstone
        // until it expires, it is never resurrected.
        let Some(live) = by_pid.get(&r.pid) else {
            continue;
        };
        if r.start.is_none() || live.start_time == r.start {
            remember_root(
                t,
                Root {
                    pid: r.pid,
                    start: r.start.or(live.start_time),
                },
            );
        }
    }
    for k in &mut t.known {
        if is_alive(k, &by_pid) {
            k.died_seen = None;
        } else if k.died_seen.is_none() {
            k.died_seen = Some(now);
        }
    }

    let mut order: Vec<&ProcInfo> = procs
        .iter()
        .filter(|p| p.pid > 4 && p.start_time.is_some())
        .collect();
    order.sort_by_key(|p| (p.start_time, p.pid));
    loop {
        let mut added = false;
        for c in &order {
            let Some(cs) = c.start_time else { continue };
            if c.ppid == c.pid
                || t.known
                    .iter()
                    .any(|k| k.pid == c.pid && k.start == Some(cs))
            {
                continue;
            }
            let live_parent = by_pid
                .get(&c.ppid)
                .and_then(|lp| lp.start_time.filter(|ls| *ls < cs).map(|ls| (lp.pid, ls)));
            let adopted = match live_parent {
                Some((pid, ls)) => t
                    .known
                    .iter()
                    .any(|k| k.pid == pid && k.start == Some(ls) && k.died_seen.is_none()),
                None => t.known.iter().any(|k| {
                    k.pid == c.ppid
                        && k.start.is_some_and(|ks| ks < cs)
                        && k.died_seen.is_some_and(|d| cs <= d)
                }),
            };
            if adopted {
                t.known.push(Known {
                    pid: c.pid,
                    start: Some(cs),
                    died_seen: None,
                });
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    // Forget dead members after the grace period.
    t.known.retain(|k| {
        k.died_seen
            .is_none_or(|d| now.duration_since(d).unwrap_or_default() <= TOMBSTONE_TTL)
    });
    let alive: HashSet<u32> = t
        .known
        .iter()
        .filter(|k| is_alive(k, &by_pid))
        .map(|k| k.pid)
        .collect();
    t.active = !alive.is_empty();
    alive
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
    matches!(
        kind,
        RuntimeKind::Node | RuntimeKind::Bun | RuntimeKind::Deno | RuntimeKind::Python
    ) && p
        .cmdline
        .as_deref()
        .is_some_and(|c| c.to_ascii_lowercase().contains(&name))
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
            let Some(parent) = by_pid.get(&cur.ppid).copied() else {
                break;
            };
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
        let r = heuristic_from(
            &procs,
            &HashMap::new(),
            &tracked,
            &["claude", "codex"],
            9999,
        );
        let pids: Vec<u32> = r.iter().map(|u| u.info.pid).collect();
        assert_eq!(pids, vec![101, 102, 201]);
        assert!(r.iter().all(|u| u.attribution == Attribution::Probable));
        assert_eq!(r[1].agent, "claude");
        assert_eq!(r[1].agent_pid, 100);
        assert_eq!(r[2].agent, "codex");
        assert_eq!(r[1].runtime.tag, Some("vite"));
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn root(pid: u32, start: u64) -> Root {
        Root {
            pid,
            start: Some(at(start)),
        }
    }

    fn pids(set: &HashSet<u32>) -> Vec<u32> {
        let mut v: Vec<u32> = set.iter().copied().collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn tree_walk_follows_descendants_and_rejects_stale_ppid() {
        let mut t = TreeState::default();
        let roots = [root(100, 10)];
        let procs = [
            p(100, 1, "pwsh.exe", "pwsh", 10),
            p(101, 100, "cmd.exe", "cmd", 20),
            p(102, 101, "node.exe", "node", 30),
            p(103, 100, "old.exe", "old", 5), // ppid 100 but older than it: reused ppid
            p(104, 999, "other.exe", "other", 40),
        ];
        let alive = update_tree(&mut t, &roots, &procs, at(100));
        assert_eq!(pids(&alive), vec![100, 101, 102]);
        assert!(t.active);
    }

    #[test]
    fn survivors_stay_members_after_root_dies() {
        let mut t = TreeState::default();
        let roots = [root(100, 10)];
        let first = [
            p(100, 1, "pwsh.exe", "pwsh", 10),
            p(101, 100, "cmd.exe", "cmd", 20),
            p(102, 101, "ping.exe", "ping", 30),
        ];
        update_tree(&mut t, &roots, &first, at(100));
        // Root and the middle process die; ping (ppid 101) survives.
        let second = [
            p(102, 101, "ping.exe", "ping", 30),
            p(500, 1, "x.exe", "x", 50),
        ];
        let alive = update_tree(&mut t, &roots, &second, at(200));
        assert_eq!(pids(&alive), vec![102]);
        assert!(t.active);
        // Later it is still remembered even though its parent is long gone.
        let alive = update_tree(&mut t, &roots, &second, at(260));
        assert_eq!(pids(&alive), vec![102]);
    }

    #[test]
    fn child_of_dead_member_is_adopted_but_pid_reuse_is_not() {
        let mut t = TreeState::default();
        let roots = [root(100, 10)];
        update_tree(
            &mut t,
            &roots,
            &[
                p(100, 1, "pwsh.exe", "pwsh", 10),
                p(101, 100, "cmd.exe", "cmd", 20),
            ],
            at(100),
        );
        // Root dies unseen; its child 101 (never reported alive again) spawned 102
        // before dying, and 102 survives. Meanwhile pid 100 is REUSED by an
        // unrelated process (start 500) that has its own child 300.
        let procs = [
            p(102, 101, "node.exe", "node", 60),
            p(100, 1, "unrelated.exe", "u", 500),
            p(300, 100, "child-of-reused.exe", "c", 510),
        ];
        let alive = update_tree(&mut t, &roots, &procs, at(600));
        assert_eq!(pids(&alive), vec![102], "{alive:?}");
    }

    #[test]
    fn dead_parent_adoption_requires_child_older_than_death() {
        let mut t = TreeState::default();
        let roots = [root(100, 10)];
        update_tree(
            &mut t,
            &roots,
            &[p(100, 1, "pwsh.exe", "pwsh", 10)],
            at(100),
        );
        // Observed dead at 200. A child started at 150 (before) is adopted;
        // one started at 300 (after we saw the parent gone) is not.
        let procs = [
            p(110, 100, "a.exe", "a", 150),
            p(111, 100, "b.exe", "b", 300),
        ];
        let alive = update_tree(&mut t, &roots, &procs, at(200));
        assert_eq!(pids(&alive), vec![110]);
    }

    #[test]
    fn tombstones_expire_and_polling_stops() {
        let mut t = TreeState::default();
        let roots = [root(100, 10)];
        update_tree(
            &mut t,
            &roots,
            &[p(100, 1, "pwsh.exe", "pwsh", 10)],
            at(100),
        );
        assert!(t.active);
        let alive = update_tree(&mut t, &roots, &[], at(110));
        assert!(alive.is_empty());
        assert!(!t.active, "nothing alive: no polling needed");
        update_tree(
            &mut t,
            &roots,
            &[],
            at(110) + TOMBSTONE_TTL + Duration::from_secs(1),
        );
        assert!(t.known.is_empty(), "{:?}", t.known);
    }

    #[test]
    fn report_marks_orphans_for_tree_only_owner() {
        let mut tr = Tracker::new();
        tr.add_owner_tree_only(
            7,
            OwnerMeta {
                label: "t".into(),
                ..Default::default()
            },
        )
        .unwrap();
        tr.owners.get_mut(&7).unwrap().roots.push(Root {
            pid: 100,
            start: None,
        });
        assert_eq!(tr.attachment_hint(7), TrackingMode::TreeOnly);
        assert_eq!(tr.attribution(7), Some(Attribution::Tree));

        let live = [
            p(100, 1, "pwsh.exe", "pwsh", 10),
            p(101, 100, "cmd.exe", "cmd", 20),
            p(102, 101, "node.exe", "node vite.js", 30),
        ];
        let ports: HashMap<u32, Vec<PortInfo>> = HashMap::new();
        let r = &tr.report_from(&live, &ports)[0];
        assert_eq!(r.processes.len(), 3);
        assert!(r.processes.iter().all(|x| x.state == ProcState::Active));
        assert!(tr.needs_polling());

        // Root + cmd die; node survives -> orphan.
        let after = [p(102, 101, "node.exe", "node vite.js", 30)];
        let r = &tr.report_from(&after, &ports)[0];
        assert_eq!(r.processes.len(), 1);
        assert_eq!(r.processes[0].state, ProcState::Orphan);
        assert_eq!(r.orphans().count(), 1);

        // A port makes it a zombie.
        let mut with_port = HashMap::new();
        with_port.insert(
            102,
            vec![PortInfo {
                proto: crate::ports::Proto::Tcp,
                local_addr: "127.0.0.1".parse().unwrap(),
                port: 5173,
                state: crate::ports::PortState::Listen,
                pid: 102,
            }],
        );
        let r = &tr.report_from(&after, &with_port)[0];
        assert_eq!(r.processes[0].state, ProcState::PortZombie);

        // Everything gone -> nothing to poll.
        let _ = tr.report_from(&[], &ports);
        assert!(!tr.needs_polling());
    }

    #[test]
    fn job_owner_hint_and_labels() {
        let mut tr = Tracker::new();
        tr.create_owner(1, OwnerMeta::default(), JobOptions::default())
            .unwrap();
        assert_eq!(tr.attachment_hint(1), TrackingMode::Job);
        assert_eq!(tr.attachment_hint(99), TrackingMode::Job);
        assert_eq!(tr.attribution(99), None);
        assert!(!tr.needs_polling());
        assert!(!tr.poll_tree());
        assert_eq!(TrackingMode::Job.attribution_label(), "exacta");
        assert_eq!(TrackingMode::TreeOnly.attribution_label(), "árbol");
        assert!(TrackingMode::TreeOnly.hint_es().contains("árbol"));
    }
}
