//! Everything that touches git, run on the background executor: the snapshot the panel shows,
//! diff previews, preflights and the execution of an [`GitOp`].
//!
//! Plain functions over a [`Repo`], no gpui: easy to test against a temp repository.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use git_svc::{Checkpoint, Commit, GitAction, NewWorktree, Repo, RepoStatus, RestoreMode, Worktree};

use crate::cmd::{GitOp, run_git};
use crate::diffview::{DiffPreview, parse_unified, preview_of, preview_untracked};
use crate::fmt::{first_line, plural, slashes};
use crate::gate::{Outcome, Review, build_review};
use crate::sections::{Counts, Section, parse_numstat};

/// Commits listed in the history.
pub const HISTORY_LEN: usize = 30;
/// Above this many changed files the per-file `+/-` counts are skipped (they are not free).
const MAX_COUNTED_FILES: usize = 1_500;

/// One read of the repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitData {
    pub status: RepoStatus,
    pub counts: Counts,
    pub commits: Arc<Vec<Commit>>,
    pub worktrees: Arc<Vec<Worktree>>,
    pub checkpoints: Arc<Vec<Checkpoint>>,
}

impl GitData {
    /// HEAD does not exist yet (a repo without commits).
    pub fn unborn(&self) -> bool {
        self.status.head_oid.is_none()
    }

    /// The worktree whose root is `root`, if listed.
    pub fn worktree_at(&self, root: &Path) -> Option<&Worktree> {
        self.worktrees.iter().find(|w| same_path(&w.path, root))
    }
}

/// Paths compared the way Windows does (case-insensitive, `\` = `/`).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| slashes(p).trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Reads status, per-file counts, worktrees, checkpoints and (when HEAD moved or `full`) the log.
/// `prev` carries over what did not change, so a burst of file events does not re-read history.
pub fn load(repo: &Repo, prev: Option<&GitData>, full: bool) -> Result<GitData, String> {
    let status = repo.status().map_err(err)?;
    let changed = status.staged.len() + status.unstaged.len();
    let numstat = |cached: bool| {
        let mut args = vec!["diff", "--numstat", "-z", "--no-color", "--no-ext-diff"];
        if cached {
            args.push("--cached");
        }
        run_git(&repo.root, &args, LOCAL)
            .ok()
            .filter(crate::cmd::GitRun::success)
            .map(|r| parse_numstat(r.stdout.as_bytes()))
            .unwrap_or_default()
    };
    let counts = if changed <= MAX_COUNTED_FILES {
        Counts {
            staged: if status.staged.is_empty() { Default::default() } else { numstat(true) },
            unstaged: if status.unstaged.is_empty() { Default::default() } else { numstat(false) },
        }
    } else {
        Counts::default()
    };
    let commits = match prev {
        Some(p) if !full && p.status.head_oid == status.head_oid => p.commits.clone(),
        _ => Arc::new(repo.recent_commits(HISTORY_LEN).unwrap_or_default()),
    };
    let mut worktrees = repo.worktrees().unwrap_or_default();
    for w in &mut worktrees {
        w.dirty = if same_path(&w.path, &repo.root) {
            Some(status.is_dirty() || !status.untracked.is_empty())
        } else {
            prev.and_then(|p| p.worktree_at(&w.path)).and_then(|o| o.dirty)
        };
    }
    let checkpoints = repo.checkpoints().unwrap_or_default();
    Ok(GitData {
        status,
        counts,
        commits,
        worktrees: Arc::new(worktrees),
        checkpoints: Arc::new(checkpoints),
    })
}

const LOCAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Worktrees with their `dirty` flag filled in (one `git status` each).
pub fn load_worktrees_dirty(repo: &Repo) -> Result<Vec<Worktree>, String> {
    repo.worktrees_with_dirty().map_err(err)
}

// ---------------------------------------------------------------------------------------------
// Diff previews
// ---------------------------------------------------------------------------------------------

