//! gpui-level tests: edits persist (debounced, comments kept), validation, hot-reload merge,
//! plugin install end to end against the built zips, deep link, sidebar options.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Entity, TestAppContext};
use store::Config;
use store::config::TEMPLATE;
use theme::Theme;
use viewer_svc::grammars::GrammarRegistry;

use crate::net::LocalFetcher;
use crate::plugins::IndexState;
use crate::save::SAVE_DEBOUNCE;
use crate::view::{Section, SettingsEvent, SettingsOptions, SettingsView};

fn dist() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/grammars/dist")
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    view: Entity<SettingsView>,
}

fn fixture(cx: &mut TestAppContext, text: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).unwrap();
    let config = Config::load_from(&path).unwrap().config;
    let registry = Arc::new(GrammarRegistry::new(dir.path().join("plugins")));
    let options = SettingsOptions {
        registry: Some(registry),
        fetcher: Some(Arc::new(LocalFetcher {
            archives_dir: dist(),
        })),
        ..SettingsOptions::new(path.clone(), "0.0.1")
    };
    let view = cx.new(|cx| SettingsView::new(config, Theme::dark(), options, cx));
    Fixture {
        _dir: dir,
        path,
        view,
    }
}

fn settle(cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock(SAVE_DEBOUNCE + Duration::from_millis(50));
    cx.run_until_parked();
}

fn disk(f: &Fixture) -> String {
    std::fs::read_to_string(&f.path).unwrap()
}

#[gpui::test]
fn edits_are_saved_after_the_debounce_and_keep_comments(cx: &mut TestAppContext) {
    let text = format!(
        "# my header\n{}",
        TEMPLATE.replace("scrollback = 10000", "scrollback = 10000 # lines")
    );
    let f = fixture(cx, &text);
    f.view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| c.terminal.scrollback = 20_000);
        v.edit_cfg(cx, |c| c.terminal.copy_on_select = true);
    });
    // Nothing is written before the quiet period ends.
    cx.run_until_parked();
    assert!(!disk(&f).contains("scrollback = 20000"));
    assert!(f.view.read_with(cx, |v, _| v.has_pending_save()));
    settle(cx);
    let saved = disk(&f);
    assert!(saved.contains("# my header"), "{saved}");
    assert!(saved.contains("scrollback = 20000 # lines"), "{saved}");
    assert!(saved.contains("copy_on_select = true"), "{saved}");
    assert!(!f.view.read_with(cx, |v, _| v.has_pending_save()));
    assert_eq!(
        Config::parse(&saved).unwrap().config.terminal.scrollback,
        20_000
    );
}

#[gpui::test]
fn a_burst_of_edits_is_one_write(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    let saved = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = saved.clone();
    cx.update(|cx| {
        cx.subscribe(&f.view, move |_, ev: &SettingsEvent, _| {
            if matches!(ev, SettingsEvent::Saved) {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        })
        .detach();
    });
    for n in 1..=5u32 {
        f.view.update(cx, |v, cx| {
            v.edit_cfg(cx, |c| c.terminal.scrollback = 1000 * n)
        });
        cx.executor().advance_clock(Duration::from_millis(100));
    }
    settle(cx);
    assert_eq!(saved.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(disk(&f).contains("scrollback = 5000"));
}

#[gpui::test]
fn invalid_values_are_rejected_and_never_written(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    let before = disk(&f);
    f.view.update(cx, |v, cx| {
        let e = v.edit(cx, |c| c.appearance.mono_size = 2.0).unwrap_err();
        assert_eq!(e.field, "appearance.mono_size");
        assert!(e.message.contains("4 y 96"));
        v.edit_field("accent_hex", cx, |c| c.appearance.accent = "azul".into());
        assert!(v.errors.contains_key("accent_hex"));
        assert_eq!(
            v.config().appearance.accent,
            "#00AAFF",
            "the bad value is not applied"
        );
        // A valid value clears the error.
        v.edit_field("accent_hex", cx, |c| c.appearance.accent = "#112233".into());
        assert!(!v.errors.contains_key("accent_hex"));
        assert_eq!(v.config().appearance.accent, "#112233");
    });
    settle(cx);
    let saved = disk(&f);
    assert_ne!(saved, before);
    assert!(saved.contains("#112233") && !saved.contains("azul"));
}

#[gpui::test]
fn text_fields_apply_through_fields_apply(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        crate::fields::apply(v, "scrollback", "12 000", cx);
        assert_eq!(v.config().terminal.scrollback, 12_000);
        crate::fields::apply(v, "scrollback", "muchas", cx);
        assert!(v.errors.contains_key("scrollback"));
        assert_eq!(v.config().terminal.scrollback, 12_000);
        crate::fields::apply(v, "scrollback", "5000000", cx);
        assert!(
            v.errors.contains_key("scrollback"),
            "over the 1 000 000 limit"
        );
        crate::fields::apply(v, "accent_hex", "0af", cx);
        assert_eq!(v.config().appearance.accent, "#00AAFF");
    });
}

