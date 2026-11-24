//! `ChatHub`: the one entity every chat view shares. It owns the [`ChatStore`], pumps its
//! `updates()` stream (a waker on the foreground executor: nothing runs while nothing happens),
//! caches the thread list for the sidebar, remembers the open tabs and routes [`ChatEvent`]s to
//! the host.
//!
//! Per-thread updates are re-emitted as [`ThreadUpdate`] events; each `ChatView` / `ChatTab`
//! filters by its own thread id, so a hidden thread costs one comparison per update and never
//! reads its state (the view reads it when it is painted).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agents_driver::{AgentKind, DetectedAgent};
use chat_core::{
    ChatStore, DeleteOptions, DeletePreview, DeleteReport, ThreadFilter, ThreadId, ThreadRecord,
    ThreadUpdate, now_ms,
};
use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use panel_api::TabHandle;
use theme::Theme;

use crate::lineage::DelegationRun;
use crate::mentions::{MAX_FILES, walk_files};
use crate::options::{AgentOption, default_agents};
use crate::tab::ChatTab;

/// What the chat UI asks the host to do. Subscribe once: `cx.subscribe(&hub, |_, ev: &ChatEvent, cx| ...)`.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    /// A thread row was clicked: open it (`hub.open_thread_tab`) or focus its tab (`hub.tab_for`).
    OpenThread(ThreadId),
    /// "Nuevo hilo": add `hub.new_thread_tab(project)` to the shell.
    NewThread { project: Option<PathBuf> },
    /// The chat-terminal bridge: show this command, with its real output, in a terminal tab.
    OpenInTerminal {
        thread: ThreadId,
        command: String,
        cwd: PathBuf,
    },
    /// An edit chip was clicked: open `path` (relative to `cwd` or absolute) with its diff.
    OpenDiff {
        thread: ThreadId,
        path: String,
        cwd: PathBuf,
    },
    /// "Restaurar…" on a checkpoint row. The host confirms and restores through `git_svc`.
    RestoreCheckpoint { thread: ThreadId, id: String },
    /// A thread was deleted: close its tab.
    ThreadRemoved(ThreadId),
}

/// The file list of a project finished loading (for the `@` picker).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesReady {
    pub root: PathBuf,
}

enum FileSlot {
    Loading,
    Ready(Arc<Vec<String>>),
}

/// See the module docs.
pub struct ChatHub {
    store: ChatStore,
    theme: Theme,
    threads: Vec<ThreadRecord>,
    loaded: bool,
    refreshing: bool,
    refresh_pending: bool,
    active: Option<ThreadId>,
    tabs: HashMap<ThreadId, WeakEntity<ChatTab>>,
    agents: Vec<AgentOption>,
    projects: Vec<PathBuf>,
    default_project: Option<PathBuf>,
    files: HashMap<String, FileSlot>,
    /// What the harness run registry says about each delegated thread (fed by the host).
    runs: HashMap<ThreadId, DelegationRun>,
    /// Models each agent told us (`agents_driver::probe_models`), by [`agent_key`]. An entry
    /// (even empty) means the agent was asked once: no retry this session.
    models: HashMap<String, Vec<agents_driver::ModelInfo>>,
    probe_models: bool,
    error: Option<String>,
    _pump: Task<()>,
}

impl EventEmitter<ChatEvent> for ChatHub {}
impl EventEmitter<ThreadUpdate> for ChatHub {}
impl EventEmitter<FilesReady> for ChatHub {}
impl EventEmitter<ModelsReady> for ChatHub {}

/// Fired when [`ChatHub::ensure_models`] learned the models of an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelsReady;

/// Cache key of an agent kind.
fn agent_key(kind: &AgentKind) -> String {
    match kind {
        AgentKind::Acp { command } => format!("acp:{command}"),
        other => crate::options::agent_name(other).to_string(),
    }
}

