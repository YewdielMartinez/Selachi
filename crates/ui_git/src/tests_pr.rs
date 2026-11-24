//! PR / CI status (B1) and "Redactar con agente" / "Crear PR" (B3) through the model, with a fake
//! `gh` (a `.cmd` shim in a temp dir). The real `gh` never runs and no real repository is touched;
//! nothing is sent to an agent: the tests play the host.
#![cfg(windows)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use git_svc::{DraftKind, GhClient, GitAction};
use gpui::{AppContext as _, Entity, TestAppContext};
use panel_api::{PanelCtx, PanelZone, WidthClass};
use theme::Theme;

use crate::cmd::GitOp;
use crate::data::testing::{git, temp_repo};
use crate::gate::Gate;
use crate::model::{DraftReady, GitModel, GitPanelEvent};
use crate::pr::{FOCUS_STALE, PrCreate, PrCreated, PrTone, PrView};
use crate::view::{GitPanel, Tab};

const SHIM: &str = r#"@echo off
echo %*>>"%~dp0calls.log"
if "%1"=="auth" goto auth
if "%1 %2"=="pr list" goto prlist
if "%1 %2"=="pr checks" goto checks
if "%1 %2"=="pr create" goto create
if "%1 %2"=="run view" goto runview
exit /b 2
:auth
if exist "%~dp0unauth" exit /b 1
exit /b 0
:prlist
type "%~dp0pr_list.json"
exit /b 0
:checks
type "%~dp0checks.json"
exit /b 1
:create
findstr "^" > "%~dp0stdin.txt"
echo https://github.com/acme/selachi/pull/7
exit /b 0
:runview
type "%~dp0log.txt"
exit /b 0
"#;

const PR_JSON: &str = r#"[{"headRefName":"main","isDraft":false,"mergeable":"MERGEABLE","number":12,"reviewDecision":"REVIEW_REQUIRED","state":"OPEN","title":"feat(ci): estado de PR en el panel","url":"https://github.com/acme/selachi/pull/12"}]"#;

struct Shim {
    dir: tempfile::TempDir,
}

impl Shim {
    fn new() -> Shim {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("gh.cmd"), SHIM).unwrap();
        std::fs::write(p.join("pr_list.json"), PR_JSON).unwrap();
        std::fs::write(
            p.join("checks.json"),
            include_str!("../../git_svc/tests/fixtures/checks_mixed.json"),
        )
        .unwrap();
        std::fs::write(
            p.join("log.txt"),
            include_str!("../../git_svc/tests/fixtures/failed_log.txt"),
        )
        .unwrap();
        Shim { dir }
    }

    fn client(&self) -> GhClient {
        GhClient::with_program(self.dir.path().join("gh.cmd"))
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }

    fn count(&self, needle: &str) -> usize {
        self.calls().lines().filter(|l| l.contains(needle)).count()
    }
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

/// A visible model over a temp repo, with the fake `gh` enabled.
fn model_with_gh(cx: &mut TestAppContext, dir: &Path, shim: &Shim) -> Entity<GitModel> {
    let model = cx.new(|_| GitModel::new());
    let (d, client) = (dir.to_path_buf(), shim.client());
    model.update(cx, |m, cx| {
        m.set_pr_client(Some(client), cx);
        m.set_root(Some(d), cx);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    model
}

fn queries(cx: &TestAppContext, model: &Entity<GitModel>) -> u32 {
    model.read_with(cx, |m, _| m.pr_queries())
}

#[gpui::test]
async fn the_integration_is_off_until_enabled(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = cx.new(|_| GitModel::new());
    let d = dir.path().to_path_buf();
    model.update(cx, |m, cx| {
        m.set_root(Some(d), cx);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.pr_queries(),
            0,
            "no gh process unless the host enables it"
        );
        assert_eq!(m.pr_view(), &PrView::Idle);
        assert!(m.pr_summary().is_none());
    });
}

#[gpui::test]
async fn first_read_queries_once_and_the_summary_is_exposed(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    assert_eq!(queries(cx, &model), 1);
    assert_eq!(shim.count("auth status"), 1);
    assert_eq!(shim.count("pr list"), 1);
    assert_eq!(shim.count("pr checks 12"), 1);
    let summary = model.read_with(cx, |m, _| m.pr_summary()).unwrap();
    assert_eq!(summary.text, "PR #12 · 2 checks fallan");
    assert_eq!(summary.tone, PrTone::Bad);
    assert_eq!(summary.url, "https://github.com/acme/selachi/pull/12");
    let snap = model
        .read_with(cx, |m, _| m.pr_snapshot().cloned())
        .unwrap();
    assert_eq!(snap.checks.len(), 7);
    assert_eq!(snap.branch, "main");
}

#[gpui::test]
async fn no_timer_ever_polls(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    assert_eq!(queries(cx, &model), 1);
    // Time passes, files change, the panel is re-touched: still one query.
    cx.executor().advance_clock(Duration::from_secs(3 * 3600));
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    model.update(cx, |m, cx| m.refresh(true, cx));
    cx.executor().advance_clock(Duration::from_secs(3 * 3600));
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 1, "no polling loop");
    assert_eq!(shim.count("pr list"), 1);
    model.read_with(cx, |m, _| assert!(!m.pr_loading()));
}

