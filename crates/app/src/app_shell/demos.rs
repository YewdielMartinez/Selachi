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
//! | `settings:<section>[:<scroll>]` | Settings on `general`, `appearance`, `design`, `terminal`, `agents`, `profiles` (Agentes > Perfiles), `git`, `processes`, `plugins`, `keyboard` or `about`, scrolled `<scroll>` logical px (or `end`). `SELACHI_DEMO_SIZE=1100x900` resizes the window (logical px), `SELACHI_DEMO_INVALID=1` types invalid text in the number fields of Agentes > Delegación so their errors show. The pane width is written to `<data dir>/demo-pane.txt` |
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
    write_demo_events(&reg, gemini.id, now);
    Some((parent.id, codex.id, gemini.id))
}

/// The events log (`<run id>.events.jsonl`, the harness' own format) of the demo's running run: what
/// the map's Actividad tab shows (messages, commands with their state, an edit, an approval).
fn write_demo_events(reg: &Registry, run: ThreadId, now: i64) {
    let t = |ago_s: i64| now - ago_s * 1000;
    let lines = [
        format!(r#"{{"t":{},"type":"status","status":"working"}}"#, t(44)),
        format!(
            r#"{{"t":{},"type":"message_delta","text":"Voy a leer el módulo de sesiones y luego correr las pruebas de expiración."}}"#,
            t(42)
        ),
        format!(
            r#"{{"t":{},"type":"tool_start","id":"c1","kind":"command","command":"cargo test -p auth --lib","title":"cargo test -p auth --lib"}}"#,
            t(38)
        ),
        format!(
            r#"{{"t":{},"type":"tool_end","id":"c1","exit_code":0,"duration_ms":9000,"output_tail":"running 14 tests\ntest session::tests::expires_after_ttl ... ok\ntest result: ok. 14 passed; 0 failed","ok":true}}"#,
            t(29)
        ),
        format!(
            r#"{{"t":{},"type":"tool_start","id":"e1","kind":"edit","path":"crates/auth/src/session.rs","title":"crates/auth/src/session.rs"}}"#,
            t(24)
        ),
        format!(
            r#"{{"t":{},"type":"tool_end","id":"e1","duration_ms":120,"output_tail":"","ok":true}}"#,
            t(24)
        ),
        format!(
            r#"{{"t":{},"type":"file_edit","path":"crates/auth/src/session.rs","added":12,"removed":3}}"#,
            t(24)
        ),
        format!(
            r#"{{"t":{},"type":"approval","request":"cargo clippy -p auth","decision":"allowed","mode":"ask"}}"#,
            t(15)
        ),
        format!(
            r#"{{"t":{},"type":"tool_start","id":"c2","kind":"command","command":"cargo clippy -p auth -- -D warnings","title":"cargo clippy -p auth -- -D warnings"}}"#,
            t(14)
        ),
        format!(
            r#"{{"t":{},"type":"message_delta","text":"Las pruebas pasan; ahora reviso los avisos de clippy."}}"#,
            t(6)
        ),
    ];
    let path = reg.events_file(&run.0.to_string());
    let _ = std::fs::write(path, lines.join("\n") + "\n");
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
            other if other.starts_with("settings:") => {
                self.demo_settings(&other["settings:".len()..], window, cx);
                true
            }
            _ => false,
        }
    }

    /// `SELACHI_DEMO=settings:<section>[:<scroll>]`: the real settings tab of the real shell, so a
    /// screenshot shows what the user sees (zone width, tab host, the platform text shaper).
    fn demo_settings(&mut self, spec: &str, window: &mut Window, cx: &mut Context<Self>) {
        use ui_settings::{AgentsTab, Section};
        if let Some((w, h)) = std::env::var("SELACHI_DEMO_SIZE")
            .ok()
            .as_deref()
            .and_then(|v| v.split_once('x'))
            .and_then(|(w, h)| Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?)))
        {
            window.resize(gpui::size(gpui::px(w), gpui::px(h)));
        }
        let mut parts = spec.split(':');
        let name = parts.next().unwrap_or("general");
        let scroll = parts.next().map(str::to_string);
        let (section, tab) = match name {
            "appearance" => (Section::Appearance, AgentsTab::General),
            "design" => (Section::Design, AgentsTab::General),
            "terminal" => (Section::Terminal, AgentsTab::General),
            "agents" => (Section::Agents, AgentsTab::General),
            "profiles" => (Section::Agents, AgentsTab::Profiles),
            "git" => (Section::Git, AgentsTab::General),
            "processes" => (Section::Processes, AgentsTab::General),
            "plugins" => (Section::Plugins, AgentsTab::General),
            "keyboard" => (Section::Keyboard, AgentsTab::General),
            "about" => (Section::About, AgentsTab::General),
            _ => (Section::General, AgentsTab::General),
        };
        self.open_settings(window, cx);
        let invalid = std::env::var_os("SELACHI_DEMO_INVALID").is_some();
        if let Some(v) = self.settings_view() {
            v.update(cx, |s, cx| {
                if section == Section::Agents {
                    s.set_agents_tab(tab, cx);
                } else {
                    s.set_section(section, cx);
                }
            });
        }
        // The page is laid out for the first time a few frames later: scroll and the invalid
        // values wait for it.
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1500))
                .await;
            this.update(cx, |s, cx| {
                if let Some(dir) = data_dir() {
                    let _ = std::fs::write(
                        dir.join("demo-pane.txt"),
                        format!("{:.0}", s.main_pane_width()),
                    );
                }
                if let Some(v) = s.settings_view() {
                    if invalid {
                        v.update(cx, |s, cx| s.demo_invalid_values(cx));
                    }
                    match scroll.as_deref() {
                        Some("end") => v.read(cx).scroll_to_end(),
                        Some(y) => {
                            if let Ok(y) = y.parse::<f32>() {
                                v.read(cx).scroll_to(y);
                            }
                        }
                        None => {}
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            let Some((parent, codex, gemini)) = ids else {
                return;
            };
            this.update_in(cx, |shell, window, cx| {
                shell.reload_runs(cx);
                shell.open_chat_thread(parent, window, cx);
                if with_map {
                    shell.set_agentmap_root(Some(ui_agentmap::NodeKey::Thread(parent)), cx);
                    if let Some(model) = cx
                        .try_global::<crate::services::Services>()
                        .map(|s| s.agentmap.clone())
                    {
                        model.update(cx, |m, cx| {
                            m.open_run(gemini, cx);
                            m.set_inspector_tab(ui_agentmap::InspectorTab::Activity, cx);
                        });
                    }
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
