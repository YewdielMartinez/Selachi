//! `[terminal]` and `[appearance]` settings reaching the open terminals and the theme: a reload (or
//! the settings screen) must change what the tabs do and how they look, not only the file.

use super::*;

use std::cell::RefCell;
use std::rc::Rc;

use store::config::{CursorStyle, Profile};

use crate::terminal_view::TermSettings;

/// A tab that records every theme it is given (a terminal tab hands it to its view).
struct ThemeLog {
    themes: Rc<RefCell<Vec<Theme>>>,
    body: Entity<Body>,
}

impl EventEmitter<TabEvent> for ThemeLog {}

impl TabModel for ThemeLog {
    fn title(&self, _: &App) -> SharedString {
        "log".into()
    }
    fn icon(&self, _: &App) -> TabIcon {
        TabIcon::Kind(ui_kit::Icon::Terminal)
    }
    fn content(&self, _: &App) -> AnyView {
        self.body.clone().into()
    }
    fn theme_changed(&mut self, theme: Theme, _: &mut Context<Self>) {
        self.themes.borrow_mut().push(theme);
    }
}

fn add_log_tab(rig: &Rig, vcx: &mut VisualTestContext) -> Rc<RefCell<Vec<Theme>>> {
    let themes = Rc::new(RefCell::new(Vec::new()));
    let log = themes.clone();
    vcx.update(|window, cx| {
        rig.shell.update(cx, |shell, cx| {
            let body = cx.new(|cx| Body {
                focus: cx.focus_handle(),
            });
            let tab = cx.new(|_| ThemeLog { themes: log, body });
            let handle = TabHandle::new(tab);
            let id = handle.id();
            shell.push_tab(handle, Vec::new(), cx);
            shell.show_new_tab(id, window, cx);
        })
    });
    vcx.run_until_parked();
    themes
}

fn reload(rig: &Rig, vcx: &mut VisualTestContext, change: impl FnOnce(&mut Config)) {
    vcx.update(|_, cx| {
        rig.shell.update(cx, |s, cx| {
            let mut next = (*s.config).clone();
            change(&mut next);
            s.adopt_config(Arc::new(next), cx);
        })
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn the_terminal_settings_are_published_and_follow_a_reload(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    let themes = add_log_tab(&rig, vcx);
    vcx.update(|_, cx| assert_eq!(TermSettings::current(cx), TermSettings::default()));

    let before = themes.borrow().len();
    reload(&rig, vcx, |c| {
        c.terminal.cursor_style = CursorStyle::Bar;
        c.terminal.copy_on_select = true;
        c.terminal.paste_ctrl_v = false;
        c.terminal.scrollback = 123;
        c.terminal.default_profile = "Dev".into();
        c.profiles.push(Profile {
            name: "Dev".into(),
            program: "cmd.exe".into(),
            ..Profile::default()
        });
    });
    vcx.update(|_, cx| {
        let s = TermSettings::current(cx);
        assert_eq!(s.cursor_style, CursorStyle::Bar);
        assert!(s.copy_on_select && !s.paste_ctrl_v);
        assert_eq!((s.scrollback, s.default_profile.as_str()), (123, "Dev"));
        assert_eq!(s.profiles.len(), 1, "new tabs find the profile");
    });
    assert!(
        themes.borrow().len() > before,
        "the open terminals are told to repaint (the cursor changed shape)"
    );

    // a reload that changes nothing of it leaves the tabs alone (zero idle cost)
    let before = themes.borrow().len();
    reload(&rig, vcx, |c| {
        c.chat.redact_copy_text = !c.chat.redact_copy_text
    });
    assert_eq!(themes.borrow().len(), before);
}

#[gpui::test]
fn the_terminal_font_size_and_leading_of_a_reload_reach_the_tabs(cx: &mut TestAppContext) {
    let (rig, vcx) = rig(cx, false);
    let themes = add_log_tab(&rig, vcx);
    let default = vcx.update(|_, cx| rig.shell.read(cx).theme.typography);
    assert_eq!(default.mono_size, 13.0);

    reload(&rig, vcx, |c| {
        c.appearance.mono_size = 19.0;
        c.appearance.line_height = 1.6;
    });
    let now = vcx.update(|_, cx| rig.shell.read(cx).theme.typography);
    assert_eq!((now.mono_size, now.mono_line_height), (19.0, 1.6));
    let last = *themes.borrow().last().expect("the tab got the new theme");
    assert_eq!(last.typography, now);
    // the colours are untouched by a typography change
    assert_eq!(last.colors, Theme::for_mode(Mode::Dark).colors);
}

#[test]
fn appearance_fonts_size_and_leading_build_the_theme_typography() {
    let mut config = Config::default();
    config.appearance.mono_size = 17.0;
    config.appearance.line_height = 1.45;
    let ty = theme_for(&config, gpui::WindowAppearance::Dark).typography;
    assert_eq!((ty.mono_size, ty.mono_line_height), (17.0, 1.45));
    // the preview and the Ctrl+Shift+T cycle keep them too
    let ty = themed(Mode::Light, &config).typography;
    assert_eq!((ty.mono_size, ty.mono_line_height), (17.0, 1.45));
}

#[test]
fn a_font_that_is_not_installed_falls_back_to_the_default() {
    theme::fonts::register_source(|| vec!["Cascadia Mono".to_string(), "Segoe UI".to_string()]);
    let mut config = Config::default();
    config.appearance.ui_font = "segoe ui".into();
    config.appearance.mono_font = "Cascadia Mono".into();
    let ty = theme_for(&config, gpui::WindowAppearance::Dark).typography;
    assert_eq!(
        (ty.ui_family, ty.mono_family),
        ("Segoe UI", "Cascadia Mono")
    );

    config.appearance.ui_font = "Not A Real Font".into();
    config.appearance.mono_font = "Also Missing".into();
    let ty = theme_for(&config, gpui::WindowAppearance::Dark).typography;
    assert_eq!((ty.ui_family, ty.mono_family), ("DM Sans", "Geist Mono"));
    assert!(!theme::fonts::is_available("Not A Real Font"));
    theme::fonts::clear_installed();
}
