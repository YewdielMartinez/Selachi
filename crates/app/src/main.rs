//! Selachi: a lightweight GPU terminal for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod adapt;
mod agents;
mod app_shell;
mod block_layout;
mod block_ui;
mod chat_host;
mod git_monitor;
mod git_panel;
mod grouping;
mod hint;
mod layout_host;
mod links;
mod mem;
mod notify;
mod palette;
mod paths;
mod rail;
mod services;
mod session;
mod settings_tab;
mod sidebar;
mod spaces;
mod splits;
mod status_bar;
mod terminal_element;
mod terminal_tab;
mod terminal_view;
mod titlebar;
mod unified_sidebar;
mod usage_monitor;
mod viewer_host;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, KeyBinding, Size, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px,
    size,
};
use panel_api::PanelRegistry;
use store::{Config, ConfigWatcher, StateDb};

use crate::app_shell::{
    AppShell, CancelClose, CancelOverlay, CloseTab, ConfigBoot, ConfirmClose, SessionBoot, ToggleFullscreen, Jump1, Jump2, Jump3, Jump4, Jump5, Jump6, Jump7,
    Jump8, Jump9, NewTab, NewTerminalTab, NextAttention, NextTab, OpenSettings, PrevTab, ToggleLayoutPanel, ToggleSidebar, ToggleTheme,
};
use crate::git_panel::ContextPanelProvider;
use crate::services::Services;

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
    let restore = config.session.restore;
    ConfigBoot { config, watcher, session: boot_session(restore) }
}

/// Opens the state database and, if `[session] restore` is not false, loads the saved session.
/// Any failure just means "start fresh": a session is a convenience, never a reason not to start.
fn boot_session(restore_enabled: bool) -> SessionBoot {
    let db = match StateDb::open_default() {
        Ok(db) => Some(db),
        Err(e) => {
            log::warn!("state database unavailable, the session will not be saved: {e}");
            None
        },
    };
    let restore = if restore_enabled {
        db.as_ref()
            .and_then(|d| d.load_session_layout().ok().flatten())
            .and_then(|s| session::SessionState::from_json(&s.json))
    } else {
        None
    };
    SessionBoot { db, restore }
}

/// The panels that ship with the app. Panel crates (`ui_git`, `ui_viewer`...) register theirs
/// here too; an id that is registered twice is replaced by the last registration.
fn register_panels(cx: &mut App) {
    let mut registry = PanelRegistry::default();
    let services = Services::new(cx);
    // Rail order: files, git, context, processes, doctor.
    registry.register(services.files.clone());
    registry.register(ui_git::GitPanelProvider::new(services.git.clone()));
    registry.register(ContextPanelProvider);
    registry.register(ui_usage::UsagePanelProvider::new(services.usage.clone()));
    registry.register(ui_review::AttentionPanelProvider::new(services.attention.clone()));
    registry.register(ui_procs::ProcsPanelProvider::new(services.procs.clone()));
    registry.register(chat_host::ThreadsProvider);
    registry.register(ui_doctor::DoctorPanelProvider::new(services.doctor.clone(), env!("CARGO_PKG_VERSION")));
    cx.set_global(services);
    cx.set_global(registry);
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", ToggleTheme, None),
        KeyBinding::new("ctrl-shift-n", NewTerminalTab, None),
        KeyBinding::new("ctrl-n", NewTab, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("ctrl-shift-l", ToggleLayoutPanel, None),
        KeyBinding::new("f11", ToggleFullscreen, None),
        KeyBinding::new("ctrl-shift-a", NextAttention, None),
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
        // Drawers / overlays of the narrow layouts close with Esc.
        KeyBinding::new("escape", CancelOverlay, Some("Drawer")),
    ]);
}

fn main() {
    env_logger::init();
    // `ui_procs::Assets` forwards to `ui_kit::Assets` and adds the runtime icons of the panels.
    Application::new().with_assets(ui_procs::Assets).run(|cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            log::error!("failed to register embedded fonts: {e}");
        }

        bind_keys(cx);
        ui_git::init(cx);
        ui_viewer::bind_keys(cx);
        ui_review::init(cx);
        ui_chat::init(cx);
        ui_settings::init(cx);
        register_panels(cx);
        // The layout popover's content: the real `ui_layout` panel.
        ui_layout::init(cx);
        ui_palette::init(cx);
        ui_palette::bind_toggle_key(cx);
        layout_host::register_layout_panel_host(cx, layout_host::ui_layout_host);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let boot = boot_config();
        let theme = crate::app_shell::theme_for(&boot.config);
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
