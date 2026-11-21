#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Worktrees (A8) and checkpoints (A10) against real git in temp repos.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use git_svc::{DeltaKind, Editor, NewWorktree, Repo, RestoreMode, WorktreeLocation};
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
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "core.autocrlf", "false"]);
}

fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
    if let Some(p) = Path::new(name).parent() {
        fs::create_dir_all(dir.join(p)).unwrap();
    }
    fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", msg]);
}

/// Repo at `<tmp>/<name>` so sibling worktrees land inside the temp dir.
fn repo_named(name: &str) -> (TempDir, PathBuf, Repo) {
    let td = tempfile::tempdir().unwrap();
    let root = td.path().join(name);
    init_repo(&root);
    commit_file(&root, "a.txt", "alpha\n", "init");
    let repo = Repo::discover(&root).unwrap().expect("repo");
    (td, root, repo)
}

fn new_repo() -> (TempDir, PathBuf, Repo) {
    repo_named("my repo")
}

fn same(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| {
        fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase().replace('\\', "/")
    };
    n(a) == n(b)
}

/// All files (not dirs) under `root` except `.git`, as relative path -> bytes.
fn dir_map(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_name() == ".git" {
                continue;
            }
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

// ------------------------------------------------------------------ worktrees

#[test]
fn worktree_create_list_remove() {
    let (td, root, repo) = new_repo(); // path contains a space
    let head = git(&root, &["rev-parse", "HEAD"]).trim().to_owned();

    let w = repo.create_worktree(&NewWorktree::new("feat-a")).unwrap();
    let expected = td.path().join("my repo.worktrees").join("feat-a");
    assert!(same(&w.path, &expected), "{:?} vs {:?}", w.path, expected);
    assert_eq!(w.branch.as_deref(), Some("selachi/feat-a"));
    assert_eq!(w.head, head);
    assert!(!w.is_main && !w.locked && !w.prunable);
    assert!(w.path.join("a.txt").is_file());

    let list = repo.worktrees().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].is_main && same(&list[0].path, &root));
    assert_eq!(list[0].branch.as_deref(), Some("main"));
    assert!(list.iter().all(|w| w.dirty.is_none()));
    let list = repo.worktrees_with_dirty().unwrap();
    assert!(list.iter().all(|w| w.dirty == Some(false)));

    // Dirty -> refuse without force.
    fs::write(w.path.join("new.txt"), "x").unwrap();
    assert!(repo.worktree_is_dirty(&w.path).unwrap());
    let pf = repo.preflight_remove_worktree(&w.path).unwrap();
    assert!(pf.requires_confirmation);
    assert!(pf.warnings.iter().any(|m| m.contains("uncommitted")), "{pf:?}");
    let err = repo.remove_worktree(&w.path, false).unwrap_err();
    assert!(err.to_string().contains("uncommitted"), "{err}");
    assert!(w.path.exists());

    repo.remove_worktree(&w.path, true).unwrap();
    assert!(!w.path.exists());
    assert_eq!(repo.worktrees().unwrap().len(), 1);
    // The branch is kept.
    assert!(git(&root, &["branch", "--list", "selachi/feat-a"]).contains("selachi/feat-a"));
}

#[test]
fn worktree_clean_remove_without_force_and_custom_options() {
    let (td, root, repo) = new_repo();
    commit_file(&root, "b.txt", "b\n", "second");
    let first = git(&root, &["rev-parse", "HEAD~1"]).trim().to_owned();

    // Unicode + spaces in name, custom branch, explicit base.
    let w = repo
        .create_worktree(&NewWorktree {
            name: "añadir cosas".into(),
            base: Some(first.clone()),
            branch: Some("topic/ñandú".into()),
            ..NewWorktree::default()
        })
        .unwrap();
    assert_eq!(w.head, first);
    assert_eq!(w.branch.as_deref(), Some("topic/ñandú"));
    assert!(!w.path.join("b.txt").exists());

    // Custom location.
    let custom = td.path().join("elsewhere").join("wt custom");
    let w2 = repo
        .create_worktree(&NewWorktree {
            name: "c".into(),
            location: WorktreeLocation::Custom(custom.clone()),
            ..NewWorktree::default()
        })
        .unwrap();
    assert!(same(&w2.path, &custom));

    // Clean worktrees go away without force.
    repo.remove_worktree(&w.path, false).unwrap();
    repo.remove_worktree(&w2.path, false).unwrap();
    assert_eq!(repo.worktrees().unwrap().len(), 1);
}

