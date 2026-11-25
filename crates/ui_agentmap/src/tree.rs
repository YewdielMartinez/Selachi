//! The pure part of the map: turning chat threads (with their delegation links) and the harness
//! run registry into a forest of orchestrators -> delegations, and the layouts drawn from it
//! (tree rows, graph columns, timeline bars). No gpui here: everything is unit-tested.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chat_core::{ThreadId, ThreadRecord, ThreadStatus, agent_label};
use ui_kit::{AgentMark, RowStatus};

use crate::registry::{RunInfo, RunStatus};

/// Deepest nesting the map walks (the policy allows two hops; this is a safety net against bad data).
pub const MAX_DEPTH: usize = 8;

/// Maximum number of external (non-harness) agent nodes retained per parent session.
pub const MAX_EXTERNAL_PER_SESSION: usize = 200;

/// Identity of a node: a chat thread (a delegated child, or a chat orchestrator), an opaque
/// terminal tab that delegated through the CLI / MCP, or an external coding-agent CLI process.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Thread(ThreadId),
    Tab(String),
    External { pid: u32, started_ms: i64 },
}

impl NodeKey {
    /// The harness run id (the child thread id as text), for thread nodes.
    pub fn run_id(&self) -> Option<String> {
        match self {
            NodeKey::Thread(t) => Some(t.0.to_string()),
            NodeKey::Tab(_) | NodeKey::External { .. } => None,
        }
    }

    /// The OS process id, for external process nodes.
    pub fn external_pid(&self) -> Option<u32> {
        match self {
            NodeKey::External { pid, .. } => Some(*pid),
            NodeKey::Thread(_) | NodeKey::Tab(_) => None,
        }
    }
}

/// An agent CLI process launched directly outside the Selachi harness (e.g. via bash `agy -p ...`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalProc {
    /// Canonical agent id (`"agy"`, `"codex"`, `"claude"`, `"gemini"`, `"opencode"`, ...).
    pub agent: String,
    /// Process id.
    pub pid: u32,
    /// Command line redacted with `store::redact`.
    pub cmdline_redacted: String,
    /// Working directory, if known.
    pub cwd: Option<PathBuf>,
    /// Model parsed from `--model` / `-m`, if present.
    pub model: Option<String>,
    /// Effort parsed from `--effort`, if present.
    pub effort: Option<String>,
    /// Process start timestamp in Unix milliseconds.
    pub started_ms: i64,
    /// Process end timestamp in Unix milliseconds (`None` while running).
    pub ended_ms: Option<i64>,
    /// Exit code if known (`Some(0)` -> Done, `Some(non-zero)` -> Failed).
    pub exit_code: Option<i32>,
    /// CPU usage percentage of this process subtree, if sampled.
    pub cpu_pct: Option<f32>,
    /// Working set (RSS) in bytes of this process subtree.
    pub rss_bytes: u64,
}

impl ExternalProc {
    /// Unique map key for this process instance (PID + start time prevents PID-reuse collisions).
    pub fn key(&self) -> NodeKey {
        NodeKey::External {
            pid: self.pid,
            started_ms: self.started_ms,
        }
    }

    /// Derived lifecycle status of this external process.
    pub fn status(&self) -> NodeStatus {
        match (self.ended_ms, self.exit_code) {
            (None, None) => NodeStatus::Running,
            (_, Some(0)) => NodeStatus::Done,
            (_, Some(_)) => NodeStatus::Failed,
            (Some(_), None) => NodeStatus::Done,
        }
    }

    /// Human Spanish status line, including the exit code when known.
    pub fn status_label(&self) -> String {
        match (self.ended_ms, self.exit_code) {
            (None, None) => "En marcha".to_string(),
            (_, Some(0)) => "Terminó (código 0)".to_string(),
            (_, Some(code)) => format!("Falló (código {code})"),
            (Some(_), None) => "Terminó".to_string(),
        }
    }
}

/// Kind of node in the subagent map.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// Chat thread (orchestrator or harness delegation).
    Thread,
    /// Terminal tab (orchestrator).
    Tab,
    /// Agent CLI launched outside the Selachi harness.
    External(Box<ExternalProc>),
}

/// What a node is doing, merged from the registry (what the harness knows) and the thread (what
/// the chat knows: only it can say "waiting for an approval").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeStatus {
    Running,
    /// Blocked on an approval of the user.
    Waiting,
    Done,
    Failed,
    Cancelled,
    TimedOut,
    OverBudget,
    Refused,
    /// Nothing is known (an orchestrator that is idle, a thread that never ran).
    Idle,
}

impl NodeStatus {
    /// The glyph of the row, `None` when the status word alone says it.
    pub fn glyph(self) -> Option<RowStatus> {
        match self {
            NodeStatus::Running => Some(RowStatus::Working),
            NodeStatus::Waiting => Some(RowStatus::Waiting),
            NodeStatus::Done => Some(RowStatus::Done),
            NodeStatus::Failed
            | NodeStatus::TimedOut
            | NodeStatus::OverBudget
            | NodeStatus::Refused => Some(RowStatus::Error),
            NodeStatus::Cancelled | NodeStatus::Idle => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NodeStatus::Running => "En marcha",
            NodeStatus::Waiting => "Espera aprobación",
            NodeStatus::Done => "Terminó",
            NodeStatus::Failed => "Falló",
            NodeStatus::Cancelled => "Cancelado",
            NodeStatus::TimedOut => "Tiempo agotado",
            NodeStatus::OverBudget => "Sin presupuesto",
            NodeStatus::Refused => "Rechazado",
            NodeStatus::Idle => "Inactivo",
        }
    }

    /// Still in flight (Running or Waiting): it can be cancelled and its duration keeps growing.
    pub fn is_active(self) -> bool {
        matches!(self, NodeStatus::Running | NodeStatus::Waiting)
    }

    pub fn is_failure(self) -> bool {
        matches!(
            self,
            NodeStatus::Failed
                | NodeStatus::TimedOut
                | NodeStatus::OverBudget
                | NodeStatus::Refused
        )
    }
}

