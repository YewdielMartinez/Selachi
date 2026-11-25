//! Feeds the subagent map with agent CLIs that were launched **outside** the harness (ola 8A / 8C):
//! an `agy -p ...` that Claude started with Bash instead of the MCP `run_agent`.
//!
//! # Where the data comes from
//!
//! The same sources the process monitor uses: one `proc_svc::snapshot()` plus a [`CpuSampler`], and the
//! shared [`Tracker`](proc_svc::Tracker) to know which processes are the roots of which tab. The
//! monitor (`ui_procs::ProcsModel`) only samples while its own panel is open, so the map does the same
//! while *it* is on screen:
//!
//! * **Map hidden: nothing runs.** No task, no timer, no snapshot, no CPU baseline.
//! * **Map visible: one sample per [`SAMPLE_INTERVAL`]** (the monitor's cadence), taken on the
//!   background executor. The loop is a [`gpui::Task`]; dropping it (the last map view went away) ends it.
//!
//! The detection itself is `proc_svc::find_external_agents_with`, pure over the snapshot. This module
//! only decides *which roots to walk* and *which matches to keep* ([`externals_by_parent`], pure and
//! unit-tested), and turns the results into the map's [`ExternalProc`] rows.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use proc_svc::{CpuSampler, ExternalAgentProc, OwnerReport, ProcInfo, find_external_agents_with};
use ui_agentmap::{ExternalProc, NodeKey};
use ui_procs::SharedTracker;

use crate::chat_host::OWNER_BASE;

/// Time between samples while the map is visible (the process monitor's own cadence).
pub const SAMPLE_INTERVAL: Duration = ui_procs::SAMPLE_INTERVAL;

/// The map node a tracker owner hangs its external agents from: a chat thread (its owner id is
/// `OWNER_BASE | thread id`) or a terminal tab (its owner id is the tab's entity id).
pub fn parent_key(owner: u64) -> NodeKey {
    if owner >= OWNER_BASE {
        NodeKey::Thread(chat_core::ThreadId((owner & !OWNER_BASE) as i64))
    } else {
        NodeKey::Tab(owner.to_string())
    }
}

fn started_ms(p: &ExternalAgentProc) -> i64 {
    p.start_time
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn to_row(p: &ExternalAgentProc) -> ExternalProc {
    ExternalProc {
        agent: p.agent.clone(),
        pid: p.pid,
        cmdline_redacted: p.cmdline_redacted.clone(),
        cwd: p.cwd.clone(),
        model: p.model.clone(),
        effort: p.effort.clone(),
        started_ms: started_ms(p),
        ended_ms: None,
        exit_code: None,
        cpu_pct: p.cpu_pct,
        rss_bytes: p.rss_bytes,
    }
}

/// Is some ancestor of `pid` (by `ppid`, validated by start time so a reused pid is not followed) in
/// `agents`?
fn has_agent_ancestor(pid: u32, by_pid: &HashMap<u32, &ProcInfo>, agents: &HashSet<u32>) -> bool {
    let mut cur = pid;
    for _ in 0..64 {
        let Some(info) = by_pid.get(&cur) else {
            return false;
        };
        let Some(parent) = by_pid.get(&info.ppid) else {
            return false;
        };
        if parent.pid == cur || parent.start_time > info.start_time {
            return false; // stale ppid: the parent started after the child
        }
        if agents.contains(&parent.pid) {
            return true;
        }
        cur = parent.pid;
    }
    false
}

/// For every owner that has a root process: the external agent CLIs in its process tree.
///
/// * A **chat thread** owner's root is the agent itself (`claude`...): every agent below it is an
///   external subagent.
/// * A **terminal tab** owner's root is the shell. The agent the user runs there is the *session*, not
///   an external subagent, so only agents nested below another agent are kept (the `agy` that a
///   `claude` started), never the top-level one.
/// * The roots of the other owners are skipped (their own tree is theirs), and `selachi-agent` /
///   `selachi` subtrees are never entered (those runs are in the registry).
///
/// Owners without a root are left out; owners with a root but nothing found are listed with an empty
/// list, so a process that disappeared gets marked as finished by the map.
pub fn externals_by_parent(
    reports: &[OwnerReport],
    procs: &[ProcInfo],
    cpu_by_pid: &HashMap<u32, f32>,
) -> Vec<(NodeKey, Vec<ExternalProc>)> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let all_roots: Vec<(u64, u32)> = reports
        .iter()
        .flat_map(|r| {
            r.processes
                .iter()
                .filter(|p| p.is_root)
                .map(|p| (r.owner, p.info.pid))
        })
        .collect();
    let mut out = Vec::new();
    for report in reports {
        let roots: Vec<u32> = all_roots
            .iter()
            .filter(|(o, _)| *o == report.owner)
            .map(|(_, p)| *p)
            .collect();
        if roots.is_empty() {
            continue;
        }
        let exclude: HashSet<u32> = all_roots
            .iter()
            .filter(|(o, _)| *o != report.owner)
            .map(|(_, p)| *p)
            .collect();
        let mut found: Vec<ExternalAgentProc> = Vec::new();
        for root in roots {
            for p in find_external_agents_with(root, procs, cpu_by_pid, &[], &exclude) {
                if !found.iter().any(|f| f.pid == p.pid) {
                    found.push(p);
                }
            }
        }
        if report.owner < OWNER_BASE {
            let agents: HashSet<u32> = found.iter().map(|p| p.pid).collect();
            found.retain(|p| has_agent_ancestor(p.pid, &by_pid, &agents));
        }
        out.push((
            parent_key(report.owner),
            found.iter().map(to_row).collect::<Vec<_>>(),
        ));
    }
    out
}

