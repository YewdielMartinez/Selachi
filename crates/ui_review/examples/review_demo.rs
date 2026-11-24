//! The diff review on a throwaway repository, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_review --example review_demo --release -- [dark|light] [options]
//!
//!   --repo PATH      review this repository's worktree instead of the demo one (read-only: notes only)
//!   --file NAME      select the file whose path contains NAME (default: src/session.rs)
//!   --editor         open the note editor under a line, with some text typed
//!   --bare           no pre-seeded notes
//!   --size WxH       window size (default 1280x820)
//! ```
//!
//! The demo repository has several modified files, an untracked one and a deleted one, and a few
//! notes in each state. "Enviar al agente" prints the Markdown to the console and marks the notes
//! as sent, like a host would.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, Application, Bounds, Context, Entity, IntoElement, Render, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use panel_api::TabModel;
use theme::{Mode, Theme};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};
use ui_review::{
    DiffRow, NoteId, NoteState, ReviewEvent, ReviewSource, ReviewTab, ReviewView, Side,
};

struct Host {
    theme: Theme,
    tab: Entity<ReviewTab>,
    log: String,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let content = self.tab.read(cx).view().clone();
        let subtitle = self
            .tab
            .read(cx)
            .subtitle(cx)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(Pill::new("t0", &t).icon(Icon::Terminal).label("claude"))
            .child(
                Pill::new("t1", &t)
                    .icon(Icon::GitBranch)
                    .label("Revisión")
                    .active(true),
            )
            .child(
                div()
                    .text_size(px(t.typography.ui_size_sm))
                    .text_color(c.muted)
                    .child(subtitle),
            );
        let status = div()
            .h(px(28.0))
            .flex_none()
            .px(px(16.0))
            .flex()
            .items_center()
            .text_size(px(t.typography.ui_size_sm))
            .text_color(c.muted)
            .child(self.log.clone());
        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(
                div().flex_1().min_h_0().px(px(8.0)).child(
                    Card::new(&t)
                        .tone(CardTone::Base, &t)
                        .size_full()
                        .overflow_hidden()
                        .child(content),
                ),
            )
            .child(status)
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=Ana Ríos",
        "-c",
        "user.email=ana@example.com",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(dir)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.status();
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, text);
}

const SESSION_OLD: &str = r#"use std::time::Duration;

pub struct Session {
    pub user: String,
    pub token: String,
    pub ttl: Duration,
}

impl Session {
    pub fn new(user: &str, token: &str) -> Self {
        Session { user: user.to_string(), token: token.to_string(), ttl: Duration::from_secs(3600) }
    }

    pub fn is_expired(&self, age: Duration) -> bool {
        age > self.ttl
    }

    pub fn refresh(&mut self) {
        self.ttl = Duration::from_secs(3600);
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.user, self.token)
    }
}

pub fn load(path: &str) -> Session {
    let text = std::fs::read_to_string(path).unwrap();
    let mut parts = text.split(':');
    Session::new(parts.next().unwrap(), parts.next().unwrap())
}
"#;

const SESSION_NEW: &str = r#"use std::path::Path;
use std::time::Duration;

/// Seconds a session lives without activity.
const DEFAULT_TTL_SECS: u64 = 900;

#[derive(Debug)]
pub struct Session {
    pub user: String,
    pub token: String,
    pub ttl: Duration,
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Malformed,
}

impl Session {
    pub fn new(user: &str, token: &str) -> Self {
        Session { user: user.to_string(), token: token.to_string(), ttl: Duration::from_secs(DEFAULT_TTL_SECS) }
    }

    pub fn is_expired(&self, age: Duration) -> bool {
        age >= self.ttl
    }

    pub fn refresh(&mut self) {
        self.ttl = Duration::from_secs(DEFAULT_TTL_SECS);
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.user, self.token)
    }
}

pub fn load(path: &Path) -> Result<Session, LoadError> {
    let text = std::fs::read_to_string(path).map_err(LoadError::Io)?;
    let mut parts = text.trim().split(':');
    let user = parts.next().ok_or(LoadError::Malformed)?;
    let token = parts.next().ok_or(LoadError::Malformed)?;
    Ok(Session::new(user, token))
}
"#;

