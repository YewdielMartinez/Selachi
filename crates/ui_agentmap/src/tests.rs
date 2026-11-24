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
use chat_core::{
    Changed, ThreadId, ThreadLink, ThreadRecord, ThreadStatus, ThreadUpdate,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{Entity, TestAppContext, VisualTestContext};
use panel_api::{PanelCtx, PanelProvider, PanelRegistry, PanelZone, WidthClass};
use theme::Theme;
use ui_kit::testing;

use crate::events::AgentMapEvent;
use crate::inspector::{available, limit_rows};
use crate::model::{AgentMapModel, CHAT_DEBOUNCE, PromptSource};
use crate::registry::ApprovalEntry;
use crate::source::SessionSource;
use crate::tree::{NodeKey, NodeStatus};
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
        record(1, "Orquestador", AgentKind::ClaudeCode, ThreadLink::default()),
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

fn open_view(
    cx: &mut TestAppContext,
    m: &Entity<AgentMapModel>,
) -> (Entity<AgentMapView>, &mut VisualTestContext) {
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
    // never attached: refresh and set_root do nothing
    m.update(cx, |m, cx| {
        m.refresh(cx);
        m.set_root(Some(NodeKey::Thread(ThreadId(1))), cx);
        m.open_run(ThreadId(2), cx);
    });
    settle(cx);
    assert_eq!(rig.source.reads.load(Ordering::SeqCst), 0);
    assert!(!cx.read_entity(&m, |m, _| m.is_watching()));

    // shown: one load and the listeners
    m.update(cx, |m, cx| m.attach(cx));
    settle(cx);
    assert_eq!(rig.source.reads.load(Ordering::SeqCst), 1);
    assert!(cx.read_entity(&m, |m, _| m.is_watching()));

    // hidden again: no watcher, and changes on disk or in the chat cost nothing
    m.update(cx, |m, cx| m.detach(cx));
    settle(cx);
    assert!(!cx.read_entity(&m, |m, _| m.is_watching()));
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
    cx.executor().advance_clock(CHAT_DEBOUNCE * 2);
    std::thread::sleep(Duration::from_millis(400));
    settle(cx);
    assert_eq!(rig.source.reads.load(Ordering::SeqCst), 1, "no reload while hidden");
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
    m.update(cx, |m, cx| m.set_root(Some(NodeKey::Thread(ThreadId(1))), cx));
    settle(cx);
    cx.read_entity(&m, |m, _| {
        assert_eq!(m.forest().nodes[m.root_index().unwrap()].title, "Orquestador");
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
        assert!(!p.text.contains("sk-ant-api03"), "secret leaked: {}", p.text);
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
    assert_eq!(provider.allowed_zones(), &[PanelZone::Main, PanelZone::Right]);
    assert_eq!(registry.for_zone(PanelZone::Right).len(), 1);
    assert!(registry.for_zone(PanelZone::Sidebar).is_empty());
    let (_root, vcx) = testing::open_window(cx, |_, _| gpui::Empty);
    vcx.update(|window, app| {
        let view = provider.build(ctx(WidthClass::Compact), window, app);
        assert!(provider.context_changed(&view, &ctx(WidthClass::Regular), window, app));
    });
    vcx.run_until_parked();
    assert_eq!(cx.read_entity(&m, |m, _| m.viewers()), 1);
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
    let nodes_for = |r: RunInfo| {
        let runs = [r];
        let f = build_forest(&ForestInput {
            threads: &threads,
            runs: &runs,
            wanted: None,
            now_ms: 10,
            tab_titles: &titles,
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
    let forest = |r: RunInfo| {
        let runs = [r];
        let f = build_forest(&ForestInput {
            threads: &threads,
            runs: &runs,
            wanted: None,
            now_ms: 10,
            tab_titles: &titles,
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
}
