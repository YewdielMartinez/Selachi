//! The pure data model of the panel: what a [`Collected`] sample of the machine becomes once it is
//! attributed and grouped as `agent -> owner (tab / thread) -> process rows`, plus the flat list of
//! listening ports. No gpui here, so all of it is unit-testable.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use proc_svc::{
    OwnerId, OwnerReport, PortInfo, ProcInfo, ProcState, Proto, RuntimeKind, TrackedProc,
    UntrackedProc, classify,
};

use crate::fmt::{format_bytes, plural, shorten_command, title_case};

/// Longest short command kept in a row (the view truncates further with an ellipsis).
const MAX_COMMAND_CHARS: usize = 90;

/// Everything one sampling pass produced. Built off the UI thread by a
/// [`ProcSource`](crate::ProcSource); turned into a [`Snapshot`] by [`build_snapshot`].
#[derive(Clone, Debug, Default)]
pub struct Collected {
    /// Attribution report per owner (processes inside our jobs).
    pub reports: Vec<OwnerReport>,
    /// Processes outside our jobs attributed by heuristic ("atribución probable").
    pub untracked: Vec<UntrackedProc>,
    /// Every listening TCP / bound UDP socket of the machine.
    pub ports: Vec<PortInfo>,
    /// Every process of the machine (names and sizes for the ports view, trees for plans).
    pub procs: Vec<ProcInfo>,
    /// CPU % per pid, normalised to the whole machine (Task Manager style). Empty on the first
    /// sample after the panel opens: there is no baseline yet.
    pub cpu: HashMap<u32, f32>,
    /// When the sample was taken.
    pub now: Option<SystemTime>,
    /// Owners tracked by process tree because Windows would not put their shell in a Job Object
    /// (`proc_svc::TrackingMode::TreeOnly`).
    pub tree_owners: Vec<OwnerId>,
}

/// State badge of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowState {
    /// The owner's shell / agent is alive.
    Active,
    /// Everything that started it exited; it survived.
    Orphan,
    /// An orphan that still holds a listening port.
    PortZombie,
}

impl RowState {
    /// Text of the badge.
    pub const fn label(self) -> &'static str {
        match self {
            RowState::Active => "activo",
            RowState::Orphan => "huérfano",
            RowState::PortZombie => "zombi de puerto",
        }
    }

    /// Orphans and port zombies are what "Limpiar huérfanos" removes.
    pub const fn is_orphan(self) -> bool {
        !matches!(self, RowState::Active)
    }
}

impl From<ProcState> for RowState {
    fn from(s: ProcState) -> Self {
        match s {
            ProcState::Active => RowState::Active,
            ProcState::Orphan => RowState::Orphan,
            ProcState::PortZombie => RowState::PortZombie,
        }
    }
}

/// A listening port shown as a pill on its process row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PortPill {
    pub port: u16,
    pub tcp: bool,
    /// Reachable beyond loopback.
    pub exposed: bool,
}

impl PortPill {
    /// `http://localhost:PORT` for TCP ports (what the arrow opens), `None` for UDP.
    pub fn url(&self) -> Option<String> {
        self.tcp.then(|| format!("http://localhost:{}", self.port))
    }

    /// `:5173`, or `:5353 udp`.
    pub fn label(&self) -> String {
        if self.tcp {
            format!(":{}", self.port)
        } else {
            format!(":{} udp", self.port)
        }
    }
}

/// Deduplicated pills (IPv4 + IPv6 listeners of the same port collapse into one).
pub fn pills(ports: &[PortInfo]) -> Vec<PortPill> {
    let mut out: Vec<PortPill> = Vec::new();
    for p in ports {
        let tcp = p.proto == Proto::Tcp;
        match out.iter_mut().find(|q| q.port == p.port && q.tcp == tcp) {
            Some(q) => q.exposed |= p.is_exposed(),
            None => out.push(PortPill {
                port: p.port,
                tcp,
                exposed: p.is_exposed(),
            }),
        }
    }
    out.sort_by_key(|p| (!p.tcp, p.port));
    out
}