const MAIN_OLD: &str = "mod session;\n\nfn main() {\n    let s = session::load(\"session.txt\");\n    println!(\"{}\", s.describe());\n}\n";
const MAIN_NEW: &str = "mod session;\n\nuse std::path::Path;\n\nfn main() {\n    match session::load(Path::new(\"session.txt\")) {\n        Ok(s) => println!(\"{}\", s.describe()),\n        Err(e) => eprintln!(\"no se pudo cargar la sesión: {e:?}\"),\n    }\n}\n";

const CONFIG_OLD: &str = "{\n  \"name\": \"selachi-demo\",\n  \"retries\": 3,\n  \"timeout_ms\": 5000,\n  \"debug\": false\n}\n";
const CONFIG_NEW: &str = "{\n  \"name\": \"selachi-demo\",\n  \"retries\": 5,\n  \"timeout_ms\": 5000,\n  \"debug\": true,\n  \"api_key\": \"sk-demo1234567890abcdefghijklmnop\"\n}\n";

const CARGO_OLD: &str = "[package]\nname = \"selachi-demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1\"\n";
const CARGO_NEW: &str = "[package]\nname = \"selachi-demo\"\nversion = \"0.2.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1\"\nthiserror = \"2\"\n";

const NOTAS_NEW: &str = "# Notas de sesión\n\nLa sesión caduca a los 15 minutos sin actividad.\n\n- `refresh()` reinicia el contador.\n- `load()` ahora devuelve `Result`.\n";

const DEPLOY_OLD: &str = "param([string]$Target = 'staging')\nWrite-Host \"Desplegando a $Target\"\n.\\build.ps1\n.\\push.ps1 -Target $Target\n";

/// A throwaway repository with modified, added and deleted files.
fn make_demo_repo() -> PathBuf {
    let root = std::env::temp_dir().join("selachi-review-demo");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    git(&root, &["init", "-q", "-b", "main"]);
    write(&root, "src/session.rs", SESSION_OLD);
    write(&root, "src/main.rs", MAIN_OLD);
    write(&root, "config.json", CONFIG_OLD);
    write(&root, "Cargo.toml", CARGO_OLD);
    write(&root, "scripts/deploy.ps1", DEPLOY_OLD);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "estado inicial"]);
    write(&root, "src/session.rs", SESSION_NEW);
    write(&root, "src/main.rs", MAIN_NEW);
    write(&root, "config.json", CONFIG_NEW);
    write(&root, "Cargo.toml", CARGO_NEW);
    write(&root, "docs/NOTAS.md", NOTAS_NEW);
    let _ = std::fs::remove_file(root.join("scripts/deploy.ps1"));
    root
}

/// The line of `path` (in the loaded diff) whose text contains `needle`, with its side.
fn find_line(view: &ReviewView, path: &str, needle: &str, removed: bool) -> Option<(Side, usize)> {
    let file = view.files().iter().find(|f| f.path == path)?;
    file.rows.iter().find_map(|r| match r {
        DiffRow::Line(l) if l.text.contains(needle) => {
            if removed {
                l.old_line
                    .filter(|_| l.new_line.is_none())
                    .map(|n| (Side::Old, n))
            } else {
                l.new_line.map(|n| (Side::New, n))
            }
        }
        _ => None,
    })
}

