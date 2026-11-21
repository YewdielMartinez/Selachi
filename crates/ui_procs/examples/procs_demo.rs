//! The process panel over real child processes, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_procs --example procs_demo --release -- [dark|light] [--zone right|sidebar|main|bottom]
//!                                                         [--tab ports] [--confirm] [--size WxH]
//! ```
//!
//! It spawns a few harmless processes (`pwsh Start-Sleep` and PowerShell `TcpListener`s on
//! 127.0.0.1:0), all inside `kill_on_close` jobs so they die with the demo (even if it is killed):
//!
//! * tab "api" (Claude): active shell + a dev server holding a port,
//! * tab "web" (Codex): its shell exits after a few seconds, leaving an orphan + a port zombie,
//! * a process tree under a renamed copy of `cmd.exe` called `codex-demo.exe`, started *outside* every
//!   tracked job: it shows up as "atribución probable".
//!
//! `--confirm` opens the "Limpiar huérfanos" confirmation after the first samples.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, AnyView, div, prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use proc_svc::{JobGroup, JobOptions, OwnerMeta, Tracker};
use theme::{Mode, Theme};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};
use ui_procs::{ProcsModel, ProcsPanel, ProcsPanelProvider, SharedTracker, TrackerSource, open_owner};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LISTENER: &str = "$l=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0);$l.Start();Start-Sleep 90";