#[gpui::test]
fn profiles_can_be_added_edited_and_removed(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        v.edit_profile(0, cx); // nothing to edit yet: no-op on an empty list
    });
    f.view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| {
            c.profiles.push(store::config::Profile {
                name: "Dev".into(),
                program: "pwsh.exe".into(),
                ..Default::default()
            })
        });
        crate::fields::apply(v, "profile.0.args", "-NoLogo -Command \"echo hi\"", cx);
        crate::fields::apply(v, "profile.0.cwd", r"C:\code", cx);
        crate::fields::apply(v, "profile.0.program", "  ", cx);
        assert!(v.errors.contains_key("profile.0.program"));
        let p = &v.config().profiles[0];
        assert_eq!(p.args, ["-NoLogo", "-Command", "echo hi"]);
        assert_eq!(p.cwd.as_deref(), Some(r"C:\code"));
        assert_eq!(p.program, "pwsh.exe");
    });
    settle(cx);
    let saved = Config::parse(&disk(&f)).unwrap().config;
    assert_eq!(saved.profiles.len(), 1);
    assert_eq!(saved.profiles[0].args.len(), 3);
}

#[gpui::test]
fn hot_reload_does_not_clobber_an_edit_in_progress(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| c.terminal.scrollback = 7_000)
    });
    // The file changed elsewhere (another key) before our debounce fired.
    let mut theirs = Config::default();
    theirs.appearance.mono_size = 15.0;
    f.view.update(cx, |v, cx| v.set_config(theirs, cx));
    f.view.read_with(cx, |v, _| {
        assert_eq!(v.config().terminal.scrollback, 7_000, "our edit survives");
        assert_eq!(
            v.config().appearance.mono_size,
            15.0,
            "their change is adopted"
        );
    });
    settle(cx);
    let saved = Config::parse(&disk(&f)).unwrap().config;
    assert_eq!(saved.terminal.scrollback, 7_000);
}

#[gpui::test]
fn general_options_persist_and_the_https_only_rule_applies_to_the_index_url(
    cx: &mut TestAppContext,
) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| c.general.confirm_close_running = false);
        v.edit_field("index_url", cx, |c| {
            c.plugins.index_url = "http://evil.example/g.toml".into()
        });
        assert!(v.errors.contains_key("index_url"));
        assert!(
            v.config().plugins.index_url.is_empty(),
            "http:// is never adopted"
        );
        v.edit_field("index_url", cx, |c| {
            c.plugins.index_url = "https://example.com/g.toml".into()
        });
        assert!(!v.errors.contains_key("index_url"));
    });
    settle(cx);
    let saved = disk(&f);
    assert!(saved.contains("confirm_close_running = false"), "{saved}");
    assert!(saved.contains("https://example.com/g.toml"));
    assert!(!saved.contains("evil.example"));
    assert!(Config::parse(&saved).is_ok());
}

#[gpui::test]
fn sidebar_options_titlebar_buttons_and_group_by_persist(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| {
            let mut r = c.layout.resolve();
            r.sidebar.row_time = false;
            r.sidebar.group_by = store::GroupBy::Status;
            r.titlebar.panel_buttons.retain(|b| b.as_str() != "usage");
            c.layout.set_resolved(&r);
        });
    });
    settle(cx);
    let saved = disk(&f);
    assert!(saved.contains("row_time = false"), "{saved}");
    let layout = Config::parse(&saved).unwrap().config.layout.resolve();
    assert!(!layout.sidebar.row_time && layout.sidebar.show_actions);
    assert!(!layout.titlebar.has("usage") && layout.titlebar.has("git"));
    assert_eq!(layout.sidebar.group_by, store::GroupBy::Status);
}

