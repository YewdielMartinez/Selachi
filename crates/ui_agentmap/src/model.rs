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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chat_core::ThreadUpdate;
use futures::StreamExt as _;
use gpui::{Context, EventEmitter, Task};
use store::DirWatcher;

use crate::events::AgentMapEvent;
use crate::registry::{ApprovalEntry, read_runs};
use crate::source::SessionSource;
use crate::tree::{Forest, ForestInput, NodeKey, build_forest};

/// Debounce of the registry watcher (the harness writes a record in a few steps).
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// Chat updates are bursty while an agent streams: fold them into one reload.
pub const CHAT_DEBOUNCE: Duration = Duration::from_millis(250);

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

    /// The data of "Duplicar como perfil" for `key`.
    pub fn profile_seed(&self, key: &NodeKey) -> Option<ProfileSeed> {
        let n = &self.forest.nodes[self.forest.index_of(key)?];
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
            if let Some(k) = key {
                self.load_detail(&k, cx);
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

    fn stop_watching(&mut self) {
        self.watcher = None;
        self.tasks.clear();
        self.debounce = None;
        self.load_task = None;
        self.detail_tasks.clear();
        self.detail_loading.clear();
        self.loading = false;
        self.again = false;
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        match DirWatcher::start(&self.runs_dir, WATCH_DEBOUNCE) {
            Ok((watcher, mut rx)) => {
                self.watcher = Some(watcher);
                self.tasks.push(cx.spawn(async move |this, cx| {
                    while rx.next().await.is_some() {
                        if this.update(cx, |m, cx| m.refresh(cx)).is_err() {
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
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let forest = cx
                .background_executor()
                .spawn(async move {
                    let threads = source.threads();
                    let runs = read_runs(&dir);
                    build_forest(&ForestInput {
                        threads: &threads,
                        runs: &runs,
                        wanted: wanted.as_ref(),
                        now_ms: store::now_ms(),
                        tab_titles: &titles,
                    })
                })
                .await;
            this.update(cx, |m, cx| m.loaded_forest(forest, cx)).ok();
        }));
    }

    fn loaded_forest(&mut self, forest: Forest, cx: &mut Context<Self>) {
        self.loading = false;
        self.load_task = None;
        self.loaded = true;
        self.builds += 1;
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
        self.forest = Arc::new(forest);
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
        cx.emit(MapChanged);
        cx.notify();
        if std::mem::take(&mut self.again) {
            self.refresh(cx);
        }
    }

    // ----- inspector data ---------------------------------------------------------------------

    /// Reads the prompt and the approvals of a node off the UI thread (once; see [`Self::detail`]).
    pub fn load_detail(&mut self, key: &NodeKey, cx: &mut Context<Self>) {
        if self.viewers == 0
            || self.details.contains_key(key)
            || self.detail_loading.contains(key)
        {
            return;
        }
        let Some(idx) = self.forest.index_of(key) else {
            return;
        };
        let node = &self.forest.nodes[idx];
        let run_prompts = node.run.as_ref().map(|r| (r.final_prompt.clone(), r.prompt.clone()));
        let registry_approvals = node
            .run
            .as_ref()
            .map(|r| r.approvals.clone())
            .unwrap_or_default();
        let thread_id = match key {
            NodeKey::Thread(id) => Some(*id),
            NodeKey::Tab(_) => None,
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
                    let mut approvals = thread_id.map(|id| source.approvals(id)).unwrap_or_default();
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
