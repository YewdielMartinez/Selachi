//! The layout panel in a window with a fake app behind it, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_layout --example standalone --release -- [dark|light|oled] [--project] [--demo] [--closed] [--scroll-end] [--size WxH]
//! ```
//!
//! The panel starts open inside a titlebar `Popover`. Every change prints the resulting
//! `[layout]` TOML to stdout. `--project` starts in the "Este proyecto" scope.

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use store::{BuiltinPreset, LayoutConfig, LayoutOverrides, ResolvedLayout};
use theme::{Mode, Theme};
use ui_kit::{Assets, Card, CardTone, Icon, IconButton, Logo, Pill, Placement, Popover};
use ui_layout::{LayoutPanel, PANEL_WIDTH, Scope, layout_panel};

struct Host {
    theme: Theme,
    open: bool,
    panel: Entity<LayoutPanel>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let toggle = {
            let this = this.clone();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                this.update(cx, |h, cx| {
                    h.open = !h.open;
                    if h.open {
                        h.panel.read(cx).focus(window);
                    }
                    cx.notify();
                });
            }
        };
        let dismiss = move |_: &mut Window, cx: &mut App| {
            this.update(cx, |h, cx| {
                h.open = false;
                cx.notify();
            });
        };

        let terminal_lines = [
            ("~/Desktop/Terminal", "cargo run -p ui_layout --example standalone"),
            ("", "   Compiling ui_layout v0.1.0"),
            ("", "    Finished `release` profile [optimized] target(s) in 4.21s"),
            ("", "     Running `target/release/examples/standalone`"),
        ];

        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(div().text_size(px(12.0)).text_color(c.muted).child("Selachi › Terminal › ~/Desktop/Terminal"))
            .child(Pill::new("t0", &t).icon(Icon::Terminal).label("crates").active(true))
            .child(Pill::new("t1", &t).icon(Icon::Agents).label("claude").on_click(|_, _, _| {}))
            .child(div().flex_1())
            .child(
                Popover::new("layout-pop", &t)
                    .open(self.open)
                    .placement(Placement::BottomStart)
                    .width(PANEL_WIDTH)
                    .padding(0.0)
                    .on_dismiss(dismiss)
                    .trigger(
                        IconButton::new("layout-btn", &t, Icon::Layout)
                            .size(28.0)
                            .active(self.open)
                            .tooltip("Diseño")
                            .on_click(toggle),
                    )
                    .child(self.panel.clone()),
            );

        let term = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .font_family(t.typography.mono_family)
            .text_size(px(t.typography.mono_size))
            .children(terminal_lines.iter().map(|(prompt, text)| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.0))
                    .child(div().text_color(c.accent).child(*prompt))
                    .child(div().text_color(c.text).child(*text))
            }));

        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(div().flex_1().px(px(8.0)).pb(px(8.0)).flex().child(term))
    }
}

/// Orquestador with a couple of tweaks and one custom preset, so the screenshot shows more states.
fn demo_config() -> LayoutConfig {
    let mut c = LayoutConfig::default();
    c.select_preset(BuiltinPreset::Orquestador.into());
    c.overrides = LayoutOverrides {
        sidebar_translucency: Some(35),
        breadcrumb: Some(true),
        ..LayoutOverrides::default()
    };
    let mut mine = ResolvedLayout::builtin(BuiltinPreset::Chat);
    mine.splits = store::Splits::TwoVertical;
    mine.right_panel = Some(store::PanelId::new("git"));
    c.custom_presets.push(store::CustomPreset { name: "Mi flujo".into(), layout: mine });
    c
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
    let open = !args.iter().any(|a| a == "--closed");
    let project = args.iter().any(|a| a == "--project");
    let demo = args.iter().any(|a| a == "--demo");
    let scroll_end = args.iter().any(|a| a == "--scroll-end");
    let (w, h) = args
        .iter()
        .position(|a| a == "--size")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or((1100.0, 1000.0));

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            eprintln!("fonts: {e}");
        }
        ui_layout::init(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let theme = Theme::for_mode(mode);
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let panel = layout_panel(
                    if demo { demo_config() } else { LayoutConfig::default() },
                    Some(PathBuf::from(r"C:\Users\venta\Desktop\Terminal")),
                    theme,
                    |config, _, _| match toml::to_string(&config) {
                        Ok(text) => println!("--- [layout] changed ---\n{text}"),
                        Err(e) => eprintln!("toml: {e}"),
                    },
                    cx,
                );
                if project {
                    // The standalone has no config file: reach the scope switch through the view.
                    panel.update(cx, |p, cx| p.set_scope(Scope::Project, cx));
                }
                if open {
                    panel.read(cx).focus(window);
                }
                if scroll_end {
                    panel.read(cx).scroll_to_end();
                }
                cx.new(|_| Host { theme, open, panel })
            },
        );
        cx.activate(true);
    });
}