/// Merges the registry status with the live thread status.
pub fn derive_status(run: Option<&RunInfo>, thread: Option<&ThreadRecord>) -> NodeStatus {
    let t = thread.map(|t| t.status);
    if let Some(rs) = run.and_then(|r| r.status) {
        return match rs {
            RunStatus::Running if t == Some(ThreadStatus::WaitingForInput) => NodeStatus::Waiting,
            RunStatus::Running => NodeStatus::Running,
            RunStatus::Done => NodeStatus::Done,
            RunStatus::Error => NodeStatus::Failed,
            RunStatus::Cancelled => NodeStatus::Cancelled,
            RunStatus::TimedOut => NodeStatus::TimedOut,
            RunStatus::BudgetExceeded => NodeStatus::OverBudget,
            RunStatus::Refused => NodeStatus::Refused,
        };
    }
    match t {
        Some(ThreadStatus::Working) => NodeStatus::Running,
        Some(ThreadStatus::WaitingForInput) => NodeStatus::Waiting,
        Some(ThreadStatus::Done) => NodeStatus::Done,
        Some(ThreadStatus::Error) => NodeStatus::Failed,
        Some(ThreadStatus::Idle) | None => NodeStatus::Idle,
    }
}

/// Normalized id of an agent name (`"Claude Code"` -> `claude`, `"gemini (ACP)"` -> `gemini`,
/// `"Antigravity"` -> `agy`): what the harness takes as `agent`.
pub fn agent_id(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let n = n.trim_end_matches("(acp)").trim();
    match n {
        "claude code" | "claude-code" | "claude" => "claude".into(),
        "antigravity" | "agy" => "agy".into(),
        other => other.replace(' ', "-"),
    }
}

/// Human name of an agent id.
pub fn agent_display(name: &str) -> String {
    match agent_id(name).as_str() {
        "claude" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "opencode" => "OpenCode".into(),
        "gemini" => "Gemini".into(),
        "agy" => "Antigravity".into(),
        _ => name.trim().to_string(),
    }
}

/// One node of the map.
#[derive(Clone, Debug)]
pub struct MapNode {
    pub key: NodeKey,
    pub kind: NodeKind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// 0 for an orchestrator.
    pub depth: usize,
    pub title: String,
    /// Normalized agent id (`claude`, `codex`...), empty when unknown.
    pub agent: String,
    pub mark: AgentMark,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub status: NodeStatus,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub files: usize,
    pub worktree: Option<PathBuf>,
    pub launcher: Option<String>,
    pub profile: Option<String>,
    pub permission: Option<String>,
    pub run: Option<RunInfo>,
    pub thread: Option<ThreadRecord>,
}

impl MapNode {
    pub fn is_root(&self) -> bool {
        self.parent.is_none()
    }

    pub fn is_external(&self) -> bool {
        matches!(self.kind, NodeKind::External(_))
    }

    pub fn external(&self) -> Option<&ExternalProc> {
        match &self.kind {
            NodeKind::External(ext) => Some(ext),
            NodeKind::Thread | NodeKind::Tab => None,
        }
    }

    /// Milliseconds it ran (so far, while it is active). `None` without a start time.
    pub fn duration_ms(&self, now_ms: i64) -> Option<i64> {
        let start = self.started_ms?;
        let end = match (self.finished_ms, self.status.is_active()) {
            (Some(f), _) => f,
            (None, true) => now_ms,
            (None, false) => return None,
        };
        Some((end - start).max(0))
    }

    /// The run id (child thread id as text), when the node is a thread.
    pub fn run_id(&self) -> Option<String> {
        self.key.run_id()
    }
}

/// What [`build_forest`] reads.
pub struct ForestInput<'a> {
    pub threads: &'a [ThreadRecord],
    pub runs: &'a [RunInfo],
    /// An orchestrator to show even when it has not delegated anything yet.
    pub wanted: Option<&'a NodeKey>,
    pub now_ms: i64,
    /// Names the host gives to terminal tabs (`tab id -> title`).
    pub tab_titles: &'a HashMap<String, String>,
    /// External (non-harness) agent processes grouped by their parent thread/tab.
    pub external: &'a HashMap<NodeKey, Vec<ExternalProc>>,
}

/// Every orchestrator of the session with its delegations.
#[derive(Clone, Debug, Default)]
pub struct Forest {
    pub nodes: Vec<MapNode>,
    /// Indices of the nodes without parent.
    pub roots: Vec<usize>,
    pub now_ms: i64,
    index: HashMap<NodeKey, usize>,
}