fn pwsh(script: &str) -> Command {
    let mut c = Command::new("pwsh");
    c.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// Keeps every demo process alive for the lifetime of the demo.
struct Fleet {
    _tracker: SharedTracker,
    _external: Option<JobGroup>,
}

fn start_fleet() -> (SharedTracker, Fleet, Vec<futures::channel::mpsc::UnboundedReceiver<proc_svc::JobEvent>>) {
    let tracker: SharedTracker = Arc::new(Mutex::new(Tracker::new()));
    let kill = JobOptions { kill_on_close: true, ..JobOptions::default() };
    let mut events = Vec::new();

    // Tab "api": shell alive for 90 s, with a dev server under it.
    let meta = OwnerMeta { agent: Some("claude".into()), label: "api".into(), cwd: Some(PathBuf::from(r"C:\Users\venta\Desktop\Terminal")) };
    if let Ok(job) = open_owner(&tracker, 1, meta, kill) {
        events.push(job.events);
        let mut t = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = t.spawn(1, &mut pwsh("Start-Process pwsh -ArgumentList '-NoProfile','-Command','$l=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0);$l.Start();Start-Sleep 90' -WindowStyle Hidden; Start-Sleep 90")) {
            eprintln!("spawn api: {e}");
        }
    }

    // Tab "web": the shell dies after 4 s; the listener (spawned straight into the job, not a
    // registered root) outlives it: orphan -> port zombie.
    let meta = OwnerMeta { agent: Some("codex".into()), label: "web".into(), cwd: Some(PathBuf::from(r"C:\Users\venta\Desktop\Terminal\crates")) };
    if let Ok(job) = open_owner(&tracker, 2, meta, kill) {
        events.push(job.events);
        let mut t = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = t.spawn(2, &mut pwsh("Start-Sleep 4")) {
            eprintln!("spawn web shell: {e}");
        }
        if let Some(j) = t.job(2) {
            for script in [LISTENER, "Start-Sleep 90"] {
                if let Err(e) = j.spawn_command(&mut pwsh(script), 0) {
                    eprintln!("spawn web child: {e}");
                }
            }
        }
    }

    // Outside every tracked job: a renamed cmd.exe called codex-demo.exe -> heuristic attribution.
    let external = spawn_external_agent();
    (tracker.clone(), Fleet { _tracker: tracker, _external: external }, events)
}

fn spawn_external_agent() -> Option<JobGroup> {
    let dir = std::env::temp_dir().join("selachi-procs-demo");
    std::fs::create_dir_all(&dir).ok()?;
    let exe = dir.join("codex-demo.exe");
    let system = PathBuf::from(std::env::var_os("SystemRoot")?).join("System32").join("cmd.exe");
    if !exe.exists() && let Err(e) = std::fs::copy(system, &exe) {
        eprintln!("copy cmd.exe: {e}");
        return None;
    }
    let job = JobGroup::with_options("demo-external", JobOptions { kill_on_close: true, ..JobOptions::default() }).ok()?;
    let mut cmd = Command::new(&exe);
    cmd.args(["/c", "pwsh", "-NoProfile", "-NonInteractive", "-Command", LISTENER])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(e) = job.spawn_command(&mut cmd, CREATE_NO_WINDOW) {
        eprintln!("spawn codex-demo: {e}");
        return None;
    }
    Some(job)
}

struct Host {
    theme: Theme,
    zone: PanelZone,
    panel: AnyView,
    panel_width: f32,
    panel_height: Option<f32>,
    /// Keeps the demo processes (and their kill-on-close jobs) alive as long as the window.
    _fleet: Fleet,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let card = |panel: AnyView, w: Option<f32>, h: Option<f32>| {
            let mut d = Card::new(&t).tone(CardTone::Raised, &t).flex_none().overflow_hidden().child(panel);
            d = match w {
                Some(w) => d.w(px(w)),
                None => d.flex_1(),
            };
            match h {
                Some(h) => d.h(px(h)),
                None => d.h_full(),
            }
        };
        let terminal = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .h_full()
            .p(px(16.0))
            .font_family(t.typography.mono_family)
            .text_size(px(t.typography.mono_size))
            .text_color(c.text)
            .child("PS C:\\Users\\venta\\Desktop\\Terminal> npm run dev");

        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(Pill::new("t0", &t).icon(Icon::Terminal).label("api").active(true))
            .child(Pill::new("t1", &t).icon(Icon::Agents).label("web"));

        let body = match self.zone {
            PanelZone::Sidebar => div()
                .flex_1()
                .min_h_0()
                .px(px(8.0))
                .pb(px(8.0))
                .flex()
                .flex_row()
                .gap(px(8.0))
                .child(card(self.panel.clone(), Some(self.panel_width), None))
                .child(terminal),
            PanelZone::Right | PanelZone::Main => div()
                .flex_1()
                .min_h_0()
                .px(px(8.0))
                .pb(px(8.0))
                .flex()
                .flex_row()
                .gap(px(8.0))
                .child(terminal)
                .child(card(self.panel.clone(), Some(self.panel_width), None)),
            PanelZone::Bottom => div()
                .flex_1()
                .min_h_0()
                .px(px(8.0))
                .pb(px(8.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(terminal)
                .child(card(self.panel.clone(), None, self.panel_height)),
        };
        div().size_full().bg(c.bg).text_color(c.text).font_family(t.typography.ui_family).flex().flex_col().child(titlebar).child(body)
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") { Mode::Light } else { Mode::Dark };
    let zone = match arg_value(&args, "--zone") {
        Some("sidebar") => PanelZone::Sidebar,
        Some("main") => PanelZone::Main,
        Some("bottom") => PanelZone::Bottom,
        _ => PanelZone::Right,
    };
    let ports = arg_value(&args, "--tab") == Some("ports");
    let confirm = args.iter().any(|a| a == "--confirm");
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or(match zone {
            PanelZone::Bottom => (1000.0, 640.0),
            PanelZone::Main => (1100.0, 700.0),
            _ => (900.0, 720.0),
        });
    let panel_width = match zone {
        PanelZone::Sidebar => 264.0,
        PanelZone::Main => 640.0,
        _ => 392.0,
    };

    Application::new().with_assets(ui_procs::Assets).run(move |cx: &mut App| {
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
        let (tracker, fleet, events) = start_fleet();
        let model: Entity<ProcsModel> = cx.new(|cx| {
            // Only the demo agent name, so real agents on this machine never show up.
            let mut m = ProcsModel::new(Arc::new(TrackerSource::new(tracker).agents(["codex-demo"])));
            for rx in events {
                m.watch_job_events(rx, cx);
            }
            m
        });
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let model_for_window = model;
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let provider = ProcsPanelProvider::new(model_for_window.clone());
                let ctx = PanelCtx {
                    zone,
                    width_class: WidthClass::from_width(if zone == PanelZone::Bottom { w } else { panel_width }),
                    cwd: None,
                    project_root: None,
                    theme,
                };
                let panel = provider.build(ctx, window, cx);
                if ports && let Ok(p) = panel.clone().downcast::<ProcsPanel>() {
                    p.update(cx, |p, cx| p.select_ports(true, cx));
                }
                if confirm {
                    let m = model_for_window.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(6)).await;
                        let _ = m.update(cx, |m, cx| m.request_clean_orphans(cx));
                    })
                    .detach();
                }
                // Keep the processes (and their kill-on-close jobs) alive as long as the window.
                cx.new(move |_| Host {
                    theme,
                    zone,
                    panel,
                    panel_width,
                    panel_height: Some(h - 40.0 - 8.0 - 150.0),
                    _fleet: fleet,
                })
            },
        );
        cx.activate(true);
    });
}
