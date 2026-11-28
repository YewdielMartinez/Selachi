//! `ChatTab`: the `panel_api::TabModel` of a chat thread. The shell lists it as a pill or as a
//! sidebar row (agent mark, title, model, Working / Waiting / Done, elapsed) and hosts its
//! [`ChatView`] in a pane of the Main zone.
//!
//! Closing the tab stops the thread's agent (the thread stays in the history) after the shell's
//! confirmation when a turn is running.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chat_core::{ChatStore, ThreadId, ThreadRecord, ThreadStatus, ThreadUpdate, UNTITLED, now_ms};
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, SharedString,
    Subscription, Window,
};
use panel_api::{CloseCheck, TabEvent, TabIcon, TabModel};
use theme::Theme;
use ui_kit::{Icon, RowStatus};

use crate::chat_view::{ChatView, ChatViewEvent};
use crate::hub::ChatHub;
use crate::options::{agent_name, model_effort_label, model_label};

pub struct ChatTab {
    hub: Entity<ChatHub>,
    view: Entity<ChatView>,
    thread: Option<ThreadId>,
    thread_sub: Option<Subscription>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for ChatTab {}

impl ChatTab {
    /// A tab for an existing thread; it loads the thread in the background.
    pub(crate) fn open(
        hub: Entity<ChatHub>,
        theme: Theme,
        store: ChatStore,
        id: ThreadId,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| ChatView::loading(hub.clone(), theme, cx));
        let mut tab = Self::with_view(hub.clone(), view, cx);
        tab.listen(id, cx);
        let task = cx.background_spawn(async move { store.open(id) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |tab, cx| {
                tab.view.update(cx, |v, cx| match res {
                    Ok(rt) => v.bind_runtime(rt, cx),
                    Err(e) => v.fail(format!("No se pudo abrir el hilo: {e}"), cx),
                });
                cx.emit(TabEvent::TitleChanged);
                cx.emit(TabEvent::StatusChanged);
                cx.emit(TabEvent::SubtitleChanged);
            });
        })
        .detach();
        tab
    }

    /// A tab with the "¿Qué construimos?" screen (the thread is created on the first message).
    pub(crate) fn draft(
        hub: Entity<ChatHub>,
        theme: Theme,
        project: Option<PathBuf>,
        permission: agents_driver::PermissionMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| ChatView::draft_with(hub.clone(), theme, project, permission, cx));
        Self::with_view(hub, view, cx)
    }

    fn with_view(hub: Entity<ChatHub>, view: Entity<ChatView>, cx: &mut Context<Self>) -> Self {
        let bound = cx.subscribe(&view, |this, _v, ev: &ChatViewEvent, cx| match ev {
            ChatViewEvent::Bound(id) => {
                let id = *id;
                if this.thread != Some(id) {
                    this.listen(id, cx);
                }
                let me = cx.entity().downgrade();
                this.hub.update(cx, |h, _| h.register_tab(id, me));
                cx.emit(TabEvent::TitleChanged);
                cx.emit(TabEvent::StatusChanged);
                cx.emit(TabEvent::SubtitleChanged);
            }
        });
        Self {
            hub,
            view,
            thread: None,
            thread_sub: None,
            _subs: vec![bound],
        }
    }

    fn listen(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        self.thread = Some(id);
        self.thread_sub = Some(
            cx.subscribe(&self.hub, move |this, _h, u: &ThreadUpdate, cx| {
                if Some(u.thread_id) != this.thread {
                    return;
                }
                if u.changed.status {
                    cx.emit(TabEvent::StatusChanged);
                }
                if u.changed.list {
                    cx.emit(TabEvent::TitleChanged);
                    cx.emit(TabEvent::SubtitleChanged);
                }
            }),
        );
    }

    pub fn view(&self) -> &Entity<ChatView> {
        &self.view
    }

    pub fn thread_id(&self) -> Option<ThreadId> {
        self.thread
    }

    /// Live metadata: the open runtime's record, else the hub's list entry.
    fn record(&self, cx: &App) -> Option<ThreadRecord> {
        let v = self.view.read(cx);
        v.runtime()
            .map(chat_core::ThreadRuntime::record)
            .or_else(|| {
                self.thread
                    .and_then(|id| self.hub.read(cx).thread(id).cloned())
            })
    }
}