/// One sample, blocking: meant for the background executor.
pub fn sample(
    tracker: &SharedTracker,
    cpu: &Mutex<CpuSampler>,
) -> Vec<(NodeKey, Vec<ExternalProc>)> {
    // Heavy syscalls first, with no lock held.
    let procs = proc_svc::snapshot();
    let cores = proc_svc::logical_cores().max(1) as f32;
    let cpu_by_pid: HashMap<u32, f32> = cpu
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .sample(&procs)
        .into_iter()
        .map(|(pid, v)| (pid, v / cores))
        .collect();
    let reports = tracker
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .report_from(&procs, &HashMap::new());
    externals_by_parent(&reports, &procs, &cpu_by_pid)
}

/// The sampling loop of the map: alive only while a map view is on screen.
#[derive(Default)]
pub struct AgentMapFeed {
    /// `Some` while the loop runs. Dropping it cancels the timer.
    pub(crate) task: Option<gpui::Task<()>>,
    /// CPU baselines between two samples; dropped when the loop stops.
    pub(crate) cpu: std::sync::Arc<Mutex<CpuSampler>>,
    /// Parents that were fed at some point: the ones that vanish from the tracker are cleared.
    pub(crate) fed: HashSet<NodeKey>,
}

impl AgentMapFeed {
    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    /// Ends the loop and forgets the CPU baseline (the next sample would span minutes).
    pub fn stop(&mut self) {
        self.task = None;
        self.cpu.lock().unwrap_or_else(|p| p.into_inner()).reset();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use proc_svc::{OwnerMeta, ProcState, TrackedProc, classify};

    use super::*;

    fn p(pid: u32, ppid: u32, name: &str, cmd: &str, start: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: name.into(),
            exe_path: None,
            cmdline: Some(cmd.into()),
            start_time: Some(UNIX_EPOCH + Duration::from_secs(start)),
            working_set: 10 * 1024 * 1024,
            private_bytes: 0,
            cpu_time: Duration::ZERO,
            threads: 1,
            access_denied: false,
        }
    }

    fn tracked(info: &ProcInfo, is_root: bool) -> TrackedProc {
        TrackedProc {
            runtime: classify(&info.name, info.cmdline.as_deref(), None),
            info: info.clone(),
            ports: Vec::new(),
            state: ProcState::Active,
            is_root,
        }
    }

    fn report(owner: u64, root: &ProcInfo) -> OwnerReport {
        OwnerReport {
            owner,
            meta: OwnerMeta::default(),
            processes: vec![tracked(root, true)],
        }
    }

    #[test]
    fn owners_map_to_chat_threads_and_terminal_tabs() {
        assert_eq!(
            parent_key(OWNER_BASE | 7),
            NodeKey::Thread(chat_core::ThreadId(7))
        );
        assert_eq!(parent_key(42), NodeKey::Tab("42".into()));
        // the shell's tab id is the one exported as SELACHI_PARENT_TAB (`TabId::as_u64`)
        assert_eq!(parent_key(4_294_967_297), NodeKey::Tab("4294967297".into()));
    }

