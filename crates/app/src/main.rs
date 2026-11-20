//! Selachi: a lightweight GPU terminal for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app_shell;
mod git_monitor;
mod git_panel;
mod grouping;
mod layout_host;
mod mem;
mod paths;
mod rail;
mod sidebar;
mod splits;
mod status_bar;
mod terminal_element;
mod terminal_tab;
mod terminal_view;
mod titlebar;
mod usage_monitor;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, KeyBinding, Size, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px,
    size,
};
use panel_api::PanelRegistry;
use store::{Config, ConfigWatcher};
use theme::Theme;

use crate::app_shell::{
    AppShell, CancelClose, CloseTab, ConfigBoot, ConfirmClose, Jump1, Jump2, Jump3, Jump4, Jump5, Jump6, Jump7,
    Jump8, Jump9, NewTab, NextTab, PrevTab, ToggleLayoutPanel, ToggleTheme,
};
use crate::git_panel::{GitPanelProvider, Variant};
use ui_kit::Assets;

/// Debounce of the `config.toml` watcher: editors write in bursts.
const CONFIG_DEBOUNCE: Duration = Duration::from_millis(250);

/// Loads `config.toml` (writing the commented template on first run) and starts watching it.
fn boot_config() -> ConfigBoot {
    let load = Config::load_or_default();
    if let Some(e) = &load.error {
        log::warn!("config.toml: {e}; using defaults");
    }
    for w in &load.warnings {
        log::warn!("config.toml: {w}");
    }
    let config = Arc::new(load.config);
    let watcher = if load.path.as_os_str().is_empty() {
        None
    } else {
        match ConfigWatcher::start(&load.path, Arc::clone(&config), CONFIG_DEBOUNCE) {
            Ok(pair) => Some(pair),
            Err(e) => {
                log::warn!("config hot reload unavailable: {e}");
                None
            },
        }
    };
    ConfigBoot { config, watcher }
}

/// The panels that ship with the app. Panel crates (`ui_git`, `ui_viewer`...) register theirs
/// here too; an id that is registered twice is replaced by the last registration.
fn register_panels(cx: &mut App) {
    let mut registry = PanelRegistry::default();
    registry.register(GitPanelProvider(Variant::Git));
    registry.register(GitPanelProvider(Variant::Context));
    cx.set_global(registry);
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", ToggleTheme, None),
        KeyBinding::new("ctrl-shift-n", NewTab, None),
        KeyBinding::new("ctrl-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("ctrl-shift-l", ToggleLayoutPanel, None),
        KeyBinding::new("ctrl-1", Jump1, None),
        KeyBinding::new("ctrl-2", Jump2, None),
        KeyBinding::new("ctrl-3", Jump3, None),
        KeyBinding::new("ctrl-4", Jump4, None),
        KeyBinding::new("ctrl-5", Jump5, None),
        KeyBinding::new("ctrl-6", Jump6, None),
        KeyBinding::new("ctrl-7", Jump7, None),
        KeyBinding::new("ctrl-8", Jump8, None),
        KeyBinding::new("ctrl-9", Jump9, None),
        // The close prompt: Enter confirms, Esc cancels.
        KeyBinding::new("enter", ConfirmClose, Some("CloseConfirm")),
        KeyBinding::new("escape", CancelClose, Some("CloseConfirm")),
    ]);
}

fn main() {
    env_logger::init();
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            log::error!("failed to register embedded fonts: {e}");
        }

        bind_keys(cx);
        register_panels(cx);
        // INTEGRATION POINT (ui_layout): once the `ui_layout` crate exposes its popover content,
        // register it here and the shell will mount it in the layout popover instead of the
        // built-in preset switcher:
        //     layout_host::register_layout_panel_host(cx, |args, window, cx| ui_layout::layout_panel(args, window, cx));

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let theme = Theme::dark();
        let boot = boot_config();
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
            |window, cx| cx.new(|cx| AppShell::new(theme, boot, window, cx)),
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
