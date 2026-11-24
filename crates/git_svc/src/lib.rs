//! `git_svc`: UI-agnostic git service for Selachi.
//!
//! State is read through the real `git` CLI (`status --porcelain=v2 -z --branch`, `log`,
//! `diff`), so user config, hooks and fsmonitor are respected. Actions are typed
//! ([`GitAction`]) and expose their exact argv so the UI can run them visibly in a terminal;
//! non-interactive ones can also be run directly with [`Repo::run`]. [`Repo::watch`] provides a
//! debounced, notification-based (no polling) change stream.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod action;
mod checkpoint;
mod commits;
mod draft;
mod error;
mod exec;
mod gh;
mod pathutil;
mod repo;
mod status;
mod watch;
mod worktree;

pub use action::{CommandOutput, GitAction, Preflight};
pub use checkpoint::{
    Checkpoint, CheckpointChange, DeltaKind, RestoreMode, RestorePlan, RestoreReport,
};
pub use commits::{parse_log, Commit};
pub use draft::{
    cap_and_redact, detect_language, draft_prompt, split_message, DraftInput, DraftKind, Lang,
    MAX_DIFF_BYTES, RECENT_SUBJECTS,
};
pub use error::{Error, Result};
pub use gh::{
    checks, clean_log, failed_check_log, gh_available, parse_check_url, parse_checks,
    parse_pr_list, parse_time, pick_pr, pr_for_branch, tail_lines, Check, CheckBucket,
    CheckConclusion, CheckStatus, CheckTotals, GhClient, Mergeable, NewPr, PrInfo, PrState,
    ReviewDecision, MAX_LOG_BYTES,
};
pub use repo::Repo;
pub use status::{parse_status, ChangeKind, FileChange, RepoStatus};
pub use watch::WatchHandle;
pub use worktree::{
    validate_worktree_name, CreatedWorktree, Editor, NewWorktree, Worktree, WorktreeLocation,
};
