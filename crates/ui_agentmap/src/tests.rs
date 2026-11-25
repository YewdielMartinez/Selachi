//! Model and view tests: building the map from links + registry, partial data, no work while
//! hidden, push-driven reloads, and the inspector's events. No real agent, no real chat db.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agents_driver::{AgentKind, PermissionMode};
use chat_core::{Changed, ThreadId, ThreadLink, ThreadRecord, ThreadStatus, ThreadUpdate};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use panel_api::{PanelCtx, PanelRegistry, PanelZone, WidthClass};
use theme::Theme;
use ui_kit::testing;

use crate::activity::{
    ActivityEvent, ActivityFilter, ActivityKind, CommandStatus, ExternalChild, LiveHint,
};
use crate::events::AgentMapEvent;
use crate::inspector::{EXTERNAL_EXPLANATION, available, external_facts, limit_rows};
use crate::model::{AgentMapModel, CHAT_DEBOUNCE, InspectorTab, PromptSource};
use crate::registry::ApprovalEntry;
use crate::source::SessionSource;
use crate::tree::{ExternalProc, MAX_EXTERNAL_PER_SESSION, NodeKey, NodeKind, NodeStatus};
use crate::view::{AgentMapPanelProvider, AgentMapView, MapMode, PANEL_ID};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn record(id: i64, title: &str, agent: AgentKind, link: ThreadLink) -> ThreadRecord {
    ThreadRecord {
        id: ThreadId(id),
        title: title.into(),
        project_root: "C:/p".into(),
        worktree_path: None,
        agent,
        model: Some("gpt-5".into()),
        effort: Some("high".into()),
        permission: PermissionMode::ReadOnly,
        status: ThreadStatus::Done,
        created_ms: 1_000 * id,
        updated_ms: 1_000 * id + 500,
        agent_session_id: None,
        archived: false,
        checkpoint_before_turn: false,
        link,
    }
}

fn child_of(parent: i64) -> ThreadLink {
    ThreadLink {
        parent_thread_id: Some(ThreadId(parent)),
        parent_tab: None,
        depth: 1,
        delegated_by: Some("mcp".into()),
    }
}

#[derive(Default)]
struct FakeSource {
    threads: Mutex<Vec<ThreadRecord>>,
    first: Mutex<HashMap<i64, String>>,
    approvals: Mutex<HashMap<i64, Vec<ApprovalEntry>>>,
    reads: AtomicUsize,
    live: Mutex<Option<UnboundedReceiver<ThreadUpdate>>>,
    /// What the chat says each thread did (source A of the activity feed).
    acts: Mutex<HashMap<i64, Vec<ActivityEvent>>>,
    act_reads: AtomicUsize,
    hint_reads: AtomicUsize,
}

impl SessionSource for FakeSource {
    fn threads(&self) -> Vec<ThreadRecord> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.threads.lock().unwrap().clone()
    }

    fn first_user_message(&self, id: ThreadId) -> Option<String> {
        self.first.lock().unwrap().get(&id.0).cloned()
    }

    fn approvals(&self, id: ThreadId) -> Vec<ApprovalEntry> {
        self.approvals
            .lock()
            .unwrap()
            .get(&id.0)
            .cloned()
            .unwrap_or_default()
    }

    fn activity(&self, id: ThreadId, _active: bool) -> Vec<ActivityEvent> {
        self.act_reads.fetch_add(1, Ordering::SeqCst);
        self.acts
            .lock()
            .unwrap()
            .get(&id.0)
            .cloned()
            .unwrap_or_default()
    }

    fn live_hint(&self, _id: ThreadId) -> LiveHint {
        self.hint_reads.fetch_add(1, Ordering::SeqCst);
        LiveHint::None
    }

    fn updates(&self) -> Option<UnboundedReceiver<ThreadUpdate>> {
        self.live.lock().unwrap().take()
    }
}

fn write_run(dir: &Path, id: i64, json: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{id}.json")), json).unwrap();
}

struct Rig {
    source: Arc<FakeSource>,
    dir: tempfile::TempDir,
    updates: UnboundedSender<ThreadUpdate>,
}

impl Rig {
    fn runs(&self) -> PathBuf {
        self.dir.path().join("agent-runs")
    }
}

/// An orchestrator (1) with two delegations: 2 done with a worktree and changes, 3 running.
fn rig() -> Rig {
    let (tx, rx) = unbounded();
    let source = Arc::new(FakeSource::default());
    *source.threads.lock().unwrap() = vec![
        record(
            1,
            "Orquestador",
            AgentKind::ClaudeCode,
            ThreadLink::default(),
        ),
        record(2, "Revisa tests", AgentKind::Codex, child_of(1)),
        record(3, "Docs", AgentKind::OpenCode, child_of(1)),
    ];
    *source.live.lock().unwrap() = Some(rx);
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path().join("agent-runs");
    write_run(
        &runs,
        2,
        r#"{"run_id":"2","thread_id":2,"agent":"codex","model":"gpt-5","permission":"read-only",
        "worktree_path":"C:/p.wt/tests","started_ms":2000,"finished_ms":9000,"status":"done",
        "parent_thread_id":1,"depth":1,"launcher":"mcp","profile":"Tests",
        "final_prompt":"Revisa los tests. token=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
        "effective_limits":{"permission":"read-only","timeout_secs":900,"budget_tokens":200000},
        "result":{"summary":"Todo bien","files_changed":["a.rs","b.rs"],
          "diff_stat":{"files":2,"insertions":8,"deletions":2,"text":" a.rs | 6 +++---\n b.rs | 4 ++--"},
          "usage":{"billable":4500},"cost":{"usd":0.12,"exact":false}}}"#,
    );
    write_run(
        &runs,
        3,
        r#"{"run_id":"3","thread_id":3,"agent":"opencode","started_ms":3000,"status":"running","parent_thread_id":1}"#,
    );
    Rig {
        source,
        dir,
        updates: tx,
    }
}

fn model(cx: &mut TestAppContext, rig: &Rig) -> Entity<AgentMapModel> {
    let source: Arc<dyn SessionSource> = rig.source.clone();
    let runs = rig.runs();
    cx.new(|_| AgentMapModel::new(source, runs))
}

fn settle(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.run_until_parked();
}

