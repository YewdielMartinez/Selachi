//! `ui_git`: the git panel (PLAN section 7, backlog A8 worktrees and A10 checkpoints).
//!
//! * [`GitModel`]: the entity the app owns. Reads the repository through `git_svc` on the
//!   background executor, **only while the panel is visible** (the `Repo::watch` handle is
//!   dropped when it is hidden), with one read in flight and one queued. Sensitive operations go
//!   through a preflight card and an explicit confirmation ([`Gate`]).
//! * [`GitPanelProvider`]: the `PanelProvider` with id `"git"` for the right, sidebar and main
//!   zones. Compact widths collapse to tabs (Cambios | Historial | Checkpoints).
//! * [`GitPanelEvent`]: what the panel asks of the host (run a command in a terminal, open a
//!   file or a diff, send a diff to an agent, a worktree was picked).
//! * [`thumbnail`]: the 1-bit wireframe for the layout panel.
//!
//! # Integration
//!
//! ```ignore
//! ui_git::init(cx);                                   // binds the text field keys, once
//! let git = cx.new(|_| ui_git::GitModel::new());
//! registry.register(ui_git::GitPanelProvider::new(git.clone()));
//! cx.subscribe(&git, |_, ev: &ui_git::GitPanelEvent, cx| match ev {
//!     GitPanelEvent::RunInTerminal { argv, cwd } => { /* open a tab / type the command */ }
//!     GitPanelEvent::OpenFile { path, line } => { /* viewer */ }
//!     GitPanelEvent::OpenDiff { path } => { /* viewer diff */ }
//!     GitPanelEvent::AskAgent { context_markdown } => { /* agent composer (redact secrets) */ }
//!     GitPanelEvent::WorktreeSelected { path } => { /* optional: cd the terminal */ }
//! }).detach();
//! ```
//!
//! The model follows the project root the shell passes in `PanelCtx` (`project_root`, else
//! `cwd`). Visibility is implicit (a painted view is a visible panel; dropping the last view
//! hides it) or explicit with [`GitModel::set_visible`].
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cmd;
mod data;
mod diffview;
mod fmt;
mod gate;
mod model;
mod sections;
mod view;
mod widgets;

pub use cmd::{GitOp, Slot, commit_message, stage_argv, unstage_argv};
pub use data::{DiffTarget, GitData, HISTORY_LEN};
pub use diffview::{DiffPreview, DiffRow, FileDiff, PreviewLine, parse_unified};
pub use gate::{Gate, Outcome, Review, ReviewLine, translate_warning};
pub use model::{
    CheckpointFiles, CheckpointView, DiffSlot, DiffState, ExecMode, GitModel, GitPanelEvent, Load, OpFinished, Stats,
    WATCH_DEBOUNCE,
};
pub use sections::{FileRow, Section, build_sections};
pub use view::{GitPanel, GitPanelProvider, Layout, PANEL_ID, Tab, WIDE_FROM_PX, init, layout_for, path_budget, thumbnail};

#[cfg(test)]
mod tests;
