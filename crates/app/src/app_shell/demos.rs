//! Demo flags for screenshots (no synthetic input, no real model, nothing leaves the machine):
//!
//! | `SELACHI_DEMO=` | Shows |
//! |---|---|
//! | `delegation` | A parent chat that delegated to Codex and Gemini: nested rows in the sidebar, the "Delegó en…" rows in the chat and the summed cost (needs `SELACHI_CHAT_FAKE=1`; use a throwaway `SELACHI_DATA_DIR`) |
//! | `approval` | `delegation` plus two pending requests: a tool and an `apply_changes` plan |
//! | `agentmap` | `delegation` plus the subagent map docked on the right, on the Codex run (needs `SELACHI_CHAT_FAKE=1`) |
//! | `copy` | A chat with the scripted agent, a text selection and the message's "..." copy menu open (needs `SELACHI_CHAT_FAKE=1`) |
//! | `delegation-settings` | Settings -> Agentes, scrolled to Delegación |
//! | `update` | Settings -> General at Actualizaciones and the "Actualización 0.2.0 lista" card |
//! | `whatsnew` | The "Novedades" screen with sample release notes |
//!
//! The demo threads are written to the data folder like real delegations (chat.db + `agent-runs`),
//! so what is on screen goes through the same code paths as a real run.

use std::path::PathBuf;

use agent_harness::{
    ApprovalRequest, PROTOCOL_VERSION, Registry, RunRecord, RunResult, RunStatus, UsageOut,
};
use agents_driver::AgentKind;
use chat_core::{ChatStore, ThreadId, ThreadLink, ThreadSpec};
use gpui::{Context, Window};

use super::AppShell;
use super::delegation::data_dir;

const SAMPLE_NOTES: &str = "## Novedades de esta versión\n\n- **Delegación entre agentes**: Claude Code puede encargar tareas a Codex o Gemini a través de Selachi, con tu aprobación.\n- **Actualizaciones**: Selachi busca versiones nuevas a los 30 s de abrir y cada 24 h; tú decides cuándo instalar.\n- **Estado del PR y de CI** en la barra de estado.\n\n## Arreglos\n\n- La barra lateral anida las delegaciones bajo su hilo padre.\n- Menos consumo en reposo.\n";

const APPLY_DETAIL: &str = "apply run 12: C:/work/selachi-codex-12 -> C:/work/selachi\n  modified     crates/auth/src/session.rs\n  modified     crates/auth/tests/session.rs\n  added        crates/auth/tests/expiry.rs\n 3 files changed, 41 insertions(+), 6 deletions(-)\nWARNING: these files also have uncommitted changes in the project: crates/auth/src/session.rs\n";

fn request(
    id: &str,
    agent: &str,
    tool: &str,
    detail: &str,
    thread: i64,
    parent: i64,
) -> ApprovalRequest {
    ApprovalRequest {
        v: PROTOCOL_VERSION,
        request_id: id.to_string(),
        run_id: thread.to_string(),
        thread_id: thread,
        parent_thread_id: Some(parent),
        agent: agent.to_string(),
        tool: tool.to_string(),
        detail: detail.to_string(),
        permission: "ask".to_string(),
        depth: 1,
        timeout_secs: 120,
    }
}

/// Creates the parent thread and its two delegated children, and their registry records. Returns
/// `(parent, codex child, gemini child)`.
fn seed(store: &ChatStore, root: PathBuf) -> Option<(ThreadId, ThreadId, ThreadId)> {
    let mut spec = ThreadSpec::new(root.clone(), AgentKind::ClaudeCode);
    spec.title = Some("Endurecer la autenticación".to_string());
    let parent = store.create_thread(spec).ok()?;
    let rt = store.open(parent.id).ok()?;
    rt.send(
        "Revisa la autenticación y reparte el trabajo: tests en Codex, resumen en Gemini",
        Vec::new(),
    )
    .ok()?;
    let child = |title: &str, agent: AgentKind| {
        let mut s = ThreadSpec::new(root.clone(), agent);
        s.title = Some(title.to_string());
        s.link = Some(ThreadLink {
            parent_thread_id: Some(parent.id),
            parent_tab: None,
            depth: 1,
            delegated_by: Some("mcp".to_string()),
        });
        store.create_thread(s).ok()
    };
    let codex = child("Revisa los tests de auth", AgentKind::Codex)?;
    let gemini = child("Resumen del módulo de sesiones", AgentKind::OpenCode)?;

    let dir = data_dir()?;
    let reg = Registry::new(&dir);
    let now = chat_core::now_ms();
    let record = |t: &ThreadRecordLite,
                  status: RunStatus,
                  started: i64,
                  finished: Option<i64>,
                  files: &[&str],
                  usd: f64,
                  summary: &str| {
        let mut result = RunResult::empty(status);
        result.files_changed = files.iter().map(|f| (*f).to_string()).collect();
        result.summary = summary.to_string();
        result.usage = UsageOut::new(41_000, 3_200, 0, 120_000);
        result.cost.usd = usd;
        RunRecord {
            run_id: t.id.0.to_string(),
            thread_id: t.id.0,
            agent: t.agent.to_string(),
            model: None,
            permission: "read-only".into(),
            cwd: root.clone(),
            project_root: root.clone(),
            worktree_path: None,
            checkpoint_id: None,
            prompt: "demo".into(),
            parent_thread_id: Some(parent.id.0),
            depth: 1,
            started_ms: started,
            finished_ms: finished,
            status,
            runner_pid: std::process::id(),
            runner_start_ms: None,
            deadline_ms: now + 900_000,
            result: Some(result),
            applied_ms: None,
            // the Codex run uses a profile: its card offers "Permitir siempre en este perfil"
            profile: (t.agent == "codex").then(|| "Tests".to_string()),
            final_prompt: None,
            limits: None,
            launcher: Some("mcp".into()),
            effort: None,
            output_format: None,
            context_files: Vec::new(),
        }
    };
    let c = ThreadRecordLite {
        id: codex.id,
        agent: "codex",
    };
    let g = ThreadRecordLite {
        id: gemini.id,
        agent: "opencode",
    };
    let _ = reg.save(&record(
        &c,
        RunStatus::Done,
        now - 150_000,
        Some(now - 30_000),
        &["crates/auth/src/session.rs", "crates/auth/tests/session.rs", "crates/auth/tests/expiry.rs"],
        0.31,
        "Añadí tres pruebas de expiración de sesión y arreglé un off-by-one en `Session::is_expired`.",
    ));
    let _ = reg.save(&record(
        &g,
        RunStatus::Running,
        now - 45_000,
        None,
        &[],
        0.12,
        "",
    ));
    Some((parent.id, codex.id, gemini.id))
}

