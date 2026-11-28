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

use proc_svc::{CpuSampler, ExternalAgentProc, OwnerReport, ProcInfo, find_external_agents_cfg};
use ui_agentmap::{ExternalChild, ExternalProc, NodeKey};
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
    extra_agents: &[String],
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
            for p in find_external_agents_cfg(root, procs, cpu_by_pid, extra_agents, &exclude) {
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

/// Most descendants listed per external agent (what `agy` started: its shells, `cargo`, `git`...).
pub const MAX_CHILDREN_PER_AGENT: usize = 48;

/// The processes below each external agent, from the same snapshot as the agents themselves: what
/// the map's Activity tab shows as the agent's commands. Pure.
///
/// Every descendant (child, grandchild...) is listed once, oldest first, except the other external
/// agents and what runs under them (they are map nodes of their own). A parent that started *after*
/// its child is a recycled pid and is not followed. Each agent is listed even with no children, so
/// the map can mark the ones that ended. Command lines are redacted here (the map redacts again).
pub fn children_of_externals(
    rows: &[(NodeKey, Vec<ExternalProc>)],
    procs: &[ProcInfo],
    cpu_by_pid: &HashMap<u32, f32>,
) -> Vec<(NodeKey, Vec<ExternalChild>)> {
    let mut kids_of: HashMap<u32, Vec<&ProcInfo>> = HashMap::new();
    for p in procs {
        kids_of.entry(p.ppid).or_default().push(p);
    }
    let agent_pids: HashSet<u32> = rows
        .iter()
        .flat_map(|(_, l)| l.iter().map(|e| e.pid))
        .collect();
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut out = Vec::new();
    for (_, list) in rows {
        for ext in list {
            let mut found: Vec<&ProcInfo> = Vec::new();
            let mut queue = vec![ext.pid];
            let mut seen: HashSet<u32> = HashSet::from([ext.pid]);
            while let Some(parent) = queue.pop() {
                let parent_start = by_pid.get(&parent).and_then(|p| p.start_time);
                for k in kids_of.get(&parent).map(Vec::as_slice).unwrap_or(&[]) {
                    // pid-reuse guard, like `proc_svc`: a real child never starts before its parent
                    let ok =
                        matches!((parent_start, k.start_time), (Some(ps), Some(ks)) if ks >= ps);
                    if !ok || agent_pids.contains(&k.pid) || !seen.insert(k.pid) {
                        continue;
                    }
                    found.push(k);
                    queue.push(k.pid);
                }
            }
            found.sort_by_key(|k| (k.start_time, k.pid));
            let excess = found.len().saturating_sub(MAX_CHILDREN_PER_AGENT);
            found.drain(..excess); // keep the newest
            out.push((
                ext.key(),
                found
                    .into_iter()
                    .map(|k| {
                        let cmd = k.cmdline.as_deref().filter(|c| !c.trim().is_empty());
                        ExternalChild {
                            pid: k.pid,
                            cmdline_redacted: store::redact(cmd.unwrap_or(&k.name)).text,
                            started_ms: k
                                .start_time
                                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                .map_or(0, |d| d.as_millis() as i64),
                            ended_ms: None,
                            exit_code: None,
                            cpu_pct: cpu_by_pid.get(&k.pid).copied(),
                            rss_bytes: k.working_set,
                        }
                    })
                    .collect(),
            ));
        }
    }
    out
}

/// One sample of the map's feed.
#[derive(Default)]
pub struct Sample {
    /// The external agents of every parent.
    pub rows: Vec<(NodeKey, Vec<ExternalProc>)>,
    /// The processes under each external agent (`ExternalProc::key()` -> children).
    pub children: Vec<(NodeKey, Vec<ExternalChild>)>,
}

/// One sample, blocking: meant for the background executor. `extra_agents` is
/// `[delegation] extra_agents` of the config.
pub fn sample(tracker: &SharedTracker, cpu: &Mutex<CpuSampler>, extra_agents: &[String]) -> Sample {
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
    let rows = externals_by_parent(&reports, &procs, &cpu_by_pid, extra_agents);
    let children = children_of_externals(&rows, &procs, &cpu_by_pid);
    Sample { rows, children }
}

/// What the map saw of the agents a session launched outside Selachi: the data of the chat row
/// «Lanzó agy fuera de Selachi · 3 procesos» and of the sidebar's map icon.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExternalSeen {
    /// Distinct agent ids in the order they were first seen.
    pub agents: Vec<String>,
    /// Processes detected, the finished ones included.
    pub count: usize,
}

