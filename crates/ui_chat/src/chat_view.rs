//! `ChatView`: the body of a chat tab. Header, virtualized conversation (`gpui::list`, variable
//! heights), plan bar + composer, and the "¿Qué construimos?" screen of a new thread.
//!
//! The conversation is *pulled*: a [`ThreadUpdate`] only marks the view dirty and notifies it;
//! the rows are rebuilt from `ThreadState` when the view is painted. A hidden thread is never
//! painted, so it reads nothing. Only the rows whose fingerprint changed are re-measured.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use agents_driver::{AgentKind, PermissionMode};
use chat_core::{
    ApprovalDecision, ChatError, ContextUsage, Plan, ThreadId, ThreadRecord, ThreadRuntime, ThreadSpec, ThreadStatus, ThreadUpdate,
    should_checkpoint,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, ListAlignment,
    ListState, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window, div, list,
    prelude::*, px, relative,
};
use panel_api::WidthClass;
use theme::{Radius, Theme};
use ui_kit::{Icon, Logo, Pill, Placement, Popover, Spinner, agent_icon, icon};

use crate::composer::{Composer, ComposerEvent, ComposerState};
use crate::field::{ChatField, FieldEvent};
use crate::hub::ChatHub;
use crate::options::{agent_mark, model_label};
use crate::rows::{ChatRow, RowCache, RowKey, build_rows, diff_rows, keys_of};
use crate::util::{project_name, read_branch};
use crate::widgets::menu_row;

/// Widest reading column at XL (docs/DESIGN.md, Responsive).
pub const READING_WIDTH_XL: f32 = 860.0;
/// Window width from which the reading column is capped.
pub const XL_FROM: f32 = 2560.0;

/// What a new thread will be created with.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftSpec {
    pub project: Option<PathBuf>,
    pub agent: AgentKind,
    pub model: Option<String>,
    pub permission: PermissionMode,
    pub use_worktree: bool,
}

impl DraftSpec {
    pub fn new(project: Option<PathBuf>) -> Self {
        Self { project, agent: AgentKind::ClaudeCode, model: None, permission: PermissionMode::Ask, use_worktree: false }
    }

    /// The `ThreadSpec` of this draft, or why it cannot be created yet.
    pub fn to_thread_spec(&self) -> Result<ThreadSpec, &'static str> {
        let project = self.project.clone().ok_or("Elige un proyecto para empezar el hilo.")?;
        let mut spec = ThreadSpec::new(project, self.agent.clone());
        spec.model = self.model.clone();
        spec.permission = self.permission;
        spec.use_worktree = self.use_worktree;
        spec.checkpoint_before_turn = should_checkpoint(self.permission);
        Ok(spec)
    }
}

pub enum ViewMode {
    /// Opening an existing thread (loading its messages in the background).
    Loading,
    Failed(String),
    /// "¿Qué construimos?": no thread yet.
    Draft { spec: DraftSpec, creating: bool, error: Option<String> },
    Thread,
}

/// Emitted to the owning tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatViewEvent {
    /// A draft became a thread.
    Bound(ThreadId),
}

/// The slice of `ThreadState` the view shows besides the rows.
#[derive(Clone, Debug, Default)]
struct Meta {
    status: ThreadStatus,
    model: Option<String>,
    context: Option<ContextUsage>,
    plan: Option<Plan>,
}

pub struct ChatView {
    pub(crate) hub: Entity<ChatHub>,
    pub(crate) theme: Theme,
    mode: ViewMode,
    runtime: Option<ThreadRuntime>,
    record: Option<ThreadRecord>,
    meta: Meta,
    composer: Entity<Composer>,
    pub(crate) rows: Vec<Rc<ChatRow>>,
    keys: Vec<(RowKey, u64)>,
    cache: RowCache,
    list: ListState,
    pub(crate) expanded: HashSet<RowKey>,
    synced_rev: u64,
    dirty: bool,
    snap_bottom: bool,
    cstate_dirty: bool,
    title_edit: Option<Entity<ChatField>>,
    title_subs: Vec<Subscription>,
    branch: Option<String>,
    pending_perm: Option<PermissionMode>,
    pending_model: Option<Option<String>>,
    notice: Option<String>,
    project_menu_open: bool,
    viewport_w: f32,
    reading_width: Option<Option<f32>>,
    thread_sub: Option<Subscription>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).field().read(cx).focus_handle(cx)
    }
}

