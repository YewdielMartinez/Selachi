//! `git_svc`: UI-agnostic git service for Selachi.
//!
//! State is read through the real `git` CLI (`status --porcelain=v2 -z --branch`, `log`,
//! `diff`), so user config, hooks and fsmonitor are respected. Actions are typed
//! ([`GitAction`]) and expose their exact argv so the UI can run them visibly in a terminal;
//! non-interactive ones can also be run directly with [`Repo::run`]. [`Repo::watch`] provides a
//! debounced, notification-based (no polling) change stream.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod action;
mod commits;
mod error;
mod exec;
mod repo;
mod status;
mod watch;

pub use action::{CommandOutput, GitAction, Preflight};
pub use commits::{parse_log, Commit};
pub use error::{Error, Result};
pub use repo::Repo;
pub use status::{parse_status, ChangeKind, FileChange, RepoStatus};
pub use watch::WatchHandle;