/// Summary of the external agents the map holds for one parent (`None` when there are none).
pub fn seen_of(procs: &[ExternalProc]) -> Option<ExternalSeen> {
    if procs.is_empty() {
        return None;
    }
    let mut agents: Vec<String> = Vec::new();
    for p in procs {
        if !agents.contains(&p.agent) {
            agents.push(p.agent.clone());
        }
    }
    Some(ExternalSeen {
        agents,
        count: procs.len(),
    })
}

/// The ingested-usage agent an external process belongs to (`None` for agents whose sessions
/// `agents_svc` does not ingest, like Gemini, and for configured extras).
pub fn usage_agent(agent: &str) -> Option<agents_svc::Agent> {
    match agent.trim().to_ascii_lowercase().as_str() {
        "agy" | "antigravity" => Some(agents_svc::Agent::Antigravity),
        "codex" => Some(agents_svc::Agent::Codex),
        "claude" | "claude-code" => Some(agents_svc::Agent::Claude),
        "opencode" => Some(agents_svc::Agent::OpenCode),
        _ => None,
    }
}

/// The external processes whose usage is still to be looked up: not looked up yet, or looked up
/// for a different end time (it was running, now it ended). `done` maps a node to the `ended_ms` its
/// lookup was made for. Agents without ingested sessions are never listed.
pub fn usage_todo<'a>(
    done: &HashMap<NodeKey, Option<i64>>,
    procs: &'a [ExternalProc],
) -> Vec<&'a ExternalProc> {
    procs
        .iter()
        .filter(|p| usage_agent(&p.agent).is_some())
        .filter(|p| done.get(&p.key()) != Some(&p.ended_ms))
        .collect()
}

/// Tokens (billable) and the estimated cost of the session an external agent ran in, from the usage
/// `agents_svc` already ingested. Blocking (opens the database): background executor only.
/// `end_ms` is the process end, or now while it runs.
pub fn lookup_usage(
    db: &std::path::Path,
    agent: &str,
    cwd: &str,
    start_ms: i64,
    end_ms: i64,
) -> Option<(u64, Option<f64>)> {
    let agent = usage_agent(agent)?;
    let u = agents_svc::usage_for_external(db, agent, cwd, start_ms, end_ms)?;
    Some((u.billable_tokens(), u.cost))
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
    /// External agents whose usage was looked up (node -> the `ended_ms` it was looked up for);
    /// forgotten when the loop stops.
    pub(crate) usage: HashMap<NodeKey, Option<i64>>,
    /// What the map last saw of each parent. Kept after the map closes: the chat row and the
    /// sidebar icon only know what the feed found while it ran (nothing else watches for them).
    pub(crate) seen: HashMap<NodeKey, ExternalSeen>,
}

