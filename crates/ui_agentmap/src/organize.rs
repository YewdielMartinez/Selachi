//! How the map organizes the forest for the eye: which nodes are shown (status filter, search,
//! «Ocultar terminados de hace más de X»), in which order (start, recent, duration) and grouped by
//! what (parent, agent, status, project). Also the timeline lanes (one per agent) and the keyboard
//! navigation of the graph. Pure: everything is unit-tested without a window.

use std::collections::HashMap;

use ui_kit::AgentMark;

use crate::fmt;
use crate::prefs::{GroupBy, MapPrefs, SortBy, StatusFilter};
use crate::tree::{
    Bar, Forest, GraphLayout, MAX_DEPTH, MapNode, NodeKey, NodeStatus, Row, Timeline, Totals,
    agent_display, timeline_of,
};

/// What the map shows: one orchestrator or all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Root(usize),
    All,
}

impl Scope {
    pub fn roots(self, forest: &Forest) -> Vec<usize> {
        match self {
            Scope::Root(r) if r < forest.nodes.len() => vec![r],
            Scope::Root(_) => Vec::new(),
            Scope::All => forest.roots.clone(),
        }
    }
}

/// The filters in force (status chip, search box, hide old).
#[derive(Clone, Copy, Debug)]
pub struct Query<'a> {
    pub filter: StatusFilter,
    pub text: &'a str,
    /// Hide finished nodes that ended more than this long ago.
    pub hide_older_ms: Option<i64>,
    pub now_ms: i64,
}

impl Query<'_> {
    /// Something narrows the list.
    pub fn is_active(&self) -> bool {
        self.filter != StatusFilter::All
            || !self.text.trim().is_empty()
            || self.hide_older_ms.is_some()
    }
}

/// Whether a delegation (a non-root node) passes the filters.
pub fn node_matches(node: &MapNode, q: &Query<'_>) -> bool {
    let by_status = match q.filter {
        StatusFilter::All => true,
        StatusFilter::Running => node.status.is_active(),
        StatusFilter::Failed => node.status.is_failure(),
        StatusFilter::Done => matches!(node.status, NodeStatus::Done | NodeStatus::Cancelled),
        StatusFilter::External => node.is_external(),
    };
    if !by_status {
        return false;
    }
    if let Some(max_age) = q.hide_older_ms
        && !node.status.is_active()
        && let Some(end) = node.finished_ms.or(node.started_ms)
        && q.now_ms - end > max_age
    {
        return false;
    }
    let text = q.text.trim();
    if text.is_empty() {
        return true;
    }
    let hay = haystack(node);
    text.to_lowercase()
        .split_whitespace()
        .all(|term| hay.contains(term))
}

/// Everything the search box looks at, lowercased.
fn haystack(node: &MapNode) -> String {
    let mut s = String::new();
    let mut add = |t: &str| {
        s.push_str(t);
        s.push('\n');
    };
    add(&node.title);
    add(&node.agent);
    add(&agent_display(&node.agent));
    for part in [&node.model, &node.effort, &node.profile, &node.launcher]
        .into_iter()
        .flatten()
    {
        add(part);
    }
    for p in [&node.project, &node.worktree].into_iter().flatten() {
        add(&fmt::leaf(p));
    }
    add(node.status.label());
    if let Some(run) = &node.run
        && let Some(prompt) = &run.prompt
    {
        add(prompt);
    }
    if let Some(ext) = node.external() {
        add(&ext.cmdline_redacted);
    }
    s.to_lowercase()
}

/// Per node: it passes the filters or one of its descendants does. `None` when nothing is filtered.
pub fn keep_flags(forest: &Forest, roots: &[usize], q: &Query<'_>) -> Option<Vec<bool>> {
    if !q.is_active() {
        return None;
    }
    fn mark(
        forest: &Forest,
        n: usize,
        depth: usize,
        q: &Query<'_>,
        keep: &mut [bool],
    ) -> bool {
        let node = &forest.nodes[n];
        let mut any = false;
        if depth < MAX_DEPTH {
            for c in &node.children {
                any |= mark(forest, *c, depth + 1, q, keep);
            }
        }
        let own = !node.is_root() && node_matches(node, q);
        keep[n] = own || any;
        keep[n]
    }
    let mut keep = vec![false; forest.nodes.len()];
    for r in roots {
        mark(forest, *r, 0, q, &mut keep);
    }
    Some(keep)
}

