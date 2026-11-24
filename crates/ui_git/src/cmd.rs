//! Typed operations of the panel and the exact `git` argv they run.
//!
//! `git_svc`'s `exec` helpers are crate-private, so the stage / unstage commands (which
//! [`git_svc::GitAction`] does not model) are built here as argv lists and run with
//! [`run_git`], a minimal `std::process` wrapper (no console window, no prompts, no optional
//! locks, `core.quotepath=false`). Everything else goes through [`git_svc::Repo`].

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use git_svc::GitAction;

/// Everything the panel can do to a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitOp {
    /// `git add -- <path>`.
    Stage(PathBuf),
    /// `git restore --staged -- <path>` (`git rm --cached` on an unborn branch).
    Unstage {
        path: PathBuf,
        unborn: bool,
    },
    /// `git add -A`.
    StageAll,
    /// `git restore --staged -- .`.
    UnstageAll {
        unborn: bool,
    },
    /// A typed `git_svc` action: commit, push, pull, fetch.
    Action(GitAction),
    /// `git commit -m <message>` and, when it worked, `git push`.
    CommitAndPush {
        message: String,
    },
    /// `git push --set-upstream origin <branch>`.
    PublishBranch {
        branch: String,
    },
    CreateWorktree {
        name: String,
    },
    RemoveWorktree {
        path: PathBuf,
        force: bool,
    },
    CreateCheckpoint {
        label: String,
    },
    RestoreCheckpoint {
        id: String,
    },
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// `git add -- <path>`.
pub fn stage_argv(path: &Path) -> Vec<String> {
    let mut v = argv(&["git", "add", "--"]);
    v.push(path.to_string_lossy().into_owned());
    v
}

/// `git restore --staged -- <path>` (or `git rm --cached` when HEAD does not exist yet).
pub fn unstage_argv(path: &Path, unborn: bool) -> Vec<String> {
    let mut v = if unborn {
        argv(&["git", "rm", "--cached", "-r", "--quiet", "--"])
    } else {
        argv(&["git", "restore", "--staged", "--"])
    };
    v.push(path.to_string_lossy().into_owned());
    v
}

/// Where the confirmation / result of an operation is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Under the sync row at the top (push, pull, worktrees).
    Header,
    /// Under the commit box.
    Commit,
    /// In the checkpoints section.
    Checkpoints,
}

impl GitOp {
    /// Commands that publish, rewrite, delete or overwrite: they never run without an explicit
    /// confirmation (see [`crate::gate::Gate`]).
    pub fn is_sensitive(&self) -> bool {
        match self {
            GitOp::Action(a) => a.is_sensitive(),
            GitOp::CommitAndPush { .. }
            | GitOp::PublishBranch { .. }
            | GitOp::RemoveWorktree { .. }
            | GitOp::RestoreCheckpoint { .. } => true,
            GitOp::Stage(_)
            | GitOp::Unstage { .. }
            | GitOp::StageAll
            | GitOp::UnstageAll { .. }
            | GitOp::CreateWorktree { .. }
            | GitOp::CreateCheckpoint { .. } => false,
        }
    }

    /// The command lines this operation runs, in order, for display and for running them visibly
    /// in a terminal. Empty for operations that are not a git command line (worktrees,
    /// checkpoints).
    pub fn argv_steps(&self) -> Vec<Vec<String>> {
        match self {
            GitOp::Stage(p) => vec![stage_argv(p)],
            GitOp::Unstage { path, unborn } => vec![unstage_argv(path, *unborn)],
            GitOp::StageAll => vec![GitAction::AddAll.argv()],
            GitOp::UnstageAll { unborn } => vec![unstage_argv(Path::new("."), *unborn)],
            GitOp::Action(a) => vec![a.argv()],
            GitOp::CommitAndPush { message } => {
                vec![
                    GitAction::Commit {
                        message: message.clone(),
                    }
                    .argv(),
                    GitAction::Push.argv(),
                ]
            }
            GitOp::PublishBranch { branch } => {
                vec![argv(&["git", "push", "--set-upstream", "origin", branch])]
            }
            GitOp::CreateWorktree { .. }
            | GitOp::RemoveWorktree { .. }
            | GitOp::CreateCheckpoint { .. }
            | GitOp::RestoreCheckpoint { .. } => Vec::new(),
        }
    }

    /// Short Spanish title ("Commit", "Push"...).
    pub fn title(&self) -> &'static str {
        match self {
            GitOp::Stage(_) | GitOp::StageAll => "Preparar",
            GitOp::Unstage { .. } | GitOp::UnstageAll { .. } => "Quitar de staged",
            GitOp::Action(GitAction::Commit { .. }) => "Commit",
            GitOp::Action(GitAction::Push) => "Push",
            GitOp::Action(GitAction::Pull) => "Pull",
            GitOp::Action(GitAction::Fetch) => "Fetch",
            GitOp::Action(_) => "Git",
            GitOp::CommitAndPush { .. } => "Commit y push",
            GitOp::PublishBranch { .. } => "Publicar rama",
            GitOp::CreateWorktree { .. } => "Nuevo worktree",
            GitOp::RemoveWorktree { .. } => "Eliminar worktree",
            GitOp::CreateCheckpoint { .. } => "Crear checkpoint",
            GitOp::RestoreCheckpoint { .. } => "Restaurar checkpoint",
        }
    }

    /// Where the confirmation / result of this operation is painted.
    pub fn slot(&self) -> Slot {
        match self {
            GitOp::Action(GitAction::Commit { .. }) | GitOp::CommitAndPush { .. } => Slot::Commit,
            GitOp::CreateCheckpoint { .. } | GitOp::RestoreCheckpoint { .. } => Slot::Checkpoints,
            _ => Slot::Header,
        }
    }

    /// Rejects what can never work (empty message, invalid worktree name). Option-looking paths
    /// are safe because every path goes after `--`.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            GitOp::Action(a) => a.validate().map_err(|e| e.to_string()),
            GitOp::CommitAndPush { message } => GitAction::Commit {
                message: message.clone(),
            }
            .validate()
            .map_err(|e| e.to_string()),
            GitOp::CreateWorktree { name } => {
                git_svc::validate_worktree_name(name).map_err(|e| e.to_string())
            }
            GitOp::PublishBranch { branch }
                if branch.trim().is_empty() || branch.starts_with('-') =>
            {
                Err("nombre de rama no válido".into())
            }
            GitOp::Stage(p) | GitOp::Unstage { path: p, .. } if p.as_os_str().is_empty() => {
                Err("ruta vacía".into())
            }
            _ => Ok(()),
        }
    }
}

