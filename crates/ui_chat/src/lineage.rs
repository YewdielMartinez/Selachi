//! Delegation lineage (PLAN section 20): a thread started through `selachi agent` / the MCP server
//! is a *child* of the thread (or terminal tab) that delegated it. Pure helpers: nesting a thread
//! list, the text of the "Delegó en Codex · …" row, and the cost of a thread with its delegations.
//!
//! The thread link itself (`parent_thread_id`, `depth`, `delegated_by`) lives on
//! [`ThreadRecord::link`]; what a *run* did (files changed, cost, how long) lives in the harness's
//! run registry, which the host reads and hands to the hub as [`DelegationRun`]s.

use std::collections::{HashMap, HashSet};

use chat_core::{ThreadId, ThreadRecord, ThreadStatus};

use crate::util::format_duration_ms;

/// Deepest nesting level drawn (the policy allows 2 hops at most; this is a safety net).
pub const MAX_NEST_DEPTH: usize = 3;

/// How a delegated run stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Done,
    /// Error, timeout or token budget exceeded.
    Failed,
    Cancelled,
}

/// What the harness registry says about one delegated run (the host converts its records).
#[derive(Clone, Debug, PartialEq)]
pub struct DelegationRun {
    /// The child thread (the registry's run id).
    pub thread: ThreadId,
    pub state: RunState,
    pub files_changed: usize,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    /// Cost of the run (reported by the agent or estimated).
    pub cost_usd: f64,
    pub tokens: u64,
    /// Final answer of the agent, already redacted and capped by the harness.
    pub summary: String,
}

impl DelegationRun {
    /// Milliseconds the run took (so far, while it is running).
    pub fn duration_ms(&self, now_ms: i64) -> i64 {
        (self.finished_ms.unwrap_or(now_ms) - self.started_ms).max(0)
    }
}

/// Agent CLIs a session started outside Selachi (`agy -p ...` from its Bash tool), as the host saw
/// them while the subagent map was open (the map's process feed; nothing else watches for them).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExternalNote {
    /// Distinct agent ids (`agy`, `codex`), in the order they were first seen.
    pub agents: Vec<String>,
    /// Processes detected (the ones that already ended included).
    pub count: usize,
}

/// "Lanzó agy fuera de Selachi · 3 procesos" (names: up to two, then "y otros").
pub fn external_note_line(note: &ExternalNote) -> String {
    let names = match note.agents.as_slice() {
        [] => "otros agentes".to_string(),
        [a] => a.clone(),
        [a, b] => format!("{a} y {b}"),
        [a, b, ..] => format!("{a}, {b} y otros"),
    };
    let procs = if note.count == 1 {
        "1 proceso".to_string()
    } else {
        format!("{} procesos", note.count)
    };
    format!("Lanzó {names} fuera de Selachi · {procs}")
}

/// "1 archivo" / "3 archivos".
pub fn files_label(n: usize) -> String {
    match n {
        0 => "sin archivos".to_string(),
        1 => "1 archivo".to_string(),
        n => format!("{n} archivos"),
    }
}

/// The text of the parent's row: `Delegó en Codex · revisa tests de auth · 2 min · 3 archivos`.
/// Without a run record only the agent and the title are known.
pub fn delegation_line(child: &ThreadRecord, run: Option<&DelegationRun>, now_ms: i64) -> String {
    let mut parts = vec![
        format!("Delegó en {}", chat_core::agent_label(&child.agent)),
        child.display_title().to_string(),
    ];
    if let Some(run) = run {
        parts.push(format_duration_ms(run.duration_ms(now_ms)));
        if run.state != RunState::Running {
            parts.push(files_label(run.files_changed));
        }
    }
    parts.join(" · ")
}

/// Short state word of a delegated run for a second line ("En curso", "Terminó", "Falló").
pub fn run_state_label(child: &ThreadRecord, run: Option<&DelegationRun>) -> &'static str {
    match run.map(|r| r.state) {
        Some(RunState::Running) => "En curso",
        Some(RunState::Done) => "Terminó",
        Some(RunState::Failed) => "Falló",
        Some(RunState::Cancelled) => "Cancelada",
        None => match child.status {
            ThreadStatus::Working => "En curso",
            ThreadStatus::Error => "Falló",
            ThreadStatus::WaitingForInput => "Espera aprobación",
            _ => "Terminó",
        },
    }
}

