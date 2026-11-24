//! `GitModel`: the entity the app owns and the panel views observe.
//!
//! # Cost model
//!
//! * **Hidden: nothing runs.** No watcher, no refresh, no timer. [`GitModel::set_visible`]`(false)`
//!   (or the last view being dropped) drops the `Repo::watch` handle and cancels the pending
//!   reads. Showing the panel again starts the watcher and takes one fresh read.
//! * **Visible: zero polling.** The repository is read once, then again only when the debounced
//!   OS watcher says something changed or one of our own operations finished. At most one read is
//!   in flight and one more is queued ([`RefreshGate`]); every git call runs on the background
//!   executor and `cx.notify()` fires only when the data actually changed.
//! * **Sensitive operations** (commit, push, pull, restore, remove worktree...) only run after
//!   the [`Gate`] released them with an explicit [`GitModel::confirm`].
//!
//! The host listens to [`GitPanelEvent`] on this entity (see the crate docs).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use git_svc::{CheckpointChange, DraftKind, Editor, GhClient, NewPr, Repo, WatchHandle};
use gpui::{Context, EventEmitter, Task};

use crate::cmd::GitOp;
use crate::data::{self, DiffTarget, GitData};
use crate::diffview::{DiffPreview, agent_markdown};
use crate::gate::{Gate, Outcome, RefreshGate, Step};
use crate::pr::{
    self, MAX_FAILED_SENT, PrCreate, PrCreated, PrQuery, PrSnapshot, PrSummary, PrTrigger, PrView,
};

/// Debounce of the file watcher.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// What the panel asks of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitPanelEvent {
    /// Run this command visibly in a terminal tab (`argv[0]` is `git`).
    RunInTerminal { argv: Vec<String>, cwd: PathBuf },
    /// Open a file (absolute path) in the viewer, at `line` (1-based) when known.
    OpenFile { path: PathBuf, line: Option<usize> },
    /// Open the full diff of a file (absolute path).
    OpenDiff { path: PathBuf },
    /// Send this markdown (a diff and where it comes from) to an agent. The host should redact
    /// secrets first (backlog A13).
    AskAgent { context_markdown: String },
    /// The user picked another worktree in the switcher: the host may `cd` the terminal.
    WorktreeSelected { path: PathBuf },
    /// "Redactar con agente" (backlog B3): the user asked an agent to draft a commit message or
    /// a PR description. **This is the only moment tokens are spent.** The host builds the prompt
    /// ([`GitPanelEvent::draft_prompt`]), runs the chosen agent read-only and answers with
    /// [`GitModel::set_draft`] (or [`GitModel::fail_draft`]). `staged_diff` is already redacted
    /// and capped at 60 KB; for a PR it is the diff of the branch against its base.
    DraftMessage {
        kind: DraftKind,
        diff_stat: String,
        staged_diff: String,
        recent_subjects: Vec<String>,
    },
}

impl GitPanelEvent {
    /// The prompt for a [`GitPanelEvent::DraftMessage`] (Spanish or English following the
    /// language of the repository history); `None` for any other event.
    pub fn draft_prompt(&self) -> Option<String> {
        match self {
            GitPanelEvent::DraftMessage {
                kind,
                diff_stat,
                staged_diff,
                recent_subjects,
            } => Some(git_svc::draft_prompt(
                *kind,
                diff_stat,
                staged_diff,
                recent_subjects,
            )),
            _ => None,
        }
    }
}

/// The agent's draft arrived ([`GitModel::set_draft`]): the commit box / PR form fill themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftReady {
    pub kind: DraftKind,
    pub text: String,
}

/// An operation ended (for the panel to clear its inputs, for the host to refresh its own state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpFinished {
    pub op: GitOp,
    pub ok: bool,
}

impl EventEmitter<GitPanelEvent> for GitModel {}
impl EventEmitter<OpFinished> for GitModel {}
impl EventEmitter<DraftReady> for GitModel {}
impl EventEmitter<PrCreated> for GitModel {}

/// Where the model is in discovering and reading the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    /// No directory was given.
    NoRoot,
    /// A directory is set; nothing was read yet (the panel has not been shown).
    Idle,
    Discovering,
    NotARepo,
    /// Repository found, first read pending.
    Loading,
    Ready,
    Failed(String),
}

/// How sensitive operations are executed once confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExecMode {
    /// Through `git_svc` / `git` without a window, the result shown inline.
    #[default]
    Direct,
    /// Emit [`GitPanelEvent::RunInTerminal`] and let the host run it visibly (PLAN section 7).
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffState {
    Loading,
    Ready(Arc<DiffPreview>),
    Failed(String),
}

/// The diff open under a file row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSlot {
    pub target: DiffTarget,
    pub state: DiffState,
}

/// "Ver cambios desde aquí" of a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointView {
    pub id: String,
    pub state: CheckpointFiles,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointFiles {
    Loading,
    Ready(Vec<CheckpointChange>),
    Failed(String),
}

