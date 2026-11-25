//! `AgentMapModel`: the entity that keeps the forest up to date.
//!
//! **Event-driven, zero idle cost.** While no view is attached ([`AgentMapModel::attach`]) the model
//! holds no watcher and no task and [`refresh`](AgentMapModel::refresh) does nothing. While a view is
//! attached it listens to two push sources and to nothing else: a `store::DirWatcher` on
//! `agent-runs` (the harness process writes a record when a run starts, changes state or ends) and
//! the chat store's update stream (a child asked for an approval, a title or a status changed).
//! Bursts are folded: a reload is one background read of the registry plus the thread list, never
//! more than one in flight, and chat updates are debounced by [`CHAT_DEBOUNCE`].

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chat_core::{ThreadId, ThreadUpdate};
use futures::StreamExt as _;
use gpui::{Context, EventEmitter, Task};
use store::DirWatcher;

use crate::activity::{
    ActivityFeed, ExternalChild, LiveHint, LiveTrack, MAX_EXTERNAL_CHILDREN, build_feed,
    external_feed, external_live_line, live_line, log_events,
};
use crate::events::AgentMapEvent;
use crate::events_log::{LogBatch, LogEvent, LogReader};
use crate::registry::{ApprovalEntry, read_runs};
use crate::source::SessionSource;
use crate::tree::{
    ExternalProc, Forest, ForestInput, NodeKey, NodeStatus, build_forest, cap_external,
};

/// Debounce of the registry watcher (the harness writes a record in a few steps).
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// Chat updates are bursty while an agent streams: fold them into one reload.
pub const CHAT_DEBOUNCE: Duration = Duration::from_millis(250);

/// Folds the bursts of chat updates and log appends of the activity feed into one read.
pub const ACTIVITY_DEBOUNCE: Duration = Duration::from_millis(200);
/// Raw log events one feed keeps (the feed itself keeps 500 rows).
const MAX_RAW_LOG: usize = 4_000;

/// The tabs of the inspector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectorTab {
    #[default]
    Summary,
    Activity,
    Prompt,
}

impl InspectorTab {
    pub const ALL: [InspectorTab; 3] = [
        InspectorTab::Summary,
        InspectorTab::Activity,
        InspectorTab::Prompt,
    ];

    pub fn label(self) -> &'static str {
        match self {
            InspectorTab::Summary => "Resumen",
            InspectorTab::Activity => "Actividad",
            InspectorTab::Prompt => "Prompt",
        }
    }
}

/// The forest changed (a reload finished).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapChanged;

/// Where the prompt shown in the inspector came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptSource {
    /// `final_prompt` of the run record: exactly what was sent, profile instructions included.
    Registry,
    /// The first user message of the child thread.
    Thread,
    /// The redacted, capped `prompt` the harness stored.
    Stored,
}

impl PromptSource {
    pub fn label(self) -> &'static str {
        match self {
            PromptSource::Registry => "Prompt final enviado",
            PromptSource::Thread => "Primer mensaje del hilo",
            PromptSource::Stored => "Prompt registrado",
        }
    }
}

/// A prompt as the inspector shows it: redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptInfo {
    pub text: String,
    pub source: PromptSource,
    /// Secrets hidden by `store::redact`.
    pub redacted: usize,
}

/// What the inspector needs beyond the node itself (read on demand, off the UI thread).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeDetail {
    pub prompt: Option<PromptInfo>,
    pub approvals: Vec<ApprovalEntry>,
}

/// Prefill for "Duplicar como perfil": what a run was, as plain data the host maps to a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileSeed {
    pub name: String,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: Option<String>,
    pub worktree: bool,
    /// The prompt the run was given (redacted), for the profile's `instructions` if the user wants it.
    pub prompt: Option<String>,
}

pub struct AgentMapModel {
    source: Arc<dyn SessionSource>,
    runs_dir: PathBuf,
    forest: Arc<Forest>,
    /// The orchestrator the host asked for (always present in the forest).
    wanted: Option<NodeKey>,
    tab_titles: HashMap<String, String>,
    /// External (non-harness) agent processes fed by the host from `proc_svc` snapshots.
    external: HashMap<NodeKey, Vec<ExternalProc>>,
    /// Tokens and estimated cost of external nodes, matched by the host from the ingested usage logs.
    ext_usage: HashMap<NodeKey, (u64, Option<f64>)>,
    /// Thread ids previously observed from `SessionSource::threads`, so deleted threads prune their
    /// external processes automatically.
    seen_threads: HashSet<ThreadId>,
    viewers: usize,
    watcher: Option<DirWatcher>,
    tasks: Vec<Task<()>>,
    load_task: Option<Task<()>>,
    debounce: Option<Task<()>>,
    loading: bool,
    again: bool,
    loaded: bool,
    builds: usize,
    details: HashMap<NodeKey, Arc<NodeDetail>>,
    detail_loading: HashSet<NodeKey>,
    detail_tasks: Vec<Task<()>>,
    /// The node whose inspector is open (shared by every view of the map).
    selected: Option<NodeKey>,
    /// A run to show as soon as the forest knows it (`open_run` before the first load).
    pending_focus: Option<NodeKey>,
    /// The inspector tab (shared by every view, like `selected`).
    tab: InspectorTab,
    /// Child processes of external agents (fed by the host; only kept while a view is attached).
    ext_children: HashMap<NodeKey, Vec<ExternalChild>>,
    /// The status line of external nodes (their newest running child).
    live_ext: HashMap<NodeKey, String>,
    /// The status line of running thread nodes (`ejecutando `cargo test``...).
    live: HashMap<NodeKey, String>,
    /// Incremental readers of the events log of every running node (for the status line).
    tails: HashMap<NodeKey, Tail>,
    /// The feed of the node whose inspector shows the Activity tab (and nothing else).
    activity: Option<ActivityWatch>,
    pass_task: Option<Task<()>>,
    pass_loading: bool,
    pass_again: bool,
    pass_debounce: Option<Task<()>>,
    sig_task: Option<Task<()>>,
    /// Fingerprint of the registry files the last forest was built from.
    reg_sig: Option<u64>,
    passes: usize,
}