/// The delegated threads whose parent is `parent`, oldest first (the order they were delegated).
pub fn children_of(threads: &[ThreadRecord], parent: ThreadId) -> Vec<&ThreadRecord> {
    let mut out: Vec<&ThreadRecord> = threads
        .iter()
        .filter(|t| t.link.parent_thread_id == Some(parent))
        .collect();
    out.sort_by_key(|t| (t.created_ms, t.id.0));
    out
}

/// Cost of the delegations under `parent`, every level (`runs` is keyed by child thread).
pub fn delegation_cost(
    threads: &[ThreadRecord],
    runs: &HashMap<ThreadId, DelegationRun>,
    parent: ThreadId,
) -> f64 {
    fn walk(
        threads: &[ThreadRecord],
        runs: &HashMap<ThreadId, DelegationRun>,
        parent: ThreadId,
        seen: &mut HashSet<ThreadId>,
    ) -> f64 {
        let mut total = 0.0;
        for c in children_of(threads, parent) {
            if seen.insert(c.id) {
                total +=
                    runs.get(&c.id).map_or(0.0, |r| r.cost_usd) + walk(threads, runs, c.id, seen);
            }
        }
        total
    }
    let mut seen = HashSet::from([parent]);
    walk(threads, runs, parent, &mut seen)
}

/// Orders `list` so every delegated thread follows its parent, when the parent is in the same
/// list. Returns each thread with its nesting depth (0 = not nested). A child whose parent is
/// elsewhere (another section, archived) stays where it was as a plain row.
pub fn nest(list: Vec<ThreadRecord>) -> Vec<(ThreadRecord, usize)> {
    let ids: HashSet<ThreadId> = list.iter().map(|t| t.id).collect();
    let nested = |t: &ThreadRecord| {
        t.link
            .parent_thread_id
            .is_some_and(|p| p != t.id && ids.contains(&p))
    };
    let mut kids: HashMap<ThreadId, Vec<usize>> = HashMap::new();
    for (i, t) in list.iter().enumerate() {
        if nested(t)
            && let Some(p) = t.link.parent_thread_id
        {
            kids.entry(p).or_default().push(i);
        }
    }
    for v in kids.values_mut() {
        v.sort_by_key(|&i| (list[i].created_ms, list[i].id.0));
    }
    let mut out = Vec::with_capacity(list.len());
    let mut placed = HashSet::new();
    fn place(
        i: usize,
        depth: usize,
        list: &[ThreadRecord],
        kids: &HashMap<ThreadId, Vec<usize>>,
        placed: &mut HashSet<usize>,
        out: &mut Vec<(usize, usize)>,
    ) {
        if !placed.insert(i) {
            return;
        }
        out.push((i, depth));
        for &k in kids.get(&list[i].id).into_iter().flatten() {
            place(k, (depth + 1).min(MAX_NEST_DEPTH), list, kids, placed, out);
        }
    }
    let mut order = Vec::with_capacity(list.len());
    for (i, t) in list.iter().enumerate() {
        if !nested(t) {
            place(i, 0, &list, &kids, &mut placed, &mut order);
        }
    }
    // Anything left is in a parent cycle: show it flat rather than lose it.
    for i in 0..list.len() {
        if !placed.contains(&i) {
            order.push((i, 0));
        }
    }
    let mut slots: Vec<Option<ThreadRecord>> = list.into_iter().map(Some).collect();
    for (i, depth) in order {
        if let Some(t) = slots[i].take() {
            out.push((t, depth));
        }
    }
    out
}

/// Children listed under a parent before «Ver todos» folds the rest.
pub const COLLAPSE_AFTER: usize = 5;

/// `3 subagentes · 1 en marcha` (the chip of a parent row that delegated).
pub fn subagents_summary(total: usize, running: usize) -> String {
    let noun = if total == 1 {
        "1 subagente".to_string()
    } else {
        format!("{total} subagentes")
    };
    if running == 0 {
        noun
    } else {
        format!("{noun} · {running} en marcha")
    }
}

