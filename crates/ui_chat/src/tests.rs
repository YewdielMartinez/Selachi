//! Window-level tests: a real `ChatStore` (temp database) with the fake agent, real gpui test
//! windows. Pure logic has its own tests next to it (`rows`, `thread_list`, `mentions`...).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agents_driver::{AgentEvent, AgentKind, AgentStatus, PermissionMode};
use chat_core::{ApprovalDecision, ChatStore, ThreadSpec, ThreadStatus};
use gpui::{Entity, TestAppContext, VisualTestContext};
use theme::Theme;

use crate::chat_view::ChatView;
use crate::composer::{ButtonMode, Composer, ComposerEvent, ComposerState};
use crate::fake::{FactoryHandle, FakeFactory, Step};
use crate::hub::ChatHub;

struct Env {
    dir: tempfile::TempDir,
    factory: Arc<FakeFactory>,
    hub: Entity<ChatHub>,
    store: ChatStore,
}

fn env(cx: &mut TestAppContext, factory: Arc<FakeFactory>) -> Env {
    cx.update(crate::init);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("proj").join("src")).unwrap();
    std::fs::write(
        dir.path().join("proj").join("src").join("main.rs"),
        "fn main() {}",
    )
    .unwrap();
    std::fs::write(dir.path().join("proj").join("README.md"), "# x").unwrap();
    let store = ChatStore::builder()
        .path(dir.path().join("chat.db"))
        .factory(Arc::new(FactoryHandle(factory.clone())))
        .open()
        .unwrap();
    let s2 = store.clone();
    let hub = cx.update(|cx| ChatHub::create(s2, Theme::dark(), cx));
    Env {
        dir,
        factory,
        hub,
        store,
    }
}

impl Env {
    fn project(&self) -> PathBuf {
        self.dir.path().join("proj")
    }
}

fn approval_script() -> Arc<FakeFactory> {
    FakeFactory::new(|_, _| {
        vec![
            Step::now(AgentEvent::Status(AgentStatus::Working)),
            Step::now(AgentEvent::Text {
                text: "Voy a ejecutar algo.".into(),
                delta: false,
            }),
            Step::now(AgentEvent::ApprovalRequest {
                id: "req-1".into(),
                tool: "Bash".into(),
                detail: "rm -rf target".into(),
            }),
            Step::now(AgentEvent::Status(AgentStatus::WaitingForInput)),
        ]
    })
}

/// Runs the executors until `pred` holds (background tasks, the update pump...).
fn settle(
    vcx: &mut VisualTestContext,
    what: &str,
    mut pred: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        vcx.run_until_parked();
        if pred(vcx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for: {what}");
}

fn thread_view<'a>(
    cx: &'a mut TestAppContext,
    e: &Env,
) -> (
    Entity<ChatView>,
    &'a mut VisualTestContext,
    chat_core::ThreadRuntime,
) {
    let mut spec = ThreadSpec::new(e.project(), AgentKind::ClaudeCode);
    spec.permission = PermissionMode::Ask;
    let rec = e.store.create_thread(spec).unwrap();
    let rt = e.store.open(rec.id).unwrap();
    let hub = e.hub.clone();
    let (view, vcx) = cx.add_window_view(move |_, cx| ChatView::loading(hub, Theme::dark(), cx));
    let rt2 = rt.clone();
    view.update(vcx, |v, cx| v.bind_runtime(rt2, cx));
    (view, vcx, rt)
}

// ---------------------------------------------------------------------------------------------
// Composer
// ---------------------------------------------------------------------------------------------

fn composer<'a>(
    cx: &'a mut TestAppContext,
    e: &Env,
) -> (
    Entity<Composer>,
    &'a mut VisualTestContext,
    Rc<RefCell<Vec<ComposerEvent>>>,
) {
    let hub = e.hub.clone();
    let (c, vcx) = cx.add_window_view(move |_, cx| Composer::new(hub, Theme::dark(), cx));
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = events.clone();
    let sub = vcx.update(|_, cx| {
        cx.subscribe(&c, move |_, ev: &ComposerEvent, _| {
            log.borrow_mut().push(ev.clone())
        })
    });
    std::mem::forget(sub);
    c.update_in(vcx, |c, window, cx| c.focus(window, cx));
    vcx.run_until_parked();
    (c, vcx, events)
}