/// Counters for tests and the doctor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub reads: u32,
    pub watcher_starts: u32,
}

pub struct GitModel {
    host_root: Option<PathBuf>,
    override_root: Option<PathBuf>,
    repo: Option<Repo>,
    load: Load,
    data: Option<Arc<GitData>>,
    visible: bool,
    viewers: usize,
    /// Bumped whenever the target repository changes: late results of the old one are dropped.
    epoch: u64,
    watch: Option<WatchHandle>,
    watch_task: Option<Task<()>>,
    watch_pending: bool,
    refresh: RefreshGate,
    refresh_task: Option<Task<()>>,
    gate: Gate,
    gate_epoch: u64,
    queue: VecDeque<GitOp>,
    exec_busy: bool,
    quiet_error: Option<String>,
    diff: Option<DiffSlot>,
    diff_task: Option<Task<()>>,
    checkpoint_view: Option<CheckpointView>,
    exec_mode: ExecMode,
    read_only: bool,
    editor: Editor,
    stats: Stats,
    // ----- pull request / CI (B1) and drafts (B3) -----
    /// `None` = the PR integration is off (the default).
    gh: Option<GhClient>,
    /// `gh` was proven installed and logged in (skips the auth probe on later queries).
    gh_ok: bool,
    pr_view: PrView,
    /// Branch the last query was started for (branch-change detection).
    pr_branch: Option<String>,
    pr_queried: Option<Instant>,
    pr_inflight: bool,
    pr_again: bool,
    pr_task: Option<Task<()>>,
    pr_queries: u32,
    /// Bumped when the target or the client changes: late results are dropped.
    pr_epoch: u64,
    pr_sending: bool,
    pr_create: PrCreate,
    drafting: Option<DraftKind>,
    drafts: Vec<(DraftKind, String)>,
}

impl Default for GitModel {
    fn default() -> Self {
        Self::new()
    }
}

impl GitModel {
    pub fn new() -> Self {
        Self {
            host_root: None,
            override_root: None,
            repo: None,
            load: Load::NoRoot,
            data: None,
            visible: false,
            viewers: 0,
            epoch: 0,
            watch: None,
            watch_task: None,
            watch_pending: false,
            refresh: RefreshGate::default(),
            refresh_task: None,
            gate: Gate::Idle,
            gate_epoch: 0,
            queue: VecDeque::new(),
            exec_busy: false,
            quiet_error: None,
            diff: None,
            diff_task: None,
            checkpoint_view: None,
            exec_mode: ExecMode::Direct,
            read_only: false,
            editor: Editor::VsCode,
            stats: Stats::default(),
            gh: None,
            gh_ok: false,
            pr_view: PrView::Idle,
            pr_branch: None,
            pr_queried: None,
            pr_inflight: false,
            pr_again: false,
            pr_task: None,
            pr_queries: 0,
            pr_epoch: 0,
            pr_sending: false,
            pr_create: PrCreate::Idle,
            drafting: None,
            drafts: Vec::new(),
        }
    }

    // ----- configuration -----------------------------------------------------------------

