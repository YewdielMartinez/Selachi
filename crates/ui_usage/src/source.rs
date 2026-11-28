//! Where the numbers come from: a query trait ([`UsageSource`]) and its `agents_svc` implementation.
//!
//! `agents_svc::Store` wraps a SQLite connection, which is `Send` but not `Sync`, so it cannot be
//! shared as a plain `Arc<Store>`: [`StoreSource`] keeps it behind a `Mutex` (the query is a few
//! indexed reads, and only one runs at a time anyway). Every method here is **blocking**; the model
//! only calls it on gpui's background executor.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use agents_svc::{Range, Store};
use chrono::Weekday;

use crate::data::{
    AGENT_ORDER, CHART_DAYS, DayUsage, LIMIT_AGENTS, LimitWindow, UsageQuery, UsageRange,
    UsageSnapshot, aggregate_agents, chart_day_bounds, local_now,
};

/// A blocking provider of [`UsageSnapshot`]s. Implemented over `agents_svc` by [`StoreSource`];
/// tests and demos can implement their own.
pub trait UsageSource: Send + Sync + 'static {
    /// Runs the whole refresh for `query`. An `Err` keeps the previous snapshot on screen.
    fn load(&self, query: &UsageQuery) -> Result<UsageSnapshot, String>;

    /// The first day of the week for the "Semana" range (`[agents] usage_week_start`). Sources
    /// without a calendar ignore it.
    fn set_week_start(&self, _week_start: Weekday) {}
}

/// How far back the forecast looks to measure the current pace.
pub const FORECAST_RECENT: Duration = Duration::from_secs(3600);

enum Inner {
    Shared(Arc<Mutex<Store>>),
    /// Opens the database on first use (and again after a failure).
    Path {
        path: PathBuf,
        store: Option<Store>,
    },
}

/// [`UsageSource`] over an `agents_svc` database.
pub struct StoreSource {
    inner: Mutex<Inner>,
    /// `Weekday::num_days_from_monday` of the configured first day of the week.
    week_start: AtomicU8,
}

impl StoreSource {
    /// Shares the store the host already has (for example next to its `Ingestor`).
    pub fn shared(store: Arc<Mutex<Store>>) -> Self {
        Self {
            inner: Mutex::new(Inner::Shared(store)),
            week_start: AtomicU8::new(0),
        }
    }

    /// Opens its own connection to `path` (SQLite WAL lets it read while the app scans and
    /// writes). The file is opened lazily, on the background executor, at the first refresh.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: Mutex::new(Inner::Path {
                path: path.into(),
                store: None,
            }),
            week_start: AtomicU8::new(0),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl UsageSource for StoreSource {
    fn set_week_start(&self, week_start: Weekday) {
        self.week_start
            .store(week_start.num_days_from_monday() as u8, Ordering::Relaxed);
    }

    fn load(&self, query: &UsageQuery) -> Result<UsageSnapshot, String> {
        let week_start =
            Weekday::try_from(self.week_start.load(Ordering::Relaxed)).unwrap_or(Weekday::Mon);
        let mut inner = lock(&self.inner);
        match &mut *inner {
            Inner::Shared(store) => {
                let mut store = lock(store);
                store.set_week_start(week_start);
                load_from(&store, query)
            }
            Inner::Path { path, store } => {
                if store.is_none() {
                    *store = Some(Store::open(path).map_err(|e| format!("{e:#}"))?);
                }
                match store.as_mut() {
                    Some(s) => {
                        s.set_week_start(week_start);
                        load_from(s, query)
                    }
                    None => Err("base de datos de uso no disponible".to_string()),
                }
            }
        }
    }
}

/// The refresh itself over an open store.
pub fn load_from(store: &Store, query: &UsageQuery) -> Result<UsageSnapshot, String> {
    let err = |e: anyhow::Error| format!("{e:#}");
    let now = local_now();
    let mut snap = UsageSnapshot {
        query: query.clone(),
        ..UsageSnapshot::default()
    };

    // Selected range: per-agent totals and (when a project is known) per-branch cost.
    match query.range.to_range(query.session_id.as_deref()) {
        Some(range) => {
            let rows = store.usage_summary(&range).map_err(err)?;
            let reported = store.reported_costs(&range).unwrap_or_else(|e| {
                log::warn!("reported costs unavailable: {e:#}");
                Vec::new()
            });
            snap.agents = aggregate_agents(&rows, &reported);
            snap.branches = query.project_root.as_deref().map(|root| {
                store.usage_by_branch(root, &range).unwrap_or_else(|e| {
                    log::warn!("usage by branch unavailable: {e:#}");
                    Vec::new()
                })
            });
        }
        None => snap.session_missing = query.range == UsageRange::Session,
    }

    // Official limits and the forecasts anchored on them (independent of the selected range).
    snap.limits = store.latest_rate_limits().map_err(err)?;
    for agent in LIMIT_AGENTS {
        for window in LimitWindow::ALL {
            match store.forecast(agent, window.key(), FORECAST_RECENT) {
                Ok(Some(f)) => snap.forecasts.push(f),
                Ok(None) => {}
                Err(e) => log::warn!("forecast unavailable: {e:#}"),
            }
        }
    }
    snap.forecasts.sort_by_key(|f| f.eta_at_ms);

    // 14-day chart: one indexed query per local day.
    for (date, start_ms, end_ms) in chart_day_bounds(&now) {
        let rows = store
            .usage_summary(&Range::Custom { start_ms, end_ms })
            .map_err(err)?;
        let mut per_agent = [0u64; AGENT_ORDER.len()];
        for (slot, agent) in AGENT_ORDER.iter().enumerate() {
            per_agent[slot] = rows
                .iter()
                .filter(|r| r.agent == *agent)
                .map(|r| r.input + r.output + r.cache_create)
                .sum();
        }
        snap.days.push(DayUsage { date, per_agent });
    }
    debug_assert_eq!(snap.days.len(), CHART_DAYS);
    Ok(snap)
}