/// One process of the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcRow {
    pub pid: u32,
    pub ppid: u32,
    pub start: Option<SystemTime>,
    /// Image name (`node.exe`).
    pub name: String,
    /// Short command (`node vite.js --port 5173`).
    pub command: String,
    /// Full command line, when readable.
    pub full_command: Option<String>,
    pub runtime: RuntimeKind,
    /// Framework / subcommand tag (`vite`, `watch`).
    pub tag: Option<&'static str>,
    pub ports: Vec<PortPill>,
    /// Working set in bytes.
    pub ram: u64,
    /// CPU % of the whole machine; `None` until a second sample exists.
    pub cpu: Option<f32>,
    pub uptime: Option<Duration>,
    pub state: RowState,
    /// Attributed by heuristic, not by job membership: "atribución probable".
    pub probable: bool,
    /// The shell / agent registered for the owner (its tab).
    pub is_root: bool,
    /// The process could not be inspected: it cannot be stopped from here.
    pub access_denied: bool,
    pub owner: Option<OwnerId>,
    /// Nesting inside its owner group (0 = top).
    pub depth: u8,
}

impl ProcRow {
    /// Whether "Detener" is offered: not the tab's own shell and inspectable.
    pub fn can_stop(&self) -> bool {
        !self.is_root && !self.access_denied
    }
}

/// Which group of rows an [`OwnerGroup`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GroupKey {
    /// A tab / thread of Selachi (job).
    Owner(OwnerId),
    /// An agent running outside Selachi, identified by the pid of its process.
    External(u32),
}

impl GroupKey {
    /// Stable key for collapsed state.
    pub fn id(self) -> String {
        match self {
            GroupKey::Owner(o) => format!("owner-{o}"),
            GroupKey::External(p) => format!("ext-{p}"),
        }
    }
}

/// A tab / thread (or an external agent) and its processes.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnerGroup {
    pub key: GroupKey,
    /// Tab title, or "Fuera de Selachi".
    pub label: String,
    /// Directory name or `claude · pid 4321`.
    pub detail: Option<String>,
    /// Every row of the group is a heuristic attribution.
    pub probable: bool,
    /// Tracked by process tree (no Job Object): `proc_svc::TrackingMode::TreeOnly`.
    pub tree_tracked: bool,
    pub rows: Vec<ProcRow>,
}

impl OwnerGroup {
    pub fn ram(&self) -> u64 {
        self.rows.iter().map(|r| r.ram).sum()
    }

    pub fn orphan_count(&self) -> usize {
        self.rows.iter().filter(|r| r.state.is_orphan()).count()
    }
}

/// An agent (Claude, Codex...) and the tabs it runs in.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentGroup {
    /// Lowercase id (`claude`); empty for "no agent".
    pub id: String,
    /// Display name (`Claude`, `Sin agente`).
    pub name: String,
    pub owners: Vec<OwnerGroup>,
}

impl AgentGroup {
    pub fn process_count(&self) -> usize {
        self.owners.iter().map(|o| o.rows.len()).sum()
    }

    pub fn ram(&self) -> u64 {
        self.owners.iter().map(OwnerGroup::ram).sum()
    }

    pub fn orphan_count(&self) -> usize {
        self.owners.iter().map(OwnerGroup::orphan_count).sum()
    }
}

/// Who owns a listening port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A process of one of our jobs.
    Tracked {
        agent: Option<String>,
        label: String,
        owner: OwnerId,
    },
    /// Heuristic: a descendant of an agent running outside Selachi.
    Probable { agent: String },
    /// Anything else on the machine.
    External,
}

impl Origin {
    fn rank(&self) -> u8 {
        match self {
            Origin::Tracked { .. } => 0,
            Origin::Probable { .. } => 1,
            Origin::External => 2,
        }
    }