#[test]
fn worktree_creation_refusals() {
    let (td, root, repo) = new_repo();
    for bad in ["", "..", "-x", "a/b", "a\\b", "CON", "nul", "com1", "lpt3.txt", "a:b", "a*b", "trail.", "x..y"] {
        let r = repo.create_worktree(&NewWorktree::new(bad));
        assert!(r.is_err(), "{bad:?} should be rejected");
    }
    assert!(repo.worktrees().unwrap().len() == 1);
    // Bad base / branch.
    assert!(repo
        .create_worktree(&NewWorktree { name: "ok".into(), base: Some("--evil".into()), ..Default::default() })
        .is_err());
    assert!(repo
        .create_worktree(&NewWorktree { name: "ok".into(), base: Some("nope-ref".into()), ..Default::default() })
        .is_err());
    for b in ["-b", "a..b", "x y", "bad~1", "con", "a/aux", "end.lock"] {
        assert!(
            repo.create_worktree(&NewWorktree { name: "ok".into(), branch: Some(b.into()), ..Default::default() })
                .is_err(),
            "branch {b:?}"
        );
    }
    // Existing branch.
    assert!(repo
        .create_worktree(&NewWorktree { name: "ok".into(), branch: Some("main".into()), ..Default::default() })
        .is_err());
    // Custom location inside the repo, with `..`, or non-empty.
    assert!(repo
        .create_worktree(&NewWorktree {
            name: "ok".into(),
            location: WorktreeLocation::Custom(root.join("inside")),
            ..Default::default()
        })
        .is_err());
    assert!(repo
        .create_worktree(&NewWorktree {
            name: "ok".into(),
            location: WorktreeLocation::Custom(td.path().join("x").join("..").join("y")),
            ..Default::default()
        })
        .is_err());
    let busy = td.path().join("busy");
    fs::create_dir_all(&busy).unwrap();
    fs::write(busy.join("f"), "1").unwrap();
    assert!(repo
        .create_worktree(&NewWorktree {
            name: "ok".into(),
            location: WorktreeLocation::Custom(busy),
            ..Default::default()
        })
        .is_err());
    // Duplicate name (same sibling dir).
    repo.create_worktree(&NewWorktree::new("dup")).unwrap();
    assert!(repo.create_worktree(&NewWorktree::new("dup")).is_err());
    // Very long path on Windows.
    if cfg!(windows) {
        let long = "x".repeat(80);
        let r = repo.create_worktree(&NewWorktree {
            name: long.clone(),
            location: WorktreeLocation::Custom(td.path().join("y".repeat(150)).join(long)),
            ..Default::default()
        });
        assert!(r.is_err());
    }
    // Nothing leaked: still main + dup.
    assert_eq!(repo.worktrees().unwrap().len(), 2);
}

#[test]
fn worktree_unborn_head_is_an_error() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path().join("empty");
    init_repo(&root);
    let repo = Repo::discover(&root).unwrap().unwrap();
    assert!(repo.create_worktree(&NewWorktree::new("x")).is_err());
}

#[test]
fn worktree_removal_refusals() {
    let (_td, root, repo) = new_repo();
    // Main worktree.
    let e = repo.remove_worktree(&root, true).unwrap_err();
    assert!(e.to_string().contains("main"), "{e}");
    assert!(repo.preflight_remove_worktree(&root).is_err());
    // Not a worktree.
    assert!(repo.remove_worktree(&root.join("nowhere"), false).is_err());

    let w = repo.create_worktree(&NewWorktree::new("lk")).unwrap();
    // Removing the worktree a handle is rooted in.
    let inside = Repo::discover(&w.path).unwrap().unwrap();
    assert!(inside.remove_worktree(&w.path, true).is_err());
    // ...but the main handle can remove it, and the linked handle lists the same worktrees.
    assert_eq!(inside.worktrees().unwrap().len(), 2);
    assert!(inside.worktrees().unwrap()[0].is_main);

    // Locked.
    git(&root, &["worktree", "lock", "--reason", "in use", &w.path.to_string_lossy()]);
    let l = repo.worktrees().unwrap();
    assert!(l[1].locked);
    assert_eq!(l[1].lock_reason.as_deref(), Some("in use"));
    let e = repo.remove_worktree(&w.path, false).unwrap_err();
    assert!(e.to_string().contains("locked"), "{e}");
    assert!(w.path.exists());
    repo.remove_worktree(&w.path, true).unwrap();
    assert!(!w.path.exists());
}