#[gpui::test]
fn enter_sends_and_shift_enter_adds_a_line(cx: &mut TestAppContext) {
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    vcx.simulate_input("hola");
    vcx.simulate_keystrokes("shift-enter");
    vcx.simulate_input("mundo");
    assert_eq!(c.read_with(vcx, |c, cx| c.text(cx)), "hola\nmundo");
    assert!(events.borrow().is_empty(), "shift+enter must not send");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        *events.borrow(),
        vec![ComposerEvent::Send {
            text: "hola\nmundo".into(),
            attachments: vec![]
        }]
    );
    assert_eq!(
        c.read_with(vcx, |c, cx| c.text(cx)),
        "",
        "the field is cleared after sending"
    );
}

#[gpui::test]
fn enter_on_an_empty_field_sends_nothing(cx: &mut TestAppContext) {
    let e = env(cx, FakeFactory::silent());
    let (_c, vcx, events) = composer(cx, &e);
    vcx.simulate_keystrokes("enter");
    vcx.simulate_input("   ");
    vcx.simulate_keystrokes("enter");
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn round_button_is_send_then_stop(cx: &mut TestAppContext) {
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    assert_eq!(
        c.read_with(vcx, |c, cx| c.button_mode(cx)),
        ButtonMode::Disabled
    );
    c.update(vcx, |c, cx| c.set_text("algo", cx));
    assert_eq!(
        c.read_with(vcx, |c, cx| c.button_mode(cx)),
        ButtonMode::Send
    );
    c.update(vcx, |c, cx| c.press_round_button(cx));
    assert_eq!(events.borrow().len(), 1);
    // A turn is running: the same button interrupts, with or without text.
    c.update(vcx, |c, cx| {
        c.set_state(
            ComposerState {
                working: true,
                ..ComposerState::default()
            },
            cx,
        );
        c.set_text("otra cosa", cx);
    });
    assert_eq!(
        c.read_with(vcx, |c, cx| c.button_mode(cx)),
        ButtonMode::Stop
    );
    c.update(vcx, |c, cx| c.press_round_button(cx));
    assert_eq!(events.borrow().last(), Some(&ComposerEvent::Stop));
    assert_eq!(
        events.borrow().len(),
        2,
        "stopping does not send the typed text"
    );
}

#[gpui::test]
fn escape_stops_a_running_turn_but_first_closes_pickers(cx: &mut TestAppContext) {
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    c.update(vcx, |c, cx| {
        c.set_state(
            ComposerState {
                working: true,
                ..ComposerState::default()
            },
            cx,
        );
    });
    vcx.simulate_keystrokes("escape");
    assert_eq!(*events.borrow(), vec![ComposerEvent::Stop]);
}

#[gpui::test]
fn at_sign_opens_the_file_picker_and_enter_inserts_the_path(cx: &mut TestAppContext) {
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    let root = e.project();
    c.update(vcx, |c, cx| {
        c.set_state(
            ComposerState {
                project_root: Some(root.clone()),
                ..ComposerState::default()
            },
            cx,
        );
    });
    vcx.simulate_input("mira @mai");
    settle(vcx, "file list", |vcx| {
        c.read_with(vcx, |c, _| c.is_mention_open())
    });
    // the walk finishes in the background; the picker then lists src/main.rs
    settle(vcx, "mention items", |vcx| {
        vcx.simulate_keystrokes("down");
        c.read_with(vcx, |c, cx| c.text(cx)).contains("@mai")
            && e.hub.read_with(vcx, |h, _| h.files_for(&root).is_some())
    });
    vcx.simulate_keystrokes("enter");
    assert_eq!(c.read_with(vcx, |c, cx| c.text(cx)), "mira @src/main.rs ");
    assert!(
        events.borrow().is_empty(),
        "accepting a mention must not send the message"
    );
    assert!(!c.read_with(vcx, |c, _| c.is_mention_open()));
}

#[gpui::test]
fn the_mention_popover_takes_up_down_and_tab_instead_of_the_text_field(cx: &mut TestAppContext) {
    use ui_kit::testing::{press, type_text};
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    let root = e.project();
    c.update(vcx, |c, cx| {
        c.set_state(
            ComposerState {
                project_root: Some(root.clone()),
                ..ComposerState::default()
            },
            cx,
        );
    });
    type_text(vcx, "@");
    settle(vcx, "two files listed", |vcx| {
        c.read_with(vcx, |c, _| {
            c.mention_state().is_some_and(|(items, _)| items.len() == 2)
        })
    });
    let items = c.read_with(vcx, |c, _| c.mention_state().unwrap().0.to_vec());
    assert_eq!(c.read_with(vcx, |c, _| c.mention_state().unwrap().1), 0);
    press(vcx, "down");
    assert_eq!(c.read_with(vcx, |c, _| c.mention_state().unwrap().1), 1);
    press(vcx, "down");
    assert_eq!(
        c.read_with(vcx, |c, _| c.mention_state().unwrap().1),
        0,
        "the highlight wraps around"
    );
    press(vcx, "up");
    assert_eq!(
        c.read_with(vcx, |c, _| c.mention_state().unwrap().1),
        1,
        "up goes back (and wraps)"
    );
    // Tab accepts the highlighted entry; it does not move focus and nothing is sent.
    press(vcx, "tab");
    assert_eq!(
        c.read_with(vcx, |c, cx| c.text(cx)),
        format!("@{} ", items[1])
    );
    assert!(!c.read_with(vcx, |c, _| c.is_mention_open()));
    assert!(events.borrow().is_empty());
    // With the popover closed, Up / Down are caret motion again: no navigation is captured.
    press(vcx, "up");
    assert!(!c.read_with(vcx, |c, _| c.is_mention_open()));
}

#[gpui::test]
fn escape_closes_the_mention_popover_without_stopping_anything(cx: &mut TestAppContext) {
    use ui_kit::testing::{press, type_text};
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    let root = e.project();
    c.update(vcx, |c, cx| {
        c.set_state(
            ComposerState {
                project_root: Some(root.clone()),
                working: true,
                ..ComposerState::default()
            },
            cx,
        );
    });
    type_text(vcx, "@");
    settle(vcx, "popover open", |vcx| {
        c.read_with(vcx, |c, _| c.is_mention_open())
    });
    press(vcx, "escape");
    assert!(!c.read_with(vcx, |c, _| c.is_mention_open()));
    assert!(
        events.borrow().is_empty(),
        "the first Esc only closes the picker"
    );
    press(vcx, "escape");
    assert_eq!(
        *events.borrow(),
        vec![ComposerEvent::Stop],
        "the second one stops the turn"
    );
}

#[gpui::test]
fn ctrl_enter_also_sends_and_attachments_are_removed_by_path(cx: &mut TestAppContext) {
    use ui_kit::testing::{press, type_text};
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, events) = composer(cx, &e);
    let (a, b) = (
        e.project().join("README.md"),
        e.project().join("src").join("main.rs"),
    );
    c.update(vcx, |c, cx| c.add_attachments([a.clone(), b.clone()], cx));
    vcx.run_until_parked();
    // Clicking the x of the first chip removes that path, not "whatever is at index 0 now".
    ui_kit::testing::click_id(vcx, &format!("attach-x-{}", b.display()));
    assert_eq!(
        c.read_with(vcx, |c, _| c.attachments().to_vec()),
        vec![a.clone()]
    );
    type_text(vcx, "mira");
    press(vcx, "ctrl-enter");
    assert_eq!(
        *events.borrow(),
        vec![ComposerEvent::Send {
            text: "mira".into(),
            attachments: vec![a]
        }]
    );
}

