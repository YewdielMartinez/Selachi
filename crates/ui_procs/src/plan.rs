//! Stop plans: exactly what a confirmation dialog lists and what gets terminated after "Confirmar".
//!
//! A plan is built from a *fresh* sample (so the preview is not 2 s stale), lists every process
//! that will end (the chosen process **and** its descendants, as `terminate_tree` would take them)
//! and is vetted with the safety layer of `proc_svc` before it is shown.

use std::collections::HashSet;
use std::time::SystemTime;

use proc_svc::{OwnerId, RuntimeKind, classify, collect_tree};

use crate::fmt::{format_bytes, plural, shorten_command};
use crate::model::{Collected, PortPill, RowState, Snapshot, pills};

/// What the plan is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanKind {
    /// "Detener" on one process (its whole tree).
    Tree,
    /// "Limpiar huérfanos".
    Orphans,
}

/// One process in the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanItem {
    pub pid: u32,
    pub name: String,
    pub command: String,
    pub runtime: RuntimeKind,
    pub ram: u64,
    pub ports: Vec<PortPill>,
    pub state: Option<RowState>,
}

/// A process that `terminate_tree` is called on.
#[derive(Clone, Debug, PartialEq)]
pub struct StopRoot {
    pub pid: u32,
    /// Creation time seen when the plan was made: a reused pid is never killed.
    pub start: Option<SystemTime>,
    /// The job it must still belong to (tracked processes only).
    pub owner: Option<OwnerId>,
    /// The root and every descendant, root first.
    pub tree: Vec<u32>,
}

/// The preview and the work order.
#[derive(Clone, Debug, PartialEq)]
pub struct StopPlan {
    pub kind: PlanKind,
    pub roots: Vec<StopRoot>,
    /// Every process that will end, roots first.
    pub items: Vec<PlanItem>,
    /// Orphans skipped because they cannot be inspected (access denied).
    pub denied: usize,
    /// Some target is not inside one of our jobs (heuristic attribution or an arbitrary process
    /// picked from the ports list).
    pub outside_jobs: bool,
    /// Roots the safety layer refuses, with the reason. They are never touched.
    pub refused: Vec<(PlanItem, String)>,
}

impl StopPlan {
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn total_ram(&self) -> u64 {
        self.items.iter().map(|i| i.ram).sum()
    }

    /// Dialog title: `Detener 3 procesos`, `Limpiar 2 huérfanos · 435 MB`.
    pub fn title(&self) -> String {
        match self.kind {
            PlanKind::Tree => format!("Detener {}", plural(self.items.len(), "proceso", "procesos")),
            PlanKind::Orphans => format!(
                "Limpiar {} · {}",
                plural(self.items.len(), "huérfano", "huérfanos"),
                format_bytes(self.total_ram())
            ),
        }
    }

    /// Moves roots refused by `check` out of the plan (with their trees).
    pub fn vet(&mut self, check: impl Fn(u32) -> Result<(), String>) {
        let mut kept = Vec::new();
        for root in std::mem::take(&mut self.roots) {
            match check(root.pid) {
                Ok(()) => kept.push(root),
                Err(reason) => {
                    let gone: HashSet<u32> = root.tree.iter().copied().collect();
                    let (out, stay): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut self.items).into_iter().partition(|i| gone.contains(&i.pid));
                    self.items = stay;
                    if let Some(item) = out.into_iter().find(|i| i.pid == root.pid) {
                        self.refused.push((item, reason));
                    }
                }
            }
        }
        self.roots = kept;
    }
}

fn item_of(c: &Collected, snap: &Snapshot, pid: u32) -> Option<PlanItem> {
    let p = c.procs.iter().find(|p| p.pid == pid)?;
    let ports: Vec<_> = c.ports.iter().filter(|q| q.pid == pid).cloned().collect();
    Some(PlanItem {
        pid,
        name: p.name.clone(),
        command: shorten_command(&p.name, p.cmdline.as_deref(), 90),
        runtime: classify(&p.name, p.cmdline.as_deref(), p.exe_path.as_deref()).kind,
        ram: p.working_set,
        ports: pills(&ports),
        state: snap.find_row(pid).map(|r| r.state),
    })
}

/// `pid` first, then its descendants (kill order reversed: parents read first in the preview).
fn tree_of(c: &Collected, pid: u32) -> Vec<u32> {
    let mut t = collect_tree(pid, &c.procs);
    t.reverse();
    t
}

fn finish(kind: PlanKind, c: &Collected, snap: &Snapshot, roots: Vec<StopRoot>, denied: usize) -> StopPlan {
    let mut seen = HashSet::new();
    let items: Vec<PlanItem> = roots
        .iter()
        .flat_map(|r| r.tree.iter().copied())
        .filter(|pid| seen.insert(*pid))
        .filter_map(|pid| item_of(c, snap, pid))
        .collect();
    let outside_jobs = roots.iter().any(|r| r.owner.is_none());
    StopPlan { kind, roots, items, denied, outside_jobs, refused: Vec::new() }
}

