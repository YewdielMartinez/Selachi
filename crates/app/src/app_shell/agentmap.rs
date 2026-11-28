//! The subagent map inside the shell (wave 7, PLAN section 21.2): where it opens from, and what the
//! shell does for each `AgentMapEvent` the inspector emits.
//!
//! The map runs nothing: every action arrives here as an event. Anything that reads the run
//! registry, calls git or starts a process happens on the background executor, never on the UI
//! thread. Nothing here polls: the map's model listens to `agent-runs` and the chat store only
//! while a map view is on screen, and the shell only feeds it tab titles while that is true.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agent_harness::{Confirmation, Harness, Registry};
use chat_core::ThreadId;
use gpui::{Context, Hsla, IntoElement, SharedString, Window, div, prelude::*, px};
use panel_api::{PanelId, TabId};
use theme::Radius;
use ui_agentmap::{
    AgentMapEvent, AgentMapModel, ExternalProc, MapPlacement, NodeKey, PANEL_ID, PrefsChanged,
};
use ui_kit::{Button, Card};
use ui_review::ReviewSource;

use super::AppShell;
use super::delegation::data_dir;
use crate::splits::pane_count;
use crate::agentmap_feed;
use crate::agentmap_host::{self, ApplyView};
use crate::hint::Hint;
use crate::services::Services;

/// `meta` key of the map's preferences (view, grouping, folds, where it lives).
const MAP_PREFS_KEY: &str = "agentmap.prefs";

/// Most changed files listed in the apply dialog.
const MAX_APPLY_LINES: usize = 12;

/// "Aplicar cambios" waiting for the user's explicit confirmation.
pub(super) struct ApplyPrompt {
    pub view: ApplyView,
    pub busy: bool,
}

/// "agy (PID 4120)" for hints.
fn agentmap_feed_label(ext: &ExternalProc) -> String {
    format!("{} (PID {})", ext.agent, ext.pid)
}

fn panel_id() -> PanelId {
    PanelId::new(PANEL_ID)
}

/// `NodeKey` of a terminal tab that delegated (the id the map shows is the tab's number).
pub(crate) fn tab_key(tab: TabId) -> NodeKey {
    NodeKey::Tab(tab.as_u64().to_string())
}

/// `tab id -> title` for the map's terminal parents.
pub(crate) fn titles_of(infos: &[panel_api::TabInfo]) -> HashMap<String, String> {
    infos
        .iter()
        .map(|i| (i.id.as_u64().to_string(), i.title.to_string()))
        .collect()
}

impl AppShell {
    fn agentmap_model(&self, cx: &gpui::App) -> Option<gpui::Entity<AgentMapModel>> {
        cx.try_global::<Services>().map(|s| s.agentmap.clone())
    }