/// Waits (real time) for a condition that depends on the OS file watcher.
fn wait_until(cx: &mut TestAppContext, mut cond: impl FnMut(&mut TestAppContext) -> bool) -> bool {
    cx.executor().allow_parking();
    for _ in 0..200 {
        cx.run_until_parked();
        if cond(cx) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn builds(cx: &mut TestAppContext, m: &Entity<AgentMapModel>) -> usize {
    cx.read_entity(m, |m, _| m.builds())
}

fn events(cx: &mut TestAppContext, m: &Entity<AgentMapModel>) -> Rc<RefCell<Vec<AgentMapEvent>>> {
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    cx.update(|app| {
        app.subscribe(m, move |_, ev: &AgentMapEvent, _| {
            sink.borrow_mut().push(ev.clone())
        })
        .detach();
    });
    log
}

fn ctx(width: WidthClass) -> PanelCtx {
    PanelCtx::new(PanelZone::Main, width, Theme::dark())
}

fn open_view<'a>(
    cx: &'a mut TestAppContext,
    m: &Entity<AgentMapModel>,
) -> (Entity<AgentMapView>, &'a mut VisualTestContext) {
    let m = m.clone();
    testing::open_window(cx, move |_, cx| {
        AgentMapView::new(ctx(WidthClass::Regular), m, cx)
    })
}

// ---------------------------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------------------------

#[gpui::test]
fn the_model_builds_the_tree_from_links_and_the_registry(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert!(m.is_loaded());
        let f = m.forest();
        assert_eq!(f.roots.len(), 1);
        let root = m.root_index().unwrap();
        assert_eq!(f.nodes[root].title, "Orquestador");
        assert_eq!(f.nodes[root].children.len(), 2);
        let two = &f.nodes[f.index_of(&NodeKey::Thread(ThreadId(2))).unwrap()];
        assert_eq!(two.status, NodeStatus::Done);
        assert_eq!(two.files, 2);
        assert_eq!(two.tokens, Some(4500));
        assert_eq!(two.profile.as_deref(), Some("Tests"));
        let three = &f.nodes[f.index_of(&NodeKey::Thread(ThreadId(3))).unwrap()];
        assert_eq!(three.status, NodeStatus::Running);
        assert_eq!(f.totals(root).running, 1);
    });
}