#[gpui::test]
fn the_embedded_layout_panel_follows_config_edits_and_feeds_them_back(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    let (view, cx) = cx.add_window_view(|_, cx| {
        let dir = f._dir.path().to_path_buf();
        let cfg = Config::load_from(&dir.join("config.toml")).unwrap().config;
        let options = SettingsOptions {
            registry: Some(Arc::new(GrammarRegistry::new(dir.join("plugins2")))),
            fetcher: Some(Arc::new(LocalFetcher {
                archives_dir: dist(),
            })),
            ..SettingsOptions::new(dir.join("config.toml"), "0.0.1")
        };
        SettingsView::new(cfg, Theme::dark(), options, cx)
    });
    view.update(cx, |v, cx| v.set_section(Section::Design, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let panel = view
        .read_with(cx, |v, _| v.layout_panel.clone())
        .expect("Diseño builds the panel");
    // Settings -> panel.
    view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| {
            let mut r = c.layout.resolve();
            r.density = store::Density::Compact;
            c.layout.set_resolved(&r);
        })
    });
    let shown = panel.read_with(cx, |p, _| p.layout().density);
    assert_eq!(shown, store::Density::Compact);
    // Panel -> settings.
    let mut changed = view.read_with(cx, |v, _| v.config().layout.clone());
    changed.select_preset(store::BuiltinPreset::Chat.into());
    panel.update_in(cx, |p, window, cx| {
        p.edit_layout(window, cx, |l| l.splits = store::Splits::TwoVertical);
    });
    cx.run_until_parked();
    let splits = view.read_with(cx, |v, _| v.config().layout.resolve().splits);
    assert_eq!(splits, store::Splits::TwoVertical);
}

#[gpui::test]
fn deep_link_opens_plugins_with_the_confirmation(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        v.handle(
            &SettingsEvent::InstallGrammar {
                id: "csharp".into(),
            },
            cx,
        )
    });
    f.view.read_with(cx, |v, _| {
        assert_eq!(v.section(), Section::Plugins);
        assert_eq!(v.plugin_focus.as_deref(), Some("csharp"));
        assert_eq!(
            v.confirm.as_ref().map(|(id, _)| id.as_str()),
            Some("csharp")
        );
    });
}

#[gpui::test]
fn plugin_index_is_fetched_lazily_cached_and_a_plugin_installs(cx: &mut TestAppContext) {
    let dist = dist();
    if !dist.join("grammars.toml").exists() {
        eprintln!("skipped: packaging/grammars/dist is not built");
        return;
    }
    let f = fixture(cx, TEMPLATE);
    // Point the model at the local index (file:// only works through LocalFetcher).
    let url = format!(
        "file:///{}",
        dist.join("grammars.toml")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    );
    let plugins = f.view.read_with(cx, |v, _| v.plugins().clone());
    plugins.read_with(cx, |m, _| {
        assert_eq!(
            *m.index_state(),
            IndexState::Idle,
            "no fetch before Plugins opens"
        )
    });
    plugins.update(cx, |m, cx| m.set_index_url(url, cx));
    f.view
        .update(cx, |v, cx| v.set_section(Section::Plugins, cx));
    cx.run_until_parked();
    plugins.read_with(cx, |m, _| {
        assert_eq!(*m.index_state(), IndexState::Ready, "{:?}", m.index_state());
        assert!(m.rows().iter().any(|r| r.id == "csharp" && r.can_install()));
    });
    // Re-entering Plugins within the hour does not refetch (state stays Ready, no Loading flash).
    f.view.update(cx, |v, cx| v.set_section(Section::About, cx));
    f.view
        .update(cx, |v, cx| v.set_section(Section::Plugins, cx));
    plugins.read_with(cx, |m, _| assert_eq!(*m.index_state(), IndexState::Ready));

    let installed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = installed.clone();
    cx.update(|cx| {
        cx.subscribe(&f.view, move |_, ev: &SettingsEvent, _| {
            if let SettingsEvent::PluginInstalled { id, restart } = ev {
                sink.lock().unwrap().push((id.clone(), *restart));
            }
        })
        .detach();
    });
    plugins.update(cx, |m, cx| m.install("csharp", cx));
    cx.run_until_parked();
    assert_eq!(
        *installed.lock().unwrap(),
        vec![("csharp".to_string(), false)]
    );
    plugins.read_with(cx, |m, _| {
        let row = m.rows().into_iter().find(|r| r.id == "csharp").unwrap();
        assert_eq!(row.label(), "Instalado");
        assert!(row.can_remove());
    });
    // And it can be removed again.
    plugins.update(cx, |m, cx| m.uninstall("csharp", cx));
    cx.run_until_parked();
    plugins.read_with(cx, |m, _| {
        assert!(
            m.rows()
                .iter()
                .find(|r| r.id == "csharp")
                .unwrap()
                .can_install()
        )
    });
}

