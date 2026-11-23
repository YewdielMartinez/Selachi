//! The chat UI over a real `ChatStore` with a FAKE agent driver (no processes, no model calls):
//! a scripted Claude-like session (thinking, an explored group, streamed Markdown, a command, an
//! edit, an approval, plan 0/4 to 2/4, usage) for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_chat --example chat_demo --release -- [dark|light] [options]
//!
//!   --scene thread|approval|streaming|empty|list|failed   what to show (default thread)
//!   --size WxH                                             window size (default 1280x800)
//!   --no-sidebar                                           hide the thread list column
//!   --width-class compact|regular                          class the sidebar list is built for
//! ```
//!
//! Scenes: `thread` a finished session; `approval` the same session stopped at a permission
//! request (click Permitir / Rechazar / Siempre: the fake continues); `streaming` the live
//! session, played in real time; `empty` the "¿Qué construimos?" screen; `list` many threads in
//! every state, with the first one open.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../src/fake.rs"]
mod fake;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agents_driver::{AgentEvent, AgentKind, AgentStatus, PermissionMode, TaskState, ToolKind};
use chat_core::{ChatStore, ThreadId, ThreadSpec, ThreadStatus};
use fake::{FactoryHandle, FakeFactory, Step};
use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use panel_api::{TabHandle, WidthClass};
use serde_json::json;
use theme::{Mode, Theme};
use ui_chat::{ChatEvent, ChatHub, ThreadListView, ThreadsPanelProvider};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};

const MODEL: &str = "claude-opus-5-5";
const TEXT_1: &str = "Voy a revisar el lexer: el problema está en cómo `skip_comment` cierra el comentario en el primer `*/`.\n\nHice un cambio pequeño, contando la profundidad:\n\n```rust\nfn skip_comment(&mut self) {\n    let mut depth = 1;\n    while depth > 0 {\n        match (self.bump(), self.peek()) {\n            ('/', '*') => depth += 1,\n            ('*', '/') => depth -= 1,\n            _ => {}\n        }\n    }\n}\n```\n\nEl plan:\n\n1. Leer el lexer actual\n2. Contar la profundidad de `/* */`\n3. Añadir tests de comentarios anidados\n4. Ejecutar `cargo test`\n";
const TEXT_2: &str = "Listo. Los comentarios anidados ya se cierran donde toca y los **14 tests** pasan. Añadí dos casos nuevos en `lexer.rs`:\n\n- `/* a /* b */ c */` se consume entero\n- un comentario sin cerrar da el error `UnterminatedComment`\n";

fn plan(done: usize, in_progress: bool) -> AgentEvent {
    let items = ["Leer el lexer actual", "Contar la profundidad de /* */", "Añadir tests de comentarios anidados", "Ejecutar cargo test"];
    AgentEvent::Plan {
        tasks: items
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let st = if i < done {
                    TaskState::Done
                } else if i == done && in_progress {
                    TaskState::InProgress
                } else {
                    TaskState::Pending
                };
                ((*t).to_string(), st)
            })
            .collect(),
    }
}

fn call(id: &str, kind: ToolKind, title: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolCall { id: id.into(), kind, title: title.into(), input_json: input }
}

fn result(id: &str, ok: bool, summary: &str) -> AgentEvent {
    AgentEvent::ToolResult { id: id.into(), ok, summary: summary.into() }
}

fn deltas(text: &str, chunk: usize, delay: u64) -> Vec<Step> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(chunk).map(|c| Step::after(delay, AgentEvent::Text { text: c.iter().collect(), delta: true })).collect()
}