#[gpui::test]
fn partial_and_corrupt_registry_files_do_not_break_the_map(cx: &mut TestAppContext) {
    let rig = rig();
    // a half-written record, junk, a cancel marker and a record whose thread does not exist
    write_run(&rig.runs(), 4, r#"{"run_id":"4"}"#);
    write_run(&rig.runs(), 5, "{not json");
    std::fs::write(rig.runs().join("3.cancel"), "cancel").unwrap();
    write_run(
        &rig.runs(),
        77,
        r#"{"run_id":"77","parent_thread_id":1,"agent":"gemini","prompt":"Investiga el bug","status":"error","started_ms":"7000"}"#,
    );
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    cx.read_entity(&m, |m, _| {
        let f = m.forest();
        let root = m.root_index().unwrap();
        // 2, 3, 4 and 77 hang from the orchestrator (4 has no parent: its own orphan root)
        let seventy = &f.nodes[f.index_of(&NodeKey::Thread(ThreadId(77))).unwrap()];
        assert_eq!(seventy.title, "Investiga el bug");
        assert_eq!(seventy.status, NodeStatus::Failed);
        assert_eq!(seventy.agent, "gemini");
        assert!(f.index_of(&NodeKey::Thread(ThreadId(4))).is_some());
        assert!(f.nodes[root].children.len() >= 3);
    });
}

#[gpui::test]
fn nothing_runs_while_the_map_is_hidden(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let sample_ext = ExternalProc {
        agent: "agy".into(),
        pid: 4242,
        cmdline_redacted: "agy -p check --effort medium".into(),
        cwd: Some(PathBuf::from("C:/p")),
        model: Some("gemini-3-pro".into()),
        effort: Some("medium".into()),
        started_ms: 4_000,
        ended_ms: None,
        exit_code: None,
        cpu_pct: Some(5.0),
        rss_bytes: 120 * 1024 * 1024,
    };
    // never attached: refresh, set_root, open_run and set_external do zero background work
    m.update(cx, |m, cx| {
        m.refresh(cx);
        m.set_root(Some(NodeKey::Thread(ThreadId(1))), cx);
        m.open_run(ThreadId(2), cx);
        m.set_external_at(
            NodeKey::Thread(ThreadId(1)),
            vec![sample_ext.clone()],
            4_500,
            cx,
        );
    });
    settle(cx);
    assert_eq!(rig.source.reads.load(Ordering::SeqCst), 0);
    assert_eq!(builds(cx, &m), 0);
    assert!(!cx.read_entity(&m, |m, _| m.is_watching()));
    assert!(!cx.read_entity(&m, |m, _| m.is_visible()));

    // shown: one load and the listeners (and the external proc appears in the built forest)
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    assert_eq!(rig.source.reads.load(Ordering::SeqCst), 1);
    assert!(cx.read_entity(&m, |m, _| m.is_watching()));
    assert!(cx.read_entity(&m, |m, _| m.is_visible()));
    cx.read_entity(&m, |m, _| {
        assert!(m.forest().index_of(&sample_ext.key()).is_some());
    });

    // hidden again: no watcher, and changes on disk, chat or external snapshots cost nothing
    m.update(cx, |m, cx| m.detach(cx));
    settle(cx);
    assert!(!cx.read_entity(&m, |m, _| m.is_watching()));
    assert!(!cx.read_entity(&m, |m, _| m.is_visible()));
    write_run(&rig.runs(), 9, r#"{"run_id":"9","parent_thread_id":1}"#);
    let _ = rig.updates.unbounded_send(ThreadUpdate {
        thread_id: ThreadId(3),
        revision: 1,
        status: ThreadStatus::Done,
        changed: Changed {
            status: true,
            ..Changed::default()
        },
    });
    m.update(cx, |m, cx| {
        m.set_external_at(NodeKey::Thread(ThreadId(1)), vec![], 9_000, cx);
    });
    cx.executor().advance_clock(CHAT_DEBOUNCE * 2);
    std::thread::sleep(Duration::from_millis(400));
    settle(cx);
    assert_eq!(
        rig.source.reads.load(Ordering::SeqCst),
        1,
        "no reload while hidden"
    );
    assert_eq!(builds(cx, &m), 1);
}

#[gpui::test]
fn a_registry_write_reloads_without_polling(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    assert_eq!(builds(cx, &m), 1);
    // the harness finishes run 3
    write_run(
        &rig.runs(),
        3,
        r#"{"run_id":"3","thread_id":3,"agent":"opencode","started_ms":3000,"finished_ms":8000,
        "status":"done","parent_thread_id":1,"result":{"files_changed":["x"]}}"#,
    );
    let m2 = m.clone();
    assert!(
        wait_until(cx, move |cx| builds(cx, &m2) >= 2),
        "the DirWatcher did not wake the model"
    );
    cx.read_entity(&m, |m, _| {
        let f = m.forest();
        let three = &f.nodes[f.index_of(&NodeKey::Thread(ThreadId(3))).unwrap()];
        assert_eq!(three.status, NodeStatus::Done);
        assert_eq!(three.files, 1);
    });
}

#[gpui::test]
fn only_relevant_chat_updates_reload(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    assert_eq!(builds(cx, &m), 1);
    let update = |thread: i64, changed: Changed| ThreadUpdate {
        thread_id: ThreadId(thread),
        revision: 1,
        status: ThreadStatus::Working,
        changed,
    };
    // streaming text of a known thread, and a status change of a thread the map does not know
    let _ = rig.updates.unbounded_send(update(
        3,
        Changed {
            messages: true,
            ..Changed::default()
        },
    ));
    let _ = rig.updates.unbounded_send(update(
        500,
        Changed {
            status: true,
            ..Changed::default()
        },
    ));
    settle(cx);
    cx.executor().advance_clock(CHAT_DEBOUNCE * 2);
    settle(cx);
    assert_eq!(builds(cx, &m), 1);
    // an approval of a delegated child: one (debounced) reload even for a burst
    for _ in 0..5 {
        let _ = rig.updates.unbounded_send(update(
            3,
            Changed {
                approvals: true,
                status: true,
                ..Changed::default()
            },
        ));
    }
    settle(cx);
    cx.executor().advance_clock(CHAT_DEBOUNCE * 2);
    settle(cx);
    assert_eq!(builds(cx, &m), 2);
}

#[gpui::test]
fn open_run_focuses_the_session_and_selects_the_node(cx: &mut TestAppContext) {
    let rig = rig();
    // a second, unrelated orchestrator
    rig.source.threads.lock().unwrap().extend([
        record(10, "Otro", AgentKind::Codex, ThreadLink::default()),
        record(11, "Hijo de otro", AgentKind::Codex, child_of(10)),
    ]);
    let m = model(cx, &rig);
    // asked before the first load: resolved when the forest arrives
    m.update(cx, |m, cx| {
        m.attach(cx);
        m.open_run(ThreadId(11), cx);
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        let f = m.forest();
        assert_eq!(f.roots.len(), 2);
        assert_eq!(f.nodes[m.root_index().unwrap()].title, "Otro");
        assert_eq!(m.selected(), Some(&NodeKey::Thread(ThreadId(11))));
    });
    // and set_root picks the other one
    m.update(cx, |m, cx| {
        m.set_root(Some(NodeKey::Thread(ThreadId(1))), cx)
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert_eq!(
            m.forest().nodes[m.root_index().unwrap()].title,
            "Orquestador"
        );
    });
}

#[gpui::test]
fn the_prompt_is_redacted_and_comes_from_the_registry_first(cx: &mut TestAppContext) {
    let rig = rig();
    rig.source
        .first
        .lock()
        .unwrap()
        .insert(3, "Escribe la documentación del módulo".into());
    rig.source.approvals.lock().unwrap().insert(
        3,
        vec![ApprovalEntry {
            tool: "Bash".into(),
            detail: "cargo test".into(),
            decision: "allowed".into(),
            at_ms: Some(5),
        }],
    );
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    m.update(cx, |m, cx| {
        m.select(Some(NodeKey::Thread(ThreadId(2))), cx);
        m.select(Some(NodeKey::Thread(ThreadId(3))), cx);
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        let d2 = m.detail(&NodeKey::Thread(ThreadId(2))).unwrap();
        let p = d2.prompt.as_ref().unwrap();
        assert_eq!(p.source, PromptSource::Registry);
        assert!(p.text.starts_with("Revisa los tests"));
        assert!(
            !p.text.contains("sk-ant-api03"),
            "secret leaked: {}",
            p.text
        );
        assert!(p.redacted >= 1);
        // no final_prompt: the child's first user message
        let d3 = m.detail(&NodeKey::Thread(ThreadId(3))).unwrap();
        assert_eq!(d3.prompt.as_ref().unwrap().source, PromptSource::Thread);
        assert_eq!(d3.approvals.len(), 1);
    });
}

#[gpui::test]
fn dropping_the_last_view_detaches_the_model(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let view = {
        let m2 = m.clone();
        cx.new(move |cx| AgentMapView::new(ctx(WidthClass::Regular), m2, cx))
    };
    settle(cx);
    assert_eq!(cx.read_entity(&m, |m, _| m.viewers()), 1);
    assert!(cx.read_entity(&m, |m, _| m.is_watching()));
    drop(view);
    cx.update(|_| {});
    settle(cx);
    assert_eq!(cx.read_entity(&m, |m, _| m.viewers()), 0);
    assert!(!cx.read_entity(&m, |m, _| m.is_watching()));
}

// ---------------------------------------------------------------------------------------------
// View + inspector
// ---------------------------------------------------------------------------------------------

#[gpui::test]
fn clicking_a_row_opens_the_inspector_and_actions_emit_events(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let log = events(cx, &m);
    let (view, vcx) = open_view(cx, &m);
    m.update(vcx, |m, cx| m.refresh(cx));
    vcx.run_until_parked();
    testing::click_id(vcx, "agentmap-row-2");
    assert_eq!(
        view.read_with(vcx, |v, cx| v.selected(cx)),
        Some(NodeKey::Thread(ThreadId(2)))
    );
    vcx.run_until_parked();

    // finished with changes: diff, worktree, apply and duplicate are offered; cancel is not
    testing::click_id(vcx, "agentmap-act-diff");
    testing::click_id(vcx, "agentmap-act-worktree");
    testing::click_id(vcx, "agentmap-act-apply");
    testing::click_id(vcx, "agentmap-act-thread");
    testing::click_id(vcx, "agentmap-act-profile");
    assert!(testing::bounds_of(vcx, "agentmap-act-cancel").is_none());
    assert_eq!(
        *log.borrow(),
        vec![
            AgentMapEvent::OpenDiff("2".into()),
            AgentMapEvent::OpenWorktree("C:/p.wt/tests".into()),
            AgentMapEvent::ApplyChanges("2".into()),
            AgentMapEvent::OpenThread(ThreadId(2)),
            AgentMapEvent::DuplicateAsProfile("2".into()),
        ]
    );
}

#[gpui::test]
fn cancel_and_retry_ask_for_confirmation_first(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let log = events(cx, &m);
    let (_view, vcx) = open_view(cx, &m);
    m.update(vcx, |m, cx| m.refresh(cx));
    vcx.run_until_parked();

    // the running child can be cancelled, not retried
    testing::click_id(vcx, "agentmap-row-3");
    vcx.run_until_parked();
    assert!(testing::bounds_of(vcx, "agentmap-act-retry").is_none());
    testing::click_id(vcx, "agentmap-act-cancel");
    assert!(log.borrow().is_empty(), "asking is not cancelling");
    testing::click_id(vcx, "agentmap-confirm-cancel-back");
    assert!(log.borrow().is_empty());
    testing::click_id(vcx, "agentmap-act-cancel");
    testing::click_id(vcx, "agentmap-confirm-cancel");
    assert_eq!(*log.borrow(), vec![AgentMapEvent::Cancel("3".into())]);

    // the finished one can be retried, with another agent
    testing::click_id(vcx, "agentmap-row-2");
    vcx.run_until_parked();
    testing::click_id(vcx, "agentmap-act-retry");
    testing::click_id(vcx, "agentmap-retry-pick-gemini");
    assert_eq!(log.borrow().len(), 1, "nothing before the confirmation");
    testing::click_id(vcx, "agentmap-confirm-retry");
    assert_eq!(
        log.borrow()[1],
        AgentMapEvent::Retry {
            run_id: "2".into(),
            agent: "gemini".into(),
            model: None
        }
    );
    // the same agent keeps its model
    testing::click_id(vcx, "agentmap-act-retry");
    testing::click_id(vcx, "agentmap-confirm-retry");
    assert_eq!(
        log.borrow()[2],
        AgentMapEvent::Retry {
            run_id: "2".into(),
            agent: "codex".into(),
            model: Some("gpt-5".into())
        }
    );
}

#[gpui::test]
fn the_three_views_render_and_select(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let (view, vcx) = open_view(cx, &m);
    m.update(vcx, |m, cx| m.refresh(cx));
    vcx.run_until_parked();
    assert!(testing::bounds_of(vcx, "agentmap-row-2").is_some());
    view.update(vcx, |v, cx| v.set_mode(MapMode::Graph, cx));
    vcx.run_until_parked();
    assert!(testing::bounds_of(vcx, "agentmap-node-2").is_some());
    testing::click_id(vcx, "agentmap-node-3");
    assert_eq!(
        view.read_with(vcx, |v, cx| v.selected(cx)),
        Some(NodeKey::Thread(ThreadId(3)))
    );
    view.update(vcx, |v, cx| v.set_mode(MapMode::Timeline, cx));
    vcx.run_until_parked();
    assert!(testing::bounds_of(vcx, "agentmap-bar-2").is_some());
    testing::click_id(vcx, "agentmap-bar-2");
    assert_eq!(
        view.read_with(vcx, |v, cx| v.selected(cx)),
        Some(NodeKey::Thread(ThreadId(2)))
    );
    // close the inspector
    testing::click_id(vcx, "agentmap-close");
    assert_eq!(view.read_with(vcx, |v, cx| v.selected(cx)), None);
}

#[gpui::test]
fn the_panel_provider_registers_and_builds(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let mut registry = PanelRegistry::default();
    registry.register(AgentMapPanelProvider::new(m.clone()));
    let provider = registry.get(&PanelId::new(PANEL_ID)).unwrap();
    assert_eq!(provider.id().as_str(), PANEL_ID);
    assert_eq!(
        provider.allowed_zones(),
        &[PanelZone::Main, PanelZone::Right]
    );
    assert_eq!(registry.for_zone(PanelZone::Right).len(), 1);
    assert!(registry.for_zone(PanelZone::Sidebar).is_empty());
    let (_root, vcx) = testing::open_window(cx, |_, _| gpui::Empty);
    vcx.update(|window, app| {
        let view = provider.build(ctx(WidthClass::Compact), window, app);
        assert!(provider.context_changed(&view, &ctx(WidthClass::Regular), window, app));
        assert_eq!(m.read(app).viewers(), 1);
    });
    vcx.run_until_parked();
}

use panel_api::PanelId;

// ---------------------------------------------------------------------------------------------
// Pure inspector helpers
// ---------------------------------------------------------------------------------------------

#[test]
fn actions_follow_the_state_of_the_node() {
    use crate::registry::RunInfo;
    use crate::tree::{ForestInput, build_forest};
    let threads = vec![
        record(1, "O", AgentKind::ClaudeCode, ThreadLink::default()),
        record(2, "C", AgentKind::Codex, child_of(1)),
    ];
    let run = |status: &str, extra: &str| {
        RunInfo::from_json(&format!(
            r#"{{"run_id":"2","agent":"codex","status":"{status}","worktree_path":"C:/w",
            "started_ms":1,"result":{{"files_changed":["a"]}}{extra}}}"#
        ))
        .unwrap()
    };
    let titles = HashMap::new();
    let external = HashMap::new();
    let nodes_for = |r: RunInfo| {
        let runs = [r];
        let f = build_forest(&ForestInput {
            threads: &threads,
            runs: &runs,
            wanted: None,
            now_ms: 10,
            tab_titles: &titles,
            external: &external,
        });
        let i = f.index_of(&NodeKey::Thread(ThreadId(2))).unwrap();
        (f.nodes[i].clone(), f.nodes[f.roots[0]].clone())
    };
    let (done, root) = nodes_for(run("done", ""));
    let a = available(&done);
    assert!(a.apply && a.diff && a.worktree && a.retry && a.duplicate && a.open_thread);
    assert!(!a.cancel);
    // the orchestrator itself offers nothing destructive
    let r = available(&root);
    assert!(!r.cancel && !r.retry && !r.apply && !r.duplicate);

    let (running, _) = nodes_for(run("running", ""));
    let a = available(&running);
    assert!(a.cancel && !a.retry && !a.apply);

    let (applied, _) = nodes_for(run("done", r#","applied_ms":99"#));
    assert!(!available(&applied).apply, "already applied");

    let (failed, _) = nodes_for(run("error", ""));
    assert!(!available(&failed).apply && available(&failed).retry);
}

#[test]
fn limits_are_translated_or_deduced() {
    use crate::registry::RunInfo;
    use crate::tree::{ForestInput, build_forest};
    let threads = vec![record(2, "C", AgentKind::Codex, child_of(1))];
    let titles = HashMap::new();
    let external = HashMap::new();
    let forest = |r: RunInfo| {
        let runs = [r];
        let f = build_forest(&ForestInput {
            threads: &threads,
            runs: &runs,
            wanted: None,
            now_ms: 10,
            tab_titles: &titles,
            external: &external,
        });
        f.nodes[f.index_of(&NodeKey::Thread(ThreadId(2))).unwrap()].clone()
    };
    let recorded = forest(
        RunInfo::from_json(
            r#"{"run_id":"2","effective_limits":{"permission":"accept-edits","timeout_secs":900,
            "budget_tokens":0,"depth":1,"extra_flag":"x"}}"#,
        )
        .unwrap(),
    );
    let rows = limit_rows(&recorded);
    let find = |l: &str| rows.iter().find(|(k, _)| k == l).map(|(_, v)| v.as_str());
    assert_eq!(find("Permiso"), Some("Aceptar ediciones"));
    assert_eq!(find("Tiempo máximo"), Some("15 min"));
    assert_eq!(find("Presupuesto de tokens"), Some("sin límite"));
    assert_eq!(find("Profundidad"), Some("1"));
    assert_eq!(find("Extra flag"), Some("x"));
    // without the field: deduced from what the run says
    let deduced = forest(
        RunInfo::from_json(
            r#"{"run_id":"2","permission":"read-only","started_ms":1000,"deadline_ms":61000}"#,
        )
        .unwrap(),
    );
    let rows = limit_rows(&deduced);
    assert!(rows.contains(&("Permiso".to_string(), "Solo lectura".to_string())));
    assert!(rows.contains(&("Tiempo máximo".to_string(), "1 min".to_string())));

    // the approval mode and the commands approved on their own come from the harness record's `limits`
    let approved = forest(
        RunInfo::from_json(
            r#"{"run_id":"2","permission":"ask","limits":{"approvals":"auto","auto_approved":12,"blocked":1}}"#,
        )
        .unwrap(),
    );
    let run = approved.run.as_ref().unwrap();
    assert_eq!(run.approval_mode.as_deref(), Some("auto"));
    assert_eq!((run.auto_approved, run.blocked), (Some(12), Some(1)));
    assert!(limit_rows(&approved).contains(&(
        "Aprobación de comandos".to_string(),
        "No preguntar".to_string()
    )));
    // an older record has none of it
    assert!(
        !limit_rows(&deduced)
            .iter()
            .any(|(k, _)| k == "Aprobación de comandos")
    );
    assert_eq!(crate::fmt::auto_approved(12), "12 comandos aprobados solos");
    assert_eq!(crate::fmt::auto_approved(1), "1 comando aprobado solo");
}

