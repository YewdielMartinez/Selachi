//! View-level tests: the wizard running inside a gpui test window with fake services.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use agents_driver::{AgentKind, DetectedAgent};
use gpui::{RenderImage, TestAppContext, VisualTestContext};
use store::{Config, StateDb};
use theme::{Mode, Theme};

use crate::agents::AgentStatus;
use crate::flow::Step;
use crate::shells::{ShellInfo, ShellKind};
use crate::view::{OnboardingEvent, OnboardingView, Services};

fn fake_services(home: PathBuf, gif: Option<PathBuf>) -> Services {
    Services {
        detect_agents: Arc::new(|| {
            vec![
                DetectedAgent {
                    kind: AgentKind::ClaudeCode,
                    path: PathBuf::from("C:/x/claude.exe"),
                    version: Some("2.1.330 (Claude Code)".into()),
                    logged_in: Some(true),
                },
                DetectedAgent { kind: AgentKind::OpenCode, path: PathBuf::from("C:/x/opencode.exe"), version: None, logged_in: None },
            ]
        }),
        detect_shells: Arc::new(|| {
            vec![
                ShellInfo { kind: ShellKind::Pwsh, name: "PowerShell 7".into(), program: "pwsh.exe".into(), args: vec![], detail: "pwsh".into() },
                ShellInfo { kind: ShellKind::Cmd, name: "Símbolo del sistema".into(), program: "cmd.exe".into(), args: vec![], detail: "cmd".into() },
            ]
        }),
        home: Some(home),
        gif_path: gif,
    }
}

struct Harness {
    view: gpui::Entity<OnboardingView>,
    events: Rc<RefCell<Vec<OnboardingEvent>>>,
    dir: tempfile::TempDir,
    db: StateDb,
}

fn setup(cx: &mut TestAppContext, start: Step, gif: bool) -> (Harness, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("Desktop").join("alpha")).unwrap();
    std::fs::write(home.join("Desktop").join("alpha").join("Cargo.toml"), "").unwrap();
    std::fs::create_dir_all(home.join("Desktop").join("beta")).unwrap();
    std::fs::write(home.join("Desktop").join("beta").join("package.json"), "").unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = StateDb::open(&data.join("state.db")).unwrap();
    let services = fake_services(home, gif.then(|| crate::gif::resolve_path().unwrap()));
    let db2 = db.clone();
    let (view, vcx) = cx.add_window_view(move |_, cx| OnboardingView::new(Config::default(), Some(db2), Theme::dark(), services, start, cx));
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &OnboardingEvent, _| sink.borrow_mut().push(ev.clone())).detach();
    });
    vcx.update(|window, cx| view.update(cx, |v, cx| v.init_window(window, cx)));
    vcx.run_until_parked();
    (Harness { view, events, dir, db }, vcx)
}

fn press(h: &Harness, cx: &mut VisualTestContext, f: impl FnOnce(&mut OnboardingView, &mut gpui::Window, &mut gpui::Context<OnboardingView>)) {
    h.view.update_in(cx, f);
    cx.run_until_parked();
}

#[gpui::test]
async fn shark_frames_are_loaded_on_step_one_and_freed_after_it(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Welcome, true);
    assert!(h.view.read_with(cx, |v, _| v.shark_loaded()), "decoded on the background executor");
    let weak: Vec<Weak<RenderImage>> = h.view.read_with(cx, |v, _| {
        v.shark.as_ref().unwrap().frames.iter().map(|f| Arc::downgrade(&f.image)).collect()
    });
    assert_eq!(weak.len(), 30);
    assert!(weak.iter().all(|w| w.upgrade().is_some()));

    press(&h, cx, |v, w, cx| v.primary(w, cx)); // Empezar
    assert_eq!(h.view.read_with(cx, |v, _| v.step()), Step::Agents);
    assert!(!h.view.read_with(cx, |v, _| v.shark_loaded()), "frames dropped when leaving step 1");
    assert!(weak.iter().all(|w| w.upgrade().is_none()), "no frame is kept alive anywhere");

    press(&h, cx, |v, w, cx| v.back(w, cx)); // back to the welcome step: loaded again
    assert!(h.view.read_with(cx, |v, _| v.shark_loaded()));
    press(&h, cx, |v, w, cx| v.skip(w, cx));
}

