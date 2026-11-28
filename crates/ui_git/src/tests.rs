//! Behavioural tests over real temporary repositories: the visibility gate (nothing runs while
//! hidden, the watcher is dropped), the single-flight refresh, the confirmation gating of
//! sensitive operations, events, and the compact / regular switching of the panel.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use git_svc::GitAction;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use panel_api::{PanelCtx, PanelZone, WidthClass};
use theme::Theme;

use crate::cmd::GitOp;
use crate::data::DiffTarget;
use crate::data::testing::{git, temp_repo};
use crate::gate::Gate;
use crate::model::{DiffState, ExecMode, GitModel, GitPanelEvent, Load};
use crate::sections::Section;
use crate::view::{GitPanel, Layout, Tab, layout_for};

fn model_for(cx: &mut TestAppContext, dir: &std::path::Path) -> Entity<GitModel> {
    let model = cx.new(|_| GitModel::new());
    let dir = dir.to_path_buf();
    model.update(cx, |m, cx| m.set_root(Some(dir), cx));
    model
}

fn commit_count(dir: &std::path::Path) -> usize {
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn events(cx: &mut TestAppContext, model: &Entity<GitModel>) -> Rc<RefCell<Vec<GitPanelEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let sub = cx.update(|cx| {
        cx.subscribe(model, move |_, ev: &GitPanelEvent, _| {
            sink.borrow_mut().push(ev.clone())
        })
    });
    std::mem::forget(sub);
    seen
}

#[gpui::test]
async fn nothing_runs_while_hidden(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.stats().reads,
            0,
            "a hidden panel must not read the repository"
        );
        assert!(!m.has_watcher());
        assert_eq!(m.load_state(), &Load::Idle);
        assert!(m.data().is_none());
    });
    // Asking to refresh while hidden is a no-op too.
    model.update(cx, |m, cx| m.refresh(true, cx));
    cx.run_until_parked();
    assert_eq!(model.read_with(cx, |m, _| m.stats().reads), 0);
}

#[gpui::test]
async fn visible_reads_and_watches_hidden_drops_the_watcher(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.load_state(), &Load::Ready);
        assert_eq!(m.stats().reads, 1);
        assert!(m.has_watcher(), "the watcher runs while visible");
        assert_eq!(m.data().unwrap().commits.len(), 1);
    });

    model.update(cx, |m, cx| m.set_visible(false, cx));
    model.read_with(cx, |m, _| {
        assert!(!m.has_watcher(), "the watcher is dropped when hidden")
    });
    // Files change while hidden: nothing reads.
    std::fs::write(dir.path().join("a.txt"), "changed while hidden\n").unwrap();
    cx.run_until_parked();
    assert_eq!(model.read_with(cx, |m, _| m.stats().reads), 1);

    // Showing again takes one fresh read and restarts the watcher; it sees the change.
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.stats().reads, 2);
        assert!(m.has_watcher());
        assert_eq!(m.stats().watcher_starts, 2);
        assert_eq!(m.data().unwrap().status.unstaged.len(), 1);
    });
}