    /// `Claude · api`, `Claude (probable)`, `Fuera de Selachi`.
    pub fn label(&self) -> String {
        match self {
            Origin::Tracked {
                agent: Some(a),
                label,
                ..
            } => format!("{} · {label}", title_case(a)),
            Origin::Tracked {
                agent: None, label, ..
            } => label.clone(),
            Origin::Probable { agent } => format!("{} (probable)", title_case(agent)),
            Origin::External => "Fuera de Selachi".to_string(),
        }
    }
}

/// A row of the "Puertos" sub-view.
#[derive(Clone, Debug, PartialEq)]
pub struct PortRow {
    pub port: u16,
    pub tcp: bool,
    pub exposed: bool,
    pub pid: u32,
    pub process: String,
    pub command: String,
    pub runtime: RuntimeKind,
    pub tag: Option<&'static str>,
    pub ram: u64,
    pub origin: Origin,
    /// Orphan / zombie state for tracked processes.
    pub state: Option<RowState>,
    pub access_denied: bool,
    pub start: Option<SystemTime>,
}

impl PortRow {
    /// `http://localhost:PORT` for TCP.
    pub fn url(&self) -> Option<String> {
        self.tcp.then(|| format!("http://localhost:{}", self.port))
    }

    pub fn can_stop(&self) -> bool {
        !self.access_denied && self.pid > 4
    }

    /// Part of Selachi's own attribution (tracked or probable), as opposed to the rest of the
    /// machine.
    pub fn is_agent_related(&self) -> bool {
        !matches!(self.origin, Origin::External)
    }
}

/// What the status bar shows: `3 huérfanos · 1,2 GB`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrphanSummary {
    pub count: usize,
    pub ram_bytes: u64,
}

impl OrphanSummary {
    /// `None` when there are no orphans (the pill is hidden).
    pub fn label(&self) -> Option<String> {
        (self.count > 0).then(|| {
            format!(
                "{} · {}",
                plural(self.count, "huérfano", "huérfanos"),
                format_bytes(self.ram_bytes)
            )
        })
    }
}

/// The grouped view of one sample.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub agents: Vec<AgentGroup>,
    pub ports: Vec<PortRow>,
    pub summary: OrphanSummary,
    pub process_count: usize,
    pub ram_total: u64,
    /// The CPU column has values (a baseline existed).
    pub has_cpu: bool,
    pub at: Option<SystemTime>,
}

impl Snapshot {
    /// Every row of every group.
    pub fn rows(&self) -> impl Iterator<Item = &ProcRow> {
        self.agents
            .iter()
            .flat_map(|a| a.owners.iter())
            .flat_map(|o| o.rows.iter())
    }

    pub fn find_row(&self, pid: u32) -> Option<&ProcRow> {
        self.rows().find(|r| r.pid == pid)
    }

    pub fn is_empty(&self) -> bool {
        self.process_count == 0
    }
}

// ---------------------------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------------------------

fn uptime_of(start: Option<SystemTime>, now: SystemTime) -> Option<Duration> {
    start.and_then(|s| now.duration_since(s).ok())
}

fn tracked_row(p: &TrackedProc, owner: OwnerId, c: &Collected, now: SystemTime) -> ProcRow {
    ProcRow {
        pid: p.info.pid,
        ppid: p.info.ppid,
        start: p.info.start_time,
        name: p.info.name.clone(),
        command: shorten_command(&p.info.name, p.info.cmdline.as_deref(), MAX_COMMAND_CHARS),
        full_command: p.info.cmdline.clone(),
        runtime: p.runtime.kind,
        tag: p.runtime.tag,
        ports: pills(&p.ports),
        ram: p.info.working_set,
        cpu: c.cpu.get(&p.info.pid).copied(),
        uptime: uptime_of(p.info.start_time, now),
        state: p.state.into(),
        probable: false,
        is_root: p.is_root,
        access_denied: p.info.access_denied,
        owner: Some(owner),
        depth: 0,
    }
}