// ---------------------------------------------------------------------------------------------
// External (non-harness) agent processes
// ---------------------------------------------------------------------------------------------

fn make_ext(pid: u32, started_ms: i64, cmd: &str) -> ExternalProc {
    ExternalProc {
        agent: "agy".into(),
        pid,
        cmdline_redacted: cmd.into(),
        cwd: Some(PathBuf::from("C:/p")),
        model: Some("gemini-3-pro".into()),
        effort: Some("medium".into()),
        started_ms,
        ended_ms: None,
        exit_code: None,
        cpu_pct: Some(12.4),
        rss_bytes: 180 * 1024 * 1024,
    }
}

#[gpui::test]
fn external_procs_stay_after_exit_redact_secrets_cap_at_200_and_prune_with_parent(
    cx: &mut TestAppContext,
) {
    let rig = rig();
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    let parent = NodeKey::Thread(ThreadId(1));

    // Feed a running external agy containing a secret in its command line.
    let ext = make_ext(
        7777,
        5_000,
        "agy -p check --token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
    );
    m.update(cx, |m, cx| {
        m.set_external_at(parent.clone(), vec![ext.clone()], 6_000, cx);
    });
    settle(cx);
    let b1 = builds(cx, &m);
    cx.read_entity(&m, |m, _| {
        let f = m.forest();
        let idx = f.index_of(&ext.key()).expect("external node in forest");
        let n = &f.nodes[idx];
        assert!(matches!(n.kind, NodeKind::External(_)));
        assert_eq!(n.status, NodeStatus::Running);
        let e = n.external().unwrap();
        assert!(!e.cmdline_redacted.contains("sk-ant-api03"));
        assert!(m.profile_seed(&ext.key()).is_none());
    });

    // Feeding the exact same snapshot does not trigger a redundant rebuild.
    m.update(cx, |m, cx| {
        m.set_external_at(parent.clone(), vec![ext.clone()], 7_000, cx);
    });
    settle(cx);
    assert_eq!(builds(cx, &m), b1, "unchanged snapshot must not rebuild");

    // Process disappears from live snapshot: marked finished and retained under parent.
    m.update(cx, |m, cx| {
        m.set_external_at(parent.clone(), vec![], 15_000, cx);
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        let f = m.forest();
        let n = &f.nodes[f.index_of(&ext.key()).expect("finished node retained")];
        assert_eq!(n.status, NodeStatus::Done);
        assert_eq!(n.finished_ms, Some(15_000));
        let e = n.external().unwrap();
        assert_eq!(e.rss_bytes, 0);
        assert_eq!(e.cpu_pct, None);
    });

    // Cap at 200 per session.
    let many: Vec<ExternalProc> = (0..220u32)
        .map(|i| make_ext(8000 + i, 20_000 + i64::from(i), "agy -p work"))
        .collect();
    m.update(cx, |m, cx| {
        m.set_external_at(parent.clone(), many, 25_000, cx);
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert_eq!(m.external_procs(&parent).len(), MAX_EXTERNAL_PER_SESSION);
    });

    // When the parent thread disappears from the chat store, its external nodes are pruned.
    rig.source
        .threads
        .lock()
        .unwrap()
        .retain(|t| t.id != ThreadId(1));
    m.update(cx, |m, cx| m.refresh(cx));
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert!(m.external_procs(&parent).is_empty());
    });
}

