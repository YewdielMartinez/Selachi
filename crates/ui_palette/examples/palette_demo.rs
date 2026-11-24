//! The command palette in a window with a fake app behind it: fake tabs and actions, the real
//! `store` (a seeded temporary state.db) and the real `tasks_svc` over this repository.
//!
//! ```text
//! cargo run -p ui_palette --example palette_demo --release -- [dark|light|oled]
//!     [--query "> nueva"] [--keys down,down,enter] [--size WxH] [--closed] [--bench]
//! ```
//!
//! `Ctrl+K` toggles the palette. Every emitted [`PaletteEvent`] is printed to stdout and shown in
//! the fake terminal. `--keys` dispatches keystrokes after the window opened (for screenshots).
//! `--bench` opens and closes the palette with a few queries and prints the timings.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    AnyView, App, Application, AsyncApp, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, Keystroke, Render, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, size,
};
use panel_api::{TabEvent, TabHandle, TabIcon, TabModel};
use store::{NewCommand, NewSnippet, StateDb};
use theme::{Mode, Theme};
use ui_kit::{Assets, Card, CardTone, Icon, Logo, Pill, RowStatus};
use ui_palette::{
    ActionEntry, CommandPalette, OpenContext, PaletteEvent, TogglePalette, bind_toggle_key,
    command_palette, init, standard_sources,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_default()
}

/// Strips the `\\?\` prefix `canonicalize` adds on Windows.
fn plain(p: PathBuf) -> PathBuf {
    PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
}

// ---- a fake tab -----------------------------------------------------------------------------

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct DemoTab {
    title: SharedString,
    subtitle: Option<SharedString>,
    icon: TabIcon,
    status: Option<RowStatus>,
    cwd: PathBuf,
    body: AnyView,
}

impl EventEmitter<TabEvent> for DemoTab {}

impl TabModel for DemoTab {
    fn title(&self, _: &App) -> SharedString {
        self.title.clone()
    }
    fn icon(&self, _: &App) -> TabIcon {
        self.icon.clone()
    }
    fn subtitle(&self, _: &App) -> Option<SharedString> {
        self.subtitle.clone()
    }
    fn status(&self, _: &App) -> Option<RowStatus> {
        self.status
    }
    fn cwd(&self, _: &App) -> Option<PathBuf> {
        Some(self.cwd.clone())
    }
    fn content(&self, _: &App) -> AnyView {
        self.body.clone()
    }
}

fn demo_tabs(cx: &mut App, root: &Path) -> Vec<TabHandle> {
    let mk = |cx: &mut App, title: &str, sub: Option<&str>, icon: TabIcon, status, cwd: PathBuf| {
        let body: AnyView = cx.new(|_| Blank).into();
        TabHandle::new(cx.new(|_| DemoTab {
            title: title.to_string().into(),
            subtitle: sub.map(|s| s.to_string().into()),
            icon,
            status,
            cwd,
            body,
        }))
    };
    vec![
        mk(
            cx,
            "selachi",
            Some("main"),
            TabIcon::Kind(Icon::Terminal),
            None,
            root.to_path_buf(),
        ),
        mk(
            cx,
            "claude",
            Some("opus-5.5 · feat/palette"),
            TabIcon::Agent("claude".into()),
            Some(RowStatus::Working),
            root.to_path_buf(),
        ),
        mk(
            cx,
            "api",
            Some("dotnet watch"),
            TabIcon::Kind(Icon::Terminal),
            Some(RowStatus::Done),
            root.join("crates"),
        ),
        mk(
            cx,
            "codex",
            Some("esperando aprobación"),
            TabIcon::Agent("codex".into()),
            Some(RowStatus::Waiting),
            root.to_path_buf(),
        ),
    ]
}

// ---- the seeded store -----------------------------------------------------------------------

