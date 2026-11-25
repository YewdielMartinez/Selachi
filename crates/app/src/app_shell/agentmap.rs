//! The subagent map inside the shell (wave 7, PLAN section 21.2): where it opens from, and what the
//! shell does for each `AgentMapEvent` the inspector emits.
//!
//! The map runs nothing: every action arrives here as an event. Anything that reads the run
//! registry, calls git or starts a process happens on the background executor, never on the UI
//! thread. Nothing here polls: the map's model listens to `agent-runs` and the chat store only
//! while a map view is on screen, and the shell only feeds it tab titles while that is true.

use std::collections::HashMap;
use std::path::PathBuf;

use agent_harness::{Confirmation, Harness, Registry};
use chat_core::ThreadId;
use gpui::{Context, Hsla, IntoElement, SharedString, Window, div, prelude::*, px};
use panel_api::{PanelId, TabId};
use theme::Radius;
use ui_agentmap::{AgentMapEvent, AgentMapModel, NodeKey, PANEL_ID};
use ui_kit::{Button, Card};
use ui_review::ReviewSource;

use super::AppShell;
use super::delegation::data_dir;
use crate::agentmap_host::{self, ApplyView};
use crate::hint::Hint;
use crate::services::Services;

/// Most changed files listed in the apply dialog.
const MAX_APPLY_LINES: usize = 12;

/// "Aplicar cambios" waiting for the user's explicit confirmation.
pub(super) struct ApplyPrompt {
    pub view: ApplyView,
    pub busy: bool,
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
        self._subs.push(cx.subscribe_in(
            &model,
            window,
            |this, _, ev: &AgentMapEvent, window, cx| {
                this.on_agentmap_event(ev.clone(), window, cx)
            },
        ));
    }

    // ------------------------------------------------------------------------------------
    // Entry points
    // ------------------------------------------------------------------------------------

    /// Docks the map on the right (or opens it as an overlay when the window is narrow) and makes
    /// sure the chat database is opening: the map reads the threads from it.
    pub(super) fn show_agentmap(&mut self, cx: &mut Context<Self>) {
        self.chat.update(cx, |c, cx| c.ensure(cx));
        self.sync_agentmap_context(cx);
        if !self.right_panel_active(&panel_id()) {
            self.toggle_right_panel(panel_id(), cx);
        }
    }

    /// "Ver en el mapa" of a "Delegó en…" row: the map on the session of that delegated thread,
    /// with its inspector open.
    pub(super) fn open_agentmap_for_run(&mut self, child: ThreadId, cx: &mut Context<Self>) {
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
    fn agentmap_cancel(&mut self, run_id: String, cx: &mut Context<Self>) {
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
