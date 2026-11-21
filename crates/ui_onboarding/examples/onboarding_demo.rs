//! The first-run wizard in a window, for screenshots and manual testing.
//!
//! ```text
//! cargo run -p ui_onboarding --example onboarding_demo --release -- [dark|light|oled] [--step 1..6]
//!     [--size WxH] [--fake] [--slow] [--reduced-motion]
//! ```
//!
//! * The data directory is a fresh temp folder (`SELACHI_DATA_DIR`), so the real config is never
//!   touched; its path is printed. Finishing writes `config.toml` and `onboarding.done` there.
//! * `--fake` shows every agent state (listo, falta sesión, instalado, no instalado) and fixed shells.
//! * `--slow` makes the fake detectors take 4 s, to see "Comprobando…".

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agents_driver::{AgentKind, DetectedAgent};
use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use store::{Config, StateDb};
use theme::{Mode, Theme};
use ui_onboarding::shells::{ShellInfo, ShellKind};
use ui_onboarding::{OnboardingEvent, Services, Step, onboarding_with};

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn fake_services(slow: bool) -> Services {
    let mut s = Services::default();
    let pause = move || {
        if slow {
            std::thread::sleep(Duration::from_secs(4));
        }
    };
    s.detect_agents = Arc::new(move || {
        pause();
        let found = |kind, version: &str, logged_in| DetectedAgent {
            kind,
            path: PathBuf::from("C:\\tools\\agent.exe"),
            version: Some(version.to_string()),
            logged_in,
        };
        vec![
            found(AgentKind::ClaudeCode, "2.1.330 (Claude Code)", Some(true)),
            found(AgentKind::OpenCode, "1.19.6", None),
            found(AgentKind::Acp { command: "gemini".into() }, "0.70.0", Some(false)),
        ]
    });
    s.detect_shells = Arc::new(move || {
        pause();
        let sh = |kind, name: &str, program: &str, detail: &str| ShellInfo {
            kind,
            name: name.into(),
            program: program.into(),
            args: vec![],
            detail: detail.into(),
        };
        vec![
            sh(ShellKind::Pwsh, "PowerShell 7", "pwsh.exe", "C:\\Users\\you\\AppData\\Local\\Microsoft\\WindowsApps\\pwsh.exe"),
            sh(ShellKind::WindowsPowerShell, "Windows PowerShell", "powershell.exe", "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            sh(ShellKind::Cmd, "Símbolo del sistema", "cmd.exe", "C:\\Windows\\System32\\cmd.exe"),
            sh(ShellKind::GitBash, "Git Bash", "bash.exe", "C:\\Program Files\\Git\\bin\\bash.exe"),
            sh(ShellKind::Wsl, "WSL: Ubuntu", "wsl.exe", "Distribución Ubuntu"),
        ]
    });
    s
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
    let start = arg_value(&args, "--step").and_then(|s| s.parse().ok()).and_then(Step::from_number).unwrap_or(Step::Welcome);
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or((960.0, 800.0));
    let fake = args.iter().any(|a| a == "--fake");
    let slow = args.iter().any(|a| a == "--slow");
    if args.iter().any(|a| a == "--reduced-motion") {
        ui_kit::set_reduced_motion(true);
    }

    let data = std::env::temp_dir().join(format!("selachi-onboarding-demo-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&data);
    // SAFETY: nothing else is running yet; this is the first thing `main` does with the environment.
    unsafe { std::env::set_var(store::paths::DATA_DIR_ENV, &data) };
    eprintln!("data dir: {}", data.display());

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
        let db = StateDb::open_default().ok();
        let config = Config::load_or_default().config;
        let services = if fake || slow { fake_services(slow) } else { Services::default() };
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let view = onboarding_with(config, db, Theme::for_mode(mode), services, start, cx);
                view.read(cx).focus(window);
                cx.subscribe(&view, |_, ev: &OnboardingEvent, cx| {
                    match ev {
                        OnboardingEvent::Finished { config, projects, favorites, extras } => {
                            eprintln!(
                                "Finished: preset={:?} projects={} favorites={} extras={extras:?}",
                                config.layout.preset,
                                projects.len(),
                                favorites.len()
                            );
                            cx.quit();
                        }
                        OnboardingEvent::Skipped => {
                            eprintln!("Skipped");
                            cx.quit();
                        }
                        other => eprintln!("{other:?}"),
                    }
                })
                .detach();
                view
            },
        );
        cx.activate(true);
    });
}