fn seed(root: &Path) -> (PathBuf, StateDb) {
    let dir = root.join("target").join("palette-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let db = match StateDb::open(&dir.join("state.db")) {
        Ok(db) => db,
        Err(e) => panic!("state db: {e}"),
    };
    let cwd = root.to_string_lossy().into_owned();
    for (cmd, code) in [
        ("cargo test -p ui_palette", 0),
        ("cargo clippy --workspace --all-targets -- -D warnings", 0),
        ("cargo run -p ui_layout --example standalone --release", 0),
        ("git status", 0),
        ("git log --oneline -n 20", 0),
        ("git push origin main", 1),
        ("dotnet ef migrations add AgregarUsuarios", 0),
        ("npm run dev", 0),
        ("docker compose up -d", 0),
        ("cargo build --release", 0),
        ("cargo test -p store", 0),
    ] {
        let mut c = NewCommand::new(cmd, cwd.clone());
        c.exit_code = Some(code);
        let _ = db.record_command(c);
    }
    // A couple of fake projects with marker files, next to the db.
    for (name, marker, body) in [
        (
            "api-gateway",
            "Cargo.toml",
            "[package]\nname = \"api-gateway\"\n",
        ),
        (
            "tienda-web",
            "package.json",
            "{\"scripts\":{\"dev\":\"vite\"}}",
        ),
        (
            "facturacion",
            "Facturacion.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"/>",
        ),
    ] {
        let p = dir.join("projects").join(name);
        let _ = std::fs::create_dir_all(&p);
        let _ = std::fs::write(p.join(marker), body);
        let _ = db.mark_project_opened(&p.to_string_lossy());
    }
    let _ = db.mark_project_opened(&cwd);
    let _ = db.add_snippet(NewSnippet::new(
        "Compilar",
        "dotnet build -c {cfg|Debug|Release} -v {v:minimal}",
    ));
    let _ = db.add_snippet(NewSnippet::new("Estado corto", "git status -sb"));
    let _ = db.flush();
    (dir, db)
}

fn actions() -> Vec<ActionEntry> {
    vec![
        ActionEntry::new("tab.new", "Nueva pestaña")
            .shortcut("Ctrl+T")
            .icon(Icon::Plus)
            .keywords("crear terminal"),
        ActionEntry::new("tab.close", "Cerrar pestaña")
            .shortcut("Ctrl+W")
            .icon(Icon::Close),
        ActionEntry::new("layout.toggle", "Abrir panel de diseño")
            .shortcut("Ctrl+Shift+L")
            .icon(Icon::Layout),
        ActionEntry::new("theme.toggle", "Alternar tema claro / oscuro")
            .shortcut("Ctrl+Shift+T")
            .icon(Icon::Settings),
        ActionEntry::new("files.open", "Mostrar archivos").icon(Icon::Files),
        ActionEntry::new("agents.new", "Nuevo chat con agente")
            .shortcut("Ctrl+Shift+N")
            .icon(Icon::Agents),
        ActionEntry::new("history.open", "Historial de comandos").icon(Icon::History),
        ActionEntry::new("settings.open", "Ajustes")
            .shortcut("Ctrl+,")
            .icon(Icon::Settings),
    ]
}

// ---- the host -------------------------------------------------------------------------------

struct Host {
    theme: Theme,
    focus: FocusHandle,
    palette: Entity<CommandPalette>,
    tabs: Vec<TabHandle>,
    root: PathBuf,
    last_event: Option<String>,
}