/// The full scripted session. `pace` scales every pause (0 = instant); `stop_at_approval` leaves
/// the thread waiting for the user.
fn session(pace: u64, stop_at_approval: bool) -> Vec<Step> {
    let d = |ms: u64| ms * pace;
    let mut s = vec![
        Step::now(AgentEvent::SessionStarted { session_id: "demo-session".into(), model: Some(MODEL.into()) }),
        Step::now(AgentEvent::Status(AgentStatus::Working)),
        Step::after(d(500), AgentEvent::Thinking { text: "El usuario quiere comentarios anidados. Primero miro el lexer y cómo se cierran hoy los comentarios.".into(), delta: false }),
        Step::after(d(400), call("t1", ToolKind::Other("LS".into()), "crates/parser", json!({"path": "crates/parser"}))),
        Step::after(d(200), result("t1", true, "src/  tests/  Cargo.toml")),
        Step::after(d(300), call("t2", ToolKind::Search, "fn skip_comment", json!({"pattern": "fn skip_comment"}))),
        Step::after(d(200), result("t2", true, "crates/parser/src/lexer.rs:88")),
        Step::after(d(300), call("t3", ToolKind::Search, "UnterminatedComment", json!({"pattern": "UnterminatedComment"}))),
        Step::after(d(200), result("t3", true, "2 coincidencias")),
        Step::after(d(300), call("t4", ToolKind::Read, "crates/parser/src/lexer.rs", json!({"file_path": "crates/parser/src/lexer.rs"}))),
        Step::after(d(200), result("t4", true, "212 líneas")),
        Step::after(d(300), plan(0, false)),
    ];
    s.extend(deltas(TEXT_1, 14, d(22)));
    s.extend([
        Step::after(d(300), plan(2, true)),
        Step::after(d(300), call("t5", ToolKind::Edit, "crates/parser/src/lexer.rs", json!({
            "file_path": "crates/parser/src/lexer.rs",
            "old_string": "fn skip_comment(&mut self) {\n    while self.bump() != '*' || self.peek() != '/' {}\n}",
            "new_string": "fn skip_comment(&mut self) {\n    let mut depth = 1;\n    while depth > 0 {\n        match (self.bump(), self.peek()) {\n            ('/', '*') => depth += 1,\n            ('*', '/') => depth -= 1,\n            _ => {}\n        }\n    }\n}"
        }))),
        Step::after(d(300), result("t5", true, "ok")),
        Step::after(d(300), call("t6", ToolKind::Command, "cargo test -p parser", json!({"command": "cargo test -p parser"}))),
        Step::after(d(1400), result("t6", true, "running 14 tests\ntest lexer::line_comments ... ok\ntest lexer::block_comments ... ok\ntest lexer::nested_comments ... ok\ntest lexer::unterminated_comment ... ok\n...\ntest result: ok. 14 passed; 0 failed")),
    ]);
    if stop_at_approval {
        s.extend([
            Step::after(d(300), AgentEvent::ApprovalRequest { id: "req-1".into(), tool: "Bash".into(), detail: "cargo clippy -p parser -- -D warnings".into() }),
            Step::after(d(50), AgentEvent::Status(AgentStatus::WaitingForInput)),
        ]);
        return s;
    }
    s.extend(finish(pace));
    s
}

/// The tail of the session: plan 4/4, closing text, usage, Done.
fn finish(pace: u64) -> Vec<Step> {
    let d = |ms: u64| ms * pace;
    let mut s = vec![Step::after(d(200), AgentEvent::Status(AgentStatus::Working)), Step::after(d(200), plan(3, true))];
    s.extend(deltas(TEXT_2, 16, d(22)));
    s.extend([
        Step::after(d(200), plan(4, false)),
        Step::after(
            d(100),
            AgentEvent::Usage { input: 58_000, output: 2_100, cache_create: 0, cache_read: 20_000, cost_usd: Some(0.12), context_window: Some(1_000_000) },
        ),
        Step::after(d(100), AgentEvent::Status(AgentStatus::Done)),
    ]);
    s
}

/// What the fake agent answers, by the prompt it receives.
fn script(pace: u64, stop_at_approval: bool) -> impl Fn(usize, &str) -> Vec<Step> + Send + Sync + 'static {
    move |_, prompt| {
        if prompt.starts_with("[working]") {
            vec![
                Step::now(AgentEvent::SessionStarted { session_id: "w".into(), model: Some(MODEL.into()) }),
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::Text { text: "Estoy migrando las pestañas…".into(), delta: true }),
            ]
        } else if prompt.starts_with("[waiting]") {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::ApprovalRequest { id: "w1".into(), tool: "Bash".into(), detail: "git push origin main".into() }),
                Step::now(AgentEvent::Status(AgentStatus::WaitingForInput)),
            ]
        } else if prompt.starts_with("[quick]") {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::Text { text: "Hecho.".into(), delta: false }),
                Step::now(AgentEvent::Status(AgentStatus::Done)),
            ]
        } else if prompt.starts_with("Refactoriza") {
            session(pace, stop_at_approval)
        } else {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::after(pace * 400, AgentEvent::Text { text: "Entendido. Esto es una respuesta de la demo (sin modelo real).".into(), delta: false }),
                Step::now(AgentEvent::Status(AgentStatus::Done)),
            ]
        }
    }
}

struct Seed {
    title: &'static str,
    project: &'static str,
    agent: AgentKind,
    model: Option<&'static str>,
    kind: &'static str,
    age_min: i64,
}