/// Counters of a subtree (the header of the map).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    /// Harness delegations (the root and external processes excluded).
    pub runs: usize,
    /// External (non-harness) agent processes under the root.
    pub external: usize,
    pub running: usize,
    pub waiting: usize,
    pub failed: usize,
    pub tokens: u64,
    pub cost_usd: f64,
    pub files: usize,
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn permission_word(mode: &str) -> String {
    match mode {
        "ReadOnly" => "read-only".into(),
        "AcceptEdits" => "accept-edits".into(),
        "Ask" => "ask".into(),
        "Full" => "full".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn mark_for_agent(agent: &str, raw_name: Option<&str>) -> AgentMark {
    if !agent.is_empty() {
        let from_display = AgentMark::from_name(&agent_display(agent));
        if from_display != AgentMark::Unknown {
            return from_display;
        }
    }
    raw_name
        .map(AgentMark::from_name)
        .unwrap_or(AgentMark::Unknown)
}

fn make_node(
    key: NodeKey,
    thread: Option<&ThreadRecord>,
    run: Option<&RunInfo>,
    tab_titles: &HashMap<String, String>,
) -> MapNode {
    let agent_name = run
        .and_then(|r| r.agent.clone())
        .or_else(|| thread.map(|t| agent_label(&t.agent)));
    let agent = agent_name.as_deref().map(agent_id).unwrap_or_default();
    let mark = mark_for_agent(&agent, agent_name.as_deref());
    let result = run.and_then(|r| r.result.as_ref());
    let status = derive_status(run, thread);
    let kind = match &key {
        NodeKey::Tab(_) => NodeKind::Tab,
        NodeKey::Thread(_) | NodeKey::External { .. } => NodeKind::Thread,
    };
    let title = match (&key, thread) {
        (_, Some(t)) => t.display_title().to_string(),
        (NodeKey::Tab(id), None) => tab_titles.get(id).cloned().unwrap_or_else(|| {
            if id.is_empty() {
                "Sesión externa".to_string()
            } else {
                "Terminal".to_string()
            }
        }),
        (NodeKey::Thread(id), None) => run
            .and_then(|r| r.prompt.as_deref())
            .map(|p| first_line(p, 60))
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| format!("Hilo {}", id.0)),
        (NodeKey::External { pid, .. }, None) => format!("Agente · PID {pid}"),
    };
    let files = result
        .map(|r| {
            r.files_changed
                .len()
                .max(r.diff_stat.as_ref().map_or(0, |d| d.files))
        })
        .unwrap_or(0);
    let mut finished_ms = run.and_then(|r| r.finished_ms);
    if finished_ms.is_none() && !status.is_active() && run.is_some() {
        finished_ms = thread.map(|t| t.updated_ms);
    }
    MapNode {
        key,
        kind,
        parent: None,
        children: Vec::new(),
        depth: 0,
        title,
        agent,
        mark,
        model: run
            .and_then(|r| r.model.clone())
            .or_else(|| thread.and_then(|t| t.model.clone())),
        effort: run
            .and_then(|r| r.effort.clone())
            .or_else(|| thread.and_then(|t| t.effort.clone())),
        status,
        started_ms: run
            .and_then(|r| r.started_ms)
            .or_else(|| thread.map(|t| t.created_ms)),
        finished_ms,
        tokens: result.and_then(|r| r.billable_tokens.or(r.total_tokens)),
        cost_usd: result.and_then(|r| r.cost_usd),
        files,
        worktree: run
            .and_then(|r| r.worktree().map(PathBuf::from))
            .or_else(|| thread.and_then(|t| t.worktree_path.clone())),
        launcher: run
            .and_then(|r| r.launcher.clone())
            .or_else(|| thread.and_then(|t| t.link.delegated_by.clone())),
        profile: run.and_then(|r| r.profile.clone()),
        permission: run
            .and_then(|r| r.permission.clone())
            .or_else(|| thread.map(|t| permission_word(&format!("{:?}", t.permission)))),
        run: run.cloned(),
        thread: thread.cloned(),
    }
}

fn make_external_node(ext: &ExternalProc) -> MapNode {
    let agent = agent_id(&ext.agent);
    let display = if agent.is_empty() {
        "Agente".to_string()
    } else {
        agent_display(&agent)
    };
    let mark = mark_for_agent(&agent, Some(&ext.agent));
    let status = ext.status();
    MapNode {
        key: ext.key(),
        kind: NodeKind::External(Box::new(ext.clone())),
        parent: None,
        children: Vec::new(),
        depth: 0,
        title: format!("{display} · PID {}", ext.pid),
        agent,
        mark,
        model: ext.model.clone(),
        effort: ext.effort.clone(),
        status,
        started_ms: Some(ext.started_ms),
        finished_ms: ext.ended_ms,
        tokens: None,
        cost_usd: None,
        files: 0,
        worktree: None,
        launcher: None,
        profile: None,
        permission: None,
        run: None,
        thread: None,
    }
}

/// Cap a session's external processes at [`MAX_EXTERNAL_PER_SESSION`], evicting oldest finished
/// entries first and then oldest running entries.
pub(crate) fn cap_external(procs: &mut Vec<ExternalProc>) {
    if procs.len() <= MAX_EXTERNAL_PER_SESSION {
        return;
    }
    // Sort so evictable entries come first: finished before running, oldest first.
    procs.sort_by_key(|p| {
        let running = p.ended_ms.is_none() && p.exit_code.is_none();
        (running, p.ended_ms.unwrap_or(i64::MAX), p.started_ms, p.pid)
    });
    let drop_n = procs.len() - MAX_EXTERNAL_PER_SESSION;
    procs.drain(0..drop_n);
    procs.sort_by_key(|p| (p.started_ms, p.pid));
}

/// The parent a delegated thread points at, from its link and its run record.
fn parent_key(thread: Option<&ThreadRecord>, run: Option<&RunInfo>, own: ThreadId) -> NodeKey {
    let parent_thread = thread
        .and_then(|t| t.link.parent_thread_id.map(|p| p.0))
        .or_else(|| run.and_then(|r| r.parent_thread_id))
        .filter(|p| *p != own.0);
    if let Some(p) = parent_thread {
        return NodeKey::Thread(ThreadId(p));
    }
    let tab = thread
        .and_then(|t| t.link.parent_tab.clone())
        .or_else(|| run.and_then(|r| r.parent_tab.clone()))
        .unwrap_or_default();
    NodeKey::Tab(tab)
}

struct Builder<'a> {
    nodes: Vec<MapNode>,
    index: HashMap<NodeKey, usize>,
    parents: Vec<Option<NodeKey>>,
    by_thread: &'a HashMap<ThreadId, &'a ThreadRecord>,
    by_run: &'a HashMap<ThreadId, &'a RunInfo>,
    tab_titles: &'a HashMap<String, String>,
}

impl Builder<'_> {
    fn add(&mut self, key: NodeKey, parent: Option<NodeKey>) {
        if self.index.contains_key(&key) {
            return;
        }
        let (thread, run) = match &key {
            NodeKey::Thread(id) => (
                self.by_thread.get(id).copied(),
                self.by_run.get(id).copied(),
            ),
            NodeKey::Tab(_) | NodeKey::External { .. } => (None, None),
        };
        self.index.insert(key.clone(), self.nodes.len());
        self.nodes
            .push(make_node(key, thread, run, self.tab_titles));
        self.parents.push(parent);
    }

    fn add_external(&mut self, ext: &ExternalProc, parent: NodeKey) {
        let key = ext.key();
        if self.index.contains_key(&key) {
            return;
        }
        self.index.insert(key, self.nodes.len());
        self.nodes.push(make_external_node(ext));
        self.parents.push(Some(parent));
    }
}