/// Plan for "Detener" on `pid`. `None` when the process is gone.
pub fn plan_tree(c: &Collected, snap: &Snapshot, pid: u32) -> Option<StopPlan> {
    let tree = tree_of(c, pid);
    if tree.is_empty() {
        return None;
    }
    let start = c.procs.iter().find(|p| p.pid == pid).and_then(|p| p.start_time);
    let owner = snap.find_row(pid).and_then(|r| r.owner);
    Some(finish(PlanKind::Tree, c, snap, vec![StopRoot { pid, start, owner, tree }], 0))
}

/// Plan for "Limpiar huérfanos": every orphan / port zombie of our jobs that can be inspected,
/// each tree once. Heuristic ("probable") rows are never part of it.
pub fn plan_orphans(c: &Collected, snap: &Snapshot) -> StopPlan {
    let orphans: Vec<_> = snap.rows().filter(|r| !r.probable && r.state.is_orphan()).collect();
    let denied = orphans.iter().filter(|r| r.access_denied).count();
    let candidates: Vec<_> = orphans.into_iter().filter(|r| !r.access_denied).collect();
    let trees: Vec<(u32, Vec<u32>)> = candidates.iter().map(|r| (r.pid, tree_of(c, r.pid))).collect();
    let mut roots = Vec::new();
    for r in &candidates {
        let tree = trees.iter().find(|(p, _)| *p == r.pid).map(|(_, t)| t.clone()).unwrap_or_default();
        if tree.is_empty() {
            continue;
        }
        let inside_other = trees.iter().any(|(p, t)| *p != r.pid && t.contains(&r.pid));
        if !inside_other {
            roots.push(StopRoot { pid: r.pid, start: r.start, owner: r.owner, tree });
        }
    }
    finish(PlanKind::Orphans, c, snap, roots, denied)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::{build_snapshot, fixtures::sample};

    #[test]
    fn orphan_plan_lists_exactly_the_orphans_once() {
        let c = sample();
        let snap = build_snapshot(&c);
        let plan = plan_orphans(&c, &snap);
        // dotnet (102) is an orphan root; esbuild (103) is its child: one root, two items.
        assert_eq!(plan.roots.iter().map(|r| r.pid).collect::<Vec<_>>(), [102]);
        assert_eq!(plan.items.iter().map(|i| i.pid).collect::<Vec<_>>(), [102, 103]);
        assert_eq!(plan.roots[0].owner, Some(1));
        assert!(!plan.outside_jobs);
        assert_eq!(plan.title(), "Limpiar 2 huérfanos · 435 MB");
        // Never the active shell, vite, or the heuristic python.
        assert!(plan.items.iter().all(|i| ![100, 101, 201].contains(&i.pid)));
    }

    #[test]
    fn orphan_plan_skips_access_denied_and_counts_them() {
        let mut c = sample();
        c.reports[0].processes[3].info.access_denied = true; // esbuild
        c.procs.iter_mut().find(|p| p.pid == 103).unwrap().access_denied = true;
        let snap = build_snapshot(&c);
        let plan = plan_orphans(&c, &snap);
        assert_eq!(plan.denied, 1);
        assert_eq!(plan.roots.iter().map(|r| r.pid).collect::<Vec<_>>(), [102]);
    }

    #[test]
    fn tree_plan_for_a_probable_row_is_flagged_outside_jobs() {
        let c = sample();
        let snap = build_snapshot(&c);
        let plan = plan_tree(&c, &snap, 201).unwrap();
        assert!(plan.outside_jobs);
        assert_eq!(plan.roots[0].owner, None);
        assert_eq!(plan.title(), "Detener 1 proceso");
        assert!(plan_tree(&c, &snap, 9_999).is_none(), "gone process");
    }

    #[test]
    fn tree_plan_includes_descendants_and_vetting_removes_refused_roots() {
        let c = sample();
        let snap = build_snapshot(&c);
        let mut plan = plan_tree(&c, &snap, 102).unwrap();
        assert_eq!(plan.items.iter().map(|i| i.pid).collect::<Vec<_>>(), [102, 103]);
        assert_eq!(plan.items[0].ports[0].port, 5000);
        plan.vet(|pid| if pid == 102 { Err("proceso crítico".into()) } else { Ok(()) });
        assert!(plan.is_empty() && plan.items.is_empty());
        assert_eq!(plan.refused.len(), 1);
        assert_eq!(plan.refused[0].0.pid, 102);
        assert_eq!(plan.refused[0].1, "proceso crítico");
    }
}
