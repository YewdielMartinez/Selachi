//! Selachi: a lightweight GPU terminal for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod adapt;
mod agentmap_feed;
mod agentmap_host;
mod agentmap_tab;
mod agents;
mod app_shell;
mod approval_queue;
mod approvals;
mod approvals_ui;
mod block_layout;
mod block_ui;
mod chat_host;
mod cli;
mod draft_agent;
mod drop;
mod git_monitor;
mod git_panel;
mod grouping;
mod hint;
mod keybindings;
mod layout_host;
mod links;
mod mem;
mod notify;
mod palette;
mod paths;
mod projects;
mod rail;
mod repaint;
mod services;
mod session;
mod session_persistence;
mod settings_tab;
mod sidebar;
mod sidebar_view;
mod single_instance;
mod spaces;
mod splits;
mod status_bar;
mod terminal_element;
mod terminal_tab;
mod terminal_view;
mod titlebar;
mod titlebar_view;
mod unified_sidebar;
mod update_host;
mod usage_monitor;
mod viewer_host;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Size, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px,
    size,
};
use panel_api::PanelRegistry;
use store::{Config, ConfigWatcher, StateDb};

use crate::app_shell::{AppShell, ConfigBoot, SessionBoot};
use crate::git_panel::ContextPanelProvider;
use crate::services::Services;

/// Debounce of the `config.toml` watcher: editors write in bursts.
const CONFIG_DEBOUNCE: Duration = Duration::from_millis(250);

/// Loads `config.toml` (writing the commented template on first run) and starts watching it.
fn boot_config(start_dir: Option<std::path::PathBuf>) -> ConfigBoot {
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
            }
        }
    };
    let restore = config.session.restore;
    ConfigBoot {
        config,
        watcher,
        session: boot_session(restore),
        start_dir,
    }
}

/// Opens the state database and, if `[session] restore` is not false, loads the saved session.
/// Any failure just means "start fresh": a session is a convenience, never a reason not to start.
fn boot_session(restore_enabled: bool) -> SessionBoot {
    let db = match StateDb::open_default() {
        Ok(db) => Some(db),
        Err(e) => {
            log::warn!("state database unavailable, the session will not be saved: {e}");
            None
        }
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
    registry.register(ui_review::AttentionPanelProvider::new(
        services.attention.clone(),
    ));
    registry.register(ui_procs::ProcsPanelProvider::new(services.procs.clone()));
    registry.register(chat_host::ThreadsProvider);
    registry.register(ui_agentmap::AgentMapPanelProvider::new(
        services.agentmap.clone(),
    ));
    registry.register(ui_doctor::DoctorPanelProvider::new(
        services.doctor.clone(),
        env!("CARGO_PKG_VERSION"),
    ));
    cx.set_global(services);
    cx.set_global(registry);
}

/// Serves the approvals pipe: delegated agents (`selachi-agent`) ask here before they run a tool or
/// apply their changes. The server thread sleeps in `ConnectNamedPipe`; the shell takes the
/// receiver when it is built. If the pipe cannot be created (another Selachi owns it) delegated
/// agents are denied by the harness with a clear reason.
fn start_approvals(cx: &mut App) {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    match approvals::start(&agent_harness::default_pipe_path(), tx) {
        Ok(_) => {
            if let Some(services) = cx.try_global::<Services>() {
                services.approvals_rx.replace(Some(rx));
            }
        }
        Err(e) => log::warn!("the approvals pipe is not available: {e}"),
    }
}

fn main() {
    // `selachi agent ...` and `selachi mcp` are the harness CLI: no window, no GUI state.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(forwarded) = cli::forwarded_args(&argv) {
        std::process::exit(cli::run_forwarded(&forwarded));
    }
    env_logger::init();
    let base = std::env::current_dir().unwrap_or_default();
    let args = cli::parse(std::env::args_os().skip(1), &base);
    if args.version {
        cli::print_version();
        return;
    }
    // One Selachi per user: a second launch hands its folder to the first and exits.
    let _instance =
        match single_instance::acquire(&single_instance::Request::Open(args.cwd.clone())) {
            single_instance::Instance::Forwarded => return,
            single_instance::Instance::Primary(guard) => guard,
        };
    let (requests_tx, requests_rx) = futures::channel::mpsc::unbounded();
    single_instance::listen(requests_tx);
    let start_dir = args.cwd;
    // `ui_procs::Assets` forwards to `ui_kit::Assets` and adds the runtime icons of the panels.
    Application::new()
        .with_assets(ui_procs::Assets)
        .run(move |cx: &mut App| {
            if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
                log::error!("failed to register embedded fonts: {e}");
            }
            app_shell::register_installed_fonts(cx);

            ui_kit::init(cx);
            keybindings::bind_static(cx);
            ui_git::init(cx);
            ui_viewer::bind_keys(cx);
            ui_review::init(cx);
            ui_chat::init(cx);
            ui_settings::init(cx);
            register_panels(cx);
            start_approvals(cx);
            // The layout popover's content: the real `ui_layout` panel.
            ui_layout::init(cx);
            ui_palette::init(cx);
            layout_host::register_layout_panel_host(cx, layout_host::ui_layout_host);

            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let boot = boot_config(start_dir);
            // Ctrl+K included: the palette's key goes through the same keymap as the rest.
            keybindings::install(cx, &boot.config.keybindings);
            let theme = crate::app_shell::theme_for(&boot.config, cx.window_appearance());
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
                    window_min_size: Some(Size {
                        width: px(480.0),
                        height: px(320.0),
                    }),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| AppShell::new(theme, boot, window, cx)),
            );
            match opened {
                Ok(handle) => {
                    cx.activate(true);
                    forward_requests(handle, requests_rx, cx);
                }
                Err(e) => {
                    log::error!("failed to open window: {e}");
                    cx.quit();
                }
            }
        });
}

/// Later launches (see [`single_instance`]) open a tab in their folder and bring the window
/// forward. The task sleeps on the channel: it costs nothing while no launch arrives.
fn forward_requests(
    window: gpui::WindowHandle<AppShell>,
    mut requests: futures::channel::mpsc::UnboundedReceiver<single_instance::Request>,
    cx: &mut App,
) {
    use futures::StreamExt as _;
    cx.spawn(async move |cx| {
        while let Some(request) = requests.next().await {
            let single_instance::Request::Open(dir) = request;
            let done = window.update(cx, |shell, window, cx| match dir {
                Some(dir) => shell.open_in_folder(dir, window, cx),
                None => window.activate_window(),
            });
            if done.is_err() {
                break;
            }
        }
    })
    .detach();
}