#[gpui::test]
async fn last_view_dropping_hides_the_panel(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| {
        m.attach_view();
        m.attach_view();
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    model.update(cx, |m, cx| m.detach_view(cx));
    assert!(
        model.read_with(cx, |m, _| m.has_watcher()),
        "one view is still open"
    );
    model.update(cx, |m, cx| m.detach_view(cx));
    assert!(!model.read_with(cx, |m, _| m.has_watcher()));
    assert!(!model.read_with(cx, |m, _| m.is_visible()));
}

#[gpui::test]
async fn refresh_bursts_cost_two_reads_at_most(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    let before = model.read_with(cx, |m, _| m.stats().reads);
    model.update(cx, |m, cx| {
        for _ in 0..25 {
            m.refresh(false, cx);
        }
        assert!(m.refresh_in_flight());
    });
    cx.run_until_parked();
    let after = model.read_with(cx, |m, _| m.stats().reads);
    assert_eq!(
        after - before,
        2,
        "one in flight plus exactly one queued follow-up"
    );
    assert!(!model.read_with(cx, |m, _| m.refresh_in_flight()));
}

#[gpui::test]
async fn unchanged_data_does_not_notify(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    let notified = Rc::new(RefCell::new(0));
    let n = notified.clone();
    let _sub = cx.update(|cx| cx.observe(&model, move |_, _| *n.borrow_mut() += 1));
    model.update(cx, |m, cx| m.refresh(false, cx));
    cx.run_until_parked();
    assert_eq!(
        *notified.borrow(),
        0,
        "a read that finds nothing new must not redraw"
    );
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    model.update(cx, |m, cx| m.refresh(false, cx));
    cx.run_until_parked();
    assert_eq!(*notified.borrow(), 1);
}

#[gpui::test]
async fn a_commit_never_runs_without_confirm(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();

    // Confirming with no card does nothing.
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(commit_count(dir.path()), 1);

    let op = GitOp::Action(GitAction::Commit {
        message: "feat: x".into(),
    });
    model.update(cx, |m, cx| m.request(op.clone(), cx));
    // While the preflight is computing, confirming still does nothing.
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert!(matches!(
        model.read_with(cx, |m, _| m.gate().clone()),
        Gate::Confirm { .. }
    ));
    assert_eq!(
        commit_count(dir.path()),
        1,
        "the card is showing, nothing ran"
    );

    // Cancelling releases nothing either.
    model.update(cx, |m, cx| m.dismiss(cx));
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(commit_count(dir.path()), 1);

    model.update(cx, |m, cx| m.request(op, cx));
    cx.run_until_parked();
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(commit_count(dir.path()), 2, "confirmed: the commit ran");
    let g = model.read_with(cx, |m, _| m.gate().clone());
    assert!(
        matches!(g, Gate::Done { ref outcome, .. } if outcome.ok),
        "{g:?}"
    );
    assert!(
        model.read_with(cx, |m, _| m.data().unwrap().status.staged.is_empty()),
        "refreshed after the op"
    );
}

#[gpui::test]
async fn blocked_commit_cannot_be_confirmed(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap(); // not staged
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    model.update(cx, |m, cx| {
        m.request(
            GitOp::Action(GitAction::Commit {
                message: "x".into(),
            }),
            cx,
        )
    });
    cx.run_until_parked();
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(
        commit_count(dir.path()),
        1,
        "nothing staged: the review is blocked"
    );
    assert!(
        matches!(model.read_with(cx, |m, _| m.gate().clone()), Gate::Confirm { review, .. } if review.blocked.is_some())
    );
}

#[gpui::test]
async fn read_only_refuses_every_operation(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| {
        m.set_read_only(true, cx);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    for op in [
        GitOp::Stage("a.txt".into()),
        GitOp::StageAll,
        GitOp::CreateCheckpoint { label: "x".into() },
        GitOp::Action(GitAction::Push),
    ] {
        model.update(cx, |m, cx| m.request(op, cx));
        cx.run_until_parked();
    }
    model.read_with(cx, |m, _| {
        assert_eq!(m.gate(), &Gate::Idle);
        assert!(m.quiet_error().unwrap().contains("Solo lectura"));
        assert!(
            m.data().unwrap().status.staged.is_empty(),
            "nothing was staged"
        );
        assert!(m.data().unwrap().checkpoints.is_empty());
    });
}

#[gpui::test]
async fn staging_updates_the_status(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    model.update(cx, |m, cx| m.request(GitOp::Stage("a.txt".into()), cx));
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        let st = &m.data().unwrap().status;
        assert_eq!((st.staged.len(), st.unstaged.len()), (1, 0));
        assert_eq!(m.gate(), &Gate::Idle, "a quiet success leaves no banner");
    });
    model.update(cx, |m, cx| {
        m.request(
            GitOp::Unstage {
                path: "a.txt".into(),
                unborn: false,
            },
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.data().unwrap().status.staged.len()),
        0
    );
    // A failing quiet op surfaces as an error line, not silently.
    model.update(cx, |m, cx| {
        m.request(GitOp::Stage("does-not-exist.txt".into()), cx)
    });
    cx.run_until_parked();
    let shown = model.read_with(cx, |m, _| {
        matches!(m.gate(), Gate::Done { outcome, .. } if !outcome.ok) || m.quiet_error().is_some()
    });
    assert!(shown);
}

#[gpui::test]
async fn events_reach_the_host(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();
    let model = model_for(cx, dir.path());
    let seen = events(cx, &model);
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();

    let target = DiffTarget::Working {
        section: Section::Changes,
        path: "a.txt".into(),
    };
    model.update(cx, |m, cx| m.toggle_diff(target.clone(), cx));
    cx.run_until_parked();
    let ready = model.read_with(cx, |m, _| {
        matches!(m.diff().map(|d| &d.state), Some(DiffState::Ready(_)))
    });
    assert!(ready);

    model.update(cx, |m, cx| {
        m.open_file(std::path::Path::new("a.txt"), Some(2), cx);
        m.open_diff(std::path::Path::new("a.txt"), cx);
        m.ask_agent(cx);
    });
    let got = seen.borrow().clone();
    let root = dir.path().to_path_buf();
    assert!(
        matches!(&got[0], GitPanelEvent::OpenFile { path, line: Some(2) } if path.ends_with("a.txt") && path.starts_with(&root) || path.file_name().is_some_and(|n| n == "a.txt"))
    );
    assert!(
        matches!(&got[1], GitPanelEvent::OpenDiff { path } if path.file_name().is_some_and(|n| n == "a.txt"))
    );
    let GitPanelEvent::AskAgent { context_markdown } = &got[2] else {
        panic!("{:?}", got[2])
    };
    assert!(
        context_markdown.contains("a.txt") && context_markdown.contains("+TWO"),
        "{context_markdown}"
    );

    // Toggling the same file closes the diff.
    model.update(cx, |m, cx| m.toggle_diff(target, cx));
    assert!(model.read_with(cx, |m, _| m.diff().is_none()));
}

#[gpui::test]
async fn terminal_mode_emits_the_command_instead_of_running_it(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| {
        m.set_exec_mode(ExecMode::Terminal);
        m.set_visible(true, cx);
    });
    let seen = events(cx, &model);
    cx.run_until_parked();
    model.update(cx, |m, cx| {
        m.request(
            GitOp::Action(GitAction::Commit {
                message: "feat: visible".into(),
            }),
            cx,
        )
    });
    cx.run_until_parked();
    assert!(
        seen.borrow().is_empty(),
        "nothing is emitted before the confirmation"
    );
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    let got = seen.borrow().clone();
    let [GitPanelEvent::RunInTerminal { argv, cwd }] = got.as_slice() else {
        panic!("{got:?}")
    };
    assert_eq!(argv, &["git", "commit", "-m", "feat: visible"]);
    assert!(cwd.ends_with(dir.path().file_name().unwrap()) || cwd == &PathBuf::from(dir.path()));
    assert_eq!(
        commit_count(dir.path()),
        1,
        "the host runs it, not the panel"
    );
}

