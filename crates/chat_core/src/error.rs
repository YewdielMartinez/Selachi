//! Error type of `chat_core`.

use std::path::PathBuf;

/// Everything that can go wrong in `chat_core`.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("sqlite error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git error: {0}")]
    Git(#[from] git_svc::Error),
    #[error("agent driver error: {0}")]
    Driver(#[from] agents_driver::DriverError),
    #[error("store error: {0}")]
    Store(#[from] store::StoreError),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    /// Unknown thread / message / approval.
    #[error("not found: {0}")]
    NotFound(String),
    /// A caller mistake (bad argument, wrong state).
    #[error("invalid: {0}")]
    Invalid(String),
    /// `use_worktree` (or a checkpoint) needs a git repository and `{0}` is not inside one.
    #[error("not inside a git repository: {0}")]
    NotAGitRepo(PathBuf),
    /// The thread runtime was closed.
    #[error("the thread is closed")]
    Closed,
    /// The chat.db writer thread is gone.
    #[error("chat.db writer thread is gone")]
    WriterClosed,
    /// The database was written by a newer build.
    #[error("chat.db is schema v{found}, newer than this build (v{supported}); update Selachi")]
    NewerSchema { found: u32, supported: u32 },
}

pub type Result<T> = std::result::Result<T, ChatError>;