#[gpui::test]
fn a_failed_index_shows_an_error_and_retry_works(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    let plugins = f.view.read_with(cx, |v, _| v.plugins().clone());
    plugins.update(cx, |m, cx| {
        m.set_index_url("https://example.invalid/missing/nothing-here.toml", cx)
    });
    f.view
        .update(cx, |v, cx| v.set_section(Section::Plugins, cx));
    cx.run_until_parked();
    plugins.read_with(cx, |m, _| {
        assert!(matches!(m.index_state(), IndexState::Failed(e) if !e.is_empty()))
    });
    // Retry hits the network layer again (still failing here, but it ran).
    plugins.update(cx, |m, cx| m.refresh(cx));
    plugins.read_with(cx, |m, _| assert_eq!(*m.index_state(), IndexState::Loading));
    cx.run_until_parked();
    plugins.read_with(cx, |m, _| {
        assert!(matches!(m.index_state(), IndexState::Failed(_)))
    });
}

#[gpui::test]
fn flush_writes_immediately(cx: &mut TestAppContext) {
    let f = fixture(cx, TEMPLATE);
    f.view.update(cx, |v, cx| {
        v.edit_cfg(cx, |c| c.git.confirm_sensitive = false);
        v.flush();
    });
    assert!(disk(&f).contains("confirm_sensitive = false"));
}

// ----- keyboard and pointer contracts (ui_kit::testing: no real input is injected) ---------------

mod ui {
    use gpui::{TestAppContext, VisualTestContext, px, size};
    use store::config::{PredictionSource, TEMPLATE};
    use ui_kit::testing;

    use super::*;

    /// A settings view as the root of a test window, plus what keeps its config dir alive.
    struct Win {
        _dir: tempfile::TempDir,
        view: Entity<SettingsView>,
    }