fn sort_nodes(forest: &Forest, v: &mut [usize], sort: SortBy, now_ms: i64) {
    let n = |i: usize| &forest.nodes[i];
    match sort {
        SortBy::Start => v.sort_by_key(|i| (n(*i).started_ms.unwrap_or(i64::MAX), *i)),
        SortBy::Recent => v.sort_by_key(|i| (std::cmp::Reverse(n(*i).started_ms.unwrap_or(i64::MIN)), *i)),
        SortBy::Duration => v.sort_by_key(|i| {
            (
                std::cmp::Reverse(n(*i).duration_ms(now_ms).unwrap_or(-1)),
                *i,
            )
        }),
    }
}

/// The children of `n` that survive `keep`, in the order of `sort`.
pub(crate) fn ordered_children(
    forest: &Forest,
    n: usize,
    keep: Option<&[bool]>,
    sort: SortBy,
    now_ms: i64,
) -> Vec<usize> {
    let mut v: Vec<usize> = forest.nodes[n]
        .children
        .iter()
        .copied()
        .filter(|c| keep.is_none_or(|k| k[*c]))
        .collect();
    sort_nodes(forest, &mut v, sort, now_ms);
    v
}

/// The collapse id of a node in [`MapPrefs::collapsed`].
pub fn node_collapse_id(key: &NodeKey) -> String {
    format!("n:{}", key.id_string())
}

/// The collapse id of a group.
pub fn group_collapse_id(prefix: &str, key: &str) -> String {
    format!("g:{prefix}:{key}")
}

/// The header of a group of rows (tree sections, timeline lanes).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupHeader {
    /// Its id in [`MapPrefs::collapsed`].
    pub id: String,
    pub label: String,
    pub count: usize,
    pub running: usize,
    pub failed: usize,
    pub collapsed: bool,
    /// The agent's mark when the group is an agent.
    pub mark: Option<AgentMark>,
}

/// A line of the list: a group header or a node.
#[derive(Clone, Debug, PartialEq)]
pub enum ListRow {
    Group(GroupHeader),
    Node(Row),
}

impl ListRow {
    pub fn node(&self) -> Option<usize> {
        match self {
            ListRow::Node(r) => Some(r.node),
            ListRow::Group(_) => None,
        }
    }
}

/// Every delegation (non-root node) under the scope.
pub fn scope_nodes(forest: &Forest, scope: Scope) -> Vec<usize> {
    let mut out = Vec::new();
    for r in scope.roots(forest) {
        out.extend(forest.subtree(r).into_iter().filter(|n| *n != r));
    }
    out
}

/// Totals over the scope (the header's summary).
pub fn scope_totals(forest: &Forest, scope: Scope) -> Totals {
    let mut t = Totals::default();
    for r in scope.roots(forest) {
        let x = forest.totals(r);
        t.runs += x.runs;
        t.external += x.external;
        t.running += x.running;
        t.waiting += x.waiting;
        t.failed += x.failed;
        t.tokens += x.tokens;
        t.cost_usd += x.cost_usd;
        t.files += x.files;
    }
    t
}

/// How many delegations each chip would show, in [`StatusFilter::ALL`] order.
pub fn filter_counts(forest: &Forest, scope: Scope) -> [usize; 5] {
    let mut c = [0usize; 5];
    for n in scope_nodes(forest, scope) {
        let node = &forest.nodes[n];
        c[0] += 1;
        if node.status.is_active() {
            c[1] += 1;
        }
        if node.status.is_failure() {
            c[2] += 1;
        }
        if matches!(node.status, NodeStatus::Done | NodeStatus::Cancelled) {
            c[3] += 1;
        }
        if node.is_external() {
            c[4] += 1;
        }
    }
    c
}

