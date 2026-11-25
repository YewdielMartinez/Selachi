//! The chat mode inside the shell (wave 5.8): its tabs and events, the attention queue fed by
//! threads, the "Nueva pestaña" menu and the checkpoint restore prompt.
//!
//! The hub's lifecycle belongs to [`ChatHost`](crate::chat_host::ChatHost): `chat.db` is opened
//! off the UI thread the first time chat is used (a new chat, a restored chat tab, the threads
//! panel); until then a terminal-only session pays nothing. What needs the hub before it is ready
//! is queued there as a [`ChatIntent`] and runs when the host reports `Ready`. Thread updates
//! arrive through the hub's event stream, which is a waker: no timers.

use std::path::PathBuf;
use std::time::Instant;

use chat_core::{ThreadId, ThreadUpdate};
use gpui::{
    AnyElement, Context, Entity, Hsla, IntoElement, KeyDownEvent, SharedString, Window, div,
    prelude::*, px,
};
use panel_api::TabId;
use theme::Radius;
use ui_chat::copy::CopySettings;
use ui_chat::{ChatEvent, ChatHub, ChatTab};
use ui_kit::{Button, Card, CardTone, Icon, ListRow};
use ui_review::{AttentionItem, AttentionKind, FocusTarget};

use super::{AppShell, ChatHostEvent, RestoreConfirm};
use crate::chat_host::{self, ChatIntent};
use crate::hint::Hint;
use crate::notify;
use crate::services::Services;
use crate::titlebar::TITLEBAR_H;

/// `[chat]` as the hub wants it.
pub(crate) fn copy_settings_of(chat: &store::ChatConfig) -> CopySettings {
    CopySettings {
        redact_commands: chat.redact_copy_commands,
        redact_text: chat.redact_copy_text,
    }
}

/// Most warnings listed in the restore prompt.
const MAX_RESTORE_LINES: usize = 8;

impl AppShell {
    // ------------------------------------------------------------------------------------
    // The hub (owned by `ChatHost`)
    // ------------------------------------------------------------------------------------