#[gpui::test]
fn external_node_renders_in_all_views_and_inspector_emits_events(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let log = events(cx, &m);
    let (view, vcx) = open_view(cx, &m);
    let ext = make_ext(4321, 4_000, "agy -p \"revisa auth\" --effort medium");
    let ext_key = ext.key();
    m.update(vcx, |m, cx| {
        m.set_external_at(NodeKey::Thread(ThreadId(1)), vec![ext.clone()], 8_000, cx);
    });
    vcx.run_until_parked();

    // Renders in Tree, Graph, and Timeline.
    assert!(testing::bounds_of(vcx, "agentmap-row-ext-4321-4000").is_some());
    view.update(vcx, |v, cx| v.set_mode(MapMode::Graph, cx));
    vcx.run_until_parked();
    assert!(testing::bounds_of(vcx, "agentmap-node-ext-4321-4000").is_some());
    view.update(vcx, |v, cx| v.set_mode(MapMode::Timeline, cx));
    vcx.run_until_parked();
    assert!(testing::bounds_of(vcx, "agentmap-bar-ext-4321-4000").is_some());

    // Select the external node and open its inspector.
    testing::click_id(vcx, "agentmap-bar-ext-4321-4000");
    vcx.run_until_parked();
    assert_eq!(view.read_with(vcx, |v, cx| v.selected(cx)), Some(ext_key));

    // No Retry, Apply, Cancel, or Duplicate for external nodes.
    assert!(testing::bounds_of(vcx, "agentmap-act-retry").is_none());
    assert!(testing::bounds_of(vcx, "agentmap-act-apply").is_none());
    assert!(testing::bounds_of(vcx, "agentmap-act-cancel").is_none());
    assert!(testing::bounds_of(vcx, "agentmap-act-profile").is_none());

    // OpenInProcesses and CopyCommand emit immediately.
    testing::click_id(vcx, "agentmap-act-processes");
    testing::click_id(vcx, "agentmap-act-copy-cmd");
    assert_eq!(
        *log.borrow(),
        vec![
            AgentMapEvent::OpenInProcesses(4321),
            AgentMapEvent::CopyCommand("agy -p \"revisa auth\" --effort medium".into()),
        ]
    );

    // StopExternal requires inline confirmation first.
    testing::click_id(vcx, "agentmap-act-stop-external");
    assert_eq!(
        log.borrow().len(),
        2,
        "must ask confirmation before stopping"
    );
    testing::click_id(vcx, "agentmap-confirm-stop-external-back");
    assert_eq!(log.borrow().len(), 2);
    testing::click_id(vcx, "agentmap-act-stop-external");
    testing::click_id(vcx, "agentmap-confirm-stop-external");
    assert_eq!(log.borrow()[2], AgentMapEvent::StopExternal(4321));
}

