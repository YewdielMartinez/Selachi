use super::*;

#[test]
fn tab_cycling_wraps() {
    assert_eq!(cycle_index(3, 2, 1), 0);
    assert_eq!(cycle_index(3, 0, -1), 2);
    assert_eq!(cycle_index(3, 1, 1), 2);
    assert_eq!(cycle_index(0, 0, 1), 0);
    assert_eq!(cycle_index(1, 0, 1), 0);
}

#[test]
fn sidebar_drag_clamps_and_flips_on_the_right() {
    assert_eq!(dragged_width(SidebarSide::Left, 240, 40.0), 280);
    assert_eq!(dragged_width(SidebarSide::Right, 240, 40.0), 200);
    assert_eq!(dragged_width(SidebarSide::Left, 240, -500.0), 180);
    assert_eq!(dragged_width(SidebarSide::Left, 240, 900.0), 480);
}

#[test]
fn right_panel_drag_grows_leftwards_and_clamps() {
    assert_eq!(dragged_right_width(360.0, -40.0), 400.0);
    assert_eq!(dragged_right_width(360.0, 40.0), 320.0);
    assert_eq!(dragged_right_width(360.0, 900.0), 240.0);
    assert_eq!(dragged_right_width(360.0, -900.0), 520.0);
}

#[test]
fn drawers_never_exceed_the_window() {
    assert_eq!(drawer_width(1000.0), 320.0);
    assert_eq!(drawer_width(300.0), 270.0);
}

#[test]
fn zone_widths_follow_the_layout() {
    use store::BuiltinPreset as B;
    let term = ResolvedLayout::builtin(B::Terminal);
    assert_eq!(AppShell::sidebar_zone_w(&term, SidebarSide::Left), 0.0);
    assert_eq!(AppShell::rail_w(&term), RAIL_W);
    let orq = ResolvedLayout::builtin(B::Orquestador);
    assert_eq!(
        AppShell::sidebar_zone_w(&orq, SidebarSide::Left),
        300.0 + 4.0
    );
    assert_eq!(AppShell::sidebar_zone_w(&orq, SidebarSide::Right), 0.0);
    let zen = ResolvedLayout::builtin(B::Zen);
    assert_eq!(AppShell::rail_w(&zen), 0.0);
    let chat = ResolvedLayout::builtin(B::Chat);
    assert_eq!(AppShell::rail_w(&chat), 0.0);
}

#[test]
fn unified_layout_dedupes_usage_and_never_gets_the_full_bar() {
    use store::BuiltinPreset as B;
    let mut l = ResolvedLayout::builtin(B::EspaciosUnificados);
    l.bottom_bar = BottomBar::Full;
    let eff = adapt_for_shell(&l, 1440.0, false);
    assert!(dedupes_usage(&eff));
    assert_eq!(eff.layout.bottom_bar, BottomBar::Compact);
    // Icon-only column or a narrow window: the footer meter is not on screen, the pill returns.
    assert!(!dedupes_usage(&adapt_for_shell(&l, 1440.0, true)));
    assert!(!dedupes_usage(&adapt_for_shell(&l, 800.0, false)));
    // The classic presets keep their bar and their usage pill.
    let mut orq = ResolvedLayout::builtin(B::Orquestador);
    orq.bottom_bar = BottomBar::Full;
    let eff = adapt_for_shell(&orq, 1440.0, false);
    assert!(!dedupes_usage(&eff));
    assert_eq!(eff.layout.bottom_bar, BottomBar::Full);
}

#[test]
fn unified_preset_shows_the_panel_buttons_once() {
    let l = ResolvedLayout::builtin(store::BuiltinPreset::EspaciosUnificados);
    // The titlebar holds them; the sidebar's icon row stays off unless the user asks for it.
    assert!(!l.titlebar.panel_buttons.is_empty());
    // the subagent map has its button next to the other panels (ola 8C)
    assert!(
        l.titlebar
            .panel_buttons
            .iter()
            .any(|b| b.as_str() == "agentmap")
    );
    assert!(!l.sidebar.panel_icons);
}