    #[test]
    fn a_chat_agent_that_launches_other_agents_with_bash_is_detected() {
        // claude (chat root) -> bash -> agy --effort medium (x2), and a harness run that must not count
        let procs = vec![
            p(100, 4, "claude.exe", "claude", 10),
            p(110, 100, "bash.exe", "bash -c agy", 20),
            p(
                120,
                110,
                "agy.exe",
                r#"agy -p "do it" --effort medium --model gemini-3-pro"#,
                30,
            ),
            p(121, 110, "agy.exe", "agy -p x", 31),
            p(
                130,
                100,
                "selachi-agent.exe",
                "selachi-agent run codex x",
                25,
            ),
            p(131, 130, "codex.exe", "codex app-server", 26),
        ];
        let rows = externals_by_parent(
            &[report(OWNER_BASE | 5, &procs[0])],
            &procs,
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        let (key, list) = &rows[0];
        assert_eq!(*key, NodeKey::Thread(chat_core::ThreadId(5)));
        let pids: Vec<u32> = list.iter().map(|e| e.pid).collect();
        assert_eq!(pids, [120, 121], "the harness' codex is not external");
        assert_eq!(list[0].agent, "agy");
        assert_eq!(list[0].model.as_deref(), Some("gemini-3-pro"));
        assert_eq!(list[0].effort.as_deref(), Some("medium"));
        assert_eq!(list[0].started_ms, 30_000);
        assert!(list[0].ended_ms.is_none() && list[0].rss_bytes > 0);
    }

    #[test]
    fn the_agent_a_user_runs_in_a_terminal_is_the_session_not_an_external_subagent() {
        // shell (tab root) -> claude -> bash -> agy ; plus a second terminal with only `codex`
        let procs = vec![
            p(10, 4, "pwsh.exe", "pwsh", 1),
            p(11, 10, "claude.exe", "claude", 2),
            p(12, 11, "bash.exe", "bash", 3),
            p(13, 12, "agy.exe", "agy -p x", 4),
            p(20, 4, "pwsh.exe", "pwsh", 1),
            p(21, 20, "codex.exe", "codex", 2),
        ];
        let rows = externals_by_parent(
            &[report(7, &procs[0]), report(8, &procs[4])],
            &procs,
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 2);
        let first = rows
            .iter()
            .find(|(k, _)| *k == NodeKey::Tab("7".into()))
            .unwrap();
        assert_eq!(
            first.1.iter().map(|e| e.pid).collect::<Vec<_>>(),
            [13],
            "only the nested agy"
        );
        let second = rows
            .iter()
            .find(|(k, _)| *k == NodeKey::Tab("8".into()))
            .unwrap();
        assert!(
            second.1.is_empty(),
            "a lone `codex` in a terminal is the user's session; the tab is still listed so a finished process is marked"
        );
    }

    #[test]
    fn owners_without_a_root_are_skipped_and_other_owners_roots_are_not_walked() {
        let procs = vec![
            p(10, 4, "pwsh.exe", "pwsh", 1),
            p(11, 10, "claude.exe", "claude", 2),
            p(12, 11, "agy.exe", "agy -p x", 3),
        ];
        // an owner with no registered root
        let empty = OwnerReport {
            owner: 3,
            meta: OwnerMeta::default(),
            processes: Vec::new(),
        };
        assert!(externals_by_parent(&[empty], &procs, &HashMap::new()).is_empty());
        // claude is its own chat owner: its agy belongs to that owner, not to the terminal
        let rows = externals_by_parent(
            &[report(7, &procs[0]), report(OWNER_BASE | 1, &procs[1])],
            &procs,
            &HashMap::new(),
        );
        let term = rows
            .iter()
            .find(|(k, _)| *k == NodeKey::Tab("7".into()))
            .unwrap();
        assert!(term.1.is_empty(), "{:?}", term.1);
        let chat = rows
            .iter()
            .find(|(k, _)| *k == NodeKey::Thread(chat_core::ThreadId(1)))
            .unwrap();
        assert_eq!(chat.1.iter().map(|e| e.pid).collect::<Vec<_>>(), [12]);
    }

    #[test]
    fn a_reused_parent_pid_is_not_followed() {
        // agy claims parent 11, but pid 11 started AFTER it: a recycled pid
        let procs = [
            p(10, 4, "pwsh.exe", "pwsh", 1),
            p(11, 10, "claude.exe", "claude", 50),
            p(12, 11, "agy.exe", "agy -p x", 5),
        ];
        let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|x| (x.pid, x)).collect();
        let agents: HashSet<u32> = [11, 12].into();
        assert!(!has_agent_ancestor(12, &by_pid, &agents));
    }

    #[test]
    fn the_feed_does_not_run_until_started_and_stop_is_idempotent() {
        let mut feed = AgentMapFeed::default();
        assert!(!feed.is_running(), "hidden map: no task");
        feed.stop();
        feed.stop();
        assert!(!feed.is_running());
        assert_eq!(SAMPLE_INTERVAL, Duration::from_secs(2));
    }
}