fn seeds() -> Vec<Seed> {
    let s = |title, project, agent: AgentKind, model, kind, age_min| Seed { title, project, agent, model, kind, age_min };
    vec![
        s("Migrar pestañas a la sidebar", "selachi", AgentKind::ClaudeCode, Some("claude-opus-5-5"), "[working]", 0),
        s("Revisar el workflow de release", "selachi", AgentKind::Codex, Some("gpt-5-codex"), "[waiting]", 0),
        s("Corregir el scroll del visor", "melon-mind", AgentKind::ClaudeCode, Some("claude-sonnet-5"), "[quick]", 49),
        s("Añadir pruebas a tasks_svc", "selachi", AgentKind::ClaudeCode, Some("claude-opus-5-5"), "[quick]", 120),
        s("Explicar el módulo de OSC 133", "selachi", AgentKind::OpenCode, None, "[quick]", 300),
        s("Dockerizar el sitio", "stepbro-website", AgentKind::ClaudeCode, Some("claude-sonnet-5"), "[quick]", 60 * 26),
        s("Bump de dependencias", "stepbro-website", AgentKind::Codex, Some("gpt-5-codex"), "[quick]", 60 * 50),
        s("Migración de la base de datos", "melon-mind", AgentKind::ClaudeCode, Some("claude-haiku-4-5"), "[quick]", 60 * 24 * 3),
    ]
}

fn make_project(root: &Path, name: &str) -> PathBuf {
    let p = root.join(name);
    let _ = std::fs::create_dir_all(p.join("src"));
    let _ = std::fs::write(p.join("src").join("main.rs"), "fn main() {}\n");
    let _ = std::fs::write(p.join("README.md"), "# demo\n");
    let _ = std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"demo\"\n");
    p
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

