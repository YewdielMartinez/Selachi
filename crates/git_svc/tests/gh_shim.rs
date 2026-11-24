//! `GhClient` against a fake `gh` (a `.cmd` shim): argv, stdin, exit codes, graceful failures.
//! The real `gh` is never run and no repository is touched.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use git_svc::{Check, CheckBucket, GhClient, NewPr, PrState, Repo};
use tempfile::TempDir;

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
if exist "%~dp0create_fails" (
  echo GraphQL: head branch not pushed 1>&2
  exit /b 1
)
findstr "^" > "%~dp0stdin.txt"
echo https://github.com/acme/selachi/pull/7
exit /b 0
:runview
type "%~dp0log.txt"
exit /b 0
"#;

struct Fixture {
    dir: TempDir,
    repo_dir: TempDir,
    gh: GhClient,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("gh.cmd"), SHIM).unwrap();
    fs::write(
        dir.path().join("pr_list.json"),
        include_str!("fixtures/pr_list_mixed.json"),
    )
    .unwrap();
    fs::write(
        dir.path().join("checks.json"),
        include_str!("fixtures/checks_mixed.json"),
    )
    .unwrap();
    fs::write(
        dir.path().join("log.txt"),
        include_str!("fixtures/failed_log.txt"),
    )
    .unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let ok = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo_dir.path())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let gh = GhClient::with_program(dir.path().join("gh.cmd"));
    Fixture { dir, repo_dir, gh }
}

impl Fixture {
    fn repo(&self) -> Repo {
        Repo::discover(self.repo_dir.path()).unwrap().unwrap()
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }
    fn touch(&self, name: &str) {
        fs::write(self.dir.path().join(name), "").unwrap();
    }
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn availability_is_the_auth_exit_code() {
    let f = fixture();
    assert!(f.gh.available());
    f.touch("unauth");
    assert!(!f.gh.available(), "not logged in: hidden");
    // No gh at all.
    let missing = GhClient::with_program("selachi-no-such-gh-binary");
    assert!(!missing.available());
    assert!(missing.pr_for_branch(&f.repo(), "main").is_none());
    assert!(missing.checks(&f.repo(), 1).is_none());
    assert_eq!(
        missing.failed_check_log(
            &f.repo(),
            &Check {
                name: "x".into(),
                workflow: String::new(),
                bucket: CheckBucket::Fail,
                status: git_svc::CheckStatus::Completed,
                conclusion: None,
                url: "https://github.com/o/r/actions/runs/1/job/2".into(),
                started: None,
                completed: None,
            }
        ),
        ""
    );
    // The auth probe never asks for a token.
    assert!(!f.calls().contains("token"));
}

#[test]
fn pr_for_branch_runs_the_expected_query() {
    let f = fixture();
    let pr = f.gh.pr_for_branch(&f.repo(), "feat/ci").unwrap();
    assert_eq!((pr.number, pr.state), (12, PrState::Open));
    let calls = f.calls();
    assert!(
        calls.contains("pr list --head feat/ci --state all"),
        "{calls}"
    );
    assert!(calls.contains("--json") && calls.contains("number,title,url,state,isDraft"));
    assert!(f.gh.pr_for_branch(&f.repo(), "nobody").is_none());
    // An option-looking branch never reaches gh.
    let before = f.calls();
    assert!(f.gh.pr_for_branch(&f.repo(), "--web").is_none());
    assert_eq!(f.calls(), before);
}

#[test]
fn checks_parse_even_though_gh_exits_non_zero() {
    let f = fixture();
    let checks = f.gh.checks(&f.repo(), 12).unwrap();
    assert_eq!(checks.len(), 7);
    assert_eq!(checks.iter().filter(|c| c.is_failing()).count(), 2);
    assert!(f.calls().contains("pr checks 12 --json"));
}

#[test]
fn failed_log_is_requested_by_job_and_redacted() {
    let f = fixture();
    let checks = f.gh.checks(&f.repo(), 12).unwrap();
    let failing = checks.iter().find(|c| c.is_failing()).unwrap();
    let log = f.gh.failed_check_log(&f.repo(), failing);
    assert!(log.contains("unresolved import"));
    assert!(!log.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"));
    assert!(f.calls().contains("run view 777 --job 888 --log-failed"));
    // A check without a run link has no log (and runs nothing).
    let before = f.calls();
    let no_link = checks.iter().find(|c| c.url.is_empty()).unwrap();
    assert_eq!(f.gh.failed_check_log(&f.repo(), no_link), "");
    assert_eq!(f.calls(), before);
}

#[test]
fn create_pr_sends_the_body_by_stdin_and_returns_the_url() {
    let f = fixture();
    let pr = NewPr {
        title: "feat: estado de PR".into(),
        body: "Linea uno\nLinea dos".into(),
        draft: true,
        base: None,
        head: Some("feat/ci".into()),
    };
    let url = f.gh.create_pr(&f.repo(), &pr).unwrap();
    assert_eq!(url, "https://github.com/acme/selachi/pull/7");
    let calls = f.calls();
    assert!(
        calls.contains(
            "pr create --title \"feat: estado de PR\" --body-file - --draft --head feat/ci"
        ),
        "{calls}"
    );
    let stdin = fs::read_to_string(f.dir.path().join("stdin.txt")).unwrap();
    assert!(
        stdin.contains("Linea uno") && stdin.contains("Linea dos"),
        "{stdin}"
    );

    // gh's error text is surfaced.
    f.touch("create_fails");
    let err = f.gh.create_pr(&f.repo(), &pr).unwrap_err();
    assert!(err.contains("head branch not pushed"), "{err}");

    // Invalid input never reaches gh.
    let before = f.calls();
    assert!(f.gh.create_pr(&f.repo(), &NewPr::default()).is_err());
    assert_eq!(f.calls(), before);
    assert!(exists(&f.dir.path().join("calls.log")));
}
