//! The file tree + viewer, for screenshots, manual testing and the performance numbers.
//!
//! ```text
//! cargo run -p ui_viewer --example viewer_demo --release -- [FILE] [options]
//!
//!   FILE                 file to open in the main card (default: this example)
//!   --root DIR           project root for the tree (default: the current directory)
//!   --light              light theme
//!   --diff [--split]     open in diff mode (unified, or side by side)
//!   --source             open markdown / JSON in their source view
//!   --zoom11             images at 1:1
//!   --find TEXT          open the find bar with TEXT
//!   --goto N             scroll to line N
//!   --filter TEXT        type TEXT in the tree's quick filter
//!   --reveal             expand the tree down to FILE
//!   --touches            show "Que toco el agente" for the files modified in the working tree
//!   --size WxH           window size (default 1280x800)
//!   --bench              generate a 10 MB source file, print open / first-screen times, quit
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gpui::{
    AnyView, App, AppContext as _, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use theme::{Mode, Theme};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};
use ui_viewer::{
    AgentSession, AgentTouchesView, FileTreePanel, FileTreeProvider, FileViewer, GitMark, Touch, ViewerConfig,
    ViewerEvent,
};

struct Opts {
    file: PathBuf,
    root: PathBuf,
    mode: Mode,
    diff: bool,
    split: bool,
    source: bool,
    zoom11: bool,
    find: Option<String>,
    goto: Option<usize>,
    filter: Option<String>,
    reveal: bool,
    touches: bool,
    bench: bool,
    size: (f32, f32),
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn parse() -> Opts {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let valued = ["--root", "--find", "--goto", "--filter", "--size"];
    let file = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || !valued.contains(&args[*i - 1].as_str())))
        .map(|(_, a)| PathBuf::from(a))
        .unwrap_or_else(|| PathBuf::from("crates/ui_viewer/examples/viewer_demo.rs"));
    let size = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280.0, 800.0));
    Opts {
        file,
        root: arg_value(&args, "--root").map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from),
        mode: if flag("--light") { Mode::Light } else { Mode::Dark },
        diff: flag("--diff"),
        split: flag("--split"),
        source: flag("--source"),
        zoom11: flag("--zoom11"),
        find: arg_value(&args, "--find").map(str::to_owned),
        goto: arg_value(&args, "--goto").and_then(|s| s.parse().ok()),
        filter: arg_value(&args, "--filter").map(str::to_owned),
        reveal: flag("--reveal"),
        touches: flag("--touches"),
        bench: flag("--bench"),
        size,
    }
}

/// `git status --porcelain` of `root` as tree markers.
fn git_marks(root: &Path) -> HashMap<PathBuf, GitMark> {
    let Ok(out) = Command::new("git").arg("-C").arg(root).args(["status", "--porcelain"]).output() else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (code, path) = (l.get(..2)?, l.get(3..)?.trim().trim_matches('"'));
            let mark = match code.trim() {
                "M" | "MM" | "AM" => GitMark::Modified,
                "A" => GitMark::Added,
                "??" => GitMark::Untracked,
                "D" => GitMark::Deleted,
                _ => GitMark::Modified,
            };
            Some((PathBuf::from(path), mark))
        })
        .collect()
}

enum Main {
    Viewer(Entity<FileViewer>),
    Touches(Entity<AgentTouchesView>),
}

struct Host {
    theme: Theme,
    tree: AnyView,
    main: Main,
    root: PathBuf,
}

impl Host {
    fn open(&mut self, path: PathBuf, diff_base: Option<String>, diff: bool, cx: &mut Context<Self>) {
        let cfg = ViewerConfig {
            project_root: Some(self.root.clone()),
            open_in_diff: diff,
            diff_base: diff_base.map_or(viewer_svc::GitBase::Head, viewer_svc::GitBase::Commit),
            ..ViewerConfig::default()
        };
        let theme = self.theme;
        let viewer = cx.new(|cx| FileViewer::new(path, cfg, theme, cx));
        self.watch(&viewer, cx);
        self.main = Main::Viewer(viewer);
        cx.notify();
    }