/// Where the events log of a run is read from, and what the status line knows of it.
struct Tail {
    reader: LogReader,
    track: LiveTrack,
}

/// The raw (redacted) log events of the watched node.
struct LogAccum {
    reader: LogReader,
    raw: Vec<LogEvent>,
}

impl LogAccum {
    fn ingest(&mut self, batch: LogBatch) {
        if batch.reset {
            self.raw.clear();
        }
        self.raw
            .extend(batch.events.into_iter().map(LogEvent::redacted));
        if self.raw.len() > MAX_RAW_LOG {
            let extra = self.raw.len() - MAX_RAW_LOG;
            self.raw.drain(..extra);
        }
    }
}

struct ActivityWatch {
    key: NodeKey,
    /// `None` while a pass has it.
    accum: Option<LogAccum>,
    feed: Arc<ActivityFeed>,
    loaded: bool,
}

struct PassTarget {
    key: NodeKey,
    thread: ThreadId,
    status: NodeStatus,
    log: PathBuf,
}

struct PassWatch {
    key: NodeKey,
    thread: ThreadId,
    started_ms: Option<i64>,
    active: bool,
    accum: LogAccum,
}

struct PassOut {
    tails: HashMap<NodeKey, Tail>,
    lines: HashMap<NodeKey, String>,
    watch: Option<(NodeKey, LogAccum, ActivityFeed)>,
}

/// The background half of an activity pass: advances the log readers of the running nodes (status
/// lines) and rebuilds the feed of the watched one. Reads only what was appended since last time.
fn run_activity_pass(
    source: &dyn SessionSource,
    targets: Vec<PassTarget>,
    mut tails: HashMap<NodeKey, Tail>,
    watch: Option<PassWatch>,
) -> PassOut {
    let mut lines = HashMap::new();
    tails.retain(|k, _| targets.iter().any(|t| &t.key == k));
    for t in &targets {
        let tail = tails.entry(t.key.clone()).or_insert_with(|| Tail {
            reader: LogReader::new(t.log.clone()),
            track: LiveTrack::default(),
        });
        let batch = tail.reader.poll();
        if batch.reset {
            tail.track = LiveTrack::default();
        }
        for ev in batch.events {
            tail.track.apply(&ev.redacted());
        }
        let hint = if t.status == NodeStatus::Running && tail.track.is_empty() {
            source.live_hint(t.thread)
        } else {
            LiveHint::None
        };
        if let Some(l) = live_line(t.status, &tail.track, &hint) {
            lines.insert(t.key.clone(), l);
        }
    }
    let watch = watch.map(|mut w| {
        let batch = w.accum.reader.poll();
        w.accum.ingest(batch);
        let chat = source.activity(w.thread, w.active);
        let log = log_events(&w.accum.raw, w.started_ms, w.active);
        let feed = build_feed(chat, log, w.active);
        (w.key, w.accum, feed)
    });
    PassOut {
        tails,
        lines,
        watch,
    }
}

/// Fingerprint of the run records (`*.json`: names, sizes, times). Appends to the events logs and
/// cancel markers do not change it, so they do not reload the forest.
fn registry_signature(dir: &Path) -> u64 {
    let mut items: Vec<(String, u64, u128)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            let ms = md
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis());
            Some((e.file_name().to_string_lossy().into_owned(), md.len(), ms))
        })
        .collect();
    items.sort();
    let mut h = DefaultHasher::new();
    items.hash(&mut h);
    h.finish()
}

impl EventEmitter<MapChanged> for AgentMapModel {}
/// The views forward the actions of the inspector here: the host subscribes to the model once.
impl EventEmitter<AgentMapEvent> for AgentMapModel {}

