//! Selachi: a lightweight GPU terminal for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app_shell;
mod assets;
mod git_monitor;
mod icons;
mod mem;
mod paths;
mod rail;
mod status_bar;
mod terminal_element;
mod terminal_view;
mod titlebar;
mod usage_monitor;

use gpui::{
    App, Application, Bounds, KeyBinding, Size, TitlebarOptions, WindowBounds, WindowOptions,
    prelude::*, px, size,
};
use theme::Theme;

use crate::app_shell::{AppShell, ToggleTheme};
use crate::assets::Assets;

fn main() {
    env_logger::init();
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            log::error!("failed to register embedded fonts: {e}");
        }

        cx.bind_keys([KeyBinding::new("ctrl-shift-t", ToggleTheme, None)]);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let theme = Theme::dark();
        let bounds = Bounds::centered(None, size(px(1100.0), px(700.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Custom chrome: the system titlebar is hidden, `titlebar.rs` draws our own.
                titlebar: Some(TitlebarOptions {
                    title: Some("Selachi".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_min_size: Some(Size { width: px(480.0), height: px(320.0) }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AppShell::new(theme, window, cx)),
        );
        match opened {
            Ok(_) => cx.activate(true),
            Err(e) => {
                log::error!("failed to open window: {e}");
                cx.quit();
            }
        }
    });
}
