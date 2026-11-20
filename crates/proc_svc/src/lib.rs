//! `proc_svc`: process monitor for agent-launched processes (PLAN.md section 15).
//!
//! UI-agnostic (no GPUI). Windows only; on other targets the crate is empty.
//!
//! * [`JobGroup`]: Job Objects, member listing, zero-polling [`JobEvent`]s.
//! * [`snapshot`]: ToolHelp32 + per-process metrics and command line.
//! * [`CpuSampler`]: CPU % from snapshot deltas.
//! * [`listening_ports`]: TCP listeners / UDP sockets with owning pid.
//! * [`Tracker`]: owner -> job attribution, orphans, port zombies, heuristics.
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
mod job;
mod ports;
mod runtime;
mod snapshot;
mod sys;
mod tracker;

pub use actions::{
    SafetyPolicy, TerminateOptions, TerminateOutcome, check_safe, collect_tree, console_break,
    terminate_job, terminate_job_with, terminate_tree, terminate_tree_with,
};
pub use cpu::{CpuSampler, logical_cores};
pub use error::{Error, RefuseReason, Result};
pub use job::{JobEvent, JobGroup, JobOptions, resume_process};
pub use ports::{PortInfo, PortState, Proto, listening_ports, ports_by_pid};
pub use runtime::{Runtime, RuntimeKind, classify};
pub use snapshot::{
    ProcInfo, SnapshotOptions, snapshot, snapshot_with, start_time_of, try_snapshot,
};
pub use tracker::{
    Attribution, OwnerId, OwnerMeta, OwnerReport, ProcState, Tracker, TrackedProc, UntrackedProc,
    heuristic_from,
};
