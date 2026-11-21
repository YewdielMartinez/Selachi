//! The git panel on a real or a demo repository, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_git --example git_demo --release -- [dark|light] [--zone right|sidebar|main]
//!     [--size WxH] [--repo PATH] [--demo] [--allow-write] [--tab changes|history|checkpoints]
//!     [--diff N] [--review commit|push|pull|restore|remove] [--worktrees] [--terminal-mode]
//! ```
//!
//! * No flags: opens THIS repository (or `--repo`) **read-only**: every mutation is refused.
//! * `--demo`: creates a throwaway repository (commits, a remote with ahead / behind, a worktree,
//!   staged / unstaged / untracked changes, two checkpoints) and enables writes in it.
//! * `--allow-write`: enables writes on the repository you pointed at. Think twice.
//! * `--diff N`: opens the diff of the N-th file row after the first read.
//! * `--review X`: asks for a sensitive operation so its confirmation card is on screen (it is
//!   never confirmed here).
//! * `--terminal-mode`: confirmed sensitive operations emit `RunInTerminal` instead of running.
//!
//! The host strip at the bottom prints every `GitPanelEvent` the panel emits.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use git_svc::Repo;
use gpui::{
    AnyView, App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use theme::{Mode, Theme};
use ui_git::{DiffTarget, ExecMode, GitModel, GitOp, GitPanel, GitPanelEvent, GitPanelProvider, Tab};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};