// ---------------------------------------------------------------------------------------------
// View + thread
// ---------------------------------------------------------------------------------------------

#[gpui::test]
fn approval_buttons_answer_through_the_runtime(cx: &mut TestAppContext) {
    let e = env(cx, approval_script());
    let (view, vcx, rt) = thread_view(cx, &e);
    view.update(vcx, |v, cx| v.send("haz algo".into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status
        == ThreadStatus::WaitingForInput
        && !s.pending_approvals().is_empty()));
    view.update(vcx, |v, cx| v.sync(cx));
    // the approval row is in the list
    let pending = view.read_with(vcx, |v, _| {
        v.rows
            .iter()
            .filter(|r| matches!(r.kind, crate::rows::RowKind::Approval { .. }))
            .count()
    });
    assert_eq!(pending, 1);
    view.update(vcx, |v, cx| v.approve("req-1", ApprovalDecision::Allow, cx));
    let end = Instant::now() + Duration::from_secs(10);
    while e.factory.approvals().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        e.factory.approvals(),
        vec![("req-1".to_string(), ApprovalDecision::Allow)]
    );
}

#[gpui::test]
fn deny_and_always_map_to_their_decisions(cx: &mut TestAppContext) {
    let e = env(cx, approval_script());
    let (view, vcx, rt) = thread_view(cx, &e);
    view.update(vcx, |v, cx| v.send("haz algo".into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| {
        !s.pending_approvals().is_empty()
    }));
    view.update(vcx, |v, cx| v.approve("req-1", ApprovalDecision::Deny, cx));
    let end = Instant::now() + Duration::from_secs(10);
    while e.factory.approvals().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(e.factory.approvals()[0].1, ApprovalDecision::Deny);
    // an id the thread does not know is reported, not sent
    view.update(vcx, |v, cx| v.approve("nope", ApprovalDecision::Always, cx));
    assert!(view.read_with(vcx, |v, _| v.notice().is_some()));
    assert_eq!(e.factory.approvals().len(), 1);
}