struct ThreadRecordLite {
    id: ThreadId,
    agent: &'static str,
}

impl AppShell {
    /// `SELACHI_DEMO=...` values of wave 6a.4 / 6b.2. Returns whether the value was one of them.
    pub(super) fn run_delegation_demo(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "delegation" | "approval" | "agentmap" => {
                self.demo_delegation(name == "approval", name == "agentmap", window, cx);
                true
            }
            "copy" => {
                self.demo_copy(window, cx);
                true
            }
            "delegation-settings" => {
                self.open_settings(window, cx);
                if let Some(v) = self.settings_view() {
                    v.update(cx, |s, cx| {
                        s.set_section(ui_settings::Section::Agents, cx);
                    });
                    v.read(cx).scroll_to_end();
                    // the scroll offset is applied after the first layout of the long page
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(1800))
                            .await;
                        this.update(cx, |s, cx| {
                            if let Some(v) = s.settings_view() {
                                v.read(cx).scroll_to_end();
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                true
            }
            "update" => {
                self.open_settings(window, cx);
                self.updates.update(cx, |u, cx| {
                    u.demo_ready("0.2.0", SAMPLE_NOTES, cx);
                });
                if let Some(v) = self.settings_view() {
                    v.update(cx, |s, cx| s.set_section(ui_settings::Section::General, cx));
                    v.read(cx).scroll_to_end();
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(1800))
                            .await;
                        this.update(cx, |s, cx| {
                            if let Some(v) = s.settings_view() {
                                v.read(cx).scroll_to_end();
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                true
            }
            "whatsnew" => {
                self.show_whats_new("0.2.0", SAMPLE_NOTES, window, cx);
                true
            }
            _ => false,
        }
    }

    fn demo_delegation(
        &mut self,
        with_cards: bool,
        with_map: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !crate::chat_host::fake_requested() {
            log::warn!("SELACHI_DEMO=delegation needs SELACHI_CHAT_FAKE=1");
            return;
        }
        // The hub opens `chat.db` in the background: the demo continues when it is ready.
        self.chat.update(cx, |c, cx| c.ensure(cx));
        cx.spawn_in(window, async move |this, cx| {
            // wait for the hub (it announces itself through `ChatHostEvent::Ready`; a short poll
            // here is fine for a demo and never runs in normal use)
            let mut hub = None;
            for _ in 0..100 {
                hub = this
                    .update(cx, |s, cx| s.chat.read(cx).hub())
                    .ok()
                    .flatten();
                if hub.is_some() {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
            }
            let Some(hub) = hub else { return };
            let Ok(store) = hub.read_with(cx, |h, _| h.store().clone()) else {
                return;
            };
            let root = std::env::current_dir().unwrap_or_default();
            let ids = cx
                .background_executor()
                .spawn(async move { seed(&store, root) })
                .await;
            let Some((parent, codex, _gemini)) = ids else {
                return;
            };
            this.update_in(cx, |shell, window, cx| {
                shell.reload_runs(cx);
                shell.open_chat_thread(parent, window, cx);
                if with_map {
                    shell.set_agentmap_root(Some(ui_agentmap::NodeKey::Thread(parent)), cx);
                }
                if with_cards {
                    shell.deleg.approvals.update(cx, |q, cx| {
                        q.add_demo(
                            request(
                                "demo-1",
                                "codex",
                                "Bash",
                                "cargo test -p auth -- --nocapture",
                                codex.0,
                                parent.0,
                            ),
                            cx,
                        );
                        q.add_demo(
                            request(
                                "demo-apply",
                                "codex",
                                "selachi:apply_changes",
                                APPLY_DETAIL,
                                codex.0,
                                parent.0,
                            ),
                            cx,
                        );
                    });
                }
            })
            .ok();
        })
        .detach();
    }

    /// `SELACHI_DEMO=copy`: a scripted chat, then (once the answer has streamed) a selection in
    /// the last assistant text and its copy menu, with no pointer input.
    fn demo_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::chat_host::fake_requested() {
            log::warn!("SELACHI_DEMO=copy needs SELACHI_CHAT_FAKE=1");
            return;
        }
        self.run_chat_intent(crate::chat_host::ChatIntent::Demo, window, cx);
        cx.spawn(async move |this, cx| {
            // demo only: the scripted answer streams for about 3 s
            cx.background_executor()
                .timer(std::time::Duration::from_millis(4500))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(tab) = shell.active_chat_tab(cx) {
                    let view = tab.read(cx).view().clone();
                    view.update(cx, |v, cx| v.debug_select((0, 20), (4, 12), true, cx));
                }
            })
            .ok();
        })
        .detach();
    }
}