#[test]
fn worktree_preflight_warns_about_unpushed_commits() {
    let (_td, _root, repo) = new_repo();
    let w = repo.create_worktree(&NewWorktree::new("work")).unwrap();
    let pf = repo.preflight_remove_worktree(&w.path).unwrap();
    assert!(pf.warnings.is_empty() && !pf.requires_confirmation, "{pf:?}");

    commit_file(&w.path, "f.txt", "1", "wip commit");
    let pf = repo.preflight_remove_worktree(&w.path).unwrap();
    assert!(pf.requires_confirmation);
    assert!(pf.warnings.iter().any(|m| m.contains("1 commit")), "{pf:?}");
    // Merged into main -> no warning any more.
    git(&repo.root, &["merge", "-q", "--ff-only", "selachi/work"]);
    let pf = repo.preflight_remove_worktree(&w.path).unwrap();
    assert!(pf.warnings.is_empty(), "{pf:?}");
}

#[test]
fn worktree_prune_and_missing_dir() {
    let (_td, _root, repo) = new_repo();
    let a = repo.create_worktree(&NewWorktree::new("a")).unwrap();
    let b = repo.create_worktree(&NewWorktree::new("b")).unwrap();
    fs::remove_dir_all(&a.path).unwrap();
    fs::remove_dir_all(&b.path).unwrap();
    let l = repo.worktrees().unwrap();
    assert_eq!(l.iter().filter(|w| w.prunable).count(), 2);
    let pf = repo.preflight_remove_worktree(&a.path).unwrap();
    assert!(pf.requires_confirmation);
    // `remove_worktree` on a vanished directory prunes the stale metadata.
    repo.remove_worktree(&a.path, false).unwrap();
    let pruned = repo.prune_worktrees().unwrap();
    let _ = pruned;
    assert_eq!(repo.worktrees().unwrap().len(), 1);
    // worktrees_with_dirty copes with nothing to stat.
    assert!(repo.worktrees_with_dirty().unwrap()[0].dirty == Some(false));
}

