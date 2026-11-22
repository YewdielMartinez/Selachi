//! The settings view in a window, with a temporary config directory and a LOCAL plugin index.
//!
//! ```text
//! cargo run -p ui_settings --example settings_demo --features dev-file-urls -- \
//!     [dark|light|oled] [--section general|appearance|design|terminal|agents|git|processes|plugins|keyboard|about] \
//!     [--size WxH] [--static] [--edit-profile] [--confirm <plugin id>] [--capture <action>] [--keep]
//! ```
//!
//! * Config: a fresh temp folder (`SELACHI_DATA_DIR`-style, never the real `%APPDATA%`), seeded with
//!   the commented template, two shell profiles and a custom key binding. Edits are saved there:
//!   the stdout log prints every event and the final `config.toml` path.
//! * Plugins: the index is `packaging/grammars/dist/grammars.toml` (copied to the temp folder with
//!   the Rust grammar's version bumped, so an update shows up) served through `LocalFetcher`
//!   (`file://` index; archives are the built zips in `packaging/grammars/dist`). `csharp` and `rust`
//!   start installed. `file://` is only possible because of the `dev-file-urls` feature.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use store::Config;
use store::config::{Profile, ThemeMode};
use theme::{Mode, Theme};
use ui_kit::{Assets, Card, CardTone};
use ui_settings::{
    LocalFetcher, Section, SettingsEvent, SettingsOptions, SettingsView, settings_view,
};
use viewer_svc::grammars::{GrammarIndex, GrammarRegistry};

struct Host {
    theme: Theme,
    shield: bool,
    view: Entity<SettingsView>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        div()
            .size_full()
            .bg(t.colors.bg)
            .text_color(t.colors.text)
            .font_family(t.typography.ui_family)
            .p(px(8.0))
            .child(Card::new(&t).tone(CardTone::Base, &t).size_full().overflow_hidden().child(self.view.clone()))
            .when(self.shield, |d| d.child(div().id("shield").absolute().top_0().left_0().size_full().occlude()))
    }
}

/// The theme for a config: mode (System follows dark here) and accent.
fn theme_for(cfg: &Config) -> Theme {
    let mut t = match cfg.appearance.mode {
        ThemeMode::Light => Theme::light(),
        ThemeMode::Oled => Theme::oled(),
        ThemeMode::Dark | ThemeMode::System => Theme::dark(),
    };
    if let Some((r, g, b)) = cfg.appearance.accent_rgb() {
        t.colors.accent = theme::rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b));
    }
    t
}

fn dist_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("packaging").join("grammars").join("dist")
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
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
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x').map(|(a, b)| (a.to_string(), b.to_string())))
        .and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))
        .unwrap_or((1180.0, 900.0));
    let section = match arg_value(&args, "--section").as_deref() {
        Some("appearance") => Section::Appearance,
        Some("design") => Section::Design,
        Some("terminal") => Section::Terminal,
        Some("agents") => Section::Agents,
        Some("git") => Section::Git,
        Some("processes") => Section::Processes,
        Some("plugins") => Section::Plugins,
        Some("keyboard") => Section::Keyboard,
        Some("about") => Section::About,
        _ => Section::General,
    };
    let edit_profile = args.iter().any(|a| a == "--edit-profile");
    let confirm = arg_value(&args, "--confirm");
    let capture = arg_value(&args, "--capture");
    let scroll_end = args.iter().any(|a| a == "--scroll-end");
    let shield = args.iter().any(|a| a == "--static");

    // ---- temp config dir, seeded ----
    let dir = std::env::temp_dir().join(format!("selachi-settings-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let config_path = dir.join("config.toml");
    let mut config = Config::load_or_default_at(&config_path).config;
    config.appearance.mode = match mode {
        Mode::Light => ThemeMode::Light,
        Mode::Oled => ThemeMode::Oled,
        Mode::Dark => ThemeMode::Dark,
    };
    config.profiles = vec![
        Profile {
            name: "PowerShell 7".into(),
            program: "pwsh.exe".into(),
            args: vec!["-NoLogo".into()],
            ..Profile::default()
        },
        Profile {
            name: "Git Bash".into(),
            program: r"C:\Program Files\Git\bin\bash.exe".into(),
            args: vec!["--login".into(), "-i".into()],
            cwd: Some(r"C:\Users\venta\Desktop".into()),
            env: [("TERM".to_string(), "xterm-256color".to_string())].into(),
        },
    ];
    config.terminal.default_profile = "PowerShell 7".into();
    config.keybindings.insert("tab.new".into(), "ctrl+t".into());
    config.save_to(&config_path).expect("seed config");

    // ---- local plugin index + registry ----
    let dist = dist_dir();
    let index_path = dir.join("grammars.toml");
    let registry = Arc::new(GrammarRegistry::new(dir.join("plugins").join("grammars")));
    match std::fs::read_to_string(dist.join("grammars.toml")) {
        Ok(text) => {
            let mut index = GrammarIndex::parse(&text).expect("dist index");
            // Pre-install csharp and rust from the built zips, then offer a newer rust.
            for id in ["csharp", "rust"] {
                if let Some(entry) = index.entry(id).cloned()
                    && let Ok(bytes) = std::fs::read(dist.join(format!("{id}-{}.zip", entry.version)))
                {
                    let _ = registry.install_from_archive(&bytes, &entry);
                }
            }
            if let Some(rust) = index.grammars.iter_mut().find(|g| g.lang == "rust") {
                rust.version = "0.24.9".into();
            }
            std::fs::write(&index_path, index.to_toml()).expect("write demo index");
        }
        Err(e) => eprintln!("no packaging/grammars/dist ({e}): the Plugins list will show the index error"),
    }
    // The demo reads the index from a file:// URL (only the LocalFetcher allows that).
    let url = format!("file:///{}", index_path.display().to_string().replace('\\', "/"));
    let text = std::fs::read_to_string(&config_path).unwrap_or_default();
    std::fs::write(&config_path, format!("{text}\n[plugins]\nindex_url = \"{url}\"\n")).expect("config url");
    println!("config: {}\nindex:  {url}", config_path.display());

    let options = SettingsOptions {
        registry: Some(registry),
        fetcher: Some(Arc::new(LocalFetcher { archives_dir: dist })),
        ..SettingsOptions::new(config_path, "0.0.1")
    };

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            eprintln!("fonts: {e}");
        }
        ui_settings::init(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |_window, cx| {
                let theme = theme_for(&config);
                let view = settings_view(config, theme, options, cx);
                let host = cx.new(|cx| {
                    cx.subscribe(&view, |host: &mut Host, view, ev: &SettingsEvent, cx| match ev {
                        SettingsEvent::ConfigChanged(cfg) => {
                            host.theme = theme_for(cfg);
                            let theme = host.theme;
                            view.update(cx, |v, cx| v.set_theme(theme, cx));
                            cx.notify();
                        }
                        other => println!("event: {other:?}"),
                    })
                    .detach();
                    Host { theme, shield, view: view.clone() }
                });
                let shown = host.read(cx).view.clone();
                shown.update(cx, |v, cx| {
                    v.set_section(section, cx);
                    if scroll_end {
                        v.scroll_to_end();
                    }
                    if edit_profile {
                        v.edit_profile(0, cx);
                    }
                    if let Some(id) = &confirm {
                        v.open_plugin(id, cx);
                    }
                    if let Some(a) = &capture {
                        v.capture_shortcut(a, cx);
                    }
                });
                host
            },
        );
        cx.activate(true);
    });
}