#[gpui::test]
fn a_hidden_thread_reads_nothing_until_it_is_painted(cx: &mut TestAppContext) {
    let e = env(
        cx,
        FakeFactory::new(|_, _| {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::Text {
                    text: "hola".into(),
                    delta: false,
                }),
                Step::now(AgentEvent::Status(AgentStatus::Done)),
            ]
        }),
    );
    let (view, vcx, rt) = thread_view(cx, &e);
    rt.send("hola", vec![]).unwrap();
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status == ThreadStatus::Done));
    // Nothing painted the view: it did not build rows (zero work while hidden).
    assert_eq!(view.read_with(vcx, |v, _| v.row_count()), 0);
    view.update(vcx, |v, cx| v.sync(cx));
    assert!(
        view.read_with(vcx, |v, _| v.row_count()) >= 2,
        "user + assistant text"
    );
}

#[gpui::test]
fn stop_interrupts_the_thread(cx: &mut TestAppContext) {
    let e = env(
        cx,
        FakeFactory::new(|_, _| vec![Step::now(AgentEvent::Status(AgentStatus::Working))]),
    );
    let (view, vcx, rt) = thread_view(cx, &e);
    view.update(vcx, |v, cx| v.send("largo".into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status
        == ThreadStatus::Working));
    view.update(vcx, |v, cx| v.stop(cx));
    let end = Instant::now() + Duration::from_secs(10);
    while e.factory.interrupts() == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(e.factory.interrupts(), 1);
}

#[gpui::test]
fn a_draft_needs_a_project_then_creates_the_thread_on_send(cx: &mut TestAppContext) {
    let e = env(
        cx,
        FakeFactory::new(|_, _| vec![Step::now(AgentEvent::Status(AgentStatus::Done))]),
    );
    let hub = e.hub.clone();
    let (view, vcx) =
        cx.add_window_view(move |_, cx| ChatView::draft(hub, Theme::dark(), None, cx));
    // no project: nothing is created and the text goes back to the field
    view.update(vcx, |v, cx| v.send("hola".into(), vec![], cx));
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |v, _| v.is_draft() && v.thread_id().is_none()));
    assert_eq!(e.store.list_threads(&Default::default()).unwrap().len(), 0);
    let composer = view.read_with(vcx, |v, _| v.composer().clone());
    assert_eq!(composer.read_with(vcx, |c, cx| c.text(cx)), "hola");
    // with a project it becomes a thread and the message reaches the agent
    let root = e.project();
    view.update(vcx, |v, cx| {
        let _ = cx;
        if let crate::chat_view::ViewMode::Draft { spec, .. } = &mut v.mode_mut() {
            spec.project = Some(root.clone());
        }
        v.send("hola".into(), vec![], cx);
    });
    settle(vcx, "thread created", |vcx| {
        view.read_with(vcx, |v, _| v.thread_id().is_some())
    });
    let end = Instant::now() + Duration::from_secs(10);
    while e.factory.sent().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(e.factory.sent(), vec!["hola".to_string()]);
    assert_eq!(e.store.list_threads(&Default::default()).unwrap().len(), 1);
}

