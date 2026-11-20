#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use futures::channel::mpsc::TryRecvError;
use git_svc::{ChangeKind, GitAction, Repo};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "core.autocrlf", "false"]);
}

fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
    fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn new_repo() -> (TempDir, Repo) {
    let td = tempfile::tempdir().unwrap();
    init_repo(td.path());
    let repo = Repo::discover(td.path()).unwrap().expect("repo");
    (td, repo)
}

#[test]
fn discover_none_outside_repo_and_some_inside_subdir() {
    let td = tempfile::tempdir().unwrap();
    // A fresh temp dir is (almost certainly) not inside a repo.
    if Repo::discover(td.path()).unwrap().is_some() {
        eprintln!("temp dir is inside a git repo on this machine; skipping negative check");
    }
    init_repo(td.path());
    fs::create_dir_all(td.path().join("a/b")).unwrap();
    let r = Repo::discover(td.path().join("a/b")).unwrap().unwrap();
    assert!(r.git_dir.ends_with(".git"));
    assert!(r.root.join(".git").exists());
    // Works from a file path too.
    fs::write(td.path().join("a/b/f.txt"), "x").unwrap();
    let r2 = Repo::discover(td.path().join("a/b/f.txt")).unwrap().unwrap();
    assert_eq!(r, r2);
    // Non-existent path: no repo, no error.
    assert!(Repo::discover(td.path().join("missing/dir")).unwrap().is_none());
}

#[test]
fn status_counts_and_kinds() {
    let (td, repo) = new_repo();
    let d = td.path();

    // Unborn branch with an untracked file.
    fs::write(d.join("first.txt"), "1").unwrap();
    let st = repo.status().unwrap();
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.head_oid, None);
    assert_eq!(st.untracked.len(), 1);
    assert!(repo.recent_commits(5).unwrap().is_empty());
    assert!(repo.diff_stat().unwrap().is_empty());

    git(d, &["add", "first.txt"]);
    git(d, &["commit", "-q", "-m", "first commit"]);
    commit_file(d, "tracked.txt", "line\n", "add tracked");
    commit_file(d, "old name.txt", "some content here\nmore\n", "add old name");

    // staged: modification, new file, rename; unstaged: modification; untracked; spaces + unicode.
    fs::write(d.join("tracked.txt"), "line\nstaged edit\n").unwrap();
    git(d, &["add", "tracked.txt"]);
    fs::write(d.join("staged_new.txt"), "n").unwrap();
    git(d, &["add", "staged_new.txt"]);
    git(d, &["mv", "old name.txt", "new name.txt"]);
    fs::write(d.join("first.txt"), "1\nunstaged edit\n").unwrap();
    fs::write(d.join("ñandú 日本.txt"), "u").unwrap();

    let st = repo.status().unwrap();
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert!(!st.detached);
    assert_eq!(st.upstream, None);
    assert_eq!((st.ahead, st.behind), (0, 0));
    assert_eq!(st.staged.len(), 3, "{:?}", st.staged);
    assert_eq!(st.unstaged.len(), 1);
    assert_eq!(st.unstaged[0].kind, ChangeKind::Modified);
    assert_eq!(st.untracked, vec![Path::new("ñandú 日本.txt").to_path_buf()]);
    assert!(st.conflicted.is_empty());
    let rename = st
        .staged
        .iter()
        .find(|c| c.kind == ChangeKind::Renamed)
        .expect("rename");
    assert_eq!(rename.path, Path::new("new name.txt"));
    assert_eq!(rename.orig_path.as_deref(), Some(Path::new("old name.txt")));
    assert!(st.staged.iter().any(|c| c.kind == ChangeKind::Added));

    // Diffs.
    let unstaged = repo.diff(Some(Path::new("first.txt")), false).unwrap();
    assert!(unstaged.contains("+unstaged edit"));
    let staged = repo.diff(Some(Path::new("tracked.txt")), true).unwrap();
    assert!(staged.contains("+staged edit"));
    assert!(repo.diff(None, true).unwrap().contains("staged_new.txt"));
    assert!(repo.diff_stat().unwrap().contains("tracked.txt"));

    // Commits.
    let commits = repo.recent_commits(10).unwrap();
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].subject, "add old name");
    assert_eq!(commits[0].author, "Test User");
    assert_eq!(commits[0].hash.len(), 40);
    assert!(commits[0].timestamp > 1_600_000_000);
    assert_eq!(repo.recent_commits(1).unwrap().len(), 1);
}