impl TabModel for ChatTab {
    fn title(&self, cx: &App) -> SharedString {
        match self.record(cx) {
            Some(r) => r.display_title().to_string().into(),
            None => UNTITLED.into(),
        }
    }

    fn icon(&self, cx: &App) -> TabIcon {
        match self.record(cx) {
            Some(r) => TabIcon::Agent(agent_name(&r.agent).into()),
            None => match self.view.read(cx).draft_spec() {
                Some(s) => TabIcon::Agent(agent_name(&s.agent).into()),
                None => TabIcon::Kind(Icon::Agents),
            },
        }
    }

    fn subtitle(&self, cx: &App) -> Option<SharedString> {
        match self.record(cx) {
            Some(r) => Some(
                match model_effort_label(r.model.as_deref(), r.effort.as_deref()) {
                    Some(l) => l.into(),
                    None => chat_core::agent_label(&r.agent).into(),
                },
            ),
            None => self.view.read(cx).draft_spec().map(|s| {
                match model_effort_label(s.model.as_deref(), s.effort.as_deref()) {
                    Some(l) => l.into(),
                    None => model_label(None).into(),
                }
            }),
        }
    }

    fn status(&self, cx: &App) -> Option<RowStatus> {
        match self.record(cx)?.status {
            ThreadStatus::Working => Some(RowStatus::Working),
            ThreadStatus::WaitingForInput => Some(RowStatus::Waiting),
            ThreadStatus::Done => Some(RowStatus::Done),
            ThreadStatus::Error => Some(RowStatus::Error),
            ThreadStatus::Idle => None,
        }
    }

    fn last_activity(&self, cx: &App) -> Option<Instant> {
        let r = self.record(cx)?;
        let age = (now_ms() - r.updated_ms).max(0) as u64;
        Instant::now().checked_sub(Duration::from_millis(age))
    }

    fn cwd(&self, cx: &App) -> Option<PathBuf> {
        match self.record(cx) {
            Some(r) => Some(r.cwd().to_path_buf()),
            None => self
                .view
                .read(cx)
                .draft_spec()
                .and_then(|s| s.project.clone()),
        }
    }

    fn project_root(&self, cx: &App) -> Option<PathBuf> {
        match self.record(cx) {
            Some(r) => Some(r.project_root),
            None => self
                .view
                .read(cx)
                .draft_spec()
                .and_then(|s| s.project.clone()),
        }
    }

    fn agent(&self, cx: &App) -> Option<SharedString> {
        self.record(cx).map(|r| agent_name(&r.agent).into())
    }

    fn content(&self, _cx: &App) -> AnyView {
        self.view.clone().into()
    }

    fn can_close(&self, cx: &App) -> CloseCheck {
        match self.record(cx) {
            Some(r) if r.status.is_busy() => {
                CloseCheck::Confirm("El agente sigue trabajando; al cerrar la pestaña se detiene (el hilo se conserva).".into())
            }
            _ => CloseCheck::Allow,
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.view.update(cx, |v, _| v.release());
        self.thread_sub = None;
        if let Some(id) = self.thread {
            self.hub.update(cx, |h, _| h.unregister_tab(id));
            let store = self.hub.read(cx).store().clone();
            // Stopping the agent can take a moment (graceful shutdown): not on the UI thread.
            cx.background_spawn(async move { store.close_thread(id) })
                .detach();
        }
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        window.focus(&self.view.read(cx).focus_handle(cx));
    }

    fn theme_changed(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.set_theme(theme, cx));
    }
}
