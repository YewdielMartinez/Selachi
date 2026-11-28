//! The subagent map over FAKE data: a temp chat database with an orchestrator and its delegations
//! (links included) and fake run records in a temp `agent-runs` folder. Nothing real is read and no
//! agent runs; events of the inspector are only printed.
//!
//! ```text
//! cargo run -p ui_agentmap --example agentmap_demo -- [dark|light] [--mode tree|graph|timeline]
//!     [--select <thread id>] [--zone main|right] [--size WxH] [--external] [--select-external <pid>]
//!     [--activity] [--tab summary|activity|prompt] [--live]
//!     [--many] [--group parent|agent|status|project] [--sort start|recent|duration]
//!     [--filter running|failed|done|external] [--query text] [--hide-older MIN] [--options]
//!
//! `--external` adds three FAKE `agy` processes launched outside the harness under the orchestrator
//! (two running, one finished); `--select-external 4120` opens the inspector of the first one.
//!
//! `--activity` writes FAKE events logs (`agent-runs/3.events.jsonl`, `4.events.jsonl`) so the
//! running delegations show their status line and the "Actividad" tab has a feed; `--tab activity`
//! opens that tab. With `--external` the first external agent also gets fake child processes.
//! `--live` keeps appending fake message lines to `3.events.jsonl` every 1.5 s (push check).
//! ```
//!
//! `--many` adds nine FAKE `agy` runs as the harness records them (raw prompts with `Repo:` / `HARD RULES`)
//! in two projects: what the map has to tidy up.
//!
//! Thread ids: 1 orchestrator, 2 tests (done), 3 docs (running), 4 security (waiting for an
//! approval), 5 lint (failed), 6 migration (done, with a child), 7 its review (done), 8 (cancelled).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use agents_driver::{AgentKind, PermissionMode};
use chat_core::{ChatDb, ChatStore, ThreadId, ThreadLink, ThreadSpec, ThreadStatus, now_ms};
use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use theme::{Mode, Theme};
use ui_agentmap::{
    AgentMapEvent, AgentMapModel, AgentMapPanelProvider, AgentMapView, ChatStoreSource,
    ExternalChild, ExternalProc, GroupBy, InspectorTab, MapMode, NodeKey, SortBy, StatusFilter,
};
use ui_kit::{Assets, Card, CardTone};

