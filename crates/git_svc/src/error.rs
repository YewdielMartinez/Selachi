use std::path::PathBuf;
use std::time::Duration;

/// Errors produced by `git_svc`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The `git` executable could not be found / spawned.
    #[error("git executable not found: {0}")]
    GitNotFound(#[source] std::io::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// git exited with a non-zero status for a command whose output we needed.
    #[error("`git {args}` failed (exit {code:?}): {stderr}")]
    Git {
        args: String,
        code: Option<i32>,
        stderr: String,
    },

    /// git did not finish within the allotted time and was killed.
    #[error("`git {args}` timed out after {timeout:?}")]
    Timeout { args: String, timeout: Duration },

    /// Output of git could not be understood.
    #[error("could not parse git output: {0}")]
    Parse(String),

    /// An argument (branch name, target...) was rejected before running git.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("path is not valid for watching: {0}")]
    BadPath(PathBuf),

    #[error("file watcher error: {0}")]
    Watch(#[from] notify::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