fn status_bucket(s: NodeStatus) -> (u8, &'static str, &'static str) {
    if s.is_active() {
        (0, "running", "En marcha")
    } else if s.is_failure() {
        (1, "failed", "Fallidos")
    } else if s == NodeStatus::Done {
        (2, "done", "Terminados")
    } else {
        (3, "other", "Cancelados")
    }
}

fn project_key(node: &MapNode) -> (String, String) {
    match node.project.as_deref().or(node.worktree.as_deref()) {
        Some(p) => (
            p.to_string_lossy().replace('\\', "/").to_lowercase(),
            fmt::leaf(p),
        ),
        None => (String::new(), "Sin proyecto".to_string()),
    }
}

fn agent_key(node: &MapNode) -> (String, String) {
    if node.agent.is_empty() {
        ("?".to_string(), "Sin agente".to_string())
    } else {
        (node.agent.clone(), agent_display(&node.agent))
    }
}

fn header_of(
    forest: &Forest,
    id: String,
    label: String,
    nodes: &[usize],
    collapsed: bool,
    mark: Option<AgentMark>,
) -> GroupHeader {
    GroupHeader {
        id,
        label,
        count: nodes.len(),
        running: nodes
            .iter()
            .filter(|n| forest.nodes[**n].status.is_active())
            .count(),
        failed: nodes
            .iter()
            .filter(|n| forest.nodes[**n].status.is_failure())
            .count(),
        collapsed,
        mark,
    }
}

/// Groups `nodes` (already filtered): `(group id, label, order, members)`, ordered for display.
fn group_nodes(
    forest: &Forest,
    nodes: &[usize],
    group: GroupBy,
    sort: SortBy,
    now_ms: i64,
) -> Vec<(String, String, Vec<usize>)> {
    let mut map: HashMap<String, (String, u8, Vec<usize>)> = HashMap::new();
    for n in nodes {
        let node = &forest.nodes[*n];
        let (key, label, order) = match group {
            GroupBy::Agent => {
                let (k, l) = agent_key(node);
                (k, l, 0)
            }
            GroupBy::Project => {
                let (k, l) = project_key(node);
                (k, l, 0)
            }
            GroupBy::Status => {
                let (o, k, l) = status_bucket(node.status);
                (k.to_string(), l.to_string(), o)
            }
            GroupBy::Parent => (String::new(), String::new(), 0),
        };
        map.entry(key)
            .or_insert_with(|| (label, order, Vec::new()))
            .2
            .push(*n);
    }
    let mut groups: Vec<(String, String, u8, Vec<usize>)> = map
        .into_iter()
        .map(|(k, (l, o, v))| (k, l, o, v))
        .collect();
    let active = |v: &[usize]| {
        v.iter()
            .filter(|n| forest.nodes[**n].status.is_active())
            .count()
    };
    groups.sort_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| active(&b.3).cmp(&active(&a.3)))
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    groups
        .into_iter()
        .map(|(k, l, _, mut v)| {
            sort_nodes(forest, &mut v, sort, now_ms);
            (k, l, v)
        })
        .collect()
}