/// Commit message from the two fields: `subject` plus an optional body after a blank line.
pub fn commit_message(subject: &str, body: &str) -> String {
    let (s, b) = (subject.trim(), body.trim());
    if b.is_empty() {
        s.to_string()
    } else {
        format!("{s}\n\n{b}")
    }
}

/// Output of [`run_git`].
#[derive(Debug, Clone)]
pub struct GitRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GitRun {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs `git <args>` in `cwd` and waits up to `timeout`. No console window, no credential
/// prompts, no optional locks, paths taken literally (`GIT_LITERAL_PATHSPECS`).
pub fn run_git<S: AsRef<OsStr>>(
    cwd: &Path,
    args: &[S],
    timeout: Duration,
) -> Result<GitRun, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotepath=false")
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("no se pudo ejecutar git: {e}"))?;
    let drain = |mut pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = pipe.as_mut() {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err_t = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break Some(s),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    match status {
        Some(s) => Ok(GitRun {
            code: s.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
        }),
        None => Err(format!(
            "git tardó más de {} s y se canceló",
            timeout.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_and_unstage_argv() {
        assert_eq!(
            stage_argv(Path::new("src/a b.rs")),
            ["git", "add", "--", "src/a b.rs"]
        );
        assert_eq!(
            unstage_argv(Path::new("src/a.rs"), false),
            ["git", "restore", "--staged", "--", "src/a.rs"]
        );
        assert_eq!(
            unstage_argv(Path::new("src/a.rs"), true),
            ["git", "rm", "--cached", "-r", "--quiet", "--", "src/a.rs"]
        );
        // A file named like an option is still a path.
        assert_eq!(stage_argv(Path::new("-rf"))[2], "--");
    }

    #[test]
    fn ops_expose_the_exact_argv() {
        assert_eq!(GitOp::StageAll.argv_steps(), [["git", "add", "-A"]]);
        assert_eq!(
            GitOp::UnstageAll { unborn: false }.argv_steps(),
            [["git", "restore", "--staged", "--", "."]]
        );
        let steps = GitOp::CommitAndPush {
            message: "feat: x".into(),
        }
        .argv_steps();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0], ["git", "commit", "-m", "feat: x"]);
        assert_eq!(steps[1], ["git", "push"]);
        assert_eq!(
            GitOp::PublishBranch {
                branch: "dev".into()
            }
            .argv_steps(),
            [["git", "push", "--set-upstream", "origin", "dev"]]
        );
        assert!(
            GitOp::CreateCheckpoint { label: "x".into() }
                .argv_steps()
                .is_empty()
        );
    }

    #[test]
    fn sensitivity_matches_the_service() {
        let commit = GitOp::Action(GitAction::Commit {
            message: "m".into(),
        });
        for op in [
            commit,
            GitOp::Action(GitAction::Push),
            GitOp::Action(GitAction::Pull),
            GitOp::CommitAndPush {
                message: "m".into(),
            },
            GitOp::PublishBranch { branch: "b".into() },
            GitOp::RemoveWorktree {
                path: "x".into(),
                force: false,
            },
            GitOp::RestoreCheckpoint { id: "1".into() },
        ] {
            assert!(op.is_sensitive(), "{op:?}");
        }
        for op in [
            GitOp::Stage("a".into()),
            GitOp::Unstage {
                path: "a".into(),
                unborn: false,
            },
            GitOp::StageAll,
            GitOp::UnstageAll { unborn: false },
            GitOp::Action(GitAction::Fetch),
            GitOp::CreateWorktree { name: "x".into() },
            GitOp::CreateCheckpoint { label: "x".into() },
        ] {
            assert!(!op.is_sensitive(), "{op:?}");
        }
    }

    #[test]
    fn commit_message_joins_subject_and_body() {
        assert_eq!(commit_message(" fix: a ", ""), "fix: a");
        assert_eq!(
            commit_message("fix: a", "  more\ntext "),
            "fix: a\n\nmore\ntext"
        );
    }

    #[test]
    fn validation() {
        assert!(
            GitOp::Action(GitAction::Commit {
                message: "  ".into()
            })
            .validate()
            .is_err()
        );
        assert!(
            GitOp::CommitAndPush {
                message: String::new()
            }
            .validate()
            .is_err()
        );
        assert!(
            GitOp::CreateWorktree { name: "con".into() }
                .validate()
                .is_err()
        );
        assert!(
            GitOp::CreateWorktree {
                name: "feature-x".into()
            }
            .validate()
            .is_ok()
        );
        assert!(
            GitOp::PublishBranch {
                branch: "--force".into()
            }
            .validate()
            .is_err()
        );
    }
}