#[test]
fn detached_head_and_conflicts() {
    let (td, repo) = new_repo();
    let d = td.path();
    commit_file(d, "f.txt", "base\n", "base");
    git(d, &["switch", "-q", "-c", "other"]);
    commit_file(d, "f.txt", "other\n", "other change");
    git(d, &["switch", "-q", "main"]);
    commit_file(d, "f.txt", "main\n", "main change");
    let merge = Command::new("git").args(["merge", "other"]).current_dir(d).output().unwrap();
    assert!(!merge.status.success());

    let st = repo.status().unwrap();
    assert_eq!(st.conflicted, vec![Path::new("f.txt").to_path_buf()]);
    let pf = repo
        .preflight(&GitAction::Commit { message: "x".into() })
        .unwrap();
    assert!(pf.warnings.iter().any(|w| w.contains("conflict")));

    git(d, &["merge", "--abort"]);
    git(d, &["checkout", "-q", "--detach", "HEAD"]);
    let st = repo.status().unwrap();
    assert!(st.detached);
    assert_eq!(st.branch, None);
}

#[test]
fn ahead_behind_against_local_bare_remote() {
    let (td, repo) = new_repo();
    let d = td.path();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(d, &["remote", "add", "origin", remote.path().to_str().unwrap()]);
    commit_file(d, "a.txt", "a\n", "c1");
    git(d, &["push", "-q", "-u", "origin", "main"]);

    let st = repo.status().unwrap();
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.ahead, st.behind), (0, 0));

    // Two local commits -> ahead 2.
    commit_file(d, "b.txt", "b\n", "c2");
    commit_file(d, "c.txt", "c\n", "c3");
    let st = repo.status().unwrap();
    assert_eq!((st.ahead, st.behind), (2, 0));
    let pf = repo.preflight(&GitAction::Push).unwrap();
    assert!(pf.requires_confirmation);
    assert!(pf.diff_stat.as_deref().unwrap_or("").contains("b.txt"));

    // Push through the typed action API.
    let out = repo.run(&GitAction::Push).unwrap();
    assert!(out.success(), "{}", out.stderr);
    assert_eq!((repo.status().unwrap().ahead, repo.status().unwrap().behind), (0, 0));

    // Another clone pushes one commit; fetch -> behind 1.
    let other = tempfile::tempdir().unwrap();
    git(
        other.path(),
        &["clone", "-q", remote.path().to_str().unwrap(), "clone"],
    );
    let clone = other.path().join("clone");
    git(&clone, &["config", "user.name", "Other"]);
    git(&clone, &["config", "user.email", "o@example.com"]);
    git(&clone, &["config", "commit.gpgsign", "false"]);
    commit_file(&clone, "d.txt", "d\n", "from other");
    git(&clone, &["push", "-q", "origin", "main"]);

    let out = repo.run(&GitAction::Fetch).unwrap();
    assert!(out.success(), "{}", out.stderr);
    let st = repo.status().unwrap();
    assert_eq!((st.ahead, st.behind), (0, 1));

    // Diverge: local commit too -> ahead 1 behind 1, push warns, pull warns.
    commit_file(d, "e.txt", "e\n", "local diverge");
    let st = repo.status().unwrap();
    assert_eq!((st.ahead, st.behind), (1, 1));
    let pf = repo.preflight(&GitAction::Push).unwrap();
    assert!(pf.warnings.iter().any(|w| w.contains("behind")));
    let pf = repo.preflight(&GitAction::Pull).unwrap();
    assert!(pf.warnings.iter().any(|w| w.contains("diverged")));
}

#[test]
fn actions_preflight_and_run() {
    let (td, repo) = new_repo();
    let d = td.path();
    commit_file(d, "a.txt", "a\n", "c1");
    git(d, &["branch", "feature"]);

    // Clean tree: switching needs no confirmation.
    let sw = GitAction::Switch { branch: "feature".into() };
    let pf = repo.preflight(&sw).unwrap();
    assert!(!pf.requires_confirmation);
    assert!(sw.is_sensitive());

    // Dirty tree: confirmation + diff stat.
    fs::write(d.join("a.txt"), "a\nchanged\n").unwrap();
    let pf = repo.preflight(&sw).unwrap();
    assert!(pf.requires_confirmation);
    assert!(pf.diff_stat.as_deref().unwrap_or("").contains("a.txt"));

    // Commit with nothing staged warns; after AddAll it does not.
    let commit = GitAction::Commit { message: "second".into() };
    let pf = repo.preflight(&commit).unwrap();
    assert!(pf.warnings.iter().any(|w| w.contains("staged")));
    assert!(repo.run(&GitAction::AddAll).unwrap().success());
    let pf = repo.preflight(&commit).unwrap();
    assert!(pf.warnings.is_empty(), "{:?}", pf.warnings);
    assert!(pf.diff_stat.as_deref().unwrap_or("").contains("a.txt"));
    let out = repo.run(&commit).unwrap();
    assert!(out.success(), "{}{}", out.stdout, out.stderr);
    assert_eq!(repo.recent_commits(1).unwrap()[0].subject, "second");

    // Switch + status/log/diff actions.
    assert!(repo.run(&sw).unwrap().success());
    assert_eq!(repo.status().unwrap().branch.as_deref(), Some("feature"));
    assert!(repo.run(&GitAction::Status).unwrap().stdout.contains("feature"));
    assert!(repo.run(&GitAction::Log).unwrap().stdout.contains("c1"));
    assert!(repo.run(&GitAction::Diff).unwrap().success());

    // Failure is reported via the output, not an error.
    let bad = repo.run(&GitAction::Checkout { target: "no-such-branch".into() }).unwrap();
    assert!(!bad.success());
    assert!(!bad.stderr.is_empty());

    // Option-looking targets are rejected before reaching git.
    assert!(repo
        .run(&GitAction::Switch { branch: "--orphan".into() })
        .is_err());
}