#[gpui::test]
async fn changing_the_root_drops_the_old_repository(cx: &mut TestAppContext) {
    let a = temp_repo();
    let b = temp_repo();
    let model = model_for(cx, a.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    let first = model.read_with(cx, |m, _| m.root().map(std::path::Path::to_path_buf));
    model.update(cx, |m, cx| m.set_root(Some(b.path().to_path_buf()), cx));
    assert!(model.read_with(cx, |m, _| m.data().is_none() && !m.has_watcher()));
    cx.run_until_parked();
    let second = model.read_with(cx, |m, _| m.root().map(std::path::Path::to_path_buf));
    assert_ne!(first, second);
    assert!(model.read_with(cx, |m, _| m.has_watcher()));

    model.update(cx, |m, cx| m.set_root(None, cx));
    assert_eq!(
        model.read_with(cx, |m, _| m.load_state().clone()),
        Load::NoRoot
    );
    // A folder that is not a repository is reported, not an error.
    let plain = tempfile::tempdir().unwrap();
    model.update(cx, |m, cx| m.set_root(Some(plain.path().to_path_buf()), cx));
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.load_state().clone()),
        Load::NotARepo
    );
}

#[gpui::test]
async fn checkpoint_flow_through_the_model(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
    model.update(cx, |m, cx| {
        m.request(
            GitOp::CreateCheckpoint {
                label: "uno".into(),
            },
            cx,
        )
    });
    cx.run_until_parked();
    let id = model
        .read_with(cx, |m, _| {
            m.data().unwrap().checkpoints.first().map(|c| c.id.clone())
        })
        .expect("a checkpoint");
    std::fs::write(dir.path().join("a.txt"), "v2\n").unwrap();

    model.update(cx, |m, cx| m.toggle_checkpoint_view(id.clone(), cx));
    cx.run_until_parked();
    let files = model
        .read_with(cx, |m, _| m.checkpoint_view().cloned())
        .unwrap();
    assert!(matches!(files.state, crate::model::CheckpointFiles::Ready(ref f) if f.len() == 1));

    model.update(cx, |m, cx| m.request(GitOp::RestoreCheckpoint { id }, cx));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "v2\n",
        "the card is waiting, nothing restored yet"
    );
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "v1\n"
    );
    let cps = model.read_with(cx, |m, _| m.data().unwrap().checkpoints.clone());
    assert!(
        cps.iter().any(|c| c.auto),
        "the pre-restore checkpoint is listed"
    );
}

