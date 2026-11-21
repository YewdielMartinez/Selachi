//! The attention queue with fake items, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_review --example attention_demo --release -- [dark|light] [options]
//!
//!   --zone right|sidebar|popover   where the list lives (default right)
//!   --compact                      force the compact width class
//!   --empty                        no items
//!   --size WxH                     window size (default 1100x700)
//! ```
//!
//! Press `Ctrl+Shift+A` to cycle through the items (the host owns that key); clicking a row jumps to
//! it and the x dismisses it. The strip at the bottom prints what the host would do.

use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, KeyBinding, Render, Window, WindowBounds, WindowOptions, actions, div,
    prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use theme::{Mode, Theme};
use ui_kit::{Card, CardTone, Icon, Logo, Pill, PillSize};
use ui_review::{AttentionEvent, AttentionItem, AttentionKind, AttentionModel, AttentionPanelProvider, AttentionView, FocusTarget};

actions!(attention_demo, [JumpNext]);

struct Host {
    theme: Theme,
    zone: Option<PanelZone>,
    popover: Option<Entity<AttentionView>>,
    panel: gpui::AnyView,
    panel_width: f32,
    count: usize,
    log: String,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(Pill::new("t0", &t).icon(Icon::Terminal).label("selachi").active(true))
            .child(Pill::new("t1", &t).icon(Icon::Terminal).label("api"));
        let terminal = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .h_full()
            .p(px(16.0))
            .font_family(ty.mono_family)
            .text_size(px(ty.mono_size))
            .text_color(c.muted)
            .child("PS C:\\Users\\venta\\Desktop\\Terminal> ");
        let panel = Card::new(&t).tone(CardTone::Raised, &t).flex_none().overflow_hidden().h_full().w(px(self.panel_width)).child(self.panel.clone());
        let body = div().flex_1().min_h_0().px(px(8.0)).pb(px(8.0)).flex().flex_row().gap(px(8.0));
        let body = match self.zone {
            Some(PanelZone::Sidebar) => body.child(panel).child(terminal),
            Some(_) => body.child(terminal).child(panel),
            None => body.child(terminal),
        };
        // Status bar: the count badge the host shows, and the popover when that is the mode.
        let badge = Pill::compact("attention-badge", &t).icon(Icon::Alert).label(format!("{} esperan", self.count));
        let status = div()
            .h(px(28.0))
            .flex_none()
            .px(px(16.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .child(badge.size(PillSize::Compact))
            .child(self.log.clone());
        let mut root = div().relative().size_full().bg(c.bg).text_color(c.text).font_family(ty.ui_family).flex().flex_col().child(titlebar).child(body).child(status);
        if let Some(p) = &self.popover {
            root = root.child(
                div()
                    .absolute()
                    .left(px(12.0))
                    .bottom(px(32.0))
                    .w(px(340.0))
                    .max_h(px(420.0))
                    .rounded(px(16.0))
                    .bg(c.surface_2)
                    .border_1()
                    .border_color(c.outline_soft)
                    .overflow_hidden()
                    .child(p.clone()),
            );
        }
        root
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn item(id: &str, owner: &str, agent: Option<&str>, kind: AttentionKind, age_s: u64) -> AttentionItem {
    AttentionItem {
        id: id.to_string().into(),
        owner_label: owner.to_string().into(),
        agent: agent.map(str::to_owned),
        kind,
        since: Instant::now() - Duration::from_secs(age_s),
        focus_target: FocusTarget::new(format!("tab-{id}")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let mode = if flag("light") { Mode::Light } else { Mode::Dark };
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1100.0f32, 700.0f32));
    let zone_arg = arg_value(&args, "--zone").unwrap_or("right").to_owned();
    let compact = flag("--compact");
    let empty = flag("--empty");

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
        let model = cx.new(|_| AttentionModel::new());
        if !empty {
            model.update(cx, |m, cx| {
                m.upsert(
                    item(
                        "1",
                        "selachi · claude",
                        Some("claude"),
                        AttentionKind::Approval { tool: "Bash".into(), detail: "cargo test -p ui_review --release".into() },
                        8,
                    ),
                    cx,
                );
                m.upsert(item("2", "api · codex", Some("codex"), AttentionKind::WaitingInput, 134), cx);
                m.upsert(item("3", "docs · opencode", Some("opencode"), AttentionKind::WaitingInput, 47 * 60), cx);
                m.upsert(item("4", "cargo test (selachi)", None, AttentionKind::Failed { exit_code: 101 }, 3 * 60 + 12), cx);
                m.upsert(item("5", "dotnet build (api)", None, AttentionKind::LongCommandDone, 26), cx);
            });
        }

        // The host binds Ctrl+Shift+A and routes the events.
        let panel_width = if compact { 260.0 } else { 340.0 };
        let (zone, popover_mode) = match zone_arg.as_str() {
            "sidebar" => (Some(PanelZone::Sidebar), false),
            "popover" => (None, true),
            _ => (Some(PanelZone::Right), false),
        };
        cx.bind_keys([KeyBinding::new("ctrl-shift-a", JumpNext, None)]);
        let m = model.clone();
        cx.on_action(move |_: &JumpNext, cx| {
            m.update(cx, |m, cx| {
                m.jump_next(cx);
            });
        });

        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let width_class = if compact { WidthClass::Compact } else { WidthClass::from_width(panel_width) };
                let (panel, popover): (gpui::AnyView, Option<Entity<AttentionView>>) = if popover_mode {
                    let m = model.clone();
                    let v = cx.new(|cx| AttentionView::new(m, theme, width_class, cx));
                    (cx.new(|_| gpui::Empty).into(), Some(v))
                } else {
                    let provider = AttentionPanelProvider::new(model.clone());
                    let ctx = PanelCtx {
                        zone: zone.unwrap_or(PanelZone::Right),
                        width_class,
                        cwd: None,
                        project_root: None,
                        theme,
                    };
                    (provider.build(ctx, window, cx), None)
                };
                let host = cx.new(|cx| {
                    let count = model.read(cx).count();
                    Host { theme, zone, popover, panel, panel_width, count, log: "Host: Ctrl+Shift+A salta al siguiente".to_string() }
                });
                let log = host.clone();
                let m = model.clone();
                cx.subscribe(&model, move |_, ev: &AttentionEvent, cx| {
                    let count = m.read(cx).count();
                    let line = match ev {
                        AttentionEvent::Jump(t) => format!("Host: saltar a {}", t.0),
                        AttentionEvent::Dismiss(id) => format!("Host: descartado {id}"),
                        AttentionEvent::Changed { count } => format!("Host: {count} en la cola"),
                    };
                    eprintln!("{line}");
                    log.update(cx, |h, cx| {
                        h.count = count;
                        h.log = line;
                        cx.notify();
                    });
                })
                .detach();
                host
            },
        );
        cx.activate(true);
    });
}