impl AgentMapModel {
    /// `runs_dir` is `<data dir>\agent-runs`.
    pub fn new(source: Arc<dyn SessionSource>, runs_dir: impl Into<PathBuf>) -> Self {
        Self {
            source,
            runs_dir: runs_dir.into(),
            forest: Arc::new(Forest::default()),
            wanted: None,
            tab_titles: HashMap::new(),
            external: HashMap::new(),
            ext_usage: HashMap::new(),
            seen_threads: HashSet::new(),
            viewers: 0,
            watcher: None,
            tasks: Vec::new(),
            load_task: None,
            debounce: None,
            loading: false,
            again: false,
            loaded: false,
            builds: 0,
            details: HashMap::new(),
            detail_loading: HashSet::new(),
            detail_tasks: Vec::new(),
            selected: None,
            pending_focus: None,
            tab: InspectorTab::Summary,
            ext_children: HashMap::new(),
            live_ext: HashMap::new(),
            live: HashMap::new(),
            tails: HashMap::new(),
            activity: None,
            pass_task: None,
            pass_loading: false,
            pass_again: false,
            pass_debounce: None,
            sig_task: None,
            reg_sig: None,
            passes: 0,
        }
    }

    // ----- reading ----------------------------------------------------------------------------

    pub fn forest(&self) -> &Arc<Forest> {
        &self.forest
    }

    /// A first load finished.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// How many reloads finished (tests use it to prove nothing runs while hidden).
    pub fn builds(&self) -> usize {
        self.builds
    }

    /// Views attached right now.
    pub fn viewers(&self) -> usize {
        self.viewers
    }

    /// True while at least one view is attached (the host can skip feeding snapshots when false).
    pub fn is_visible(&self) -> bool {
        self.viewers > 0
    }