    fn open(cx: &mut TestAppContext) -> (Win, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, TEMPLATE).unwrap();
        let config = Config::load_from(&path).unwrap().config;
        let options = SettingsOptions {
            registry: Some(Arc::new(GrammarRegistry::new(dir.path().join("plugins")))),
            fetcher: Some(Arc::new(LocalFetcher {
                archives_dir: dist(),
            })),
            ..SettingsOptions::new(path, "0.0.1")
        };
        let (view, vcx) = testing::open_window(cx, |_, cx| {
            SettingsView::new(config, Theme::dark(), options, cx)
        });
        (Win { _dir: dir, view }, vcx)
    }

    /// A 600 px window: the section list folds into a drop-down and rows stack their control under
    /// the text (the test text system is wider than a real font, so a side-by-side control would
    /// sit outside the window).
    fn shrink(vcx: &mut VisualTestContext) {
        vcx.simulate_resize(size(px(600.0), px(800.0)));
        // The probe measures the new width while painting; the next frame acts on it.
        for _ in 0..3 {
            vcx.update(|window, _| window.refresh());
            vcx.run_until_parked();
        }
    }

    #[gpui::test]
    fn clicking_a_setting_row_toggle_edits_the_config(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        shrink(vcx);
        w.view.update(vcx, |v, cx| v.set_section(Section::Git, cx));
        vcx.run_until_parked();
        let before = w
            .view
            .read_with(vcx, |v, _| v.config().git.confirm_sensitive);
        testing::click_id(vcx, "git-confirm");
        let after = w
            .view
            .read_with(vcx, |v, _| v.config().git.confirm_sensitive);
        assert_eq!(
            after, !before,
            "the click reached the toggle and the config"
        );
        testing::click_id(vcx, "git-confirm");
        assert_eq!(
            w.view
                .read_with(vcx, |v, _| v.config().git.confirm_sensitive),
            before
        );
    }

    #[gpui::test]
    fn tab_walks_the_section_list_and_then_the_controls(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        // General: 10 section pills, then the language select, then the first toggle.
        let stops = Section::ALL.len() + 1;
        for _ in 0..stops {
            testing::press(vcx, "tab");
        }
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.section()),
            Section::General,
            "tab alone never changes section"
        );
        let before = w.view.read_with(vcx, |v, _| v.config().session.restore);
        testing::press(vcx, "tab enter");
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.config().session.restore),
            !before,
            "Enter on the first toggle"
        );
        // Shift+Tab goes back to the select; Enter opens it instead of toggling anything.
        testing::press(vcx, "shift-tab enter");
        let open = w.view.read_with(vcx, |v, cx| {
            v.selects["general-lang"].entity.read(cx).is_open()
        });
        assert!(
            open,
            "shift-tab came back to the language select and Enter opened it"
        );
    }

    #[gpui::test]
    fn a_section_pill_changes_section_from_the_keyboard(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        // Tab to the second pill ("Apariencia") and activate it.
        testing::press(vcx, "tab tab enter");
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.section()),
            Section::Appearance
        );
    }

    #[gpui::test]
    fn typing_into_a_text_field_updates_the_config(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        w.view
            .update(vcx, |v, cx| v.set_section(Section::Terminal, cx));
        vcx.run_until_parked();
        let field = w.view.read_with(vcx, |v, _| v.fields["scrollback"].clone());
        vcx.update(|window, cx| field.read(cx).focus(window));
        testing::press(vcx, "ctrl-a");
        testing::type_text(vcx, "20 000");
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.config().terminal.scrollback),
            20_000
        );
        // Invalid text is not applied and the field shows the error.
        testing::press(vcx, "ctrl-a");
        testing::type_text(vcx, "muchas");
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.config().terminal.scrollback),
            20_000
        );
        vcx.run_until_parked();
        assert!(
            field.read_with(vcx, |f, _| f.error().is_some()),
            "the field is flagged"
        );
    }

    #[gpui::test]
    fn a_select_changes_value_from_the_keyboard(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        w.view
            .update(vcx, |v, cx| v.set_section(Section::Terminal, cx));
        vcx.run_until_parked();
        let select = w
            .view
            .read_with(vcx, |v, _| v.selects["prediction"].entity.clone());
        vcx.update(|window, cx| select.read(cx).focus(window));
        assert_eq!(
            w.view
                .read_with(vcx, |v, _| v.config().terminal.completion.prediction),
            PredictionSource::History
        );
        testing::press(vcx, "down down enter");
        assert_eq!(
            w.view
                .read_with(vcx, |v, _| v.config().terminal.completion.prediction),
            PredictionSource::HistoryAndPlugin
        );
        assert!(!select.read_with(vcx, |s, _| s.is_open()));
    }

    #[gpui::test]
    fn a_narrow_zone_swaps_the_section_list_for_a_drop_down(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        assert!(
            w.view
                .read_with(vcx, |v, _| !v.selects.contains_key("settings-section")),
            "wide: the list is shown"
        );
        shrink(vcx);
        assert!(
            w.view
                .read_with(vcx, |v, _| v.selects.contains_key("settings-section")),
            "narrow: a drop-down"
        );
        let select = w
            .view
            .read_with(vcx, |v, _| v.selects["settings-section"].entity.clone());
        vcx.update(|window, cx| select.read(cx).focus(window));
        // The first Down opens the list on the current section, the second moves to the next.
        testing::press(vcx, "down down enter");
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.section()),
            Section::Appearance
        );
    }
}

// ----- delegation, registration and updates ------------------------------------------------------

mod delegation {
    use std::sync::Mutex;

    use gpui::{TestAppContext, VisualTestContext, px, size};
    use store::config::TEMPLATE;
    use ui_kit::testing;

    use super::*;
    use crate::registration::{CommandRunner, McpTarget};

