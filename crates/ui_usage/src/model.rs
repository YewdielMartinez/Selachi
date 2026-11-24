//! `UsageModel`: the panel's data, refreshed **on demand**.
//!
//! * No timers. The host (which already scans the agents' logs every 60 s while its window is
//!   focused) calls [`UsageModel::notify_scanned`] after each scan; that is the only periodic
//!   trigger. Changing the range, the active session or the project root also refreshes.
//! * Hidden = no queries. A refresh requested while no view is painted only marks the model dirty;
//!   the first paint afterwards runs it.
//! * Queries run on gpui's background executor, one at a time; a request that arrives meanwhile is
//!   coalesced into a single follow-up query. Dropping the model (or hiding it) cancels the task.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agents_svc::Store;
use gpui::{Context, SharedString, Task};

use crate::data::{UsageQuery, UsageRange, UsageSnapshot};
use crate::source::{StoreSource, UsageSource};

pub struct UsageModel {
    source: Option<Arc<dyn UsageSource>>,
    range: UsageRange,
    session_id: Option<String>,
    project_root: Option<String>,
    snapshot: Option<Arc<UsageSnapshot>>,
    error: Option<SharedString>,
    visible: bool,
    viewers: usize,
    /// Inputs changed (or a scan happened) since the last query started.
    dirty: bool,
    in_flight: bool,
    task: Option<Task<()>>,
    queries: usize,
}

impl Default for UsageModel {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageModel {
    /// An empty model: until the host gives it a source the panel says there is no data.
    pub fn new() -> Self {
        Self {
            source: None,
            range: UsageRange::default(),
            session_id: None,
            project_root: None,
            snapshot: None,
            error: None,
            visible: false,
            viewers: 0,
            dirty: true,
            in_flight: false,
            task: None,
            queries: 0,
        }
    }

    pub fn with_source(source: Arc<dyn UsageSource>) -> Self {
        let mut m = Self::new();
        m.source = Some(source);
        m
    }

    // ----- hooks the host feeds ----------------------------------------------------------------

    /// Shares the `agents_svc::Store` the host already has. `Store` holds a SQLite connection
    /// (`Send`, not `Sync`), hence the `Mutex`.
    pub fn set_store(&mut self, store: Arc<Mutex<Store>>, cx: &mut Context<Self>) {
        self.set_source(Arc::new(StoreSource::shared(store)), cx);
    }

    /// Reads `selachi.db` through its own connection, opened lazily on the background executor
    /// (the simplest wiring: `%APPDATA%\Selachi\selachi.db`).
    pub fn set_db_path(&mut self, path: impl Into<PathBuf>, cx: &mut Context<Self>) {
        self.set_source(Arc::new(StoreSource::path(path)), cx);
    }

    /// Any query implementation (tests, demos).
    pub fn set_source(&mut self, source: Arc<dyn UsageSource>, cx: &mut Context<Self>) {
        self.source = Some(source);
        self.snapshot = None;
        self.request(cx);
    }

    /// The host finished a scan of the agents' logs: new events may exist. Refreshes only when the
    /// panel is visible; otherwise it just remembers to refresh at the next paint.
    pub fn notify_scanned(&mut self, cx: &mut Context<Self>) {
        self.request(cx);
    }

    /// The agent session the "Sesión" range sums (`None` = no active session).
    pub fn set_session_id(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id != session_id {
            self.session_id = session_id;
            if self.range == UsageRange::Session {
                self.request(cx);
            } else {
                self.dirty = true;
            }
        }
    }

    /// The project whose branches "Por rama" lists (the active tab's project root).
    pub fn set_project_root(&mut self, root: Option<String>, cx: &mut Context<Self>) {
        if self.project_root != root {
            self.project_root = root;
            self.request(cx);
        }
    }

    /// Picks a range of the selector.
    pub fn set_range(&mut self, range: UsageRange, cx: &mut Context<Self>) {
        if self.range != range {
            self.range = range;
            self.request(cx);
            cx.notify();
        }
    }

    // ----- visibility --------------------------------------------------------------------------

    /// Tells the model whether the panel is on screen. Hiding cancels a query in flight; showing
    /// runs one if something changed meanwhile (or nothing was ever loaded).
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            if self.dirty || self.snapshot.is_none() {
                self.request(cx);
            }
        } else {
            self.task = None;
            if self.in_flight {
                self.in_flight = false;
                self.dirty = true;
            }
        }
    }

    /// Heartbeat from every paint of a view: a painted panel is a visible panel. Never notifies.
    pub fn touch(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.set_visible(true, cx);
        }
    }

    pub fn attach_view(&mut self) {
        self.viewers += 1;
    }

    pub fn detach_view(&mut self, cx: &mut Context<Self>) {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers == 0 {
            self.set_visible(false, cx);
        }
    }

    // ----- reads -------------------------------------------------------------------------------

    pub fn range(&self) -> UsageRange {
        self.range
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn project_root(&self) -> Option<&str> {
        self.project_root.as_deref()
    }

    pub fn snapshot(&self) -> Option<&Arc<UsageSnapshot>> {
        self.snapshot.as_ref()
    }

    /// The last refresh failed (the previous snapshot, if any, stays on screen).
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_loading(&self) -> bool {
        self.in_flight
    }

    /// How many queries have been started (diagnostics and tests).
    pub fn queries_started(&self) -> usize {
        self.queries
    }

    // ----- refresh -----------------------------------------------------------------------------

    fn query(&self) -> UsageQuery {
        UsageQuery {
            range: self.range,
            session_id: self.session_id.clone(),
            project_root: self.project_root.clone(),
        }
    }

    /// Marks the model dirty and, when visible and idle, starts the query.
    fn request(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        if !self.visible || self.in_flight {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        self.dirty = false;
        self.in_flight = true;
        self.queries += 1;
        let query = self.query();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.load(&query) })
                .await;
            let _ = this.update(cx, |m, cx| m.apply(result, cx));
        }));
    }

    fn apply(&mut self, result: Result<UsageSnapshot, String>, cx: &mut Context<Self>) {
        self.in_flight = false;
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(Arc::new(snapshot));
                self.error = None;
            }
            Err(e) => {
                log::warn!("usage refresh failed: {e}");
                self.error = Some(e.into());
            }
        }
        cx.notify();
        // Something changed while the query ran: one more (coalesced) round.
        if self.dirty && self.visible {
            self.request(cx);
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Counts calls and returns a snapshot echoing the query (no database).
    pub struct FakeSource {
        pub calls: AtomicUsize,
        pub fail: std::sync::atomic::AtomicBool,
    }

    impl FakeSource {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: Default::default(),
            })
        }

        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl UsageSource for FakeSource {
        fn load(&self, query: &UsageQuery) -> Result<UsageSnapshot, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err("boom".to_string());
            }
            Ok(UsageSnapshot {
                query: query.clone(),
                ..UsageSnapshot::default()
            })
        }
    }
}