/// The lines of the list view for the current organization.
///
/// * [`GroupBy::Parent`]: the delegation tree (orchestrators with their children). Collapsed nodes
///   hide their children, except while a search text is active (matches must be visible).
/// * Agent / Status / Project: delegations in flat groups, each with a collapsible header.
///
/// With filters active an orchestrator only shows if some delegation under it passes. An empty
/// result means that nothing matches.
pub fn list_rows(forest: &Forest, scope: Scope, prefs: &MapPrefs, q: &Query<'_>) -> Vec<ListRow> {
    let roots = scope.roots(forest);
    let mut out = Vec::new();
    if prefs.group == GroupBy::Parent {
        let keep = keep_flags(forest, &roots, q);
        let ignore_collapse = !q.text.trim().is_empty();
        for root in roots {
            if keep.as_ref().is_some_and(|k| !k[root]) {
                continue;
            }
            push_tree(
                forest,
                root,
                0,
                keep.as_deref(),
                prefs,
                q,
                ignore_collapse,
                &mut out,
            );
        }
        return out;
    }
    let nodes: Vec<usize> = scope_nodes(forest, scope)
        .into_iter()
        .filter(|n| node_matches(&forest.nodes[*n], q))
        .collect();
    let prefix = prefs.group.id_word();
    for (key, label, members) in group_nodes(forest, &nodes, prefs.group, prefs.sort, q.now_ms) {
        let id = group_collapse_id(prefix, &key);
        let collapsed = prefs.is_collapsed(&id);
        let mark = (prefs.group == GroupBy::Agent)
            .then(|| members.first().map(|n| forest.nodes[*n].mark))
            .flatten();
        out.push(ListRow::Group(header_of(
            forest, id, label, &members, collapsed, mark,
        )));
        if !collapsed {
            out.extend(members.into_iter().map(|n| {
                ListRow::Node(Row {
                    node: n,
                    depth: 1,
                    has_children: false,
                    collapsed: false,
                })
            }));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_tree(
    forest: &Forest,
    n: usize,
    depth: usize,
    keep: Option<&[bool]>,
    prefs: &MapPrefs,
    q: &Query<'_>,
    ignore_collapse: bool,
    out: &mut Vec<ListRow>,
) {
    let node = &forest.nodes[n];
    let kids = ordered_children(forest, n, keep, prefs.sort, q.now_ms);
    let collapsed =
        !ignore_collapse && !kids.is_empty() && prefs.is_collapsed(&node_collapse_id(&node.key));
    out.push(ListRow::Node(Row {
        node: n,
        depth,
        has_children: !kids.is_empty(),
        collapsed,
    }));
    if !collapsed && depth < MAX_DEPTH {
        for c in kids {
            push_tree(forest, c, depth + 1, keep, prefs, q, ignore_collapse, out);
        }
    }
}

/// A line of the timeline.
#[derive(Clone, Debug, PartialEq)]
pub enum LaneRow {
    Lane(GroupHeader),
    Bar(Bar),
}

/// The timeline with one lane per agent: header (agent, count, running) and the runs of that agent
/// in the order of `prefs.sort`. The time axis covers every filtered run, collapsed lanes included.
pub fn timeline_lanes(
    forest: &Forest,
    scope: Scope,
    prefs: &MapPrefs,
    q: &Query<'_>,
) -> (Timeline, Vec<LaneRow>) {
    let nodes: Vec<usize> = scope_nodes(forest, scope)
        .into_iter()
        .filter(|n| node_matches(&forest.nodes[*n], q))
        .collect();
    let tl = timeline_of(forest, &nodes, q.now_ms);
    let bar_of: HashMap<usize, Bar> = tl.bars.iter().map(|b| (b.node, *b)).collect();
    let with_bar: Vec<usize> = nodes
        .into_iter()
        .filter(|n| bar_of.contains_key(n))
        .collect();
    let mut rows = Vec::new();
    for (key, label, members) in group_nodes(forest, &with_bar, GroupBy::Agent, prefs.sort, q.now_ms)
    {
        let id = group_collapse_id("lane", &key);
        let collapsed = prefs.is_collapsed(&id);
        let mark = members.first().map(|n| forest.nodes[*n].mark);
        rows.push(LaneRow::Lane(header_of(
            forest, id, label, &members, collapsed, mark,
        )));
        if !collapsed {
            rows.extend(
                members
                    .iter()
                    .filter_map(|n| bar_of.get(n).copied())
                    .map(LaneRow::Bar),
            );
        }
    }
    (tl, rows)
}

/// A direction of the keyboard navigation in the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// To the parent.
    Left,
    /// To the first child.
    Right,
    /// To the previous sibling in the column.
    Up,
    Down,
}

/// The node reached from `from` (`None`: from nothing, the first one) going `dir` in `layout`.
pub fn nav_target(
    forest: &Forest,
    layout: &GraphLayout,
    from: Option<&NodeKey>,
    dir: Dir,
) -> Option<usize> {
    let Some(from) = from else {
        return layout.placed.first().map(|p| p.node);
    };
    let slot = layout
        .placed
        .iter()
        .position(|p| &forest.nodes[p.node].key == from)?;
    let cur = layout.placed[slot];
    match dir {
        Dir::Left => layout
            .edges
            .iter()
            .find(|(_, c)| *c == slot)
            .map(|(p, _)| layout.placed[*p].node),
        Dir::Right => layout
            .edges
            .iter()
            .filter(|(p, _)| *p == slot)
            .map(|(_, c)| layout.placed[*c])
            .min_by(|a, b| a.row.total_cmp(&b.row))
            .map(|p| p.node),
        Dir::Up | Dir::Down => {
            let same_col = layout.placed.iter().filter(|p| p.col == cur.col);
            if dir == Dir::Up {
                same_col
                    .filter(|p| p.row < cur.row)
                    .max_by(|a, b| a.row.total_cmp(&b.row))
                    .map(|p| p.node)
            } else {
                same_col
                    .filter(|p| p.row > cur.row)
                    .min_by(|a, b| a.row.total_cmp(&b.row))
                    .map(|p| p.node)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use agents_driver::{AgentKind, PermissionMode};
    use chat_core::{ThreadId, ThreadLink, ThreadRecord, ThreadStatus};

    use super::*;
    use crate::registry::RunInfo;
    use crate::tree::{ForestInput, build_forest};

    fn thread(id: i64, title: &str, agent: AgentKind, parent: Option<i64>, project: &str) -> ThreadRecord {
        ThreadRecord {
            id: ThreadId(id),
            title: title.into(),
            project_root: project.into(),
            worktree_path: None,
            agent,
            model: Some("m".into()),
            effort: None,
            permission: PermissionMode::ReadOnly,
            status: ThreadStatus::Done,
            created_ms: 1_000 * id,
            updated_ms: 1_000 * id,
            agent_session_id: None,
            archived: false,
            checkpoint_before_turn: false,
            link: ThreadLink {
                parent_thread_id: parent.map(ThreadId),
                parent_tab: None,
                depth: u32::from(parent.is_some()),
                delegated_by: parent.map(|_| "mcp".into()),
            },
        }
    }

    fn run(
        id: i64,
        agent: &str,
        status: &str,
        started: i64,
        finished: Option<i64>,
        prompt: &str,
    ) -> RunInfo {
        RunInfo::from_json(&format!(
            r#"{{"run_id":"{id}","agent":"{agent}","status":"{status}","started_ms":{started},"prompt":"{prompt}"{fin}}}"#,
            fin = finished
                .map(|f| format!(r#","finished_ms":{f}"#))
                .unwrap_or_default()
        ))
        .unwrap()
    }

    /// 1 orchestrator (codex) -> 2 done (codex, p1), 3 running (agy, p1), 4 failed (agy, p2),
    /// 5 cancelled (codex, p2); 6 is a second orchestrator with 7 (claude, done).
    fn forest() -> Forest {
        let threads = vec![
            thread(1, "Orquestador", AgentKind::ClaudeCode, None, "C:/p1"),
            thread(2, "t2", AgentKind::Codex, Some(1), "C:/p1"),
            thread(3, "t3", AgentKind::Acp { command: "agy acp".into() }, Some(1), "C:/p1"),
            thread(4, "t4", AgentKind::Acp { command: "agy acp".into() }, Some(1), "C:/p2"),
            thread(5, "t5", AgentKind::Codex, Some(1), "C:/p2"),
            thread(6, "Otro orquestador", AgentKind::ClaudeCode, None, "C:/p3"),
            thread(7, "t7", AgentKind::ClaudeCode, Some(6), "C:/p3"),
        ];
        let runs = vec![
            run(2, "codex", "done", 10_000, Some(70_000), "Revisa los tests de auth"),
            run(3, "agy", "running", 20_000, None, "Documenta los endpoints"),
            run(4, "agy", "error", 30_000, Some(35_000), "Corrige el lint"),
            run(5, "codex", "cancelled", 40_000, Some(41_000), "Prueba con Playwright"),
            run(7, "claude", "done", 50_000, Some(60_000), "Revisa la migración"),
        ];
        let titles = HashMap::new();
        let ext = HashMap::new();
        build_forest(&ForestInput {
            threads: &threads,
            runs: &runs,
            wanted: None,
            now_ms: 100_000,
            tab_titles: &titles,
            external: &ext,
        })
    }

    fn q(filter: StatusFilter, text: &str) -> Query<'_> {
        Query {
            filter,
            text,
            hide_older_ms: None,
            now_ms: 100_000,
        }
    }

    fn titles(f: &Forest, rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                ListRow::Group(g) => format!("[{}:{}]", g.label, g.count),
                ListRow::Node(n) => format!("{}{}", "-".repeat(n.depth), f.nodes[n.node].title),
            })
            .collect()
    }

    fn root_of(f: &Forest, id: i64) -> usize {
        f.index_of(&NodeKey::Thread(ThreadId(id))).unwrap()
    }

    #[test]
    fn the_tree_lists_the_root_and_its_delegations_in_start_order() {
        let f = forest();
        let prefs = MapPrefs::default();
        let rows = list_rows(&f, Scope::Root(root_of(&f, 1)), &prefs, &q(StatusFilter::All, ""));
        assert_eq!(
            titles(&f, &rows),
            vec![
                "Orquestador",
                "-Revisa los tests de auth",
                "-Documenta los endpoints",
                "-Corrige el lint",
                "-Prueba con Playwright"
            ]
        );
    }

    #[test]
    fn sorting_by_recent_and_by_duration() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let mut prefs = MapPrefs {
            sort: SortBy::Recent,
            ..MapPrefs::default()
        };
        let rows = list_rows(&f, root, &prefs, &q(StatusFilter::All, ""));
        assert_eq!(
            titles(&f, &rows)[1..],
            [
                "-Prueba con Playwright",
                "-Corrige el lint",
                "-Documenta los endpoints",
                "-Revisa los tests de auth"
            ]
        );
        prefs.sort = SortBy::Duration;
        let rows = list_rows(&f, root, &prefs, &q(StatusFilter::All, ""));
        // running (80 s so far) beats done (60 s), failed (5 s) and cancelled (1 s)
        assert_eq!(
            titles(&f, &rows)[1..],
            [
                "-Documenta los endpoints",
                "-Revisa los tests de auth",
                "-Corrige el lint",
                "-Prueba con Playwright"
            ]
        );
    }

    #[test]
    fn status_filters_keep_the_path_to_the_matches() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let prefs = MapPrefs::default();
        let rows = list_rows(&f, root, &prefs, &q(StatusFilter::Running, ""));
        assert_eq!(titles(&f, &rows), vec!["Orquestador", "-Documenta los endpoints"]);
        let rows = list_rows(&f, root, &prefs, &q(StatusFilter::Failed, ""));
        assert_eq!(titles(&f, &rows), vec!["Orquestador", "-Corrige el lint"]);
        let rows = list_rows(&f, root, &prefs, &q(StatusFilter::Done, ""));
        assert_eq!(
            titles(&f, &rows),
            vec!["Orquestador", "-Revisa los tests de auth", "-Prueba con Playwright"]
        );
        // nothing external here: no rows at all (the view shows «Sin resultados»)
        assert!(list_rows(&f, root, &prefs, &q(StatusFilter::External, "")).is_empty());
        let counts = filter_counts(&f, root);
        assert_eq!(counts, [4, 1, 1, 2, 0]);
    }

    #[test]
    fn search_matches_title_agent_model_project_and_prompt() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let prefs = MapPrefs::default();
        let hit = |text: &str| -> Vec<String> {
            let rows = list_rows(&f, root, &prefs, &q(StatusFilter::All, text));
            titles(&f, &rows).into_iter().skip(1).collect()
        };
        assert_eq!(hit("lint"), vec!["-Corrige el lint"]);
        assert_eq!(hit("ANTIGRAVITY"), vec!["-Documenta los endpoints", "-Corrige el lint"]);
        assert_eq!(hit("p2"), vec!["-Corrige el lint", "-Prueba con Playwright"]);
        assert_eq!(hit("tests auth"), vec!["-Revisa los tests de auth"]);
        assert!(hit("zzz").is_empty());
        // terms combine with AND
        assert_eq!(hit("agy lint"), vec!["-Corrige el lint"]);
    }

    #[test]
    fn hide_older_drops_only_finished_runs_that_ended_long_ago() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let prefs = MapPrefs::default();
        let query = Query {
            filter: StatusFilter::All,
            text: "",
            hide_older_ms: Some(40_000),
            now_ms: 100_000,
        };
        let rows = list_rows(&f, root, &prefs, &query);
        // the running one stays whatever its age; 2 ended 30 s ago, 4 65 s ago, 5 59 s ago
        assert_eq!(
            titles(&f, &rows),
            vec!["Orquestador", "-Revisa los tests de auth", "-Documenta los endpoints"]
        );
    }

    #[test]
    fn collapsed_nodes_hide_children_unless_searching() {
        let f = forest();
        let r = root_of(&f, 1);
        let mut prefs = MapPrefs::default();
        prefs.toggle_collapsed(&node_collapse_id(&f.nodes[r].key));
        let rows = list_rows(&f, Scope::Root(r), &prefs, &q(StatusFilter::All, ""));
        assert_eq!(titles(&f, &rows), vec!["Orquestador"]);
        match &rows[0] {
            ListRow::Node(n) => assert!(n.collapsed && n.has_children),
            other => panic!("{other:?}"),
        }
        let rows = list_rows(&f, Scope::Root(r), &prefs, &q(StatusFilter::All, "lint"));
        assert_eq!(titles(&f, &rows).len(), 2, "a search expands what it matches");
    }

    #[test]
    fn all_sessions_stack_the_orchestrators() {
        let f = forest();
        let rows = list_rows(&f, Scope::All, &MapPrefs::default(), &q(StatusFilter::All, ""));
        let t = titles(&f, &rows);
        assert!(t.contains(&"Orquestador".to_string()));
        assert!(t.contains(&"Otro orquestador".to_string()));
        assert_eq!(t.len(), 7);
        // a filter hides the sessions without a match
        let rows = list_rows(&f, Scope::All, &MapPrefs::default(), &q(StatusFilter::Running, ""));
        assert_eq!(titles(&f, &rows), vec!["Orquestador", "-Documenta los endpoints"]);
        assert_eq!(scope_totals(&f, Scope::All).runs, 5);
    }

    #[test]
    fn grouping_by_agent_status_and_project() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let all = q(StatusFilter::All, "");
        let mut prefs = MapPrefs {
            group: GroupBy::Agent,
            ..MapPrefs::default()
        };
        // the running agy group goes first, then by name
        let rows = list_rows(&f, root, &prefs, &all);
        assert_eq!(
            titles(&f, &rows),
            vec![
                "[Antigravity:2]",
                "-Documenta los endpoints",
                "-Corrige el lint",
                "[Codex:2]",
                "-Revisa los tests de auth",
                "-Prueba con Playwright"
            ]
        );
        prefs.group = GroupBy::Status;
        let rows = list_rows(&f, root, &prefs, &all);
        assert_eq!(
            titles(&f, &rows)
                .into_iter()
                .filter(|t| t.starts_with('['))
                .collect::<Vec<_>>(),
            vec!["[En marcha:1]", "[Fallidos:1]", "[Terminados:1]", "[Cancelados:1]"]
        );
        prefs.group = GroupBy::Project;
        let rows = list_rows(&f, root, &prefs, &all);
        assert_eq!(
            titles(&f, &rows)
                .into_iter()
                .filter(|t| t.starts_with('['))
                .collect::<Vec<_>>(),
            vec!["[p1:2]", "[p2:2]"]
        );
    }

    #[test]
    fn a_collapsed_group_hides_its_rows_and_remembers_it() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let mut prefs = MapPrefs {
            group: GroupBy::Agent,
            ..MapPrefs::default()
        };
        prefs.toggle_collapsed(&group_collapse_id("agent", "codex"));
        let rows = list_rows(&f, root, &prefs, &q(StatusFilter::All, ""));
        let t = titles(&f, &rows);
        assert!(t.contains(&"[Codex:2]".to_string()));
        assert!(!t.contains(&"-Revisa los tests de auth".to_string()));
        match rows.iter().find_map(|r| match r {
            ListRow::Group(g) if g.label == "Codex" => Some(g),
            _ => None,
        }) {
            Some(g) => assert!(g.collapsed && g.count == 2),
            None => panic!("no codex group"),
        }
        // the choice survives a round trip of the preferences
        let back = MapPrefs::from_json(&prefs.to_json());
        assert!(back.is_collapsed(&group_collapse_id("agent", "codex")));
    }

    #[test]
    fn the_timeline_has_a_lane_per_agent() {
        let f = forest();
        let root = Scope::Root(root_of(&f, 1));
        let mut prefs = MapPrefs::default();
        let (tl, rows) = timeline_lanes(&f, root, &prefs, &q(StatusFilter::All, ""));
        assert_eq!(tl.bars.len(), 4);
        let lanes: Vec<&GroupHeader> = rows
            .iter()
            .filter_map(|r| match r {
                LaneRow::Lane(g) => Some(g),
                LaneRow::Bar(_) => None,
            })
            .collect();
        assert_eq!(lanes.len(), 2);
        assert_eq!((lanes[0].label.as_str(), lanes[0].running), ("Antigravity", 1));
        assert_eq!(rows.len(), 2 + 4);
        // collapsing a lane keeps the axis but drops its bars
        prefs.toggle_collapsed(&group_collapse_id("lane", "agy"));
        let (tl2, rows2) = timeline_lanes(&f, root, &prefs, &q(StatusFilter::All, ""));
        assert_eq!(tl2, tl);
        assert_eq!(rows2.len(), 2 + 2);
    }

    #[test]
    fn keyboard_navigation_moves_between_parent_children_and_siblings() {
        let f = forest();
        let r = root_of(&f, 1);
        let layout = crate::tree::graph_layout_of(
            &f,
            &[r],
            &|_| false,
            None,
            SortBy::Start,
            100_000,
        );
        let key = |id: i64| NodeKey::Thread(ThreadId(id));
        let id_of = |n: usize| f.nodes[n].key.clone();
        // from the root: right goes to the first child (2), down/up do nothing
        assert_eq!(nav_target(&f, &layout, Some(&key(1)), Dir::Right).map(id_of), Some(key(2)));
        assert_eq!(nav_target(&f, &layout, Some(&key(1)), Dir::Down), None);
        // siblings in the column
        assert_eq!(nav_target(&f, &layout, Some(&key(2)), Dir::Down).map(id_of), Some(key(3)));
        assert_eq!(nav_target(&f, &layout, Some(&key(3)), Dir::Up).map(id_of), Some(key(2)));
        assert_eq!(nav_target(&f, &layout, Some(&key(2)), Dir::Up), None);
        // back to the parent
        assert_eq!(nav_target(&f, &layout, Some(&key(4)), Dir::Left).map(id_of), Some(key(1)));
        // from nothing: the first node
        assert_eq!(nav_target(&f, &layout, None, Dir::Down).map(id_of), Some(key(1)));
    }
}