    /// Records what it was asked to run and runs nothing.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(McpTarget, PathBuf)>>,
    }

    impl CommandRunner for Fake {
        fn run(&self, target: McpTarget, agent_exe: &Path) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push((target, agent_exe.to_path_buf()));
            Ok("fake ok".into())
        }
    }

    struct Win {
        _dir: tempfile::TempDir,
        view: Entity<SettingsView>,
        fake: Arc<Fake>,
    }

    fn open(cx: &mut TestAppContext) -> (Win, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, TEMPLATE).unwrap();
        let config = Config::load_from(&path).unwrap().config;
        let fake = Arc::new(Fake::default());
        let options = SettingsOptions {
            registry: Some(Arc::new(GrammarRegistry::new(dir.path().join("plugins")))),
            fetcher: Some(Arc::new(LocalFetcher {
                archives_dir: dist(),
            })),
            agent_exe: Some(PathBuf::from(r"C:\Selachi\selachi-agent.exe")),
            runner: Some(fake.clone()),
            ..SettingsOptions::new(path, "0.0.1")
        };
        let (view, vcx) = testing::open_window(cx, |_, cx| {
            SettingsView::new(config, Theme::dark(), options, cx)
        });
        vcx.simulate_resize(size(px(600.0), px(3600.0)));
        for _ in 0..3 {
            vcx.update(|window, _| window.refresh());
            vcx.run_until_parked();
        }
        (
            Win {
                _dir: dir,
                view,
                fake,
            },
            vcx,
        )
    }

    #[gpui::test]
    fn the_delegation_switch_and_agent_chips_edit_the_config(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        w.view
            .update(vcx, |v, cx| v.set_section(Section::Agents, cx));
        vcx.run_until_parked();
        assert!(w.view.read_with(vcx, |v, _| v.config().delegation.enabled));
        testing::click_id(vcx, "deleg-enabled");
        assert!(!w.view.read_with(vcx, |v, _| v.config().delegation.enabled));
        testing::click_id(vcx, "deleg-agent-codex");
        let allowed = w
            .view
            .read_with(vcx, |v, _| v.config().delegation.allowed_agents.clone());
        assert_eq!(allowed, ["claude", "opencode", "gemini", "agy"]);
    }

    #[gpui::test]
    fn registering_runs_nothing_until_the_user_confirms(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        w.view
            .update(vcx, |v, cx| v.set_section(Section::Agents, cx));
        vcx.run_until_parked();
        // opening the dialog shows the command and runs nothing
        testing::click_id(vcx, "register-claude");
        assert!(w.fake.calls.lock().unwrap().is_empty());
        assert_eq!(
            w.view.read_with(vcx, |v, _| v.register.clone()),
            Some((
                McpTarget::ClaudeCode,
                crate::registration::RegPhase::Confirm
            )),
            "the dialog is open, nothing ran"
        );
        // "Cancelar" closes it (the popover body is not part of the test frame): still nothing
        w.view.update(vcx, |v, cx| {
            v.register = None;
            cx.notify();
        });
        assert_eq!(w.view.read_with(vcx, |v, _| v.register.clone()), None);
        assert!(w.fake.calls.lock().unwrap().is_empty());
        // the dialog's confirm button calls `run_registration`: now it runs, once, with the exact
        // executable (the popover body is not part of the test frame)
        w.view
            .update(vcx, |v, cx| v.run_registration(McpTarget::ClaudeCode, cx));
        vcx.run_until_parked();
        let calls = w.fake.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(
                McpTarget::ClaudeCode,
                PathBuf::from(r"C:\Selachi\selachi-agent.exe")
            )]
        );
        let ok = w.view.read_with(vcx, |v, _| {
            v.register_results.get(&McpTarget::ClaudeCode).cloned()
        });
        assert_eq!(ok, Some(Ok("fake ok".to_string())));
    }

    #[gpui::test]
    fn the_update_settings_edit_the_config_and_ask_the_host(cx: &mut TestAppContext) {
        let (w, vcx) = open(cx);
        assert!(w.view.read_with(vcx, |v, _| v.config().updates.check));
        testing::click_id(vcx, "updates-check");
        assert!(!w.view.read_with(vcx, |v, _| v.config().updates.check));
        testing::click_id(vcx, "updates-check");
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = asked.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&w.view, move |_, ev: &SettingsEvent, _| {
                if matches!(ev, SettingsEvent::CheckUpdates) {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .detach();
        });
        testing::click_id(vcx, "updates-check-now");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        // a ready update offers the restart
        w.view.update(vcx, |v, cx| {
            v.set_update_status(
                crate::view::UpdateStatus {
                    text: "Actualización 0.2.0 lista".into(),
                    busy: false,
                    ready: true,
                },
                cx,
            )
        });
        vcx.run_until_parked();
        assert!(testing::bounds_of(vcx, "updates-install-now").is_some());
    }
}