fn untracked_row(u: &UntrackedProc, c: &Collected, now: SystemTime) -> ProcRow {
    ProcRow {
        pid: u.info.pid,
        ppid: u.info.ppid,
        start: u.info.start_time,
        name: u.info.name.clone(),
        command: shorten_command(&u.info.name, u.info.cmdline.as_deref(), MAX_COMMAND_CHARS),
        full_command: u.info.cmdline.clone(),
        runtime: u.runtime.kind,
        tag: u.runtime.tag,
        ports: pills(&u.ports),
        ram: u.info.working_set,
        cpu: c.cpu.get(&u.info.pid).copied(),
        uptime: uptime_of(u.info.start_time, now),
        state: RowState::Active,
        probable: true,
        is_root: false,
        access_denied: u.info.access_denied,
        owner: None,
        depth: 0,
    }
}

/// Orders rows as a tree (a child right below its parent) and fills [`ProcRow::depth`]. A parent
/// only counts if it started before its child (pid reuse guard). Roots (the tab's shell) first.
pub fn order_tree(rows: Vec<ProcRow>) -> Vec<ProcRow> {
    let index: HashMap<u32, usize> = rows.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
    let parent_of = |r: &ProcRow| -> Option<usize> {
        let &pi = index.get(&r.ppid)?;
        let parent = &rows[pi];
        if parent.pid == r.pid {
            return None;
        }
        match (parent.start, r.start) {
            (Some(a), Some(b)) if a >= b => None,
            _ => Some(pi),
        }
    };
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    let mut tops: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        match parent_of(r) {
            Some(p) => children[p].push(i),
            None => tops.push(i),
        }
    }
    let key = |i: &usize| (!rows[*i].is_root, rows[*i].pid);
    tops.sort_by_key(key);
    for c in &mut children {
        c.sort_by_key(key);
    }
    let mut order: Vec<(usize, u8)> = Vec::with_capacity(rows.len());
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, u8)> = tops.iter().rev().map(|&i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        order.push((i, depth));
        stack.extend(
            children[i]
                .iter()
                .rev()
                .map(|&c| (c, depth.saturating_add(1))),
        );
    }
    // A cycle (impossible with the start-time guard, but cheap to survive) leaves rows unvisited.
    for i in 0..rows.len() {
        if seen.insert(i) {
            order.push((i, 0));
        }
    }
    let mut slots: Vec<Option<ProcRow>> = rows.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|(i, depth)| {
            slots[i].take().map(|mut r| {
                r.depth = depth;
                r
            })
        })
        .collect()
}

fn agent_entry<'a>(agents: &'a mut Vec<AgentGroup>, agent: Option<&str>) -> &'a mut AgentGroup {
    let id = agent.map(|a| a.trim().to_lowercase()).unwrap_or_default();
    let ix = match agents.iter().position(|g| g.id == id) {
        Some(ix) => ix,
        None => {
            let name = if id.is_empty() {
                "Sin agente".to_string()
            } else {
                title_case(&id)
            };
            agents.push(AgentGroup {
                id,
                name,
                owners: Vec::new(),
            });
            agents.len() - 1
        }
    };
    &mut agents[ix]
}

fn dir_label(cwd: &Option<PathBuf>) -> Option<String> {
    let p = cwd.as_ref()?;
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| Some(p.display().to_string()))
}