fn drain(rx: &mut futures::channel::mpsc::UnboundedReceiver<()>) -> usize {
    let mut n = 0;
    while let Ok(()) = rx.try_recv() {
        n += 1;
    }
    n
}

fn wait_event(rx: &mut futures::channel::mpsc::UnboundedReceiver<()>, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(()) => return true,
            Err(TryRecvError::Closed) => return false,
            Err(TryRecvError::Empty) => sleep(Duration::from_millis(20)),
        }
    }
    false
}

#[test]
fn watcher_follows_directories_created_after_start() {
    let (td, repo) = new_repo();
    let d = td.path();
    commit_file(d, "a.txt", "a
", "c1");
    fs::create_dir_all(d.join("existing")).unwrap();

    let (_handle, mut rx) = repo.watch(Duration::from_millis(100)).unwrap();
    sleep(Duration::from_millis(500));
    drain(&mut rx);

    // A file inside a top-level directory that existed at start.
    fs::write(d.join("existing/x.txt"), "x").unwrap();
    assert!(wait_event(&mut rx, Duration::from_secs(5)), "no event for existing dir");

    // A new top-level directory: its creation fires, and so do later edits inside it.
    sleep(Duration::from_millis(400));
    drain(&mut rx);
    fs::create_dir_all(d.join("fresh")).unwrap();
    assert!(wait_event(&mut rx, Duration::from_secs(5)), "no event for new dir");
    sleep(Duration::from_millis(600));
    drain(&mut rx);
    fs::write(d.join("fresh/y.txt"), "y").unwrap();
    assert!(wait_event(&mut rx, Duration::from_secs(5)), "new dir was not watched");
}

#[test]
fn watcher_fires_after_git_add_and_stops_on_drop() {
    let (td, repo) = new_repo();
    let d = td.path();
    commit_file(d, "a.txt", "a\n", "c1");
    fs::write(d.join("b.txt"), "b\n").unwrap();

    let (handle, mut rx) = repo.watch(Duration::from_millis(100)).unwrap();
    // Let the watcher settle, then drop any events from setup.
    sleep(Duration::from_millis(500));
    drain(&mut rx);

    // `git add` only touches .git/index (the worktree is unchanged).
    git(d, &["add", "b.txt"]);
    assert!(
        wait_event(&mut rx, Duration::from_secs(5)),
        "no event after git add"
    );

    // Worktree edit also fires.
    sleep(Duration::from_millis(400));
    drain(&mut rx);
    fs::write(d.join("a.txt"), "changed\n").unwrap();
    assert!(wait_event(&mut rx, Duration::from_secs(5)), "no event after edit");

    // A burst coalesces into few events.
    sleep(Duration::from_millis(400));
    drain(&mut rx);
    for i in 0..20 {
        fs::write(d.join(format!("burst{i}.txt")), "x").unwrap();
    }
    assert!(wait_event(&mut rx, Duration::from_secs(5)));
    sleep(Duration::from_millis(500));
    assert!(drain(&mut rx) <= 2, "burst was not debounced");

    // target/ noise is ignored.
    sleep(Duration::from_millis(400));
    drain(&mut rx);
    fs::create_dir_all(d.join("target/debug")).unwrap();
    fs::write(d.join("target/debug/x.o"), "x").unwrap();
    sleep(Duration::from_millis(800));
    assert_eq!(drain(&mut rx), 0, "target/ should be ignored");

    // Dropping the handle closes the channel.
    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut closed = false;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Err(TryRecvError::Closed) => {
                closed = true;
                break;
            }
            _ => sleep(Duration::from_millis(20)),
        }
    }
    assert!(closed, "channel not closed after dropping handle");
}
