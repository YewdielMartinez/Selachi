//! The chat mode inside the shell (wave 5.8): the hub, its tabs and its events, the attention
//! queue fed by threads, the "Nueva pestaña" menu and the checkpoint restore prompt.
//!
//! Everything here is on demand. The hub (and so `chat.db`) is created the first time chat is used
//! (a new chat, a restored chat tab, the threads panel); until then a terminal-only session pays
//! nothing. Thread updates arrive through the hub's event stream, which is a waker: no timers.

use std::path::PathBuf;
use std::time::Instant;

use chat_core::{ThreadId, ThreadUpdate};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, Context, Entity, Hsla, IntoElement, KeyDownEvent, SharedString, Window, div, prelude::*, px};
use panel_api::{TabHandle, TabId};
use theme::Radius;
use ui_chat::{ChatEvent, ChatHub, ChatTab};
use ui_kit::{Button, Card, CardTone, Icon, ListRow};
use ui_review::{AttentionItem, AttentionKind, FocusTarget};

use super::{AppShell, RestoreConfirm, TabSlot};
use crate::chat_host::{self, HookMsg};
use crate::hint::Hint;
use crate::notify;
use crate::services::Services;
use crate::titlebar::TITLEBAR_H;

/// Most warnings listed in the restore prompt.
const MAX_RESTORE_LINES: usize = 8;

impl AppShell {
    // ------------------------------------------------------------------------------------
    // The hub
    // ------------------------------------------------------------------------------------