    /// What the chat host reports: the hub is ready (or could not open), one of its events, a
    /// thread update, a closed process owner.
    pub(super) fn on_chat_host_event(
        &mut self,
        event: &ChatHostEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChatHostEvent::Ready => {
                // The threads panel was built with a stand-in while the hub loaded.
                if self
                    .right
                    .as_ref()
                    .is_some_and(|r| r.id.as_str() == ui_chat::THREADS_PANEL_ID)
                {
                    self.right = None;
                }
                self.sync_chat_projects(cx);
                self.sync_chat_active(cx);
                if let Some(hub) = self.chat.read(cx).hub() {
                    // The hub notifies when the list changes (a delegated child appeared): repaint
                    // the sidebar rows that nest them.
                    self._subs
                        .push(cx.observe(&hub, |this, _, cx| this.refresh(cx)));
                }
                self.reload_runs(cx);
                self.agentmap_chat_ready(cx);
                self.apply_copy_settings(cx);
                self.finish_deferred_startup(window, cx);
                for intent in self.chat.update(cx, |c, _| c.take_pending()) {
                    self.run_chat_intent(intent, window, cx);
                }
                self.refresh(cx);
            }
            ChatHostEvent::Failed(e) => {
                self.show_hint(Hint::text(format!("El chat no está disponible: {e}")), cx);
                self.finish_deferred_startup(window, cx);
            }
            ChatHostEvent::Chat(ev) => self.on_chat_event(ev.clone(), window, cx),
            ChatHostEvent::Thread(u) => self.on_thread_update(u, cx),
            ChatHostEvent::OwnerClosed(owner) => {
                self.closed_owners.push(*owner);
                self.prune_closed_owners(cx);
            }
        }
    }

    /// A session that waited for the hub (its chat tabs need it) is restored now; so is the folder
    /// the launch asked for.
    fn finish_deferred_startup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((state, then_open)) = self.session.update(cx, |s, _| s.take_pending_restore())
        else {
            return;
        };
        self.restore_session(state, window, cx);
        if let Some(dir) = then_open {
            self.open_tab(Some(dir), window, cx);
        }
    }

    /// Runs `intent` now when the hub is ready, else queues it (and starts the hub loading).
    pub(super) fn run_chat_intent(
        &mut self,
        intent: ChatIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hub) = self.chat.read(cx).hub() else {
            // `false`: chat is unavailable (the user was told once); nothing to do.
            self.chat.update(cx, |c, cx| c.queue(intent, cx));
            return;
        };
        match intent {
            ChatIntent::OpenThread(thread) => {
                let handle = hub.update(cx, |h, cx| h.open_thread_tab(thread, cx));
                self.add_handle_tab(handle, window, cx);
            }
            ChatIntent::NewThread(project) => {
                let handle = hub.update(cx, |h, cx| h.new_thread_tab(project, cx));
                self.add_handle_tab(handle, window, cx);
            }
            ChatIntent::Demo => self.run_chat_demo(hub, window, cx),
        }
    }

    /// The projects the new-thread picker offers: the favorites and recents of the sidebar plus the
    /// project of every open tab. Told to the hub only when they changed.
    pub(super) fn sync_chat_projects(&mut self, cx: &mut Context<Self>) {
        if self.chat.read(cx).hub().is_none() {
            return;
        }
        let mut projects: Vec<PathBuf> = Vec::new();
        let open = self.tabs.iter().filter_map(|t| {
            let i = t.handle.info(cx);
            i.project_root.or(i.cwd)
        });
        for p in open.chain(self.spaces.known.iter().map(|(p, _)| p.clone())) {
            if !projects
                .iter()
                .any(|q| crate::paths::is_within(q, &p) && crate::paths::is_within(&p, q))
            {
                projects.push(p);
            }
        }
        let default = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd));
        self.chat
            .update(cx, |c, cx| c.sync_projects(projects, default, cx));
    }

    // ------------------------------------------------------------------------------------
    // "Ocultar secretos al copiar" (`[chat]` in config.toml)
    // ------------------------------------------------------------------------------------

    /// Pushes `[chat]` to the hub: at startup, when the hub opens and on every hot reload.
    pub(super) fn apply_copy_settings(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self.chat.read(cx).hub() else {
            return;
        };
        let settings = copy_settings_of(&self.config.chat);
        hub.update(cx, |h, cx| h.set_copy_settings(settings, cx));
    }

    /// The user flipped a toggle in a chat menu: keep it in the config and write it back to
    /// `config.toml` (the writer edits in place, so comments and key order survive).
    pub(super) fn save_copy_settings(&mut self, s: CopySettings, cx: &mut Context<Self>) {
        let chat = store::ChatConfig {
            redact_copy_commands: s.redact_commands,
            redact_copy_text: s.redact_text,
        };
        if self.config.chat == chat {
            return;
        }
        let mut cfg = (*self.config).clone();
        cfg.chat = chat;
        self.config = std::sync::Arc::new(cfg);
        self.schedule_save(cx);
        if let Some(settings) = self.settings_view() {
            let cfg = (*self.config).clone();
            settings.update(cx, |v, cx| v.set_config(cfg, cx));
        }
    }

    // ------------------------------------------------------------------------------------
    // Chat tabs
    // ------------------------------------------------------------------------------------

    /// Shows a tab model's handle in the main zone (first empty pane, else the focused one).
    fn add_handle_tab(
        &mut self,
        handle: panel_api::TabHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabId {
        let id = handle.id();
        self.push_tab(handle, Vec::new(), cx);
        self.show_new_tab(id, window, cx);
        id
    }

    /// The open chat tab of `thread`.
    fn chat_tab_of(&self, thread: ThreadId, cx: &gpui::App) -> Option<(TabId, Entity<ChatTab>)> {
        self.tabs.iter().find_map(|t| {
            let tab = t.handle.downcast::<ChatTab>()?;
            (tab.read(cx).thread_id() == Some(thread)).then(|| (t.handle.id(), tab))
        })
    }

    /// The chat tab in front, if the active tab is one.
    pub(super) fn active_chat_tab(&self, _cx: &gpui::App) -> Option<Entity<ChatTab>> {
        self.active_id()
            .and_then(|id| self.slot(id))
            .and_then(|s| s.handle.downcast::<ChatTab>())
    }

    /// A thread row was clicked: its tab, opened or brought forward.
    pub(super) fn open_chat_thread(
        &mut self,
        thread: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((id, _)) = self.chat_tab_of(thread, cx) {
            self.select_tab(id, window, cx);
            return;
        }
        self.run_chat_intent(ChatIntent::OpenThread(thread), window, cx);
    }

    /// A restored chat tab. It loads the thread in the background and starts no agent.
    pub(super) fn restore_chat_tab(
        &mut self,
        thread: ThreadId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        let hub = self.chat.read(cx).hub()?;
        let handle = hub.update(cx, |h, cx| h.open_thread_tab(thread, cx));
        let id = handle.id();
        self.push_tab(handle, Vec::new(), cx);
        Some(id)
    }

    /// "Nuevo chat": a tab with the "¿Qué construimos?" screen (agent picker inside).
    pub(super) fn new_chat_tab(
        &mut self,
        project: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_chat_intent(ChatIntent::NewThread(project), window, cx);
    }

    fn on_chat_event(&mut self, event: ChatEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ChatEvent::OpenThread(id) => self.open_chat_thread(id, window, cx),
            ChatEvent::NewThread { project } => self.new_chat_tab(project, window, cx),
            ChatEvent::OpenInTerminal { command, cwd, .. } => {
                self.open_terminal_running(&command, cwd, window, cx)
            }
            ChatEvent::OpenDiff { path, cwd, .. } => {
                let p = PathBuf::from(&path);
                let abs = if p.is_absolute() { p } else { cwd.join(p) };
                self.open_viewer(abs, None, true, None, window, cx);
            }
            ChatEvent::RestoreCheckpoint { thread, id } => self.request_restore(thread, id, cx),
            ChatEvent::OpenAgentMap(child) => self.open_agentmap_for_run(child, cx),
            ChatEvent::CopySettingsChanged(s) => self.save_copy_settings(s, cx),
            ChatEvent::ThreadRemoved(id) => {
                if let Some((tab, _)) = self.chat_tab_of(id, cx) {
                    self.close_tab(tab, window, cx);
                }
                self.chat.update(cx, |c, _| c.set_attention(id, None));
                self.remove_chat_attention(id, cx);
            }
        }
    }

    /// The chat-terminal bridge ("Abrir en terminal"): a new terminal in `cwd` that runs the
    /// command, so its real output is on screen. A multi-line command is typed without Enter: the
    /// user decides.
    fn open_terminal_running(
        &mut self,
        command: &str,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_tab(Some(cwd), window, cx);
        self.type_in_active(command, !command.contains('\n'), cx);
    }

    // ------------------------------------------------------------------------------------
    // Thread updates: active thread, usage session, attention
    // ------------------------------------------------------------------------------------

    /// The front tab changed or a chat tab got its thread: the hub highlights it and the usage
    /// panel's "Sesión" range follows its agent session.
    pub(super) fn sync_chat_active(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_chat_tab(cx);
        let thread = tab.as_ref().and_then(|t| t.read(cx).thread_id());
        if let Some(hub) = self.chat.read(cx).hub() {
            hub.update(cx, |h, cx| h.set_active_thread(thread, cx));
        }
        let session = tab.and_then(|t| {
            t.read(cx)
                .view()
                .read(cx)
                .runtime()
                .and_then(|rt| rt.record().agent_session_id)
        });
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services
                .usage
                .update(cx, |m, cx| m.set_session_id(session, cx));
        }
    }

    fn on_thread_update(&mut self, u: &ThreadUpdate, cx: &mut Context<Self>) {
        let active = self
            .active_chat_tab(cx)
            .and_then(|t| t.read(cx).thread_id());
        if active == Some(u.thread_id) && (u.changed.status || u.changed.list) {
            self.sync_chat_active(cx);
        }
        if u.changed.status || u.changed.approvals {
            self.feed_chat_attention(u.thread_id, cx);
        }
    }

    fn remove_chat_attention(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services
                .attention
                .update(cx, |m, cx| m.remove(&format!("chat-{}", thread.0), cx));
        }
    }

    /// Puts the thread in the attention queue while it needs the user (approval, waiting, failed)
    /// and takes it out when it does not. A Windows toast fires once per change when the window
    /// is not focused. Nothing is queued while the user is looking at that very tab.
    fn feed_chat_attention(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        let Some((tab_id, tab)) = self.chat_tab_of(thread, cx) else {
            return;
        };
        let state = tab.read(cx).view().read(cx).runtime().map(|rt| {
            let approval = rt.with_state(|s| {
                s.pending_approvals()
                    .first()
                    .map(|a| (a.tool.clone(), a.detail.clone()))
            });
            (rt.record(), approval)
        });
        let Some((record, approval)) = state else {
            return;
        };
        let Some(kind) = chat_host::attention_for(record.status, approval) else {
            self.chat.update(cx, |c, _| c.set_attention(thread, None));
            self.remove_chat_attention(thread, cx);
            return;
        };
        let looking = self.active_id() == Some(tab_id) && notify::window_active();
        if looking {
            self.chat.update(cx, |c, _| c.set_attention(thread, None));
            self.remove_chat_attention(thread, cx);
            return;
        }
        let changed = self.chat.read(cx).attention_of(thread) != Some(&kind);
        self.chat
            .update(cx, |c, _| c.set_attention(thread, Some(kind.clone())));
        let agent = chat_core::agent_label(&record.agent);
        if let Some(services) = cx.try_global::<Services>().cloned() {
            let item = AttentionItem {
                id: format!("chat-{}", thread.0).into(),
                owner_label: format!("{} · {}", record.display_title(), agent).into(),
                agent: Some(ui_review::agent_label(&record.agent)),
                kind: kind.clone(),
                since: Instant::now(),
                focus_target: FocusTarget::new(tab_id.as_u64().to_string()),
            };
            services.attention.update(cx, |m, cx| m.upsert(item, cx));
        }
        if changed && !notify::window_active() {
            // What the agent wants to run may carry secrets: through the redactor like the rest.
            let detail = match &kind {
                AttentionKind::Approval { tool, detail } => {
                    term_core::redact(&format!("{tool} {detail}")).0
                }
                _ => record.display_title().to_string(),
            };
            notify::attention_toast(
                tab_id.as_u64(),
                &chat_host::toast_title(&kind, &agent),
                record.display_title(),
                &detail,
            );
        }
    }

    // ------------------------------------------------------------------------------------
    // Scripted demo (screenshots without synthetic key presses)
    // ------------------------------------------------------------------------------------

    /// `SELACHI_DEMO=menu` opens the "Nueva pestaña" menu; `SELACHI_DEMO=chat` (needs
    /// `SELACHI_CHAT_FAKE=1`) creates a thread, sends one message to the scripted agent and opens
    /// its tab. Nothing happens without the variable.
    pub(super) fn run_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match std::env::var("SELACHI_DEMO").as_deref() {
            Ok("menu") => self.open_new_tab_menu(window, cx),
            Ok("chat") if chat_host::fake_requested() => {
                self.run_chat_intent(ChatIntent::Demo, window, cx)
            }
            Ok(other) => {
                self.run_delegation_demo(other, window, cx);
            }
            _ => {}
        }
    }

    fn run_chat_demo(&mut self, hub: Entity<ChatHub>, window: &mut Window, cx: &mut Context<Self>) {
        let store = hub.read(cx).store().clone();
        let root = std::env::current_dir().unwrap_or_default();
        let job = cx.background_executor().spawn(async move {
            let rec = store
                .create_thread(chat_core::ThreadSpec::new(
                    root,
                    agents_driver::AgentKind::ClaudeCode,
                ))
                .ok()?;
            let rt = store.open(rec.id).ok()?;
            rt.send(
                "Explica cómo se cierran los comentarios en el lexer",
                Vec::new(),
            )
            .ok()?;
            Some(rec.id)
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Some(id) = job.await {
                this.update_in(cx, |shell, window, cx| {
                    shell.open_chat_thread(id, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    // ------------------------------------------------------------------------------------
    // Checkpoint restore (RestoreCheckpoint)
    // ------------------------------------------------------------------------------------

    /// Asks `git_svc` what restoring checkpoint `id` would do (off the UI thread) and shows it for
    /// confirmation. Nothing is touched before the user agrees.
    fn request_restore(&mut self, thread: ThreadId, id: String, cx: &mut Context<Self>) {
        let Some(record) = self
            .chat
            .read(cx)
            .hub()
            .and_then(|h| h.read(cx).thread(thread).cloned())
        else {
            self.show_hint(Hint::text("No se encontró el hilo de ese checkpoint"), cx);
            return;
        };
        let cwd = record.cwd().to_path_buf();
        cx.spawn(async move |this, cx| {
            let (dir, cp) = (cwd.clone(), id.clone());
            let res = cx
                .background_executor()
                .spawn(async move {
                    let repo = git_svc::Repo::discover(&dir)
                        .map_err(|e| e.to_string())?
                        .ok_or("la carpeta del hilo no es un repositorio git")?;
                    repo.preflight_restore(&cp, &git_svc::RestoreMode::All)
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |shell, cx| match res {
                Ok(pf) if !pf.requires_confirmation => {
                    shell.show_hint(
                        Hint::text("El proyecto ya está en el estado de ese checkpoint"),
                        cx,
                    );
                }
                Ok(pf) => {
                    shell.prompts.restore = Some(RestoreConfirm {
                        id,
                        cwd,
                        warnings: pf.warnings,
                        stat: pf.diff_stat,
                        busy: false,
                    });
                    shell.refresh(cx);
                }
                Err(e) => shell.show_hint(Hint::text(format!("No se puede restaurar: {e}")), cx),
            })
            .ok();
        })
        .detach();
    }

    /// The user confirmed: restores the whole tree (git_svc first takes an "antes de restaurar"
    /// checkpoint, so this can be undone with another restore).
    pub(super) fn confirm_restore(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &mut self.prompts.restore else {
            return;
        };
        if r.busy {
            return;
        }
        r.busy = true;
        let (dir, id) = (r.cwd.clone(), r.id.clone());
        self.refresh(cx);
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let repo = git_svc::Repo::discover(&dir)
                        .map_err(|e| e.to_string())?
                        .ok_or("no es un repositorio git")?;
                    repo.restore_checkpoint(&id, git_svc::RestoreMode::All)
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |shell, cx| {
                shell.prompts.restore = None;
                let text = match res {
                    Ok(rep) => format!(
                        "Checkpoint restaurado: {} archivos recuperados, {} borrados",
                        rep.restored.len(),
                        rep.deleted.len()
                    ),
                    Err(e) => format!("No se pudo restaurar: {e}"),
                };
                shell.show_hint(Hint::text(text), cx);
                shell.after_git_operation(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn restore_overlay(
        &self,
        r: &RestoreConfirm,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (cancel, go) = (cx.entity(), cx.entity());
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .max_h(px(180.0))
            .overflow_hidden();
        for w in r.warnings.iter().take(MAX_RESTORE_LINES) {
            list = list.child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .child(SharedString::from(w.clone())),
            );
        }
        let more = r.warnings.len().saturating_sub(MAX_RESTORE_LINES);
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(Hsla { h: 0.0, s: 0.0, l: 0.0, a: 0.4 })
            .child(
                Card::new(&self.theme)
                    .radius(Radius::MD)
                    .p_4()
                    .w(px(460.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(div().text_size(px(ty.ui_size_lg)).text_color(c.text).child("¿Restaurar el checkpoint?"))
                    .child(div().text_size(px(ty.ui_size_md)).text_color(c.muted).child(
                        "Los archivos del proyecto vuelven a ese punto. Antes se guarda un checkpoint \"antes de restaurar\" para poder deshacerlo.",
                    ))
                    .child(list)
                    .children((more > 0).then(|| {
                        div().text_size(px(ty.ui_size_sm)).text_color(c.outline).child(SharedString::from(format!("… y {more} más")))
                    }))
                    .children(r.stat.clone().map(|s| {
                        div()
                            .max_h(px(90.0))
                            .overflow_hidden()
                            .font_family(self.theme.typography.mono_family)
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child(SharedString::from(s))
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(Button::ghost("restore-cancel", &self.theme, "Cancelar").on_click(move |_, _, cx| {
                                cancel.update(cx, |s, cx| {
                                    s.prompts.restore = None;
                                    s.refresh(cx);
                                });
                            }))
                            .child(
                                Button::primary("restore-go", &self.theme, if r.busy { "Restaurando…" } else { "Restaurar" })
                                    .on_click(move |_, _, cx| go.update(cx, |s, cx| s.confirm_restore(cx))),
                            ),
                    ),
            )
    }

    // ------------------------------------------------------------------------------------
    // "Nueva pestaña" menu (Ctrl+N): Terminal / Chat
    // ------------------------------------------------------------------------------------

    /// Ctrl+N and the sidebar's "Nueva pestaña": a small menu. Ctrl+Shift+N skips it.
    pub(crate) fn open_new_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompts.new_tab_menu = true;
        self.chrome.titlebar.update(cx, |t, cx| t.close_menus(cx));
        window.focus(&self.prompts.menu_focus);
        self.refresh(cx);
    }

    pub(super) fn close_new_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.prompts.new_tab_menu {
            return;
        }
        self.prompts.new_tab_menu = false;
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
        self.refresh(cx);
    }

    fn pick_new_tab(&mut self, chat: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.prompts.new_tab_menu = false;
        if chat {
            let project = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd));
            self.new_chat_tab(project, window, cx);
        } else {
            self.new_tab_here(window, cx);
        }
        self.refresh(cx);
    }

    /// The menu card, placed under the sidebar's "Nueva pestaña" row (or the top-left corner when
    /// the layout has no unified sidebar). A transparent scrim behind it closes it on click.
    pub(super) fn new_tab_menu_el(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let l = &self.eff.layout;
        let left_w = Self::sidebar_zone_w(l, store::SidebarSide::Left);
        let unified = Self::is_unified(l) && left_w > 0.0;
        let x = Self::rail_w(l) + if left_w > 0.0 { 18.0 } else { 12.0 };
        let y = TITLEBAR_H + if unified { 42.0 } else { 6.0 };
        let (scrim, a, b, keys) = (cx.entity(), cx.entity(), cx.entity(), cx.entity());
        let theme = self.theme;
        let row = |id: &'static str, icon: Icon, title: &'static str, hint: &'static str| {
            ListRow::new(id, &theme)
                .icon(icon)
                .title(title)
                .subtitle(hint)
        };
        div()
            .absolute()
            .inset_0()
            .id("new-tab-scrim")
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                scrim.update(cx, |s, cx| s.close_new_tab_menu(window, cx));
            })
            .child(
                div()
                    .id("new-tab-menu")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(250.0))
                    .track_focus(&self.prompts.menu_focus)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(move |e: &KeyDownEvent, window, cx| {
                        let key = e.keystroke.key.as_str();
                        keys.update(cx, |s, cx| match key {
                            "escape" => s.close_new_tab_menu(window, cx),
                            "enter" | "t" => s.pick_new_tab(false, window, cx),
                            "c" => s.pick_new_tab(true, window, cx),
                            _ => {}
                        });
                    })
                    .child(
                        Card::new(&self.theme)
                            .tone(CardTone::High, &self.theme)
                            .radius(Radius::MD)
                            .p(px(4.0))
                            .flex()
                            .flex_col()
                            .child(
                                row(
                                    "nt-terminal",
                                    Icon::Terminal,
                                    "Terminal",
                                    "T · Ctrl+Shift+N",
                                )
                                .on_click(move |_, window, cx| {
                                    a.update(cx, |s, cx| s.pick_new_tab(false, window, cx))
                                }),
                            )
                            .child(
                                row(
                                    "nt-chat",
                                    Icon::Agents,
                                    "Chat",
                                    "C · Elige el agente dentro",
                                )
                                .on_click(move |_, window, cx| {
                                    b.update(cx, |s, cx| s.pick_new_tab(true, window, cx))
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
