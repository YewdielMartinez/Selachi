//! `ProcsModel`: the entity the app owns and the panel views observe.
//!
//! # Cost model
//!
//! * **Hidden: nothing runs.** No timer, no sampling. The CPU baseline is dropped.
//! * **Visible: one sample every [`SAMPLE_INTERVAL`]** (2 s), taken on the background executor.
//!   "Visible" means a view called [`ProcsModel::set_visible`]`(true)` or paints (every render of
//!   the panel calls [`ProcsModel::touch`]). If the panel stops painting (hidden, minimised) for
//!   more than two intervals the loop ends by itself.
//! * **Job events** (a process of a job started or exited, delivered by the kernel through a
//!   completion port, zero polling) trigger one throttled refresh even while hidden, so the status
//!   bar pill ([`ProcsModel::orphan_summary`]) stays right.
//! * Termination is a two-step flow that never acts without a confirmation: see [`Dialog`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Context, EventEmitter, SharedString, Task};
use proc_svc::JobEvent;

use crate::model::{Collected, OrphanSummary, Snapshot, build_snapshot};
use crate::plan::{StopPlan, plan_orphans, plan_tree};
use crate::source::{ProcSource, SharedTracker, StopResult, TrackerSource};

/// Time between samples while the panel is visible.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// Minimum gap between event-driven refreshes while visible.
const EVENT_GAP_VISIBLE: Duration = Duration::from_millis(750);
/// Minimum gap between event-driven refreshes while hidden.
const EVENT_GAP_HIDDEN: Duration = Duration::from_secs(2);

/// The inline confirmation flow. Nothing is terminated unless the model is in
/// [`Dialog::Confirm`] and [`ProcsModel::confirm`] is called.
#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    None,
    /// A fresh sample is being taken to build the preview.
    Planning,
    /// The preview: what exactly will be terminated. Waiting for "Detener" / "Cancelar".
    Confirm(StopPlan),
    /// Terminating (graceful, then forced after the grace period).
    Running(StopPlan),
    /// Outcome of a run.
    Done {
        plan: StopPlan,
        result: StopResult,
    },
    /// A plain message ("Ya no hay huérfanos").
    Notice(SharedString),
}

#[derive(Clone, Copy)]
enum PlanRequest {
    Tree(u32),
    Orphans,
}

/// Emitted when the orphan summary changes (the status bar can listen instead of observing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummaryChanged(pub OrphanSummary);

impl EventEmitter<SummaryChanged> for ProcsModel {}

pub struct ProcsModel {
    source: Arc<dyn ProcSource>,
    snapshot: Arc<Snapshot>,
    interval: Duration,
    visible: bool,
    viewers: usize,
    last_touch: Option<Instant>,
    loop_running: bool,
    loop_task: Option<Task<()>>,
    in_flight: bool,
    queued: bool,
    last_refresh: Option<Instant>,
    event_timer: Option<Task<()>>,
    event_tasks: Vec<Task<()>>,
    dialog: Dialog,
    dialog_epoch: u64,
}

impl ProcsModel {
    /// A model sampling a shared tracker.
    pub fn with_tracker(tracker: SharedTracker) -> Self {
        Self::new(Arc::new(TrackerSource::new(tracker)))
    }

    /// A model over any source (tests, custom attribution).
    pub fn new(source: Arc<dyn ProcSource>) -> Self {
        Self {
            source,
            snapshot: Arc::new(Snapshot::default()),
            interval: SAMPLE_INTERVAL,
            visible: false,
            viewers: 0,
            last_touch: None,
            loop_running: false,
            loop_task: None,
            in_flight: false,
            queued: false,
            last_refresh: None,
            event_timer: None,
            event_tasks: Vec::new(),
            dialog: Dialog::None,
            dialog_epoch: 0,
        }
    }

    /// Sampling period while visible (default [`SAMPLE_INTERVAL`]).
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    // ----- reads -------------------------------------------------------------------------

    /// The last sample, grouped. Cheap to clone.
    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    /// For the status bar: `3 huérfanos · 1,2 GB` (see [`OrphanSummary::label`]); show the pill
    /// only when `count > 0`.
    pub fn orphan_summary(&self) -> OrphanSummary {
        self.snapshot.summary
    }

    pub fn dialog(&self) -> &Dialog {
        &self.dialog
    }