    /// The chat hub, created on first use: opens `chat.db`, bridges thread processes to the shared
    /// tracker, probes the installed agents (off-thread) and wires the events. `None` when the
    /// database cannot be opened (the user sees a hint once; chat stays unavailable).
    pub(super) fn ensure_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<ChatHub>> {
        if let Some(h) = &self.chat {
            return Some(h.clone());
        }
        if self.chat_failed {
            return None;
        }
        let services = cx.try_global::<Services>().cloned()?;
        let (tx, mut rx) = unbounded();
        let hooks = std::sync::Arc::new(chat_host::ProcHooks::new(services.tracker.clone(), tx));
        let store = match chat_host::open_store(hooks) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("chat is unavailable: {e}");
                self.chat_failed = true;
                self.show_hint(Hint::text(format!("El chat no está disponible: {e}")), cx);
                return None;
            },
        };
        let hub = ChatHub::create(store, self.theme, cx);
        *services.chat.borrow_mut() = Some(hub.clone());
        hub.update(cx, |h, cx| h.detect_agents(cx));
        self._chat_subs.push(cx.subscribe_in(&hub, window, |this, _, ev: &ChatEvent, window, cx| {
            this.on_chat_event(ev.clone(), window, cx);
        }));
        self._chat_subs.push(cx.subscribe(&hub, |this, _, u: &ThreadUpdate, cx| this.on_thread_update(u, cx)));
        // The hooks run on chat worker threads; what needs the UI thread arrives here.
        let procs = services.procs;
        self._chat_task = Some(cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let done = this.update(cx, |shell, cx| match msg {
                    HookMsg::Events(events) => procs.update(cx, |m, cx| m.watch_job_events(events, cx)),
                    HookMsg::Closed(owner) => {
                        shell.closed_owners.push(owner);
                        shell.prune_closed_owners(cx);
                    },
                });
                if done.is_err() {
                    break;
                }
            }
        }));
        self.chat = Some(hub.clone());
        self.chat_projects.clear();
        self.sync_chat_projects(cx);
        Some(hub)
    }

    /// The projects the new-thread picker offers: the favorites and recents of the sidebar plus the
    /// project of every open tab. Told to the hub only when they changed.
    pub(super) fn sync_chat_projects(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self.chat.clone() else { return };
        let mut projects: Vec<PathBuf> = Vec::new();
        let open = self.tabs.iter().filter_map(|t| {
            let i = t.handle.info(cx);
            i.project_root.or(i.cwd)
        });
        for p in open.chain(self.known_projects.iter().map(|(p, _)| p.clone())) {
            if !projects.iter().any(|q| crate::paths::is_within(q, &p) && crate::paths::is_within(&p, q)) {
                projects.push(p);
            }
        }
        let default = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd));
        hub.update(cx, |h, _| h.set_default_project(default));
        if projects != self.chat_projects {
            self.chat_projects.clone_from(&projects);
            hub.update(cx, |h, cx| h.set_projects(projects, cx));
        }
    }

    // ------------------------------------------------------------------------------------
    // Chat tabs
    // ------------------------------------------------------------------------------------

    /// Shows a tab model's handle in the main zone (first empty pane, else the focused one).
    fn add_handle_tab(&mut self, handle: TabHandle, window: &mut Window, cx: &mut Context<Self>) -> TabId {
        let id = handle.id();
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        self.tabs.push(TabSlot { handle, _subs: vec![sub] });
        self.panes.open(id);
        self.scroll_active = true;
        self.after_active_changed(window, cx);
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
        self.active_id().and_then(|id| self.slot(id)).and_then(|s| s.handle.downcast::<ChatTab>())
    }

    /// A thread row was clicked: its tab, opened or brought forward.
    pub(super) fn open_chat_thread(&mut self, thread: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((id, _)) = self.chat_tab_of(thread, cx) {
            self.select_tab(id, window, cx);
            return;
        }
        let Some(hub) = self.ensure_chat(window, cx) else { return };
        let handle = hub.update(cx, |h, cx| h.open_thread_tab(thread, cx));
        self.add_handle_tab(handle, window, cx);
    }

    /// A restored chat tab. It loads the thread in the background and starts no agent.
    pub(super) fn restore_chat_tab(&mut self, thread: ThreadId, window: &mut Window, cx: &mut Context<Self>) -> Option<TabId> {
        let hub = self.ensure_chat(window, cx)?;
        let handle = hub.update(cx, |h, cx| h.open_thread_tab(thread, cx));
        let id = handle.id();
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        self.tabs.push(TabSlot { handle, _subs: vec![sub] });
        Some(id)
    }

    /// "Nuevo chat": a tab with the "¿Qué construimos?" screen (agent picker inside).
    pub(super) fn new_chat_tab(&mut self, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hub) = self.ensure_chat(window, cx) else { return };
        let handle = hub.update(cx, |h, cx| h.new_thread_tab(project, cx));
        self.add_handle_tab(handle, window, cx);
    }

    fn on_chat_event(&mut self, event: ChatEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ChatEvent::OpenThread(id) => self.open_chat_thread(id, window, cx),
            ChatEvent::NewThread { project } => self.new_chat_tab(project, window, cx),
            ChatEvent::OpenInTerminal { command, cwd, .. } => self.open_terminal_running(&command, cwd, window, cx),
            ChatEvent::OpenDiff { path, cwd, .. } => {
                let p = PathBuf::from(&path);
                let abs = if p.is_absolute() { p } else { cwd.join(p) };
                self.open_viewer(abs, None, true, None, window, cx);
            },
            ChatEvent::RestoreCheckpoint { thread, id } => self.request_restore(thread, id, cx),
            ChatEvent::ThreadRemoved(id) => {
                if let Some((tab, _)) = self.chat_tab_of(id, cx) {
                    self.close_tab(tab, window, cx);
                }
                self.chat_attn.remove(&id);
                self.remove_chat_attention(id, cx);
            },
        }
    }

    /// The chat-terminal bridge ("Abrir en terminal"): a new terminal in `cwd` that runs the
    /// command, so its real output is on screen. A multi-line command is typed without Enter: the
    /// user decides.
    fn open_terminal_running(&mut self, command: &str, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
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
        if let Some(hub) = self.chat.clone() {
            hub.update(cx, |h, cx| h.set_active_thread(thread, cx));
        }
        let session = tab.and_then(|t| t.read(cx).view().read(cx).runtime().and_then(|rt| rt.record().agent_session_id));
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services.usage.update(cx, |m, cx| m.set_session_id(session, cx));
        }
    }

    fn on_thread_update(&mut self, u: &ThreadUpdate, cx: &mut Context<Self>) {
        let active = self.active_chat_tab(cx).and_then(|t| t.read(cx).thread_id());
        if active == Some(u.thread_id) && (u.changed.status || u.changed.list) {
            self.sync_chat_active(cx);
        }
        if u.changed.status || u.changed.approvals {
            self.feed_chat_attention(u.thread_id, cx);
        }
    }

    fn remove_chat_attention(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services.attention.update(cx, |m, cx| m.remove(&format!("chat-{}", thread.0), cx));
        }
    }

    /// Puts the thread in the attention queue while it needs the user (approval, waiting, failed)
    /// and takes it out when it does not. A Windows toast fires once per change when the window
    /// is not focused. Nothing is queued while the user is looking at that very tab.
    fn feed_chat_attention(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        let Some((tab_id, tab)) = self.chat_tab_of(thread, cx) else { return };
        let state = tab.read(cx).view().read(cx).runtime().map(|rt| {
            let approval = rt.with_state(|s| s.pending_approvals().first().map(|a| (a.tool.clone(), a.detail.clone())));
            (rt.record(), approval)
        });
        let Some((record, approval)) = state else { return };
        let Some(kind) = chat_host::attention_for(record.status, approval) else {
            self.chat_attn.remove(&thread);
            self.remove_chat_attention(thread, cx);
            return;
        };
        let looking = self.active_id() == Some(tab_id) && notify::window_active();
        if looking {
            self.chat_attn.remove(&thread);
            self.remove_chat_attention(thread, cx);
            return;
        }
        let changed = self.chat_attn.get(&thread) != Some(&kind);
        self.chat_attn.insert(thread, kind.clone());
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
                AttentionKind::Approval { tool, detail } => term_core::redact(&format!("{tool} {detail}")).0,
                _ => record.display_title().to_string(),
            };
            notify::attention_toast(tab_id.as_u64(), &chat_host::toast_title(&kind, &agent), record.display_title(), &detail);
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
                let Some(hub) = self.ensure_chat(window, cx) else { return };
                let store = hub.read(cx).store().clone();
                let root = std::env::current_dir().unwrap_or_default();
                let job = cx.background_executor().spawn(async move {
                    let rec = store.create_thread(chat_core::ThreadSpec::new(root, agents_driver::AgentKind::ClaudeCode)).ok()?;
                    let rt = store.open(rec.id).ok()?;
                    rt.send("Explica cómo se cierran los comentarios en el lexer", Vec::new()).ok()?;
                    Some(rec.id)
                });
                cx.spawn_in(window, async move |this, cx| {
                    if let Some(id) = job.await {
                        this.update_in(cx, |shell, window, cx| shell.open_chat_thread(id, window, cx)).ok();
                    }
                })
                .detach();
            },
            _ => {},
        }
    }

    // ------------------------------------------------------------------------------------
    // Checkpoint restore (RestoreCheckpoint)
    // ------------------------------------------------------------------------------------

    /// Asks `git_svc` what restoring checkpoint `id` would do (off the UI thread) and shows it for
    /// confirmation. Nothing is touched before the user agrees.
    fn request_restore(&mut self, thread: ThreadId, id: String, cx: &mut Context<Self>) {
        let Some(record) = self.chat.as_ref().and_then(|h| h.read(cx).thread(thread).cloned()) else {
            self.show_hint(Hint::text("No se encontró el hilo de ese checkpoint"), cx);
            return;
        };
        let cwd = record.cwd().to_path_buf();
        cx.spawn(async move |this, cx| {
            let (dir, cp) = (cwd.clone(), id.clone());
            let res = cx
                .background_executor()
                .spawn(async move {
                    let repo = git_svc::Repo::discover(&dir).map_err(|e| e.to_string())?.ok_or("la carpeta del hilo no es un repositorio git")?;
                    repo.preflight_restore(&cp, &git_svc::RestoreMode::All).map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |shell, cx| match res {
                Ok(pf) if !pf.requires_confirmation => {
                    shell.show_hint(Hint::text("El proyecto ya está en el estado de ese checkpoint"), cx);
                },
                Ok(pf) => {
                    shell.restore_confirm =
                        Some(RestoreConfirm { id, cwd, warnings: pf.warnings, stat: pf.diff_stat, busy: false });
                    cx.notify();
                },
                Err(e) => shell.show_hint(Hint::text(format!("No se puede restaurar: {e}")), cx),
            })
            .ok();
        })
        .detach();
    }

    /// The user confirmed: restores the whole tree (git_svc first takes an "antes de restaurar"
    /// checkpoint, so this can be undone with another restore).
    pub(super) fn confirm_restore(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &mut self.restore_confirm else { return };
        if r.busy {
            return;
        }
        r.busy = true;
        let (dir, id) = (r.cwd.clone(), r.id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let repo = git_svc::Repo::discover(&dir).map_err(|e| e.to_string())?.ok_or("no es un repositorio git")?;
                    repo.restore_checkpoint(&id, git_svc::RestoreMode::All).map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |shell, cx| {
                shell.restore_confirm = None;
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

    pub(super) fn restore_overlay(&self, r: &RestoreConfirm, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (cancel, go) = (cx.entity(), cx.entity());
        let mut list = div().flex().flex_col().gap(px(2.0)).max_h(px(180.0)).overflow_hidden();
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
                                    s.restore_confirm = None;
                                    cx.notify();
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
        self.new_tab_menu = true;
        self.menu_open = false;
        self.tabs_menu_open = false;
        window.focus(&self.menu_focus);
        cx.notify();
    }

    pub(super) fn close_new_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.new_tab_menu {
            return;
        }
        self.new_tab_menu = false;
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
        cx.notify();
    }

    fn pick_new_tab(&mut self, chat: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.new_tab_menu = false;
        if chat {
            let project = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd));
            self.new_chat_tab(project, window, cx);
        } else {
            self.new_tab_here(window, cx);
        }
        cx.notify();
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
            ListRow::new(id, &theme).icon(icon).title(title).subtitle(hint)
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
                    .track_focus(&self.menu_focus)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(move |e: &KeyDownEvent, window, cx| {
                        let key = e.keystroke.key.as_str();
                        keys.update(cx, |s, cx| match key {
                            "escape" => s.close_new_tab_menu(window, cx),
                            "enter" | "t" => s.pick_new_tab(false, window, cx),
                            "c" => s.pick_new_tab(true, window, cx),
                            _ => {},
                        });
                    })
                    .child(
                        Card::new(&self.theme).tone(CardTone::High, &self.theme).radius(Radius::MD).p(px(4.0)).flex().flex_col().child(
                            row("nt-terminal", Icon::Terminal, "Terminal", "T · Ctrl+Shift+N")
                                .on_click(move |_, window, cx| a.update(cx, |s, cx| s.pick_new_tab(false, window, cx))),
                        )
                        .child(
                            row("nt-chat", Icon::Agents, "Chat", "C · Elige el agente dentro")
                                .on_click(move |_, window, cx| b.update(cx, |s, cx| s.pick_new_tab(true, window, cx))),
                        ),
                    ),
            )
            .into_any_element()
    }
}
