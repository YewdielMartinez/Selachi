//! The layout contract of every Ajustes screen as the REAL shell shows it: the settings tab of the
//! main zone (zone width, `MeasuredWidth`, cached views, the width class pushed by the shell), not
//! the standalone settings demo. The checks are the ones of `ui_settings::layout_checks` (controls
//! inside their card, one right edge, no letter columns, no collapsed fields, one-word pills on
//! one row, errors wrapped inside their column).
//!
//! The panes are ~520, ~700, ~900 and ~1200 logical px wide. The test window uses gpui's test text
//! system (wider than a real font); DirectWrite is covered by the screenshots
//! `docs/screenshots/w11-app-settings-*.png`.

use super::*;

use ui_kit::layout_probe;
use ui_settings::layout_checks::{check, measure};
use ui_settings::{AgentsTab, Section};

const PANES: [f32; 4] = [520.0, 700.0, 900.0, 1200.0];

#[derive(Clone, Copy, Debug)]
enum Screen {
    Section(Section),
    AgentProfiles,
}

fn screens() -> Vec<Screen> {
    let mut v: Vec<Screen> = Section::ALL.into_iter().map(Screen::Section).collect();
    v.push(Screen::AgentProfiles);
    v
}

/// Window width that gives the settings tab a pane `pane` px wide. The shell's chrome changes with
/// the window's width class (rail, drawers, sidebar), so the width is searched on the shell itself.
fn window_for_pane(rig: &Rig, vcx: &mut VisualTestContext, pane: f32) -> f32 {
    let mut best = (f32::MAX, 480.0);
    let mut w = 480.0;
    while w <= 1800.0 {
        vcx.simulate_resize(gpui::size(gpui::px(w), gpui::px(900.0)));
        vcx.run_until_parked();
        let got = vcx.update(|_, cx| rig.shell.read(cx).main_pane_width());
        if (got - pane).abs() < best.0 {
            best = ((got - pane).abs(), w);
        }
        w += 4.0;
    }
    best.1
}

fn open(rig: &Rig, vcx: &mut VisualTestContext, screen: Screen, invalid: bool) {
    vcx.update(|window, cx| {
        rig.shell.update(cx, |shell, cx| {
            shell.open_settings(window, cx);
            let v = shell.settings_view().expect("settings view");
            v.update(cx, |s, cx| match screen {
                Screen::Section(sec) => s.set_section(sec, cx),
                Screen::AgentProfiles => s.set_agents_tab(AgentsTab::Profiles, cx),
            });
        })
    });
    vcx.run_until_parked();
    if invalid {
        vcx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(6000.0)));
        for _ in 0..3 {
            vcx.update(|window, _| window.refresh());
            vcx.run_until_parked();
        }
        vcx.update(|_, cx| {
            rig.shell.update(cx, |shell, cx| {
                let v = shell.settings_view().expect("settings view");
                v.update(cx, |s, cx| s.demo_invalid_values(cx));
            })
        });
        vcx.run_until_parked();
    }
}

fn run(cx: &mut TestAppContext, invalid: bool) {
    let mut bad = Vec::new();
    // the shell's chrome does not depend on the screen: search the widths once
    let windows: Vec<f32> = {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        open(&rig, vcx, Screen::Section(Section::General), false);
        PANES.iter().map(|p| window_for_pane(&rig, vcx, *p)).collect()
    };
    for screen in screens() {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        open(&rig, vcx, screen, invalid && matches!(screen, Screen::Section(Section::Agents)));
        for (pane, window) in PANES.into_iter().zip(windows.iter().copied()) {
            let (boxes, texts) = measure(vcx, window);
            let got = vcx.update(|_, cx| rig.shell.read(cx).main_pane_width());
            assert!(
                (got - pane).abs() < 8.0,
                "window {window} gave a {got} px pane, wanted {pane}"
            );
            let label = format!("{screen:?} (pane {pane})");
            check(&label, window, &boxes, &texts, &mut bad);
        }
        let _ = layout_probe::take();
    }
    bad.sort();
    bad.dedup();
    assert!(
        bad.is_empty(),
        "{} layout problems in the app:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[gpui::test]
fn every_settings_screen_lays_out_cleanly_inside_the_real_shell(cx: &mut TestAppContext) {
    run(cx, false);
}

#[gpui::test]
fn field_errors_wrap_under_their_field_inside_the_real_shell(cx: &mut TestAppContext) {
    run(cx, true);
}
