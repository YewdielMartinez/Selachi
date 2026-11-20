//! Perf baseline: how much does GPUI itself cost?
//!
//! Variants (cumulative), selected with CLI flags so one binary measures them all:
//!   (none)    one window, one div
//!   --fonts   + embedded fonts registered
//!   --term    + a TerminalView with a shell session (implies --fonts)
//!   --text    + a static line of text (forces glyph atlas / text shaping, no PTY)
//!
//! Phase timings are logged at `info` (`RUST_LOG=info`).
//!
//! `cargo build --release --example empty_window`, then
//! `pwsh -File bench/measure.ps1 -Exe target/release/examples/empty_window.exe -Args '--fonts'`.

// GUI subsystem like the release binary will be, so no conhost is counted in the measurement.
// Set SELACHI_LOG=1 to attach to the parent console and see the log output.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
unsafe extern "system" {
    fn AttachConsole(process_id: u32) -> i32;
}

// The terminal modules are shared with the real binary via #[path]; they only need these two
// constants from their siblings, so stub them here instead of depending on the shell modules.
mod app_shell {
    pub const RAIL_W: f32 = 0.0;
    pub const TAB_H: f32 = 0.0;
    pub const TERM_PAD: f32 = 8.0;
}
#[path = "../src/mem.rs"]
#[allow(dead_code)]
mod mem;
mod status_bar {
    pub const STATUS_H: f32 = 0.0;
}
#[path = "../src/terminal_element.rs"]
#[allow(dead_code)]
mod terminal_element;
#[path = "../src/terminal_view.rs"]
#[allow(dead_code)]
mod terminal_view;

use std::time::Instant;

use gpui::{
    App, Application, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use theme::Theme;

use terminal_view::TerminalView;

enum Body {
    Empty,
    Text,
    Term(gpui::Entity<TerminalView>),
}

struct Root {
    body: Body,
    t0: Instant,
    painted: std::cell::Cell<bool>,
}

impl Render for Root {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.painted.replace(true) {
            log::info!("first render at {} ms", self.t0.elapsed().as_millis());
        }
        let base = div().size_full().bg(rgb(0x101114)).text_color(rgb(0xdddddd));
        match &self.body {
            Body::Empty => base,
            Body::Text => base.font_family("Geist Mono").child("hello selachi 0123456789"),
            Body::Term(t) => base.child(t.clone()),
        }
    }
}

fn main() {
    let t0 = Instant::now();
    #[cfg(windows)]
    if std::env::var_os("SELACHI_LOG").is_some() {
        // SAFETY: plain Win32 call; ATTACH_PARENT_PROCESS is (DWORD)-1.
        unsafe { AttachConsole(u32::MAX) };
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let term = args.iter().any(|a| a == "--term");
    let fonts = term || args.iter().any(|a| a == "--fonts" || a == "--text");
    let text = args.iter().any(|a| a == "--text");

    Application::new().run(move |cx: &mut App| {
        log::info!(
            "app started at {} ms (process start + {})",
            t0.elapsed().as_millis(),
            mem::uptime_label()
        );
        if fonts {
            if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
                log::error!("fonts: {e}");
            }
            log::info!("fonts registered at {} ms", t0.elapsed().as_millis());
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1100.0), px(700.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                log::info!("window created at {} ms", t0.elapsed().as_millis());
                let body = if term {
                    Body::Term(cx.new(|cx| TerminalView::new(Theme::dark(), window, cx)))
                } else if text {
                    Body::Text
                } else {
                    Body::Empty
                };
                if term {
                    log::info!("terminal view built at {} ms", t0.elapsed().as_millis());
                }
                cx.new(|_| Root { body, t0, painted: std::cell::Cell::new(false) })
            },
        );
        if opened.is_err() {
            cx.quit();
        } else {
            cx.activate(true);
        }
    });
}