    fn watch(&self, viewer: &Entity<FileViewer>, cx: &mut Context<Self>) {
        cx.subscribe(viewer, |_, _, ev: &ViewerEvent, _| eprintln!("viewer event: {ev:?}")).detach();
    }
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let sidebar = Card::new(&t).tone(CardTone::Raised, &t).flex_none().w(px(272.0)).h_full().overflow_hidden().child(self.tree.clone());
        let main: AnyView = match &self.main {
            Main::Viewer(v) => v.clone().into(),
            Main::Touches(v) => v.clone().into(),
        };
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(Pill::new("t-files", &t).icon(Icon::Files).label("Archivos").active(true));
        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(8.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .child(sidebar)
                    .child(div().flex_1().min_w_0().h_full().child(main)),
            )
    }
}

/// A 10 MB Rust-looking source file.
fn generate_big(dir: &Path) -> PathBuf {
    let path = dir.join("big_generated.rs");
    // Reuse the file between runs (a second run measures a file the OS has already scanned).
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= 10 * 1024 * 1024) {
        return path;
    }
    let mut s = String::with_capacity(11 * 1024 * 1024);
    let mut i = 0usize;
    while s.len() < 10 * 1024 * 1024 {
        s.push_str(&format!(
            "/// Generated function number {i}.\npub fn generated_{i}(input: &[u8], offset: usize) -> Result<usize, String> {{\n    let mut total = 0usize;\n    for (ix, b) in input.iter().enumerate().skip(offset) {{\n        total = total.wrapping_add(usize::from(*b) * (ix + {i}));\n    }}\n    if total % 7 == 0 {{ Err(format!(\"bad total {{}}\", total)) }} else {{ Ok(total) }}\n}}\n\n"
        ));
        i += 1;
    }
    let _ = std::fs::write(&path, s);
    path
}