struct Host {
    theme: Theme,
    zone: PanelZone,
    panel: AnyView,
    panel_width: f32,
    events: Vec<String>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let panel = Card::new(&t).tone(CardTone::Raised, &t).flex_none().overflow_hidden().h_full().child(self.panel.clone());
        let log: String = if self.events.is_empty() {
            "PS C:\\Users\\venta\\Desktop\\Terminal> ".to_string()
        } else {
            self.events.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n")
        };
        let terminal = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .h_full()
            .p(px(16.0))
            .font_family(t.typography.mono_family)
            .text_size(px(t.typography.mono_size))
            .text_color(c.muted)
            .child(log);
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(Pill::new("t0", &t).icon(Icon::Terminal).label("crates").active(true));
        let row = div().flex_1().min_h_0().px(px(8.0)).pb(px(8.0)).flex().flex_row().gap(px(8.0));
        let body = match self.zone {
            PanelZone::Sidebar => row.child(panel.w(px(self.panel_width))).child(terminal),
            PanelZone::Main => row.child(panel.flex_1().w_full()),
            _ => row.child(terminal).child(panel.w(px(self.panel_width))),
        };
        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(body)
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn git(dir: &Path, args: &[&str], date: Option<&str>) -> bool {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "user.name=Ana Ríos", "-c", "user.email=ana@example.com", "-c", "commit.gpgsign=false", "-c", "protocol.file.allow=always"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(d) = date {
        cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.status().is_ok_and(|s| s.success())
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, text);
}

fn ago(secs: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{} +0000", now.saturating_sub(secs))
}

/// A throwaway repository with enough going on to show every part of the panel.
fn make_demo_repo() -> PathBuf {
    let root = std::env::temp_dir().join("selachi-git-demo");
    let _ = std::fs::remove_dir_all(&root);
    let main = root.join("selachi-app");
    let remote = root.join("origin.git");
    let _ = std::fs::create_dir_all(&main);
    git(&main, &["init", "-q", "-b", "main"], None);

    let day = 86_400;
    write(&main, "README.md", "# Selachi\n\nTerminal para agentes.\n");
    write(&main, "Cargo.toml", "[package]\nname = \"selachi\"\nversion = \"0.0.1\"\n\n[dependencies]\ngpui = \"0.2\"\n");
    write(&main, "src/main.rs", "fn main() {\n    println!(\"hola\");\n}\n");
    write(&main, "package.json", "{\n  \"name\": \"selachi-web\",\n  \"version\": \"0.1.0\",\n  \"scripts\": {\n    \"dev\": \"vite\"\n  }\n}\n");
    write(&main, "docs/old.md", "# Notas antiguas\n\nYa no hacen falta.\n");
    git(&main, &["add", "-A"], None);
    git(&main, &["commit", "-q", "-m", "chore: scaffold del workspace"], Some(&ago(21 * day)));

    write(&main, "config.toml", "[ui]\ntheme = \"dark\"\nfont_size = 13\n\n[git]\nwatch = true\ndebounce_ms = 300\n");
    git(&main, &["add", "-A"], None);
    git(&main, &["commit", "-q", "--author", "Luis Mora <luis@example.com>", "-m", "feat(config): añade config.toml con hot-reload"], Some(&ago(12 * day)));

    write(&main, "style.css", ":root {\n  --bg: #131316;\n  --accent: #00aaff;\n}\n\nbody {\n  background: var(--bg);\n  color: #e4e2e5;\n}\n");
    git(&main, &["add", "-A"], None);
    git(&main, &["commit", "-q", "-m", "style: tokens base del tema"], Some(&ago(5 * day)));

    write(&main, "src/main.rs", "fn main() {\n    let name = \"selachi\";\n    println!(\"hola, {name}\");\n}\n");
    git(&main, &["add", "-A"], None);
    git(&main, &["commit", "-q", "--author", "Luis Mora <luis@example.com>", "-m", "fix(parser): maneja entrada vacía sin entrar en pánico"], Some(&ago(2 * day)));

    // The remote and one commit that only the remote has (behind 1).
    git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"], None);
    git(&main, &["remote", "add", "origin", remote.to_string_lossy().as_ref()], None);
    git(&main, &["push", "-q", "-u", "origin", "main"], None);
    let other = root.join("other");
    git(&root, &["clone", "-q", remote.to_string_lossy().as_ref(), "other"], None);
    write(&other, "CHANGELOG.md", "# Cambios\n\n- v0.0.1\n");
    git(&other, &["add", "-A"], None);
    git(&other, &["commit", "-q", "--author", "Marta Gil <marta@example.com>", "-m", "docs: primer CHANGELOG"], Some(&ago(4 * 3600)));
    git(&other, &["push", "-q", "origin", "main"], None);

    // Two local commits the remote does not have (ahead 2).
    write(&main, "docs/checkpoints.md", "# Checkpoints\n\nGuardan el estado antes de que trabaje un agente.\n");
    git(&main, &["add", "-A"], None);
    git(&main, &["commit", "-q", "-m", "docs: describe los checkpoints"], Some(&ago(3 * 3600)));
    write(&main, "src/lib.rs", "pub fn version() -> &'static str {\n    \"0.0.1\"\n}\n");
    git(&main, &["add", "-A"], None);
    git(&main, &["commit", "-q", "-m", "feat: expone la versión"], Some(&ago(40 * 60)));
    git(&main, &["fetch", "-q"], None);

    // A linked worktree.
    let wt = root.join("selachi-app.worktrees").join("feat-search");
    git(&main, &["worktree", "add", "-q", "-b", "selachi/feat-search", wt.to_string_lossy().as_ref()], None);
    write(&wt, "search.txt", "wip\n");

    // Working changes, then checkpoints.
    write(&main, "config.toml", "[ui]\ntheme = \"dark\"\nfont_size = 14\n\n[git]\nwatch = true\ndebounce_ms = 250\nhistory = 30\n");
    write(&main, "style.css", ":root {\n  --bg: #131316;\n  --accent: #00aaff;\n  --accent-strong: #005eeb;\n}\n\nbody {\n  background: var(--bg);\n  color: #e4e2e5;\n}\n");
    git(&main, &["add", "config.toml", "style.css"], None);
    if let Ok(Some(repo)) = Repo::discover(&main) {
        let _ = repo.create_checkpoint("antes del refactor");
    }
    write(&main, "package.json", "{\n  \"name\": \"selachi-web\",\n  \"version\": \"0.2.0\",\n  \"scripts\": {\n    \"dev\": \"vite --host\",\n    \"build\": \"vite build\"\n  }\n}\n");
    write(&main, "src/main.rs", "fn main() {\n    let name = \"selachi\";\n    let version = lib::version();\n    println!(\"hola, {name} {version}\");\n}\n");
    write(&main, "README.md", "# Selachi\n\nTerminal para agentes de IA.\n\n## Uso\n\nAbre una pestaña y escribe.\n");
    let _ = std::fs::remove_file(main.join("docs/old.md"));
    if let Ok(Some(repo)) = Repo::discover(&main) {
        let _ = repo.create_checkpoint("tras los tests en verde");
    }
    write(&main, "src/search.rs", "pub fn search(q: &str) -> Vec<String> {\n    vec![q.to_string()]\n}\n");
    write(&main, "notes/todo.md", "- [ ] revisar el panel de git\n- [ ] probar worktrees\n");
    write(&main, "assets/logo.png", "\u{89}PNG fake binary");
    main
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") { Mode::Light } else { Mode::Dark };
    let zone = match arg_value(&args, "--zone") {
        Some("sidebar") => PanelZone::Sidebar,
        Some("main") => PanelZone::Main,
        _ => PanelZone::Right,
    };
    let panel_width = match zone {
        PanelZone::Sidebar => 264.0,
        PanelZone::Main => 900.0,
        _ => 420.0,
    };
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or(if zone == PanelZone::Main { (1000.0, 800.0) } else { (900.0, 800.0) });
    let demo = args.iter().any(|a| a == "--demo");
    let allow_write = demo || args.iter().any(|a| a == "--allow-write");
    let repo_dir: PathBuf = if demo {
        make_demo_repo()
    } else {
        arg_value(&args, "--repo").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    };
    let tab = match arg_value(&args, "--tab") {
        Some("history") => Some(Tab::History),
        Some("checkpoints") => Some(Tab::Checkpoints),
        Some("changes") => Some(Tab::Changes),
        _ => None,
    };
    let diff_ix: Option<usize> = arg_value(&args, "--diff").and_then(|s| s.parse().ok());
    let review = arg_value(&args, "--review").map(str::to_owned);
    let worktrees = args.iter().any(|a| a == "--worktrees");
    let terminal_mode = args.iter().any(|a| a == "--terminal-mode");
    eprintln!("git_demo: {} (writes {})", repo_dir.display(), if allow_write { "ON" } else { "off, read-only" });

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
        ui_git::init(cx);
        let theme = Theme::for_mode(mode);
        let model: Entity<GitModel> = cx.new(|_| GitModel::new());
        model.update(cx, |m, cx| {
            m.set_read_only(!allow_write, cx);
            if terminal_mode {
                m.set_exec_mode(ExecMode::Terminal);
            }
        });
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let provider = GitPanelProvider::new(model.clone());
                let ctx = PanelCtx {
                    zone,
                    width_class: WidthClass::from_width(panel_width),
                    cwd: Some(repo_dir.clone()),
                    project_root: Some(repo_dir.clone()),
                    theme,
                };
                let panel = provider.build(ctx, window, cx);
                let host = cx.new(|_| Host { theme, zone, panel: panel.clone(), panel_width, events: Vec::new() });
                let log = host.clone();
                cx.subscribe(&model, move |_, ev: &GitPanelEvent, cx| {
                    let line = format!("evento: {ev:?}");
                    eprintln!("{line}");
                    log.update(cx, |h, cx| {
                        h.events.push(line);
                        cx.notify();
                    });
                })
                .detach();

                // Scripted state for screenshots.
                let panel = panel.downcast::<GitPanel>().ok();
                let m = model.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_millis(3500)).await;
                    let _ = cx.update(|cx| {
                        if let Some(p) = &panel {
                            p.update(cx, |p, cx| {
                                if let Some(t) = tab {
                                    p.select_tab(t, cx);
                                }
                                if worktrees {
                                    p.set_worktree_menu(true, cx);
                                }
                            });
                        }
                        if let Some(ix) = diff_ix {
                            let target = m.read(cx).data().and_then(|d| {
                                ui_git::build_sections(&d.status, &d.counts)
                                    .into_iter()
                                    .flat_map(|(_, rows)| rows)
                                    .nth(ix)
                                    .map(|r| DiffTarget::Working { section: r.section, path: r.path })
                            });
                            if let Some(t) = target {
                                m.update(cx, |m, cx| m.toggle_diff(t, cx));
                            }
                        }
                        if let Some(kind) = &review {
                            let id = m.read(cx).data().and_then(|d| d.checkpoints.first().map(|c| c.id.clone()));
                            let wt = m.read(cx).data().and_then(|d| d.worktrees.iter().find(|w| !w.is_main).map(|w| w.path.clone()));
                            let op = match kind.as_str() {
                                "push" => Some(GitOp::Action(git_svc::GitAction::Push)),
                                "pull" => Some(GitOp::Action(git_svc::GitAction::Pull)),
                                "restore" => id.map(|id| GitOp::RestoreCheckpoint { id }),
                                "remove" => wt.map(|path| GitOp::RemoveWorktree { path, force: true }),
                                _ => Some(GitOp::Action(git_svc::GitAction::Commit { message: "feat(config): sube el tamaño de fuente".into() })),
                            };
                            if let Some(op) = op {
                                if kind == "commit"
                                    && let Some(p) = &panel
                                {
                                    p.update(cx, |p, cx| p.set_message("feat(config): sube el tamaño de fuente", "Ajusta font_size a 14 y añade el historial.", cx));
                                }
                                m.update(cx, |m, cx| m.request(op, cx));
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