#[test]
fn external_inspector_facts_and_available_actions() {
    use crate::tree::{ForestInput, build_forest};

    let mut running_ext = make_ext(9001, 10_000, "agy -p check --effort medium");
    running_ext.cpu_pct = Some(3.44);
    let mut exited_ext = make_ext(9002, 10_000, "");
    exited_ext.ended_ms = Some(75_000);
    exited_ext.exit_code = Some(2);
    exited_ext.cpu_pct = None;
    exited_ext.rss_bytes = 0;

    let mut external = HashMap::new();
    external.insert(
        NodeKey::Tab("t1".into()),
        vec![running_ext.clone(), exited_ext.clone()],
    );
    let titles = HashMap::new();
    let f = build_forest(&ForestInput {
        threads: &[],
        runs: &[],
        wanted: None,
        now_ms: 40_000,
        tab_titles: &titles,
        external: &external,
    });

    let run_node = &f.nodes[f.index_of(&running_ext.key()).unwrap()];
    let av = available(run_node);
    assert!(av.stop_external && av.open_in_processes && av.copy_command);
    assert!(!av.retry && !av.apply && !av.cancel && !av.diff && !av.open_thread && !av.duplicate);

    let facts = external_facts(run_node, 40_000);
    let find = |k: &str| facts.iter().find(|(l, _)| l == k).map(|(_, v)| v.as_str());
    assert_eq!(find("Agente"), Some("Antigravity"));
    assert_eq!(find("Modelo"), Some("gemini-3-pro · medio"));
    assert_eq!(find("PID"), Some("9001"));
    assert_eq!(find("Estado"), Some("En marcha"));
    assert_eq!(find("Duración"), Some("30 s"));
    assert_eq!(find("CPU"), Some("3,4%"));
    assert_eq!(find("RAM"), Some("180 MB"));
    assert_eq!(find("Carpeta"), Some("C:/p"));
    assert_eq!(
        EXTERNAL_EXPLANATION,
        "Lanzado fuera de Selachi: sin prompt, límites ni diff. Usa run_agent del MCP selachi para verlos."
    );

    let done_node = &f.nodes[f.index_of(&exited_ext.key()).unwrap()];
    let av_done = available(done_node);
    assert!(!av_done.stop_external, "finished process cannot be stopped");
    assert!(!av_done.copy_command, "empty cmdline cannot be copied");
    assert!(av_done.open_in_processes);
    let done_facts = external_facts(done_node, 90_000);
    let find_done = |k: &str| {
        done_facts
            .iter()
            .find(|(l, _)| l == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(find_done("Estado"), Some("Falló (código 2)"));
    assert_eq!(find_done("Duración"), Some("1 min 05"));
    assert_eq!(find_done("CPU"), None);
    assert_eq!(find_done("RAM"), None);
}

// ---------------------------------------------------------------------------------------------
// Live activity (9A)
// ---------------------------------------------------------------------------------------------

fn write_events(dir: &Path, id: i64, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{id}.events.jsonl")), text).unwrap();
}

fn append_events(dir: &Path, id: i64, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{id}.events.jsonl")))
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

const RUNNING_LOG: &str = concat!(
    r#"{"t":100,"type":"message_delta","text":"Voy a probar."}"#,
    "\n",
    r#"{"t":200,"type":"tool_start","id":"c1","kind":"command","command":"cargo test -p store"}"#,
    "\n",
    r#"{"t":900,"type":"tool_end","id":"c1","exit_code":0,"duration_ms":700,"output_tail":"ok"}"#,
    "\n",
    r#"{"t":1000,"type":"tool_start","id":"c2","kind":"command","command":"cargo clippy"}"#,
    "\n",
);

fn feed_len(cx: &mut TestAppContext, m: &Entity<AgentMapModel>, id: i64) -> usize {
    cx.read_entity(m, |m, _| {
        m.activity_feed(&NodeKey::Thread(ThreadId(id)))
            .map_or(0, |f| f.events.len())
    })
}

fn acts(rig: &Rig) -> usize {
    rig.source.act_reads.load(Ordering::SeqCst)
}

#[gpui::test]
fn running_nodes_show_a_live_status_line_and_finished_ones_do_not(cx: &mut TestAppContext) {
    let rig = rig();
    write_events(&rig.runs(), 3, RUNNING_LOG);
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert_eq!(
            m.live_line(&NodeKey::Thread(ThreadId(3))),
            Some("ejecutando `cargo clippy`"),
            "the command that started and did not end"
        );
        assert_eq!(
            m.live_line(&NodeKey::Thread(ThreadId(2))),
            None,
            "done: no line"
        );
        assert!(m.has_live_lines());
    });
    // reading the line did not read any feed
    assert_eq!(acts(&rig), 0);
}

#[gpui::test]
fn the_activity_tab_reads_only_the_selected_node_and_only_on_that_tab(cx: &mut TestAppContext) {
    let rig = rig();
    write_events(&rig.runs(), 3, RUNNING_LOG);
    write_events(
        &rig.runs(),
        2,
        r#"{"t":5,"type":"message_delta","text":"hecho"}"#,
    );
    let m = model(cx, &rig);
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);

    // inspector open on the Summary tab: the feed is not read
    m.update(cx, |m, cx| m.select(Some(NodeKey::Thread(ThreadId(3))), cx));
    settle(cx);
    assert_eq!(acts(&rig), 0);
    assert_eq!(feed_len(cx, &m, 3), 0);

    // Activity tab: the feed of node 3 (log commands + messages) appears, and only that one
    m.update(cx, |m, cx| m.set_inspector_tab(InspectorTab::Activity, cx));
    settle(cx);
    assert!(acts(&rig) >= 1);
    assert_eq!(feed_len(cx, &m, 3), 3, "message + two commands");
    assert_eq!(feed_len(cx, &m, 2), 0);

    // another node: the feed of 3 is dropped, 2's is read
    m.update(cx, |m, cx| m.select(Some(NodeKey::Thread(ThreadId(2))), cx));
    settle(cx);
    assert_eq!(feed_len(cx, &m, 3), 0);
    assert_eq!(feed_len(cx, &m, 2), 1);

    // back to Summary: nothing is watched and appends cost nothing
    m.update(cx, |m, cx| m.set_inspector_tab(InspectorTab::Summary, cx));
    settle(cx);
    let (reads, passes) = (acts(&rig), cx.read_entity(&m, |m, _| m.passes()));
    append_events(
        &rig.runs(),
        2,
        "{\"t\":6,\"type\":\"message_delta\",\"text\":\"mas\"}\n",
    );
    std::thread::sleep(Duration::from_millis(400));
    settle(cx);
    assert_eq!(acts(&rig), reads);
    assert_eq!(feed_len(cx, &m, 2), 0);
    let _ = passes;
}

