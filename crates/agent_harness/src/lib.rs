//! `agent_harness`: Selachi as a harness for other coding agents (PLAN section 20, wave 6a.1).
//!
//! A session (Claude Code in a terminal tab, a chat thread, any agent with a shell) delegates a
//! subtask to another agent (Codex, Gemini, OpenCode, Antigravity, Claude) through the console
//! binary `selachi-agent`; Selachi launches, isolates, limits and measures that run:
//!
//! * **Child thread**: the run is a thread in `chat.db` linked to its parent
//!   (`chat_core::ThreadLink`: `parent_thread_id`, `parent_tab`, `depth`, `delegated_by`), so the
//!   app can show the lineage.
//! * **Containment**: the agent runs in a Job Object with kill-on-close (`proc_svc`), optionally in
//!   its own git worktree, always after a git checkpoint (`git_svc`), so `diff` / `apply` are exact.
//! * **Limits**: permission never above the caller's (`SELACHI_PARENT_PERMISSION`, default
//!   read-only; `full` never delegable), depth <= 2 (`SELACHI_DELEGATION_DEPTH`), at most 3 runs at once
//!   (lock files), 15 min timeout, per-run and daily token budgets, refusal when the agent's weekly
//!   usage is over the threshold (`agents_svc`).
//! * **Approvals**: the child never approves itself; requests go to a Selachi window over a named
//!   pipe ([`approvals`], protocol v1) or are denied with a reason.
//! * **MCP**: `selachi-agent mcp` serves the same engine as typed tools over stdio ([`mcp`]).
//! * **Secrets**: the prompt and everything that comes back go through `store::redact`.
//!
//! # `[delegation]` in `config.toml`
//!
//! Read directly from the file (it is not in `store::Config` yet, the integrator moves it there):
//!
//! ```toml
//! [delegation]
//! enabled = true
//! allowed_agents = []                 # empty = all
//! max_permission = "accept_edits"     # read_only | ask | accept_edits
//! max_concurrent = 3
//! max_depth = 1
//! timeout_secs = 900
//! daily_token_budget = 2000000        # billable tokens (input + output + cache writes); 0 = unlimited
//! weekly_usage_threshold_percent = 90 # 0 = never refuse
//! approval_timeout_secs = 120
//! ```
//!
//! ```no_run
//! use agent_harness::{Harness, Permission, RunRequest};
//! let harness = Harness::builder().build().unwrap();
//! let mut req = RunRequest::new("codex", "review the auth tests", ".");
//! req.permission = Permission::ReadOnly;
//! let result = harness.run(req);
//! println!("{}", result.to_json());
//! ```
#![cfg(windows)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agents;
pub mod apply;
pub mod approvals;
pub mod cli;
pub mod control;
pub mod fake;
pub mod jobs;
pub mod mcp;
pub mod output;
pub mod policy;
pub mod profiles;
pub mod registry;
pub mod run;
pub mod slots;
pub mod usage;

pub use agents::{
    agent_name, canonical_name, list_agents, AgentList, AgentResolver, SystemResolver,
};
pub use apply::{ApplyPlan, ApplyReport, Confirmation, Risk};
pub use approvals::{
    default_pipe_path, pipe_path_for, ApprovalBroker, ApprovalRequest, ClientMessage, Decision,
    DenyAll, PipeBroker, ServerMessage, Verdict, PROTOCOL_VERSION,
};
pub use control::{CancelOutcome, StatusOut};
pub use output::{DiffStat, RunResult, RunStatus, UsageOut};
pub use policy::{Caller, DelegationConfig, Permission, Policy, Refusal, RefusalCode};
pub use profiles::{Library, Profile, ProfileError};
pub use registry::{Registry, RunLimits, RunRecord};
pub use run::{Harness, HarnessBuilder, RunRequest};
pub use usage::{AgentsSvcProbe, LimitRow, UsageProbe};