    /// External (non-harness) agent processes currently retained for `parent`.
    pub fn external_procs(&self, parent: &NodeKey) -> &[ExternalProc] {
        self.external.get(parent).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The registry watcher and the chat stream are alive (true only while a view is attached).
    pub fn is_watching(&self) -> bool {
        self.watcher.is_some() || !self.tasks.is_empty()
    }

    /// The orchestrator being shown: the one the host asked for, else the busiest / newest.
    pub fn root_index(&self) -> Option<usize> {
        self.wanted
            .as_ref()
            .and_then(|k| self.forest.index_of(k))
            .or_else(|| self.forest.default_root())
    }

    /// `(key, title, has work in flight)` of every orchestrator, for the selector.
    pub fn root_choices(&self) -> Vec<(NodeKey, String, bool)> {
        self.forest
            .roots
            .iter()
            .map(|r| {
                (
                    self.forest.nodes[*r].key.clone(),
                    self.forest.nodes[*r].title.clone(),
                    self.forest.has_active(*r),
                )
            })
            .collect()
    }

    pub fn detail(&self, key: &NodeKey) -> Option<Arc<NodeDetail>> {
        self.details.get(key).cloned()
    }

    /// The tab the inspector shows.
    pub fn inspector_tab(&self) -> InspectorTab {
        self.tab
    }

    /// The live feed of `key`, when its inspector shows the Activity tab (the only node whose
    /// activity is read). `None` otherwise or before the first read finished ([`Self::activity_loaded`]).
    pub fn activity_feed(&self, key: &NodeKey) -> Option<Arc<ActivityFeed>> {
        self.activity
            .as_ref()
            .filter(|a| &a.key == key)
            .map(|a| a.feed.clone())
    }

    /// The first read of the feed of `key` finished.
    pub fn activity_loaded(&self, key: &NodeKey) -> bool {
        self.activity
            .as_ref()
            .is_some_and(|a| &a.key == key && a.loaded)
    }

    /// The status line of a running node (`ejecutando `cargo test -p store``, `escribiendo…`,
    /// `esperando aprobación`). `None` for nodes that are not in flight or when nothing is known.
    pub fn live_line(&self, key: &NodeKey) -> Option<&str> {
        self.live
            .get(key)
            .or_else(|| self.live_ext.get(key))
            .map(String::as_str)
    }

    /// Whether any node has a status line (the tree makes its rows taller then).
    pub fn has_live_lines(&self) -> bool {
        !self.live.is_empty() || !self.live_ext.is_empty()
    }

    /// Activity passes run so far (tests use it to prove nothing is read while hidden).
    pub fn passes(&self) -> usize {
        self.passes
    }

    /// Child processes currently retained for the external node `key`.
    pub fn external_children(&self, key: &NodeKey) -> &[ExternalChild] {
        self.ext_children.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The data of "Duplicar como perfil" for `key`.
    pub fn profile_seed(&self, key: &NodeKey) -> Option<ProfileSeed> {
        let n = &self.forest.nodes[self.forest.index_of(key)?];
        if n.is_external() {
            return None;
        }
        let prompt = self
            .details
            .get(key)
            .and_then(|d| d.prompt.as_ref().map(|p| p.text.clone()));
        Some(ProfileSeed {
            name: n.profile.clone().unwrap_or_else(|| n.title.clone()),
            agent: n.agent.clone(),
            model: n.model.clone(),
            effort: n.effort.clone(),
            permission: n.permission.clone(),
            worktree: n.worktree.is_some(),
            prompt,
        })
    }

    // ----- host controls ----------------------------------------------------------------------

    /// Shows the session of this orchestrator (the active thread or tab). It is part of the map even
    /// when it has not delegated yet. `None` goes back to the automatic choice.
    pub fn set_root(&mut self, key: Option<NodeKey>, cx: &mut Context<Self>) {
        if self.wanted != key {
            self.wanted = key;
            self.refresh(cx);
            cx.notify();
        }
    }

    pub fn wanted_root(&self) -> Option<&NodeKey> {
        self.wanted.as_ref()
    }

    /// The node whose inspector is open.
    pub fn selected(&self) -> Option<&NodeKey> {
        self.selected.as_ref()
    }

    /// Opens or closes the inspector of a node (`None` closes it) and reads its detail.
    pub fn select(&mut self, key: Option<NodeKey>, cx: &mut Context<Self>) {
        if self.selected != key {
            self.selected = key.clone();
            if matches!(key, Some(NodeKey::External { .. })) && self.tab == InspectorTab::Prompt {
                // External nodes have no prompt tab.
                self.tab = InspectorTab::Summary;
            }
            if let Some(k) = key {
                self.load_detail(&k, cx);
            }
            if self.sync_activity(cx) {
                self.run_pass(cx);
            }
            cx.notify();
        }
    }

    /// Switches the inspector tab. The activity of the selected node is read only while the
    /// Activity tab is shown.
    pub fn set_inspector_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        if matches!(self.selected, Some(NodeKey::External { .. })) && tab == InspectorTab::Prompt {
            return;
        }
        if self.tab != tab {
            self.tab = tab;
            if self.sync_activity(cx) {
                self.run_pass(cx);
            }
            cx.notify();
        }
    }

    /// "Delegó en…" row, sidebar icon, palette: show the map on the session of delegated thread
    /// `child` with its inspector open. Works before the first load (it is resolved then).
    pub fn open_run(&mut self, child: chat_core::ThreadId, cx: &mut Context<Self>) {
        let key = NodeKey::Thread(child);
        if self.apply_focus(&key) {
            self.select(Some(key), cx);
        } else {
            self.pending_focus = Some(key);
            self.refresh(cx);
        }
    }

    /// Makes `key`'s orchestrator the shown one when the forest knows `key`.
    fn apply_focus(&mut self, key: &NodeKey) -> bool {
        let Some(idx) = self.forest.index_of(key) else {
            return false;
        };
        let root = self.forest.root_of(idx);
        self.wanted = Some(self.forest.nodes[root].key.clone());
        true
    }

    /// Titles of the terminal tabs that delegated (`tab id -> title`). Opaque ids otherwise.
    pub fn set_tab_titles(&mut self, titles: HashMap<String, String>, cx: &mut Context<Self>) {
        if self.tab_titles != titles {
            self.tab_titles = titles;
            self.refresh(cx);
        }
    }

    /// Feeds the live external (non-harness) agent processes descending from `parent` (obtained by
    /// the host from `proc_svc` snapshots). The map never polls on its own.
    ///
    /// * Previously-running processes that are no longer in `procs` are marked finished (`ended_ms`)
    ///   and kept in the map until `parent` is gone (or [`Self::clear_external`] is called).
    /// * Command lines are defensively passed through `store::redact`.
    /// * Retained entries per parent are capped at [`crate::MAX_EXTERNAL_PER_SESSION`] (200).
    /// * While no view is attached (`viewers == 0`), triggers no reload, no disk/DB reads and no
    ///   timers.
    pub fn set_external(
        &mut self,
        parent: NodeKey,
        procs: Vec<ExternalProc>,
        cx: &mut Context<Self>,
    ) {
        self.set_external_at(parent, procs, store::now_ms(), cx);
    }

    /// Like [`Self::set_external`] with an explicit timestamp (for deterministic tests).
    pub fn set_external_at(
        &mut self,
        parent: NodeKey,
        procs: Vec<ExternalProc>,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        if procs.is_empty() && !self.external.contains_key(&parent) {
            return;
        }
        let entry = self.external.entry(parent).or_default();
        if merge_external(entry, procs, now_ms) {
            self.refresh(cx);
        }
    }

    /// Drops all retained external agent processes for `parent` (call when a tab/session closes).
    pub fn clear_external(&mut self, parent: &NodeKey, cx: &mut Context<Self>) {
        if self.external.remove(parent).is_some() {
            self.refresh(cx);
        }
    }

    /// Feeds the live **child processes** of an external agent (what `agy` started: `cargo`, `git`...),
    /// obtained by the host from the same `proc_svc` snapshot as [`Self::set_external`]. `agent` is
    /// the external node (`ExternalProc::key()`). The Activity tab of that node shows them as
    /// commands in progress, and its tree row shows the newest one.
    ///
    /// * Previously running children missing from `procs` are marked finished and kept (at most
    ///   [`MAX_EXTERNAL_CHILDREN`] per agent, finished ones dropped first).
    /// * Command lines are passed through `store::redact` again.
    /// * While no view is attached nothing is stored and nothing is done (the host samples only
    ///   while the map is visible anyway).
    pub fn set_external_children(
        &mut self,
        agent: NodeKey,
        procs: Vec<ExternalChild>,
        cx: &mut Context<Self>,
    ) {
        self.set_external_children_at(agent, procs, store::now_ms(), cx);
    }

    /// Like [`Self::set_external_children`] with an explicit timestamp (deterministic tests).
    pub fn set_external_children_at(
        &mut self,
        agent: NodeKey,
        procs: Vec<ExternalChild>,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        if self.viewers == 0 || !matches!(agent, NodeKey::External { .. }) {
            return;
        }
        if procs.is_empty() && !self.ext_children.contains_key(&agent) {
            return;
        }
        let entry = self.ext_children.entry(agent.clone()).or_default();
        if merge_children(entry, procs, now_ms) {
            self.refresh_external_feed(&agent, now_ms);
            cx.notify();
        }
    }

    /// Tokens and cost the host found for the external agent `key` (`ExternalProc::key()`), from the
    /// usage logs `agents_svc` ingests. The cost is always an estimate (the inspector says so).
    /// `None` clears it. Applies in place (no reload) and, while no view is attached, stores
    /// nothing: the host only computes it while the map is visible.
    pub fn set_external_usage(
        &mut self,
        key: NodeKey,
        usage: Option<(u64, Option<f64>)>,
        cx: &mut Context<Self>,
    ) {
        if self.viewers == 0 || !matches!(key, NodeKey::External { .. }) {
            return;
        }
        let changed = match usage {
            Some(u) => self.ext_usage.insert(key, u) != Some(u),
            None => self.ext_usage.remove(&key).is_some(),
        };
        if changed {
            self.apply_ext_usage();
            cx.notify();
        }
    }

    /// Tokens and estimated cost the host attached to an external node.
    pub fn external_usage(&self, key: &NodeKey) -> Option<(u64, Option<f64>)> {
        self.ext_usage.get(key).copied()
    }

    /// Writes the matched usage into the external nodes of the forest (a node that is not in the
    /// forest yet gets it when it is).
    fn apply_ext_usage(&mut self) {
        if self.ext_usage.is_empty() && self.forest.nodes.iter().all(|n| n.tokens.is_none() || !n.is_external()) {
            return;
        }
        let f = Arc::make_mut(&mut self.forest);
        for n in f.nodes.iter_mut().filter(|n| n.is_external()) {
            let u = self.ext_usage.get(&n.key).copied();
            n.tokens = u.map(|(t, _)| t);
            n.cost_usd = u.and_then(|(_, c)| c);
        }
    }

    /// Recomputes the status line of external node `key` and, when its Activity tab is open, its feed.
    fn refresh_external_feed(&mut self, key: &NodeKey, now_ms: i64) {
        let kids = self.ext_children.get(key).cloned().unwrap_or_default();
        match external_live_line(&kids) {
            Some(l) => {
                self.live_ext.insert(key.clone(), l);
            }
            None => {
                self.live_ext.remove(key);
            }
        }
        if let Some(a) = self.activity.as_mut()
            && &a.key == key
        {
            let active = self
                .forest
                .index_of(key)
                .map(|i| self.forest.nodes[i].status.is_active())
                .unwrap_or(true);
            a.feed = Arc::new(external_feed(&kids, now_ms, active));
            a.loaded = true;
        }
    }

    // ----- lifecycle --------------------------------------------------------------------------

    /// A view is shown: start listening (first view) and load.
    pub fn attach(&mut self, cx: &mut Context<Self>) {
        self.viewers += 1;
        if self.viewers == 1 {
            self.start_watching(cx);
            self.refresh(cx);
        }
    }

    /// A view went away: stop listening when it was the last one.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers == 0 {
            self.stop_watching();
            cx.notify();
        }
    }

    /// The host's [`SessionSource`] can answer now (the chat database opens lazily, after the model
    /// exists): while a view is attached, listen to its update stream and reload. No-op otherwise.
    pub fn source_ready(&mut self, cx: &mut Context<Self>) {
        if self.viewers == 0 {
            return;
        }
        self.stop_watching();
        self.start_watching(cx);
        self.refresh(cx);
    }

    fn stop_watching(&mut self) {
        self.watcher = None;
        self.tasks.clear();
        self.debounce = None;
        self.load_task = None;
        self.detail_tasks.clear();
        self.detail_loading.clear();
        self.loading = false;
        self.again = false;
        // The activity side: no readers, no feed, no status lines, no child processes.
        self.pass_task = None;
        self.pass_debounce = None;
        self.sig_task = None;
        self.pass_loading = false;
        self.pass_again = false;
        self.tails.clear();
        self.live.clear();
        self.live_ext.clear();
        self.ext_children.clear();
        self.ext_usage.clear();
        self.activity = None;
        self.reg_sig = None;
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        match DirWatcher::start(&self.runs_dir, WATCH_DEBOUNCE) {
            Ok((watcher, mut rx)) => {
                self.watcher = Some(watcher);
                self.tasks.push(cx.spawn(async move |this, cx| {
                    while rx.next().await.is_some() {
                        if this.update(cx, |m, cx| m.on_registry_event(cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => log::warn!("agent map: cannot watch {}: {e}", self.runs_dir.display()),
        }
        if let Some(mut updates) = self.source.updates() {
            self.tasks.push(cx.spawn(async move |this, cx| {
                while let Some(u) = updates.next().await {
                    if this.update(cx, |m, cx| m.on_thread_update(&u, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    /// A chat thread changed: reload when it matters to the map (one of its nodes, or a new thread).
    fn on_thread_update(&mut self, u: &ThreadUpdate, cx: &mut Context<Self>) {
        let ch = u.changed;
        // The status line of a running node and the feed of the watched one follow the thread's
        // messages (streaming text, commands) and approvals.
        if (ch.messages || ch.approvals || ch.status) && self.follows_thread(u.thread_id) {
            self.schedule_pass(cx);
        }
        let relevant = ch.list || ch.status || ch.approvals || ch.usage;
        let known = self
            .forest
            .index_of(&NodeKey::Thread(u.thread_id))
            .is_some();
        if !relevant || !(known || ch.list) {
            return;
        }
        self.schedule_refresh(cx);
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.debounce.is_some() {
            return;
        }
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHAT_DEBOUNCE).await;
            this.update(cx, |m, cx| {
                m.debounce = None;
                m.refresh(cx);
            })
            .ok();
        }));
    }

    /// Reloads the forest (the host calls it when it knows something changed that the sources do not
    /// push). A no-op while no view is attached; folds calls made while a reload is running.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.viewers == 0 {
            return;
        }
        if self.loading {
            self.again = true;
            return;
        }
        self.loading = true;
        let source = self.source.clone();
        let dir = self.runs_dir.clone();
        let wanted = self.wanted.clone();
        let titles = self.tab_titles.clone();
        let external = self.external.clone();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let (forest, live_threads, sig) = cx
                .background_executor()
                .spawn(async move {
                    let sig = registry_signature(&dir);
                    let threads = source.threads();
                    let live_threads: HashSet<ThreadId> = threads.iter().map(|t| t.id).collect();
                    let runs = read_runs(&dir);
                    let forest = build_forest(&ForestInput {
                        threads: &threads,
                        runs: &runs,
                        wanted: wanted.as_ref(),
                        now_ms: store::now_ms(),
                        tab_titles: &titles,
                        external: &external,
                    });
                    (forest, live_threads, sig)
                })
                .await;
            this.update(cx, |m, cx| m.loaded_forest(forest, live_threads, sig, cx))
                .ok();
        }));
    }

    fn loaded_forest(
        &mut self,
        forest: Forest,
        live_threads: HashSet<ThreadId>,
        sig: u64,
        cx: &mut Context<Self>,
    ) {
        self.reg_sig = Some(sig);
        self.loading = false;
        self.load_task = None;
        self.loaded = true;
        self.builds += 1;
        // Prune external nodes belonging to threads that used to exist and are now gone.
        let disappeared: Vec<ThreadId> = self
            .seen_threads
            .iter()
            .copied()
            .filter(|id| !live_threads.contains(id))
            .collect();
        let mut pruned = false;
        for id in disappeared {
            self.seen_threads.remove(&id);
            if self.external.remove(&NodeKey::Thread(id)).is_some() {
                pruned = true;
            }
        }
        self.seen_threads.extend(live_threads);
        if pruned {
            self.again = true;
        }
        // Nodes in flight may have new approvals: read their detail again the next time.
        let live: Vec<NodeKey> = forest
            .nodes
            .iter()
            .filter(|n| n.status.is_active())
            .map(|n| n.key.clone())
            .collect();
        for k in live {
            self.details.remove(&k);
        }
        // the usage of an external process that left the map goes with it
        let external = &self.external;
        self.ext_usage
            .retain(|k, _| external.values().any(|v| v.iter().any(|p| &p.key() == k)));
        self.forest = Arc::new(forest);
        self.apply_ext_usage();
        if let Some(key) = self.pending_focus.take()
            && self.apply_focus(&key)
        {
            self.selected = Some(key.clone());
            self.load_detail(&key, cx);
        }
        // The selection of a node that vanished (a deleted thread) closes the inspector.
        if let Some(k) = &self.selected
            && self.forest.index_of(k).is_none()
        {
            self.selected = None;
        }
        if let Some(k) = self.selected.clone() {
            self.load_detail(&k, cx);
        }
        // External nodes that vanished take their child processes with them.
        let forest = self.forest.clone();
        self.ext_children
            .retain(|k, _| forest.index_of(k).is_some());
        self.live_ext.retain(|k, _| forest.index_of(k).is_some());
        let now = store::now_ms();
        let ext_keys: Vec<NodeKey> = self.ext_children.keys().cloned().collect();
        for k in ext_keys {
            self.refresh_external_feed(&k, now);
        }
        self.sync_activity(cx);
        self.run_pass(cx);
        cx.emit(MapChanged);
        cx.notify();
        if std::mem::take(&mut self.again) {
            self.refresh(cx);
        }
    }

    // ----- live activity ----------------------------------------------------------------------

    /// The events log of run `run_id`: `<runs dir>\<run id>.events.jsonl`.
    fn log_path(&self, run_id: &str) -> PathBuf {
        self.runs_dir.join(format!("{run_id}.events.jsonl"))
    }

    /// Does the status line of `thread` or the watched feed depend on it?
    fn follows_thread(&self, thread: ThreadId) -> bool {
        let key = NodeKey::Thread(thread);
        if self.activity.as_ref().is_some_and(|a| a.key == key) {
            return true;
        }
        self.forest
            .index_of(&key)
            .is_some_and(|i| self.forest.nodes[i].status.is_active())
    }

    /// Makes the watched node match what the inspector shows: the selected node while the Activity
    /// tab is open and a view is attached, nothing otherwise. Returns true when a new watch started
    /// (it needs a pass).
    fn sync_activity(&mut self, _cx: &mut Context<Self>) -> bool {
        let wanted = self
            .selected
            .clone()
            .filter(|_| self.viewers > 0 && self.tab == InspectorTab::Activity)
            .filter(|k| self.forest.index_of(k).is_some());
        if wanted.as_ref() == self.activity.as_ref().map(|a| &a.key) {
            return false;
        }
        self.activity = None;
        let Some(key) = wanted else {
            return false;
        };
        let accum = key.run_id().map(|id| LogAccum {
            reader: LogReader::new(self.log_path(&id)),
            raw: Vec::new(),
        });
        self.activity = Some(ActivityWatch {
            key: key.clone(),
            accum,
            feed: Arc::new(ActivityFeed::default()),
            loaded: false,
        });
        if matches!(key, NodeKey::External { .. }) {
            self.refresh_external_feed(&key, store::now_ms());
            if let Some(a) = self.activity.as_mut() {
                a.loaded = true;
            }
            return false;
        }
        true
    }

    /// The registry folder changed (a record, or an events log line). Reloads the forest only when
    /// a record changed; always advances the activity readers.
    fn on_registry_event(&mut self, cx: &mut Context<Self>) {
        let dir = self.runs_dir.clone();
        self.sig_task = Some(cx.spawn(async move |this, cx| {
            let sig = cx
                .background_executor()
                .spawn(async move { registry_signature(&dir) })
                .await;
            this.update(cx, |m, cx| {
                m.sig_task = None;
                if m.reg_sig != Some(sig) {
                    m.refresh(cx);
                } else {
                    m.run_pass(cx);
                }
            })
            .ok();
        }));
    }

    fn schedule_pass(&mut self, cx: &mut Context<Self>) {
        if self.viewers == 0 || self.pass_debounce.is_some() {
            return;
        }
        self.pass_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ACTIVITY_DEBOUNCE).await;
            this.update(cx, |m, cx| {
                m.pass_debounce = None;
                m.run_pass(cx);
            })
            .ok();
        }));
    }

    /// One activity pass: status lines of the running nodes and the feed of the watched node. Reads
    /// only the appended part of each events log (and the chat state of the watched thread) on the
    /// background executor. A no-op while hidden; folds calls made while one is running.
    fn run_pass(&mut self, cx: &mut Context<Self>) {
        if self.viewers == 0 {
            return;
        }
        if self.pass_loading {
            self.pass_again = true;
            return;
        }
        let targets: Vec<PassTarget> = self
            .forest
            .nodes
            .iter()
            .filter_map(|n| match &n.key {
                NodeKey::Thread(id) if n.status.is_active() => Some(PassTarget {
                    key: n.key.clone(),
                    thread: *id,
                    status: n.status,
                    log: self.log_path(&id.0.to_string()),
                }),
                _ => None,
            })
            .collect();
        let forest = self.forest.clone();
        let watch = self.activity.as_mut().and_then(|a| {
            let NodeKey::Thread(id) = a.key else {
                return None;
            };
            let node = &forest.nodes[forest.index_of(&a.key)?];
            Some(PassWatch {
                key: a.key.clone(),
                thread: id,
                started_ms: node.started_ms,
                active: node.status.is_active(),
                accum: a.accum.take()?,
            })
        });
        if targets.is_empty() && watch.is_none() {
            if !self.live.is_empty() || !self.tails.is_empty() {
                self.live.clear();
                self.tails.clear();
                cx.notify();
            }
            return;
        }
        self.passes += 1;
        self.pass_loading = true;
        let source = self.source.clone();
        let tails = std::mem::take(&mut self.tails);
        self.pass_task = Some(cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { run_activity_pass(&*source, targets, tails, watch) })
                .await;
            this.update(cx, |m, cx| m.pass_done(out, cx)).ok();
        }));
    }

    fn pass_done(&mut self, out: PassOut, cx: &mut Context<Self>) {
        self.pass_loading = false;
        self.pass_task = None;
        self.tails = out.tails;
        let mut changed = self.live != out.lines;
        self.live = out.lines;
        if let Some((key, accum, feed)) = out.watch
            && let Some(a) = self.activity.as_mut()
            && a.key == key
        {
            a.accum = Some(accum);
            if !a.loaded || *a.feed != feed {
                a.feed = Arc::new(feed);
                changed = true;
            }
            a.loaded = true;
        }
        if changed {
            cx.notify();
        }
        if std::mem::take(&mut self.pass_again) {
            self.run_pass(cx);
        }
    }

    // ----- inspector data ---------------------------------------------------------------------

    /// Reads the prompt and the approvals of a node off the UI thread (once; see [`Self::detail`]).
    pub fn load_detail(&mut self, key: &NodeKey, cx: &mut Context<Self>) {
        if matches!(key, NodeKey::External { .. }) {
            // External nodes have no registry prompt or chat approvals: everything is on MapNode.
            return;
        }
        if self.viewers == 0 || self.details.contains_key(key) || self.detail_loading.contains(key)
        {
            return;
        }
        let Some(idx) = self.forest.index_of(key) else {
            return;
        };
        let node = &self.forest.nodes[idx];
        let run_prompts = node
            .run
            .as_ref()
            .map(|r| (r.final_prompt.clone(), r.prompt.clone()));
        let registry_approvals = node
            .run
            .as_ref()
            .map(|r| r.approvals.clone())
            .unwrap_or_default();
        let thread_id = match key {
            NodeKey::Thread(id) => Some(*id),
            NodeKey::Tab(_) | NodeKey::External { .. } => None,
        };
        let source = self.source.clone();
        self.detail_loading.insert(key.clone());
        let key = key.clone();
        self.detail_tasks.push(cx.spawn(async move |this, cx| {
            let detail = cx
                .background_executor()
                .spawn(async move {
                    let (final_prompt, stored) = run_prompts.unwrap_or_default();
                    let from_thread = thread_id.and_then(|id| source.first_user_message(id));
                    let (text, src) = match (final_prompt, from_thread, stored) {
                        (Some(p), _, _) => (Some(p), PromptSource::Registry),
                        (None, Some(p), _) => (Some(p), PromptSource::Thread),
                        (None, None, p) => (p, PromptSource::Stored),
                    };
                    let prompt = text.map(|t| {
                        let r = store::redact(&t);
                        PromptInfo {
                            text: r.text,
                            source: src,
                            redacted: r.count,
                        }
                    });
                    let mut approvals =
                        thread_id.map(|id| source.approvals(id)).unwrap_or_default();
                    if approvals.is_empty() {
                        approvals = registry_approvals;
                    }
                    NodeDetail { prompt, approvals }
                })
                .await;
            this.update(cx, |m, cx| {
                m.detail_loading.remove(&key);
                m.details.insert(key, Arc::new(detail));
                cx.notify();
            })
            .ok();
        }));
    }
}