/// Which diff the panel shows inline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffTarget {
    Working { section: Section, path: PathBuf },
    Checkpoint { id: String, path: PathBuf },
}

impl DiffTarget {
    pub fn path(&self) -> &Path {
        match self {
            DiffTarget::Working { path, .. } | DiffTarget::Checkpoint { path, .. } => path,
        }
    }
}

/// Loads the preview of `target`.
pub fn load_diff(repo: &Repo, target: &DiffTarget) -> Result<DiffPreview, String> {
    let rel = target.path();
    let text = slashes(rel);
    let abs = repo.root.join(rel);
    let from_text = |raw: String| {
        let files = parse_unified(&raw);
        match files.iter().find(|f| f.path == text).or(files.first()) {
            Some(f) => preview_of(f, Some(&abs)),
            None => crate::diffview::build_preview(&text, &[], false, None),
        }
    };
    match target {
        DiffTarget::Working { section: Section::Untracked | Section::Conflicts, .. } => {
            preview_untracked(&repo.root, rel)
        }
        DiffTarget::Working { section, .. } => {
            let raw = repo.diff(Some(rel), *section == Section::Staged).map_err(err)?;
            Ok(from_text(raw))
        }
        DiffTarget::Checkpoint { id, .. } => {
            let raw = repo.diff_since_checkpoint(id).map_err(err)?;
            Ok(from_text(raw))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------------------------

/// The card for a sensitive operation, from a fresh status.
pub fn preflight(repo: &Repo, op: &GitOp) -> Result<Review, String> {
    op.validate()?;
    let status = repo.status().map_err(err)?;
    let (pf, plan) = match op {
        GitOp::Action(a) => (repo.preflight(a).map_err(err)?, None),
        GitOp::CommitAndPush { message } => {
            (repo.preflight(&GitAction::Commit { message: message.clone() }).map_err(err)?, None)
        }
        GitOp::RemoveWorktree { path, .. } => (repo.preflight_remove_worktree(path).map_err(err)?, None),
        GitOp::RestoreCheckpoint { id } => {
            let pf = repo.preflight_restore(id, &RestoreMode::All).map_err(err)?;
            let plan = repo.restore_plan(id, &RestoreMode::All).map_err(err)?;
            (pf, Some(plan))
        }
        _ => (git_svc::Preflight::default(), None),
    };
    Ok(build_review(op, &pf, plan.as_ref(), &status))
}

// ---------------------------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------------------------

fn fail(summary: impl Into<String>, detail: impl Into<String>, retry: Option<Vec<String>>) -> Outcome {
    Outcome { ok: false, summary: summary.into(), detail: detail.into(), retry }
}

fn done(summary: impl Into<String>) -> Outcome {
    Outcome { ok: true, summary: summary.into(), detail: String::new(), retry: None }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Runs an `argv` (including the leading `git`) through [`run_git`].
fn run_argv(repo: &Repo, argv: &[String], timeout: std::time::Duration) -> Result<crate::cmd::GitRun, String> {
    run_git(&repo.root, &argv[1..], timeout)
}

fn run_action(repo: &Repo, action: &GitAction) -> Outcome {
    let argv = action.argv();
    let network = matches!(action, GitAction::Push | GitAction::Pull | GitAction::Fetch);
    let retry = network.then(|| argv.clone());
    match repo.run(action) {
        Err(e) => fail(format!("{}: {e}", op_title(action)), "", retry),
        Ok(out) if out.success() => {
            let line = first_line(&out.stdout);
            let line = if line.is_empty() { first_line(&out.stderr) } else { line };
            done(match action {
                GitAction::Push => "Push completado".to_string(),
                GitAction::Fetch => "Fetch completado".to_string(),
                _ if line.is_empty() => format!("{} completado", op_title(action)),
                _ => clip(line, 140),
            })
        }
        Ok(out) => {
            let msg = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
            fail(format!("{} falló: {}", op_title(action), clip(first_line(msg), 140)), clip(msg, 700), retry)
        }
    }
}

fn op_title(a: &GitAction) -> &'static str {
    match a {
        GitAction::Commit { .. } => "Commit",
        GitAction::Push => "Push",
        GitAction::Pull => "Pull",
        GitAction::Fetch => "Fetch",
        _ => "Git",
    }
}

/// Runs `op` and reports what happened. Sensitive operations must only be passed here after the
/// confirmation gate released them.
pub fn execute(repo: &Repo, op: &GitOp) -> Outcome {
    if let Err(e) = op.validate() {
        return fail(e, "", None);
    }
    match op {
        GitOp::Stage(_) | GitOp::Unstage { .. } | GitOp::StageAll | GitOp::UnstageAll { .. } => {
            let steps = op.argv_steps();
            let Some(argv) = steps.first() else { return fail("operación vacía", "", None) };
            match run_argv(repo, argv, LOCAL) {
                Ok(r) if r.success() => done(op.title()),
                Ok(r) => fail(format!("{} falló: {}", op.title(), clip(first_line(&r.stderr), 140)), clip(&r.stderr, 700), None),
                Err(e) => fail(e, "", None),
            }
        }
        GitOp::Action(a) => run_action(repo, a),
        GitOp::CommitAndPush { message } => {
            let commit = run_action(repo, &GitAction::Commit { message: message.clone() });
            if !commit.ok {
                return commit;
            }
            let push = run_action(repo, &GitAction::Push);
            if push.ok {
                done("Commit y push completados")
            } else {
                fail(format!("Commit creado, pero el push falló: {}", push.summary), push.detail, push.retry)
            }
        }
        GitOp::PublishBranch { branch } => {
            let argv = &op.argv_steps()[0];
            match run_argv(repo, argv, std::time::Duration::from_secs(120)) {
                Ok(r) if r.success() => done(format!("Rama {branch} publicada en origin")),
                Ok(r) => fail(
                    format!("Publicar falló: {}", clip(first_line(&r.stderr), 140)),
                    clip(&r.stderr, 700),
                    Some(argv.clone()),
                ),
                Err(e) => fail(e, "", Some(argv.clone())),
            }
        }
        GitOp::CreateWorktree { name } => match repo.create_worktree(&NewWorktree::new(name.trim())) {
            Ok(w) => Outcome {
                ok: true,
                summary: format!("Worktree «{}» creado", name.trim()),
                detail: slashes(&w.path),
                retry: None,
            },
            Err(e) => fail(format!("No se pudo crear el worktree: {e}"), "", None),
        },
        GitOp::RemoveWorktree { path, force } => match repo.remove_worktree(path, *force) {
            Ok(()) => done("Worktree eliminado"),
            Err(e) => fail(format!("No se pudo eliminar el worktree: {e}"), "", None),
        },
        GitOp::CreateCheckpoint { label } => {
            let label = if label.trim().is_empty() { "manual" } else { label.trim() };
            match repo.create_checkpoint(label) {
                Ok(c) => done(format!("Checkpoint «{}» creado", c.label)),
                Err(e) => fail(format!("No se pudo crear el checkpoint: {e}"), "", None),
            }
        }
        GitOp::RestoreCheckpoint { id } => match repo.restore_checkpoint(id, RestoreMode::All) {
            Ok(r) => Outcome {
                ok: true,
                summary: format!(
                    "Restaurado: {} · {} eliminados",
                    plural(r.restored.len(), "archivo", "archivos"),
                    r.deleted.len()
                ),
                detail: r
                    .auto_checkpoint
                    .map(|c| format!("Estado anterior guardado en el checkpoint «{}»", c.label))
                    .unwrap_or_default(),
                retry: None,
            },
            Err(e) => fail(format!("No se pudo restaurar: {e}"), "", None),
        },
    }
}

/// Unified result of a worktree list entry for the switcher: directory name and branch label.
pub fn worktree_label(w: &Worktree) -> (String, String) {
    let name = w.path.file_name().map_or_else(|| slashes(&w.path), |n| n.to_string_lossy().into_owned());
    let branch = match (&w.branch, w.detached) {
        (Some(b), _) => b.clone(),
        (None, true) => "HEAD separado".to_string(),
        (None, false) => String::new(),
    };
    (name, branch)
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;
    use std::process::Command;

    /// Runs git in `dir` (tests only).
    pub fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=Tester", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repo with one commit (`a.txt`, `src/lib.rs`).
    pub fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").expect("write");
        std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").expect("write");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn repo(dir: &tempfile::TempDir) -> Repo {
        Repo::discover(dir.path()).expect("discover").expect("a repo")
    }

    #[test]
    fn snapshot_groups_changes_and_counts_lines() {
        let dir = temp_repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "fn main() { 1 }\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        git(dir.path(), &["add", "src/lib.rs"]);
        let r = repo(&dir);
        let d = load(&r, None, true).unwrap();
        assert_eq!(d.status.staged.len(), 1);
        assert_eq!(d.status.unstaged.len(), 1);
        assert_eq!(d.status.untracked.len(), 1);
        assert_eq!(d.counts.unstaged[Path::new("a.txt")], Some((2, 1)));
        assert_eq!(d.counts.staged[Path::new("src/lib.rs")], Some((1, 1)));
        assert_eq!(d.commits.len(), 1);
        assert_eq!(d.worktrees.len(), 1);
        assert_eq!(d.worktrees[0].dirty, Some(true));
        // A second load with the same HEAD reuses the commit list.
        let d2 = load(&r, Some(&d), false).unwrap();
        assert!(Arc::ptr_eq(&d.commits, &d2.commits));
        let d3 = load(&r, Some(&d), true).unwrap();
        assert!(!Arc::ptr_eq(&d.commits, &d3.commits));
        assert_eq!(d, d3);
    }

    #[test]
    fn stage_and_unstage_run_the_exact_commands() {
        let dir = temp_repo();
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        let r = repo(&dir);
        let o = execute(&r, &GitOp::Stage("a.txt".into()));
        assert!(o.ok, "{o:?}");
        assert_eq!(load(&r, None, false).unwrap().status.staged.len(), 1);
        let o = execute(&r, &GitOp::Unstage { path: "a.txt".into(), unborn: false });
        assert!(o.ok, "{o:?}");
        let d = load(&r, None, false).unwrap();
        assert!(d.status.staged.is_empty() && d.status.unstaged.len() == 1);
        assert!(execute(&r, &GitOp::StageAll).ok);
        assert!(execute(&r, &GitOp::UnstageAll { unborn: false }).ok);
        assert!(load(&r, None, false).unwrap().status.staged.is_empty());
    }

    #[test]
    fn unborn_repos_unstage_with_rm_cached() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("f.txt"), "x\n").unwrap();
        git(dir.path(), &["add", "f.txt"]);
        let r = repo(&dir);
        let d = load(&r, None, true).unwrap();
        assert!(d.unborn() && d.commits.is_empty());
        assert!(execute(&r, &GitOp::Unstage { path: "f.txt".into(), unborn: true }).ok);
        assert!(load(&r, None, false).unwrap().status.staged.is_empty());
    }

    #[test]
    fn commit_flow_goes_through_preflight_then_execute() {
        let dir = temp_repo();
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        let r = repo(&dir);
        let op = GitOp::Action(GitAction::Commit { message: "feat: change".into() });
        // Nothing staged: the review blocks the commit.
        let review = preflight(&r, &op).unwrap();
        assert!(review.blocked.is_some());
        assert!(execute(&r, &GitOp::Stage("a.txt".into())).ok);
        let review = preflight(&r, &op).unwrap();
        assert!(review.blocked.is_none(), "{review:?}");
        assert!(review.stat.as_deref().is_some_and(|s| s.contains("a.txt")));
        let o = execute(&r, &op);
        assert!(o.ok, "{o:?}");
        let d = load(&r, None, true).unwrap();
        assert_eq!(d.commits[0].subject, "feat: change");
        assert!(d.status.is_clean());
    }

    #[test]
    fn checkpoint_create_diff_and_restore() {
        let dir = temp_repo();
        let r = repo(&dir);
        std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        assert!(execute(&r, &GitOp::CreateCheckpoint { label: "antes".into() }).ok);
        let cps = r.checkpoints().unwrap();
        assert_eq!(cps.len(), 1);
        let id = cps[0].id.clone();
        std::fs::write(dir.path().join("a.txt"), "v2\n").unwrap();
        std::fs::write(dir.path().join("extra.txt"), "new\n").unwrap();

        let p = load_diff(&r, &DiffTarget::Checkpoint { id: id.clone(), path: "a.txt".into() }).unwrap();
        assert_eq!((p.added, p.removed), (1, 1));

        let op = GitOp::RestoreCheckpoint { id };
        let review = preflight(&r, &op).unwrap();
        assert!(review.danger);
        let kinds: Vec<_> = review.lines.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&crate::gate::LineKind::Overwrite), "{review:?}");
        assert!(kinds.contains(&crate::gate::LineKind::Delete), "{review:?}");
        let o = execute(&r, &op);
        assert!(o.ok, "{o:?}");
        assert!(o.detail.contains("antes de restaurar"), "{o:?}");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "v1\n");
        assert!(!dir.path().join("extra.txt").exists());
        // The automatic pre-restore checkpoint is listed (marked auto).
        let d = load(&r, None, false).unwrap();
        assert!(d.checkpoints.iter().any(|c| c.auto));
    }

    #[test]
    fn worktree_create_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main-repo");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("f"), "x").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "init"]);
        let r = Repo::discover(&main).unwrap().unwrap();
        assert!(execute(&r, &GitOp::CreateWorktree { name: "con".into() }).detail.is_empty());
        let o = execute(&r, &GitOp::CreateWorktree { name: "feat-x".into() });
        assert!(o.ok, "{o:?}");
        let wts = load_worktrees_dirty(&r).unwrap();
        assert_eq!(wts.len(), 2);
        let wt = wts.iter().find(|w| !w.is_main).unwrap();
        assert_eq!(worktree_label(wt).0, "feat-x");
        let review = preflight(&r, &GitOp::RemoveWorktree { path: wt.path.clone(), force: false }).unwrap();
        assert!(review.danger);
        let o = execute(&r, &GitOp::RemoveWorktree { path: wt.path.clone(), force: false });
        assert!(o.ok, "{o:?}");
    }

    #[test]
    fn diffs_for_each_section() {
        let dir = temp_repo();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.path().join("fresh.rs"), "fn x() {}\nfn y() {}\n").unwrap();
        let r = repo(&dir);
        let w = load_diff(&r, &DiffTarget::Working { section: Section::Changes, path: "a.txt".into() }).unwrap();
        assert_eq!((w.added, w.removed), (1, 1));
        let u = load_diff(&r, &DiffTarget::Working { section: Section::Untracked, path: "fresh.rs".into() }).unwrap();
        assert_eq!((u.added, u.removed), (2, 0));
        git(dir.path(), &["add", "a.txt"]);
        let s = load_diff(&r, &DiffTarget::Working { section: Section::Staged, path: "a.txt".into() }).unwrap();
        assert_eq!((s.added, s.removed), (1, 1));
        let none = load_diff(&r, &DiffTarget::Working { section: Section::Changes, path: "a.txt".into() }).unwrap();
        assert!(none.is_empty(), "everything is staged now");
    }
}
