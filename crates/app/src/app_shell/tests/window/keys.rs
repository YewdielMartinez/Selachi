//! The Teclado section of Ajustes drives the real shortcuts: `[keybindings]` rebinds the window
//! live, "Restablecer" brings the defaults back, and "Sistema" follows the OS light/dark setting.

use super::*;

use gpui::WindowAppearance;
use store::config::ThemeMode;
use ui_kit::testing;

fn mode(rig: &Rig, vcx: &mut VisualTestContext) -> Mode {
    vcx.update(|_, cx| rig.shell.read(cx).theme.mode)
}

fn adopt(rig: &Rig, vcx: &mut VisualTestContext, edit: impl FnOnce(&mut Config)) {
    vcx.update(|_, cx| {
        rig.shell.update(cx, |s, cx| {
            let mut config = (*s.config).clone();
            edit(&mut config);
            s.adopt_config(Arc::new(config), cx);
        })
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn a_rebound_shortcut_fires_on_the_new_chord_and_the_old_one_goes_quiet(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    add_tab(&rig, vcx, "uno", None);
    let start = mode(&rig, vcx);

    testing::press(vcx, "ctrl-shift-t");
    assert_ne!(mode(&rig, vcx), start, "the default chord works");
    testing::press(vcx, "ctrl-shift-t");
    testing::press(vcx, "ctrl-shift-t");
    assert_eq!(mode(&rig, vcx), start, "three presses cycle back");

    // The user rebinds "Cambiar tema" in Teclado; the settings view emits the new config.
    adopt(&rig, vcx, |c| {
        c.keybindings
            .insert("theme.toggle".into(), "ctrl+alt+j".into());
    });
    testing::press(vcx, "ctrl-shift-t");
    assert_eq!(
        mode(&rig, vcx),
        start,
        "the old chord no longer does anything"
    );
    testing::press(vcx, "ctrl-alt-j");
    assert_ne!(mode(&rig, vcx), start, "the new chord toggles the theme");
}

#[gpui::test]
fn restablecer_brings_the_default_chord_back(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    add_tab(&rig, vcx, "uno", None);
    let start = mode(&rig, vcx);
    adopt(&rig, vcx, |c| {
        c.keybindings
            .insert("theme.toggle".into(), "ctrl+alt+j".into());
    });
    testing::press(vcx, "ctrl-alt-j");
    assert_ne!(
        mode(&rig, vcx),
        start,
        "the custom chord works before the reset"
    );
    testing::press(vcx, "ctrl-alt-j");
    testing::press(vcx, "ctrl-alt-j");
    assert_eq!(mode(&rig, vcx), start);
    // "Restablecer todo" clears [keybindings]: the config the settings view emits.
    adopt(&rig, vcx, |c| c.keybindings.clear());
    testing::press(vcx, "ctrl-alt-j");
    assert_eq!(
        mode(&rig, vcx),
        start,
        "the custom chord is gone after the reset"
    );
    testing::press(vcx, "ctrl-shift-t");
    assert_ne!(mode(&rig, vcx), start, "the default chord is back");
}

#[gpui::test]
fn the_fullscreen_chord_of_general_is_the_one_that_toggles_fullscreen(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    add_tab(&rig, vcx, "uno", None);
    let full = |vcx: &mut VisualTestContext| vcx.update(|w, _| w.is_fullscreen());
    assert!(!full(vcx));
    testing::press(vcx, "f11");
    assert!(full(vcx), "F11 is the default");
    testing::press(vcx, "f11");
    assert!(!full(vcx));

    // General -> Pantalla completa -> Cambiar writes keybindings.window.fullscreen.
    adopt(&rig, vcx, |c| {
        c.keybindings
            .insert("window.fullscreen".into(), "ctrl+alt+f".into());
    });
    testing::press(vcx, "f11");
    assert!(!full(vcx), "F11 was released");
    testing::press(vcx, "ctrl-alt-f");
    assert!(full(vcx), "the new chord toggles fullscreen");
}

#[gpui::test]
fn two_actions_on_one_chord_do_not_both_fire_nor_break_the_others(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    add_tab(&rig, vcx, "uno", None);
    let start = mode(&rig, vcx);
    // A hand-edited config puts "Cambiar tema" on Ctrl+W (the chord of "Cerrar pestaña").
    adopt(&rig, vcx, |c| {
        c.keybindings.insert("theme.toggle".into(), "ctrl+w".into());
    });
    testing::press(vcx, "ctrl-w");
    assert_ne!(
        mode(&rig, vcx),
        start,
        "the explicit user choice wins the chord"
    );
    // An unrelated shortcut keeps working.
    let zone_before = vcx.update(|w, cx| rig.shell.read(cx).current_zone(w, cx));
    testing::press(vcx, "ctrl-shift-e");
    let zone_after = vcx.update(|w, cx| rig.shell.read(cx).current_zone(w, cx));
    assert_ne!(zone_before, zone_after);
}

#[gpui::test]
fn sistema_starts_with_the_os_appearance_not_always_dark(cx: &mut TestAppContext) {
    // The test platform reports Light.
    let (rig, vcx) = rig(cx, false);
    adopt(&rig, vcx, |c| c.appearance.mode = ThemeMode::System);
    assert_eq!(mode(&rig, vcx), Mode::Light, "the OS is light");
    adopt(&rig, vcx, |c| c.appearance.mode = ThemeMode::Dark);
    assert_eq!(mode(&rig, vcx), Mode::Dark, "explicit modes ignore the OS");
}

#[gpui::test]
fn sistema_follows_the_os_when_it_switches_while_the_app_runs(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    adopt(&rig, vcx, |c| c.appearance.mode = ThemeMode::System);
    assert_eq!(mode(&rig, vcx), Mode::Light);
    let os = |rig: &Rig, vcx: &mut VisualTestContext, a| {
        vcx.update(|_, cx| rig.shell.update(cx, |s, cx| s.os_appearance_changed(a, cx)))
    };
    os(&rig, vcx, WindowAppearance::Dark);
    assert_eq!(mode(&rig, vcx), Mode::Dark, "the OS went dark");
    os(&rig, vcx, WindowAppearance::VibrantLight);
    assert_eq!(mode(&rig, vcx), Mode::Light);

    // OLED, Light and Dark are explicit: the OS does not move them.
    for (explicit, shown) in [
        (ThemeMode::Oled, Mode::Oled),
        (ThemeMode::Light, Mode::Light),
        (ThemeMode::Dark, Mode::Dark),
    ] {
        adopt(&rig, vcx, |c| c.appearance.mode = explicit);
        os(&rig, vcx, WindowAppearance::Light);
        assert_eq!(mode(&rig, vcx), shown);
        os(&rig, vcx, WindowAppearance::Dark);
        assert_eq!(mode(&rig, vcx), shown);
    }
}

#[gpui::test]
fn the_palette_shows_the_shortcut_the_action_has_now(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    let shortcut_of = |vcx: &mut VisualTestContext,
                       overrides: &std::collections::BTreeMap<String, String>,
                       id: &str| {
        vcx.update(|_, cx| {
            crate::palette::palette_actions(overrides, cx)
                .into_iter()
                .find(|e| e.id.as_ref() == id)
                .and_then(|e| e.shortcut)
                .map(|s| s.to_string())
        })
    };
    let none = Default::default();
    assert_eq!(
        shortcut_of(vcx, &none, "theme.toggle").as_deref(),
        Some("Ctrl+Shift+T")
    );
    let mut ov = std::collections::BTreeMap::new();
    ov.insert("theme.toggle".to_string(), "ctrl+alt+j".to_string());
    assert_eq!(
        shortcut_of(vcx, &ov, "theme.toggle").as_deref(),
        Some("Ctrl+Alt+J")
    );
    let _ = rig;
}