#[test]
fn pane_counts_match_the_layouts() {
    assert_eq!(pane_count(Splits::Grid2x2), 4);
}

// ---------------------------------------------------------------------------------------------
// gpui tests: the shell in a test window (`ui_kit::testing`), with fake tabs. Nothing here opens a
// real window, spawns a shell or sends input to the desktop.
// ---------------------------------------------------------------------------------------------

mod window {
    use super::*;
    use std::cell::RefCell;

    use gpui::{EventEmitter, KeyBinding, TestAppContext, VisualTestContext, Window, div};
    use panel_api::{PanelProvider, TabEvent, TabIcon, TabModel};
    use ui_kit::testing;

    use crate::repaint::{self, View};
    use crate::sidebar_view::CloseFocusedRow;

    struct Body {
        focus: FocusHandle,
    }

    impl Render for Body {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().track_focus(&self.focus)
        }
    }

    /// A tab without a process: a focusable body and, optionally, a reason to ask before closing.
    struct FakeTab {
        title: &'static str,
        confirm: Option<&'static str>,
        body: Entity<Body>,
    }

    impl EventEmitter<TabEvent> for FakeTab {}

    impl TabModel for FakeTab {
        fn title(&self, _: &App) -> SharedString {
            self.title.into()
        }
        fn icon(&self, _: &App) -> TabIcon {
            TabIcon::Kind(ui_kit::Icon::Terminal)
        }
        fn content(&self, _: &App) -> AnyView {
            self.body.clone().into()
        }
        fn can_close(&self, _: &App) -> CloseCheck {
            match self.confirm {
                Some(why) => CloseCheck::Confirm(why.into()),
                None => CloseCheck::Allow,
            }
        }
        fn focus(&self, window: &mut Window, cx: &App) {
            self.body.read(cx).focus.focus(window);
        }
    }

    /// A right panel that records the width class it was given.
    #[derive(Clone, Default)]
    struct FakePanel {
        classes: Rc<RefCell<Vec<WidthClass>>>,
    }

    impl PanelProvider for FakePanel {
        fn id(&self) -> PanelId {
            PanelId::new("fake-panel")
        }
        fn title(&self) -> SharedString {
            "Fake".into()
        }
        fn icon(&self) -> ui_kit::Icon {
            ui_kit::Icon::Folder
        }
        fn allowed_zones(&self) -> &'static [PanelZone] {
            &[PanelZone::Right]
        }
        fn build(&self, ctx: PanelCtx, _: &mut Window, cx: &mut App) -> AnyView {
            self.classes.borrow_mut().push(ctx.width_class);
            cx.new(|cx| Body {
                focus: cx.focus_handle(),
            })
            .into()
        }
        fn context_changed(
            &self,
            _: &AnyView,
            ctx: &PanelCtx,
            _: &mut Window,
            _: &mut App,
        ) -> bool {
            self.classes.borrow_mut().push(ctx.width_class);
            true
        }
    }

    fn isolate_data_dir() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir =
                std::env::temp_dir().join(format!("selachi-shell-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).ok();
            // SAFETY: set once, before the first test builds a shell; it only redirects where
            // the stores of the test process live.
            unsafe { std::env::set_var(store::paths::DATA_DIR_ENV, &dir) };
        });
    }

    struct Rig {
        shell: Entity<AppShell>,
        panel: FakePanel,
    }

    /// A shell in the "Espacios unificados" layout (sidebar docked, tabs as sidebar rows, no
    /// quick actions so the first tab stop of the sidebar is the first tab row), with the fake
    /// panel as the right panel when asked.
    fn rig(cx: &mut TestAppContext, right_panel: bool) -> (Rig, &mut VisualTestContext) {
        isolate_data_dir();
        let panel = FakePanel::default();
        let for_registry = panel.clone();
        cx.update(|cx| {
            // The same bindings the app installs at start.
            crate::keybindings::bind_static(cx);
            crate::keybindings::install(cx, &Default::default());
            let mut registry = PanelRegistry::default();
            registry.register(for_registry);
            registry.register(crate::chat_host::ThreadsProvider);
            cx.set_global(registry);
            let services = Services::new(cx);
            cx.set_global(services);
        });
        let mut config = Config::default();
        config
            .layout
            .select_preset(store::BuiltinPreset::EspaciosUnificados.into());
        let mut target = config.global_layout();
        target.sidebar.show_actions = false;
        target.right_panel = right_panel.then(|| PanelId::new("fake-panel"));
        let mut layout = config.layout.clone();
        layout.set_resolved(&target);
        config.set_layout(layout);
        let boot = ConfigBoot {
            config: Arc::new(config),
            watcher: None,
            session: SessionBoot::default(),
            start_dir: None,
        };
        let theme = Theme::for_mode(Mode::Dark);
        let (shell, vcx) = testing::open_window(cx, |window, cx| {
            let (shell, _) = AppShell::build(theme, boot, window, cx);
            shell
        });
        (Rig { shell, panel }, vcx)
    }

    fn add_tab(
        rig: &Rig,
        vcx: &mut VisualTestContext,
        title: &'static str,
        confirm: Option<&'static str>,
    ) -> TabId {
        let id = vcx.update(|window, cx| {
            rig.shell.update(cx, |shell, cx| {
                let body = cx.new(|cx| Body {
                    focus: cx.focus_handle(),
                });
                let tab = cx.new(|_| FakeTab {
                    title,
                    confirm,
                    body,
                });
                let handle = TabHandle::new(tab);
                let id = handle.id();
                shell.push_tab(handle, Vec::new(), cx);
                shell.show_new_tab(id, window, cx);
                id
            })
        });
        vcx.run_until_parked();
        id
    }

    fn tab_count(rig: &Rig, vcx: &mut VisualTestContext) -> usize {
        vcx.update(|_, cx| rig.shell.read(cx).tabs.len())
    }

    fn zone(rig: &Rig, vcx: &mut VisualTestContext) -> Zone {
        vcx.update(|window, cx| rig.shell.read(cx).current_zone(window, cx))
    }

    #[gpui::test]
    fn sidebar_hover_does_not_notify_the_shell(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        let a = add_tab(&rig, vcx, "uno", None);
        add_tab(&rig, vcx, "dos", None);
        vcx.run_until_parked();

        let notified = Rc::new(Cell::new(0u32));
        let _sub = vcx.update(|_, cx| {
            let n = notified.clone();
            cx.observe(&rig.shell, move |_, _| n.set(n.get() + 1))
        });
        let (shell0, side0, bar0) = (
            repaint::count(View::Shell),
            repaint::count(View::Sidebar),
            repaint::count(View::TitleBar),
        );

        // The pointer enters the first row: a real mouse move over the row's bounds.
        let bounds = testing::bounds_of(vcx, &format!("urow-{}", a.as_u64()))
            .expect("the first tab row is on screen");
        vcx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::none());
        vcx.run_until_parked();

        let sidebar = vcx.update(|_, cx| rig.shell.read(cx).chrome.left.clone());
        assert_eq!(
            vcx.update(|_, cx| sidebar.read(cx).hovered_row()),
            Some(a),
            "the hover lives in the sidebar view"
        );
        let (shell1, side1, bar1) = (
            repaint::count(View::Shell),
            repaint::count(View::Sidebar),
            repaint::count(View::TitleBar),
        );
        eprintln!(
            "repaint scope of one sidebar hover: shell {} sidebar {} titlebar {}",
            shell1 - shell0,
            side1 - side0,
            bar1 - bar0
        );
        assert_eq!(notified.get(), 0, "the shell entity was not notified");
        assert!(side1 > side0, "the sidebar repainted");
        assert_eq!(
            bar1, bar0,
            "the cached title bar was reused, not re-rendered"
        );
    }

    #[gpui::test]
    fn leaving_another_space_does_not_clear_the_hovered_one(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        let sidebar = vcx.update(|_, cx| rig.shell.read(cx).chrome.left.clone());
        let (a, b) = (PathBuf::from("C:/a"), PathBuf::from("C:/b"));
        vcx.update(|_, cx| {
            sidebar.update(cx, |v, cx| {
                v.set_space_hover(&a, true, cx);
                // The next header's "entered" arrives before the previous one's "left".
                v.set_space_hover(&b, true, cx);
                v.set_space_hover(&a, false, cx);
            });
            assert_eq!(sidebar.read(cx).hovered_space(), Some(&b));
        });
    }

    #[gpui::test]
    fn f6_cycles_the_zones(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, true);
        add_tab(&rig, vcx, "uno", None);
        add_tab(&rig, vcx, "dos", None);
        vcx.run_until_parked();
        let zones = vcx.update(|_, cx| rig.shell.read(cx).visible_zones());
        assert_eq!(zones, vec![Zone::Sidebar, Zone::Main, Zone::Right]);
        // A new tab has the focus: the main zone.
        assert_eq!(zone(&rig, vcx), Zone::Main);
        testing::press(vcx, "f6");
        assert_eq!(zone(&rig, vcx), Zone::Right);
        testing::press(vcx, "f6");
        assert_eq!(zone(&rig, vcx), Zone::Sidebar);
        testing::press(vcx, "f6");
        assert_eq!(zone(&rig, vcx), Zone::Main);
        testing::press(vcx, "shift-f6");
        assert_eq!(zone(&rig, vcx), Zone::Sidebar);
        // The shortcuts go straight to a zone.
        testing::press(vcx, "ctrl-shift-`");
        assert_eq!(zone(&rig, vcx), Zone::Main);
        testing::press(vcx, "ctrl-shift-e");
        assert_eq!(zone(&rig, vcx), Zone::Sidebar);
    }

    #[gpui::test]
    fn delete_closes_the_focused_row(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        let b = add_tab(&rig, vcx, "dos", None);
        vcx.run_until_parked();
        // The sidebar's first tab stop is the "+" of the Espacios header (add a project); the
        // next one is the first row ("uno").
        testing::press(vcx, "ctrl-shift-e");
        assert_eq!(zone(&rig, vcx), Zone::Sidebar);
        testing::press(vcx, "tab");
        testing::press(vcx, "delete");
        assert_eq!(tab_count(&rig, vcx), 1);
        let left = vcx.update(|_, cx| rig.shell.read(cx).tab_ids());
        assert_eq!(
            left,
            vec![b],
            "the focused row's tab is the one that closed"
        );
    }

    #[gpui::test]
    fn delete_on_a_running_tab_asks_first(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", Some("Hay un comando en ejecución"));
        add_tab(&rig, vcx, "dos", None);
        vcx.run_until_parked();
        testing::press(vcx, "ctrl-shift-e");
        testing::press(vcx, "tab");
        testing::press(vcx, "delete");
        assert_eq!(tab_count(&rig, vcx), 2, "nothing closed yet");
        assert!(vcx.update(|_, cx| rig.shell.read(cx).prompts.confirm_close.is_some()));
        // Esc cancels, Enter confirms.
        testing::press(vcx, "escape");
        assert_eq!(tab_count(&rig, vcx), 2);
        testing::press(vcx, "ctrl-shift-e");
        testing::press(vcx, "tab");
        testing::press(vcx, "delete");
        testing::press(vcx, "enter");
        assert_eq!(tab_count(&rig, vcx), 1);
    }

    #[gpui::test]
    fn the_chat_hub_is_not_created_while_rendering(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        // Frames of an idle shell never touch chat.db.
        for _ in 0..3 {
            vcx.update(|window, _| window.refresh());
            vcx.run_until_parked();
        }
        let host = vcx.update(|_, cx| rig.shell.read(cx).chat.clone());
        assert!(vcx.update(|_, cx| !host.read(cx).loading() && host.read(cx).hub().is_none()));

        // "Nuevo chat": the hub is not there when the call returns; a task opens the database.
        vcx.update(|window, cx| {
            rig.shell
                .update(cx, |shell, cx| shell.new_chat_tab(None, window, cx));
        });
        assert!(
            vcx.update(|_, cx| host.read(cx).loading() && host.read(cx).hub().is_none()),
            "the database opens on the background executor"
        );
        assert_eq!(
            tab_count(&rig, vcx),
            1,
            "no chat tab until the hub is ready"
        );
        vcx.run_until_parked();
        assert!(vcx.update(|_, cx| host.read(cx).hub().is_some()));
        assert_eq!(tab_count(&rig, vcx), 2, "the queued chat tab opened");
    }

    #[gpui::test]
    fn panels_get_their_zones_real_width_class(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, true);
        add_tab(&rig, vcx, "uno", None);
        vcx.run_until_parked();
        assert_eq!(
            rig.panel.classes.borrow().first().copied(),
            Some(WidthClass::Regular),
            "the panel was built with its zone's class"
        );
        // Dragging the right panel to its narrowest makes its zone compact.
        vcx.update(|_, cx| {
            rig.shell.update(cx, |shell, cx| {
                shell.splits.right_w = Some(RIGHT_MIN_W);
                shell.refresh(cx);
            });
        });
        vcx.run_until_parked();
        assert_eq!(
            rig.panel.classes.borrow().last().copied(),
            Some(WidthClass::Compact)
        );
    }

    #[gpui::test]
    fn the_map_costs_nothing_until_it_is_shown_and_again_after_it_is_hidden(
        cx: &mut TestAppContext,
    ) {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        let model = vcx.update(|_, cx| {
            let m = cx.global::<Services>().agentmap.clone();
            let provider = ui_agentmap::AgentMapPanelProvider::new(m.clone());
            cx.update_global::<PanelRegistry, _>(|r, _| r.register(provider));
            m
        });
        // (the rig builds the shell without `finish_startup`, which is what wires the map)
        vcx.update(|window, cx| rig.shell.update(cx, |s, cx| s.init_agentmap(window, cx)));
        // Hidden: no view, no watcher, no task, no load, and no sampling of external agents.
        vcx.update(|_, cx| {
            let m = model.read(cx);
            assert_eq!((m.viewers(), m.builds()), (0, 0));
            assert!(!m.is_watching());
            assert!(
                !rig.shell.read(cx).agentmap_feed.is_running(),
                "no process sampling while the map is hidden"
            );
        });
        vcx.update(|_, cx| rig.shell.update(cx, |s, cx| s.show_agentmap(cx)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let m = model.read(cx);
            assert_eq!(m.viewers(), 1, "the docked map attached");
            assert!(m.is_watching());
            assert!(
                rig.shell.read(cx).agentmap_feed.is_running(),
                "the map on screen samples the processes (8A)"
            );
        });
        // Hidden again: the view goes away and with it every listener.
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                s.toggle_right_panel(PanelId::new(ui_agentmap::PANEL_ID), cx)
            })
        });
        // The panel animates out for a moment, then it is dropped.
        vcx.executor()
            .advance_clock(TRANSITION_KEEP + Duration::from_millis(50));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let m = model.read(cx);
            assert_eq!(m.viewers(), 0);
            assert!(!m.is_watching());
            assert!(
                !rig.shell.read(cx).agentmap_feed.is_running(),
                "the sampling stopped with the last view"
            );
        });
    }

    /// 8C: the inspector's actions on an external agent reach the shell. Nothing is killed for a
    /// process the map does not show (a stale or reused pid), and "Copiar comando" fills the clipboard.
    #[gpui::test]
    fn external_agent_actions_are_routed_and_never_touch_unknown_processes(
        cx: &mut TestAppContext,
    ) {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        let model = vcx.update(|_, cx| cx.global::<Services>().agentmap.clone());
        vcx.update(|window, cx| rig.shell.update(cx, |s, cx| s.init_agentmap(window, cx)));
        let hint = |vcx: &mut VisualTestContext| {
            vcx.update(|_, cx| {
                rig.shell
                    .read(cx)
                    .prompts
                    .hint
                    .as_ref()
                    .map(|h| h.text.to_string())
            })
        };

        vcx.update(|_, cx| {
            model.update(cx, |_, cx| {
                cx.emit(ui_agentmap::AgentMapEvent::CopyCommand(
                    "agy -p \"hola\" --effort medium".into(),
                ))
            })
        });
        vcx.run_until_parked();
        let copied = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(copied.as_deref(), Some("agy -p \"hola\" --effort medium"));
        assert_eq!(hint(vcx).as_deref(), Some("Comando copiado"));

        // our own pid is not in the map: refused, and (above all) not terminated
        vcx.update(|_, cx| {
            model.update(cx, |_, cx| {
                cx.emit(ui_agentmap::AgentMapEvent::StopExternal(std::process::id()))
            })
        });
        vcx.run_until_parked();
        assert_eq!(
            hint(vcx).as_deref(),
            Some("Ese proceso ya no está en el mapa")
        );
    }

    /// 8A: samples are handed to the map per parent, an empty sample marks the finished ones, parents
    /// that vanished are cleared, and "Detener" refuses a pid whose start time is not the one shown.
    #[gpui::test]
    fn external_samples_reach_the_map_and_stop_checks_the_process_identity(
        cx: &mut TestAppContext,
    ) {
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        let model = vcx.update(|_, cx| cx.global::<Services>().agentmap.clone());
        let me = std::process::id();
        let ext = |pid: u32, started_ms: i64| ui_agentmap::ExternalProc {
            agent: "agy".into(),
            pid,
            cmdline_redacted: "agy -p x".into(),
            cwd: None,
            model: None,
            effort: None,
            started_ms,
            ended_ms: None,
            exit_code: None,
            cpu_pct: None,
            rss_bytes: 1,
        };
        let tab = ui_agentmap::NodeKey::Tab("7".into());
        let other = ui_agentmap::NodeKey::Tab("8".into());
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                s.apply_external_rows(
                    vec![
                        (tab.clone(), vec![ext(me, 1)]),
                        (other.clone(), vec![ext(5, 2)]),
                    ],
                    cx,
                )
            })
        });
        vcx.update(|_, cx| {
            let m = model.read(cx);
            assert_eq!(m.external_procs(&tab).len(), 1);
            assert_eq!(m.external_procs(&other).len(), 1);
        });
        // next sample: tab 8 is gone, tab 7's agy ended
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                s.apply_external_rows(vec![(tab.clone(), vec![])], cx)
            })
        });
        vcx.update(|_, cx| {
            let m = model.read(cx);
            assert!(
                m.external_procs(&other).is_empty(),
                "the vanished parent was cleared"
            );
            let kept = m.external_procs(&tab);
            assert_eq!(kept.len(), 1, "a finished agent stays in the map");
            assert!(kept[0].ended_ms.is_some(), "and is marked as finished");
        });

        // "Detener" on a pid whose start time is not the one the map shows (a reused pid, or here our
        // own process with a made-up start): refused, nothing terminated
        vcx.update(|window, cx| {
            rig.shell.update(cx, |s, cx| {
                s.init_agentmap(window, cx);
                s.agentmap_feed.fed.insert(tab.clone());
            })
        });
        vcx.update(|_, cx| {
            model.update(cx, |_, cx| {
                cx.emit(ui_agentmap::AgentMapEvent::StopExternal(me))
            })
        });
        vcx.run_until_parked();
        let hint = vcx.update(|_, cx| {
            rig.shell
                .read(cx)
                .prompts
                .hint
                .as_ref()
                .map(|h| h.text.to_string())
        });
        let hint = hint.unwrap_or_default();
        assert!(
            hint.contains("No se pudo detener") && hint.contains("PID lo usa otro programa"),
            "{hint}"
        );
    }

    /// 9D: a terminal whose agent launched other agents outside Selachi gets the map icon in the
    /// sidebar, with no harness delegation and no map open: only because the feed saw them once.
    #[gpui::test]
    fn a_terminal_with_only_external_agents_gets_the_map_icon(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        let id = add_tab(&rig, vcx, "uno", None);
        let other = add_tab(&rig, vcx, "dos", None);
        let ext = ui_agentmap::ExternalProc {
            agent: "agy".into(),
            pid: 4242,
            cmdline_redacted: "agy -p x".into(),
            cwd: None,
            model: None,
            effort: None,
            started_ms: 1,
            ended_ms: None,
            exit_code: None,
            cpu_pct: None,
            rss_bytes: 1,
        };
        let key = agentmap::tab_key(id);
        let mapped = |vcx: &mut VisualTestContext| {
            vcx.update(|_, cx| rig.shell.read(cx).compute_lineage(cx).mapped)
        };
        assert!(mapped(vcx).is_empty(), "nothing seen yet: no icon");
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                s.apply_external_rows(vec![(key.clone(), vec![ext])], cx)
            })
        });
        assert_eq!(mapped(vcx), std::collections::HashSet::from([id]));
        assert!(!mapped(vcx).contains(&other), "only that terminal");
        // the agent ended: the map keeps it, so the icon stays
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                s.apply_external_rows(vec![(key.clone(), vec![])], cx)
            })
        });
        assert_eq!(mapped(vcx), std::collections::HashSet::from([id]));
        // the terminal went away from the tracker: forgotten
        vcx.update(|_, cx| {
            rig.shell
                .update(cx, |s, cx| s.apply_external_rows(vec![], cx))
        });
        assert!(mapped(vcx).is_empty());
        // and nothing above started the sampling loop
        vcx.update(|_, cx| assert!(!rig.shell.read(cx).agentmap_feed.is_running()));
    }

    /// 9D: «Permitir siempre en este perfil» is on the card only when the run has a profile, and it
    /// answers the request (the harness then saves the pattern).
    #[gpui::test]
    fn the_always_in_profile_button_needs_a_profile(cx: &mut TestAppContext) {
        use agent_harness::{ApprovalRequest, PROTOCOL_VERSION};
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        let req = |id: &str| ApprovalRequest {
            v: PROTOCOL_VERSION,
            request_id: id.into(),
            // a run id no real registry has: the card must not pick up a profile from the user's data
            run_id: "987654321".into(),
            thread_id: 5,
            parent_thread_id: Some(1),
            agent: "codex".into(),
            tool: "Bash".into(),
            detail: "cargo test -p auth".into(),
            permission: "ask".into(),
            depth: 1,
            timeout_secs: 120,
        };
        let queue = vcx.update(|_, cx| rig.shell.read(cx).deleg.approvals.clone());
        vcx.update(|_, cx| {
            queue.update(cx, |q, cx| {
                q.add_demo(req("a"), cx);
                q.add_demo(req("b"), cx);
            })
        });
        let settle = |vcx: &mut VisualTestContext| {
            for _ in 0..3 {
                vcx.update(|window, _| window.refresh());
                vcx.run_until_parked();
            }
        };
        settle(vcx);
        assert!(
            ui_kit::testing::bounds_of(vcx, "appr-always-run-a").is_some(),
            "the card is painted"
        );
        assert!(
            ui_kit::testing::bounds_of(vcx, "appr-always-profile-a").is_none(),
            "no profile: no button"
        );
        vcx.update(|_, cx| queue.update(cx, |q, cx| q.set_profile("a", Some("Tests".into()), cx)));
        settle(vcx);
        assert!(ui_kit::testing::bounds_of(vcx, "appr-always-profile-a").is_some());
        assert!(
            ui_kit::testing::bounds_of(vcx, "appr-always-profile-b").is_none(),
            "the other run has none"
        );
        ui_kit::testing::click_id(vcx, "appr-always-profile-a");
        let left: Vec<String> = vcx.update(|_, cx| {
            queue
                .read(cx)
                .pending()
                .iter()
                .map(|p| p.request.request_id.clone())
                .collect()
        });
        assert_eq!(left, ["b"], "the click answered request a");
    }

    #[gpui::test]
    fn the_chat_toggles_are_written_to_the_config_and_pushed_back_by_a_reload(
        cx: &mut TestAppContext,
    ) {
        let (rig, vcx) = rig(cx, false);
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                assert!(s.config.chat.redact_copy_commands && !s.config.chat.redact_copy_text);
                s.save_copy_settings(
                    ui_chat::copy::CopySettings {
                        redact_commands: false,
                        redact_text: true,
                    },
                    cx,
                );
                assert!(!s.config.chat.redact_copy_commands && s.config.chat.redact_copy_text);
                // A hot reload that brings `[chat]` back to the defaults is adopted.
                let mut reloaded = (*s.config).clone();
                reloaded.chat = store::ChatConfig::default();
                s.adopt_config(Arc::new(reloaded), cx);
                assert_eq!(s.config.chat, store::ChatConfig::default());
            });
        });
    }

    #[gpui::test]
    fn agents_and_git_settings_reach_their_consumers_on_a_reload(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        vcx.update(|_, cx| {
            let git = cx.global::<Services>().git.clone();
            rig.shell.update(cx, |s, cx| {
                // The defaults at startup.
                assert!(s.usage.read(cx).show_estimates());
                assert_eq!(s.usage.read(cx).week_start(), agents_svc::Weekday::Mon);
                assert!(git.read(cx).confirm_sensitive());

                let mut reloaded = (*s.config).clone();
                reloaded.agents.usage_week_start = store::config::WeekStart::Sunday;
                reloaded.agents.show_estimates = false;
                reloaded.git.confirm_sensitive = false;
                s.adopt_config(Arc::new(reloaded), cx);

                assert_eq!(s.usage.read(cx).week_start(), agents_svc::Weekday::Sun);
                assert!(!s.usage.read(cx).show_estimates());
                assert!(!git.read(cx).confirm_sensitive());
            });
        });
    }

    #[gpui::test]
    fn new_chats_start_with_the_configured_permission_and_follow_a_reload(cx: &mut TestAppContext) {
        use agents_driver::PermissionMode;
        let (rig, vcx) = rig(cx, false);
        add_tab(&rig, vcx, "uno", None);
        // The setting is already ReadOnly when the chat hub opens.
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                let mut c = (*s.config).clone();
                c.agents.permission_default = store::config::PermissionDefault::ReadOnly;
                s.adopt_config(Arc::new(c), cx);
            });
        });
        vcx.update(|window, cx| {
            rig.shell
                .update(cx, |shell, cx| shell.new_chat_tab(None, window, cx));
        });
        vcx.run_until_parked();
        let host = vcx.update(|_, cx| rig.shell.read(cx).chat.clone());
        let hub = vcx.update(|_, cx| host.read(cx).hub()).expect("hub");
        let default =
            |vcx: &mut VisualTestContext| vcx.update(|_, cx| hub.read(cx).default_permission());
        assert_eq!(default(vcx), PermissionMode::ReadOnly);

        // A live change reaches the open hub; «Acceso completo» is never a starting point.
        vcx.update(|_, cx| {
            rig.shell.update(cx, |s, cx| {
                let mut c = (*s.config).clone();
                c.agents.permission_default = store::config::PermissionDefault::Full;
                s.adopt_config(Arc::new(c), cx);
            });
        });
        assert_eq!(default(vcx), PermissionMode::AcceptEdits);
    }

    #[gpui::test]
    fn terminal_parents_get_their_real_names_in_the_map(cx: &mut TestAppContext) {
        let (rig, vcx) = rig(cx, false);
        let id = add_tab(&rig, vcx, "api", None);
        let infos = vcx.update(|_, cx| rig.shell.read(cx).infos(cx));
        let titles = agentmap::titles_of(&infos);
        assert_eq!(
            titles.get(&id.as_u64().to_string()).map(String::as_str),
            Some("api")
        );
        assert_eq!(
            agentmap::tab_key(id),
            ui_agentmap::NodeKey::Tab(id.as_u64().to_string())
        );
    }

    mod keys;
    mod projects;
    mod settings;
    mod settings_layout;
}