impl Focusable for Host {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Host {
    fn open_context(&self, cx: &App, initial: &str) -> OpenContext {
        OpenContext {
            cwd: Some(self.root.clone()),
            project_root: Some(self.root.clone()),
            tabs: self.tabs.iter().map(|t| t.info(cx)).collect(),
            active_tab: self.tabs.first().map(|t| t.id()),
            initial: initial.to_string(),
        }
    }
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let infos: Vec<_> = self.tabs.iter().map(|h| h.info(cx)).collect();
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(c.muted)
                    .child("Selachi › Terminal › ~/Desktop/Terminal"),
            )
            .children(infos.iter().enumerate().map(|(i, info)| {
                Pill::new(format!("tab-{i}"), &t)
                    .icon(info.icon.icon())
                    .label(info.title.clone())
                    .active(i == 0)
            }));
        let lines = [
            (
                "~/Desktop/Terminal",
                "cargo run -p ui_palette --example palette_demo --release".to_string(),
            ),
            (
                "",
                "    Finished `release` profile [optimized] target(s) in 3.84s".to_string(),
            ),
            (
                "",
                "     Running `target/release/examples/palette_demo.exe`".to_string(),
            ),
            ("", "Pulsa Ctrl+K para abrir la paleta.".to_string()),
        ];
        let last = self.last_event.clone();
        let term = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .font_family(t.typography.mono_family)
            .text_size(px(t.typography.mono_size))
            .children(lines.into_iter().map(|(prompt, text)| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.0))
                    .child(div().text_color(c.accent).child(prompt))
                    .child(div().text_color(c.text).child(text))
            }))
            .when_some(last, |d, e| {
                d.child(
                    div()
                        .mt(px(8.0))
                        .text_color(c.muted)
                        .child(format!("evento: {e}")),
                )
            });

        div()
            .id("host")
            .key_context("Host")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| {
                let ctx = this.open_context(cx, "");
                this.palette.update(cx, |p, cx| p.toggle(ctx, window, cx));
            }))
            .relative()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(div().flex_1().px(px(8.0)).pb(px(8.0)).flex().child(term))
            // The palette: the last child of the root, an absolute overlay.
            .child(self.palette.clone())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") {
        Mode::Light
    } else if args.iter().any(|a| a == "oled") {
        Mode::Oled
    } else {
        Mode::Dark
    };
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let initial = value("--query").unwrap_or_default();
    let keys: Vec<String> = value("--keys")
        .map(|k| k.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let closed = args.iter().any(|a| a == "--closed");
    let bench = args.iter().any(|a| a == "--bench");
    let (w, h) = value("--size")
        .and_then(|s| {
            s.split_once('x')
                .map(|(w, h)| (w.to_string(), h.to_string()))
        })
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or((1100.0, 760.0));

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
                eprintln!("fonts: {e}");
            }
            init(cx);
            bind_toggle_key(cx);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let theme = Theme::for_mode(mode);
            let root = plain(repo_root());
            let (_dir, db) = seed(&root);
            let tabs = demo_tabs(cx, &root);
            let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
            let initial_text = initial.clone();
            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |window, cx| {
                    let sources =
                        standard_sources(Some(db.clone()), actions(), vec!["Mi flujo".into()]);
                    let palette = command_palette(theme, Some(db), sources, cx);
                    let host = cx.new(|cx| Host {
                        theme,
                        focus: cx.focus_handle(),
                        palette: palette.clone(),
                        tabs,
                        root,
                        last_event: None,
                    });
                    cx.subscribe(&palette, {
                        let host = host.clone();
                        move |_, ev: &PaletteEvent, cx| {
                            println!("PaletteEvent: {ev:?}");
                            if !matches!(ev, PaletteEvent::Closed) {
                                host.update(cx, |h, cx| {
                                    h.last_event = Some(format!("{ev:?}"));
                                    cx.notify();
                                });
                            }
                        }
                    })
                    .detach();
                    window.focus(&host.read(cx).focus);
                    if !closed {
                        let ctx = host.read(cx).open_context(cx, &initial_text);
                        palette.update(cx, |p, cx| p.open(ctx, window, cx));
                    }
                    if !keys.is_empty() {
                        let handle = window.window_handle();
                        cx.spawn(async move |cx: &mut AsyncApp| {
                            cx.background_executor()
                                .timer(Duration::from_millis(700))
                                .await;
                            for k in keys {
                                let _ = handle.update(cx, |_, window, cx| {
                                    if let Ok(ks) = Keystroke::parse(&k) {
                                        window.dispatch_keystroke(ks, cx);
                                    }
                                });
                                cx.background_executor()
                                    .timer(Duration::from_millis(350))
                                    .await;
                            }
                        })
                        .detach();
                    }
                    if bench {
                        run_bench(window, host.clone(), palette, cx);
                    }
                    host
                },
            );
            cx.activate(true);
        });
}

/// Types a few queries into the palette and prints how long each source took to answer.
fn run_bench(
    window: &mut Window,
    host: Entity<Host>,
    palette: Entity<CommandPalette>,
    cx: &mut App,
) {
    let handle = window.window_handle();
    let _ = host;
    cx.spawn(async move |cx: &mut AsyncApp| {
        for round in 0..3 {
            for raw in ["", ">", "nueva", "#cargo", "!build", "$mig", "@api", "git"] {
                let _ = handle.update(cx, |_, window, cx| {
                    palette.update(cx, |p, cx| {
                        if p.is_open() {
                            p.close(window, cx);
                        }
                        let ctx = OpenContext {
                            cwd: Some(plain(repo_root())),
                            project_root: Some(plain(repo_root())),
                            initial: raw.to_string(),
                            ..OpenContext::default()
                        };
                        p.open(ctx, window, cx);
                    });
                });
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
                let _ = handle.update(cx, |_, _, cx| {
                    let t = palette.read(cx).timings().clone();
                    println!(
                        "round {round} query {raw:?}: first {:?}, per source {:?}",
                        t.first_results, t.per_source
                    );
                });
            }
        }
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}