#[test]
fn worktree_copy_untracked_is_opt_in() {
    let (_td, root, repo) = new_repo();
    fs::write(root.join(".gitignore"), ".env\n*.local\n").unwrap();
    git(&root, &["add", ".gitignore"]);
    git(&root, &["commit", "-q", "-m", "ignore"]);
    fs::write(root.join(".env"), "SECRET=1").unwrap();
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub").join(".env"), "SECRET=2").unwrap();
    fs::write(root.join("sub").join("x.local"), "L").unwrap();
    fs::write(root.join("untracked.txt"), "u").unwrap();

    // Default: nothing copied.
    let w = repo.create_worktree(&NewWorktree::new("plain")).unwrap();
    assert!(!w.path.join(".env").exists());
    assert!(!w.path.join("untracked.txt").exists());

    let c = repo
        .create_worktree_detailed(&NewWorktree {
            name: "withenv".into(),
            copy_untracked: vec![".env".into(), "sub/*.local".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(fs::read_to_string(c.worktree.path.join(".env")).unwrap(), "SECRET=1");
    assert_eq!(fs::read_to_string(c.worktree.path.join("sub/.env")).unwrap(), "SECRET=2");
    assert!(c.worktree.path.join("sub/x.local").is_file());
    assert!(!c.worktree.path.join("untracked.txt").exists());
    assert_eq!(c.copied.len(), 3, "{:?}", c.copied);

    // Dangerous patterns are rejected before anything is created.
    let n = repo.worktrees().unwrap().len();
    for bad in ["../secret", "/abs", ":(top)x", "C:/x", ""] {
        assert!(repo
            .create_worktree(&NewWorktree {
                name: "bad".into(),
                copy_untracked: vec![bad.into()],
                ..Default::default()
            })
            .is_err());
    }
    assert_eq!(repo.worktrees().unwrap().len(), n);
}

#[test]
fn worktree_create_from_linked_worktree_uses_main_siblings() {
    let (td, _root, repo) = new_repo();
    let w = repo.create_worktree(&NewWorktree::new("one")).unwrap();
    let inside = Repo::discover(&w.path).unwrap().unwrap();
    let w2 = inside.create_worktree(&NewWorktree::new("two")).unwrap();
    assert!(same(&w2.path, &td.path().join("my repo.worktrees").join("two")));
}

#[test]
fn worktree_open_in_editor_argv() {
    let (_td, root, repo) = new_repo();
    let w = repo.create_worktree(&NewWorktree::new("ed")).unwrap();
    let argv = repo.open_in_editor(&w.path, &Editor::VsCode).unwrap();
    assert_eq!(argv[0], if cfg!(windows) { "code.cmd" } else { "code" });
    assert_eq!(argv[1], "-n");
    assert!(same(Path::new(&argv[2]), &w.path));

    let argv = repo
        .open_in_editor(&w.path, &Editor::Custom(r#""C:\Program Files\Ed\ed.exe" --new"#.into()))
        .unwrap();
    assert_eq!(argv[0], r"C:\Program Files\Ed\ed.exe");
    assert_eq!(argv[1], "--new");
    assert_eq!(argv.len(), 3);
    let argv = repo
        .open_in_editor(&w.path, &Editor::Custom("zed --folder={path} {path}".into()))
        .unwrap();
    assert_eq!(argv.len(), 3);
    assert!(same(Path::new(&argv[2]), &w.path));
    // Only worktrees of this repo; empty command rejected.
    assert!(repo.open_in_editor(&root.join("nope"), &Editor::VsCode).is_err());
    assert!(repo.open_in_editor(&w.path, &Editor::Custom("  ".into())).is_err());
}

// ------------------------------------------------------------------ checkpoints

fn index_bytes(root: &Path) -> Vec<u8> {
    fs::read(root.join(".git").join("index")).unwrap()
}

/// A repo with: a stash, staged + unstaged edits, an untracked file and an ignored file.
fn busy_repo() -> (TempDir, PathBuf, Repo) {
    let (td, root, repo) = new_repo();
    commit_file(&root, "b.txt", "bravo\n", "b");
    commit_file(&root, "c.txt", "charlie\n", "c");
    fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
    git(&root, &["add", ".gitignore"]);
    git(&root, &["commit", "-q", "-m", "gi"]);
    // A stash entry.
    fs::write(root.join("c.txt"), "stashed\n").unwrap();
    git(&root, &["stash", "-q"]);
    // Working state.
    fs::write(root.join("a.txt"), "alpha edited\n").unwrap();
    fs::write(root.join("b.txt"), "bravo staged\n").unwrap();
    git(&root, &["add", "b.txt"]);
    fs::write(root.join("b.txt"), "bravo staged then edited\n").unwrap();
    fs::create_dir_all(root.join("dir/sub")).unwrap();
    fs::write(root.join("dir/sub/u.txt"), "untracked\n").unwrap();
    fs::write(root.join("ignored.log"), "noise").unwrap();
    (td, root, repo)
}

#[test]
fn checkpoint_create_changes_nothing() {
    let (_td, root, repo) = busy_repo();
    let status = git(&root, &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let stash = git(&root, &["stash", "list"]);
    let stash_ref = git(&root, &["rev-parse", "refs/stash"]);
    assert!(!stash.trim().is_empty());
    let index = index_bytes(&root);
    let files = dir_map(&root);
    let refs_before = git(&root, &["for-each-ref", "--format=%(refname) %(objectname)"]);

    let cp = repo.create_checkpoint("before the agent").unwrap();

    assert_eq!(git(&root, &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"]), status);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["stash", "list"]), stash);
    assert_eq!(git(&root, &["rev-parse", "refs/stash"]), stash_ref);
    assert_eq!(index_bytes(&root), index, "real index file must be byte-identical");
    assert_eq!(dir_map(&root), files);
    // No leftover temp index.
    let leftovers: Vec<_> = fs::read_dir(root.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("selachi-tmp-index"))
        .collect();
    assert!(leftovers.is_empty());
    // Only the checkpoint ref was added.
    let refs_after = git(&root, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let added: Vec<&str> = refs_after.lines().filter(|l| !refs_before.contains(l)).collect();
    assert_eq!(added.len(), 1, "{added:?}");
    assert!(added[0].starts_with(&format!("refs/selachi/checkpoints/{}", cp.id)));

    assert_eq!(cp.label, "before the agent");
    assert!(cp.includes_untracked && !cp.auto);
    assert!(same(&cp.worktree, &root));
    assert_eq!(cp.head.as_deref(), Some(head.trim()));
    // Stash-like shape: parents are [HEAD, index commit]; tree has untracked, not ignored.
    let parents = git(&root, &["rev-list", "--parents", "-n", "1", &cp.commit]);
    assert_eq!(parents.split_whitespace().count(), 3);
    assert!(parents.contains(head.trim()));
    let tree = git(&root, &["ls-tree", "-r", "--name-only", &cp.commit]);
    assert!(tree.lines().any(|l| l == "dir/sub/u.txt"));
    assert!(!tree.lines().any(|l| l == "ignored.log"));
    let wt_b = git(&root, &["show", &format!("{}:b.txt", cp.commit)]);
    assert_eq!(wt_b, "bravo staged then edited\n");
    let idx_b = git(&root, &["show", &format!("{}^2:b.txt", cp.commit)]);
    assert_eq!(idx_b, "bravo staged\n");

    let list = repo.checkpoints().unwrap();
    assert_eq!(list, vec![cp]);
}

#[test]
fn checkpoint_restore_all_is_exact_and_undoable() {
    let (_td, root, repo) = busy_repo();
    // Binary + CRLF + empty files in the snapshot.
    fs::write(root.join("bin.dat"), [0u8, 1, 2, 255, 254, 0, 13, 10, 13]).unwrap();
    fs::write(root.join("crlf.txt"), b"one\r\ntwo\nthree\r\n").unwrap();
    fs::write(root.join("empty.txt"), b"").unwrap();

    let before_files = dir_map(&root);
    let before_status = git(&root, &["status", "--porcelain=v2", "-z", "--untracked-files=all"]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let cp = repo.create_checkpoint("snap").unwrap();

    // The "agent": modify, delete, create (also nested), stage, touch ignored.
    fs::write(root.join("a.txt"), "agent wrote\n").unwrap();
    fs::remove_file(root.join("b.txt")).unwrap();
    fs::remove_file(root.join("dir/sub/u.txt")).unwrap();
    fs::write(root.join("bin.dat"), b"gone").unwrap();
    fs::create_dir_all(root.join("newdir/deep")).unwrap();
    fs::write(root.join("newdir/deep/n.txt"), "new").unwrap();
    fs::write(root.join("new.txt"), "new").unwrap();
    git(&root, &["add", "new.txt", "a.txt"]);
    fs::write(root.join("ignored.log"), "changed by agent").unwrap();
    fs::write(root.join("ignored2.log"), "x").unwrap(); // ignored? no: untracked, will be deleted
    let after_files = dir_map(&root);

    let plan = repo.restore_plan(&cp.id, &RestoreMode::All).unwrap();
    let names = |v: &Vec<PathBuf>| {
        let mut s: Vec<String> = v.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        s.sort();
        s
    };
    assert_eq!(names(&plan.create), ["b.txt", "dir/sub/u.txt"]);
    assert_eq!(names(&plan.overwrite), ["a.txt", "bin.dat"]);
    assert_eq!(names(&plan.delete), ["ignored2.log", "new.txt", "newdir/deep/n.txt"]);
    let pf = repo.preflight_restore(&cp.id, &RestoreMode::All).unwrap();
    assert!(pf.requires_confirmation);
    assert!(pf.warnings.iter().any(|w| w.contains("will delete") && w.contains("new.txt")));
    assert!(pf.warnings.iter().any(|w| w.contains("will overwrite") && w.contains("a.txt")));
    assert!(pf.warnings.iter().any(|w| w.contains("will re-create") && w.contains("b.txt")));
    // Preflight changes nothing.
    assert_eq!(dir_map(&root), after_files);

    let report = repo.restore_checkpoint(&cp.id, RestoreMode::All).unwrap();

    // Exact original state; ignored.log was never touched.
    let mut expected = before_files;
    expected.insert("ignored.log".into(), b"changed by agent".to_vec());
    let got = dir_map(&root);
    assert_eq!(got, expected);
    assert_eq!(fs::read(root.join("crlf.txt")).unwrap(), b"one\r\ntwo\nthree\r\n");
    assert!(!root.join("newdir").exists(), "empty dirs left by deleted files are pruned");
    assert_eq!(git(&root, &["status", "--porcelain=v2", "-z", "--untracked-files=all"]), before_status);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["ls-files", "--stage"]), git_ls_stage_of(&cp, &root));

    // Auto checkpoint exists and restoring it brings the agent's state back (undo).
    let auto = report.auto_checkpoint.expect("auto checkpoint");
    assert!(auto.auto);
    assert!(auto.label.starts_with("antes de restaurar"), "{}", auto.label);
    let ids: Vec<String> = repo.checkpoints().unwrap().into_iter().map(|c| c.id).collect();
    assert!(ids.contains(&auto.id) && ids.contains(&cp.id));
    assert!(report.skipped.is_empty());
    assert_eq!(report.deleted.len(), 3);

    repo.restore_checkpoint(&auto.id, RestoreMode::All).unwrap();
    assert_eq!(dir_map(&root), after_files);
}

fn git_ls_stage_of(cp: &git_svc::Checkpoint, root: &Path) -> String {
    // Staged state of the snapshot = its index commit's tree.
    let idx_tree = git(root, &["rev-parse", &format!("{}^2^{{tree}}", cp.commit)]);
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new("git")
        .args(["ls-tree", "-r", "-t", "--full-tree", idx_tree.trim()])
        .current_dir(root)
        .output()
        .unwrap();
    let _ = tmp;
    // Compare against the real index in `ls-files --stage` format via ls-tree equivalence.
    let real = git(root, &["ls-files", "--stage"]);
    let from_tree: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.contains(" tree "))
        .map(|l| {
            let (meta, path) = l.split_once('\t').unwrap();
            let f: Vec<&str> = meta.split(' ').collect();
            format!("{} {} 0\t{}", f[0], f[2], path)
        })
        .collect();
    assert_eq!(real.lines().map(str::to_owned).collect::<Vec<_>>(), from_tree);
    real
}

#[test]
fn checkpoint_roundtrips_bytes_with_autocrlf_enabled() {
    let (_td, root, repo) = new_repo();
    git(&root, &["config", "core.autocrlf", "true"]);
    fs::write(root.join("mixed.txt"), b"a\r\nb\nc\r\n").unwrap();
    fs::write(root.join("a.txt"), b"alpha\r\nedit\r\n").unwrap();
    let before = dir_map(&root);
    let cp = repo.create_checkpoint("crlf").unwrap();
    fs::write(root.join("mixed.txt"), b"x").unwrap();
    fs::write(root.join("a.txt"), b"y").unwrap();
    repo.restore_checkpoint(&cp.id, RestoreMode::All).unwrap();
    assert_eq!(dir_map(&root), before);
}

#[test]
fn checkpoint_restore_files_subset() {
    let (_td, root, repo) = busy_repo();
    let cp = repo.create_checkpoint("snap").unwrap();
    fs::write(root.join("a.txt"), "agent a\n").unwrap();
    fs::write(root.join("b.txt"), "agent b\n").unwrap();
    fs::remove_file(root.join("dir/sub/u.txt")).unwrap();
    fs::write(root.join("new.txt"), "new").unwrap();
    fs::write(root.join("new2.txt"), "new2").unwrap();

    // One tracked file by relative path; one deleted untracked file by directory; new2 by absolute.
    let r = repo
        .restore_checkpoint(
            &cp.id,
            RestoreMode::Files(vec![PathBuf::from("a.txt"), PathBuf::from("dir"), root.join("new2.txt")]),
        )
        .unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "alpha edited\n");
    assert_eq!(fs::read_to_string(root.join("dir/sub/u.txt")).unwrap(), "untracked\n");
    assert!(!root.join("new2.txt").exists(), "file created after the checkpoint is deleted");
    // Untouched: not requested.
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "agent b\n");
    assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "new");
    assert!(r.auto_checkpoint.is_some());
    assert_eq!(r.restored.len(), 2);
    assert_eq!(r.deleted.len(), 1);

    // A file with no difference: no-op, and no leftover auto checkpoint.
    let n = repo.checkpoints().unwrap().len();
    let r = repo
        .restore_checkpoint(&cp.id, RestoreMode::Files(vec![PathBuf::from("a.txt")]))
        .unwrap();
    assert!(r.auto_checkpoint.is_none() && r.restored.is_empty());
    assert_eq!(repo.checkpoints().unwrap().len(), n);

    // Bad paths.
    assert!(repo.restore_checkpoint(&cp.id, RestoreMode::Files(vec![])).is_err());
    assert!(repo.restore_checkpoint(&cp.id, RestoreMode::Files(vec![PathBuf::from("../x")])).is_err());
    assert!(repo
        .restore_checkpoint(&cp.id, RestoreMode::Files(vec![std::env::temp_dir().join("elsewhere.txt")]))
        .is_err());
    assert!(repo.restore_checkpoint("nope", RestoreMode::All).is_err());
    assert!(repo.restore_checkpoint("../../x", RestoreMode::All).is_err());
}

#[test]
fn checkpoint_diff_and_changed_files() {
    let (_td, root, repo) = busy_repo();
    let cp = repo.create_checkpoint("snap").unwrap();
    assert!(repo.diff_since_checkpoint(&cp.id).unwrap().is_empty());
    assert!(repo.changed_files_since(&cp.id).unwrap().is_empty());

    fs::write(root.join("a.txt"), "alpha changed again\n").unwrap();
    fs::remove_file(root.join("dir/sub/u.txt")).unwrap();
    fs::write(root.join("brand-new.txt"), "hello\n").unwrap();
    fs::write(root.join("ignored.log"), "different noise").unwrap(); // ignored: must not appear

    let mut ch = repo.changed_files_since(&cp.id).unwrap();
    ch.sort_by(|a, b| a.path.cmp(&b.path));
    let got: Vec<(String, DeltaKind)> =
        ch.iter().map(|c| (c.path.to_string_lossy().replace('\\', "/"), c.kind)).collect();
    assert_eq!(
        got,
        [
            ("a.txt".to_owned(), DeltaKind::Modified),
            ("brand-new.txt".to_owned(), DeltaKind::Added),
            ("dir/sub/u.txt".to_owned(), DeltaKind::Deleted),
        ]
    );
    let d = repo.diff_since_checkpoint(&cp.id).unwrap();
    assert!(d.contains("+alpha changed again"), "{d}");
    assert!(d.contains("-alpha edited"));
    assert!(d.contains("+++ b/brand-new.txt"), "untracked files are part of the diff: {d}");
    assert!(d.contains("-untracked"));
    assert!(!d.contains("ignored.log"));
}

#[test]
fn checkpoint_tracked_only_never_touches_untracked() {
    let (_td, root, repo) = busy_repo();
    let cp = repo.create_checkpoint_with("tracked only", false).unwrap();
    assert!(!cp.includes_untracked);
    let tree = git(&root, &["ls-tree", "-r", "--name-only", &cp.commit]);
    assert!(!tree.lines().any(|l| l == "dir/sub/u.txt"));
    fs::write(root.join("a.txt"), "changed").unwrap();
    fs::write(root.join("fresh.txt"), "fresh").unwrap();
    fs::write(root.join("dir/sub/u.txt"), "edited untracked").unwrap();
    repo.restore_checkpoint(&cp.id, RestoreMode::All).unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "alpha edited\n");
    assert_eq!(fs::read_to_string(root.join("fresh.txt")).unwrap(), "fresh");
    assert_eq!(fs::read_to_string(root.join("dir/sub/u.txt")).unwrap(), "edited untracked");
}

#[test]
fn checkpoint_in_unborn_repo_and_when_head_moves() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path().join("fresh");
    init_repo(&root);
    let repo = Repo::discover(&root).unwrap().unwrap();
    fs::write(root.join("one.txt"), "1").unwrap();
    git(&root, &["add", "one.txt"]);
    fs::write(root.join("two.txt"), "2").unwrap();
    let cp = repo.create_checkpoint("unborn").unwrap();
    assert!(cp.head.is_none());
    fs::write(root.join("one.txt"), "changed").unwrap();
    fs::remove_file(root.join("two.txt")).unwrap();
    fs::write(root.join("three.txt"), "3").unwrap();
    repo.restore_checkpoint(&cp.id, RestoreMode::All).unwrap();
    assert_eq!(fs::read_to_string(root.join("one.txt")).unwrap(), "1");
    assert_eq!(fs::read_to_string(root.join("two.txt")).unwrap(), "2");
    assert!(!root.join("three.txt").exists());
    assert!(git(&root, &["status", "--porcelain"]).contains("A  one.txt"));

    // HEAD moves (the agent commits): restore still returns files; HEAD is never rewritten.
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    let cp2 = repo.create_checkpoint("after first").unwrap();
    fs::write(root.join("one.txt"), "agent").unwrap();
    git(&root, &["commit", "-q", "-am", "agent commit"]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let r = repo.preflight_restore(&cp2.id, &RestoreMode::All).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("HEAD moved")));
    repo.restore_checkpoint(&cp2.id, RestoreMode::All).unwrap();
    assert_eq!(fs::read_to_string(root.join("one.txt")).unwrap(), "1");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    // Index was reset to HEAD: the file shows as an unstaged modification.
    assert!(git(&root, &["status", "--porcelain"]).contains(" M one.txt"));
}

#[test]
fn checkpoints_are_filtered_per_worktree() {
    let (_td, root, repo) = new_repo();
    let w = repo.create_worktree(&NewWorktree::new("agent1")).unwrap();
    let wt = Repo::discover(&w.path).unwrap().unwrap();

    fs::write(root.join("main-only.txt"), "m").unwrap();
    let cm = repo.create_checkpoint("main").unwrap();
    sleep(Duration::from_millis(5));
    fs::write(w.path.join("wt-only.txt"), "w").unwrap();
    let cw = wt.create_checkpoint("worktree").unwrap();
    assert!(same(&cw.worktree, &w.path));

    let main_ids: Vec<_> = repo.checkpoints().unwrap().into_iter().map(|c| c.id).collect();
    let wt_ids: Vec<_> = wt.checkpoints().unwrap().into_iter().map(|c| c.id).collect();
    assert_eq!(main_ids, std::slice::from_ref(&cm.id));
    assert_eq!(wt_ids, std::slice::from_ref(&cw.id));
    // Refs are repo-wide.
    let all = wt.all_checkpoints().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, cw.id, "newest first");
    assert_eq!(repo.all_checkpoints().unwrap(), all);

    // Cross-worktree restore/delete refused.
    assert!(wt.restore_checkpoint(&cm.id, RestoreMode::All).is_err());
    assert!(repo.restore_checkpoint(&cw.id, RestoreMode::All).is_err());
    assert!(wt.delete_checkpoint(&cm.id).is_err());
    // Index/HEAD/status of the worktree untouched by create; restore works inside it.
    fs::write(w.path.join("wt-only.txt"), "changed").unwrap();
    fs::write(w.path.join("a.txt"), "edited in wt").unwrap();
    wt.restore_checkpoint(&cw.id, RestoreMode::All).unwrap();
    assert_eq!(fs::read_to_string(w.path.join("wt-only.txt")).unwrap(), "w");
    assert_eq!(fs::read_to_string(w.path.join("a.txt")).unwrap(), "alpha\n");
    // The main worktree was not affected by any of it.
    assert_eq!(fs::read_to_string(root.join("main-only.txt")).unwrap(), "m");
    assert!(!w.path.join("main-only.txt").exists());
    // Leftover temp index does not exist in the worktree's git dir either.
    assert!(wt.checkpoints().unwrap().iter().any(|c| c.auto));
}