/// Attributes and groups a sample. Pure.
pub fn build_snapshot(c: &Collected) -> Snapshot {
    let now = c.now.unwrap_or_else(SystemTime::now);
    let by_pid: HashMap<u32, &ProcInfo> = c.procs.iter().map(|p| (p.pid, p)).collect();
    let mut agents: Vec<AgentGroup> = Vec::new();
    let mut origins: HashMap<u32, (Origin, Option<RowState>)> = HashMap::new();

    for rep in &c.reports {
        let rows: Vec<ProcRow> = rep
            .processes
            .iter()
            .map(|p| tracked_row(p, rep.owner, c, now))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let label = if rep.meta.label.trim().is_empty() {
            format!("Pestaña {}", rep.owner)
        } else {
            rep.meta.label.clone()
        };
        for r in &rows {
            origins.insert(
                r.pid,
                (
                    Origin::Tracked {
                        agent: rep.meta.agent.clone(),
                        label: label.clone(),
                        owner: rep.owner,
                    },
                    Some(r.state),
                ),
            );
        }
        agent_entry(&mut agents, rep.meta.agent.as_deref())
            .owners
            .push(OwnerGroup {
                key: GroupKey::Owner(rep.owner),
                label,
                detail: dir_label(&rep.meta.cwd),
                probable: false,
                tree_tracked: c.tree_owners.contains(&rep.owner),
                rows: order_tree(rows),
            });
    }

    let mut external: Vec<(&str, u32, Vec<ProcRow>)> = Vec::new();
    for u in &c.untracked {
        origins.insert(
            u.info.pid,
            (
                Origin::Probable {
                    agent: u.agent.clone(),
                },
                None,
            ),
        );
        let row = untracked_row(u, c, now);
        match external
            .iter_mut()
            .find(|(a, pid, _)| *a == u.agent && *pid == u.agent_pid)
        {
            Some((_, _, rows)) => rows.push(row),
            None => external.push((&u.agent, u.agent_pid, vec![row])),
        }
    }
    for (agent, agent_pid, rows) in external {
        agent_entry(&mut agents, Some(agent))
            .owners
            .push(OwnerGroup {
                key: GroupKey::External(agent_pid),
                label: "Fuera de Selachi".to_string(),
                detail: Some(format!("{agent} · pid {agent_pid}")),
                probable: true,
                tree_tracked: false,
                rows: order_tree(rows),
            });
    }

    // Named agents alphabetically, "Sin agente" last; owners by key (tabs first, then external).
    agents.sort_by(|a, b| (a.id.is_empty(), &a.id).cmp(&(b.id.is_empty(), &b.id)));
    for a in &mut agents {
        a.owners.sort_by_key(|o| match o.key {
            GroupKey::Owner(id) => (0, id),
            GroupKey::External(pid) => (1, u64::from(pid)),
        });
    }

    let (mut count, mut ram_bytes) = (0usize, 0u64);
    let (mut process_count, mut ram_total, mut has_cpu) = (0usize, 0u64, false);
    for r in agents
        .iter()
        .flat_map(|a| a.owners.iter())
        .flat_map(|o| o.rows.iter())
    {
        process_count += 1;
        ram_total += r.ram;
        has_cpu |= r.cpu.is_some();
        if !r.probable && r.state.is_orphan() {
            count += 1;
            ram_bytes += r.ram;
        }
    }

    Snapshot {
        agents,
        ports: build_port_rows(c, &by_pid, &origins),
        summary: OrphanSummary { count, ram_bytes },
        process_count,
        ram_total,
        has_cpu,
        at: Some(now),
    }
}