    /// Subscribes to the map's events (once, at startup).
    pub(super) fn init_agentmap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.agentmap_model(cx) else {
            return;
        };
        // what the user chose last time (placement, grouping, folded groups...)
        if let Some(json) = self
            .session
            .read(cx)
            .db()
            .and_then(|db| db.meta_get(MAP_PREFS_KEY).ok().flatten())
        {
            model.update(cx, |m, cx| m.set_prefs_json(&json, cx));
        }
        self._subs.push(cx.subscribe_in(
            &model,
            window,
            |this, _, ev: &AgentMapEvent, window, cx| {
                this.on_agentmap_event(ev.clone(), window, cx)
            },
        ));
        self._subs
            .push(cx.subscribe(&model, |this, _, ev: &PrefsChanged, cx| {
                if let Some(db) = this.session.read(cx).db().cloned() {
                    let json = ev.0.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let _ = db.meta_set(MAP_PREFS_KEY, &json);
                        })
                        .detach();
                }
            }));
        // A map view attaching / detaching notifies the model: start or stop the sampling of agent
        // CLIs launched outside the harness (idempotent, so the notifications of the feed itself
        // change nothing).
        self._subs
            .push(cx.observe(&model, |this, _, cx| this.sync_agentmap_feed(cx)));
    }

    // ------------------------------------------------------------------------------------
    // Agents launched outside the harness (ola 8A -> 8C)
    // ------------------------------------------------------------------------------------

    /// Runs the sampling loop exactly while a map view is on screen. Hidden: no task, no timer, no
    /// snapshot, no CPU baseline.
    pub(super) fn sync_agentmap_feed(&mut self, cx: &mut Context<Self>) {
        let visible = self
            .agentmap_model(cx)
            .is_some_and(|m| m.read(cx).viewers() > 0);
        match (visible, self.agentmap_feed.is_running()) {
            (true, false) => self.start_agentmap_feed(cx),
            (false, true) => self.agentmap_feed.stop(),
            _ => {}
        }
    }

    fn start_agentmap_feed(&mut self, cx: &mut Context<Self>) {
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        let tracker = services.tracker;
        let cpu = self.agentmap_feed.cpu.clone();
        self.agentmap_feed.task = Some(cx.spawn(async move |this, cx| {
            loop {
                // `[delegation] extra_agents` is read every round: a settings change applies at once
                let Ok(extra) = this.update(cx, |s, _| s.config.delegation.extra_agents.clone())
                else {
                    break;
                };
                let (t, c) = (tracker.clone(), cpu.clone());
                let sample = cx
                    .background_executor()
                    .spawn(async move { agentmap_feed::sample(&t, &c, &extra) })
                    .await;
                if this
                    .update(cx, |shell, cx| shell.apply_sample(sample, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(agentmap_feed::SAMPLE_INTERVAL)
                    .await;
            }
        }));
    }

    /// One sample arrived: the agents of every parent, the processes under each of them and (for the
    /// ones whose usage is ingested) their tokens and estimated cost.
    pub(super) fn apply_sample(&mut self, sample: agentmap_feed::Sample, cx: &mut Context<Self>) {
        let agentmap_feed::Sample { rows, children } = sample;
        self.apply_external_rows(rows, cx);
        if let Some(model) = self.agentmap_model(cx) {
            model.update(cx, |m, cx| {
                for (key, kids) in children {
                    m.set_external_children(key, kids, cx);
                }
            });
        }
        self.sync_external_usage(cx);
    }

    /// One sample arrived: hand every parent its external agents (an empty list marks the ones that
    /// ended) and drop the parents whose tab or thread is gone.
    pub(super) fn apply_external_rows(
        &mut self,
        rows: Vec<(NodeKey, Vec<ExternalProc>)>,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.agentmap_model(cx) else {
            return;
        };
        let live: HashSet<NodeKey> = rows.iter().map(|(k, _)| k.clone()).collect();
        let gone: Vec<NodeKey> = self
            .agentmap_feed
            .fed
            .iter()
            .filter(|k| !live.contains(*k))
            .cloned()
            .collect();
        model.update(cx, |m, cx| {
            for k in &gone {
                m.clear_external(k, cx);
            }
            for (key, procs) in rows {
                m.set_external(key, procs, cx);
            }
        });
        for k in &gone {
            self.agentmap_feed.fed.remove(k);
        }
        self.agentmap_feed.fed.extend(live);
        self.sync_external_seen(&gone, cx);
    }

    /// The chat rows and the sidebar follow what the feed saw of each parent (see
    /// [`agentmap_feed::ExternalSeen`]): `gone` are the parents that left the tracker.
    fn sync_external_seen(&mut self, gone: &[NodeKey], cx: &mut Context<Self>) {
        let Some(model) = self.agentmap_model(cx) else {
            return;
        };
        let mut changed = false;
        for k in gone {
            changed |= self.agentmap_feed.seen.remove(k).is_some();
        }
        let fed: Vec<NodeKey> = self.agentmap_feed.fed.iter().cloned().collect();
        for k in fed {
            let Some(seen) = agentmap_feed::seen_of(model.read(cx).external_procs(&k)) else {
                continue;
            };
            if self.agentmap_feed.seen.get(&k) != Some(&seen) {
                self.agentmap_feed.seen.insert(k, seen);
                changed = true;
            }
        }
        if changed {
            self.push_external_notes(cx);
        }
    }

    /// Hands the chat hub the notes of the chat threads and repaints the sidebar (its map icon).
    fn push_external_notes(&mut self, cx: &mut Context<Self>) {
        let notes: HashMap<ThreadId, ui_chat::ExternalNote> = self
            .agentmap_feed
            .seen
            .iter()
            .filter_map(|(k, s)| match k {
                NodeKey::Thread(id) => Some((
                    *id,
                    ui_chat::ExternalNote {
                        agents: s.agents.clone(),
                        count: s.count,
                    },
                )),
                _ => None,
            })
            .collect();
        if let Some(hub) = self.chat.read(cx).hub() {
            hub.update(cx, |h, cx| h.set_external_notes(notes, cx));
        }
        self.refresh(cx);
    }

    /// The tabs (chat or terminal) whose session launched agents outside Selachi, as far as the map
    /// has seen: the sidebar shows the map icon on their rows even without harness delegations.
    pub(super) fn tabs_with_external_agents(&self, cx: &gpui::App) -> HashSet<TabId> {
        if self.agentmap_feed.seen.is_empty() {
            return HashSet::new();
        }
        let mut out = HashSet::new();
        for slot in &self.tabs {
            let id = slot.handle.id();
            let key = slot
                .handle
                .downcast::<ui_chat::ChatTab>()
                .and_then(|t| t.read(cx).thread_id())
                .map_or_else(|| tab_key(id), NodeKey::Thread);
            if self.agentmap_feed.seen.contains_key(&key) {
                out.insert(id);
            }
        }
        out
    }

    /// The folder a parent (chat thread or terminal tab) works in: where an external agent without
    /// its own `--cwd` ran.
    fn parent_cwd(&self, parent: &NodeKey, cx: &gpui::App) -> Option<PathBuf> {
        match parent {
            NodeKey::Thread(id) => {
                let hub = self.chat.read(cx).hub()?;
                let hub = hub.read(cx);
                let t = hub.thread(*id)?;
                Some(
                    t.worktree_path
                        .clone()
                        .unwrap_or_else(|| t.project_root.clone()),
                )
            }
            NodeKey::Tab(n) => self
                .infos(cx)
                .into_iter()
                .find(|i| i.id.as_u64().to_string() == *n)
                .and_then(|i| i.cwd.or(i.project_root)),
            NodeKey::External { .. } => None,
        }
    }

    /// Looks up the tokens and estimated cost of the external agents that have none yet (or whose
    /// end time changed): one read of the usage database per process, on the background executor,
    /// never per sample. Only runs from the feed, so never while the map is hidden.
    fn sync_external_usage(&mut self, cx: &mut Context<Self>) {
        let (Some(model), Some(db)) = (self.agentmap_model(cx), crate::usage_monitor::db_path())
        else {
            return;
        };
        let now = chat_core::now_ms();
        let parents: Vec<NodeKey> = self.agentmap_feed.fed.iter().cloned().collect();
        let mut jobs: Vec<(NodeKey, String, String, i64, i64)> = Vec::new();
        for parent in parents {
            let procs = model.read(cx).external_procs(&parent).to_vec();
            for p in agentmap_feed::usage_todo(&self.agentmap_feed.usage, &procs) {
                let Some(cwd) = p.cwd.clone().or_else(|| self.parent_cwd(&parent, cx)) else {
                    continue;
                };
                self.agentmap_feed.usage.insert(p.key(), p.ended_ms);
                jobs.push((
                    p.key(),
                    p.agent.clone(),
                    cwd.to_string_lossy().into_owned(),
                    p.started_ms,
                    p.ended_ms.unwrap_or(now),
                ));
            }
        }
        if jobs.is_empty() {
            return;
        }
        let job = cx.background_executor().spawn(async move {
            jobs.into_iter()
                .map(|(key, agent, cwd, start, end)| {
                    (
                        key,
                        agentmap_feed::lookup_usage(&db, &agent, &cwd, start, end),
                    )
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = job.await;
            this.update(cx, |shell, cx| {
                if !shell.agentmap_feed.is_running() {
                    return;
                }
                if let Some(model) = shell.agentmap_model(cx) {
                    model.update(cx, |m, cx| {
                        for (key, usage) in found {
                            if usage.is_some() {
                                m.set_external_usage(key, usage, cx);
                            }
                        }
                    });
                }
            })
            .ok();
        })
        .detach();
    }

    /// The external process the map shows for `pid` (searched among the parents that were fed).
    fn external_proc(&self, pid: u32, cx: &gpui::App) -> Option<ExternalProc> {
        let model = self.agentmap_model(cx)?;
        let model = model.read(cx);
        self.agentmap_feed
            .fed
            .iter()
            .flat_map(|k| model.external_procs(k))
            .find(|e| e.pid == pid)
            .cloned()
    }

    /// "Detener" of an external agent (the user already confirmed inline in the inspector): ends that
    /// process subtree only, with `proc_svc`'s own termination (graceful, then forced, same safety
    /// rules as the Processes panel). A reused pid is never touched: the process must still be the one
    /// the map shows (same start time).
    fn agentmap_stop_external(&mut self, pid: u32, cx: &mut Context<Self>) {
        let Some(ext) = self.external_proc(pid, cx) else {
            self.show_hint(Hint::text("Ese proceso ya no está en el mapa"), cx);
            return;
        };
        let started = ext.started_ms;
        let label = agentmap_feed_label(&ext);
        let job = cx.background_executor().spawn(async move {
            let same = proc_svc::start_time_of(pid)
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .is_some_and(|ms| (ms - started).abs() <= 2_000);
            if !same {
                return Err("el proceso ya terminó o su PID lo usa otro programa".to_string());
            }
            proc_svc::terminate_tree(pid, std::time::Duration::from_secs(3))
                .map(|o| o.is_clean())
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let text = match job.await {
                Ok(true) => format!("{label} detenido"),
                Ok(false) => format!("{label}: algunos procesos no se pudieron detener"),
                Err(e) => format!("No se pudo detener {label}: {e}"),
            };
            this.update(cx, |shell, cx| {
                shell.show_hint(Hint::text(text), cx);
                // reflect it now instead of waiting for the next sample
                shell.resample_agentmap_feed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Takes one sample right away (only while the map is on screen).
    fn resample_agentmap_feed(&mut self, cx: &mut Context<Self>) {
        if !self.agentmap_feed.is_running() {
            return;
        }
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        let (tracker, cpu) = (services.tracker, self.agentmap_feed.cpu.clone());
        let extra = self.config.delegation.extra_agents.clone();
        let job = cx
            .background_executor()
            .spawn(async move { agentmap_feed::sample(&tracker, &cpu, &extra) });
        cx.spawn(async move |this, cx| {
            let sample = job.await;
            this.update(cx, |shell, cx| {
                if shell.agentmap_feed.is_running() {
                    shell.apply_sample(sample, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Abrir en Procesos": the Processes panel (it has no per-pid focus yet, so the hint says which
    /// process to look for).
    fn agentmap_open_in_processes(&mut self, pid: u32, cx: &mut Context<Self>) {
        let id = PanelId::new(ui_procs::PANEL_ID);
        if !self.right_panel_active(&id) {
            self.toggle_right_panel(id, cx);
        }
        self.show_hint(
            Hint::text(format!("Proceso {pid} en el panel de Procesos")),
            cx,
        );
    }

    /// "Copiar comando": the redacted command line of the external agent.
    fn agentmap_copy_command(&mut self, command: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(command));
        self.show_hint(Hint::text("Comando copiado"), cx);
    }

    // ------------------------------------------------------------------------------------
    // Entry points
    // ------------------------------------------------------------------------------------

    /// Docks the map on the right (or opens it as an overlay when the window is narrow) and makes
    /// sure the chat database is opening: the map reads the threads from it.
    ///
    /// Where it opens is the user's last choice (`placement` of the map's preferences): the right
    /// dock, or a tab of the main zone.
    pub(super) fn show_agentmap(&mut self, cx: &mut Context<Self>) {
        self.chat.update(cx, |c, cx| c.ensure(cx));
        self.sync_agentmap_context(cx);
        if self.agentmap_placement(cx) == MapPlacement::Tab {
            self.show_agentmap_tab_soon(cx);
            return;
        }
        if !self.right_panel_active(&panel_id()) {
            self.toggle_right_panel(panel_id(), cx);
        }
    }

    fn agentmap_placement(&self, cx: &gpui::App) -> MapPlacement {
        self.agentmap_model(cx)
            .map_or(MapPlacement::Dock, |m| m.read(cx).prefs().placement)
    }

    /// The tab that hosts the map, if there is one.
    pub(super) fn agentmap_tab_id(&self) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|t| {
                t.handle
                    .downcast::<crate::agentmap_tab::AgentMapTab>()
                    .is_some()
            })
            .map(|t| t.handle.id())
    }

    /// Opening a tab needs the window; most entry points of the map do not have it at hand, so the
    /// tab is opened right after (the same turn of the event loop).
    fn show_agentmap_tab_soon(&mut self, cx: &mut Context<Self>) {
        let handle = self.host.window;
        cx.spawn(async move |this, cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |s, cx| s.open_agentmap_tab(window, cx)).ok();
            });
        })
        .detach();
    }

    /// Brings the map tab forward, creating it the first time.
    pub(super) fn open_agentmap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.agentmap_tab_id() {
            self.select_tab(id, window, cx);
            return;
        }
        let class = self.main_class();
        let ctx = self.main_ctx(class, cx);
        let Some(provider) = cx
            .try_global::<panel_api::PanelRegistry>()
            .and_then(|r| r.get(&panel_id()))
        else {
            return;
        };
        let content = provider.build(ctx, window, cx);
        let Ok(view) = content.clone().downcast::<ui_agentmap::AgentMapView>() else {
            return;
        };
        let entity = cx.new(|_| crate::agentmap_tab::AgentMapTab::new(view, content));
        let handle = panel_api::TabHandle::new(entity);
        let id = handle.id();
        self.push_tab(handle, Vec::new(), cx);
        self.show_new_tab(id, window, cx);
    }

    /// The map only listens while it is on screen: a map tab that is not in a visible pane detaches.
    pub(super) fn sync_agentmap_tab_visibility(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.agentmap_tab_id() else {
            return;
        };
        let visible = self
            .splits
            .panes
            .panes()
            .iter()
            .take(pane_count(self.eff.layout.splits))
            .any(|p| *p == Some(id));
        if let Some(tab) = self
            .slot(id)
            .and_then(|s| s.handle.downcast::<crate::agentmap_tab::AgentMapTab>())
        {
            tab.update(cx, |t, cx| t.set_visible(visible, cx));
        }
        // and the feed of agents launched outside Selachi follows (it runs only while a view is on screen)
        self.sync_agentmap_feed(cx);
    }

    /// «Abrir en pestaña»: the map leaves the dock for a tab and stays there next time.
    fn agentmap_to_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.agentmap_model(cx) {
            m.update(cx, |m, cx| m.update_prefs(|p| p.placement = MapPlacement::Tab, cx));
        }
        if self.right_panel_active(&panel_id()) {
            self.toggle_right_panel(panel_id(), cx);
        }
        self.open_agentmap_tab(window, cx);
    }

    /// «Acoplar a la derecha»: the tab closes and the map goes back to the dock.
    fn agentmap_to_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.agentmap_model(cx) {
            m.update(cx, |m, cx| m.update_prefs(|p| p.placement = MapPlacement::Dock, cx));
        }
        if let Some(id) = self.agentmap_tab_id() {
            self.close_tab(id, window, cx);
        }
        if !self.right_panel_active(&panel_id()) {
            self.toggle_right_panel(panel_id(), cx);
        }
    }

    /// "Ver en el mapa" of a "Delegó en…" row: the map on the session of that delegated thread,
    /// with its inspector open.
    /// (The row «Lanzó agy fuera de Selachi» of a parent thread sends its own id: that is not a run, so
    /// the map opens on that session.)
    pub(super) fn open_agentmap_for_run(&mut self, child: ThreadId, cx: &mut Context<Self>) {
        let is_run = self
            .chat
            .read(cx)
            .hub()
            .is_some_and(|h| h.read(cx).delegation_run(child).is_some());
        if !is_run
            && self
                .agentmap_feed
                .seen
                .contains_key(&NodeKey::Thread(child))
        {
            self.set_agentmap_root(Some(NodeKey::Thread(child)), cx);
            return;
        }
        if let Some(model) = self.agentmap_model(cx) {
            model.update(cx, |m, cx| m.open_run(child, cx));
        }
        self.show_agentmap(cx);
    }

    /// The sidebar's map icon of a parent tab: the map on that tab's session.
    pub(super) fn open_agentmap_for_tab(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let thread = self
            .slot(tab)
            .and_then(|s| s.handle.downcast::<ui_chat::ChatTab>())
            .and_then(|t| t.read(cx).thread_id());
        let key = thread.map_or_else(|| tab_key(tab), NodeKey::Thread);
        self.set_agentmap_root(Some(key), cx);
    }

    /// The palette's "Mapa de subagentes": the session of the tab in front (the automatic choice
    /// when that tab has no session of its own).
    pub(super) fn open_agentmap_from_palette(&mut self, cx: &mut Context<Self>) {
        let key = self.active_id().map(|id| {
            self.slot(id)
                .and_then(|s| s.handle.downcast::<ui_chat::ChatTab>())
                .and_then(|t| t.read(cx).thread_id())
                .map_or_else(|| tab_key(id), NodeKey::Thread)
        });
        self.set_agentmap_root(key, cx);
    }

    pub(super) fn set_agentmap_root(&mut self, key: Option<NodeKey>, cx: &mut Context<Self>) {
        if let Some(model) = self.agentmap_model(cx) {
            model.update(cx, |m, cx| m.set_root(key, cx));
        }
        self.show_agentmap(cx);
    }

    /// Feeds the map the names of the terminal tabs (so a terminal parent reads "api" and not an
    /// opaque id). Only while a map view is on screen: with the map closed this costs one read.
    pub(super) fn sync_agentmap_context(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.agentmap_model(cx) else {
            return;
        };
        if model.read(cx).viewers() == 0 && !self.right_panel_active(&panel_id()) {
            return;
        }
        let titles = titles_of(&self.infos(cx));
        model.update(cx, |m, cx| m.set_tab_titles(titles, cx));
    }

    /// The chat database just opened: the map can read threads and listen to them.
    pub(super) fn agentmap_chat_ready(&mut self, cx: &mut Context<Self>) {
        let (Some(services), Some(hub)) = (
            cx.try_global::<Services>().cloned(),
            self.chat.read(cx).hub(),
        ) else {
            return;
        };
        let store = hub.read(cx).store().clone();
        services
            .agentmap_source
            .set(ui_agentmap::ChatStoreSource::new(store));
        services.agentmap.update(cx, |m, cx| m.source_ready(cx));
    }

    // ------------------------------------------------------------------------------------
    // Events of the inspector
    // ------------------------------------------------------------------------------------

    fn on_agentmap_event(
        &mut self,
        ev: AgentMapEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            AgentMapEvent::OpenThread(id) => self.open_chat_thread(id, window, cx),
            AgentMapEvent::OpenDiff(run_id) => self.agentmap_open_diff(run_id, window, cx),
            AgentMapEvent::Cancel(run_id) => self.agentmap_cancel(run_id, cx),
            AgentMapEvent::Retry {
                run_id,
                agent,
                model,
            } => self.agentmap_retry(run_id, agent, model, cx),
            AgentMapEvent::OpenWorktree(path) => self.agentmap_open_worktree(path, cx),
            AgentMapEvent::ApplyChanges(run_id) => self.agentmap_plan_apply(run_id, cx),
            AgentMapEvent::DuplicateAsProfile(run_id) => {
                self.agentmap_duplicate(run_id, window, cx)
            }
            AgentMapEvent::StopExternal(pid) => self.agentmap_stop_external(pid, cx),
            AgentMapEvent::OpenInProcesses(pid) => self.agentmap_open_in_processes(pid, cx),
            AgentMapEvent::CopyCommand(command) => self.agentmap_copy_command(command, cx),
            AgentMapEvent::OpenInTab => self.agentmap_to_tab(window, cx),
            AgentMapEvent::DockRight => self.agentmap_to_dock(window, cx),
        }
    }

    /// "Ver diff": the review tab over the run's worktree, against the checkpoint taken before it
    /// started (what `selachi-agent diff` prints).
    fn agentmap_open_diff(&mut self, run_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = data_dir() else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { Registry::new(&dir).load(&run_id) });
        cx.spawn_in(window, async move |this, cx| {
            let res = job.await;
            this.update_in(cx, |shell, window, cx| match res {
                Ok(rec) => match rec.checkpoint_id {
                    Some(id) => shell.open_review_source(
                        ReviewSource::SinceCheckpoint { root: rec.cwd, id },
                        window,
                        cx,
                    ),
                    None => shell.show_hint(
                        Hint::text("Esta ejecución no tiene checkpoint (no corrió en un repositorio git): no hay diff"),
                        cx,
                    ),
                },
                Err(e) => shell.show_hint(Hint::text(format!("No se puede abrir el diff: {e}")), cx),
            })
            .ok();
        })
        .detach();
    }

    /// The user already confirmed in the inspector: write the cancel marker the runner watches.
    pub(super) fn agentmap_cancel(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(dir) = data_dir() else {
            return;
        };
        let id = run_id.clone();
        let job = cx
            .background_executor()
            .spawn(async move { Registry::new(&dir).request_cancel(&id) });
        cx.spawn(async move |this, cx| {
            let text = match job.await {
                Ok(()) => format!("Cancelación solicitada para la ejecución {run_id}"),
                Err(e) => format!("No se pudo cancelar: {e}"),
            };
            this.update(cx, |shell, cx| shell.show_hint(Hint::text(text), cx))
                .ok();
        })
        .detach();
    }

    /// "Reintentar": the same task with the chosen agent / model in a new worktree. It is a new
    /// `selachi-agent` process (the engine of the CLI and the MCP server), so it behaves exactly like
    /// a delegation and shows up in the map by itself.
    fn agentmap_retry(
        &mut self,
        run_id: String,
        agent: String,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (Some(dir), Ok(exe)) = (data_dir(), crate::cli::agent_exe()) else {
            return;
        };
        if !exe.is_file() {
            self.show_hint(
                Hint::text(format!(
                    "No se encontró {} junto a Selachi: reinstala para reintentar",
                    crate::cli::AGENT_EXE
                )),
                cx,
            );
            return;
        }
        let label = agent.clone();
        let job = cx.background_executor().spawn(async move {
            let rec = Registry::new(&dir).load(&run_id)?;
            let plan = agentmap_host::retry_plan(&rec, &agent, model.as_deref());
            agentmap_host::spawn_retry(&exe, &plan).map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let text = match job.await {
                Ok(()) => format!("Reintentando con {label} en un worktree nuevo"),
                Err(e) => format!("No se pudo reintentar: {e}"),
            };
            this.update(cx, |shell, cx| shell.show_hint(Hint::text(text), cx))
                .ok();
        })
        .detach();
    }

    /// "Abrir worktree": the folder in the file manager.
    fn agentmap_open_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !path.is_dir() {
            self.show_hint(
                Hint::text("El worktree ya no existe (se borró o se limpió)"),
                cx,
            );
            return;
        }
        if let Err(e) = std::process::Command::new("explorer.exe")
            .arg(&path)
            .spawn()
        {
            self.show_hint(Hint::text(format!("No se pudo abrir la carpeta: {e}")), cx);
        }
    }

    /// "Duplicar como perfil": Ajustes -> Agentes -> Perfiles with a new profile seeded from the run.
    fn agentmap_duplicate(&mut self, run_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.agentmap_model(cx) else {
            return;
        };
        let Some(seed) = run_id
            .parse::<i64>()
            .ok()
            .and_then(|id| model.read(cx).profile_seed(&NodeKey::Thread(ThreadId(id))))
        else {
            self.show_hint(Hint::text("No se encontró esa ejecución en el mapa"), cx);
            return;
        };
        let profile = agentmap_host::profile_from_seed(&seed, &run_id);
        self.open_settings(window, cx);
        if let Some(settings) = self.settings_view() {
            settings.update(cx, |s, cx| s.new_profile_from(profile, cx));
        }
    }

    // ------------------------------------------------------------------------------------
    // Apply (preview + explicit confirmation, never silent)
    // ------------------------------------------------------------------------------------

    /// Computes what `apply` would do (read-only, off the UI thread) and asks.
    fn agentmap_plan_apply(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(dir) = data_dir() else {
            return;
        };
        let job = cx.background_executor().spawn(async move {
            let h = Harness::builder().data_dir(dir).build()?;
            h.apply_plan(&run_id)
        });
        cx.spawn(async move |this, cx| {
            let res = job.await;
            this.update(cx, |shell, cx| match res {
                Ok(plan) => {
                    shell.prompts.apply = Some(ApplyPrompt {
                        view: agentmap_host::apply_view(&plan),
                        busy: false,
                    });
                    shell.refresh(cx);
                }
                Err(e) => shell.show_hint(
                    Hint::text(format!("No se pueden aplicar los cambios: {e}")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// The user pressed "Aplicar cambios" in the dialog (the only way changes are applied from the UI).
    pub(super) fn confirm_apply(&mut self, cx: &mut Context<Self>) {
        let Some(p) = &mut self.prompts.apply else {
            return;
        };
        if p.busy || !p.view.can_apply() {
            return;
        }
        p.busy = true;
        let run_id = p.view.run_id.clone();
        let Some(dir) = data_dir() else {
            return;
        };
        self.refresh(cx);
        let job = cx.background_executor().spawn(async move {
            Harness::builder()
                .data_dir(dir)
                .build()?
                .apply(&run_id, Confirmation::Gui)
        });
        cx.spawn(async move |this, cx| {
            let res = job.await;
            this.update(cx, |shell, cx| {
                shell.prompts.apply = None;
                let text = match res {
                    Ok(rep) => format!(
                        "Cambios aplicados: {} archivo{} en {}",
                        rep.files.len(),
                        if rep.files.len() == 1 { "" } else { "s" },
                        rep.target
                    ),
                    Err(e) => format!("No se pudieron aplicar los cambios: {e}"),
                };
                shell.show_hint(Hint::text(text), cx);
                shell.after_git_operation(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn apply_overlay(
        &self,
        p: &ApplyPrompt,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (cancel, go) = (cx.entity(), cx.entity());
        let v = &p.view;
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .max_h(px(200.0))
            .overflow_hidden();
        for (change, path) in v.files.iter().take(MAX_APPLY_LINES) {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.0))
                    .text_size(px(ty.ui_size_sm))
                    .child(
                        div()
                            .flex_none()
                            .w(px(92.0))
                            .text_color(c.muted)
                            .child(SharedString::from(*change)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(self.theme.typography.mono_family)
                            .text_color(c.text)
                            .child(SharedString::from(path.clone())),
                    ),
            );
        }
        let more = v.files.len().saturating_sub(MAX_APPLY_LINES);
        let notes = v.warnings.iter().cloned().chain(v.blocked.clone());
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
                    .w(px(520.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_lg))
                            .text_color(c.text)
                            .child("¿Aplicar los cambios al proyecto?"),
                    )
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.muted)
                            .child(SharedString::from(format!(
                                "Se copian al árbol de trabajo de {} sin confirmarlos ni añadirlos al índice. Nada cambia hasta que pulses el botón.",
                                v.target
                            ))),
                    )
                    .child(list)
                    .children((more > 0).then(|| {
                        div()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.outline)
                            .child(SharedString::from(format!("… y {more} más")))
                    }))
                    .children((!v.stat.is_empty()).then(|| {
                        div()
                            .max_h(px(90.0))
                            .overflow_hidden()
                            .font_family(self.theme.typography.mono_family)
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child(SharedString::from(v.stat.clone()))
                    }))
                    .children(notes.map(|n| {
                        div()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.text)
                            .child(SharedString::from(n))
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                Button::ghost("apply-cancel", &self.theme, "Cancelar").on_click(
                                    move |_, _, cx| {
                                        cancel.update(cx, |s, cx| {
                                            s.prompts.apply = None;
                                            s.refresh(cx);
                                        });
                                    },
                                ),
                            )
                            .child(
                                Button::primary(
                                    "apply-go",
                                    &self.theme,
                                    if p.busy { "Aplicando…" } else { "Aplicar cambios" },
                                )
                                .disabled(!v.can_apply() || p.busy)
                                .on_click(move |_, _, cx| go.update(cx, |s, cx| s.confirm_apply(cx))),
                            ),
                    ),
            )
    }
}