/// Pure merge of an incoming snapshot of external procs into the retained list for one parent.
/// Returns `true` when anything changed.
fn merge_external(
    existing: &mut Vec<ExternalProc>,
    incoming: Vec<ExternalProc>,
    now_ms: i64,
) -> bool {
    let mut sanitized: Vec<ExternalProc> = Vec::with_capacity(incoming.len());
    let mut incoming_keys: HashSet<(u32, i64)> = HashSet::with_capacity(incoming.len());
    for mut p in incoming {
        if !incoming_keys.insert((p.pid, p.started_ms)) {
            continue;
        }
        if !p.cmdline_redacted.is_empty() {
            p.cmdline_redacted = store::redact(&p.cmdline_redacted).text;
        }
        sanitized.push(p);
    }

    let mut changed = false;
    for e in existing.iter_mut() {
        if !incoming_keys.contains(&(e.pid, e.started_ms))
            && e.ended_ms.is_none()
            && e.exit_code.is_none()
        {
            e.ended_ms = Some(now_ms.max(e.started_ms));
            e.cpu_pct = None;
            e.rss_bytes = 0;
            changed = true;
        }
    }

    for p in sanitized {
        if let Some(e) = existing
            .iter_mut()
            .find(|e| e.pid == p.pid && e.started_ms == p.started_ms)
        {
            if *e != p {
                *e = p;
                changed = true;
            }
        } else {
            existing.push(p);
            changed = true;
        }
    }

    if changed {
        existing.sort_by_key(|p| (p.started_ms, p.pid));
        cap_external(existing);
    }
    changed
}