#[gpui::test]
async fn the_shark_is_dropped_if_the_user_leaves_before_it_loads(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = fake_services(dir.path().to_path_buf(), Some(crate::gif::resolve_path().unwrap()));
    let (view, cx) = cx.add_window_view(move |_, cx| OnboardingView::new(Config::default(), None, Theme::dark(), services, Step::Welcome, cx));
    view.update_in(cx, |v, w, cx| {
        v.init_window(w, cx); // starts loading
        v.primary(w, cx); // leaves at once
    });
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.shark_loaded()));
}

#[gpui::test]
async fn a_missing_gif_degrades_to_an_empty_hero(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut services = fake_services(dir.path().to_path_buf(), Some(dir.path().join("nope.gif")));
    services.gif_path = Some(dir.path().join("nope.gif"));
    let (view, cx) = cx.add_window_view(move |_, cx| OnboardingView::new(Config::default(), None, Theme::dark(), services, Step::Welcome, cx));
    view.update_in(cx, |v, w, cx| v.init_window(w, cx));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.shark_loaded()));
    assert!(view.read_with(cx, |v, _| v.shark_failed));
}

#[gpui::test]
async fn reduced_motion_keeps_one_static_frame(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = fake_services(dir.path().to_path_buf(), Some(crate::gif::resolve_path().unwrap()));
    let mut config = Config::default();
    config.layout.overrides.reduced_motion = Some(true);
    let (view, cx) = cx.add_window_view(move |_, cx| OnboardingView::new(config, None, Theme::dark(), services, Step::Welcome, cx));
    view.update_in(cx, |v, w, cx| v.init_window(w, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.shark.as_ref().map(|s| s.frames.len())), Some(1));
}

#[gpui::test]
async fn detection_rows_follow_the_detector(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Agents, false);
    let rows = h.view.read_with(cx, |v, _| v.agent_rows().to_vec());
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].status, AgentStatus::Ready);
    assert_eq!(rows[1].status, AgentStatus::NotInstalled);
    assert_eq!(rows[2].status, AgentStatus::Unverified);
    assert_eq!(rows[3].status, AgentStatus::NotInstalled);
}

#[gpui::test]
async fn login_button_emits_the_command(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Agents, false);
    press(&h, cx, |v, _, cx| v.open_login(crate::agents::AgentCli::OpenCode, cx));
    let events = h.events.borrow();
    assert!(matches!(
        events.as_slice(),
        [OnboardingEvent::OpenLoginTerminal { agent: crate::agents::AgentCli::OpenCode, command }] if command == "opencode auth login"
    ));
}

#[gpui::test]
async fn projects_stream_in_with_choices(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Projects, false);
    let names: Vec<String> = h.view.read_with(cx, |v, _| v.project_list().items().iter().map(|i| i.project.name.clone()).collect());
    assert_eq!(names, ["alpha", "beta"]);
    assert!(!h.view.read_with(cx, |v, _| v.scan_running), "scan finished");
    press(&h, cx, |v, _, cx| {
        v.projects.toggle_checked(0);
        v.projects.toggle_favorite(1);
        cx.notify();
    });
    let outcome = h.view.read_with(cx, |v, _| v.outcome());
    assert_eq!(outcome.projects.len(), 1);
    assert_eq!(outcome.projects[0].name, "beta");
    assert_eq!(outcome.favorites.len(), 1);
}

#[gpui::test]
async fn switching_a_root_off_rescans_and_keeps_choices(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Projects, false);
    press(&h, cx, |v, _, cx| {
        v.projects.toggle_checked(0);
        cx.notify();
    });
    press(&h, cx, |v, w, cx| v.toggle_root(0, w, cx));
    assert!(h.view.read_with(cx, |v, _| v.project_list().is_empty()), "no roots checked: nothing to show");
    press(&h, cx, |v, w, cx| v.toggle_root(0, w, cx));
    let items = h.view.read_with(cx, |v, _| v.project_list().items().to_vec());
    assert_eq!(items.len(), 2);
    assert!(!items[0].checked, "alpha stays unchecked after the rescan");
    assert!(items[1].checked);
}

