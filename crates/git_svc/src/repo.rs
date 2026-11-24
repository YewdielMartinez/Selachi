//! `Repo`: discovery, reading state and running actions.

use std::path::{Path, PathBuf};

use crate::action::{CommandOutput, GitAction, Preflight};
use crate::commits::{parse_log, Commit, LOG_FORMAT};
use crate::error::Result;
use crate::exec::{run_git, run_git_ok, LOCAL_TIMEOUT};
use crate::status::{parse_status, RepoStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// Working tree root.
    pub root: PathBuf,
    /// The (possibly per-worktree) git dir, absolute.
    pub git_dir: PathBuf,
    /// Common git dir (holds refs/objects); equals `git_dir` except in linked worktrees.
    pub(crate) common_dir: PathBuf,
}

impl Repo {
    /// Find the repository containing `path` (a directory, or a file inside one).
    /// `Ok(None)` when `path` is not inside a work tree (including bare repos).
    pub fn discover(path: impl AsRef<Path>) -> Result<Option<Repo>> {
        let path = path.as_ref();
        let cwd: &Path = if path.is_dir() {
            path
        } else {
            match path.parent() {
                Some(p) if p.is_dir() => p,
                _ => return Ok(None),
            }
        };
        let raw = run_git(
            cwd,
            &[
                "rev-parse",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
            ],
            LOCAL_TIMEOUT,
        )?;
        if !raw.success() {
            // 128: not a git repository / not a work tree.
            return Ok(None);
        }
        let out = raw.stdout_string();
        let mut lines = out.lines();
        let (Some(top), Some(gd), Some(common)) = (lines.next(), lines.next(), lines.next()) else {
            return Ok(None);
        };
        let resolve = |s: &str| {
            let p = PathBuf::from(s.trim());
            if p.is_absolute() {
                p
            } else {
                cwd.join(p)
            }
        };
        Ok(Some(Repo {
            root: PathBuf::from(top.trim()),
            git_dir: PathBuf::from(gd.trim()),
            common_dir: resolve(common),
        }))
    }

