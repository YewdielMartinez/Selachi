//! `chat_core`: the model of Selachi's chat mode (PLAN section 14), UI-agnostic (no gpui, no
//! tokio: std threads and `futures` channels, like the other service crates).
//!
//! ```text
//! ChatStore ──create/list/open──► ThreadRuntime ──send/approve/switch──► agent process
//!    │  chat.db (SQLite, WAL,          │  worker thread (commands)         (agents_driver)
//!    │  single writer thread)          │  event pump (AgentEvent stream)
//!    │                                 ▼
//!    └── updates() ◄── ThreadUpdate ◄── ThreadState (messages, tool groups, plan, approvals,
//!        (<= 1 per 16 ms per thread)                usage, cost, context %, rate limits)
//! ```
//!
//! * [`ChatStore`]: threads (create with optional git worktree, list/group, open, archive,
//!   delete with a preview), the `updates()` stream and shared services.
//! * [`ThreadRuntime`] (alias [`ThreadHandle`]): live thread. `send`, `interrupt`, `approve`,
//!   `switch_agent_or_model`, `set_permission_mode`, `close`; read with `with_state`.
//! * [`ThreadState`]: the folded conversation. See [`state`] for the block model.
//! * [`DriverFactory`] / [`ProcessHooks`]: seams for spawning agents and registering process
//!   owners with the host (`proc_svc`).
//!
//! Nothing is stored that could be a secret: messages and tool runs are redacted
//! (`store::redact`) before they reach `chat.db`, and tool inputs are capped at 4 KB.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod chat_store;
mod db;
mod error;
pub mod handoff;
mod hooks;
mod model;
mod runtime;
pub mod state;

pub use chat_store::{
    slugify, worktree_name, ChatStore, ChatStoreBuilder, DeleteOptions, DeletePreview, DeleteReport,
    WorktreePreview,
};
pub use db::{default_db_path, latest_version, migrate, ChatDb, StoredApproval, StoredMessage, StoredToolRun};
pub use error::{ChatError, Result};
pub use hooks::{
    AgentControl, DefaultDriverFactory, DriverFactory, JobHandle, OwnerInfo, ProcessHooks, SpawnedAgent,
};
pub use model::{
    agent_label, group_by_project, now_ms, should_checkpoint, ApprovalDecision, Archived, CostSource, Role,
    ThreadFilter, ThreadGroup, ThreadGroups, ThreadId, ThreadLink, ThreadRecord, ThreadSpec, ThreadStatus,
    UNTITLED,
};
pub use runtime::{ThreadHandle, ThreadRuntime, ThreadUpdate, MIN_UPDATE_INTERVAL};
pub use state::{
    classify, Block, Changed, CheckpointRef, ContextUsage, CostPolicy, ExploreCounts, ExploreGroup, Message,
    MessageCost, MessageUsage, Plan, PlanItem, ThinkingBlock, ThreadState, ToolClass, ToolRun, ToolStatus,
    UsageTotals, ApprovalRow, ApprovalState, MAX_TOOL_INPUT_BYTES,
};
