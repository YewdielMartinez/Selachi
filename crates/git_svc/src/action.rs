//! Typed git actions. The UI can either run [`GitAction::argv`] visibly in a terminal (PLAN §7),
//! or call [`crate::Repo::run`] for non-interactive execution.

use std::time::Duration;

use crate::error::{Error, Result};
use crate::exec::{LOCAL_TIMEOUT, NETWORK_TIMEOUT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitAction {
    Status,
    AddAll,
    Commit { message: String },
    Push,
    Pull,
    Fetch,
    Switch { branch: String },
    Checkout { target: String },
    Log,
    Diff,
}

impl GitAction {
    /// The exact argv to execute, including the leading `"git"`.
    /// The caller is responsible for quoting when composing a shell line.
    pub fn argv(&self) -> Vec<String> {
        let parts: Vec<&str> = match self {
            Self::Status => vec!["git", "status", "--short", "--branch"],
            Self::AddAll => vec!["git", "add", "-A"],
            Self::Commit { message } => vec!["git", "commit", "-m", message],
            Self::Push => vec!["git", "push"],
            Self::Pull => vec!["git", "pull"],
            Self::Fetch => vec!["git", "fetch", "--prune"],
            Self::Switch { branch } => vec!["git", "switch", branch],
            Self::Checkout { target } => vec!["git", "checkout", target],
            Self::Log => vec![
                "git",
                "log",
                "--oneline",
                "--graph",
                "--decorate",
                "-n",
                "20",
            ],
            Self::Diff => vec!["git", "diff"],
        };
        parts.into_iter().map(str::to_owned).collect()
    }

    /// Statically sensitive actions: they publish, rewrite or merge state and deserve a
    /// preview + confirmation. `Switch`/`Checkout` are reported sensitive here because the
    /// dirty-tree check needs the repo; use [`crate::Repo::preflight`] and
    /// [`Preflight::requires_confirmation`] for the precise answer.
    pub fn is_sensitive(&self) -> bool {
        matches!(
            self,
            Self::Commit { .. }
                | Self::Push
                | Self::Pull
                | Self::Switch { .. }
                | Self::Checkout { .. }
        )
    }

    /// Reject arguments that git would parse as options (`-f`, `--orphan`, ...) or that are empty.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Commit { message } if message.trim().is_empty() => {
                Err(Error::InvalidArgument("empty commit message".into()))
            }
            Self::Switch { branch: v } | Self::Checkout { target: v } => {
                if v.trim().is_empty() {
                    Err(Error::InvalidArgument("empty branch/target".into()))
                } else if v.starts_with('-') {
                    Err(Error::InvalidArgument(format!(
                        "{v:?} looks like an option, not a branch/target"
                    )))
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn timeout(&self) -> Duration {
        match self {
            Self::Push | Self::Pull | Self::Fetch => NETWORK_TIMEOUT,
            _ => LOCAL_TIMEOUT,
        }
    }
}

/// Pre-execution information for the confirmation UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    pub warnings: Vec<String>,
    /// `git diff --stat` style summary of what the action affects, when meaningful.
    pub diff_stat: Option<String>,
    /// Whether the UI should ask for confirmation before running (see PLAN §7).
    pub requires_confirmation: bool,
}

/// Result of [`crate::Repo::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_shapes() {
        assert_eq!(GitAction::AddAll.argv(), ["git", "add", "-A"]);
        assert_eq!(
            GitAction::Commit {
                message: "fix: a b".into()
            }
            .argv(),
            ["git", "commit", "-m", "fix: a b"]
        );
        assert_eq!(
            GitAction::Switch {
                branch: "dev".into()
            }
            .argv(),
            ["git", "switch", "dev"]
        );
        assert!(GitAction::Push.argv().starts_with(&["git".to_string()]));
    }

    #[test]
    fn sensitivity() {
        assert!(GitAction::Push.is_sensitive());
        assert!(GitAction::Pull.is_sensitive());
        assert!(GitAction::Commit {
            message: "x".into()
        }
        .is_sensitive());
        assert!(GitAction::Checkout { target: "x".into() }.is_sensitive());
        assert!(!GitAction::Status.is_sensitive());
        assert!(!GitAction::Fetch.is_sensitive());
        assert!(!GitAction::AddAll.is_sensitive());
        assert!(!GitAction::Log.is_sensitive());
        assert!(!GitAction::Diff.is_sensitive());
    }

    #[test]
    fn validation() {
        assert!(GitAction::Switch {
            branch: "--detach".into()
        }
        .validate()
        .is_err());
        assert!(GitAction::Checkout { target: "".into() }
            .validate()
            .is_err());
        assert!(GitAction::Commit {
            message: "  ".into()
        }
        .validate()
        .is_err());
        assert!(GitAction::Switch {
            branch: "feature/x".into()
        }
        .validate()
        .is_ok());
    }
}