impl ChatView {
    fn base(hub: Entity<ChatHub>, theme: Theme, mode: ViewMode, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(hub.clone(), theme, cx));
        let subs = vec![
            cx.subscribe(&composer, |this, _c, ev: &ComposerEvent, cx| this.on_composer(ev.clone(), cx)),
            cx.observe(&hub, |this, _h, cx| {
                this.cstate_dirty = true;
                cx.notify();
            }),
        ];
        Self {
            hub,
            theme,
            mode,
            runtime: None,
            record: None,
            meta: Meta::default(),
            composer,
            rows: Vec::new(),
            keys: Vec::new(),
            cache: RowCache::default(),
            list: ListState::new(0, ListAlignment::Bottom, px(1200.0)),
            expanded: HashSet::new(),
            synced_rev: u64::MAX,
            dirty: true,
            snap_bottom: false,
            cstate_dirty: true,
            title_edit: None,
            title_subs: Vec::new(),
            branch: None,
            pending_perm: None,
            pending_model: None,
            notice: None,
            project_menu_open: false,
            viewport_w: 1200.0,
            reading_width: None,
            thread_sub: None,
            _subs: subs,
        }
    }

    /// A view that will show an existing thread once [`ChatView::bind_runtime`] is called.
    pub fn loading(hub: Entity<ChatHub>, theme: Theme, cx: &mut Context<Self>) -> Self {
        Self::base(hub, theme, ViewMode::Loading, cx)
    }

    /// A "¿Qué construimos?" view for a thread that does not exist yet.
    pub fn draft(hub: Entity<ChatHub>, theme: Theme, project: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self::base(hub, theme, ViewMode::Draft { spec: DraftSpec::new(project), creating: false, error: None }, cx)
    }

    // ----- accessors -----------------------------------------------------------------------

    pub fn mode(&self) -> &ViewMode {
        &self.mode
    }

    #[cfg(test)]
    pub(crate) fn mode_mut(&mut self) -> &mut ViewMode {
        &mut self.mode
    }

    pub fn is_draft(&self) -> bool {
        matches!(self.mode, ViewMode::Draft { .. })
    }

    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    pub fn runtime(&self) -> Option<&ThreadRuntime> {
        self.runtime.as_ref()
    }

    pub fn thread_id(&self) -> Option<ThreadId> {
        self.runtime.as_ref().map(ThreadRuntime::id)
    }

    /// The thread's metadata as of the last paint.
    pub fn record(&self) -> Option<&ThreadRecord> {
        self.record.as_ref()
    }

    pub fn draft_spec(&self) -> Option<&DraftSpec> {
        match &self.mode {
            ViewMode::Draft { spec, .. } => Some(spec),
            _ => None,
        }
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Directory the agent runs in (or the draft's project).
    pub fn cwd(&self) -> PathBuf {
        match (&self.record, &self.mode) {
            (Some(r), _) => r.cwd().to_path_buf(),
            (None, ViewMode::Draft { spec, .. }) => spec.project.clone().unwrap_or_default(),
            _ => PathBuf::new(),
        }
    }

    /// The rows currently shown.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        self.composer.update(cx, |c, cx| c.set_theme(theme, cx));
        // Colors changed under cached measurements only through text (never size): repaint.
        cx.notify();
    }

    /// Overrides the reading column cap (`None` = full width, default: 860 px from XL).
    pub fn set_reading_width(&mut self, width: Option<f32>, cx: &mut Context<Self>) {
        self.reading_width = Some(width);
        self.list.reset(self.rows.len());
        cx.notify();
    }

    pub(crate) fn column_max(&self) -> Option<f32> {
        match self.reading_width {
            Some(w) => w,
            None if self.viewport_w >= XL_FROM => Some(READING_WIDTH_XL),
            None => None,
        }
    }

    pub fn width_class(&self) -> WidthClass {
        WidthClass::from_width(self.viewport_w)
    }

    // ----- lifecycle -----------------------------------------------------------------------

    /// The thread finished loading (or was created): show it.
    pub fn bind_runtime(&mut self, rt: ThreadRuntime, cx: &mut Context<Self>) {
        let id = rt.id();
        self.record = Some(rt.record());
        self.runtime = Some(rt);
        self.mode = ViewMode::Thread;
        self.cache = RowCache::default();
        self.rows.clear();
        self.keys.clear();
        self.list.reset(0);
        self.synced_rev = u64::MAX;
        self.dirty = true;
        self.cstate_dirty = true;
        self.pending_perm = None;
        self.pending_model = None;
        self.thread_sub = Some(cx.subscribe(&self.hub, move |this, _h, u: &ThreadUpdate, cx| {
            if u.thread_id == id {
                this.on_update(u, cx);
            }
        }));
        self.refresh_branch(cx);
        cx.emit(ChatViewEvent::Bound(id));
        cx.notify();
    }

    /// Opening the thread failed.
    pub fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.mode = ViewMode::Failed(message);
        cx.notify();
    }

    /// Drops everything that is still listening (the tab was closed).
    pub fn release(&mut self) {
        self.thread_sub = None;
        self.title_edit = None;
        self.title_subs.clear();
    }

    fn on_update(&mut self, u: &ThreadUpdate, cx: &mut Context<Self>) {
        if u.changed.list {
            self.dirty = true;
        }
        if u.changed.status && u.status == ThreadStatus::Done {
            self.refresh_branch(cx);
        }
        cx.notify();
    }

    fn refresh_branch(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd();
        if cwd.as_os_str().is_empty() {
            return;
        }
        let task = cx.background_spawn(async move { read_branch(&cwd) });
        cx.spawn(async move |this, cx| {
            let branch = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.branch != branch {
                    this.branch = branch;
                    this.cstate_dirty = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ----- sync ----------------------------------------------------------------------------

    /// Brings rows, list and composer in line with the thread. Cheap when nothing changed.
    pub(crate) fn sync(&mut self, cx: &mut Context<Self>) {
        if let Some(rt) = self.runtime.clone() {
            let rev = rt.revision();
            let force = std::mem::take(&mut self.dirty);
            if rev != self.synced_rev || force {
                self.synced_rev = rev;
                let cache = &mut self.cache;
                let (rows, meta) = rt.with_state(|st| {
                    (
                        build_rows(st, cache),
                        Meta { status: st.status, model: st.model.clone(), context: st.context, plan: st.plan.clone() },
                    )
                });
                self.meta = meta;
                let record = rt.record();
                if self.pending_perm == Some(record.permission) {
                    self.pending_perm = None;
                }
                if self.pending_model.as_ref().is_some_and(|m| *m == record.model) {
                    self.pending_model = None;
                }
                self.record = Some(record);
                let new_keys = keys_of(&rows);
                if let Some(sp) = diff_rows(&self.keys, &new_keys) {
                    self.list.splice(sp.old, sp.new_len);
                }
                self.rows = rows;
                self.keys = new_keys;
                if std::mem::take(&mut self.snap_bottom) {
                    // Jump to the end and follow it again.
                    self.list.reset(self.rows.len());
                }
                self.cstate_dirty = true;
            }
        }
        if std::mem::take(&mut self.cstate_dirty) {
            let state = self.composer_state(cx);
            self.composer.update(cx, |c, cx| {
                c.set_state(state, cx);
            });
        }
    }

    fn composer_state(&self, cx: &App) -> ComposerState {
        let hub = self.hub.read(cx);
        let agents = hub.agents().to_vec();
        match (&self.mode, &self.record) {
            (ViewMode::Draft { spec, .. }, _) => ComposerState {
                working: false,
                context: None,
                permission: spec.permission,
                can_edit_permission: true,
                agent: spec.agent.clone(),
                model: spec.model.clone(),
                agents,
                project_root: spec.project.clone(),
                branch: None,
                path: None,
                plan: None,
                placeholder: "¿Qué quieres construir?  (@ para mencionar archivos)".into(),
            },
            (_, Some(r)) => ComposerState {
                working: self.meta.status.is_busy(),
                context: self.meta.context,
                permission: self.pending_perm.unwrap_or(r.permission),
                can_edit_permission: !self.meta.status.is_busy(),
                agent: r.agent.clone(),
                model: match &self.pending_model {
                    Some(m) => m.clone(),
                    None => r.model.clone().or_else(|| self.meta.model.clone()),
                },
                agents,
                project_root: Some(r.cwd().to_path_buf()),
                branch: self.branch.clone(),
                path: Some(r.cwd().to_path_buf()),
                plan: self.meta.plan.clone(),
                placeholder: "Escribe un mensaje…  (@ para mencionar archivos)".into(),
            },
            _ => ComposerState { agents, ..ComposerState::default() },
        }
    }

    // ----- actions -------------------------------------------------------------------------

    /// Expands or collapses a row, re-measuring only that item.
    pub fn toggle(&mut self, key: &RowKey, cx: &mut Context<Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.clone());
        }
        if let Some(ix) = self.keys.iter().position(|(k, _)| k == key) {
            self.list.splice(ix..ix + 1, 1);
        }
        cx.notify();
    }

    fn set_notice(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        cx.notify();
    }

    /// Answers an approval row (Permitir / Rechazar / Siempre).
    pub fn approve(&mut self, request_id: &str, decision: ApprovalDecision, cx: &mut Context<Self>) {
        let Some(rt) = &self.runtime else { return };
        if let Err(e) = rt.approve(request_id, decision) {
            let msg = format!("No se pudo responder al permiso: {e}");
            self.set_notice(msg, cx);
        }
    }

    /// Sends a message to the thread (or creates it first, for a draft).
    pub fn send(&mut self, text: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.notice = None;
        if let Some(rt) = self.runtime.clone() {
            self.snap_bottom = true;
            if let Err(e) = rt.send(text, attachments) {
                self.set_notice(format!("No se pudo enviar: {e}"), cx);
            }
            cx.notify();
            return;
        }
        let ViewMode::Draft { spec, creating, error } = &mut self.mode else { return };
        if *creating {
            return;
        }
        let thread_spec = match spec.to_thread_spec() {
            Ok(s) => s,
            Err(why) => {
                *error = Some(why.to_string());
                // Put the text back: nothing was sent.
                let t = text.clone();
                self.composer.update(cx, |c, cx| c.set_text(&t, cx));
                cx.notify();
                return;
            }
        };
        *creating = true;
        *error = None;
        let store = self.hub.read(cx).store().clone();
        let task = cx.background_spawn(async move {
            let rec = store.create_thread(thread_spec)?;
            store.open(rec.id)
        });
        cx.spawn(async move |this, cx| {
            let res: Result<ThreadRuntime, ChatError> = task.await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(rt) => {
                    let sent = rt.send(text.clone(), attachments);
                    this.snap_bottom = true;
                    this.bind_runtime(rt, cx);
                    if let Err(e) = sent {
                        this.set_notice(format!("No se pudo enviar: {e}"), cx);
                    }
                }
                Err(e) => {
                    if let ViewMode::Draft { creating, error, .. } = &mut this.mode {
                        *creating = false;
                        *error = Some(format!("No se pudo crear el hilo: {e}"));
                    }
                    let t = text.clone();
                    this.composer.update(cx, |c, cx| c.set_text(&t, cx));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Interrupts the running turn.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(rt) = &self.runtime {
            if let Err(e) = rt.interrupt() {
                let msg = format!("No se pudo detener: {e}");
                self.set_notice(msg, cx);
            }
        }
    }

    fn on_composer(&mut self, ev: ComposerEvent, cx: &mut Context<Self>) {
        match ev {
            ComposerEvent::Send { text, attachments } => self.send(text, attachments, cx),
            ComposerEvent::Stop => self.stop(cx),
            ComposerEvent::SetPermission(mode) => match (&mut self.mode, self.runtime.clone()) {
                (ViewMode::Draft { spec, .. }, _) => {
                    spec.permission = mode;
                    self.cstate_dirty = true;
                    cx.notify();
                }
                (_, Some(rt)) => match rt.set_permission_mode(mode) {
                    Ok(()) => {
                        self.pending_perm = Some(mode);
                        self.cstate_dirty = true;
                        cx.notify();
                    }
                    Err(e) => self.set_notice(format!("No se pudo cambiar el permiso: {e}"), cx),
                },
                _ => {}
            },
            ComposerEvent::SetModel(model) => match (&mut self.mode, self.runtime.clone()) {
                (ViewMode::Draft { spec, .. }, _) => {
                    spec.model = model;
                    self.cstate_dirty = true;
                    cx.notify();
                }
                (_, Some(rt)) => {
                    let agent = self.record.as_ref().map(|r| r.agent.clone()).unwrap_or(AgentKind::ClaudeCode);
                    match rt.switch_agent_or_model(agent, model.clone()) {
                        Ok(()) => {
                            self.pending_model = Some(model);
                            self.cstate_dirty = true;
                            cx.notify();
                        }
                        Err(e) => self.set_notice(format!("No se pudo cambiar el modelo: {e}"), cx),
                    }
                }
                _ => {}
            },
            ComposerEvent::PickAgent(kind) => match (&mut self.mode, self.runtime.clone()) {
                (ViewMode::Draft { spec, .. }, _) => {
                    spec.agent = kind;
                    spec.model = None;
                    self.cstate_dirty = true;
                    self.composer.update(cx, |c, cx| c.close_popup(cx));
                    cx.notify();
                }
                (_, Some(rt)) => {
                    let target = kind.clone();
                    let task = cx.background_spawn(async move { rt.handoff_preview(&target, None) });
                    cx.spawn(async move |this, cx| {
                        let text = task.await.ok().flatten();
                        let _ = this.update(cx, |this, cx| {
                            this.composer.update(cx, |c, cx| c.set_handoff_preview(&kind, text, cx));
                        });
                    })
                    .detach();
                }
                _ => {}
            },
            ComposerEvent::ConfirmSwitch { agent, model } => {
                if let Some(rt) = &self.runtime {
                    if let Err(e) = rt.switch_agent_or_model(agent, model) {
                        let msg = format!("No se pudo cambiar de agente: {e}");
                        self.set_notice(msg, cx);
                    }
                }
            }
        }
    }

    // ----- title ---------------------------------------------------------------------------

    fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = &self.record else { return };
        let title = rec.display_title().to_string();
        let theme = self.theme;
        let field = cx.new(|cx| {
            let mut f = ChatField::new(theme, "Título del hilo", false, cx);
            f.set_font_size(15.0);
            f.set_text(&title, cx);
            f
        });
        let focus = field.read(cx).focus_handle(cx);
        self.title_subs = vec![
            cx.subscribe(&field, |this, f, ev: &FieldEvent, cx| match ev {
                FieldEvent::Submit => this.commit_rename(f.read(cx).text().to_string(), cx),
                FieldEvent::Cancel => this.cancel_rename(cx),
                _ => {}
            }),
            cx.on_focus_out(&focus, window, |this, _e, _w, cx| {
                if let Some(f) = this.title_edit.clone() {
                    let t = f.read(cx).text().to_string();
                    this.commit_rename(t, cx);
                }
            }),
        ];
        window.focus(&focus);
        self.title_edit = Some(field);
        cx.notify();
    }

    fn commit_rename(&mut self, text: String, cx: &mut Context<Self>) {
        if self.title_edit.take().is_none() {
            return;
        }
        self.title_subs.clear();
        let text = text.trim().to_string();
        if let (Some(id), Some(rec)) = (self.thread_id(), &self.record) {
            if !text.is_empty() && text != rec.display_title() {
                self.hub.update(cx, |h, cx| h.rename_thread(id, text, cx));
            }
        }
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.title_edit.take().is_some() {
            self.title_subs.clear();
            cx.notify();
        }
    }

    // ----- rendering -----------------------------------------------------------------------

    fn column(&self, content: impl IntoElement) -> impl IntoElement {
        let max = self.column_max();
        div().w_full().flex().flex_row().justify_center().px(px(24.0)).child(div().w_full().min_w_0().when_some(max, |d, w| d.max_w(px(w))).child(content))
    }

    fn header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let rec = self.record.clone();
        let title: AnyElement = match (&self.title_edit, &rec) {
            (Some(f), _) => div().w(px(380.0)).max_w_full().px(px(8.0)).py(px(3.0)).rounded(px(8.0)).bg(c.surface_2).child(f.clone()).into_any_element(),
            (None, Some(r)) => div()
                .id("chat-title")
                .max_w(px(520.0))
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(8.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_lg))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c.text)
                .cursor_text()
                .hover(move |s| s.bg(c.surface_2))
                .tooltip(ui_kit::Tooltip::text(&t, "Clic para renombrar"))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.begin_rename(window, cx)))
                .child(SharedString::from(r.display_title().to_string()))
                .into_any_element(),
            _ => div().into_any_element(),
        };
        let crumb = rec.as_ref().map(|r| {
            let project = project_name(&r.project_root);
            match &r.worktree_path {
                Some(w) => format!("{project} › {}", project_name(w)),
                None => match &self.branch {
                    Some(b) => format!("{project} › {b}"),
                    None => project,
                },
            }
        });
        let agent = rec.as_ref().map(|r| {
            let model = match &self.pending_model {
                Some(m) => m.clone(),
                None => r.model.clone().or_else(|| self.meta.model.clone()),
            };
            (agent_mark(&r.agent), model_label(model.as_deref()))
        });
        let narrow = self.viewport_w < 640.0;
        let inner = div()
            .h(px(52.0))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .font_family(ty.ui_family)
            .child(title)
            .when(!narrow, |d| {
                d.children(crumb.map(|b| div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(SharedString::from(b))))
            })
            .child(div().flex_1())
            .children(agent.map(|(mark, model)| {
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .child(agent_icon(mark, 16.0, c.muted))
                    .child(model)
            }));
        div().w_full().flex_none().child(self.column(inner)).into_any_element()
    }

    fn notice_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = self.notice.clone()?;
        let t = self.theme;
        let c = t.colors;
        Some(
            div()
                .id("chat-notice")
                .w_full()
                .mb(px(6.0))
                .px(px(14.0))
                .py(px(8.0))
                .rounded(px(Radius::XS))
                .bg(c.surface_1)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.5))
                .text_color(c.error)
                .child(icon(Icon::Alert, 16.0, c.error))
                .child(div().flex_1().min_w_0().child(SharedString::from(n)))
                .child(
                    div()
                        .id("chat-notice-x")
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(move |s| s.bg(c.surface_3))
                        .child(icon(Icon::Close, 10.0, c.muted))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_thread(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync(cx);
        let t = self.theme;
        let c = t.colors;
        let header = self.header(cx);
        let body: AnyElement = if self.rows.is_empty() {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.0))
                .text_color(c.muted)
                .child("Escribe un mensaje para empezar.")
                .into_any_element()
        } else {
            list(self.list.clone(), cx.processor(|this, ix: usize, _w, cx| this.render_row(ix, cx))).size_full().into_any_element()
        };
        let composer = div()
            .w_full()
            .flex_none()
            .pt(px(8.0))
            .pb(px(14.0))
            .child(self.column(div().flex().flex_col().children(self.notice_line(cx)).child(self.composer.clone())));
        div().size_full().flex().flex_col().child(header).child(div().flex_1().min_h_0().child(body)).child(composer).into_any_element()
    }

    fn project_pill(&self, spec: &DraftSpec, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let label = spec.project.as_deref().map(project_name).unwrap_or_else(|| "Elegir proyecto".to_string());
        let pill = Pill::new("draft-project", &t)
            .icon(Icon::Folder)
            .label(label)
            .child(icon(Icon::ChevronDown, 14.0, c.muted))
            .tooltip(spec.project.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Carpeta donde trabajará el agente".into()))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.project_menu_open = !this.project_menu_open;
                cx.notify();
            }));
        let projects = self.hub.read(cx).projects();
        let current = spec.project.clone();
        let mut rows: Vec<AnyElement> = projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = p.clone();
                let selected = current.as_deref() == Some(p.as_path());
                menu_row(
                    ("project", i),
                    &t,
                    Some(icon(Icon::Folder, 16.0, c.muted).into_any_element()),
                    project_name(p),
                    Some(p.display().to_string().into()),
                    selected,
                    true,
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_project(Some(path.clone()), cx)),
                )
                .into_any_element()
            })
            .collect();
        rows.push(
            menu_row(
                "project-pick",
                &t,
                Some(icon(Icon::FolderOpen, 16.0, c.muted).into_any_element()),
                "Elegir carpeta…",
                None,
                false,
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| this.pick_project(cx)),
            )
            .into_any_element(),
        );
        Popover::new("draft-project-pop", &t)
            .open(self.project_menu_open)
            .placement(Placement::BottomStart)
            .width(340.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| {
                this.update(cx, |v, cx| {
                    v.project_menu_open = false;
                    cx.notify();
                })
            })
            .trigger(pill)
            .children(rows)
            .into_any_element()
    }

    fn set_project(&mut self, project: Option<PathBuf>, cx: &mut Context<Self>) {
        self.project_menu_open = false;
        if let ViewMode::Draft { spec, error, .. } = &mut self.mode {
            spec.project = project.clone();
            *error = None;
        }
        self.hub.update(cx, |h, _| h.set_default_project(project));
        self.cstate_dirty = true;
        cx.notify();
    }

    fn pick_project(&mut self, cx: &mut Context<Self>) {
        self.project_menu_open = false;
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = rx.await {
                if let Some(p) = paths.pop() {
                    let _ = this.update(cx, |this, cx| this.set_project(Some(p), cx));
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn render_draft(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync(cx);
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let ViewMode::Draft { spec, creating, error } = &self.mode else { return div().into_any_element() };
        let (spec, creating, error) = (spec.clone(), *creating, error.clone());
        let project = self.project_pill(&spec, cx);
        let worktree = Pill::new("draft-worktree", &t)
            .icon(Icon::GitBranch)
            .label("Worktree")
            .active(spec.use_worktree)
            .tooltip("Crea un worktree git propio para este hilo: dos agentes en el mismo repo sin pisarse")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                if let ViewMode::Draft { spec, .. } = &mut this.mode {
                    spec.use_worktree = !spec.use_worktree;
                }
                cx.notify();
            }));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(24.0))
            .gap(px(22.0))
            .font_family(ty.ui_family)
            .child(Logo::new(t.mode).size(64.0))
            .child(div().text_size(px(30.0)).font_weight(FontWeight::SEMIBOLD).text_color(c.text).child("¿Qué construimos?"))
            .child(
                div()
                    .w_full()
                    .max_w(px(760.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().flex().flex_row().items_center().gap(px(4.0)).child(project).child(worktree))
                    .children(self.notice_line(cx))
                    .child(self.composer.clone())
                    .children(error.map(|e| div().px(px(8.0)).text_size(px(12.5)).text_color(c.error).child(SharedString::from(e))))
                    .children(creating.then(|| {
                        div()
                            .px(px(8.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(12.5))
                            .text_color(c.muted)
                            .child(Spinner::new(c.accent))
                            .child("Creando el hilo…")
                    })),
            )
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport_w = f32::from(window.viewport_size().width);
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let content: AnyElement = match &self.mode {
            ViewMode::Loading => div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .text_size(px(13.0))
                .text_color(c.muted)
                .child(Spinner::new(c.accent))
                .child("Abriendo el hilo…")
                .into_any_element(),
            ViewMode::Failed(msg) => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .text_color(c.muted)
                .child(icon(Icon::Alert, 16.0, c.error))
                .child(div().text_color(c.error).child(SharedString::from(msg.clone())))
                .into_any_element(),
            ViewMode::Draft { .. } => self.render_draft(cx),
            ViewMode::Thread => self.render_thread(cx),
        };
        div().size_full().relative().font_family(ty.ui_family).text_color(c.text).line_height(relative(1.4)).child(content)
    }
}