fn build_port_rows(
    c: &Collected,
    by_pid: &HashMap<u32, &ProcInfo>,
    origins: &HashMap<u32, (Origin, Option<RowState>)>,
) -> Vec<PortRow> {
    let mut rows: Vec<PortRow> = Vec::new();
    for p in &c.ports {
        let tcp = p.proto == Proto::Tcp;
        let (origin, state) = origins
            .get(&p.pid)
            .cloned()
            .unwrap_or((Origin::External, None));
        // UDP sockets of unrelated processes are noise (mDNS, SSDP...).
        if !tcp && matches!(origin, Origin::External) {
            continue;
        }
        if let Some(r) = rows
            .iter_mut()
            .find(|r| r.pid == p.pid && r.port == p.port && r.tcp == tcp)
        {
            r.exposed |= p.is_exposed();
            continue;
        }
        let info = by_pid.get(&p.pid);
        let name = info.map_or_else(|| format!("pid {}", p.pid), |i| i.name.clone());
        let cmdline = info.and_then(|i| i.cmdline.as_deref());
        let runtime = classify(&name, cmdline, info.and_then(|i| i.exe_path.as_deref()));
        rows.push(PortRow {
            port: p.port,
            tcp,
            exposed: p.is_exposed(),
            pid: p.pid,
            command: shorten_command(&name, cmdline, MAX_COMMAND_CHARS),
            process: name,
            runtime: runtime.kind,
            tag: runtime.tag,
            ram: info.map_or(0, |i| i.working_set),
            origin,
            state,
            access_denied: info.is_none_or(|i| i.access_denied),
            start: info.and_then(|i| i.start_time),
        });
    }
    rows.sort_by_key(|r| (r.origin.rank(), r.port, r.pid));
    rows
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod fixtures {
    //! Builders shared by the tests of this crate.

    use std::net::{IpAddr, Ipv4Addr};
    use std::time::UNIX_EPOCH;

    use proc_svc::{OwnerMeta, PortState, Runtime};

    use super::*;

    pub const NOW_SECS: u64 = 10_000;

    pub fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(NOW_SECS)
    }

    pub fn info(pid: u32, ppid: u32, name: &str, cmd: &str, start: u64, ram_mb: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: name.into(),
            exe_path: None,
            cmdline: Some(cmd.into()),
            start_time: Some(UNIX_EPOCH + Duration::from_secs(start)),
            working_set: ram_mb * 1024 * 1024,
            private_bytes: 0,
            cpu_time: Duration::ZERO,
            threads: 1,
            access_denied: false,
        }
    }

    pub fn port(pid: u32, port: u16, tcp: bool) -> PortInfo {
        PortInfo {
            pid,
            proto: if tcp { Proto::Tcp } else { Proto::Udp },
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            state: if tcp {
                PortState::Listen
            } else {
                PortState::Bound
            },
        }
    }

    pub fn tracked(
        i: ProcInfo,
        ports: Vec<PortInfo>,
        state: ProcState,
        is_root: bool,
    ) -> TrackedProc {
        let runtime: Runtime = classify(&i.name, i.cmdline.as_deref(), None);
        TrackedProc {
            info: i,
            ports,
            state,
            runtime,
            is_root,
        }
    }

    pub fn report(
        owner: OwnerId,
        agent: Option<&str>,
        label: &str,
        processes: Vec<TrackedProc>,
    ) -> OwnerReport {
        OwnerReport {
            owner,
            meta: OwnerMeta {
                agent: agent.map(Into::into),
                label: label.into(),
                cwd: Some(PathBuf::from(r"C:\work\api")),
            },
            processes,
        }
    }

    /// Claude tab "api": active shell + vite on :5173 + an orphaned dotnet zombie on :5000 + an
    /// orphan esbuild; plus one external codex descendant.
    pub fn sample() -> Collected {
        let shell = info(100, 1, "pwsh.exe", "pwsh -NoLogo", 9_000, 90);
        let vite = info(
            101,
            100,
            "node.exe",
            r"node C:\w\node_modules\vite\bin\vite.js",
            9_100,
            300,
        );
        let dotnet = info(102, 1, "dotnet.exe", "dotnet watch run", 9_200, 410);
        let esbuild = info(103, 102, "esbuild.exe", "esbuild --service", 9_300, 25);
        let ext_agent = info(200, 1, "codex.exe", "codex", 8_000, 80);
        let ext_child = info(
            201,
            200,
            "python.exe",
            "python -m http.server 8000",
            9_500,
            30,
        );
        let report = report(
            1,
            Some("claude"),
            "api",
            vec![
                tracked(shell.clone(), vec![], ProcState::Active, true),
                tracked(
                    vite.clone(),
                    vec![port(101, 5173, true)],
                    ProcState::Active,
                    false,
                ),
                tracked(
                    dotnet.clone(),
                    vec![port(102, 5000, true)],
                    ProcState::PortZombie,
                    false,
                ),
                tracked(esbuild.clone(), vec![], ProcState::Orphan, false),
            ],
        );
        let untracked = UntrackedProc {
            info: ext_child.clone(),
            ports: vec![port(201, 8000, true)],
            agent: "codex".into(),
            agent_pid: 200,
            attribution: proc_svc::Attribution::Probable,
            runtime: classify(&ext_child.name, ext_child.cmdline.as_deref(), None),
        };
        let mut sys = info(4, 0, "System", "", 1, 0);
        sys.access_denied = true;
        let other = info(300, 1, "svchost.exe", "svchost -k x", 100, 20);
        Collected {
            reports: vec![report],
            untracked: vec![untracked],
            ports: vec![
                port(101, 5173, true),
                port(102, 5000, true),
                port(201, 8000, true),
                port(4, 445, true),
                port(300, 5353, false),
                port(102, 5001, false),
            ],
            procs: vec![
                shell, vite, dotnet, esbuild, ext_agent, ext_child, sys, other,
            ],
            cpu: HashMap::from([(101, 3.4), (102, 0.0)]),
            now: Some(now()),
            tree_owners: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn state_mapping() {
        assert_eq!(RowState::from(ProcState::Active), RowState::Active);
        assert_eq!(RowState::from(ProcState::Orphan), RowState::Orphan);
        assert_eq!(RowState::from(ProcState::PortZombie), RowState::PortZombie);
        assert_eq!(RowState::PortZombie.label(), "zombi de puerto");
        assert!(RowState::Orphan.is_orphan() && !RowState::Active.is_orphan());
    }

    #[test]
    fn groups_agent_then_owner_then_rows() {
        let s = build_snapshot(&sample());
        let names: Vec<&str> = s.agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Claude", "Codex"]);
        let claude = &s.agents[0];
        assert_eq!(claude.owners.len(), 1);
        let tab = &claude.owners[0];
        assert_eq!(
            (tab.label.as_str(), tab.detail.as_deref(), tab.probable),
            ("api", Some("api"), false)
        );
        // Tree order: shell, vite (child of shell), dotnet, esbuild (child of dotnet).
        let pids: Vec<(u32, u8)> = tab.rows.iter().map(|r| (r.pid, r.depth)).collect();
        assert_eq!(pids, [(100, 0), (101, 1), (102, 0), (103, 1)]);
        assert!(tab.rows[0].is_root && !tab.rows[0].can_stop());
        assert!(tab.rows[1].can_stop());
    }

    #[test]
    fn heuristic_rows_are_probable_and_never_orphans() {
        let s = build_snapshot(&sample());
        let codex = &s.agents[1];
        let group = &codex.owners[0];
        assert_eq!(group.key, GroupKey::External(200));
        assert!(group.probable);
        assert_eq!(group.label, "Fuera de Selachi");
        assert_eq!(group.detail.as_deref(), Some("codex · pid 200"));
        let row = &group.rows[0];
        assert!(row.probable && row.state == RowState::Active && row.owner.is_none());
        assert_eq!(row.runtime, RuntimeKind::Python);
        assert_eq!(row.ports[0].url().as_deref(), Some("http://localhost:8000"));
    }

    #[test]
    fn orphan_summary_counts_only_tracked_orphans_and_zombies() {
        let s = build_snapshot(&sample());
        // dotnet (zombie, 410) + esbuild (orphan, 25).
        assert_eq!(
            s.summary,
            OrphanSummary {
                count: 2,
                ram_bytes: 435 * 1024 * 1024
            }
        );
        assert_eq!(s.summary.label().as_deref(), Some("2 huérfanos · 435 MB"));
        let one = OrphanSummary {
            count: 1,
            ram_bytes: 1_288_490_189,
        };
        assert_eq!(one.label().as_deref(), Some("1 huérfano · 1,2 GB"));
        assert_eq!(OrphanSummary::default().label(), None);
    }

    #[test]
    fn cpu_uptime_and_ports_flow_into_rows() {
        let s = build_snapshot(&sample());
        let vite = s.find_row(101).unwrap();
        assert_eq!(vite.cpu, Some(3.4));
        assert_eq!(vite.uptime, Some(Duration::from_secs(900)));
        assert_eq!(vite.runtime, RuntimeKind::Node);
        assert_eq!(vite.tag, Some("vite"));
        assert_eq!(vite.command, "node vite.js");
        assert_eq!(
            vite.ports,
            [PortPill {
                port: 5173,
                tcp: true,
                exposed: false
            }]
        );
        assert_eq!(s.find_row(103).unwrap().cpu, None);
        assert!(s.has_cpu);
        assert_eq!(s.process_count, 5);
    }

    #[test]
    fn ports_view_dedupes_filters_udp_and_sorts_by_origin() {
        let s = build_snapshot(&sample());
        let seen: Vec<(u16, bool, u32)> = s.ports.iter().map(|p| (p.port, p.tcp, p.pid)).collect();
        // Tracked first (5000 tcp, 5001 udp of dotnet, 5173), then probable (8000), then others
        // (445). svchost's UDP 5353 is dropped.
        assert_eq!(
            seen,
            [
                (5000, true, 102),
                (5001, false, 102),
                (5173, true, 101),
                (8000, true, 201),
                (445, true, 4)
            ]
        );
        let zombie = &s.ports[0];
        assert_eq!(zombie.state, Some(RowState::PortZombie));
        assert_eq!(zombie.origin.label(), "Claude · api");
        assert!(zombie.is_agent_related() && zombie.can_stop());
        assert_eq!(s.ports[3].origin.label(), "Codex (probable)");
        let system = &s.ports[4];
        assert!(!system.is_agent_related());
        assert!(
            system.access_denied && !system.can_stop(),
            "System pid 4 cannot be stopped"
        );
        assert_eq!(s.ports[1].url(), None, "udp has no url");
    }

    #[test]
    fn pills_collapse_v4_and_v6() {
        let mut v6 = port(1, 3000, true);
        v6.local_addr = "::".parse().unwrap();
        let p = pills(&[port(1, 3000, true), v6, port(1, 53, false)]);
        assert_eq!(
            p,
            [
                PortPill {
                    port: 3000,
                    tcp: true,
                    exposed: true
                },
                PortPill {
                    port: 53,
                    tcp: false,
                    exposed: false
                }
            ]
        );
        assert_eq!(p[0].label(), ":3000");
        assert_eq!(p[1].label(), ":53 udp");
    }

    #[test]
    fn empty_owner_groups_are_hidden() {
        let c = Collected {
            reports: vec![report(7, Some("claude"), "vacío", vec![])],
            ..Collected::default()
        };
        let s = build_snapshot(&c);
        assert!(s.agents.is_empty() && s.is_empty());
    }

    #[test]
    fn rows_without_agent_group_last() {
        let mut c = sample();
        c.reports.push(report(
            2,
            None,
            "",
            vec![tracked(
                info(500, 1, "cmd.exe", "cmd", 9_000, 5),
                vec![],
                ProcState::Active,
                true,
            )],
        ));
        let s = build_snapshot(&c);
        assert_eq!(s.agents.last().unwrap().name, "Sin agente");
        assert_eq!(s.agents.last().unwrap().owners[0].label, "Pestaña 2");
    }

    #[test]
    fn order_tree_keeps_every_row_when_parents_are_not_older() {
        let mut a = build_snapshot(&sample()).rows().next().unwrap().clone();
        a.pid = 1;
        a.ppid = 2;
        a.start = Some(now());
        let mut b = a.clone();
        b.pid = 2;
        b.ppid = 1;
        let out = order_tree(vec![a, b]);
        assert_eq!(out.len(), 2);
    }
}