#[gpui::test]
fn a_new_draft_starts_with_the_configured_permission_and_existing_threads_keep_theirs(
    cx: &mut TestAppContext,
) {
    let e = env(
        cx,
        FakeFactory::new(|_, _| vec![Step::now(AgentEvent::Status(AgentStatus::Done))]),
    );
    // a thread created before the setting changed
    let old = e
        .store
        .create_thread(ThreadSpec::new(e.project(), AgentKind::ClaudeCode))
        .unwrap();
    assert_eq!(old.permission, PermissionMode::Ask);

    e.hub.update(cx, |h, _| {
        h.set_default_permission(PermissionMode::ReadOnly)
    });
    // A tab made by the hub (the real path: created while the hub is being updated)...
    let handle = e
        .hub
        .update(cx, |h, cx| h.new_thread_tab(Some(e.project()), cx));
    let tab = handle.downcast::<crate::tab::ChatTab>().unwrap();
    let tab_view = tab.read_with(cx, |t, _| t.view().clone());
    let tab_spec = tab_view.read_with(cx, |v, _| v.draft_spec().cloned().unwrap());
    assert_eq!(tab_spec.permission, PermissionMode::ReadOnly);
    // ...and a view made directly.
    let hub = e.hub.clone();
    let (view, vcx) =
        cx.add_window_view(move |_, cx| ChatView::draft(hub, Theme::dark(), None, cx));
    let spec = view.read_with(vcx, |v, _| v.draft_spec().cloned().unwrap());
    assert_eq!(spec.permission, PermissionMode::ReadOnly);
    // the composer shows it and the thread is created with it
    let state = view.read_with(vcx, |v, cx| v.composer_state(cx));
    assert_eq!(state.permission, PermissionMode::ReadOnly);
    let mut spec = spec;
    spec.project = Some(e.project());
    assert_eq!(
        spec.to_thread_spec().unwrap().permission,
        PermissionMode::ReadOnly
    );
    // the old thread did not change
    let again = e.store.get_thread(old.id).unwrap().unwrap();
    assert_eq!(again.permission, PermissionMode::Ask);
}

#[gpui::test]
fn the_hub_list_follows_thread_status(cx: &mut TestAppContext) {
    let e = env(cx, approval_script());
    let (view, vcx, rt) = thread_view(cx, &e);
    let id = rt.id();
    settle(vcx, "initial list", |vcx| {
        e.hub.read_with(vcx, |h, _| h.thread(id).is_some())
    });
    view.update(vcx, |v, cx| v.send("x".into(), vec![], cx));
    settle(vcx, "waiting in the list", |vcx| {
        e.hub.read_with(vcx, |h, _| {
            h.thread(id).map(|t| t.status) == Some(ThreadStatus::WaitingForInput)
        })
    });
}

// ---------------------------------------------------------------------------------------------
// Delegation lineage (PLAN section 20)
// ---------------------------------------------------------------------------------------------