fn ctx(class: WidthClass, root: &std::path::Path) -> PanelCtx {
    PanelCtx {
        zone: PanelZone::Right,
        width_class: class,
        cwd: Some(root.to_path_buf()),
        project_root: Some(root.to_path_buf()),
        theme: Theme::dark(),
    }
}

#[test]
fn layout_selection() {
    assert_eq!(layout_for(WidthClass::Compact, 250.0), Layout::Tabs);
    assert_eq!(
        layout_for(WidthClass::Compact, 900.0),
        Layout::Tabs,
        "the class decides, not the pixels"
    );
    assert_eq!(layout_for(WidthClass::Regular, 420.0), Layout::Stacked);
    assert_eq!(layout_for(WidthClass::Regular, 900.0), Layout::Columns);
}

#[gpui::test]
async fn compact_collapses_to_tabs_and_regular_stacks(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    let model = cx.new(|_| GitModel::new());
    let root = dir.path().to_path_buf();
    let m = model.clone();
    let c = ctx(WidthClass::Compact, &root);
    let (panel, vcx) = cx.add_window_view(move |_, cx| GitPanel::new(c, m, cx));
    let vcx: &mut VisualTestContext = vcx;
    panel.update(vcx, |p, cx| p.retarget(ctx(WidthClass::Compact, &root), cx));
    vcx.run_until_parked();
    assert_eq!(panel.read_with(vcx, |p, _| p.layout()), Layout::Tabs);
    assert!(panel.read_with(vcx, |p, _| p.is_compact()));
    assert!(
        model.read_with(vcx, |m, _| m.is_visible()),
        "painting makes the panel visible"
    );
    assert_eq!(
        model.read_with(vcx, |m, _| m.load_state().clone()),
        Load::Ready
    );

    for tab in Tab::ALL {
        panel.update(vcx, |p, cx| p.select_tab(tab, cx));
        vcx.run_until_parked();
        assert_eq!(panel.read_with(vcx, |p, _| p.tab()), tab);
    }

    panel.update(vcx, |p, cx| p.retarget(ctx(WidthClass::Regular, &root), cx));
    vcx.run_until_parked();
    // The test window is wide, so the measured width may promote Regular to two columns.
    assert!(matches!(
        panel.read_with(vcx, |p, _| p.layout()),
        Layout::Stacked | Layout::Columns
    ));
    assert!(!panel.read_with(vcx, |p, _| p.is_compact()));

    // Back to compact (the shell resized the zone).
    panel.update(vcx, |p, cx| p.retarget(ctx(WidthClass::Compact, &root), cx));
    vcx.run_until_parked();
    assert_eq!(panel.read_with(vcx, |p, _| p.layout()), Layout::Tabs);
}

#[gpui::test]
async fn every_state_paints_without_panicking(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(dir.path().join("new.rs"), "fn x() {}\n").unwrap();
    git(dir.path(), &["add", "src"]);
    let model = cx.new(|_| GitModel::new());
    let root = dir.path().to_path_buf();
    let (m, c) = (model.clone(), ctx(WidthClass::Regular, &root));
    let (panel, vcx) = cx.add_window_view(move |_, cx| {
        m.update(cx, |m, cx| m.set_root(Some(c.cwd.clone().unwrap()), cx));
        GitPanel::new(c, m, cx)
    });
    vcx.run_until_parked();
    model.update(vcx, |m, cx| {
        m.toggle_diff(
            DiffTarget::Working {
                section: Section::Changes,
                path: "a.txt".into(),
            },
            cx,
        );
        m.request(
            GitOp::CreateCheckpoint {
                label: "uno".into(),
            },
            cx,
        );
    });
    vcx.run_until_parked();
    model.update(vcx, |m, cx| m.request(GitOp::Action(GitAction::Push), cx));
    vcx.run_until_parked();
    panel.update(vcx, |p, cx| {
        p.set_worktree_menu(true, cx);
        p.set_message("feat: hola", "cuerpo", cx);
    });
    vcx.run_until_parked();
    for class in [WidthClass::Compact, WidthClass::Regular] {
        panel.update(vcx, |p, cx| p.retarget(ctx(class, &root), cx));
        for tab in Tab::ALL {
            panel.update(vcx, |p, cx| p.select_tab(tab, cx));
            vcx.run_until_parked();
        }
    }
    assert_eq!(
        panel.read_with(vcx, |p, cx| p.message(cx)),
        "feat: hola\n\ncuerpo"
    );
}