/// Builds the forest from the threads and the registry.
///
/// A *delegation* is a thread whose link says so (`delegated_by` or a parent thread) or that has a
/// registry record. Its parent is the thread that delegated it, else the terminal tab, else the
/// "external session" node. Missing pieces degrade, never fail: a run without a thread becomes a
/// node named after its prompt, a parent thread that is gone becomes a placeholder, a cycle in the
/// links is cut.
pub fn build_forest(input: &ForestInput<'_>) -> Forest {
    let by_thread: HashMap<ThreadId, &ThreadRecord> =
        input.threads.iter().map(|t| (t.id, t)).collect();
    let mut by_run: HashMap<ThreadId, &RunInfo> = HashMap::new();
    for r in input.runs {
        if let Some(id) = r.thread_id {
            by_run.entry(ThreadId(id)).or_insert(r);
        }
    }

    // The delegations, oldest first so the order of the children is the order of delegation.
    let mut delegated: Vec<ThreadId> = input
        .threads
        .iter()
        .filter(|t| t.is_delegation())
        .map(|t| t.id)
        .chain(by_run.keys().copied())
        .collect();
    delegated.sort();
    delegated.dedup();

    let mut b = Builder {
        nodes: Vec::new(),
        index: HashMap::new(),
        parents: Vec::new(),
        by_thread: &by_thread,
        by_run: &by_run,
        tab_titles: input.tab_titles,
    };
    for id in &delegated {
        let parent = parent_key(by_thread.get(id).copied(), by_run.get(id).copied(), *id);
        b.add(NodeKey::Thread(*id), Some(parent));
    }
    // External (non-harness) agent processes attached under their parent thread or tab.
    let mut ext_parents: Vec<&NodeKey> = input
        .external
        .iter()
        .filter(|(_, procs)| !procs.is_empty())
        .map(|(k, _)| k)
        .collect();
    ext_parents.sort_by_key(|k| match k {
        NodeKey::Thread(t) => (0u8, t.0, String::new()),
        NodeKey::Tab(s) => (1u8, 0, s.clone()),
        NodeKey::External { pid, started_ms } => (2u8, *started_ms, pid.to_string()),
    });
    for parent in ext_parents {
        let mut procs = input.external[parent].clone();
        cap_external(&mut procs);
        for ext in &procs {
            b.add_external(ext, parent.clone());
        }
    }
    // Parents that are not delegations themselves are orchestrators (no parent of their own).
    let parent_keys: Vec<NodeKey> = b.parents.iter().flatten().cloned().collect();
    for key in parent_keys.into_iter().chain(input.wanted.cloned()) {
        b.add(key, None);
    }
    let Builder {
        mut nodes,
        index,
        parents,
        ..
    } = b;

    // Link, cutting cycles: a chain longer than the node count loops.
    let parent_idx: Vec<Option<usize>> = parents
        .iter()
        .enumerate()
        .map(|(i, p)| {
            p.as_ref()
                .and_then(|k| index.get(k).copied())
                .filter(|pi| *pi != i)
        })
        .collect();
    let mut fixed = parent_idx;
    for start in 0..nodes.len() {
        let mut seen = HashSet::from([start]);
        let mut cur = start;
        while let Some(p) = fixed[cur] {
            if !seen.insert(p) {
                fixed[cur] = None;
                break;
            }
            cur = p;
        }
    }
    for (i, p) in fixed.iter().enumerate() {
        nodes[i].parent = *p;
        if let Some(p) = p {
            nodes[*p].children.push(i);
        }
    }
    for i in 0..nodes.len() {
        let mut kids = std::mem::take(&mut nodes[i].children);
        kids.sort_by_key(|k| (nodes[*k].started_ms.unwrap_or(i64::MAX), *k));
        nodes[i].children = kids;
    }
    let mut roots: Vec<usize> = (0..nodes.len())
        .filter(|i| nodes[*i].parent.is_none())
        .collect();
    // depth by walking down from the roots
    let mut stack: Vec<(usize, usize)> = roots.iter().map(|r| (*r, 0)).collect();
    while let Some((n, d)) = stack.pop() {
        nodes[n].depth = d;
        if d < MAX_DEPTH {
            for c in nodes[n].children.clone() {
                stack.push((c, d + 1));
            }
        }
    }
    let mut forest = Forest {
        nodes,
        roots: Vec::new(),
        now_ms: input.now_ms,
        index,
    };
    // newest activity first
    roots.sort_by_key(|r| std::cmp::Reverse(forest.last_activity(*r)));
    forest.roots = roots;
    forest
}

impl Forest {
    pub fn index_of(&self, key: &NodeKey) -> Option<usize> {
        self.index.get(key).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Node and descendants, preorder.
    pub fn subtree(&self, root: usize) -> Vec<usize> {
        let mut out = Vec::new();
        if root >= self.nodes.len() {
            return out;
        }
        let mut stack = vec![(root, 0usize)];
        while let Some((n, d)) = stack.pop() {
            if out.len() > self.nodes.len() {
                break;
            }
            out.push(n);
            if d < MAX_DEPTH {
                for c in self.nodes[n].children.iter().rev() {
                    stack.push((*c, d + 1));
                }
            }
        }
        out
    }

    /// The orchestrator a node belongs to.
    pub fn root_of(&self, mut node: usize) -> usize {
        for _ in 0..=MAX_DEPTH + 1 {
            match self.nodes[node].parent {
                Some(p) => node = p,
                None => break,
            }
        }
        node
    }

    /// Latest timestamp inside a subtree (start, finish) or 0.
    pub fn last_activity(&self, root: usize) -> i64 {
        self.subtree(root)
            .into_iter()
            .flat_map(|n| {
                let n = &self.nodes[n];
                [
                    n.started_ms,
                    n.finished_ms,
                    n.thread.as_ref().map(|t| t.updated_ms),
                ]
            })
            .flatten()
            .max()
            .unwrap_or(0)
    }

    /// Whether some node of the subtree is in flight.
    pub fn has_active(&self, root: usize) -> bool {
        self.subtree(root)
            .into_iter()
            .any(|n| self.nodes[n].status.is_active())
    }

    /// The orchestrator the map opens on: one with work in flight (the newest), else the newest.
    pub fn default_root(&self) -> Option<usize> {
        self.roots
            .iter()
            .copied()
            .find(|r| self.has_active(*r))
            .or_else(|| self.roots.first().copied())
    }

    pub fn totals(&self, root: usize) -> Totals {
        let mut t = Totals::default();
        for n in self.subtree(root) {
            let node = &self.nodes[n];
            if n != root {
                if node.is_external() {
                    t.external += 1;
                } else {
                    t.runs += 1;
                }
                match node.status {
                    NodeStatus::Running => t.running += 1,
                    NodeStatus::Waiting => t.waiting += 1,
                    s if s.is_failure() => t.failed += 1,
                    _ => {}
                }
            }
            t.tokens += node.tokens.unwrap_or(0);
            t.cost_usd += node.cost_usd.unwrap_or(0.0);
            t.files += node.files;
        }
        t
    }
}

// ---------------------------------------------------------------------------------------------
// Tree rows
// ---------------------------------------------------------------------------------------------

/// One visible line of the compact tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: usize,
    pub depth: usize,
    pub has_children: bool,
    pub collapsed: bool,
}