struct Host {
    theme: Theme,
    panel: gpui::AnyView,
    _keep: Option<tempfile::TempDir>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        div()
            .size_full()
            .bg(t.colors.bg)
            .text_color(t.colors.text)
            .font_family(t.typography.ui_family)
            .p(px(8.0))
            .child(
                Card::new(&t)
                    .tone(CardTone::Raised, &t)
                    .size_full()
                    .overflow_hidden()
                    .child(self.panel.clone()),
            )
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

struct Spec {
    title: &'static str,
    agent: AgentKind,
    agent_id: &'static str,
    model: &'static str,
    effort: &'static str,
    parent: Option<i64>,
    /// Minutes ago it started, and how long it ran (`None`: still running).
    start_min_ago: f64,
    ran_min: Option<f64>,
    status: &'static str,
    tokens: u64,
    cost: f64,
    files: usize,
    worktree: bool,
    prompt: &'static str,
    thread_status: ThreadStatus,
}

fn specs() -> Vec<Spec> {
    vec![
        Spec {
            title: "Migrar la autenticación a OAuth",
            agent: AgentKind::ClaudeCode,
            agent_id: "claude",
            model: "opus-4.5",
            effort: "high",
            parent: None,
            start_min_ago: 14.0,
            ran_min: None,
            status: "",
            tokens: 0,
            cost: 0.0,
            files: 0,
            worktree: false,
            prompt: "",
            thread_status: ThreadStatus::Working,
        },
        Spec {
            title: "Revisar los tests de auth",
            agent: AgentKind::Codex,
            agent_id: "codex",
            model: "gpt-5.1-codex",
            effort: "high",
            parent: Some(1),
            start_min_ago: 12.0,
            ran_min: Some(3.4),
            status: "done",
            tokens: 48_200,
            cost: 0.42,
            files: 3,
            worktree: true,
            prompt: "Revisa los tests de src/auth y añade los que falten para el flujo de refresco de token. No toques código de producción. api_key=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
            thread_status: ThreadStatus::Done,
        },
        Spec {
            title: "Documentar los endpoints nuevos",
            agent: AgentKind::Acp {
                command: "gemini --acp".into(),
            },
            agent_id: "gemini",
            model: "gemini-2.5-pro",
            effort: "medium",
            parent: Some(1),
            start_min_ago: 9.0,
            ran_min: None,
            status: "running",
            tokens: 0,
            cost: 0.0,
            files: 0,
            worktree: true,
            prompt: "Documenta los endpoints /oauth/authorize y /oauth/token en docs/API.md con ejemplos.",
            thread_status: ThreadStatus::Working,
        },
        Spec {
            title: "Auditoría de seguridad del callback",
            agent: AgentKind::OpenCode,
            agent_id: "opencode",
            model: "claude-sonnet-4.5",
            effort: "high",
            parent: Some(1),
            start_min_ago: 7.0,
            ran_min: None,
            status: "running",
            tokens: 0,
            cost: 0.0,
            files: 0,
            worktree: false,
            prompt: "Audita el manejo del parámetro state y de la redirección en el callback OAuth. Solo lectura.",
            thread_status: ThreadStatus::WaitingForInput,
        },
        Spec {
            title: "Corregir el lint del módulo",
            agent: AgentKind::Codex,
            agent_id: "codex",
            model: "gpt-5.1-codex-mini",
            effort: "low",
            parent: Some(1),
            start_min_ago: 6.0,
            ran_min: Some(0.7),
            status: "error",
            tokens: 6_100,
            cost: 0.03,
            files: 0,
            worktree: false,
            prompt: "Corrige los avisos de clippy de src/auth.",
            thread_status: ThreadStatus::Error,
        },
        Spec {
            title: "Escribir la migración de sesiones",
            agent: AgentKind::Acp {
                command: "agy acp".into(),
            },
            agent_id: "agy",
            model: "gemini-3-pro",
            effort: "high",
            parent: Some(1),
            start_min_ago: 11.0,
            ran_min: Some(5.2),
            status: "done",
            tokens: 212_000,
            cost: 1.18,
            files: 5,
            worktree: true,
            prompt: "Escribe la migración SQL que mueve las sesiones antiguas al nuevo esquema.",
            thread_status: ThreadStatus::Done,
        },
        Spec {
            title: "Revisar la migración",
            agent: AgentKind::ClaudeCode,
            agent_id: "claude",
            model: "sonnet-4.5",
            effort: "medium",
            parent: Some(6),
            start_min_ago: 5.5,
            ran_min: Some(2.1),
            status: "done",
            tokens: 31_400,
            cost: 0.19,
            files: 0,
            worktree: false,
            prompt: "Revisa la migración de sesiones buscando pérdida de datos.",
            thread_status: ThreadStatus::Done,
        },
        Spec {
            title: "Probar con Playwright",
            agent: AgentKind::Codex,
            agent_id: "codex",
            model: "gpt-5.1-codex",
            effort: "medium",
            parent: Some(1),
            start_min_ago: 3.0,
            ran_min: Some(0.5),
            status: "cancelled",
            tokens: 2_000,
            cost: 0.01,
            files: 0,
            worktree: false,
            prompt: "Prueba el flujo de login con Playwright.",
            thread_status: ThreadStatus::Idle,
        },
    ]
}

/// FAKE "real data" load: many `agy` runs under the orchestrator, the way the harness records them
/// (thread titled `agy: <first prompt line>`, prompts that start with `Repo:` and `HARD RULES`), in
/// two projects and with a mix of states.
fn add_many(store: &ChatStore, runs: &Path, root: &Path, parent: i64) {
    let now = now_ms();
    let min = |m: f64| (m * 60_000.0) as i64;
    let tasks = [
        ("Audita el manejo de errores del cliente HTTP.", "done", 1.4, 22.0, "proyecto"),
        ("Revisa la cobertura de tests del módulo de sesiones.", "done", 2.2, 21.0, "proyecto"),
        ("Busca usos de unwrap en crates de servicios.", "error", 0.6, 19.0, "proyecto"),
        ("Documenta la API pública de store.", "done", 3.8, 18.0, "api"),
        ("Compara el esquema de la base con las migraciones.", "done", 1.1, 16.0, "api"),
        ("Lista los TODO del repositorio y agrúpalos por crate.", "done", 0.9, 14.0, "proyecto"),
        ("Revisa la accesibilidad del panel de ajustes.", "running", 0.0, 6.0, "proyecto"),
        ("Prueba el flujo de actualización en una instalación limpia.", "running", 0.0, 4.0, "api"),
        ("Resume los cambios de la rama para el PR.", "cancelled", 0.4, 12.0, "proyecto"),
    ];
    for (i, (task, status, ran, ago, project)) in tasks.iter().enumerate() {
        let proj = root.join(project);
        std::fs::create_dir_all(&proj).expect("project");
        let prompt = format!(
            "Repo: C:\\\\Users\\\\venta\\\\Desktop\\\\{project}\\nHARD RULES\\n- Solo lectura\\n- No ejecutes comandos destructivos\\n\\n{task} Cita archivo y línea."
        );
        let mut spec = ThreadSpec::new(&proj, AgentKind::Acp { command: "agy acp".into() });
        spec.title = Some("agy: Repo: C:\\Users\\vent…".into());
        spec.model = Some("gemini-3-pro".into());
        spec.effort = Some(if i % 3 == 0 { "high" } else { "medium" }.into());
        spec.permission = PermissionMode::ReadOnly;
        spec.link = Some(ThreadLink {
            parent_thread_id: Some(ThreadId(parent)),
            parent_tab: None,
            depth: 1,
            delegated_by: Some("mcp".into()),
        });
        let rec = store.create_thread(spec).expect("thread");
        let id = rec.id.0;
        let started = now - min(*ago);
        let fin = (*status != "running").then(|| started + min(*ran));
        let json = format!(
            r#"{{"run_id":"{id}","thread_id":{id},"agent":"agy","model":"gemini-3-pro","effort":"medium","permission":"read-only",
            "cwd":"C:/Users/demo/{project}","project_root":"C:/Users/demo/{project}","prompt":"{prompt}","parent_thread_id":{parent},
            "depth":1,"started_ms":{started},{f}"status":"{status}","launcher":"mcp","deadline_ms":{dl},
            "result":{{"summary":"ok","files_changed":[],"usage":{{"billable":{tok}}},"cost":{{"usd":{cost},"exact":false}},"notes":[]}}}}"#,
            f = fin.map(|f| format!(r#""finished_ms":{f},"#)).unwrap_or_default(),
            dl = started + min(15.0),
            tok = 8_000 + 3_000 * i as u64,
            cost = 0.05 + 0.03 * i as f64,
        );
        std::fs::write(runs.join(format!("{id}.json")), json).expect("run");
        let db = ChatDb::open_with(&root.join("chat.db"), false).expect("db");
        db.set_status(
            ThreadId(id),
            match *status {
                "running" => ThreadStatus::Working,
                "error" => ThreadStatus::Error,
                "cancelled" => ThreadStatus::Idle,
                _ => ThreadStatus::Done,
            },
        )
        .expect("status");
    }
}

fn build_fake_data(root: &Path, many: bool) -> (ChatStore, std::path::PathBuf) {
    let project = root.join("proyecto");
    std::fs::create_dir_all(&project).expect("project");
    let db_path = root.join("chat.db");
    let store = ChatStore::open_at(&db_path).expect("chat db");
    let runs = root.join("agent-runs");
    std::fs::create_dir_all(&runs).expect("runs");
    let now = now_ms();
    let min = |m: f64| (m * 60_000.0) as i64;

    let mut ids = Vec::new();
    for s in specs() {
        let mut spec = ThreadSpec::new(&project, s.agent.clone());
        spec.title = Some(s.title.to_string());
        spec.model = Some(s.model.to_string());
        spec.effort = Some(s.effort.to_string());
        spec.permission = PermissionMode::ReadOnly;
        spec.link = s.parent.map(|p| ThreadLink {
            parent_thread_id: Some(ThreadId(ids[(p - 1) as usize])),
            parent_tab: None,
            depth: if p == 1 { 1 } else { 2 },
            delegated_by: Some(if s.agent_id == "gemini" { "cli" } else { "mcp" }.into()),
        });
        let rec = store.create_thread(spec).expect("thread");
        ids.push(rec.id.0);
    }
    // statuses and the fake registry
    let db = ChatDb::open_with(&db_path, false).expect("db");
    for (i, s) in specs().iter().enumerate() {
        let id = ids[i];
        db.set_status(ThreadId(id), s.thread_status)
            .expect("status");
        if s.parent.is_none() {
            continue;
        }
        let started = now - min(s.start_min_ago);
        let finished = s.ran_min.map(|r| started + min(r));
        let parent = ids[(s.parent.unwrap() - 1) as usize];
        let worktree = if s.worktree {
            format!(
                r#""worktree_path":"C:/Users/demo/proyecto.worktrees/{}-{}","#,
                s.agent_id, id
            )
        } else {
            String::new()
        };
        let result = if s.status == "running" {
            String::new()
        } else {
            let files: Vec<String> = (0..s.files)
                .map(|f| format!(r#""src/auth/file{f}.rs""#))
                .collect();
            format!(
                r#","result":{{"summary":"{}","files_changed":[{}],"diff_stat":{{"files":{},"insertions":{},"deletions":{},"text":" src/auth/file0.rs | 24 ++++++++++----\n src/auth/file1.rs | 11 +++++\n {} files changed"}},"usage":{{"billable":{}}},"cost":{{"usd":{},"exact":false}},"notes":[]}}"#,
                match s.status {
                    "done" => "Terminó sin incidencias.",
                    "error" => "El agente falló: clippy no pudo compilar el módulo.",
                    _ => "Cancelado por el usuario.",
                },
                files.join(","),
                s.files,
                s.files * 8,
                s.files * 2,
                s.files,
                s.tokens,
                s.cost
            )
        };
        let perm = if s.worktree {
            "accept-edits"
        } else {
            "read-only"
        };
        let json = format!(
            r#"{{"run_id":"{id}","thread_id":{id},"agent":"{}","model":"{}","effort":"{}","permission":"{perm}",
            "cwd":"C:/Users/demo/proyecto","project_root":"C:/Users/demo/proyecto",{worktree}
            "prompt":"{}","final_prompt":"=== SELACHI PROFILE: {} ===\nSolo lectura. Cita archivo y línea.\n=== END SELACHI PROFILE ===\n\n=== TASK ===\n{}",
            "parent_thread_id":{parent},"depth":{},"started_ms":{started},{fin}"status":"{}",
            "launcher":"{}","profile":"{}","deadline_ms":{},
            "effective_limits":{{"permission":"{perm}","timeout_secs":900,"budget_tokens":300000,"depth":{}}},
            "limits":{{"approvals":"{}","auto_approved":{},"auto_approved_commands":["cargo test -p auth","rg -n token src"],"blocked":{}}},
            "approvals":[{{"tool":"Bash","detail":"cargo test -p auth","decision":"allowed"}},{{"tool":"Edit","detail":"src/auth/mod.rs","decision":"{}"}}]{result}}}"#,
            s.agent_id,
            s.model,
            s.effort,
            s.prompt.replace('"', "'"),
            if s.agent_id == "codex" {
                "Tests"
            } else {
                "Revisor"
            },
            s.prompt.replace('"', "'"),
            if parent == ids[0] { 1 } else { 2 },
            if s.status.is_empty() {
                "running"
            } else {
                s.status
            },
            if s.agent_id == "gemini" { "cli" } else { "mcp" },
            if s.agent_id == "codex" { "Tests" } else { "" },
            started + min(15.0),
            if parent == ids[0] { 1 } else { 2 },
            if s.agent_id == "codex" { "auto" } else { "ask" },
            if s.agent_id == "codex" { 12 } else { 0 },
            if s.agent_id == "codex" { 1 } else { 0 },
            if s.thread_status == ThreadStatus::WaitingForInput {
                "pending"
            } else {
                "denied"
            },
            fin = finished
                .map(|f| format!(r#""finished_ms":{f},"#))
                .unwrap_or_default(),
        );
        std::fs::write(runs.join(format!("{id}.json")), json).expect("run record");
    }
    drop(db);
    if many {
        add_many(&store, &runs, root, ids[0]);
    }
    (store, runs)
}

/// One fake line of an events log (`t` = ms since the run started).
fn ev(t: i64, body: &str) -> String {
    format!("{{\"t\":{t},{body}}}\n")
}

/// FAKE events logs: delegation 3 (running: docs) has a long story, 4 (waiting) a pending approval.
fn write_fake_events(runs: &Path) {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let mut three = String::new();
    three += &ev(
        2_000,
        &format!(
            r#""type":"message_delta","text":"{}""#,
            esc(
                "Voy a documentar los endpoints /oauth/authorize y /oauth/token. Primero reviso cómo están implementados en el servidor."
            )
        ),
    );
    three += &ev(
        9_000,
        &format!(
            r#""type":"tool_start","id":"c1","kind":"command","command":"{}""#,
            esc("rg -n \"oauth/(authorize|token)\" src docs")
        ),
    );
    three += &ev(
        9_400,
        &format!(
            r#""type":"tool_end","id":"c1","exit_code":0,"duration_ms":380,"output_tail":"{}""#,
            esc(
                "src/auth/routes.rs:42:    .route(\"/oauth/authorize\", get(authorize))\nsrc/auth/routes.rs:43:    .route(\"/oauth/token\", post(token))\ndocs/API.md:118:## OAuth"
            )
        ),
    );
    three += &ev(
        15_000,
        &format!(
            r#""type":"message_delta","text":"{}""#,
            esc(
                "Encontré las dos rutas y su documentación actual está incompleta:\n1. /oauth/authorize no documenta el parámetro state.\n2. /oauth/token no muestra el refresco de token.\n3. Faltan ejemplos con curl.\nVoy a reescribir la sección OAuth de docs/API.md y a comprobar que compila."
            )
        ),
    );
    three += &ev(
        30_000,
        r#""type":"file_edit","path":"docs/API.md","added":64,"removed":2"#,
    );
    three += &ev(
        31_000,
        r#""type":"approval","request":"r1","decision":"pending","command":"cargo doc --no-deps -p auth""#,
    );
    three += &ev(
        31_400,
        r#""type":"approval","request":"r1","decision":"auto","mode":"allowlist","reason":"coincide con «cargo doc *» del perfil""#,
    );
    three += &ev(
        32_000,
        r#""type":"tool_start","id":"c2","kind":"command","command":"cargo doc --no-deps -p auth""#,
    );
    three += &ev(
        44_000,
        &format!(
            r#""type":"tool_end","id":"c2","exit_code":101,"duration_ms":12000,"output_tail":"{}""#,
            esc(
                "   Compiling auth v0.4.1\nerror[E0425]: cannot find value `state` in this scope\n  --> src/auth/routes.rs:51:23\n   |\n51 |     let s = verify(state, token=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789);\n   |                    ^^^^^ not found in this scope\nerror: could not document `auth`"
            )
        ),
    );
    three += &ev(
        50_000,
        r#""type":"approval","request":"r2","decision":"blocked","reason":"rm -rf está en la lista de bloqueo","command":"rm -rf target""#,
    );
    three += &ev(
        52_000,
        r#""type":"file_edit","path":"src/auth/routes.rs","added":3,"removed":1"#,
    );
    three += &ev(
        58_000,
        &format!(
            r#""type":"message_delta","text":"{}""#,
            esc("Corregido el parámetro. Lanzo las pruebas para validar los ejemplos…")
        ),
    );
    three += &ev(
        60_000,
        r#""type":"tool_start","id":"c3","kind":"command","command":"cargo test -p auth -- --nocapture""#,
    );
    std::fs::write(runs.join("3.events.jsonl"), three).expect("events 3");

    let mut four = String::new();
    four += &ev(
        3_000,
        &format!(
            r#""type":"message_delta","text":"{}""#,
            esc("Reviso cómo se valida el parámetro state en el callback.")
        ),
    );
    four += &ev(
        20_000,
        r#""type":"tool_start","id":"c1","kind":"command","command":"rg -n state src/auth""#,
    );
    four += &ev(
        20_300,
        r#""type":"tool_end","id":"c1","exit_code":0,"duration_ms":300,"output_tail":"src/auth/callback.rs:18: let state = params.state;""#,
    );
    four += &ev(
        40_000,
        r#""type":"approval","request":"q1","decision":"pending","command":"curl -s https://idp.example.com/.well-known/openid-configuration""#,
    );
    std::fs::write(runs.join("4.events.jsonl"), four).expect("events 4");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") {
        Mode::Light
    } else {
        Mode::Dark
    };
    let map_mode = match arg_value(&args, "--mode") {
        Some("graph") => MapMode::Graph,
        Some("timeline") => MapMode::Timeline,
        _ => MapMode::Tree,
    };
    let select: Option<i64> = arg_value(&args, "--select").and_then(|s| s.parse().ok());
    let zone = match arg_value(&args, "--zone") {
        Some("right") => PanelZone::Right,
        _ => PanelZone::Main,
    };
    let external = args.iter().any(|a| a == "--external");
    let many = args.iter().any(|a| a == "--many");
    let options = args.iter().any(|a| a == "--options");
    let group = match arg_value(&args, "--group") {
        Some("agent") => GroupBy::Agent,
        Some("status") => GroupBy::Status,
        Some("project") => GroupBy::Project,
        _ => GroupBy::Parent,
    };
    let sort = match arg_value(&args, "--sort") {
        Some("recent") => SortBy::Recent,
        Some("duration") => SortBy::Duration,
        _ => SortBy::Start,
    };
    let filter = match arg_value(&args, "--filter") {
        Some("running") => StatusFilter::Running,
        Some("failed") => StatusFilter::Failed,
        Some("done") => StatusFilter::Done,
        Some("external") => StatusFilter::External,
        _ => StatusFilter::All,
    };
    let query = arg_value(&args, "--query").unwrap_or("").to_string();
    let hide_older: Option<u32> = arg_value(&args, "--hide-older").and_then(|s| s.parse().ok());
    let activity = args.iter().any(|a| a == "--activity");
    let live = args.iter().any(|a| a == "--live");
    let tab = match arg_value(&args, "--tab") {
        Some("activity") => InspectorTab::Activity,
        Some("prompt") => InspectorTab::Prompt,
        _ => InspectorTab::Summary,
    };
    let select_external: Option<u32> =
        arg_value(&args, "--select-external").and_then(|s| s.parse().ok());
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or((1180.0, 700.0));

    let dir = tempfile::tempdir().expect("temp dir");
    let (store, runs) = build_fake_data(dir.path(), many);
    eprintln!("fake data in {}", dir.path().display());
    if activity {
        write_fake_events(&runs);
    }
    if live {
        // Push check: a line every 1.5 s into the log of delegation 3 (the OS watcher wakes the map).
        let path = runs.join("3.events.jsonl");
        std::thread::spawn(move || {
            use std::io::Write;
            for i in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let _ = writeln!(
                        f,
                        r#"{{"t":{},"type":"message_delta","text":" paso {i}"}}"#,
                        now_ms()
                    );
                }
            }
        });
    }

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
                eprintln!("fonts: {e}");
            }
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let theme = Theme::for_mode(mode);
            let source = Arc::new(ChatStoreSource::new(store));
            let model: Entity<AgentMapModel> = cx.new(|_| AgentMapModel::new(source, runs));
            cx.subscribe(&model, |_, ev: &AgentMapEvent, _| println!("event: {ev:?}"))
                .detach();
            let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
            let keep = Some(dir);
            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |window, cx| {
                    let provider = AgentMapPanelProvider::new(model.clone());
                    let ctx = PanelCtx::new(zone, WidthClass::from_width(w), theme);
                    let panel = provider.build(ctx, window, cx);
                    if let Ok(v) = panel.clone().downcast::<AgentMapView>() {
                        v.update(cx, |v, cx| {
                            v.set_mode(map_mode, cx);
                            v.set_options_open(options, cx);
                            v.set_search_text(&query, cx);
                        });
                    }
                    model.update(cx, |m, cx| {
                        m.update_prefs(
                            |p| {
                                p.group = group;
                                p.sort = sort;
                                p.filter = filter;
                            },
                            cx,
                        );
                        m.update_session(|s| s.hide_older_min = hide_older, cx);
                    });
                    model.update(cx, |m, cx| {
                        m.set_inspector_tab(tab, cx);
                        if external {
                            let now = now_ms();
                            let agy = |pid: u32, ago_min: i64, cmd: &str, ended: Option<i64>| {
                                ExternalProc {
                                    agent: "agy".into(),
                                    pid,
                                    cmdline_redacted: cmd.into(),
                                    cwd: Some("C:/Users/demo/proyecto".into()),
                                    model: Some("gemini-3-pro".into()),
                                    effort: Some("medium".into()),
                                    started_ms: now - ago_min * 60_000,
                                    ended_ms: ended.map(|m| now - m * 60_000),
                                    exit_code: ended.map(|_| 0),
                                    cpu_pct: ended.is_none().then_some(3.4),
                                    rss_bytes: if ended.is_some() { 0 } else { 214 << 20 },
                                }
                            };
                            m.set_external(
                                NodeKey::Thread(ThreadId(1)),
                                vec![
                                    agy(
                                        4120,
                                        6,
                                        "agy -p \"revisa los tests de auth\" --effort medium",
                                        None,
                                    ),
                                    agy(
                                        4188,
                                        5,
                                        "agy -p \"actualiza la documentación\" --effort medium",
                                        None,
                                    ),
                                    agy(
                                        3980,
                                        14,
                                        "agy -p \"lista los TODO\" --effort low",
                                        Some(9),
                                    ),
                                ],
                                cx,
                            );
                        }
                        if let Some(id) = select {
                            m.open_run(ThreadId(id), cx);
                        }
                        // The external nodes exist once the first forest load finished: select and
                        // feed their children then (a load that ends before would drop them).
                        let model2 = model.clone();
                        cx.spawn(async move |_, cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(1200))
                                .await;
                            let _ = model2.update(cx, |m, cx| {
                            if let Some(pid) = select_external
                                && let Some(key) = m
                                    .external_procs(&NodeKey::Thread(ThreadId(1)))
                                    .iter()
                                    .find(|e| e.pid == pid)
                                    .map(ExternalProc::key)
                            {
                                m.select(Some(key), cx);
                            }
                            if external && activity {
                                // FAKE child processes of the first external agent.
                                let now = now_ms();
                                let kid = |pid: u32,
                                           ago_s: i64,
                                           cmd: &str,
                                           end_s: Option<i64>,
                                           code: Option<i32>| {
                                    ExternalChild {
                                        pid,
                                        cmdline_redacted: cmd.into(),
                                        started_ms: now - ago_s * 1000,
                                        ended_ms: end_s.map(|e| now - e * 1000),
                                        exit_code: code,
                                        cpu_pct: end_s.is_none().then_some(38.5),
                                        rss_bytes: if end_s.is_some() { 0 } else { 612 << 20 },
                                    }
                                };
                                let agent = m
                                    .external_procs(&NodeKey::Thread(ThreadId(1)))
                                    .iter()
                                    .find(|e| e.pid == 4120)
                                    .map(ExternalProc::key);
                                if let Some(agent) = agent {
                                    m.set_external_children(
                                        agent,
                                        vec![
                                            kid(
                                                5101,
                                                280,
                                                "git status --porcelain",
                                                Some(279),
                                                Some(0),
                                            ),
                                            kid(
                                                5140,
                                                200,
                                                "rg -n \"state\" src/auth",
                                                Some(199),
                                                Some(0),
                                            ),
                                            kid(
                                                5188,
                                                95,
                                                "cargo test -p auth --no-run",
                                                Some(60),
                                                Some(101),
                                            ),
                                            kid(
                                                5230,
                                                41,
                                                "cargo build -p auth --release --features oauth,tls",
                                                None,
                                                None,
                                            ),
                                        ],
                                        cx,
                                    );
                                }
                            }
                            });
                        })
                        .detach();
                    });
                    cx.new(move |_| Host {
                        theme,
                        panel,
                        _keep: keep,
                    })
                },
            );
            cx.activate(true);
        });
}