    /// The sampling loop is alive (true only while the panel is visible).
    pub fn is_sampling(&self) -> bool {
        self.loop_running
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    // ----- visibility and sampling ---------------------------------------------------------

    /// The shell (or a view) says the panel is shown / hidden. Showing starts the 2 s loop with an
    /// immediate sample; hiding cancels it and drops the CPU baseline.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible {
            self.last_touch = None;
            self.start_loop(cx);
        } else {
            self.stop_loop();
        }
    }

    /// Heartbeat: call it from every render of a view. Marks the panel as visible and (re)starts
    /// the loop when it had stopped. Never notifies, so it is safe to call while painting.
    pub fn touch(&mut self, cx: &mut Context<Self>) {
        self.last_touch = Some(Instant::now());
        if !self.visible || !self.loop_running {
            self.visible = true;
            self.start_loop(cx);
        }
    }

    /// A view was created.
    pub fn attach_view(&mut self) {
        self.viewers += 1;
    }

    /// A view was dropped; the last one hides the panel.
    pub fn detach_view(&mut self, cx: &mut Context<Self>) {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers == 0 {
            self.set_visible(false, cx);
        }
    }

    fn start_loop(&mut self, cx: &mut Context<Self>) {
        if self.loop_running {
            return;
        }
        self.loop_running = true;
        self.refresh(cx);
        let interval = self.interval;
        self.loop_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let keep = this
                    .update(cx, |m, cx| {
                        let stale = m.last_touch.is_some_and(|t| {
                            t.elapsed() > interval * 2 + Duration::from_millis(500)
                        });
                        if !m.visible || stale {
                            m.visible = false;
                            m.loop_running = false;
                            m.source.reset_sampling();
                            return false;
                        }
                        m.refresh(cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn stop_loop(&mut self) {
        self.loop_task = None; // cancels the timer
        if std::mem::replace(&mut self.loop_running, false) {
            self.source.reset_sampling();
        }
    }

    /// Takes a sample now (coalesced: at most one in flight, one queued behind it).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            self.queued = true;
            return;
        }
        self.in_flight = true;
        self.last_refresh = Some(Instant::now());
        let source = self.source.clone();
        cx.spawn(async move |this, cx| {
            let data = cx
                .background_executor()
                .spawn(async move { source.collect() })
                .await;
            let _ = this.update(cx, |m, cx| m.apply(data, cx));
        })
        .detach();
    }

    fn apply(&mut self, data: Collected, cx: &mut Context<Self>) {
        self.in_flight = false;
        self.set_snapshot(build_snapshot(&data), cx);
        if std::mem::take(&mut self.queued) {
            self.refresh(cx);
        }
    }

    fn set_snapshot(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        let before = self.snapshot.summary;
        self.snapshot = Arc::new(snap);
        if before != self.snapshot.summary {
            cx.emit(SummaryChanged(self.snapshot.summary));
        }
        cx.notify();
    }

    // ----- job events ----------------------------------------------------------------------

    /// Refreshes (throttled) whenever a job reports a lifecycle event. Hand it the receiver of
    /// `JobGroup::notifications()`; the task ends with the job.
    pub fn watch_job_events(
        &mut self,
        mut rx: UnboundedReceiver<JobEvent>,
        cx: &mut Context<Self>,
    ) {
        let task = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                if this.update(cx, |m, cx| m.on_job_event(cx)).is_err() {
                    break;
                }
            }
        });
        self.event_tasks.push(task);
    }

    /// One job event happened: refresh, but never more than once per throttle window.
    pub fn on_job_event(&mut self, cx: &mut Context<Self>) {
        let gap = if self.visible {
            EVENT_GAP_VISIBLE
        } else {
            EVENT_GAP_HIDDEN
        };
        let wait = self
            .last_refresh
            .map_or(Duration::ZERO, |t| gap.saturating_sub(t.elapsed()));
        if wait.is_zero() {
            self.refresh(cx);
            return;
        }
        if self.event_timer.is_some() {
            return;
        }
        self.event_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |m, cx| {
                if let Some(t) = m.event_timer.take() {
                    t.detach(); // this very task: let it finish
                }
                m.refresh(cx);
            });
        }));
    }

    // ----- stop flow -----------------------------------------------------------------------

    /// "Detener" on one process: builds the preview of its tree. Terminates nothing.
    pub fn request_stop(&mut self, pid: u32, cx: &mut Context<Self>) {
        self.begin_plan(PlanRequest::Tree(pid), cx);
    }

    /// "Limpiar huérfanos": builds the preview of every orphan. Terminates nothing.
    pub fn request_clean_orphans(&mut self, cx: &mut Context<Self>) {
        self.begin_plan(PlanRequest::Orphans, cx);
    }

    fn begin_plan(&mut self, request: PlanRequest, cx: &mut Context<Self>) {
        if matches!(
            self.dialog,
            Dialog::Planning | Dialog::Confirm(_) | Dialog::Running(_)
        ) {
            return; // one dialog at a time
        }
        self.dialog = Dialog::Planning;
        self.dialog_epoch += 1;
        let epoch = self.dialog_epoch;
        cx.notify();
        let source = self.source.clone();
        cx.spawn(async move |this, cx| {
            let built = cx
                .background_executor()
                .spawn(async move {
                    let c = source.collect();
                    let snap = build_snapshot(&c);
                    let plan = match request {
                        PlanRequest::Tree(pid) => plan_tree(&c, &snap, pid),
                        PlanRequest::Orphans => Some(plan_orphans(&c, &snap)),
                    }
                    .map(|mut p| {
                        p.vet(|pid| source.check_stop(pid));
                        p
                    });
                    (snap, plan)
                })
                .await;
            let _ = this.update(cx, |m, cx| m.plan_ready(epoch, built.0, built.1, cx));
        })
        .detach();
    }

    fn plan_ready(
        &mut self,
        epoch: u64,
        snap: Snapshot,
        plan: Option<StopPlan>,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.dialog_epoch || self.dialog != Dialog::Planning {
            return; // cancelled meanwhile
        }
        self.dialog = match plan {
            Some(p) if !p.is_empty() || !p.refused.is_empty() => Dialog::Confirm(p),
            Some(p) if p.denied > 0 => Dialog::Notice(
                "Los huérfanos restantes no se pueden inspeccionar (sin permiso).".into(),
            ),
            Some(_) => Dialog::Notice("No hay huérfanos que limpiar.".into()),
            None => Dialog::Notice("Ese proceso ya no existe.".into()),
        };
        self.set_snapshot(snap, cx);
    }

    /// "Cancelar" / "Cerrar". Ignored while terminating.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if matches!(self.dialog, Dialog::None | Dialog::Running(_)) {
            return;
        }
        self.dialog = Dialog::None;
        self.dialog_epoch += 1;
        cx.notify();
    }

    /// "Detener" in the confirmation: **the only call that terminates anything**, and only from
    /// [`Dialog::Confirm`] with a non-empty plan.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let plan = match &self.dialog {
            Dialog::Confirm(p) if !p.is_empty() => p.clone(),
            _ => return,
        };
        self.dialog = Dialog::Running(plan.clone());
        self.dialog_epoch += 1;
        cx.notify();
        let source = self.source.clone();
        let for_task = plan.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.terminate(&for_task) })
                .await;
            let _ = this.update(cx, |m, cx| {
                m.dialog = Dialog::Done { plan, result };
                cx.notify();
                m.refresh(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::model::fixtures::sample;

    /// A source over canned data that counts what is asked of it.
    pub struct FakeSource {
        pub data: Mutex<Collected>,
        pub collects: AtomicUsize,
        pub terminated: Mutex<Vec<Vec<u32>>>,
        pub resets: AtomicUsize,
        pub refuse: Mutex<Vec<u32>>,
    }

    impl FakeSource {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                data: Mutex::new(sample()),
                collects: AtomicUsize::new(0),
                terminated: Mutex::new(Vec::new()),
                resets: AtomicUsize::new(0),
                refuse: Mutex::new(Vec::new()),
            })
        }

        pub fn collects(&self) -> usize {
            self.collects.load(Ordering::SeqCst)
        }

        pub fn terminations(&self) -> usize {
            self.terminated.lock().map_or(0, |t| t.len())
        }
    }

    impl ProcSource for FakeSource {
        fn collect(&self) -> Collected {
            self.collects.fetch_add(1, Ordering::SeqCst);
            self.data.lock().map(|d| d.clone()).unwrap_or_default()
        }

        fn check_stop(&self, pid: u32) -> Result<(), String> {
            match self.refuse.lock() {
                Ok(r) if r.contains(&pid) => Err("proceso crítico".into()),
                _ => Ok(()),
            }
        }

        fn terminate(&self, plan: &StopPlan) -> StopResult {
            let pids: Vec<u32> = plan.roots.iter().map(|r| r.pid).collect();
            if let Ok(mut t) = self.terminated.lock() {
                t.push(pids.clone());
            }
            StopResult {
                graceful: pids,
                ..StopResult::default()
            }
        }

        fn reset_sampling(&self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }
}