#[gpui::test]
async fn a_new_branch_queries_again_and_drops_the_old_pr(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    git(dir.path(), &["checkout", "-q", "-b", "feat/new"]);
    model.update(cx, |m, cx| m.refresh(true, cx));
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 2);
    // The shim's PR belongs to `main`: the new branch has none (and the old PR is gone).
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.pr_view(),
            &PrView::NoPr {
                branch: "feat/new".into()
            }
        );
        assert!(m.pr_summary().is_none());
    });
    // A read where the branch did not change asks nothing.
    model.update(cx, |m, cx| m.refresh(true, cx));
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 2);
}

#[gpui::test]
async fn a_finished_push_queries_again(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    let url = remote.path().to_string_lossy().into_owned();
    git(dir.path(), &["remote", "add", "origin", &url]);
    git(dir.path(), &["push", "-q", "-u", "origin", "main"]);
    std::fs::write(dir.path().join("b.txt"), "x\n").unwrap();
    git(dir.path(), &["add", "b.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "second"]);

    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    assert_eq!(queries(cx, &model), 1);

    model.update(cx, |m, cx| m.request(GitOp::Action(GitAction::Push), cx));
    cx.run_until_parked();
    assert!(matches!(
        model.read_with(cx, |m, _| m.gate().clone()),
        Gate::Confirm { .. }
    ));
    assert_eq!(queries(cx, &model), 1, "asking is not pushing");
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert!(matches!(
        model.read_with(cx, |m, _| m.gate().clone()),
        Gate::Done { .. }
    ));
    assert_eq!(
        queries(cx, &model),
        2,
        "the push finished: PR and CI may have changed"
    );
}

#[gpui::test]
async fn explicit_refresh_and_stale_focus_only(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    assert_eq!(queries(cx, &model), 1);

    // Focus right after a query: nothing.
    let now = Instant::now();
    model.update(cx, |m, cx| m.on_window_focus_at(now, cx));
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 1);
    model.update(cx, |m, cx| {
        m.on_window_focus_at(now + FOCUS_STALE - Duration::from_secs(1), cx)
    });
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 1, "under five minutes");

    // Focus after more than five minutes: one query.
    model.update(cx, |m, cx| {
        m.on_window_focus_at(now + FOCUS_STALE + Duration::from_secs(5), cx)
    });
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 2);

    // The button always asks (and re-probes gh).
    model.update(cx, |m, cx| m.refresh_pr_now(cx));
    cx.run_until_parked();
    assert_eq!(queries(cx, &model), 3);
    assert_eq!(
        shim.count("auth status"),
        2,
        "first query and the manual one"
    );
}

#[gpui::test]
async fn queries_are_single_flight(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    model.update(cx, |m, cx| {
        m.refresh_pr_now(cx);
        m.refresh_pr_now(cx);
        m.refresh_pr_now(cx);
    });
    cx.run_until_parked();
    // One in flight and one coalesced from the other two.
    assert_eq!(queries(cx, &model), 3);
}

#[gpui::test]
async fn unauthenticated_gh_hides_everything(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    std::fs::write(shim.dir.path().join("unauth"), "").unwrap();
    let model = model_with_gh(cx, dir.path(), &shim);
    model.read_with(cx, |m, _| {
        assert_eq!(m.pr_view(), &PrView::Unavailable);
        assert!(!m.pr_view().is_visible());
        assert!(m.pr_summary().is_none());
    });
    assert_eq!(
        shim.count("pr list"),
        0,
        "nothing else is asked without auth"
    );
    // Logging in and pressing refresh brings it back.
    std::fs::remove_file(shim.dir.path().join("unauth")).unwrap();
    model.update(cx, |m, cx| m.refresh_pr_now(cx));
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| m.pr_summary()).is_some());
}