fn path_key(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

impl ChatHub {
    /// Creates the hub entity. Starts the update pump and loads the thread list in the background.
    pub fn create(store: ChatStore, theme: Theme, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(store, theme, cx))
    }

    pub fn new(store: ChatStore, theme: Theme, cx: &mut Context<Self>) -> Self {
        let mut rx = store.updates();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                // Everything already queued goes in one go.
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |hub, cx| hub.apply(batch, cx)).is_err() {
                    break;
                }
            }
        });
        let mut hub = Self {
            store,
            theme,
            threads: Vec::new(),
            loaded: false,
            refreshing: false,
            refresh_pending: false,
            active: None,
            tabs: HashMap::new(),
            agents: default_agents(),
            projects: Vec::new(),
            default_project: None,
            files: HashMap::new(),
            runs: HashMap::new(),
            models: HashMap::new(),
            probe_models: true,
            error: None,
            _pump: pump,
        };
        hub.refresh(cx);
        hub
    }

    // ----- accessors -----------------------------------------------------------------------

    pub fn store(&self) -> &ChatStore {
        &self.store
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Non-archived threads, newest first (live status).
    pub fn threads(&self) -> &[ThreadRecord] {
        &self.threads
    }

    /// The list finished loading at least once.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn thread(&self, id: ThreadId) -> Option<&ThreadRecord> {
        self.threads.iter().find(|t| t.id == id)
    }

    pub fn agents(&self) -> &[AgentOption] {
        &self.agents
    }

    /// Replaces the agents offered by the pickers (for example from the onboarding detection).
    pub fn set_agents(&mut self, agents: Vec<AgentOption>, cx: &mut Context<Self>) {
        self.agents = agents;
        cx.notify();
    }

    /// Projects for the new-thread picker, plus the roots of existing threads.
    pub fn projects(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for p in self
            .projects
            .iter()
            .chain(self.threads.iter().map(|t| &t.project_root))
        {
            if seen.insert(path_key(p)) {
                out.push(p.clone());
            }
        }
        out
    }

    pub fn set_projects(&mut self, projects: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.projects = projects;
        cx.notify();
    }

    /// The project a new thread starts in (the active tab's project).
    pub fn default_project(&self) -> Option<&Path> {
        self.default_project
            .as_deref()
            .or_else(|| self.projects.first().map(PathBuf::as_path))
    }

    pub fn set_default_project(&mut self, project: Option<PathBuf>) {
        self.default_project = project;
    }

    // ----- delegation lineage ----------------------------------------------------------------

    /// Replaces what the host knows about delegated runs (from the harness run registry). Views
    /// repaint only when something changed.
    pub fn set_delegation_runs(&mut self, runs: Vec<DelegationRun>, cx: &mut Context<Self>) {
        let map: HashMap<ThreadId, DelegationRun> =
            runs.into_iter().map(|r| (r.thread, r)).collect();
        if map != self.runs {
            self.runs = map;
            cx.notify();
        }
    }

    /// The run record of a delegated thread, when the registry has one.
    pub fn delegation_run(&self, thread: ThreadId) -> Option<&DelegationRun> {
        self.runs.get(&thread)
    }

    /// Every run record, keyed by child thread.
    pub fn delegation_runs(&self) -> &HashMap<ThreadId, DelegationRun> {
        &self.runs
    }

    /// The threads `parent` delegated, oldest first.
    pub fn children_of(&self, parent: ThreadId) -> Vec<&ThreadRecord> {
        crate::lineage::children_of(&self.threads, parent)
    }

    /// Cost of everything `parent` delegated (all levels).
    pub fn delegation_cost(&self, parent: ThreadId) -> f64 {
        crate::lineage::delegation_cost(&self.threads, &self.runs, parent)
    }

    /// The thread whose tab is in front (highlighted in the list).
    pub fn active_thread(&self) -> Option<ThreadId> {
        self.active
    }

    pub fn set_active_thread(&mut self, id: Option<ThreadId>, cx: &mut Context<Self>) {
        if self.active != id {
            self.active = id;
            cx.notify();
        }
    }

    /// Last error of a background operation (rename, archive, delete...), shown by the list.
    pub fn last_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn report_error(
        &mut self,
        what: &str,
        e: impl std::fmt::Display,
        cx: &mut Context<Self>,
    ) {
        log::warn!("ui_chat: {what}: {e}");
        self.error = Some(format!("{what}: {e}"));
        cx.notify();
    }

    // ----- tabs ----------------------------------------------------------------------------

    /// The tab of `id`, creating it (it loads the thread in the background) if it is not open.
    pub fn open_thread_tab(&mut self, id: ThreadId, cx: &mut Context<Self>) -> TabHandle {
        if let Some(tab) = self.tabs.get(&id).and_then(WeakEntity::upgrade) {
            return TabHandle::new(tab);
        }
        let hub = cx.entity();
        let (theme, store) = (self.theme, self.store.clone());
        let tab = cx.new(|cx| ChatTab::open(hub, theme, store, id, cx));
        self.tabs.insert(id, tab.downgrade());
        TabHandle::new(tab)
    }

    /// A tab showing the "¿Qué construimos?" screen. It becomes a thread on the first message.
    pub fn new_thread_tab(
        &mut self,
        project: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> TabHandle {
        let hub = cx.entity();
        let project = project.or_else(|| self.default_project().map(Path::to_path_buf));
        let theme = self.theme;
        TabHandle::new(cx.new(|cx| ChatTab::draft(hub, theme, project, cx)))
    }

    /// What the agent told us about its models (empty until asked, or when it has none).
    pub fn models_of(&self, kind: &AgentKind) -> &[agents_driver::ModelInfo] {
        self.models.get(&agent_key(kind)).map_or(&[], Vec::as_slice)
    }

    /// Hosts that use scripted agents turn the real probe off (no processes are started).
    pub fn set_probe_models(&mut self, on: bool) {
        self.probe_models = on;
    }

    /// Asks the agent for its models once per session (started without a prompt, see
    /// `agents_driver::probe_models`) and emits [`ModelsReady`]. Claude Code needs no probe.
    pub fn ensure_models(
        &mut self,
        kind: &AgentKind,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if !self.probe_models || *kind == AgentKind::ClaudeCode {
            return;
        }
        let key = agent_key(kind);
        if self.models.contains_key(&key) {
            return;
        }
        self.models.insert(key.clone(), Vec::new());
        let (kind, cwd) = (
            kind.clone(),
            cwd.or_else(|| std::env::current_dir().ok())
                .unwrap_or_default(),
        );
        let task = cx.background_spawn(async move {
            agents_driver::probe_models(kind, cwd, std::time::Duration::from_secs(20))
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(models) => {
                let _ = this.update(cx, |hub, cx| {
                    hub.models.insert(key, models);
                    cx.emit(ModelsReady);
                });
            }
            Err(e) => log::debug!("chat: could not list the models of {key}: {e}"),
        })
        .detach();
    }

    /// The open tab of `id`, if any (to focus it instead of opening a second one).
    pub fn tab_for(&self, id: ThreadId, _cx: &App) -> Option<TabHandle> {
        self.tabs
            .get(&id)
            .and_then(WeakEntity::upgrade)
            .map(TabHandle::new)
    }

    pub(crate) fn register_tab(&mut self, id: ThreadId, tab: WeakEntity<ChatTab>) {
        self.tabs.insert(id, tab);
    }

    pub(crate) fn unregister_tab(&mut self, id: ThreadId) {
        self.tabs.remove(&id);
    }

    /// Emits a [`ChatEvent`] to the host.
    pub fn emit_event(&mut self, ev: ChatEvent, cx: &mut Context<Self>) {
        cx.emit(ev);
    }

    // ----- thread list ---------------------------------------------------------------------

    /// Reloads the list from the store (background). Calls while one is running are folded.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            self.refresh_pending = true;
            return;
        }
        self.refreshing = true;
        let store = self.store.clone();
        let task = cx.background_spawn(async move { store.list_threads(&ThreadFilter::default()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |hub, cx| {
                hub.refreshing = false;
                match res {
                    Ok(list) => {
                        hub.threads = list;
                        hub.loaded = true;
                    }
                    Err(e) => hub.report_error("no se pudo leer la lista de hilos", e, cx),
                }
                cx.notify();
                if std::mem::take(&mut hub.refresh_pending) {
                    hub.refresh(cx);
                }
            });
        })
        .detach();
    }

    fn apply(&mut self, batch: Vec<ThreadUpdate>, cx: &mut Context<Self>) {
        let mut refresh = false;
        let mut notify = false;
        for u in batch {
            if u.changed.list {
                refresh = true;
            } else if let Some(t) = self.threads.iter_mut().find(|t| t.id == u.thread_id) {
                if u.changed.status && t.status != u.status {
                    t.status = u.status;
                    notify = true;
                }
                if u.changed.messages || u.changed.status {
                    t.updated_ms = now_ms();
                }
            } else if u.changed.status || u.changed.messages {
                // A thread the list does not know yet.
                refresh = true;
            }
            cx.emit(u);
        }
        if refresh {
            self.refresh(cx);
        }
        if notify {
            cx.notify();
        }
    }

    pub fn rename_thread(&mut self, id: ThreadId, title: String, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let task = cx.background_spawn(async move { store.rename_thread(id, &title) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                let _ = this.update(cx, |hub, cx| {
                    hub.report_error("no se pudo renombrar", e, cx)
                });
            }
        })
        .detach();
    }

    pub fn archive_thread(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let task = cx.background_spawn(async move { store.archive_thread(id, true) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |hub, cx| match res {
                Ok(()) => cx.emit(ChatEvent::ThreadRemoved(id)),
                Err(e) => hub.report_error("no se pudo archivar", e, cx),
            });
        })
        .detach();
    }

    /// What deleting `id` involves (message count, worktree state). Runs in the background.
    pub fn delete_preview(
        &self,
        id: ThreadId,
        cx: &mut Context<Self>,
    ) -> Task<chat_core::Result<DeletePreview>> {
        let store = self.store.clone();
        cx.background_spawn(async move { store.delete_preview(id) })
    }

    /// Deletes a thread after the user confirmed (see [`crate::DeleteDialog`]).
    pub fn delete_thread(
        &mut self,
        id: ThreadId,
        opts: DeleteOptions,
        cx: &mut Context<Self>,
    ) -> Task<chat_core::Result<DeleteReport>> {
        let store = self.store.clone();
        let task = cx.background_spawn(async move { store.delete_thread(id, opts) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |hub, cx| match &res {
                Ok(_) => {
                    hub.tabs.remove(&id);
                    cx.emit(ChatEvent::ThreadRemoved(id));
                }
                Err(e) => hub.report_error("no se pudo eliminar el hilo", e, cx),
            });
            res
        })
    }

    // ----- agents --------------------------------------------------------------------------

    /// Probes the installed agent CLIs (`agents_driver::detect`, blocking: it runs in the
    /// background) and greys out the ones that are not installed.
    pub fn detect_agents(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async { agents_driver::detect() });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |hub, cx| hub.merge_detected(&found, cx));
        })
        .detach();
    }

    fn merge_detected(&mut self, found: &[DetectedAgent], cx: &mut Context<Self>) {
        for a in &mut self.agents {
            let hit = found
                .iter()
                .find(|d| std::mem::discriminant(&d.kind) == std::mem::discriminant(&a.kind));
            a.available = hit.is_some();
            a.version = hit.and_then(|d| d.version.clone());
        }
        cx.notify();
    }

    /// The agent option of `kind` (a plain one when the host never listed it).
    pub fn agent_option(&self, kind: &AgentKind) -> AgentOption {
        self.agents
            .iter()
            .find(|a| &a.kind == kind)
            .cloned()
            .unwrap_or_else(|| AgentOption::new(kind.clone()))
    }

    // ----- project files (the @ picker) ----------------------------------------------------

    /// The listed files of `root`, once loaded.
    pub fn files_for(&self, root: &Path) -> Option<Arc<Vec<String>>> {
        match self.files.get(&path_key(root)) {
            Some(FileSlot::Ready(f)) => Some(f.clone()),
            _ => None,
        }
    }

    /// Starts listing `root` (once; emits [`FilesReady`] when done).
    pub fn load_files(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let key = path_key(&root);
        if self.files.contains_key(&key) {
            return;
        }
        self.files.insert(key.clone(), FileSlot::Loading);
        let walk_root = root.clone();
        let task = cx.background_spawn(async move { walk_files(&walk_root, MAX_FILES) });
        cx.spawn(async move |this, cx| {
            let files = task.await;
            let _ = this.update(cx, |hub, cx| {
                hub.files.insert(key, FileSlot::Ready(Arc::new(files)));
                cx.emit(FilesReady { root });
            });
        })
        .detach();
    }

    /// Forgets the listed files of `root` (after the agent changed the tree).
    pub fn invalidate_files(&mut self, root: &Path) {
        self.files.remove(&path_key(root));
    }
}