fn seed(view: &Entity<ReviewView>, cx: &mut App) {
    let picks: [(&str, &str, bool, usize, &str, NoteState); 5] = [
        (
            "src/session.rs",
            "DEFAULT_TTL_SECS: u64",
            false,
            0,
            "Mejor leer esto de la configuración que dejarlo fijo.",
            NoteState::Draft,
        ),
        (
            "src/session.rs",
            "age >= self.ttl",
            false,
            0,
            "El borde cambió de `>` a `>=`: ¿está cubierto por un test?",
            NoteState::Draft,
        ),
        (
            "src/session.rs",
            "let text = std::fs::read_to_string(path).unwrap();",
            true,
            0,
            "Bien que se quite el `unwrap()`, pero conserva el contexto del error.",
            NoteState::Sent,
        ),
        (
            "src/session.rs",
            "let user = parts.next()",
            false,
            2,
            "Estas dos líneas se repiten: extráelas a una función `field(&mut parts)`.",
            NoteState::Sent,
        ),
        (
            "src/session.rs",
            "Duration::from_secs(DEFAULT_TTL_SECS) }",
            false,
            0,
            "Usa `Duration::from_secs` solo en un sitio.",
            NoteState::Resolved,
        ),
    ];
    for (path, needle, removed, extra, text, state) in picks {
        let at = find_line(view.read(cx), path, needle, removed);
        if let Some((side, line)) = at {
            let id: Option<NoteId> = view.update(cx, |v, cx| {
                v.add_note(path, side, line, line + extra, text, cx)
            });
            if let Some(id) = id {
                match state {
                    NoteState::Sent => view.update(cx, |v, cx| v.mark_sent(&[id], cx)),
                    NoteState::Resolved => view.update(cx, |v, cx| v.mark_resolved(&[id], cx)),
                    NoteState::Draft => {}
                }
            }
        }
    }
    // One note on another file.
    let cfg = find_line(view.read(cx), "config.json", "api_key", false);
    if let Some((side, line)) = cfg {
        view.update(cx, |v, cx| {
            v.add_note(
                "config.json",
                side,
                line,
                line,
                "No subas la clave al repositorio.",
                cx,
            )
        });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let mode = if flag("light") {
        Mode::Light
    } else {
        Mode::Dark
    };
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280.0f32, 820.0f32));
    let demo = arg_value(&args, "--repo").is_none();
    let root = arg_value(&args, "--repo").map_or_else(make_demo_repo, PathBuf::from);
    let want_file = arg_value(&args, "--file")
        .unwrap_or("src/session.rs")
        .to_owned();
    let editor = flag("--editor");
    let bare = flag("--bare");
    eprintln!("review_demo: {}", root.display());

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
        ui_review::init(cx);
        let theme = Theme::for_mode(mode);
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let source = ReviewSource::Worktree { root: root.clone() };
                let tab = cx.new(|cx| ReviewTab::new(source, theme, cx));
                let host = cx.new(|_| Host { theme, tab: tab.clone(), log: "Host: esperando eventos de la revisión".to_string() });

                // What a host does with the events.
                let log = host.clone();
                let tab_for_events = tab.clone();
                cx.subscribe(&tab, move |_, ev: &ReviewEvent, cx| match ev {
                    ReviewEvent::SendToAgent { markdown, notes } => {
                        println!("---- SendToAgent ({} notas) ----\n{markdown}", notes.len());
                        let ids: Vec<NoteId> = notes.iter().map(|n| n.id).collect();
                        tab_for_events.update(cx, |t, cx| t.mark_sent(&ids, cx));
                        log.update(cx, |h, cx| {
                            h.log = format!("Host: enviadas {} notas al agente ({} caracteres de Markdown)", notes.len(), markdown.len());
                            cx.notify();
                        });
                    }
                    ReviewEvent::NotesChanged { total, drafts } => {
                        log.update(cx, |h, cx| {
                            h.log = format!("Host: {total} notas, {drafts} en borrador");
                            cx.notify();
                        });
                    }
                })
                .detach();

                // Scripted state for screenshots.
                let handle: AnyWindowHandle = window.window_handle();
                let view = tab.read(cx).view().clone();
                let want = want_file.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_millis(1800)).await;
                    let _ = cx.update(|cx| {
                        let full = view.read(cx).files().iter().find(|f| f.path.contains(&want)).map(|f| f.path.clone());
                        if let Some(full) = full {
                            view.update(cx, |v, cx| v.select_path(&full, cx));
                        }
                        if demo && !bare {
                            seed(&view, cx);
                        }
                        if editor {
                            let at = find_line(view.read(cx), &want, "Malformed,", false).or_else(|| find_line(view.read(cx), &want, "retries", false));
                            if let Some((side, line)) = at {
                                let _ = handle.update(cx, |_, window, cx| {
                                    view.update(cx, |v, cx| {
                                        v.begin_note(side, line, false, window, cx);
                                        v.set_editor_text("¿Por qué `Malformed` no lleva el texto que no se pudo leer?\nAsí el mensaje de error sería más útil.", cx);
                                    });
                                });
                            }
                        }
                    });
                })
                .detach();
                host
            },
        );
        cx.activate(true);
    });
}