#[gpui::test]
async fn failing_checks_go_to_the_agent_with_redacted_logs(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    let seen = events(cx, &model);
    model.update(cx, |m, cx| m.send_failed_checks_to_agent(cx));
    cx.run_until_parked();
    let ev = seen.borrow();
    let Some(GitPanelEvent::AskAgent { context_markdown }) = ev.last() else {
        panic!("expected AskAgent, got {ev:?}");
    };
    assert!(context_markdown.contains("PR #12"));
    assert!(context_markdown.contains("test (windows-latest)"));
    assert!(context_markdown.contains("unresolved import"));
    assert!(
        !context_markdown.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"),
        "secrets in logs never leave"
    );
    assert!(context_markdown.contains("oculto"));
    // Two failing checks with a job link: both logs were asked, by job.
    assert_eq!(shim.count("run view 777 --job 888 --log-failed"), 1);
    assert_eq!(shim.count("run view 777 --job 889 --log-failed"), 1);
    drop(ev);
    model.read_with(cx, |m, _| assert!(!m.is_sending_failed_checks()));
}

#[gpui::test]
async fn pr_creation_waits_for_the_confirmation_card(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    // The branch has no PR (the shim's belongs to `main`).
    git(dir.path(), &["checkout", "-q", "-b", "feat/x"]);
    let model = model_with_gh(cx, dir.path(), &shim);
    let created = Rc::new(RefCell::new(Vec::new()));
    let sink = created.clone();
    let sub = cx.update(|cx| {
        cx.subscribe(&model, move |_, ev: &PrCreated, _| {
            sink.borrow_mut().push(ev.url.clone())
        })
    });
    std::mem::forget(sub);

    let pr = git_svc::NewPr {
        title: "feat: algo".into(),
        body: "Linea uno\nLinea dos".into(),
        draft: true,
        base: None,
        head: None,
    };
    model.update(cx, |m, cx| m.request_create_pr(pr.clone(), cx));
    cx.run_until_parked();
    // The card shows the exact command, with the branch filled in. Nothing ran.
    let PrCreate::Confirm(shown) = model.read_with(cx, |m, _| m.pr_create().clone()) else {
        panic!("expected the confirmation card");
    };
    assert_eq!(
        shown.argv(),
        [
            "gh",
            "pr",
            "create",
            "--title",
            "feat: algo",
            "--body-file",
            "-",
            "--draft",
            "--head",
            "feat/x"
        ]
    );
    assert_eq!(shim.count("pr create"), 0, "no PR before the confirmation");

    // Cancel: still nothing.
    model.update(cx, |m, cx| m.cancel_create_pr(cx));
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.pr_create().clone()),
        PrCreate::Idle
    );
    assert_eq!(shim.count("pr create"), 0);
    // Confirming without a card does nothing either.
    model.update(cx, |m, cx| m.confirm_create_pr(cx));
    cx.run_until_parked();
    assert_eq!(shim.count("pr create"), 0);

    // Confirm for real.
    let before = queries(cx, &model);
    model.update(cx, |m, cx| m.request_create_pr(pr, cx));
    model.update(cx, |m, cx| m.confirm_create_pr(cx));
    cx.run_until_parked();
    assert_eq!(shim.count("pr create"), 1);
    assert!(shim.calls().contains("--head feat/x"));
    let body = std::fs::read_to_string(shim.dir.path().join("stdin.txt")).unwrap();
    assert!(
        body.contains("Linea uno") && body.contains("Linea dos"),
        "{body}"
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.pr_create().clone()),
        PrCreate::Done {
            url: "https://github.com/acme/selachi/pull/7".into()
        }
    );
    assert_eq!(
        created.borrow().as_slice(),
        ["https://github.com/acme/selachi/pull/7"]
    );
    assert_eq!(queries(cx, &model), before + 1, "the new PR is looked up");
}

