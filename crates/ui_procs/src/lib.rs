//! `ui_procs`: the process monitor panel (PLAN.md section 15, backlog A7).
//!
//! * [`ProcsModel`]: the entity the app owns. Fed by a shared `proc_svc::Tracker`
//!   ([`ProcsModel::with_tracker`]) or any [`ProcSource`]. Samples every 2 s **only while the panel
//!   is visible**, refreshes (throttled) on job events, does nothing when hidden. Exposes
//!   [`ProcsModel::orphan_summary`] for the status-bar pill.
//! * [`ProcsPanelProvider`]: the `PanelProvider` with id `"procs"` for the sidebar, right, main
//!   and bottom zones. Agent -> tab -> process tree, a "Puertos" sub-view, "Detener" per process
//!   and "Limpiar huérfanos", always behind an inline confirmation that lists exactly what will be
//!   terminated ([`Dialog`], [`StopPlan`]).
//! * [`Assets`]: `ui_kit::Assets` plus the runtime pixel icons (`assets/runtime/*.svg`).
//! * [`thumbnail`]: the 1-bit wireframe for the layout panel.
//!
//! ```ignore
//! let tracker: SharedTracker = Arc::new(Mutex::new(Tracker::new()));
//! let procs = cx.new(|_| ProcsModel::with_tracker(tracker.clone()));
//! registry.register(ProcsPanelProvider::new(procs.clone()));
//! // status bar: only when `orphan_summary().label()` is Some
//! cx.observe(&procs, |procs, cx| { let s = procs.read(cx).orphan_summary(); /* ... */ });
//! ```
#![cfg(windows)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod assets;
mod fmt;
mod model;
mod plan;
mod source;
mod state;
mod view;

pub use assets::{ARROW_UP_RIGHT, ASSETS, Assets, load as load_asset, runtime_icon_path};
pub use fmt::{format_bytes, format_cpu, format_uptime, shorten_command};
pub use model::{
    AgentGroup, Collected, GroupKey, Origin, OrphanSummary, OwnerGroup, PortPill, PortRow, ProcRow,
    RowState, Snapshot, build_snapshot, order_tree, pills,
};
pub use plan::{PlanItem, PlanKind, StopPlan, StopRoot, plan_orphans, plan_tree};
pub use source::{ProcSource, SharedTracker, StopResult, TrackerSource};
pub use state::{Dialog, ProcsModel, SAMPLE_INTERVAL, SummaryChanged};
pub use view::{PANEL_ID, ProcsPanel, ProcsPanelProvider, thumbnail};

use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedReceiver;
use proc_svc::{JobEvent, JobOptions, OwnerId, OwnerMeta, Tracker};

/// What [`open_owner`] returns for a new tab / thread.
pub struct OwnerJob {
    /// Raw handle of the job, for `PROC_THREAD_ATTRIBUTE_JOB_LIST` in the `STARTUPINFOEX` of
    /// `CreateProcessW` (the ConPTY spawn). Owned by the tracker: never close it, and do not use it
    /// after `Tracker::remove_owner`.
    pub job_handle: std::os::windows::io::RawHandle,
    /// Job lifecycle events; hand it to [`ProcsModel::watch_job_events`].
    pub events: UnboundedReceiver<JobEvent>,
}

/// Creates the job of a tab / thread in the shared tracker and enables its event stream.
pub fn open_owner(
    tracker: &Arc<Mutex<Tracker>>,
    owner: OwnerId,
    meta: OwnerMeta,
    options: JobOptions,
) -> proc_svc::Result<OwnerJob> {
    let mut t = tracker.lock().unwrap_or_else(|p| p.into_inner());
    let job = t.create_owner(owner, meta, options)?;
    let job_handle = job.raw_handle();
    let events = job.notifications()?;
    Ok(OwnerJob { job_handle, events })
}

#[cfg(test)]
mod tests;