/// Which rows of a parent's list to draw. `depths[i]` is the nesting depth of row `i` (0 = delegated
/// by the parent itself) and `running[i]` whether it is in flight. Up to [`COLLAPSE_AFTER`] top-level
/// delegations are kept unless `show_all`: the running ones first, then the newest (rows come oldest
/// first). A kept delegation brings its own sub-delegations. Indices come back in the original order.
pub fn visible_rows(depths: &[usize], running: &[bool], show_all: bool) -> Vec<usize> {
    let n = depths.len().min(running.len());
    if show_all || n == 0 {
        return (0..n).collect();
    }
    // groups: (first row, end row exclusive, any running)
    let mut groups: Vec<(usize, usize, bool)> = Vec::new();
    for i in 0..n {
        if depths[i] == 0 || groups.is_empty() {
            groups.push((i, i + 1, running[i]));
        } else if let Some(g) = groups.last_mut() {
            g.1 = i + 1;
            g.2 |= running[i];
        }
    }
    if groups.len() <= COLLAPSE_AFTER {
        return (0..n).collect();
    }
    let mut keep: Vec<usize> = Vec::new();
    // running ones first (oldest running first), then fill with the newest
    keep.extend(
        groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.2)
            .map(|(k, _)| k)
            .take(COLLAPSE_AFTER),
    );
    for k in (0..groups.len()).rev() {
        if keep.len() >= COLLAPSE_AFTER {
            break;
        }
        if !keep.contains(&k) {
            keep.push(k);
        }
    }
    keep.sort_unstable();
    keep.into_iter()
        .flat_map(|k| groups[k].0..groups[k].1)
        .collect()
}

#[cfg(test)]
mod tests {
    use agents_driver::{AgentKind, PermissionMode};
    use chat_core::ThreadLink;
    use std::path::PathBuf;

    use super::*;

    fn rec(id: i64, parent: Option<i64>, created: i64) -> ThreadRecord {
        ThreadRecord {
            id: ThreadId(id),
            title: format!("hilo {id}"),
            project_root: PathBuf::from("C:/p"),
            worktree_path: None,
            agent: if parent.is_some() {
                AgentKind::Codex
            } else {
                AgentKind::ClaudeCode
            },
            model: None,
            effort: None,
            permission: PermissionMode::Ask,
            status: ThreadStatus::Done,
            created_ms: created,
            updated_ms: created,
            agent_session_id: None,
            archived: false,
            checkpoint_before_turn: false,
            link: ThreadLink {
                parent_thread_id: parent.map(ThreadId),
                parent_tab: None,
                depth: u32::from(parent.is_some()),
                delegated_by: parent.map(|_| "mcp".to_string()),
            },
        }
    }

    fn run(
        id: i64,
        state: RunState,
        files: usize,
        start: i64,
        end: Option<i64>,
        usd: f64,
    ) -> DelegationRun {
        DelegationRun {
            thread: ThreadId(id),
            state,
            files_changed: files,
            started_ms: start,
            finished_ms: end,
            cost_usd: usd,
            tokens: 1000,
            summary: String::new(),
        }
    }

    #[test]
    fn the_line_says_who_what_how_long_and_how_many_files() {
        let child = rec(2, Some(1), 0);
        let r = run(2, RunState::Done, 3, 0, Some(120_000), 0.0);
        assert_eq!(
            delegation_line(&child, Some(&r), 999_999),
            "Delegó en Codex · hilo 2 · 2 min · 3 archivos"
        );
        let r = run(2, RunState::Done, 1, 0, Some(45_000), 0.0);
        assert!(delegation_line(&child, Some(&r), 0).ends_with("1 archivo"));
        // still running: elapsed so far, files not known yet
        let r = run(2, RunState::Running, 0, 0, None, 0.0);
        assert_eq!(
            delegation_line(&child, Some(&r), 180_000),
            "Delegó en Codex · hilo 2 · 3 min"
        );
        assert_eq!(delegation_line(&child, None, 0), "Delegó en Codex · hilo 2");
    }