// ---- the commit box through the keyboard (a real window, no desktop input) -----------------------

/// A panel over a repo with `a.txt` staged (or only modified), painted and loaded.
fn commit_window(
    cx: &mut TestAppContext,
    stage: bool,
) -> (
    tempfile::TempDir,
    Entity<GitModel>,
    Entity<GitPanel>,
    &mut VisualTestContext,
) {
    let dir = temp_repo();
    std::fs::write(
        dir.path().join("a.txt"),
        "changed
",
    )
    .unwrap();
    if stage {
        git(dir.path(), &["add", "a.txt"]);
    }
    let model = cx.new(|_| GitModel::new());
    let (m, c) = (model.clone(), ctx(WidthClass::Regular, dir.path()));
    let (panel, vcx) = ui_kit::testing::open_window(cx, move |_, cx| {
        m.update(cx, |m, cx| m.set_root(c.cwd.clone(), cx));
        GitPanel::new(c, m, cx)
    });
    vcx.run_until_parked();
    assert_eq!(
        model.read_with(vcx, |m, _| m.load_state().clone()),
        Load::Ready
    );
    (dir, model, panel, vcx)
}

fn gate(model: &Entity<GitModel>, vcx: &VisualTestContext) -> Gate {
    model.read_with(vcx, |m, _| m.gate().clone())
}

#[gpui::test]
async fn enter_in_the_subject_opens_the_review_and_commits_only_after_confirm(
    cx: &mut TestAppContext,
) {
    let (dir, model, panel, vcx) = commit_window(cx, true);
    panel.update_in(vcx, |p, window, cx| p.focus_field(false, window, cx));
    ui_kit::testing::type_text(vcx, "feat: from the keyboard");
    ui_kit::testing::press(vcx, "enter");
    vcx.run_until_parked();
    assert!(
        matches!(gate(&model, vcx), Gate::Confirm { .. }),
        "Enter shows the review card: {:?}",
        gate(&model, vcx)
    );
    assert_eq!(commit_count(dir.path()), 1, "Enter alone never commits");
    // Pressing Enter again with the card open does not run it either.
    ui_kit::testing::press(vcx, "enter");
    vcx.run_until_parked();
    assert_eq!(commit_count(dir.path()), 1);
    ui_kit::testing::click_id(vcx, "gate-confirm");
    vcx.run_until_parked();
    assert_eq!(commit_count(dir.path()), 2, "the confirm button ran it");
    assert_eq!(
        panel.read_with(vcx, |p, cx| p.message(cx)),
        "",
        "the box is cleared after a commit"
    );
}

#[gpui::test]
async fn commit_needs_a_subject_and_staged_files(cx: &mut TestAppContext) {
    // A subject but nothing staged.
    let (dir, model, panel, vcx) = commit_window(cx, false);
    panel.update_in(vcx, |p, window, cx| p.focus_field(false, window, cx));
    ui_kit::testing::type_text(vcx, "feat: x");
    ui_kit::testing::press(vcx, "enter");
    vcx.run_until_parked();
    assert_eq!(
        gate(&model, vcx),
        Gate::Idle,
        "nothing staged: no review card"
    );
    assert_eq!(commit_count(dir.path()), 1);
}

#[gpui::test]
async fn empty_subject_does_not_open_the_review(cx: &mut TestAppContext) {
    let (_dir, model, panel, vcx) = commit_window(cx, true);
    panel.update_in(vcx, |p, window, cx| p.focus_field(false, window, cx));
    ui_kit::testing::type_text(vcx, "   ");
    ui_kit::testing::press(vcx, "enter");
    vcx.run_until_parked();
    assert_eq!(gate(&model, vcx), Gate::Idle);
}