fn main() {
    let opts = parse();
    Application::new().with_assets(ui_kit::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            eprintln!("fonts: {e}");
        }
        ui_viewer::bind_keys(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let theme = Theme::for_mode(opts.mode);
        let root = opts.root.canonicalize().unwrap_or(opts.root.clone());
        let bench_dir = opts.bench.then(|| {
            let d = std::env::temp_dir().join("selachi-viewer-bench");
            let _ = std::fs::create_dir_all(&d);
            d
        });
        let file = match &bench_dir {
            Some(d) => generate_big(d),
            None => {
                let f = if opts.file.is_absolute() { opts.file.clone() } else { root.join(&opts.file) };
                f.canonicalize().map(|p| PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))).unwrap_or(f)
            }
        };
        let root = PathBuf::from(root.to_string_lossy().trim_start_matches(r"\\?\"));
        let (w, h) = opts.size;
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let provider = FileTreeProvider::new();
                let ctx = PanelCtx {
                    zone: PanelZone::Sidebar,
                    width_class: WidthClass::from_width(272.0),
                    cwd: Some(root.clone()),
                    project_root: Some(root.clone()),
                    theme,
                };
                let tree = provider.build(ctx, window, cx);
                provider.set_git_status(git_marks(&root), cx);
                let panel = tree.clone().downcast::<FileTreePanel>().ok();

                let cfg = ViewerConfig {
                    project_root: Some(root.clone()),
                    open_in_diff: opts.diff,
                    line: opts.goto,
                    ..ViewerConfig::default()
                };
                let main = if opts.touches {
                    let touched: Vec<Touch> = git_marks(&root)
                        .into_iter()
                        .filter(|(_, m)| matches!(m, GitMark::Modified | GitMark::Added))
                        .enumerate()
                        .map(|(i, (p, _))| Touch { path: root.join(p), tool: if i % 2 == 0 { "Edit".into() } else { "Write".into() }, ts_ms: 1_000 + i as i64 })
                        .collect();
                    let session = AgentSession {
                        agent: "claude".into(),
                        session_id: "3f2a91c4-demo".into(),
                        project_root: Some(root.clone()),
                        base: None,
                        tokens: Some(1_840_000),
                        cost_usd: Some(4.1),
                        touches: touched,
                    };
                    Main::Touches(cx.new(|cx| AgentTouchesView::new(session, theme, cx)))
                } else {
                    let f = file.clone();
                    let viewer = cx.new(|cx| FileViewer::new(f, cfg, theme, cx));
                    Main::Viewer(viewer)
                };

                if let Main::Viewer(v) = &main {
                    let v = v.clone();
                    let (split, source, zoom11, find) = (opts.split, opts.source, opts.zoom11, opts.find.clone());
                    // Apply the display options once the file has loaded.
                    window
                        .spawn(cx, async move |cx| {
                            for _ in 0..200 {
                                cx.background_executor().timer(Duration::from_millis(25)).await;
                                let ready = cx.update(|_, cx| !v.read(cx).is_loading()).unwrap_or(true);
                                if ready {
                                    break;
                                }
                            }
                            cx.update(|window, cx| {
                                v.update(cx, |v, cx| {
                                    if split {
                                        v.set_diff_split(true, cx);
                                    }
                                    if source {
                                        v.toggle_source(cx);
                                    }
                                    if zoom11 {
                                        v.set_image_fit(false, cx);
                                    }
                                    if let Some(q) = &find {
                                        v.show_find(q, window, cx);
                                    }
                                });
                            })
                            .ok();
                        })
                        .detach();
                }

                if let Some(panel) = &panel {
                    if let Some(q) = &opts.filter {
                        panel.update(cx, |p, cx| p.set_filter_text(q, cx));
                    }
                    if opts.reveal || opts.filter.is_none() {
                        let f = file.clone();
                        panel.update(cx, |p, cx| p.reveal(&f, cx));
                    }
                }

                if opts.bench {
                    let v = match &main {
                        Main::Viewer(v) => Some(v.clone()),
                        Main::Touches(_) => None,
                    };
                    if let Some(v) = v {
                        window
                            .spawn(cx, async move |cx| {
                                for _ in 0..2_000 {
                                    cx.background_executor().timer(Duration::from_millis(2)).await;
                                    let done = cx.update(|_, cx| v.read(cx).first_screen_time().is_some()).unwrap_or(true);
                                    if done {
                                        break;
                                    }
                                }
                                cx.background_executor().timer(Duration::from_millis(1_500)).await;
                                let _ = cx.update(|_, cx| {
                                    let v = v.read(cx);
                                    println!(
                                        "bench: file={} size={} load(Document::open)={:?} first_screen={:?} lines_indexed={} highlight_ready={}",
                                        v.path().display(),
                                        v.document().map_or(0, |d| d.len()),
                                        v.load_time(),
                                        v.first_screen_time(),
                                        v.document().is_some_and(|d| d.is_indexed()),
                                        v.document().is_some_and(|d| d.highlight_ready()),
                                    );
                                    cx.quit();
                                });
                            })
                            .detach();
                    }
                }

                let host = cx.new(|_| Host { theme, tree, main, root: root.clone() });
                if let Some(panel) = &panel {
                    let host = host.clone();
                    cx.subscribe(panel, move |_, ev: &ViewerEvent, cx| {
                        if let ViewerEvent::OpenFile { path } = ev {
                            let path = path.clone();
                            host.update(cx, |h, cx| h.open(path, None, false, cx));
                        }
                    })
                    .detach();
                }
                if let Main::Viewer(v) = &host.read(cx).main {
                    let v = v.clone();
                    host.update(cx, |h, cx| h.watch(&v, cx));
                }
                if let Main::Touches(v) = &host.read(cx).main {
                    let v = v.clone();
                    let host = host.clone();
                    cx.subscribe(&v, move |_, ev: &ViewerEvent, cx| {
                        if let ViewerEvent::OpenDiff { path, base } = ev {
                            let (path, base) = (path.clone(), base.clone());
                            host.update(cx, |h, cx| h.open(path, base, true, cx));
                        }
                    })
                    .detach();
                }
                host
            },
        );
        cx.activate(true);
    });
}