#[gpui::test]
fn nothing_activity_related_runs_while_the_map_is_hidden(cx: &mut TestAppContext) {
    let rig = rig();
    write_events(&rig.runs(), 3, RUNNING_LOG);
    let m = model(cx, &rig);
    // hidden: selecting, switching tabs, feeding children and chat updates do no work at all
    m.update(cx, |m, cx| {
        m.select(Some(NodeKey::Thread(ThreadId(3))), cx);
        m.set_inspector_tab(InspectorTab::Activity, cx);
        m.set_external_children_at(
            NodeKey::External {
                pid: 1,
                started_ms: 1,
            },
            vec![],
            0,
            cx,
        );
    });
    let _ = rig.updates.unbounded_send(ThreadUpdate {
        thread_id: ThreadId(3),
        revision: 1,
        status: ThreadStatus::Working,
        changed: Changed {
            messages: true,
            ..Changed::default()
        },
    });
    append_events(
        &rig.runs(),
        3,
        "{\"t\":2000,\"type\":\"message_delta\",\"text\":\"x\"}\n",
    );
    cx.executor().advance_clock(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(300));
    settle(cx);
    assert_eq!(cx.read_entity(&m, |m, _| m.passes()), 0);
    assert_eq!(acts(&rig), 0);
    assert_eq!(rig.source.hint_reads.load(Ordering::SeqCst), 0);

    // shown on the Activity tab of the selected node: it reads
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    assert!(cx.read_entity(&m, |m, _| m.passes()) >= 1);
    assert!(acts(&rig) >= 1);
    assert!(feed_len(cx, &m, 3) >= 3);

    // hidden again: everything is released, and later appends / updates cost nothing
    m.update(cx, |m, cx| m.detach(cx));
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert!(m.activity_feed(&NodeKey::Thread(ThreadId(3))).is_none());
        assert!(m.live_line(&NodeKey::Thread(ThreadId(3))).is_none());
        assert!(!m.is_watching());
    });
    let (passes, reads) = (cx.read_entity(&m, |m, _| m.passes()), acts(&rig));
    append_events(
        &rig.runs(),
        3,
        "{\"t\":3000,\"type\":\"message_delta\",\"text\":\"y\"}\n",
    );
    let _ = rig.updates.unbounded_send(ThreadUpdate {
        thread_id: ThreadId(3),
        revision: 2,
        status: ThreadStatus::Working,
        changed: Changed {
            messages: true,
            approvals: true,
            ..Changed::default()
        },
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(300));
    settle(cx);
    assert_eq!(cx.read_entity(&m, |m, _| m.passes()), passes);
    assert_eq!(acts(&rig), reads);
}

#[gpui::test]
fn an_events_log_append_updates_the_feed_without_polling(cx: &mut TestAppContext) {
    let rig = rig();
    write_events(&rig.runs(), 3, RUNNING_LOG);
    let m = model(cx, &rig);
    m.update(cx, |m, cx| {
        m.select(Some(NodeKey::Thread(ThreadId(3))), cx);
        m.set_inspector_tab(InspectorTab::Activity, cx);
        m.attach(cx);
    });
    settle(cx);
    assert_eq!(feed_len(cx, &m, 3), 3);
    let builds_before = builds(cx, &m);

    // the harness finishes the second command and writes half of the next line
    append_events(
        &rig.runs(),
        3,
        "{\"t\":1500,\"type\":\"tool_end\",\"id\":\"c2\",\"exit_code\":1,\"output_tail\":\"warning\"}\n{\"t\":1600,\"type\":\"file_edit\",\"pa",
    );
    let m2 = m.clone();
    assert!(
        wait_until(cx, move |cx| {
            cx.read_entity(&m2, |m, _| {
                m.activity_feed(&NodeKey::Thread(ThreadId(3))).is_some_and(|f| {
                    f.events.iter().any(|e| {
                        matches!(&e.kind, ActivityKind::Command(c) if c.status == CommandStatus::Failed)
                    })
                })
            })
        }),
        "the DirWatcher did not wake the activity feed"
    );
    assert_eq!(feed_len(cx, &m, 3), 3, "the half line waits");
    assert_eq!(
        builds(cx, &m),
        builds_before,
        "a log append does not reload the forest"
    );
    // the rest of the line arrives
    append_events(&rig.runs(), 3, "th\":\"src/a.rs\",\"added\":4}\n");
    let m3 = m.clone();
    assert!(wait_until(cx, move |cx| feed_len(cx, &m3, 3) == 4));
}

#[gpui::test]
fn chat_events_and_the_log_merge_in_the_feed(cx: &mut TestAppContext) {
    let rig = rig();
    rig.source.acts.lock().unwrap().insert(
        3,
        vec![ActivityEvent::new(
            "m:1.0",
            3_050,
            ActivityKind::Message {
                text: "texto del chat".into(),
                streaming: false,
            },
        )],
    );
    write_events(&rig.runs(), 3, RUNNING_LOG);
    let m = model(cx, &rig);
    m.update(cx, |m, cx| {
        m.select(Some(NodeKey::Thread(ThreadId(3))), cx);
        m.set_inspector_tab(InspectorTab::Activity, cx);
        m.attach(cx);
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        let f = m.activity_feed(&NodeKey::Thread(ThreadId(3))).unwrap();
        // chat message preferred over the log's delta; the log's commands are added
        assert_eq!(f.events.len(), 3);
        assert!(matches!(&f.events[0].kind, ActivityKind::Message { text, .. } if text == "texto del chat"));
        assert!(f.active);
    });
}

#[gpui::test]
fn a_chat_update_of_a_running_thread_refreshes_the_feed(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    m.update(cx, |m, cx| {
        m.select(Some(NodeKey::Thread(ThreadId(3))), cx);
        m.set_inspector_tab(InspectorTab::Activity, cx);
        m.attach(cx);
    });
    settle(cx);
    assert_eq!(feed_len(cx, &m, 3), 0);
    rig.source.acts.lock().unwrap().insert(
        3,
        vec![ActivityEvent::new(
            "m:1.0",
            3_050,
            ActivityKind::Message {
                text: "streaming".into(),
                streaming: true,
            },
        )],
    );
    let _ = rig.updates.unbounded_send(ThreadUpdate {
        thread_id: ThreadId(3),
        revision: 3,
        status: ThreadStatus::Working,
        changed: Changed {
            messages: true,
            ..Changed::default()
        },
    });
    cx.executor().advance_clock(crate::ACTIVITY_DEBOUNCE * 2);
    settle(cx);
    assert_eq!(feed_len(cx, &m, 3), 1);
    // a thread the map does not follow (done node 2) is ignored
    let before = cx.read_entity(&m, |m, _| m.passes());
    let _ = rig.updates.unbounded_send(ThreadUpdate {
        thread_id: ThreadId(2),
        revision: 3,
        status: ThreadStatus::Done,
        changed: Changed {
            messages: true,
            ..Changed::default()
        },
    });
    cx.executor().advance_clock(crate::ACTIVITY_DEBOUNCE * 2);
    settle(cx);
    assert_eq!(cx.read_entity(&m, |m, _| m.passes()), before);
}

fn kid(pid: u32, started: i64, ended: Option<i64>, code: Option<i32>, cmd: &str) -> ExternalChild {
    ExternalChild {
        pid,
        cmdline_redacted: cmd.into(),
        started_ms: started,
        ended_ms: ended,
        exit_code: code,
        cpu_pct: ended.is_none().then_some(20.0),
        rss_bytes: if ended.is_none() { 80 << 20 } else { 0 },
    }
}