#[gpui::test]
async fn description_is_multiline_and_ctrl_enter_starts_the_commit(cx: &mut TestAppContext) {
    let (dir, model, panel, vcx) = commit_window(cx, true);
    panel.update(vcx, |p, cx| p.set_message("feat: x", "", cx));
    vcx.run_until_parked();
    panel.update_in(vcx, |p, window, cx| p.focus_field(true, window, cx));
    ui_kit::testing::type_text(vcx, "linea uno");
    ui_kit::testing::press(vcx, "enter");
    ui_kit::testing::type_text(vcx, "linea dos");
    vcx.run_until_parked();
    assert_eq!(
        gate(&model, vcx),
        Gate::Idle,
        "Enter in the description is a newline"
    );
    assert_eq!(
        panel.read_with(vcx, |p, cx| p.message(cx)),
        "feat: x

linea uno
linea dos"
    );
    ui_kit::testing::press(vcx, "ctrl-enter");
    vcx.run_until_parked();
    assert!(matches!(gate(&model, vcx), Gate::Confirm { .. }));
    assert_eq!(commit_count(dir.path()), 1);
}

// ---------------------------------------------------------------------------------------------
// `[git] confirm_sensitive`
// ---------------------------------------------------------------------------------------------

#[gpui::test]
async fn confirm_sensitive_off_commits_without_a_card_and_on_still_asks(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    let op = GitOp::Action(GitAction::Commit {
        message: "feat: relaxed".into(),
    });

    // On (the default): the card opens and nothing runs.
    model.update(cx, |m, cx| m.request(op.clone(), cx));
    cx.run_until_parked();
    assert!(matches!(
        model.read_with(cx, |m, _| m.gate().clone()),
        Gate::Confirm { .. }
    ));
    assert_eq!(commit_count(dir.path()), 1);
    model.update(cx, |m, cx| m.dismiss(cx));

    // Off: the commit runs at once.
    model.update(cx, |m, _| m.set_confirm_sensitive(false));
    model.update(cx, |m, cx| m.request(op, cx));
    cx.run_until_parked();
    assert_eq!(commit_count(dir.path()), 2, "no card: the commit ran");
    let g = model.read_with(cx, |m, _| m.gate().clone());
    assert!(
        matches!(g, Gate::Done { ref outcome, .. } if outcome.ok),
        "the result banner still shows: {g:?}"
    );
}

#[gpui::test]
async fn confirm_sensitive_off_in_terminal_mode_still_runs_visibly(cx: &mut TestAppContext) {
    let dir = temp_repo();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| {
        m.set_exec_mode(ExecMode::Terminal);
        m.set_confirm_sensitive(false);
        m.set_visible(true, cx);
    });
    let seen = events(cx, &model);
    cx.run_until_parked();
    model.update(cx, |m, cx| {
        m.request(
            GitOp::Action(GitAction::Commit {
                message: "feat: visible".into(),
            }),
            cx,
        )
    });
    cx.run_until_parked();
    let got = seen.borrow().clone();
    let [GitPanelEvent::RunInTerminal { argv, .. }] = got.as_slice() else {
        panic!("{got:?}")
    };
    assert_eq!(argv, &["git", "commit", "-m", "feat: visible"]);
}

#[gpui::test]
async fn confirm_sensitive_off_never_skips_the_card_of_a_destructive_operation(
    cx: &mut TestAppContext,
) {
    let dir = temp_repo();
    let model = model_for(cx, dir.path());
    model.update(cx, |m, cx| {
        m.set_confirm_sensitive(false);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    for op in [
        GitOp::RestoreCheckpoint { id: "none".into() },
        GitOp::RemoveWorktree {
            path: dir.path().join("nope"),
            force: true,
        },
        GitOp::Action(GitAction::Checkout {
            target: "HEAD".into(),
        }),
    ] {
        model.update(cx, |m, cx| m.request(op.clone(), cx));
        // never straight to running: it is preflighting or showing the card
        let g = model.read_with(cx, |m, _| m.gate().clone());
        assert!(
            matches!(g, Gate::Preflighting { .. }),
            "{op:?} must go through the preflight: {g:?}"
        );
        cx.run_until_parked();
        let g = model.read_with(cx, |m, _| m.gate().clone());
        assert!(
            matches!(g, Gate::Confirm { .. } | Gate::Done { .. }),
            "{op:?}: {g:?}"
        );
        assert!(
            !matches!(g, Gate::Running { .. }),
            "{op:?} must not run unconfirmed"
        );
        model.update(cx, |m, cx| m.dismiss(cx));
    }
}