/// The rows under `root` (itself first) skipping the children of collapsed nodes.
pub fn visible_rows(forest: &Forest, root: usize, collapsed: &HashSet<NodeKey>) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((n, d)) = stack.pop() {
        let node = &forest.nodes[n];
        let is_collapsed = collapsed.contains(&node.key) && !node.children.is_empty();
        rows.push(Row {
            node: n,
            depth: d,
            has_children: !node.children.is_empty(),
            collapsed: is_collapsed,
        });
        if !is_collapsed && d < MAX_DEPTH {
            for c in node.children.iter().rev() {
                stack.push((*c, d + 1));
            }
        }
        if rows.len() > forest.nodes.len() {
            break;
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------------------------

/// A node placed on the grid of the graph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub node: usize,
    pub col: usize,
    /// Row coordinate; parents sit at the middle of their children.
    pub row: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphLayout {
    pub placed: Vec<Placed>,
    /// `(parent, child)` as indices into `placed`.
    pub edges: Vec<(usize, usize)>,
    pub cols: usize,
    pub rows: f32,
}

/// Columns by depth, leaves on consecutive rows, parents centered on their children.
pub fn graph_layout(forest: &Forest, root: usize, collapsed: &HashSet<NodeKey>) -> GraphLayout {
    let mut layout = GraphLayout::default();
    let mut next_leaf = 0.0f32;

    fn place(
        forest: &Forest,
        n: usize,
        col: usize,
        collapsed: &HashSet<NodeKey>,
        next_leaf: &mut f32,
        layout: &mut GraphLayout,
    ) -> usize {
        let node = &forest.nodes[n];
        let slot = layout.placed.len();
        layout.placed.push(Placed {
            node: n,
            col,
            row: 0.0,
        });
        layout.cols = layout.cols.max(col + 1);
        let hide = collapsed.contains(&node.key) || col >= MAX_DEPTH;
        if node.children.is_empty() || hide {
            layout.placed[slot].row = *next_leaf;
            *next_leaf += 1.0;
            return slot;
        }
        let kids: Vec<usize> = node
            .children
            .iter()
            .map(|c| place(forest, *c, col + 1, collapsed, next_leaf, layout))
            .collect();
        for k in &kids {
            layout.edges.push((slot, *k));
        }
        let first = layout.placed[kids[0]].row;
        let last = layout.placed[kids[kids.len() - 1]].row;
        layout.placed[slot].row = (first + last) / 2.0;
        slot
    }

    if forest.nodes.is_empty() {
        return layout;
    }
    place(forest, root, 0, collapsed, &mut next_leaf, &mut layout);
    layout.rows = next_leaf;
    layout
}

// ---------------------------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    pub node: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub active: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timeline {
    pub t0: i64,
    pub t1: i64,
    pub bars: Vec<Bar>,
    /// Most delegations running at the same moment.
    pub peak_parallel: usize,
}

impl Timeline {
    pub fn span_ms(&self) -> i64 {
        (self.t1 - self.t0).max(1)
    }

    /// Position of `t` in `0.0..=1.0`.
    pub fn frac(&self, t: i64) -> f32 {
        ((t - self.t0) as f64 / self.span_ms() as f64).clamp(0.0, 1.0) as f32
    }
}

/// One bar per delegation under `root` (the root itself is the whole session and has none).
pub fn timeline(forest: &Forest, root: usize, now_ms: i64) -> Timeline {
    let mut bars: Vec<Bar> = forest
        .subtree(root)
        .into_iter()
        .filter(|n| *n != root)
        .filter_map(|n| {
            let node = &forest.nodes[n];
            let start = node.started_ms?;
            let end = node.finished_ms.unwrap_or(if node.status.is_active() {
                now_ms
            } else {
                start
            });
            Some(Bar {
                node: n,
                start_ms: start,
                end_ms: end.max(start),
                active: node.status.is_active(),
            })
        })
        .collect();
    bars.sort_by_key(|b| (b.start_ms, b.node));
    if bars.is_empty() {
        return Timeline {
            t0: now_ms,
            t1: now_ms + 1,
            ..Timeline::default()
        };
    }
    let t0 = bars.iter().map(|b| b.start_ms).min().unwrap_or(now_ms);
    let t1 = bars
        .iter()
        .map(|b| b.end_ms)
        .max()
        .unwrap_or(now_ms)
        .max(t0 + 1);
    // sweep line for the peak parallelism
    let mut events: Vec<(i64, i32)> = bars
        .iter()
        .flat_map(|b| [(b.start_ms, 1), (b.end_ms.max(b.start_ms + 1), -1)])
        .collect();
    events.sort_by_key(|(t, d)| (*t, *d));
    let (mut cur, mut peak) = (0i32, 0i32);
    for (_, d) in events {
        cur += d;
        peak = peak.max(cur);
    }
    Timeline {
        t0,
        t1,
        bars,
        peak_parallel: peak.max(0) as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents_driver::{AgentKind, PermissionMode};
    use chat_core::ThreadLink;

    pub(crate) fn thread(id: i64, title: &str, agent: AgentKind, link: ThreadLink) -> ThreadRecord {
        ThreadRecord {
            id: ThreadId(id),
            title: title.into(),
            project_root: "C:/p".into(),
            worktree_path: None,
            agent,
            model: Some("m".into()),
            effort: None,
            permission: PermissionMode::ReadOnly,
            status: ThreadStatus::Done,
            created_ms: 1_000 * id,
            updated_ms: 1_000 * id + 500,
            agent_session_id: None,
            archived: false,
            checkpoint_before_turn: false,
            link,
        }
    }

    fn child_link(parent: i64) -> ThreadLink {
        ThreadLink {
            parent_thread_id: Some(ThreadId(parent)),
            parent_tab: None,
            depth: 1,
            delegated_by: Some("mcp".into()),
        }
    }

    fn run(id: i64, status: &str, started: i64, finished: Option<i64>) -> RunInfo {
        RunInfo::from_json(&format!(
            r#"{{"run_id":"{id}","agent":"codex","status":"{status}","started_ms":{started}
            {fin},"result":{{"files_changed":["a","b"],"usage":{{"billable":1200}},"cost":{{"usd":0.5}}}}}}"#,
            fin = finished
                .map(|f| format!(r#","finished_ms":{f}"#))
                .unwrap_or_default()
        ))
        .unwrap()
    }

    fn empty_ext() -> &'static HashMap<NodeKey, Vec<ExternalProc>> {
        static EMPTY: std::sync::LazyLock<HashMap<NodeKey, Vec<ExternalProc>>> =
            std::sync::LazyLock::new(HashMap::new);
        &EMPTY
    }

    fn input<'a>(
        threads: &'a [ThreadRecord],
        runs: &'a [RunInfo],
        titles: &'a HashMap<String, String>,
    ) -> ForestInput<'a> {
        ForestInput {
            threads,
            runs,
            wanted: None,
            now_ms: 100_000,
            tab_titles: titles,
            external: empty_ext(),
        }
    }

    #[test]
    fn builds_the_tree_from_links_and_the_registry() {
        let titles = HashMap::new();
        let threads = vec![
            thread(
                1,
                "Orquestador",
                AgentKind::ClaudeCode,
                ThreadLink::default(),
            ),
            thread(2, "Revisa tests", AgentKind::Codex, child_link(1)),
            thread(3, "Docs", AgentKind::OpenCode, child_link(1)),
            thread(4, "Nieto", AgentKind::Codex, child_link(2)),
            thread(9, "Chat normal", AgentKind::Codex, ThreadLink::default()),
        ];
        let runs = vec![
            run(2, "done", 2_000, Some(9_000)),
            run(3, "running", 3_000, None),
        ];
        let f = build_forest(&input(&threads, &runs, &titles));
        assert_eq!(f.roots.len(), 1, "the plain chat is not on the map");
        let root = f.roots[0];
        assert_eq!(f.nodes[root].title, "Orquestador");
        assert_eq!(f.nodes[root].mark, AgentMark::Claude);
        let kids: Vec<_> = f.nodes[root]
            .children
            .iter()
            .map(|c| f.nodes[*c].title.clone())
            .collect();
        assert_eq!(kids, vec!["Revisa tests", "Docs"]);
        let two = &f.nodes[f.index_of(&NodeKey::Thread(ThreadId(2))).unwrap()];
        assert_eq!(two.status, NodeStatus::Done);
        assert_eq!(two.tokens, Some(1200));
        assert_eq!(two.files, 2);
        assert_eq!(two.cost_usd, Some(0.5));
        assert_eq!(two.duration_ms(100_000), Some(7_000));
        assert_eq!(two.depth, 1);
        assert_eq!(two.children.len(), 1);
        let three = &f.nodes[f.index_of(&NodeKey::Thread(ThreadId(3))).unwrap()];
        assert_eq!(three.status, NodeStatus::Running);
        assert_eq!(
            three.duration_ms(10_000),
            Some(7_000),
            "running time follows `now`"
        );
        let t = f.totals(root);
        assert_eq!((t.runs, t.running, t.tokens, t.files), (3, 1, 2400, 4));
        assert_eq!(f.default_root(), Some(root));
    }

    #[test]
    fn terminal_tab_parents_and_external_sessions() {
        let mut titles = HashMap::new();
        titles.insert("tab-1".to_string(), "api".to_string());
        let link_tab = ThreadLink {
            parent_thread_id: None,
            parent_tab: Some("tab-1".into()),
            depth: 1,
            delegated_by: Some("cli".into()),
        };
        let link_ext = ThreadLink {
            delegated_by: Some("cli".into()),
            depth: 1,
            ..ThreadLink::default()
        };
        let threads = vec![
            thread(5, "A", AgentKind::Codex, link_tab),
            thread(6, "B", AgentKind::Codex, link_ext),
        ];
        let f = build_forest(&input(&threads, &[], &titles));
        assert_eq!(f.roots.len(), 2);
        let titles: Vec<_> = f.roots.iter().map(|r| f.nodes[*r].title.clone()).collect();
        assert!(titles.contains(&"api".to_string()));
        assert!(titles.contains(&"Sesión externa".to_string()));
    }

    #[test]
    fn missing_pieces_degrade_instead_of_failing() {
        let titles = HashMap::new();
        // a run whose thread is gone, with a parent that does not exist either
        let orphan = RunInfo::from_json(
            r#"{"run_id":"40","parent_thread_id":99,"prompt":"Revisa el módulo de auth\nsegunda línea","status":"running"}"#,
        )
        .unwrap();
        let f = build_forest(&input(&[], &[orphan], &titles));
        assert_eq!(f.roots.len(), 1);
        let root = &f.nodes[f.roots[0]];
        assert_eq!(root.title, "Hilo 99");
        let child = &f.nodes[root.children[0]];
        assert_eq!(child.title, "Revisa el módulo de auth");
        assert_eq!(child.status, NodeStatus::Running);
        assert_eq!(child.tokens, None);
        assert_eq!(child.duration_ms(5), None, "no start time, no duration");
        assert_eq!(child.agent, "");
    }

    #[test]
    fn a_cycle_in_the_links_is_cut() {
        let titles = HashMap::new();
        let threads = vec![
            thread(1, "A", AgentKind::Codex, child_link(2)),
            thread(2, "B", AgentKind::Codex, child_link(1)),
            thread(3, "Self", AgentKind::Codex, child_link(3)),
        ];
        let f = build_forest(&input(&threads, &[], &titles));
        assert!(!f.roots.is_empty());
        for n in 0..f.nodes.len() {
            // every node reaches a root
            let _ = f.root_of(n);
            assert!(f.subtree(f.root_of(n)).len() <= f.nodes.len());
        }
    }

    #[test]
    fn wanted_root_is_shown_even_without_delegations() {
        let titles = HashMap::new();
        let threads = vec![thread(
            1,
            "Solo",
            AgentKind::ClaudeCode,
            ThreadLink::default(),
        )];
        let key = NodeKey::Thread(ThreadId(1));
        let mut inp = input(&threads, &[], &titles);
        inp.wanted = Some(&key);
        let f = build_forest(&inp);
        assert_eq!(f.roots.len(), 1);
        assert_eq!(f.nodes[f.roots[0]].title, "Solo");
        assert!(f.nodes[f.roots[0]].children.is_empty());
        assert_eq!(f.totals(f.roots[0]).runs, 0);
    }

    #[test]
    fn status_transitions() {
        let mut t = thread(2, "x", AgentKind::Codex, child_link(1));
        // registry alone
        assert_eq!(
            derive_status(Some(&run(2, "running", 1, None)), None),
            NodeStatus::Running
        );
        // running + the thread asks for an approval
        t.status = ThreadStatus::WaitingForInput;
        assert_eq!(
            derive_status(Some(&run(2, "running", 1, None)), Some(&t)),
            NodeStatus::Waiting
        );
        // finished states come from the registry whatever the thread says
        for (word, want) in [
            ("done", NodeStatus::Done),
            ("error", NodeStatus::Failed),
            ("cancelled", NodeStatus::Cancelled),
            ("timed_out", NodeStatus::TimedOut),
            ("budget_exceeded", NodeStatus::OverBudget),
            ("refused", NodeStatus::Refused),
        ] {
            assert_eq!(
                derive_status(Some(&run(2, word, 1, Some(2))), Some(&t)),
                want,
                "{word}"
            );
        }
        // no run: the thread decides
        t.status = ThreadStatus::Working;
        assert_eq!(derive_status(None, Some(&t)), NodeStatus::Running);
        t.status = ThreadStatus::Error;
        assert_eq!(derive_status(None, Some(&t)), NodeStatus::Failed);
        assert_eq!(derive_status(None, None), NodeStatus::Idle);
        // glyphs
        assert_eq!(NodeStatus::Running.glyph(), Some(RowStatus::Working));
        assert_eq!(NodeStatus::TimedOut.glyph(), Some(RowStatus::Error));
        assert_eq!(NodeStatus::Cancelled.glyph(), None);
        assert!(NodeStatus::Waiting.is_active() && !NodeStatus::Done.is_active());
    }

    #[test]
    fn a_node_moves_from_running_to_done_between_builds() {
        let titles = HashMap::new();
        let threads = vec![
            thread(1, "O", AgentKind::ClaudeCode, ThreadLink::default()),
            thread(2, "C", AgentKind::Codex, child_link(1)),
        ];
        let before = build_forest(&input(&threads, &[run(2, "running", 1_000, None)], &titles));
        assert!(before.has_active(before.roots[0]));
        let after = build_forest(&input(
            &threads,
            &[run(2, "done", 1_000, Some(4_000))],
            &titles,
        ));
        assert!(!after.has_active(after.roots[0]));
        assert_eq!(after.totals(after.roots[0]).running, 0);
    }

    fn sample() -> (Forest, usize) {
        let titles = HashMap::new();
        let threads = vec![
            thread(1, "O", AgentKind::ClaudeCode, ThreadLink::default()),
            thread(2, "A", AgentKind::Codex, child_link(1)),
            thread(3, "B", AgentKind::Codex, child_link(1)),
            thread(4, "A1", AgentKind::Codex, child_link(2)),
        ];
        let runs = vec![
            run(2, "done", 10_000, Some(40_000)),
            run(3, "running", 20_000, None),
            run(4, "done", 15_000, Some(30_000)),
        ];
        let f = build_forest(&input(&threads, &runs, &titles));
        let root = f.roots[0];
        (f, root)
    }

    #[test]
    fn rows_follow_collapse_state() {
        let (f, root) = sample();
        let all = visible_rows(&f, root, &HashSet::new());
        let titles: Vec<_> = all.iter().map(|r| f.nodes[r.node].title.as_str()).collect();
        assert_eq!(titles, vec!["O", "A", "A1", "B"]);
        assert_eq!(all[2].depth, 2);
        let mut collapsed = HashSet::new();
        collapsed.insert(NodeKey::Thread(ThreadId(2)));
        let rows = visible_rows(&f, root, &collapsed);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].collapsed && rows[1].has_children);
    }

    #[test]
    fn graph_places_columns_by_depth_and_centers_parents() {
        let (f, root) = sample();
        let g = graph_layout(&f, root, &HashSet::new());
        assert_eq!(g.cols, 3);
        assert_eq!(g.placed.len(), 4);
        assert_eq!(g.edges.len(), 3);
        let find = |title: &str| {
            *g.placed
                .iter()
                .find(|p| f.nodes[p.node].title == title)
                .unwrap()
        };
        assert_eq!(find("A1").col, 2);
        assert_eq!(find("A1").row, 0.0);
        assert_eq!(find("B").row, 1.0);
        assert_eq!(
            find("A").row,
            0.0,
            "a single child keeps its parent on its row"
        );
        assert_eq!(find("O").row, 0.5, "root between its two children");
        assert_eq!(g.rows, 2.0);
    }

    #[test]
    fn timeline_shows_parallelism() {
        let (f, root) = sample();
        let tl = timeline(&f, root, 50_000);
        assert_eq!(tl.bars.len(), 3);
        assert_eq!(tl.t0, 10_000);
        assert_eq!(tl.t1, 50_000, "the running bar reaches `now`");
        assert_eq!(tl.peak_parallel, 3);
        assert!((tl.frac(30_000) - 0.5).abs() < 1e-6);
        assert!(tl.bars.iter().any(|b| b.active));
        // an empty session
        let empty = timeline(&Forest::default(), 0, 5);
        assert!(empty.bars.is_empty() && empty.span_ms() >= 1);
    }

    #[test]
    fn agent_names_normalize() {
        assert_eq!(agent_id("Claude Code"), "claude");
        assert_eq!(agent_id("gemini (ACP)"), "gemini");
        assert_eq!(agent_id("Antigravity"), "agy");
        assert_eq!(agent_display("codex"), "Codex");
        assert_eq!(agent_display("my-agent"), "my-agent");
    }

    fn ext_proc(
        agent: &str,
        pid: u32,
        started_ms: i64,
        ended_ms: Option<i64>,
        exit_code: Option<i32>,
    ) -> ExternalProc {
        ExternalProc {
            agent: agent.into(),
            pid,
            cmdline_redacted: format!("{agent} -p task-{pid}"),
            cwd: Some(PathBuf::from("C:/repo")),
            model: Some("gemini-3-pro".into()),
            effort: Some("medium".into()),
            started_ms,
            ended_ms,
            exit_code,
            cpu_pct: ended_ms.is_none().then_some(8.5),
            rss_bytes: if ended_ms.is_none() {
                140 * 1024 * 1024
            } else {
                0
            },
        }
    }

    #[test]
    fn forest_attaches_external_nodes_under_thread_and_tab() {
        let mut titles = HashMap::new();
        titles.insert("tab-9".to_string(), "pwsh".to_string());
        // Thread 1 has NO harness delegations, only external agy children.
        let threads = vec![thread(
            1,
            "Sesión Claude",
            AgentKind::ClaudeCode,
            ThreadLink::default(),
        )];
        let mut external: HashMap<NodeKey, Vec<ExternalProc>> = HashMap::new();
        external.insert(
            NodeKey::Thread(ThreadId(1)),
            vec![
                ext_proc("agy", 4100, 10_000, None, None),
                ext_proc("codex", 4101, 12_000, Some(22_000), Some(1)),
            ],
        );
        external.insert(
            NodeKey::Tab("tab-9".into()),
            vec![ext_proc("gemini", 5200, 15_000, Some(20_000), Some(0))],
        );
        let f = build_forest(&ForestInput {
            threads: &threads,
            runs: &[],
            wanted: None,
            now_ms: 30_000,
            tab_titles: &titles,
            external: &external,
        });
        assert_eq!(f.roots.len(), 2);
        let t1_root = f.index_of(&NodeKey::Thread(ThreadId(1))).unwrap();
        assert_eq!(f.nodes[t1_root].children.len(), 2);
        assert!(f.has_active(t1_root));
        assert_eq!(f.default_root(), Some(t1_root));

        let agy_idx = f
            .index_of(&NodeKey::External {
                pid: 4100,
                started_ms: 10_000,
            })
            .unwrap();
        let agy = &f.nodes[agy_idx];
        assert!(agy.is_external());
        assert_eq!(agy.mark, AgentMark::Antigravity);
        assert_eq!(agy.status, NodeStatus::Running);
        assert_eq!(agy.depth, 1);
        assert_eq!(agy.duration_ms(30_000), Some(20_000));

        let codex_idx = f
            .index_of(&NodeKey::External {
                pid: 4101,
                started_ms: 12_000,
            })
            .unwrap();
        assert_eq!(f.nodes[codex_idx].status, NodeStatus::Failed);

        let totals = f.totals(t1_root);
        assert_eq!(totals.runs, 0);
        assert_eq!(totals.external, 2);
        assert_eq!(totals.running, 1);
        assert_eq!(totals.failed, 1);

        // Tree, graph, and timeline all include external nodes.
        let rows = visible_rows(&f, t1_root, &HashSet::new());
        assert_eq!(rows.len(), 3);
        let graph = graph_layout(&f, t1_root, &HashSet::new());
        assert_eq!(graph.placed.len(), 3);
        assert_eq!(graph.edges.len(), 2);
        let tl = timeline(&f, t1_root, 30_000);
        assert_eq!(tl.bars.len(), 2);
        assert_eq!(tl.peak_parallel, 2);
    }

    #[test]
    fn external_nodes_are_capped_at_200_evicting_oldest_finished_first() {
        let mut procs = Vec::new();
        // 150 finished + 80 running = 230 total -> must keep all 80 running + 120 newest finished.
        for i in 0..150u32 {
            procs.push(ext_proc(
                "agy",
                1000 + i,
                i64::from(i) * 100,
                Some(i64::from(i) * 100 + 50),
                Some(0),
            ));
        }
        for i in 0..80u32 {
            procs.push(ext_proc("agy", 5000 + i, 50_000 + i64::from(i), None, None));
        }
        let mut external = HashMap::new();
        external.insert(NodeKey::Tab("t".into()), procs);
        let titles = HashMap::new();
        let f = build_forest(&ForestInput {
            threads: &[],
            runs: &[],
            wanted: None,
            now_ms: 100_000,
            tab_titles: &titles,
            external: &external,
        });
        let root = f.index_of(&NodeKey::Tab("t".into())).unwrap();
        assert_eq!(f.nodes[root].children.len(), MAX_EXTERNAL_PER_SESSION);
        let running_count = f.nodes[root]
            .children
            .iter()
            .filter(|c| f.nodes[**c].status == NodeStatus::Running)
            .count();
        assert_eq!(running_count, 80, "all running processes must be preserved");
        // Oldest finished (pid 1000..1030) were evicted; pid 1030..1150 remain.
        assert!(
            f.index_of(&NodeKey::External {
                pid: 1000,
                started_ms: 0
            })
            .is_none()
        );
        assert!(
            f.index_of(&NodeKey::External {
                pid: 1149,
                started_ms: 14_900
            })
            .is_some()
        );
    }
}