#[gpui::test]
fn external_agents_show_their_child_processes_as_commands(cx: &mut TestAppContext) {
    let rig = rig();
    let m = model(cx, &rig);
    let ext = ExternalProc {
        agent: "agy".into(),
        pid: 4242,
        cmdline_redacted: "agy -p check".into(),
        cwd: None,
        model: None,
        effort: None,
        started_ms: 4_000,
        ended_ms: None,
        exit_code: None,
        cpu_pct: Some(5.0),
        rss_bytes: 1 << 20,
    };
    let key = ext.key();
    // hidden: children are not stored
    m.update(cx, |m, cx| {
        m.set_external_children_at(
            key.clone(),
            vec![kid(1, 5_000, None, None, "cargo build")],
            6_000,
            cx,
        )
    });
    assert!(cx.read_entity(&m, |m, _| m.external_children(&key).is_empty()));

    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    m.update(cx, |m, cx| {
        m.set_external_at(NodeKey::Thread(ThreadId(1)), vec![ext.clone()], 4_500, cx);
    });
    settle(cx);
    m.update(cx, |m, cx| {
        m.select(Some(key.clone()), cx);
        m.set_inspector_tab(InspectorTab::Activity, cx);
        m.set_external_children_at(
            key.clone(),
            vec![
                kid(
                    11,
                    5_000,
                    None,
                    None,
                    "cargo build --token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
                ),
                kid(10, 4_100, Some(4_900), Some(0), "git status"),
            ],
            6_000,
            cx,
        );
    });
    settle(cx);
    cx.read_entity(&m, |m, _| {
        let f = m.activity_feed(&key).expect("feed of the external node");
        assert!(f.external);
        assert_eq!(f.events.len(), 2);
        let line = m.live_line(&key).unwrap();
        assert!(line.starts_with("ejecutando `cargo build"), "{line}");
        assert!(!line.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(matches!(&f.events[1].kind, ActivityKind::Command(c) if c.status == CommandStatus::Running && c.cpu_pct == Some(20.0)));
    });
    // the Prompt tab does not exist for external nodes
    m.update(cx, |m, cx| m.set_inspector_tab(InspectorTab::Prompt, cx));
    assert_eq!(
        cx.read_entity(&m, |m, _| m.inspector_tab()),
        InspectorTab::Activity
    );
    // a process that vanished is marked finished and kept
    m.update(cx, |m, cx| {
        m.set_external_children_at(
            key.clone(),
            vec![kid(10, 4_100, Some(4_900), Some(0), "git status")],
            9_000,
            cx,
        )
    });
    cx.read_entity(&m, |m, _| {
        let kids = m.external_children(&key);
        assert!(kids.iter().all(|c| !c.is_running()));
        assert_eq!(m.live_line(&key), None);
    });
}

#[gpui::test]
fn the_activity_tab_renders_filters_and_expands_rows(cx: &mut TestAppContext) {
    let rig = rig();
    write_events(
        &rig.runs(),
        3,
        &format!(
            "{RUNNING_LOG}{}{}",
            "{\"t\":1100,\"type\":\"file_edit\",\"path\":\"src/a.rs\",\"added\":4,\"removed\":1}\n",
            "{\"t\":1200,\"type\":\"approval\",\"request\":\"r\",\"decision\":\"blocked\",\"reason\":\"lista\",\"command\":\"rm -rf x\"}\n",
        ),
    );
    let m = model(cx, &rig);
    let (view, vcx) = open_view(cx, &m);
    m.update(vcx, |m, cx| {
        m.select(Some(NodeKey::Thread(ThreadId(3))), cx);
        m.set_inspector_tab(InspectorTab::Activity, cx);
    });
    vcx.run_until_parked();
    // rows: message, 2 commands, file, approval
    assert_eq!(view.read_with(vcx, |v, _| v.act.shown_len()), 5);

    view.update(vcx, |v, cx| {
        v.set_activity_filter(ActivityFilter::Commands, cx)
    });
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _| v.act.shown_len()),
        3,
        "2 commands + the approval"
    );
    view.update(vcx, |v, cx| {
        v.set_activity_filter(ActivityFilter::Files, cx)
    });
    vcx.run_until_parked();
    assert_eq!(view.read_with(vcx, |v, _| v.act.shown_len()), 1);
    view.update(vcx, |v, cx| {
        v.set_activity_filter(ActivityFilter::Messages, cx)
    });
    vcx.run_until_parked();
    assert_eq!(view.read_with(vcx, |v, _| v.act.shown_len()), 1);

    // the other tabs still render their content
    for tab in [InspectorTab::Prompt, InspectorTab::Summary] {
        view.update(vcx, |v, cx| v.set_inspector_tab(tab, cx));
        vcx.run_until_parked();
    }
    assert!(
        testing::bounds_of(vcx, "agentmap-act-cancel").is_some(),
        "actions live in Resumen"
    );
    assert_eq!(
        view.read_with(vcx, |v, cx| v.inspector_tab(cx)),
        InspectorTab::Summary
    );
}

#[gpui::test]
fn external_usage_lands_on_the_node_survives_reloads_and_is_not_kept_hidden(
    cx: &mut TestAppContext,
) {
    let rig = rig();
    let m = model(cx, &rig);
    let ext = ExternalProc {
        agent: "codex".into(),
        pid: 777,
        cmdline_redacted: "codex exec x".into(),
        cwd: None,
        model: None,
        effort: None,
        started_ms: 4_000,
        ended_ms: None,
        exit_code: None,
        cpu_pct: None,
        rss_bytes: 1 << 20,
    };
    let key = ext.key();
    // hidden: nothing is stored
    m.update(cx, |m, cx| {
        m.set_external_usage(key.clone(), Some((1_000, Some(0.5))), cx)
    });
    assert_eq!(cx.read_entity(&m, |m, _| m.external_usage(&key)), None);

    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    m.update(cx, |m, cx| {
        m.set_external_at(NodeKey::Thread(ThreadId(1)), vec![ext.clone()], 4_500, cx);
    });
    settle(cx);
    m.update(cx, |m, cx| {
        m.set_external_usage(key.clone(), Some((12_345, Some(0.42))), cx)
    });
    let node = |cx: &mut TestAppContext| {
        cx.read_entity(&m, |m, _| {
            let f = m.forest();
            let n = &f.nodes[f.index_of(&key).unwrap()];
            (n.tokens, n.cost_usd)
        })
    };
    assert_eq!(node(cx), (Some(12_345), Some(0.42)));
    // a reload rebuilds the forest: the usage is applied again
    m.update(cx, |m, cx| m.refresh(cx));
    settle(cx);
    assert_eq!(node(cx), (Some(12_345), Some(0.42)));
    // tokens without a price: no cost
    m.update(cx, |m, cx| {
        m.set_external_usage(key.clone(), Some((5, None)), cx)
    });
    assert_eq!(node(cx), (Some(5), None));
    m.update(cx, |m, cx| m.set_external_usage(key.clone(), None, cx));
    assert_eq!(node(cx), (None, None));
    // the last view going away drops everything
    m.update(cx, |m, cx| {
        m.set_external_usage(key.clone(), Some((1, None)), cx);
        m.detach(cx);
    });
    assert_eq!(cx.read_entity(&m, |m, _| m.external_usage(&key)), None);
}
