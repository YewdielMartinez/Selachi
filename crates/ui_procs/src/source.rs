//! Where samples come from and where terminations go.
//!
//! [`ProcSource`] is the seam between the model and `proc_svc`: [`TrackerSource`] is the real
//! thing (a shared `Tracker`), tests plug a fake in. Every method is **blocking** and meant for the
//! background executor.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use proc_svc::{
    CpuSampler, TerminateOptions, Tracker, check_safe, heuristic_from, listening_ports,
    logical_cores, ports_by_pid, snapshot, start_time_of, terminate_tree_with,
};

use crate::model::Collected;
use crate::plan::StopPlan;

/// The tracker the app and the panel share. Keep the lock short: the panel only takes it to read
/// job membership, never while terminating.
pub type SharedTracker = Arc<Mutex<Tracker>>;

/// What happened when a confirmed plan ran.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopResult {
    /// Exited when asked politely (Ctrl+Break / WM_CLOSE).
    pub graceful: Vec<u32>,
    /// Had to be force-terminated after the grace period.
    pub forced: Vec<u32>,
    /// Could not be terminated, with the reason.
    pub failed: Vec<(u32, String)>,
    /// Not touched because the plan no longer matched reality (pid reused, left the job...).
    pub skipped: Vec<(u32, String)>,
}

impl StopResult {
    pub fn stopped(&self) -> usize {
        self.graceful.len() + self.forced.len()
    }

    pub fn is_clean(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty()
    }
}

/// Samples the machine and terminates what a [`StopPlan`] lists.
pub trait ProcSource: Send + Sync + 'static {
    /// One full sample (processes, ports, attribution, CPU deltas).
    fn collect(&self) -> Collected;

    /// May `pid` be terminated at all? (System, critical, other user, self...) `Err` carries the
    /// reason shown in the dialog.
    fn check_stop(&self, _pid: u32) -> Result<(), String> {
        Ok(())
    }

    /// Terminates the roots of a confirmed plan (graceful, then forced).
    fn terminate(&self, plan: &StopPlan) -> StopResult;

    /// The panel was hidden: forget CPU baselines so the next sample does not span minutes.
    fn reset_sampling(&self) {}
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The real source: a shared [`Tracker`] plus a CPU sampler.
pub struct TrackerSource {
    tracker: SharedTracker,
    agent_names: Vec<String>,
    cpu: Mutex<CpuSampler>,
    options: TerminateOptions,
}

impl TrackerSource {
    /// Agents whose descendants are attributed by heuristic when they run outside Selachi.
    pub const DEFAULT_AGENTS: [&'static str; 4] = ["claude", "codex", "gemini", "opencode"];

    pub fn new(tracker: SharedTracker) -> Self {
        Self {
            tracker,
            agent_names: Self::DEFAULT_AGENTS
                .iter()
                .map(ToString::to_string)
                .collect(),
            cpu: Mutex::new(CpuSampler::new()),
            options: TerminateOptions::default(),
        }
    }

    /// Replaces the agent names used for the heuristic ("atribución probable").
    pub fn agents(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.agent_names = names.into_iter().map(Into::into).collect();
        self
    }

    /// Grace period and safety policy used by `terminate`.
    pub fn options(mut self, options: TerminateOptions) -> Self {
        self.options = options;
        self
    }
}

impl ProcSource for TrackerSource {
    fn collect(&self) -> Collected {
        // Heavy syscalls first, with no lock held.
        let procs = snapshot();
        let ports = listening_ports();
        let by_pid = ports_by_pid(&ports);
        let (reports, tracked, tree_owners) = {
            let t = lock(&self.tracker);
            let reports = t.report_from(&procs, &by_pid);
            let tracked = t
                .owners()
                .into_iter()
                .filter_map(|o| t.job(o))
                .flat_map(|j| j.members())
                .collect();
            let tree_owners = t
                .owners()
                .into_iter()
                .filter(|o| t.attachment_hint(*o) == proc_svc::TrackingMode::TreeOnly)
                .collect();
            (reports, tracked, tree_owners)
        };
        let names: Vec<&str> = self.agent_names.iter().map(String::as_str).collect();
        let untracked = heuristic_from(&procs, &by_pid, &tracked, &names, std::process::id());
        let cores = logical_cores() as f32;
        let cpu = lock(&self.cpu)
            .sample(&procs)
            .into_iter()
            .map(|(pid, v)| (pid, v / cores))
            .collect();
        Collected {
            reports,
            untracked,
            ports,
            procs,
            cpu,
            now: Some(SystemTime::now()),
            tree_owners,
        }
    }

    fn check_stop(&self, pid: u32) -> Result<(), String> {
        check_safe(pid, &self.options.safety).map_err(|e| e.to_string())
    }

    fn terminate(&self, plan: &StopPlan) -> StopResult {
        let mut out = StopResult::default();
        for root in &plan.roots {
            if let Some(owner) = root.owner {
                let member = lock(&self.tracker)
                    .job(owner)
                    .is_some_and(|j| j.contains(root.pid));
                if !member {
                    out.skipped
                        .push((root.pid, "ya no pertenece al job de su pestaña".into()));
                    continue;
                }
            }
            if root.start.is_some() && start_time_of(root.pid) != root.start {
                out.skipped
                    .push((root.pid, "ya terminó o el pid lo usa otro proceso".into()));
                continue;
            }
            match terminate_tree_with(root.pid, &self.options) {
                Ok(o) => {
                    out.graceful.extend(o.exited_gracefully);
                    out.forced.extend(o.killed);
                    out.failed.extend(o.failed);
                }
                Err(e) => out.failed.push((root.pid, e.to_string())),
            }
        }
        out
    }

    fn reset_sampling(&self) {
        lock(&self.cpu).reset();
    }
}