#[gpui::test]
async fn pr_creation_refuses_read_only_and_bad_input(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let shim = Shim::new();
    let model = model_with_gh(cx, dir.path(), &shim);
    model.update(cx, |m, cx| m.set_read_only(true, cx));
    let pr = git_svc::NewPr {
        title: "t".into(),
        ..Default::default()
    };
    model.update(cx, |m, cx| m.request_create_pr(pr.clone(), cx));
    assert_eq!(
        model.read_with(cx, |m, _| m.pr_create().clone()),
        PrCreate::Idle
    );
    assert!(model.read_with(cx, |m, _| m.quiet_error().is_some()));
    model.update(cx, |m, cx| {
        m.set_read_only(false, cx);
        m.dismiss(cx);
        m.request_create_pr(git_svc::NewPr::default(), cx);
    });
    assert_eq!(
        model.read_with(cx, |m, _| m.pr_create().clone()),
        PrCreate::Idle
    );
    assert!(
        model.read_with(cx, |m, _| m.quiet_error().is_some()),
        "empty title"
    );
    cx.run_until_parked();
    assert_eq!(shim.count("pr create"), 0);
}

// ---- "Redactar con agente" -----------------------------------------------------------------------

fn stage_secret_change(dir: &Path) {
    std::fs::write(
        dir.join("a.txt"),
        "one\ntoken = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD\"\nthree\n",
    )
    .unwrap();
    git(dir, &["add", "a.txt"]);
}