#[gpui::test]
async fn theme_choice_previews_live(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Appearance, false);
    press(&h, cx, |v, _, cx| v.set_theme_choice(crate::model::ThemeChoice::Light, cx));
    assert_eq!(h.view.read_with(cx, |v, _| v.theme().mode), Mode::Light);
    press(&h, cx, |v, _, cx| v.set_theme_choice(crate::model::ThemeChoice::Light, cx)); // same: no event
    press(&h, cx, |v, _, cx| v.set_theme_choice(crate::model::ThemeChoice::Oled, cx));
    let modes: Vec<Mode> = h
        .events
        .borrow()
        .iter()
        .filter_map(|e| if let OnboardingEvent::ThemePreview(m) = e { Some(*m) } else { None })
        .collect();
    assert_eq!(modes, [Mode::Light, Mode::Oled]);
}

#[gpui::test]
async fn skipping_the_welcome_step_marks_done_and_changes_nothing(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Welcome, false);
    let data = h.dir.path().join("data");
    assert!(crate::flag::needs_onboarding(&h.db));
    press(&h, cx, |v, w, cx| v.skip(w, cx));
    assert!(matches!(h.events.borrow().as_slice(), [OnboardingEvent::Skipped]));
    assert!(!crate::flag::needs_onboarding(&h.db));
    assert!(!data.join("config.toml").exists(), "skipping writes no config");
}

#[gpui::test]
async fn finishing_writes_config_projects_favorites_and_the_flag(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx, Step::Welcome, false);
    let data = h.dir.path().join("data");
    std::fs::write(data.join("config.toml"), format!("# keep this comment\n{}", store::config::TEMPLATE)).unwrap();
    press(&h, cx, |v, w, cx| v.primary(w, cx)); // -> Agents
    press(&h, cx, |v, _, cx| {
        v.draft_mut().claude_statusline_hook = true;
        cx.notify();
    });
    press(&h, cx, |v, w, cx| v.primary(w, cx)); // -> Projects (scan runs)
    press(&h, cx, |v, _, cx| {
        v.projects.toggle_favorite(0);
        cx.notify();
    });
    press(&h, cx, |v, w, cx| v.primary(w, cx)); // -> Appearance
    press(&h, cx, |v, _, cx| {
        v.set_theme_choice(crate::model::ThemeChoice::Light, cx);
        v.draft_mut().density = store::Density::Compact;
    });
    press(&h, cx, |v, w, cx| v.skip(w, cx)); // skip Appearance -> Terminal: its choices must not apply
    press(&h, cx, |v, _, cx| {
        v.draft_mut().restore_session = false;
        cx.notify();
    });
    press(&h, cx, |v, w, cx| v.primary(w, cx)); // -> Done
    assert_eq!(h.view.read_with(cx, |v, _| v.step()), Step::Done);
    press(&h, cx, |v, w, cx| v.primary(w, cx)); // Abrir Selachi

    let events = h.events.borrow();
    let Some(OnboardingEvent::Finished { config, projects, favorites }) = events.iter().find(|e| matches!(e, OnboardingEvent::Finished { .. })) else {
        panic!("no Finished event in {events:?}");
    };
    assert!(config.agents.claude_statusline_hook);
    assert_eq!(config.appearance.mode, store::config::ThemeMode::Dark, "skipped step keeps the base");
    assert!(!config.session.restore);
    assert_eq!(projects.len(), 2);
    assert_eq!(favorites.len(), 1);

    let saved = std::fs::read_to_string(data.join("config.toml")).unwrap();
    assert!(saved.starts_with("# keep this comment"));
    assert!(saved.contains("claude_statusline_hook = true"));
    assert!(saved.contains("restore = false"));
    assert!(saved.contains("mode = \"dark\""));
    assert!(!crate::flag::needs_onboarding(&h.db));
    assert_eq!(h.db.projects().unwrap().len(), 2);
    assert_eq!(h.db.favorites().unwrap().len(), 1);
    drop(events);
    assert!(!h.view.read_with(cx, |v, _| v.shark_loaded()));
    assert!(!h.view.read_with(cx, |v, _| v.scan_running), "everything stopped after finishing");
}
