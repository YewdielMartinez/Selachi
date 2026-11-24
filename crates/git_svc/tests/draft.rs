//! `Repo::draft_input` over a real temporary repository: what goes to the agent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use git_svc::{draft_prompt, DraftKind, Repo};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo() -> (tempfile::TempDir, Repo) {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q", "-b", "main"]);
    fs::write(td.path().join("a.txt"), "uno\n").unwrap();
    git(td.path(), &["add", "."]);
    git(
        td.path(),
        &["commit", "-q", "-m", "feat: añade el archivo inicial"],
    );
    let repo = Repo::discover(td.path()).unwrap().unwrap();
    (td, repo)
}

#[test]
fn commit_input_is_the_staged_diff_redacted() {
    let (td, repo) = repo();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    fs::write(
        td.path().join("a.txt"),
        format!("uno\ntoken = \"{secret}\"\n"),
    )
    .unwrap();
    fs::write(td.path().join("unstaged.txt"), "no debe aparecer\n").unwrap();
    git(td.path(), &["add", "a.txt"]);
    let input = repo.draft_input(DraftKind::Commit).unwrap();
    assert!(!input.is_empty());
    assert!(input.diff.contains("+token"));
    assert!(!input.diff.contains(secret), "secrets never leave");
    assert!(input.diff.contains("oculto"));
    assert!(input.diff_stat.contains("a.txt"));
    assert!(!input.diff.contains("unstaged"));
    assert_eq!(input.recent_subjects, ["feat: añade el archivo inicial"]);
    let prompt = draft_prompt(
        DraftKind::Commit,
        &input.diff_stat,
        &input.diff,
        &input.recent_subjects,
    );
    assert!(prompt.contains("en español"));
    assert!(!prompt.contains(secret));
}

#[test]
fn nothing_staged_is_empty() {
    let (_td, repo) = repo();
    assert!(repo.draft_input(DraftKind::Commit).unwrap().is_empty());
}

#[test]
fn pr_input_is_the_branch_against_its_base() {
    let (td, repo) = repo();
    git(td.path(), &["checkout", "-q", "-b", "feat/x"]);
    fs::write(td.path().join("b.txt"), "nuevo en la rama\n").unwrap();
    git(td.path(), &["add", "."]);
    git(td.path(), &["commit", "-q", "-m", "feat: b"]);
    assert_eq!(repo.base_ref().as_deref(), Some("main"));
    let input = repo.draft_input(DraftKind::PullRequest).unwrap();
    assert!(input.diff.contains("nuevo en la rama"));
    assert!(input.diff_stat.contains("b.txt"));
    assert!(
        !input.diff.contains("a.txt"),
        "only the branch's own changes"
    );
}
