//! Layout contract of every Ajustes screen, asserted from the boxes the components report
//! (`ui_kit::layout_probe`) and the text gpui painted (`gpui::text_probe`):
//!
//! * (a) every control stays inside its card;
//! * (b) the controls of a group that sit beside their text share one right edge (+-1 px);
//! * (c) no text box is a column: narrower than 24 px with more than 3 rows;
//! * a wrapped text never paints wider than its box, a field never collapses, and a text column
//!   never runs under its own control.
//!
//! Each section is opened at 760, 1000, 1250 and 1500 px, in dark and light, with the content
//! window tall enough to lay everything out (what scrolling shows is the same layout). The test
//! text system is wider than a real font, so these widths stress the layout more than the app.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{TestAppContext, VisualTestContext};
use store::Config;
use theme::Theme;
use ui_kit::testing;
use viewer_svc::grammars::GrammarRegistry;

use crate::layout_checks::{check, measure};
use crate::net::LocalFetcher;
use crate::view::{AgentsTab, Section, SettingsOptions, SettingsView};
use crate::{profile_templates, profile_to_toml};

const WIDTHS: [f32; 4] = [760.0, 1000.0, 1250.0, 1500.0];

/// A screen to open before measuring.
#[derive(Clone, Copy, Debug)]
enum Screen {
    Section(Section),
    /// Agentes > Perfiles, list only.
    AgentProfiles,
    /// Agentes > Perfiles with a profile open in the editor form.
    AgentProfileForm,
    /// Terminal with a shell profile open in its form.
    ShellProfileForm,
}

struct Rig {
    _dir: tempfile::TempDir,
    view: gpui::Entity<SettingsView>,
}

fn rig(cx: &mut TestAppContext, theme: Theme) -> (Rig, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    std::fs::create_dir_all(data.join("agents")).unwrap();
    for t in profile_templates() {
        let file = t.name.to_lowercase().replace(' ', "-").replace('é', "e");
        std::fs::write(
            data.join("agents").join(format!("{file}.toml")),
            profile_to_toml(&t),
        )
        .unwrap();
    }
    let path = data.join("config.toml");
    std::fs::write(&path, store::config::TEMPLATE).unwrap();
    let mut config = Config::load_from(&path).unwrap().config;
    config.profiles = vec![
        store::config::Profile {
            name: "PowerShell 7".into(),
            program: "pwsh.exe".into(),
            args: vec!["-NoLogo".into()],
            ..Default::default()
        },
        store::config::Profile {
            name: "Git Bash".into(),
            program: r"C:\Program Files\Git\bin\bash.exe".into(),
            args: vec!["--login".into(), "-i".into()],
            cwd: Some(r"C:\Users\venta\Desktop".into()),
            env: [("TERM".to_string(), "xterm-256color".to_string())].into(),
        },
    ];
    let dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/grammars/dist");
    let options = SettingsOptions {
        registry: Some(Arc::new(GrammarRegistry::new(data.join("plugins")))),
        fetcher: Some(Arc::new(LocalFetcher { archives_dir: dist })),
        agent_exe: Some(PathBuf::from(r"C:\Selachi\selachi-agent.exe")),
        ..SettingsOptions::new(path, "0.0.1")
    };
    let (view, vcx) = testing::open_window(cx, move |_, cx| {
        SettingsView::new(config, theme, options, cx)
    });
    (Rig { _dir: dir, view }, vcx)
}

fn open_screen(rig: &Rig, vcx: &mut VisualTestContext, screen: Screen) {
    match screen {
        Screen::Section(s) => rig.view.update(vcx, |v, cx| v.set_section(s, cx)),
        Screen::AgentProfiles => rig
            .view
            .update(vcx, |v, cx| v.set_agents_tab(AgentsTab::Profiles, cx)),
        Screen::AgentProfileForm => {
            rig.view
                .update(vcx, |v, cx| v.set_agents_tab(AgentsTab::Profiles, cx));
            let editor = rig.view.update(vcx, |v, cx| v.profile_editor(cx));
            vcx.run_until_parked();
            let first = profile_templates().remove(0);
            editor.update(vcx, |e, cx| {
                if !e.open_profile(&first.name, cx) {
                    e.start_new(Some(first.clone()), cx);
                }
            });
        }
        Screen::ShellProfileForm => rig.view.update(vcx, |v, cx| v.edit_profile(0, cx)),
    }
    vcx.run_until_parked();
}

fn screens() -> Vec<Screen> {
    let mut v: Vec<Screen> = Section::ALL.into_iter().map(Screen::Section).collect();
    v.push(Screen::AgentProfiles);
    v.push(Screen::AgentProfileForm);
    v.push(Screen::ShellProfileForm);
    v
}

fn run_all(cx: &mut TestAppContext, theme: Theme) {
    let mut bad = Vec::new();
    for screen in screens() {
        let (rig, vcx) = rig(cx, theme);
        open_screen(&rig, vcx, screen);
        for width in WIDTHS {
            let (boxes, texts) = measure(vcx, width);
            check(&screen, width, &boxes, &texts, &mut bad);
        }
    }
    bad.sort();
    bad.dedup();
    assert!(
        bad.is_empty(),
        "{} layout problems:
{}",
        bad.len(),
        bad.join(
            "
"
        )
    );
}

#[gpui::test]
fn every_screen_lays_out_cleanly_in_dark(cx: &mut TestAppContext) {
    run_all(cx, Theme::dark());
}

#[gpui::test]
fn every_screen_lays_out_cleanly_in_light(cx: &mut TestAppContext) {
    run_all(cx, Theme::light());
}