    /// Every mutating operation is refused ("Solo lectura"). For demos on a real repository.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if self.read_only != read_only {
            self.read_only = read_only;
            cx.notify();
        }
    }

    pub fn set_exec_mode(&mut self, mode: ExecMode) {
        self.exec_mode = mode;
    }

    /// Editor for "Abrir en editor" (default VS Code).
    pub fn set_editor(&mut self, editor: Editor) {
        self.editor = editor;
    }

    // ----- reads -------------------------------------------------------------------------

    pub fn data(&self) -> Option<&Arc<GitData>> {
        self.data.as_ref()
    }

    pub fn load_state(&self) -> &Load {
        &self.load
    }

    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    pub fn diff(&self) -> Option<&DiffSlot> {
        self.diff.as_ref()
    }

    pub fn checkpoint_view(&self) -> Option<&CheckpointView> {
        self.checkpoint_view.as_ref()
    }

    /// Error of a quiet operation (stage, unstage...) that could not be shown on a card.
    pub fn quiet_error(&self) -> Option<&str> {
        self.quiet_error.as_deref()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The repository root the panel shows (the worktree picked in the switcher, else the host's).
    pub fn root(&self) -> Option<&Path> {
        self.repo.as_ref().map(|r| r.root.as_path())
    }

    /// The directory the panel was pointed at, before discovery.
    pub fn target_dir(&self) -> Option<&Path> {
        self.override_root.as_deref().or(self.host_root.as_deref())
    }

    /// A `Repo::watch` handle is alive (true only while visible).
    pub fn has_watcher(&self) -> bool {
        self.watch.is_some()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn refresh_in_flight(&self) -> bool {
        self.refresh.in_flight()
    }

    // ----- target ------------------------------------------------------------------------

    /// The host's project root (or cwd). Same value = no-op; a new one drops the old repo and,
    /// when visible, discovers the new one.
    pub fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.host_root == root {
            return;
        }
        self.host_root = root;
        self.override_root = None;
        self.retarget(cx);
    }

    fn retarget(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.repo = None;
        self.data = None;
        self.watch = None;
        self.watch_task = None;
        self.watch_pending = false;
        self.refresh.reset();
        self.refresh_task = None;
        self.gate = Gate::Idle;
        self.queue.clear();
        self.exec_busy = false;
        self.quiet_error = None;
        self.diff = None;
        self.diff_task = None;
        self.checkpoint_view = None;
        self.reset_pr();
        self.drafting = None;
        self.drafts.clear();
        self.load = if self.target_dir().is_some() {
            Load::Idle
        } else {
            Load::NoRoot
        };
        if self.visible {
            self.activate(cx);
        }
        cx.notify();
    }

    /// Picks a worktree of the same repository.
    pub fn switch_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.root().is_some_and(|r| data::same_path(r, &path)) {
            return;
        }
        self.override_root = Some(path.clone());
        self.retarget(cx);
        cx.emit(GitPanelEvent::WorktreeSelected { path });
    }

    // ----- visibility --------------------------------------------------------------------

    /// The shell says the panel is shown / hidden. Showing discovers / reads and starts the
    /// watcher; hiding drops the watcher and cancels pending reads.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.activate(cx);
        } else {
            self.deactivate();
        }
    }

    /// Heartbeat: a painted view is a visible panel. Never notifies.
    pub fn touch(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.set_visible(true, cx);
        }
    }

    pub fn attach_view(&mut self) {
        self.viewers += 1;
    }

    /// A view was dropped; the last one hides the panel.
    pub fn detach_view(&mut self, cx: &mut Context<Self>) {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers == 0 {
            self.set_visible(false, cx);
        }
    }

    fn deactivate(&mut self) {
        self.watch = None;
        self.watch_task = None;
        self.watch_pending = false;
        self.refresh_task = None;
        self.refresh.reset();
        self.diff_task = None;
        if matches!(
            self.diff,
            Some(DiffSlot {
                state: DiffState::Loading,
                ..
            })
        ) {
            self.diff = None;
        }
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        if self.repo.is_some() {
            self.refresh(true, cx);
            self.start_watch(cx);
        } else {
            self.discover(cx);
        }
    }

    fn discover(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.target_dir().map(Path::to_path_buf) else {
            self.load = Load::NoRoot;
            return;
        };
        if matches!(self.load, Load::Discovering) {
            return;
        }
        self.load = Load::Discovering;
        cx.notify();
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { Repo::discover(&dir) })
                .await;
            let _ = this.update(cx, |m, cx| m.discovered(epoch, found, cx));
        })
        .detach();
    }

    fn discovered(
        &mut self,
        epoch: u64,
        found: git_svc::Result<Option<Repo>>,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.epoch {
            return;
        }
        match found {
            Ok(Some(repo)) => {
                self.repo = Some(repo);
                self.load = Load::Loading;
                if self.visible {
                    self.refresh(true, cx);
                    self.start_watch(cx);
                }
            }
            Ok(None) => self.load = Load::NotARepo,
            Err(e) => self.load = Load::Failed(e.to_string()),
        }
        cx.notify();
    }

    // ----- watcher -----------------------------------------------------------------------

    fn start_watch(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.watch.is_some() || self.watch_pending || !self.visible {
            return;
        }
        self.watch_pending = true;
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            // Setting the OS watches walks the top-level directories: keep it off the UI thread.
            let started = cx
                .background_executor()
                .spawn(async move { repo.watch(WATCH_DEBOUNCE) })
                .await;
            let Ok(Some(mut rx)) = this.update(cx, |m, _| {
                m.watch_pending = false;
                if epoch != m.epoch || !m.visible {
                    return None;
                }
                match started {
                    Ok((handle, rx)) => {
                        m.watch = Some(handle);
                        m.stats.watcher_starts += 1;
                        Some(rx)
                    }
                    Err(_) => None,
                }
            }) else {
                return;
            };
            let _ = this.update(cx, |m, cx| {
                m.watch_task = Some(cx.spawn(async move |this, cx| {
                    while rx.next().await.is_some() {
                        if this.update(cx, |m, cx| m.refresh(false, cx)).is_err() {
                            break;
                        }
                    }
                }));
            });
        })
        .detach();
    }

    // ----- refresh -----------------------------------------------------------------------

    /// Reads the repository again (`full` also re-reads the commit log). One read in flight, one
    /// queued. No-op while hidden.
    pub fn refresh(&mut self, full: bool, cx: &mut Context<Self>) {
        if !self.visible || self.repo.is_none() {
            return;
        }
        if self.refresh.request(full) {
            self.spawn_refresh(full, cx);
        }
    }

    fn spawn_refresh(&mut self, full: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let prev = self.data.clone();
        let epoch = self.epoch;
        self.stats.reads += 1;
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { data::load(&repo, prev.as_deref(), full) })
                .await;
            let _ = this.update(cx, |m, cx| m.apply_data(epoch, loaded, cx));
        }));
    }

    fn apply_data(&mut self, epoch: u64, loaded: Result<GitData, String>, cx: &mut Context<Self>) {
        if epoch != self.epoch {
            return;
        }
        let mut changed = false;
        match loaded {
            Ok(d) => {
                if self.data.as_deref() != Some(&d) {
                    self.data = Some(Arc::new(d));
                    changed = true;
                }
                if self.load != Load::Ready {
                    self.load = Load::Ready;
                    changed = true;
                }
            }
            Err(e) => {
                let failed = Load::Failed(e);
                if self.load != failed {
                    self.load = failed;
                    changed = true;
                }
            }
        }
        if changed {
            self.data_changed(cx);
            self.branch_maybe_changed(cx);
            cx.notify();
        }
        match self.refresh.finish() {
            Some(full) if self.visible => self.spawn_refresh(full, cx),
            Some(_) => self.refresh.reset(),
            None => {}
        }
    }

    /// The data changed: keep the open diff / checkpoint view honest.
    fn data_changed(&mut self, cx: &mut Context<Self>) {
        let (Some(data), Some(slot)) = (self.data.clone(), self.diff.as_ref()) else {
            self.drop_missing_checkpoint_view();
            return;
        };
        if let DiffTarget::Working { section, path } = &slot.target {
            if !crate::sections::contains(&data.status, *section, path) {
                self.diff = None;
                self.diff_task = None;
            } else {
                let target = slot.target.clone();
                self.load_diff(target, cx);
            }
        }
        self.drop_missing_checkpoint_view();
    }

    fn drop_missing_checkpoint_view(&mut self) {
        if let (Some(view), Some(data)) = (&self.checkpoint_view, &self.data)
            && !data.checkpoints.iter().any(|c| c.id == view.id)
        {
            self.checkpoint_view = None;
            if matches!(
                self.diff,
                Some(DiffSlot {
                    target: DiffTarget::Checkpoint { .. },
                    ..
                })
            ) {
                self.diff = None;
            }
        }
    }

    // ----- operations --------------------------------------------------------------------

    /// Asks for `op`. Quiet operations (stage, unstage, fetch, create checkpoint...) run at once;
    /// sensitive ones compute their preflight and wait on the card for [`GitModel::confirm`].
    pub fn request(&mut self, op: GitOp, cx: &mut Context<Self>) {
        if self.read_only {
            self.quiet_error = Some("Solo lectura: esta vista no modifica el repositorio".into());
            cx.notify();
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if let Err(e) = op.validate() {
            self.quiet_error = Some(e);
            cx.notify();
            return;
        }
        self.quiet_error = None;
        self.gate_epoch += 1;
        let epoch = self.gate_epoch;
        match self.gate.begin(op, epoch) {
            Step::Run(op) => self.enqueue(op, cx),
            Step::Busy => {}
            Step::Preflight { op, epoch } => {
                let model_epoch = self.epoch;
                cx.spawn(async move |this, cx| {
                    let review = cx
                        .background_executor()
                        .spawn(async move { data::preflight(&repo, &op) })
                        .await;
                    let _ = this.update(cx, |m, cx| {
                        if model_epoch == m.epoch {
                            m.gate.preflight_ready(epoch, review);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// "Confirmar": releases the operation the card shows. Does nothing when no card is showing
    /// or the review is blocked.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(op) = self.gate.confirm() else {
            return;
        };
        if self.exec_mode == ExecMode::Terminal && !op.argv_steps().is_empty() {
            for argv in op.argv_steps() {
                self.emit_terminal(argv, cx);
            }
            let line = op
                .argv_steps()
                .iter()
                .map(|a| a.join(" "))
                .collect::<Vec<_>>()
                .join(" && ");
            self.gate.finished(
                op.clone(),
                Outcome {
                    ok: true,
                    summary: "Enviado a la terminal".into(),
                    detail: line,
                    retry: None,
                },
            );
            cx.emit(OpFinished { op, ok: true });
            self.refresh(true, cx);
        } else {
            self.enqueue(op, cx);
        }
        cx.notify();
    }

    /// "Cancelar" / dismisses the banner and the quiet error.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.gate.cancel();
        self.quiet_error = None;
        cx.notify();
    }

    fn enqueue(&mut self, op: GitOp, cx: &mut Context<Self>) {
        self.queue.push_back(op);
        self.pump(cx);
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.exec_busy {
            return;
        }
        let (Some(op), Some(repo)) = (self.queue.pop_front(), self.repo.clone()) else {
            return;
        };
        self.exec_busy = true;
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let for_run = op.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move { data::execute(&repo, &for_run) })
                .await;
            let _ = this.update(cx, |m, cx| m.finished(epoch, op, outcome, cx));
        })
        .detach();
    }

    fn finished(&mut self, epoch: u64, op: GitOp, outcome: Outcome, cx: &mut Context<Self>) {
        if epoch != self.epoch {
            return;
        }
        self.exec_busy = false;
        let ok = outcome.ok;
        let summary = outcome.summary.clone();
        if !self.gate.finished(op.clone(), outcome) && !ok {
            self.quiet_error = Some(summary);
        }
        cx.emit(OpFinished { op: op.clone(), ok });
        if pr::refresh_after(&op, ok) {
            self.refresh_pr(PrTrigger::AfterPush, cx);
        }
        // History changes with everything except plain staging.
        let full = !matches!(
            op,
            GitOp::Stage(_) | GitOp::Unstage { .. } | GitOp::StageAll | GitOp::UnstageAll { .. }
        );
        self.refresh(full, cx);
        self.pump(cx);
        cx.notify();
    }

    fn emit_terminal(&mut self, argv: Vec<String>, cx: &mut Context<Self>) {
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
        cx.emit(GitPanelEvent::RunInTerminal { argv, cwd: root });
    }

    /// "Reintentar en la terminal" of a failed network operation.
    pub fn retry_in_terminal(&mut self, cx: &mut Context<Self>) {
        let argv = match &self.gate {
            Gate::Done {
                outcome: Outcome {
                    retry: Some(argv), ..
                },
                ..
            } => argv.clone(),
            _ => return,
        };
        self.emit_terminal(argv, cx);
        self.gate.cancel();
        cx.notify();
    }

    // ----- diff --------------------------------------------------------------------------

    /// Opens (or closes, when it is already open) the inline diff of a file.
    pub fn toggle_diff(&mut self, target: DiffTarget, cx: &mut Context<Self>) {
        if self.diff.as_ref().is_some_and(|d| d.target == target) {
            self.diff = None;
            self.diff_task = None;
            cx.notify();
            return;
        }
        self.load_diff(target, cx);
        cx.notify();
    }

    pub fn close_diff(&mut self, cx: &mut Context<Self>) {
        if self.diff.take().is_some() {
            self.diff_task = None;
            cx.notify();
        }
    }

    fn load_diff(&mut self, target: DiffTarget, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        // Reloading the same file keeps showing the old preview instead of flashing "Cargando".
        let keep = match &self.diff {
            Some(DiffSlot { target: t, state }) if *t == target => Some(state.clone()),
            _ => None,
        };
        self.diff = Some(DiffSlot {
            target: target.clone(),
            state: keep.unwrap_or(DiffState::Loading),
        });
        let epoch = self.epoch;
        let wanted = target.clone();
        self.diff_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { data::load_diff(&repo, &target) })
                .await;
            let _ = this.update(cx, |m, cx| {
                if epoch != m.epoch {
                    return;
                }
                let Some(slot) = m.diff.as_mut().filter(|s| s.target == wanted) else {
                    return;
                };
                let state = match result {
                    Ok(p) => DiffState::Ready(Arc::new(p)),
                    Err(e) => DiffState::Failed(e),
                };
                if slot.state != state {
                    slot.state = state;
                    cx.notify();
                }
            });
        }));
    }

    /// "Agente": sends the open diff to an agent.
    pub fn ask_agent(&mut self, cx: &mut Context<Self>) {
        let Some(DiffSlot {
            target,
            state: DiffState::Ready(preview),
        }) = self.diff.clone()
        else {
            return;
        };
        let context = match &target {
            DiffTarget::Working { section, .. } => section.title().to_lowercase(),
            DiffTarget::Checkpoint { id, .. } => {
                let label = self
                    .data
                    .as_ref()
                    .and_then(|d| d.checkpoints.iter().find(|c| &c.id == id))
                    .map_or_else(|| id.clone(), |c| c.label.clone());
                format!("cambios desde el checkpoint «{label}»")
            }
        };
        cx.emit(GitPanelEvent::AskAgent {
            context_markdown: agent_markdown(&context, &preview),
        });
    }

    /// "Abrir": the file of the open diff, at its first change.
    pub fn open_file(&mut self, path: &Path, line: Option<usize>, cx: &mut Context<Self>) {
        if let Some(root) = self.root() {
            cx.emit(GitPanelEvent::OpenFile {
                path: root.join(path),
                line,
            });
        }
    }

    /// "Ampliar": the full diff in the host's viewer.
    pub fn open_diff(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(root) = self.root() {
            cx.emit(GitPanelEvent::OpenDiff {
                path: root.join(path),
            });
        }
    }

    // ----- checkpoints -------------------------------------------------------------------

    /// "Ver cambios desde aquí": lists what changed since the checkpoint (click again to close).
    pub fn toggle_checkpoint_view(&mut self, id: String, cx: &mut Context<Self>) {
        if self.checkpoint_view.as_ref().is_some_and(|v| v.id == id) {
            self.checkpoint_view = None;
            if matches!(
                self.diff,
                Some(DiffSlot {
                    target: DiffTarget::Checkpoint { .. },
                    ..
                })
            ) {
                self.diff = None;
            }
            cx.notify();
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.checkpoint_view = Some(CheckpointView {
            id: id.clone(),
            state: CheckpointFiles::Loading,
        });
        let epoch = self.epoch;
        let wanted = id.clone();
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { repo.changed_files_since(&id).map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |m, cx| {
                if epoch != m.epoch {
                    return;
                }
                if let Some(view) = m.checkpoint_view.as_mut().filter(|v| v.id == wanted) {
                    view.state = match files {
                        Ok(f) => CheckpointFiles::Ready(f),
                        Err(e) => CheckpointFiles::Failed(e),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    // ----- pull request and CI (B1) ----------------------------------------------------------
    //
    // Cost model: `gh` runs only on a trigger (see [`crate::pr`]): the branch changed, a push
    // finished, an explicit refresh, or a window focus when the last query is older than five
    // minutes. There is no timer. One query in flight, one coalesced. Off by default.

    /// Turns the PR / CI integration on with the system `gh`. Without `gh` installed and logged
    /// in, everything stays hidden.
    pub fn enable_pr_status(&mut self, cx: &mut Context<Self>) {
        self.set_pr_client(Some(GhClient::system()), cx);
    }

    /// Turns the integration on with a specific client (a fake `gh` in tests) or off (`None`).
    pub fn set_pr_client(&mut self, client: Option<GhClient>, cx: &mut Context<Self>) {
        self.gh = client;
        self.reset_pr();
        if self.gh.is_some() && self.repo.is_some() && self.visible {
            self.pr_branch = self.current_branch();
            self.refresh_pr(PrTrigger::BranchChanged, cx);
        }
        cx.notify();
    }

    fn reset_pr(&mut self) {
        self.pr_epoch += 1;
        self.pr_view = PrView::Idle;
        self.pr_branch = None;
        self.pr_queried = None;
        self.pr_inflight = false;
        self.pr_again = false;
        self.pr_task = None;
        self.pr_sending = false;
        self.pr_create = PrCreate::Idle;
        self.gh_ok = false;
    }

    fn current_branch(&self) -> Option<String> {
        self.data.as_ref().and_then(|d| d.status.branch.clone())
    }

    /// What the panel knows about the PR of the current branch.
    pub fn pr_view(&self) -> &PrView {
        &self.pr_view
    }

    /// The loaded PR and checks, if any.
    pub fn pr_snapshot(&self) -> Option<&Arc<PrSnapshot>> {
        match &self.pr_view {
            PrView::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// The one-line summary for the status bar (`PR #12 · 2 checks fallan` / `PR #12 · ✓ CI`),
    /// `None` when there is no PR, `gh` is unavailable or the integration is off.
    pub fn pr_summary(&self) -> Option<PrSummary> {
        self.pr_snapshot().map(|s| pr::summarize(&s.pr, &s.checks))
    }

    /// A query is in flight.
    pub fn pr_loading(&self) -> bool {
        self.pr_inflight
    }

    /// `gh` queries issued so far (tests and the doctor: proves there is no polling).
    pub fn pr_queries(&self) -> u32 {
        self.pr_queries
    }

    pub fn pr_last_queried(&self) -> Option<Instant> {
        self.pr_queried
    }

    /// "Actualizar": asks again now (and re-probes `gh`, in case the user logged in).
    pub fn refresh_pr_now(&mut self, cx: &mut Context<Self>) {
        self.gh_ok = false;
        self.refresh_pr(PrTrigger::Manual, cx);
    }

    /// The host's window gained focus: refreshes only when the last query is older than five
    /// minutes (or there was none).
    pub fn on_window_focus(&mut self, cx: &mut Context<Self>) {
        self.on_window_focus_at(Instant::now(), cx);
    }

    /// [`GitModel::on_window_focus`] with an explicit clock.
    pub fn on_window_focus_at(&mut self, now: Instant, cx: &mut Context<Self>) {
        if self.gh.is_some() && pr::focus_refresh_due(self.pr_queried, now) {
            self.refresh_pr(PrTrigger::WindowFocus, cx);
        }
    }

    /// A push the host ran itself (`ExecMode::Terminal`) finished: its PR / CI may have changed.
    pub fn notify_push_finished(&mut self, cx: &mut Context<Self>) {
        self.refresh_pr(PrTrigger::AfterPush, cx);
    }

    /// After every repository read: a new branch is a new PR query.
    fn branch_maybe_changed(&mut self, cx: &mut Context<Self>) {
        if self.gh.is_none() {
            return;
        }
        let branch = self.current_branch();
        if branch == self.pr_branch {
            return;
        }
        self.pr_branch = branch.clone();
        if branch.is_some() {
            self.refresh_pr(PrTrigger::BranchChanged, cx);
        } else {
            self.pr_view = PrView::Detached;
        }
    }

    fn refresh_pr(&mut self, trigger: PrTrigger, cx: &mut Context<Self>) {
        let (Some(client), Some(repo)) = (self.gh.clone(), self.repo.clone()) else {
            return;
        };
        if self.pr_inflight {
            self.pr_again = true;
            return;
        }
        self.pr_inflight = true;
        self.pr_queries += 1;
        // Another branch's PR must not linger while the new one loads.
        if trigger == PrTrigger::BranchChanged
            || matches!(self.pr_view, PrView::Idle | PrView::Detached)
        {
            self.pr_view = PrView::Loading;
        }
        cx.notify();
        let epoch = self.pr_epoch;
        let known = self.gh_ok;
        self.pr_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { pr::query_pr(&client, &repo, known) })
                .await;
            let _ = this.update(cx, |m, cx| m.apply_pr(epoch, result, cx));
        }));
    }

    fn apply_pr(&mut self, epoch: u64, result: PrQuery, cx: &mut Context<Self>) {
        if epoch != self.pr_epoch {
            return;
        }
        self.pr_inflight = false;
        self.pr_queried = Some(Instant::now());
        let prev = self.pr_view.clone();
        let next = match result {
            PrQuery::Unavailable => {
                self.gh_ok = false;
                PrView::Unavailable
            }
            PrQuery::NoBranch => {
                self.gh_ok = true;
                PrView::Detached
            }
            PrQuery::NoPr { branch } => {
                self.gh_ok = true;
                match &prev {
                    // `gh` answered nothing for a branch that had a PR a moment ago: most likely
                    // a failed query (offline), so keep what is shown.
                    PrView::Ready(s) if s.branch == branch => prev.clone(),
                    _ => PrView::NoPr { branch },
                }
            }
            PrQuery::Ready(snap) => {
                self.gh_ok = true;
                PrView::Ready(Arc::new(snap))
            }
        };
        self.pr_view = next;
        cx.notify();
        if std::mem::take(&mut self.pr_again) {
            self.refresh_pr(PrTrigger::Manual, cx);
        }
    }

    /// "Enviar checks fallidos al agente": downloads the logs of the failing checks (at most
    /// three, redacted) and emits [`GitPanelEvent::AskAgent`] with them.
    pub fn send_failed_checks_to_agent(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(repo), Some(snap)) = (
            self.gh.clone(),
            self.repo.clone(),
            self.pr_snapshot().cloned(),
        ) else {
            return;
        };
        if self.pr_sending {
            return;
        }
        let failing: Vec<_> = snap.failing().take(MAX_FAILED_SENT).cloned().collect();
        if failing.is_empty() {
            return;
        }
        self.pr_sending = true;
        cx.notify();
        let epoch = self.pr_epoch;
        cx.spawn(async move |this, cx| {
            let logs = cx
                .background_executor()
                .spawn(async move {
                    failing
                        .into_iter()
                        .map(|c| {
                            let log = client.failed_check_log(&repo, &c);
                            (c, log)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |m, cx| {
                if epoch != m.pr_epoch {
                    return;
                }
                m.pr_sending = false;
                cx.emit(GitPanelEvent::AskAgent {
                    context_markdown: pr::failed_checks_markdown(&snap, &logs),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// The logs of the failing checks are being downloaded.
    pub fn is_sending_failed_checks(&self) -> bool {
        self.pr_sending
    }

    // ----- "Crear PR" (B3) ---------------------------------------------------------------------

    pub fn pr_create(&self) -> &PrCreate {
        &self.pr_create
    }

    /// "Revisar": validates the PR and shows the confirmation card with the **exact command**.
    /// Nothing runs until [`GitModel::confirm_create_pr`].
    pub fn request_create_pr(&mut self, mut pr: NewPr, cx: &mut Context<Self>) {
        if self.read_only {
            self.quiet_error = Some("Solo lectura: esta vista no modifica el repositorio".into());
            cx.notify();
            return;
        }
        if self.gh.is_none()
            || self.repo.is_none()
            || matches!(self.pr_create, PrCreate::Running(_))
        {
            return;
        }
        // The card names the branch the PR is made from.
        if pr.head.is_none() {
            pr.head = self.current_branch();
        }
        if let Err(e) = pr.validate() {
            self.quiet_error = Some(e);
            cx.notify();
            return;
        }
        self.quiet_error = None;
        self.pr_create = PrCreate::Confirm(pr);
        cx.notify();
    }

    /// "Confirmar" on the PR card: runs `gh pr create` (the body goes by stdin). Does nothing
    /// unless the card is showing.
    pub fn confirm_create_pr(&mut self, cx: &mut Context<Self>) {
        let PrCreate::Confirm(pr) = self.pr_create.clone() else {
            return;
        };
        let (Some(client), Some(repo)) = (self.gh.clone(), self.repo.clone()) else {
            return;
        };
        self.pr_create = PrCreate::Running(pr.clone());
        cx.notify();
        let epoch = self.pr_epoch;
        cx.spawn(async move |this, cx| {
            let for_run = pr.clone();
            let result = cx
                .background_executor()
                .spawn(async move { client.create_pr(&repo, &for_run) })
                .await;
            let _ = this.update(cx, |m, cx| {
                if epoch != m.pr_epoch {
                    return;
                }
                match result {
                    Ok(url) => {
                        m.pr_create = PrCreate::Done { url: url.clone() };
                        cx.emit(PrCreated { url });
                        m.refresh_pr(PrTrigger::AfterCreate, cx);
                    }
                    Err(error) => m.pr_create = PrCreate::Failed { pr, error },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Cancelar" / "Cerrar" on the PR card.
    pub fn cancel_create_pr(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.pr_create, PrCreate::Idle | PrCreate::Running(_)) {
            self.pr_create = PrCreate::Idle;
            cx.notify();
        }
    }

    // ----- "Redactar con agente" (B3) ----------------------------------------------------------
    //
    // Tokens are spent only when the user clicks the button: the click collects the (redacted,
    // capped) diff on the background executor and emits `GitPanelEvent::DraftMessage`. The host
    // runs the agent and answers with `set_draft` / `fail_draft`.

    /// A draft was asked and the host has not answered yet.
    pub fn drafting(&self) -> Option<DraftKind> {
        self.drafting
    }

    /// "Redactar con agente": collects the diff and emits [`GitPanelEvent::DraftMessage`].
    pub fn request_draft(&mut self, kind: DraftKind, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.drafting.is_some() {
            return;
        }
        self.quiet_error = None;
        self.drafting = Some(kind);
        cx.notify();
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let input = cx
                .background_executor()
                .spawn(async move { repo.draft_input(kind) })
                .await;
            let _ = this.update(cx, |m, cx| {
                if epoch != m.epoch || m.drafting != Some(kind) {
                    return;
                }
                match input {
                    Ok(i) if !i.is_empty() => cx.emit(GitPanelEvent::DraftMessage {
                        kind,
                        diff_stat: i.diff_stat,
                        staged_diff: i.diff,
                        recent_subjects: i.recent_subjects,
                    }),
                    Ok(_) => {
                        m.drafting = None;
                        m.quiet_error = Some(match kind {
                            DraftKind::Commit => "No hay cambios en staged que describir".into(),
                            DraftKind::PullRequest => {
                                "La rama no tiene cambios respecto a la rama base".into()
                            }
                        });
                    }
                    Err(e) => {
                        m.drafting = None;
                        m.quiet_error = Some(format!("No se pudo preparar el diff: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The host's answer: the drafted text (for a commit, subject, blank line, body; for a PR,
    /// title, blank line, description). Fills the commit box / PR form, where the user can edit it
    /// before confirming. Ignored when no draft of that kind is pending (the user cancelled).
    pub fn set_draft(&mut self, kind: DraftKind, text: &str, cx: &mut Context<Self>) {
        if self.drafting != Some(kind) {
            return;
        }
        self.drafting = None;
        self.drafts.retain(|(k, _)| *k != kind);
        self.drafts.push((kind, text.to_owned()));
        cx.emit(DraftReady {
            kind,
            text: text.to_owned(),
        });
        cx.notify();
    }

    /// The host could not produce the draft (agent missing, cancelled, failed).
    pub fn fail_draft(&mut self, kind: DraftKind, message: &str, cx: &mut Context<Self>) {
        if self.drafting == Some(kind) {
            self.drafting = None;
            self.quiet_error = Some(message.to_owned());
            cx.notify();
        }
    }

    /// "Cancelar" while drafting: a late answer is ignored.
    pub fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        if self.drafting.take().is_some() {
            cx.notify();
        }
    }

    /// The latest draft of `kind` nobody picked up yet (a panel opened after the answer arrived).
    pub fn take_draft(&mut self, kind: DraftKind) -> Option<String> {
        let i = self.drafts.iter().position(|(k, _)| *k == kind)?;
        Some(self.drafts.remove(i).1)
    }

    // ----- worktrees ---------------------------------------------------------------------

    /// Fills the `dirty` flag of every worktree (one `git status` each). Call it when the
    /// switcher opens.
    pub fn refresh_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move { data::load_worktrees_dirty(&repo) })
                .await;
            let _ = this.update(cx, |m, cx| {
                if epoch != m.epoch {
                    return;
                }
                if let (Ok(list), Some(data)) = (list, m.data.as_mut()) {
                    let d = Arc::make_mut(data);
                    if *d.worktrees != list {
                        d.worktrees = Arc::new(list);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// "Abrir en editor": spawns the editor on the worktree, detached and without a console.
    pub fn open_in_editor(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let argv = match repo.open_in_editor(path, &self.editor) {
            Ok(a) => a,
            Err(e) => {
                self.quiet_error = Some(format!("No se pudo abrir el editor: {e}"));
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { spawn_detached(&argv) })
                .await;
            if let Err(e) = res {
                let _ = this.update(cx, |m, cx| {
                    m.quiet_error = Some(e);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

fn spawn_detached(argv: &[String]) -> Result<(), String> {
    let (exe, args) = argv.split_first().ok_or("comando vacío")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn()
        .map(drop)
        .map_err(|e| format!("No se pudo abrir el editor ({exe}): {e}"))
}