#[gpui::test]
fn a_session_that_launched_agents_outside_selachi_says_so_and_opens_the_map(
    cx: &mut TestAppContext,
) {
    use crate::lineage::{ExternalNote, external_note_line};

    // the wording
    let one = ExternalNote {
        agents: vec!["agy".into()],
        count: 1,
    };
    assert_eq!(
        external_note_line(&one),
        "Lanzó agy fuera de Selachi · 1 proceso"
    );
    let many = ExternalNote {
        agents: vec!["agy".into(), "codex".into(), "aider".into()],
        count: 3,
    };
    assert_eq!(
        external_note_line(&many),
        "Lanzó agy, codex y otros fuera de Selachi · 3 procesos"
    );

    let e = env(cx, FakeFactory::new(|_, _| Vec::new()));
    let (_view, vcx, rt) = thread_view(cx, &e);
    let parent = rt.record().id;
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    assert!(
        ui_kit::testing::bounds_of(vcx, "deleg-external").is_none(),
        "nothing detected: no row"
    );
    e.hub.update(vcx, |h, cx| {
        h.set_external_notes(
            std::collections::HashMap::from([(
                parent,
                ExternalNote {
                    agents: vec!["agy".into()],
                    count: 3,
                },
            )]),
            cx,
        )
    });
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    assert!(
        ui_kit::testing::bounds_of(vcx, "deleg-external").is_some(),
        "the muted row is painted"
    );
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = seen.clone();
    let _sub = e.hub.update(vcx, |_, cx| {
        cx.subscribe(&e.hub, move |_, _, ev: &crate::hub::ChatEvent, _| {
            log.borrow_mut().push(ev.clone())
        })
    });
    ui_kit::testing::click_id(vcx, "deleg-external-map");
    assert_eq!(
        seen.borrow().as_slice(),
        [crate::hub::ChatEvent::OpenAgentMap(parent)]
    );
}

#[gpui::test]
fn a_parent_chat_lists_its_delegations_and_sums_their_cost(cx: &mut TestAppContext) {
    use crate::lineage::{DelegationRun, RunState};
    use chat_core::{ThreadId, ThreadLink};

    let e = env(cx, FakeFactory::new(|_, _| Vec::new()));
    let (view, vcx, rt) = thread_view(cx, &e);
    let parent = rt.record().id;
    let mut spec = ThreadSpec::new(e.project(), AgentKind::Codex);
    spec.title = Some("Revisa los tests de auth".into());
    spec.link = Some(ThreadLink {
        parent_thread_id: Some(parent),
        parent_tab: None,
        depth: 1,
        delegated_by: Some("mcp".into()),
    });
    let child = e.store.create_thread(spec).unwrap();
    let now = chat_core::now_ms();
    e.hub.update(vcx, |h, cx| {
        h.refresh(cx);
        h.set_delegation_runs(
            vec![DelegationRun {
                thread: child.id,
                state: RunState::Done,
                files_changed: 3,
                started_ms: now - 120_000,
                finished_ms: Some(now),
                cost_usd: 0.42,
                tokens: 12_000,
                summary: "Añadí tres pruebas.".into(),
            }],
            cx,
        );
    });
    settle(vcx, "the hub lists the child", |vcx| {
        e.hub
            .read_with(vcx, |h, _| h.children_of(parent).len() == 1)
    });
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    // the row is on screen and its click expands it
    let id = format!("deleg-{}", child.id.0);
    assert!(
        ui_kit::testing::bounds_of(vcx, &id).is_some(),
        "the delegation row is painted"
    );
    assert!(!view.read_with(vcx, |v, _| v.delegation_open(child.id)));
    ui_kit::testing::click_id(vcx, &id);
    assert!(view.read_with(vcx, |v, _| v.delegation_open(child.id)));
    // "Ver en el mapa" asks the host to show the subagent map on that child
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = seen.clone();
    let _sub = e.hub.update(vcx, |_, cx| {
        cx.subscribe(&e.hub, move |_, _, ev: &crate::hub::ChatEvent, _| {
            log.borrow_mut().push(ev.clone())
        })
    });
    ui_kit::testing::click_id(vcx, &format!("deleg-map-{}", child.id.0));
    assert_eq!(
        seen.borrow().as_slice(),
        [crate::hub::ChatEvent::OpenAgentMap(child.id)]
    );
    // the header cost includes what was delegated, and says so
    let (text, tip) = view
        .read_with(vcx, |v, cx| v.cost_label(cx))
        .expect("a cost label");
    assert_eq!(text, "$0.42 · incluye delegaciones");
    assert!(tip.contains("Delegaciones $0.42"), "{tip}");
    // a thread that delegated nothing has no such label
    assert_eq!(
        e.hub
            .read_with(vcx, |h, _| h.delegation_cost(ThreadId(child.id.0))),
        0.0
    );
}

mod copy_tests;
mod layout_tests;
mod mem_tests;