struct Host {
    theme: Theme,
    tab: TabHandle,
    list: Option<Entity<ThreadListView>>,
    log: String,
    project: PathBuf,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let info = self.tab.info(cx);
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child({
                let mut p = Pill::new("tab", &t).active(true).label(info.title.to_string());
                p = match &info.icon {
                    panel_api::TabIcon::Agent(name) => p.agent(ui_kit::AgentMark::from_name(name)),
                    panel_api::TabIcon::Kind(i) => p.icon(*i),
                };
                p
            })
            .child(Pill::new("tab2", &t).icon(Icon::Terminal).label("terminal"));
        let sidebar = self.list.clone().map(|l| div().w(px(300.0)).h_full().flex_none().rounded(px(16.0)).bg(c.surface_1).overflow_hidden().child(l));
        let main = Card::new(&t).tone(CardTone::Base, &t).flex_1().min_w_0().h_full().overflow_hidden().child(self.tab.content(cx));
        let status = div()
            .h(px(28.0))
            .flex_none()
            .px(px(16.0))
            .flex()
            .items_center()
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .child(self.log.clone());
        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(ty.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(div().flex_1().min_h_0().px(px(8.0)).pb(px(4.0)).flex().flex_row().gap(px(8.0)).children(sidebar).child(main))
            .child(status)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let mode = if flag("light") { Mode::Light } else { Mode::Dark };
    let scene = arg_value(&args, "--scene").unwrap_or("thread").to_owned();
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280.0f32, 800.0f32));
    let sidebar = !flag("--no-sidebar") && !matches!(scene.as_str(), "empty" | "failed");
    let compact = arg_value(&args, "--width-class") == Some("compact");

    let tmp = std::env::temp_dir().join(format!("selachi-chat-demo-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let projects: Vec<PathBuf> = ["selachi", "melon-mind", "stepbro-website"].iter().map(|n| make_project(&tmp, n)).collect();

    let pace = if scene == "streaming" { 1 } else { 0 };
    let stop_at_approval = scene == "approval";
    let factory = FakeFactory::new(script(pace, stop_at_approval));
    *factory.on_approve.lock().unwrap_or_else(|p| p.into_inner()) = {
        let mut v = vec![Step::now(AgentEvent::Status(AgentStatus::Working))];
        v.extend(finish(1));
        v
    };
    let store = ChatStore::builder()
        .path(tmp.join("chat.db"))
        .factory(Arc::new(FactoryHandle(factory)))
        .open()
        .expect("open chat store");

    // The main thread of the scene: the scripted session.
    let main_project = projects[0].clone();
    let mut main_id: Option<ThreadId> = None;
    if matches!(scene.as_str(), "thread" | "approval" | "streaming" | "list") {
        let mut spec = ThreadSpec::new(&main_project, AgentKind::ClaudeCode);
        spec.model = Some(MODEL.into());
        spec.permission = PermissionMode::AcceptEdits;
        let rec = store.create_thread(spec).expect("create thread");
        let rt = store.open(rec.id).expect("open thread");
        if scene != "streaming" {
            let _ = rt.send("Refactoriza el parser para que soporte comentarios anidados y añade tests", Vec::new());
            let want_status = if stop_at_approval { ThreadStatus::WaitingForInput } else { ThreadStatus::Done };
            rt.wait_for(Duration::from_secs(20), |s| s.status == want_status);
        }
        main_id = Some(rec.id);
    }

    // The other threads of the list, one per state.
    if scene == "list" {
        let mut to_close = Vec::new();
        for seed in seeds() {
            let mut spec = ThreadSpec::new(projects.iter().find(|p| p.ends_with(seed.project)).cloned().unwrap_or_else(|| main_project.clone()), seed.agent.clone());
            spec.model = seed.model.map(str::to_string);
            spec.title = Some(seed.title.to_string());
            let rec = store.create_thread(spec).expect("create seed thread");
            let rt = store.open(rec.id).expect("open seed thread");
            let _ = rt.send(format!("{} {}", seed.kind, seed.title), Vec::new());
            let want = match seed.kind {
                "[working]" => ThreadStatus::Working,
                "[waiting]" => ThreadStatus::WaitingForInput,
                _ => ThreadStatus::Done,
            };
            rt.wait_for(Duration::from_secs(10), |s| s.status == want);
            if seed.kind == "[quick]" {
                to_close.push((rec.id, seed.age_min));
            }
        }
        // Age the finished threads (a closed runtime reads its row from the database).
        let db = tmp.join("chat.db");
        store.flush().ok();
        for (id, _) in &to_close {
            store.close_thread(*id);
        }
        store.flush().ok();
        if let Ok(conn) = rusqlite::Connection::open(&db) {
            let now = chat_core::now_ms();
            for (id, age) in &to_close {
                let _ = conn.execute("UPDATE threads SET updated = ?1 WHERE id = ?2", rusqlite::params![now - age * 60_000, id.0]);
            }
        }
    }

    Application::new().with_assets(ui_kit::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            eprintln!("fonts: {e}");
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        ui_chat::init(cx);
        let theme = Theme::for_mode(mode);
        let hub = ChatHub::create(store, theme, cx);
        hub.update(cx, |h, cx| {
            h.set_projects(projects.clone(), cx);
            h.set_default_project(Some(main_project.clone()));
        });
        let tab = hub.update(cx, |h, cx| match main_id {
            Some(id) => h.open_thread_tab(id, cx),
            None => h.new_thread_tab(Some(main_project.clone()), cx),
        });
        if let Some(id) = main_id {
            hub.update(cx, |h, cx| h.set_active_thread(Some(id), cx));
        }
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let streaming = scene == "streaming";
        let _ = cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, move |window, cx| {
            let list = sidebar.then(|| {
                let hub = hub.clone();
                cx.new(|cx| ThreadListView::new(hub, theme, if compact { WidthClass::Compact } else { WidthClass::Regular }, cx))
            });
            let host = cx.new(|cx| {
                let _ = ThreadsPanelProvider::new(hub.clone());
                let host = Host { theme, tab: tab.clone(), list, log: "Host: los eventos del chat se anotan aquí".into(), project: main_project.clone() };
                cx.subscribe(&hub.clone(), |this: &mut Host, hub, ev: &ChatEvent, cx| {
                    this.log = format!("Host recibe: {ev:?}");
                    match ev {
                        ChatEvent::OpenThread(id) => this.tab = hub.update(cx, |h, cx| h.open_thread_tab(*id, cx)),
                        ChatEvent::NewThread { project } => {
                            let p = project.clone().or_else(|| Some(this.project.clone()));
                            this.tab = hub.update(cx, |h, cx| h.new_thread_tab(p, cx));
                        }
                        _ => {}
                    }
                    cx.notify();
                })
                .detach();
                host
            });
            if streaming {
                // Type the prompt and send it, as a user would.
                let tab = tab.clone();
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor().timer(Duration::from_millis(600)).await;
                        let _ = cx.update(|_, cx| {
                            if let Some(t) = tab.downcast::<ui_chat::ChatTab>() {
                                let view = t.read(cx).view().clone();
                                view.update(cx, |v, cx| {
                                    v.send("Refactoriza el parser para que soporte comentarios anidados y añade tests".into(), Vec::new(), cx);
                                });
                            }
                        });
                    })
                    .detach();
            }
            host
        });
        cx.activate(true);
    });
}