    /// Current branch / upstream / ahead-behind / changed files.
    pub fn status(&self) -> Result<RepoStatus> {
        let raw = run_git_ok(
            &self.root,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=normal",
            ],
        )?;
        parse_status(&raw.stdout)
    }

    /// Up to `n` most recent commits on HEAD. Empty for an unborn branch.
    pub fn recent_commits(&self, n: usize) -> Result<Vec<Commit>> {
        if !self.has_head() {
            return Ok(Vec::new());
        }
        let n = n.to_string();
        let raw = run_git_ok(&self.root, &["log", "-n", &n, LOG_FORMAT])?;
        parse_log(&raw.stdout_string())
    }

    /// Unified diff of one path (or the whole tree). `staged` selects index-vs-HEAD
    /// instead of worktree-vs-index.
    pub fn diff(&self, path: Option<&Path>, staged: bool) -> Result<String> {
        let mut args: Vec<std::ffi::OsString> = ["diff", "--no-color", "--no-ext-diff"]
            .iter()
            .map(Into::into)
            .collect();
        if staged {
            args.push("--cached".into());
        }
        if let Some(p) = path {
            args.push("--".into());
            args.push(p.into());
        }
        Ok(run_git_ok(&self.root, &args)?.stdout_string())
    }

    /// `git diff --stat` of everything changed relative to HEAD (staged + unstaged).
    pub fn diff_stat(&self) -> Result<String> {
        let args: &[&str] = if self.has_head() {
            &["diff", "--stat", "--no-color", "HEAD"]
        } else {
            &["diff", "--stat", "--no-color", "--cached"]
        };
        Ok(run_git_ok(&self.root, args)?.stdout_string())
    }

    /// Run an action non-interactively and capture its output. A non-zero exit is reported in
    /// the returned [`CommandOutput`], not as an error. Prompts for credentials are disabled.
    pub fn run(&self, action: &GitAction) -> Result<CommandOutput> {
        action.validate()?;
        let argv = action.argv();
        let raw = run_git(&self.root, &argv[1..], action.timeout())?;
        Ok(CommandOutput {
            code: raw.code,
            stdout: raw.stdout_string(),
            stderr: raw.stderr_string(),
        })
    }

    /// Collect warnings and a diff summary to show before running a sensitive action.
    pub fn preflight(&self, action: &GitAction) -> Result<Preflight> {
        action.validate()?;
        let st = self.status()?;
        let mut pf = Preflight::default();
        let dirty = st.is_dirty();

        match action {
            GitAction::Commit { .. } => {
                pf.requires_confirmation = true;
                if !st.conflicted.is_empty() {
                    pf.warnings.push(format!(
                        "{} file(s) have unresolved conflicts; commit will fail",
                        st.conflicted.len()
                    ));
                }
                if st.staged.is_empty() {
                    pf.warnings
                        .push(if st.unstaged.is_empty() && st.untracked.is_empty() {
                            "nothing to commit".to_owned()
                        } else {
                            "nothing is staged (run add first)".to_owned()
                        });
                }
                if st.detached {
                    pf.warnings
                        .push("HEAD is detached; the commit will not be on a branch".into());
                }
                pf.diff_stat = self.stat(&["diff", "--stat", "--no-color", "--cached"]);
            }
            GitAction::Push => {
                pf.requires_confirmation = true;
                if st.detached {
                    pf.warnings.push("HEAD is detached; nothing to push".into());
                } else if st.upstream.is_none() {
                    pf.warnings
                        .push("branch has no upstream; push needs --set-upstream".into());
                } else {
                    if st.behind > 0 {
                        pf.warnings.push(format!(
                            "branch is {} commit(s) behind upstream; push will likely be rejected",
                            st.behind
                        ));
                    }
                    if st.ahead == 0 {
                        pf.warnings.push("no local commits to push".into());
                    }
                    pf.diff_stat =
                        self.stat(&["diff", "--stat", "--no-color", "@{upstream}...HEAD"]);
                }
            }
            GitAction::Pull => {
                pf.requires_confirmation = true;
                if st.upstream.is_none() {
                    pf.warnings
                        .push("branch has no upstream to pull from".into());
                } else {
                    if dirty {
                        pf.warnings
                            .push("working tree has uncommitted changes; pull may conflict".into());
                    }
                    if st.ahead > 0 && st.behind > 0 {
                        pf.warnings.push(format!(
                            "branch has diverged ({} ahead, {} behind); pull will merge or rebase",
                            st.ahead, st.behind
                        ));
                    }
                    pf.diff_stat =
                        self.stat(&["diff", "--stat", "--no-color", "HEAD...@{upstream}"]);
                }
            }
            GitAction::Switch { .. } | GitAction::Checkout { .. } => {
                if dirty {
                    pf.requires_confirmation = true;
                    pf.warnings.push(
                        "working tree has uncommitted changes that may be overwritten or carried over"
                            .into(),
                    );
                    pf.diff_stat = self.diff_stat().ok().filter(|s| !s.trim().is_empty());
                }
            }
            GitAction::AddAll
            | GitAction::Status
            | GitAction::Fetch
            | GitAction::Log
            | GitAction::Diff => {}
        }
        Ok(pf)
    }

    fn has_head(&self) -> bool {
        run_git(
            &self.root,
            &["rev-parse", "--verify", "-q", "HEAD"],
            LOCAL_TIMEOUT,
        )
        .map(|r| r.success())
        .unwrap_or(false)
    }

    /// Best-effort `--stat` output; `None` on failure or empty output.
    fn stat(&self, args: &[&str]) -> Option<String> {
        let raw = run_git(&self.root, args, LOCAL_TIMEOUT).ok()?;
        if !raw.success() {
            return None;
        }
        let s = raw.stdout_string();
        (!s.trim().is_empty()).then_some(s)
    }
}