impl AgentMapFeed {
    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    /// Ends the loop and forgets the CPU baseline (the next sample would span minutes).
    pub fn stop(&mut self) {
        self.task = None;
        self.usage.clear();
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
            &[],
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
            &[],
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
        assert!(externals_by_parent(&[empty], &procs, &HashMap::new(), &[]).is_empty());
        // claude is its own chat owner: its agy belongs to that owner, not to the terminal
        let rows = externals_by_parent(
            &[report(7, &procs[0]), report(OWNER_BASE | 1, &procs[1])],
            &procs,
            &HashMap::new(),
            &[],
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

    fn ext_proc(agent: &str, pid: u32, started_ms: i64, ended: Option<i64>) -> ExternalProc {
        ExternalProc {
            agent: agent.into(),
            pid,
            cmdline_redacted: format!("{agent} -p x"),
            cwd: None,
            model: None,
            effort: None,
            started_ms,
            ended_ms: ended,
            exit_code: None,
            cpu_pct: None,
            rss_bytes: 1 << 20,
        }
    }

    #[test]
    fn the_processes_under_an_external_agent_are_its_children_but_other_agents_are_not() {
        // claude (chat root) -> bash -> agy(120) -> cmd -> cargo -> rustc ; agy(121) alone ;
        // agy(120) also started codex(125) (another external node of its own, with a child)
        let procs = vec![
            p(100, 4, "claude.exe", "claude", 10),
            p(110, 100, "bash.exe", "bash -c agy", 20),
            p(120, 110, "agy.exe", "agy -p x", 30),
            p(
                122,
                120,
                "cmd.exe",
                "cmd /c cargo test --token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
                31,
            ),
            p(123, 122, "cargo.exe", "cargo test", 32),
            p(124, 123, "rustc.exe", "rustc x", 33),
            p(125, 120, "codex.exe", "codex exec y", 34),
            p(126, 125, "git.exe", "git status", 35),
            p(121, 110, "agy.exe", "agy -p z", 31),
            // a recycled parent pid: the "child" started before its parent
            p(127, 120, "old.exe", "old", 5),
        ];
        let cpu: HashMap<u32, f32> = [(123, 12.5)].into();
        let rows = externals_by_parent(&[report(OWNER_BASE | 5, &procs[0])], &procs, &cpu, &[]);
        let kids = children_of_externals(&rows, &procs, &cpu);
        let of = |pid: u32| -> Vec<u32> {
            kids.iter()
                .find(|(k, _)| matches!(k, NodeKey::External { pid: p, .. } if *p == pid))
                .map(|(_, l)| l.iter().map(|c| c.pid).collect())
                .unwrap_or_else(|| panic!("agent {pid} is listed"))
        };
        assert_eq!(
            of(120),
            [122, 123, 124],
            "the shell, cargo and rustc; not codex nor old.exe"
        );
        assert_eq!(of(125), [126], "codex has its own children");
        assert!(
            of(121).is_empty(),
            "an agent with no children is still listed (so it can end)"
        );
        let find = |pid: u32| {
            kids.iter()
                .flat_map(|(_, l)| l)
                .find(|c| c.pid == pid)
                .unwrap()
        };
        assert_eq!(find(123).cpu_pct, Some(12.5));
        assert_eq!(find(123).started_ms, 32_000);
        assert!(
            !find(122)
                .cmdline_redacted
                .contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "{}",
            find(122).cmdline_redacted
        );
    }

    #[test]
    fn extra_agents_from_the_config_are_found_and_shells_are_ignored() {
        let procs = vec![
            p(100, 4, "claude.exe", "claude", 10),
            p(110, 100, "aider.exe", "aider --yes", 20),
            p(111, 100, "node.exe", "node build.js", 21),
        ];
        let owners = [report(OWNER_BASE | 5, &procs[0])];
        let none = externals_by_parent(&owners, &procs, &HashMap::new(), &[]);
        assert!(none[0].1.is_empty(), "aider is not an agent by default");
        let extra = vec!["aider".to_string(), "node".to_string()];
        let rows = externals_by_parent(&owners, &procs, &HashMap::new(), &extra);
        assert_eq!(
            rows[0].1.iter().map(|e| e.pid).collect::<Vec<_>>(),
            [110],
            "aider is found, node (a runtime) is refused even when configured"
        );
        assert_eq!(rows[0].1[0].agent, "aider");
    }

    #[test]
    fn what_the_map_saw_summarizes_per_parent() {
        assert_eq!(seen_of(&[]), None);
        let seen = seen_of(&[
            ext_proc("agy", 1, 10, None),
            ext_proc("codex", 2, 11, Some(20)),
            ext_proc("agy", 3, 12, None),
        ])
        .unwrap();
        assert_eq!(seen.agents, ["agy", "codex"]);
        assert_eq!(seen.count, 3, "finished processes count too");
    }

    #[test]
    fn usage_is_looked_up_once_per_process_and_again_only_when_it_ends() {
        assert_eq!(usage_agent("agy"), Some(agents_svc::Agent::Antigravity));
        assert_eq!(usage_agent("Codex"), Some(agents_svc::Agent::Codex));
        assert_eq!(usage_agent("claude"), Some(agents_svc::Agent::Claude));
        assert_eq!(usage_agent("opencode"), Some(agents_svc::Agent::OpenCode));
        assert_eq!(usage_agent("gemini"), None, "its sessions are not ingested");
        assert_eq!(usage_agent("aider"), None);

        let running = ext_proc("agy", 1, 1_000, None);
        let gemini = ext_proc("gemini", 2, 1_000, None);
        let mut done: HashMap<NodeKey, Option<i64>> = HashMap::new();
        let list = [running.clone(), gemini];
        assert_eq!(
            usage_todo(&done, &list).len(),
            1,
            "only agents with ingested sessions"
        );
        done.insert(running.key(), None);
        assert!(
            usage_todo(&done, &list).is_empty(),
            "already looked up while running"
        );
        let ended = ext_proc("agy", 1, 1_000, Some(9_000));
        assert_eq!(
            usage_todo(&done, std::slice::from_ref(&ended)).len(),
            1,
            "its end time changed: look again for the final numbers"
        );
        done.insert(ended.key(), Some(9_000));
        assert!(usage_todo(&done, &[ended]).is_empty());
    }

    /// `2026-05-01T11:00:00Z` -> unix ms, without a date crate for one test.
    fn unix_ms(s: &str) -> i64 {
        let (date, time) = s.trim_end_matches('Z').split_once('T').unwrap();
        let d: Vec<i64> = date.split('-').map(|x| x.parse().unwrap()).collect();
        let t: Vec<i64> = time.split(':').map(|x| x.parse().unwrap()).collect();
        // days from civil (Howard Hinnant)
        let (y, m, dd) = (d[0] - i64::from(d[1] <= 2), d[1], d[2]);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + dd - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        ((days * 24 + t[0]) * 60 + t[1]) * 60_000 + t[2] * 1000
    }

    /// The numbers come from the ingested logs (a synthetic Claude session) as billable tokens and
    /// an estimated cost; another folder, an unsupported agent or a missing database find nothing.
    #[test]
    fn usage_comes_from_the_ingested_session_of_that_folder_and_window() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude").join("p");
        std::fs::create_dir_all(&claude).unwrap();
        let cwd = r"C:\work\app";
        let mut f = std::fs::File::create(claude.join("a.jsonl")).unwrap();
        for (ts, id, out) in [
            ("2026-05-01T11:00:00Z", "m1", 200),
            ("2026-05-01T11:05:00Z", "m2", 300),
        ] {
            writeln!(
                f,
                r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"s1","cwd":"{}","gitBranch":"main","requestId":"r-{id}","uuid":"u-{id}","message":{{"id":"{id}","model":"claude-opus-5-5","usage":{{"input_tokens":10,"output_tokens":{out},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"content":[]}}}}"#,
                cwd.replace('\\', "\\\\")
            )
            .unwrap();
        }
        drop(f);
        let db = tmp.path().join("t.db");
        let mut ing = agents_svc::Ingestor::open(
            &db,
            agents_svc::IngestConfig {
                claude_projects: Some(tmp.path().join("claude")),
                ..Default::default()
            },
        )
        .unwrap();
        ing.scan().unwrap();
        drop(ing);
        let (start, end) = (
            unix_ms("2026-05-01T10:59:00Z"),
            unix_ms("2026-05-01T11:10:00Z"),
        );
        let (tokens, cost) = lookup_usage(&db, "claude", cwd, start, end).expect("the session");
        assert_eq!(
            tokens,
            10 + 200 + 10 + 300,
            "input + output of both responses"
        );
        assert!(cost.is_some_and(|c| c > 0.0), "{cost:?}");
        assert!(lookup_usage(&db, "claude", r"C:\elsewhere", start, end).is_none());
        assert!(lookup_usage(&db, "gemini", cwd, start, end).is_none());
        assert!(lookup_usage(&tmp.path().join("none.db"), "claude", cwd, start, end).is_none());
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
