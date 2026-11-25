//! `proc_svc`: process monitor for agent-launched processes (PLAN.md section 15).
//!
//! UI-agnostic (no GPUI). Windows only; on other targets the crate is empty.
//!
//! * [`JobGroup`]: Job Objects, member listing, zero-polling [`JobEvent`]s.
//! * [`snapshot`]: ToolHelp32 + per-process metrics and command line.
//! * [`CpuSampler`]: CPU % from snapshot deltas.
//! * [`listening_ports`]: TCP listeners / UDP sockets with owning pid.
//! * [`Tracker`]: owner -> job attribution, orphans, port zombies, heuristics.
//!   Job-less owners (Store `pwsh`) use [`TrackingMode::TreeOnly`] (process-tree walk).
//! * [`classify`]: runtime kind (`node`/`vite`, `dotnet`, ...).
//! * [`terminate_tree`] / [`terminate_job`]: graceful then forced, with safety checks.
//!
//! Cost model: nothing runs in the background except the optional completion
//! port thread of [`JobGroup::notifications`] (blocked in the kernel when idle).
//! Snapshots and port tables are taken only when the caller asks.
#![cfg(windows)]

mod actions;
mod cpu;
mod error;
mod external;
mod job;
mod ports;
mod runtime;
mod snapshot;
mod sys;
mod tracker;

pub use actions::{
    check_safe, collect_tree, console_break, terminate_job, terminate_job_with, terminate_tree,
    terminate_tree_with, SafetyPolicy, TerminateOptions, TerminateOutcome,
};
pub use cpu::{logical_cores, CpuSampler};
pub use error::{Error, RefuseReason, Result};
pub use external::{
    extra_agent_stems, find_external_agents, find_external_agents_cfg, find_external_agents_with,
    match_agent_exe, parse_agent_cli_args, ExternalAgentProc, ParsedAgentArgs, KNOWN_AGENTS,
    MAX_EXTRA_AGENTS,
};
pub use job::{resume_process, JobEvent, JobGroup, JobOptions};
pub use ports::{listening_ports, ports_by_pid, PortInfo, PortState, Proto};
pub use runtime::{classify, Runtime, RuntimeKind};
pub use snapshot::{
    snapshot, snapshot_with, start_time_of, try_snapshot, ProcInfo, SnapshotOptions,
};
pub use tracker::{
    heuristic_from, Attribution, OwnerId, OwnerMeta, OwnerReport, ProcState, TrackedProc, Tracker,
    TrackingMode, UntrackedProc, TREE_POLL_INTERVAL,
};
