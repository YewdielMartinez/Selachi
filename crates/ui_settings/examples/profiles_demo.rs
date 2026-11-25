//! Ajustes -> Agentes -> Perfiles over a temp data folder (never the real `%APPDATA%`): the four
//! starter templates as global profiles and one project profile that attaches a context file.
//!
//! ```text
//! cargo run -p ui_settings --example profiles_demo -- [dark|light] [--scroll <px>] [--dry-run]
//!     [--open "<profile name>"] [--size WxH]
//! ```

#![allow(clippy::expect_used)]

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use theme::{Mode, Theme};
use ui_kit::{Assets, Card, CardTone};
use ui_settings::{
    AgentsTab, Profile, SettingsOptions, SettingsView, profile_templates, profile_to_toml,
    settings_view,
};

struct Host {
    theme: Theme,
    view: Entity<SettingsView>,
    _dir: tempfile::TempDir,
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
                    .tone(CardTone::Base, &t)
                    .size_full()
                    .overflow_hidden()
                    .child(self.view.clone()),
            )
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") {
        Mode::Light
    } else {
        Mode::Dark
    };
    let scroll: f32 = arg_value(&args, "--scroll")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let dry = args.iter().any(|a| a == "--dry-run");
    let open = arg_value(&args, "--open")
        .unwrap_or("Pruebas de integración")
        .to_string();
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or((1180.0, 760.0));

    // fake data folder: global templates, one project profile, one project file to attach
    let dir = tempfile::tempdir().expect("temp dir");
    let data = dir.path().join("data");
    let project = dir.path().join("proyecto");
    std::fs::create_dir_all(data.join("agents")).expect("agents dir");
    std::fs::create_dir_all(project.join(".git")).expect("project");
    std::fs::create_dir_all(project.join("docs")).expect("docs");
    std::fs::create_dir_all(project.join(".selachi").join("agents")).expect("project agents");
    std::fs::write(
        project.join("docs").join("PLAN.md"),
        "# Plan\nObjetivo: migrar a OAuth.\n",
    )
    .expect("plan");
    std::fs::write(data.join("config.toml"), store::config::TEMPLATE).expect("config");
    for t in profile_templates() {
        let file = t.name.to_lowercase().replace(' ', "-").replace("é", "e");
        std::fs::write(
            data.join("agents").join(format!("{file}.toml")),
            profile_to_toml(&t),
        )
        .expect("template");
    }
    let mine = Profile {
        name: "Pruebas de integración".into(),
        description: "Escribe pruebas de integración para el flujo OAuth y las ejecuta.".into(),
        agent: "codex".into(),
        model: Some("gpt-5.1-codex".into()),
        effort: Some("high".into()),
        permission: "accept-edits".into(),
        worktree: true,
        timeout: Some(1200),
        budget_tokens: Some(300_000),
        instructions: "Escribe pruebas de integración para el flujo descrito en el plan.\nUsa el estilo de las pruebas existentes y no toques el código de producción.\nResume qué cubren y qué falta.".into(),
        allowed_tools: vec!["Read".into(), "Grep".into(), "Bash(cargo test:*)".into()],
        denied_tools: vec!["WebFetch".into()],
        context_files: vec!["docs/PLAN.md".into()],
        ..Profile::default()
    };
    std::fs::write(
        project
            .join(".selachi")
            .join("agents")
            .join("pruebas-de-integracion.toml"),
        profile_to_toml(&mine),
    )
    .expect("project profile");

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
            let config_path = data.join("config.toml");
            let mut config = store::Config::load_from(&config_path)
                .expect("config")
                .config;
            // Delegación lets edits through only up to `ask`: the form shows the clamp
            config.delegation.max_permission = "accept_edits".into();
            let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |_, cx| {
                    ui_settings::init(cx);
                    let view =
                        settings_view(config, theme, SettingsOptions::new(config_path, "demo"), cx);
                    view.update(cx, |v, cx| {
                        v.set_cwd(Some(project.clone()), cx);
                        v.set_agents_tab(AgentsTab::Profiles, cx);
                    });
                    let editor = view.update(cx, |v, cx| v.profile_editor(cx));
                    let view_host = view.clone();
                    let open2 = open.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(900))
                            .await;
                        let _ = editor.update(cx, |e, cx| {
                            e.open_profile(&open2, cx);
                            e.set_dry_run(dry, cx);
                        });
                        let _ = view.update(cx, |v, _| v.scroll_to(scroll));
                    })
                    .detach();
                    cx.new(move |_| Host {
                        theme,
                        view: view_host,
                        _dir: dir,
                    })
                },
            );
            cx.activate(true);
        });
}