    #[test]
    fn children_follow_their_parent_oldest_first() {
        let list = vec![
            rec(5, None, 50),
            rec(3, Some(1), 30),
            rec(1, None, 10),
            rec(4, Some(1), 20),
            rec(9, None, 5),
        ];
        let got: Vec<(i64, usize)> = nest(list).into_iter().map(|(t, d)| (t.id.0, d)).collect();
        assert_eq!(got, vec![(5, 0), (1, 0), (4, 1), (3, 1), (9, 0)]);
    }

    #[test]
    fn a_child_without_its_parent_in_the_list_stays_a_plain_row() {
        let got: Vec<(i64, usize)> = nest(vec![rec(2, Some(99), 1), rec(3, None, 2)])
            .into_iter()
            .map(|(t, d)| (t.id.0, d))
            .collect();
        assert_eq!(got, vec![(2, 0), (3, 0)]);
    }

    #[test]
    fn grandchildren_nest_deeper_and_cycles_do_not_loop() {
        let got: Vec<(i64, usize)> = nest(vec![
            rec(1, None, 1),
            rec(2, Some(1), 2),
            rec(3, Some(2), 3),
        ])
        .into_iter()
        .map(|(t, d)| (t.id.0, d))
        .collect();
        assert_eq!(got, vec![(1, 0), (2, 1), (3, 2)]);
        // 1 <-> 2 point at each other: both are shown, flat
        let cyc = nest(vec![rec(1, Some(2), 1), rec(2, Some(1), 2)]);
        assert_eq!(cyc.len(), 2);
    }

    #[test]
    fn costs_of_the_delegations_add_up_across_levels() {
        let threads = vec![
            rec(1, None, 1),
            rec(2, Some(1), 2),
            rec(3, Some(2), 3),
            rec(4, Some(1), 4),
        ];
        let mut runs = HashMap::new();
        runs.insert(ThreadId(2), run(2, RunState::Done, 0, 0, Some(1), 0.25));
        runs.insert(ThreadId(3), run(3, RunState::Done, 0, 0, Some(1), 0.10));
        runs.insert(ThreadId(4), run(4, RunState::Done, 0, 0, Some(1), 0.05));
        let total = delegation_cost(&threads, &runs, ThreadId(1));
        assert!((total - 0.40).abs() < 1e-9, "{total}");
        assert_eq!(delegation_cost(&threads, &runs, ThreadId(4)), 0.0);
        assert_eq!(crate::util::format_cost(total), "$0.40");
    }

    #[test]
    fn the_chip_counts_subagents_and_the_running_ones() {
        assert_eq!(subagents_summary(1, 0), "1 subagente");
        assert_eq!(subagents_summary(3, 1), "3 subagentes · 1 en marcha");
        assert_eq!(subagents_summary(7, 0), "7 subagentes");
    }

    #[test]
    fn long_lists_keep_five_with_the_running_ones_first() {
        // 8 top-level delegations; #1 (old) is running, #6 has a sub-delegation
        let depths = [0, 0, 0, 0, 0, 0, 1, 0, 0];
        let mut running = [false; 9];
        running[1] = true;
        let got = visible_rows(&depths, &running, false);
        // groups: 0,1,2,3,4,(5,6),7,8 -> running (1) + newest four (8,7,5-6,4)
        assert_eq!(got, vec![1, 4, 5, 6, 7, 8]);
        assert_eq!(visible_rows(&depths, &running, true).len(), 9);
        // up to five groups nothing is hidden
        assert_eq!(visible_rows(&[0, 0, 1, 0], &[false; 4], false), vec![0, 1, 2, 3]);
        assert!(visible_rows(&[], &[], false).is_empty());
    }

    #[test]
    fn state_words() {
        let child = rec(2, Some(1), 0);
        assert_eq!(
            run_state_label(&child, Some(&run(2, RunState::Failed, 0, 0, None, 0.0))),
            "Falló"
        );
        assert_eq!(run_state_label(&child, None), "Terminó");
    }
}