/// Pure merge of a snapshot of an external agent's child processes into the retained list.
/// Returns `true` when anything changed.
fn merge_children(
    existing: &mut Vec<ExternalChild>,
    incoming: Vec<ExternalChild>,
    now_ms: i64,
) -> bool {
    let mut seen: HashSet<(u32, i64)> = HashSet::with_capacity(incoming.len());
    let mut sanitized: Vec<ExternalChild> = Vec::with_capacity(incoming.len());
    for mut c in incoming {
        if !seen.insert((c.pid, c.started_ms)) {
            continue;
        }
        c.cmdline_redacted = store::redact(&c.cmdline_redacted).text;
        sanitized.push(c);
    }
    let mut changed = false;
    for e in existing.iter_mut() {
        if !seen.contains(&(e.pid, e.started_ms)) && e.is_running() {
            e.ended_ms = Some(now_ms.max(e.started_ms));
            e.cpu_pct = None;
            e.rss_bytes = 0;
            changed = true;
        }
    }
    for c in sanitized {
        if let Some(e) = existing
            .iter_mut()
            .find(|e| e.pid == c.pid && e.started_ms == c.started_ms)
        {
            // A process that already ended keeps its end; only live data refreshes.
            if e.is_running() && *e != c {
                *e = c;
                changed = true;
            }
        } else {
            existing.push(c);
            changed = true;
        }
    }
    if changed {
        existing.sort_by_key(|c| (c.started_ms, c.pid));
        while existing.len() > MAX_EXTERNAL_CHILDREN {
            // Oldest finished first; if all run, the oldest.
            let ix = existing.iter().position(|c| !c.is_running()).unwrap_or(0);
            existing.remove(ix);
        }
    }
    changed
}