#[gpui::test]
async fn drafting_emits_a_redacted_diff_and_waits_for_the_host(cx: &mut TestAppContext) {
    let dir = temp_repo();
    stage_secret_change(dir.path());
    let model = cx.new(|_| GitModel::new());
    let d = dir.path().to_path_buf();
    model.update(cx, |m, cx| {
        m.set_root(Some(d), cx);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    let seen = events(cx, &model);
    assert!(seen.borrow().is_empty(), "nothing is drafted on its own");

    let ready = Rc::new(RefCell::new(Vec::new()));
    let sink = ready.clone();
    let sub = cx.update(|cx| {
        cx.subscribe(&model, move |_, ev: &DraftReady, _| {
            sink.borrow_mut().push(ev.clone())
        })
    });
    std::mem::forget(sub);

    model.update(cx, |m, cx| m.request_draft(DraftKind::Commit, cx));
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.drafting()),
        Some(DraftKind::Commit)
    );
    let ev = seen.borrow().last().cloned().unwrap();
    let GitPanelEvent::DraftMessage {
        kind,
        diff_stat,
        staged_diff,
        recent_subjects,
    } = &ev
    else {
        panic!("expected DraftMessage, got {ev:?}");
    };
    assert_eq!(*kind, DraftKind::Commit);
    assert!(diff_stat.contains("a.txt"));
    assert!(staged_diff.contains("+token"));
    assert!(!staged_diff.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"));
    assert_eq!(recent_subjects, &["init"]);
    let prompt = ev.draft_prompt().unwrap();
    assert!(prompt.contains("commit"));
    assert!(!prompt.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"));
    assert!(
        GitPanelEvent::OpenDiff {
            path: PathBuf::new()
        }
        .draft_prompt()
        .is_none()
    );

    // A second click while one is pending does not emit again.
    let n = seen.borrow().len();
    model.update(cx, |m, cx| m.request_draft(DraftKind::Commit, cx));
    cx.run_until_parked();
    assert_eq!(seen.borrow().len(), n);

    // The host answers; the text is editable (the panel's fields take it).
    model.update(cx, |m, cx| {
        m.set_draft(DraftKind::Commit, "fix: x\n\ncuerpo", cx)
    });
    assert_eq!(model.read_with(cx, |m, _| m.drafting()), None);
    assert_eq!(ready.borrow().len(), 1);
    assert_eq!(ready.borrow()[0].text, "fix: x\n\ncuerpo");
    assert_eq!(
        model
            .update(cx, |m, _| m.take_draft(DraftKind::Commit))
            .as_deref(),
        Some("fix: x\n\ncuerpo")
    );
    assert_eq!(
        model.update(cx, |m, _| m.take_draft(DraftKind::Commit)),
        None
    );
}

#[gpui::test]
async fn cancelled_or_failed_drafts_do_not_overwrite_anything(cx: &mut TestAppContext) {
    let dir = temp_repo();
    stage_secret_change(dir.path());
    let model = cx.new(|_| GitModel::new());
    let d = dir.path().to_path_buf();
    model.update(cx, |m, cx| {
        m.set_root(Some(d), cx);
        m.set_visible(true, cx);
        m.request_draft(DraftKind::Commit, cx);
    });
    cx.run_until_parked();
    model.update(cx, |m, cx| m.cancel_draft(cx));
    // A late answer after the user cancelled is dropped.
    model.update(cx, |m, cx| m.set_draft(DraftKind::Commit, "late", cx));
    assert_eq!(
        model.update(cx, |m, _| m.take_draft(DraftKind::Commit)),
        None
    );

    // A failure clears the spinner and tells the user.
    model.update(cx, |m, cx| m.request_draft(DraftKind::Commit, cx));
    cx.run_until_parked();
    model.update(cx, |m, cx| {
        m.fail_draft(DraftKind::Commit, "el agente no respondió", cx)
    });
    model.read_with(cx, |m, _| {
        assert_eq!(m.drafting(), None);
        assert_eq!(m.quiet_error(), Some("el agente no respondió"));
    });
}

#[gpui::test]
async fn nothing_to_describe_says_so_without_asking_the_agent(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = cx.new(|_| GitModel::new());
    let d = dir.path().to_path_buf();
    model.update(cx, |m, cx| {
        m.set_root(Some(d), cx);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    let seen = events(cx, &model);
    model.update(cx, |m, cx| m.request_draft(DraftKind::Commit, cx));
    cx.run_until_parked();
    assert!(seen.borrow().is_empty(), "no staged diff, no tokens");
    model.read_with(cx, |m, _| {
        assert_eq!(m.drafting(), None);
        assert!(m.quiet_error().unwrap().contains("staged"));
    });
}

// ---- the panel ------------------------------------------------------------------------------------

fn ctx(class: WidthClass, dir: &Path) -> PanelCtx {
    PanelCtx {
        zone: PanelZone::Right,
        width_class: class,
        cwd: Some(dir.to_path_buf()),
        project_root: Some(dir.to_path_buf()),
        theme: Theme::dark(),
    }
}

#[gpui::test]
async fn the_panel_paints_the_pr_section_and_takes_the_draft(cx: &mut TestAppContext) {
    let dir = temp_repo();
    stage_secret_change(dir.path());
    let shim = Shim::new();
    let model = cx.new(|_| GitModel::new());
    let (m, c, client) = (
        model.clone(),
        ctx(WidthClass::Regular, dir.path()),
        shim.client(),
    );
    let (panel, vcx) = ui_kit::testing::open_window(cx, move |_, cx| {
        m.update(cx, |m, cx| {
            m.set_pr_client(Some(client), cx);
            m.set_root(c.cwd.clone(), cx);
        });
        GitPanel::new(c, m, cx)
    });
    vcx.run_until_parked();
    assert!(model.read_with(vcx, |m, _| m.pr_summary()).is_some());
    // Every layout and tab paints with PR data (including the PR tab).
    for class in [WidthClass::Compact, WidthClass::Regular] {
        panel.update(vcx, |p, cx| p.retarget(ctx(class, dir.path()), cx));
        for tab in Tab::ALL {
            panel.update(vcx, |p, cx| p.select_tab(tab, cx));
            vcx.run_until_parked();
        }
    }

    // The draft fills the commit box, editable afterwards.
    model.update(vcx, |m, cx| m.request_draft(DraftKind::Commit, cx));
    vcx.run_until_parked();
    model.update(vcx, |m, cx| {
        m.set_draft(
            DraftKind::Commit,
            "feat(ci): muestra el estado\n\nExplica por qué.",
            cx,
        )
    });
    vcx.run_until_parked();
    assert_eq!(
        panel.read_with(vcx, |p, cx| p.message(cx)),
        "feat(ci): muestra el estado\n\nExplica por qué."
    );
    panel.update(vcx, |p, cx| p.set_message("editado", "", cx));
    assert_eq!(panel.read_with(vcx, |p, cx| p.message(cx)), "editado");
}

#[gpui::test]
async fn the_pr_tab_is_only_listed_with_a_pr_integration(cx: &mut TestAppContext) {
    let dir = temp_repo();
    let model = cx.new(|_| GitModel::new());
    let (m, c) = (model.clone(), ctx(WidthClass::Compact, dir.path()));
    let (panel, vcx) = ui_kit::testing::open_window(cx, move |_, cx| {
        m.update(cx, |m, cx| m.set_root(c.cwd.clone(), cx));
        GitPanel::new(c, m, cx)
    });
    vcx.run_until_parked();
    // Without gh the PR tab is not offered and selecting it falls back to Cambios: no panic.
    panel.update(vcx, |p, cx| p.select_tab(Tab::Pr, cx));
    vcx.run_until_parked();
    assert!(model.read_with(vcx, |m, _| !m.pr_view().is_visible()));
}
