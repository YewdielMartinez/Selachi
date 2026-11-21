//! The doctor panel measuring the demo process itself, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_doctor --example doctor_demo --release -- [dark|light] [--zone right|sidebar|main|bottom] [--size WxH]
//! ```
//!
//! Two harmless `pwsh Start-Sleep` children are started inside a `kill_on_close` job (they die with
//! the demo, even if it is killed) so the "Procesos hijos" tile has something to show. Frame probes
//! wrap the root, like the real shell should.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use gpui::{
    AnyView, App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use proc_svc::{JobGroup, JobOptions};
use theme::{Mode, Theme};
use ui_doctor::{AppStats, DoctorModel, DoctorPanelProvider, FrameStats, frame_begin, frame_end};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};

struct Host {
    theme: Theme,
    zone: PanelZone,
    panel: AnyView,
    panel_width: f32,
    stats: std::sync::Arc<FrameStats>,
    _children: Option<JobGroup>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let panel = Card::new(&t).tone(CardTone::Raised, &t).flex_none().overflow_hidden().h_full().child(self.panel.clone());
        let terminal = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .h_full()
            .p(px(16.0))
            .font_family(t.typography.mono_family)
            .text_size(px(t.typography.mono_size))
            .child("PS C:\\Users\\venta\\Desktop\\Terminal> ");
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
            _ => row.child(terminal).child(panel.w(px(self.panel_width))),
        };
        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(frame_begin())
            .child(titlebar)
            .child(body)
            .child(frame_end(self.stats.clone()))
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn spawn_children() -> Option<JobGroup> {
    let job = JobGroup::with_options("doctor-demo", JobOptions { kill_on_close: true, ..JobOptions::default() }).ok()?;
    for _ in 0..2 {
        let mut c = Command::new("pwsh");
        c.args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep 120"])
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        job.spawn_command(&mut c, 0).ok()?;
    }
    Some(job)
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
    let panel_width = match zone {
        PanelZone::Sidebar => 264.0,
        PanelZone::Main => 640.0,
        _ => 420.0,
    };
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or((900.0, 760.0));

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
        let theme = Theme::for_mode(mode);
        let children = spawn_children();
        let model: Entity<DoctorModel> = cx.new(|_| DoctorModel::new());
        let dir = std::env::temp_dir().join("selachi-doctor-demo");
        let _ = std::fs::create_dir_all(&dir);
        let (a, b) = (dir.join("selachi.db"), dir.join("state.db"));
        let _ = std::fs::write(&a, vec![0u8; 1_300_000]);
        let _ = std::fs::write(&b, vec![0u8; 340_000]);
        model.update(cx, |m, cx| {
            m.set_db_paths(vec![("selachi.db".into(), a), ("state.db".into(), b)]);
            m.set_app_stats(
                AppStats { shells: Some(2), agents: Some(1), grammars: vec!["rust".into(), "toml".into(), "markdown".into()] },
                cx,
            );
        });
        let stats = model.read(cx).frame_stats();
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let provider = DoctorPanelProvider::new(model, "0.0.1");
                let ctx = PanelCtx {
                    zone,
                    width_class: WidthClass::from_width(panel_width),
                    cwd: None,
                    project_root: None,
                    theme,
                };
                let panel = provider.build(ctx, window, cx);
                cx.new(|_| Host { theme, zone, panel, panel_width, stats, _children: children })
            },
        );
        cx.activate(true);
    });
}