#[test]
fn checkpoint_delete_and_prune() {
    let (_td, root, repo) = new_repo();
    let mut ids = Vec::new();
    for i in 0..5 {
        fs::write(root.join("a.txt"), format!("v{i}")).unwrap();
        ids.push(repo.create_checkpoint(&format!("cp{i}")).unwrap().id);
        sleep(Duration::from_millis(5));
    }
    let list = repo.checkpoints().unwrap();
    assert_eq!(list.len(), 5);
    assert_eq!(list[0].label, "cp4");
    assert_eq!(list[4].label, "cp0");

    // Not old enough: nothing pruned even though over keep_last.
    assert!(repo.prune_checkpoints(2, Duration::from_secs(3600)).unwrap().is_empty());
    assert_eq!(repo.checkpoints().unwrap().len(), 5);
    // keep_last wins over age.
    assert!(repo.prune_checkpoints(10, Duration::ZERO).unwrap().is_empty());
    // Old enough and beyond keep_last.
    let gone = repo.prune_checkpoints(2, Duration::ZERO).unwrap();
    assert_eq!(gone.len(), 3);
    let left: Vec<_> = repo.checkpoints().unwrap().into_iter().map(|c| c.label).collect();
    assert_eq!(left, ["cp4", "cp3"]);

    repo.delete_checkpoint(&ids[3]).unwrap();
    assert_eq!(repo.checkpoints().unwrap().len(), 1);
    assert!(repo.delete_checkpoint(&ids[3]).is_err());
    assert!(repo.delete_checkpoint("bad id").is_err());
    // Labels with newlines / unicode are sanitised, not rejected.
    let c = repo.create_checkpoint("línea 1\nlínea 2 🦈").unwrap();
    assert_eq!(c.label, "línea 1 línea 2 🦈");
    assert_eq!(repo.checkpoints().unwrap()[0].label, c.label);
}
