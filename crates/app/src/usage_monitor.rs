//! Claude usage for the status bar, from `agents_svc` (local logs, offline).
//!
//! The database lives in `%APPDATA%\Selachi\selachi.db`. Opening it and every ingest run on
//! gpui's background executor. The `Ingestor` is *moved* into the job and handed back with the
//! result, so jobs can never overlap and no lock is held across an await.
//!
//! **Event-driven, zero cost at rest.** A [`agents_svc::LogWatcher`] (notify) watches the agents'
//! log directories and only *marks* what changed: no timer, no database access and no UI wake-up
//! on its side. Ingestion happens only when the numbers can be seen, that is while the window is
//! focused, and then
//! * right when focus returns (at least [`ACTIVATION_GAP`] after the previous ingest), and
//! * at most once per [`TICK`] while it stays focused,
//!
//! and only for the files the watcher named (incremental tail reads; an unchanged file is never
//! re-read, an unchanged `.zst` is never re-decompressed). A burst is capped at
//! [`MAX_FILES_PER_JOB`] files per job; the rest follows after a short pause. A full scan runs
//! only when nothing is known yet (first focus), after an overflow / watcher error, or after a
//! failed job. Unfocused or minimized: nothing runs, the dirty set just keeps deduplicating.
//!
//! Repaints happen only when the displayed numbers actually change (`info` differs); the 60 s
//! tick is a plain background timer (no frame requested) and exists only while focused.
//!
//! Nothing is invented: a percentage is shown only when a fresh official rate-limit snapshot
//! exists (written by Claude Code itself); otherwise only the local billable total (input +
//! output + cache creation, no cache reads) is shown and it is labelled "estimado". If the ingest or the queries fail, the monitor reports nothing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agents_svc::{Agent, IngestConfig, Ingestor, LogWatcher, Range, Weekday};
use gpui::{Context, Task};

/// While focused, changed logs are ingested at most this often.
const TICK: Duration = Duration::from_secs(60);
/// Minimum time between two ingests when focus returns (debounce of focus flapping).
const ACTIVATION_GAP: Duration = Duration::from_secs(5);
/// Minimum time between two ingests on a tick (ticks are 60 s apart; this only guards drift).
const TICK_GAP: Duration = Duration::from_secs(30);
/// Pause before the next slice of a burst, so a huge backlog never hogs a core.
const SLICE_PAUSE: Duration = Duration::from_millis(500);
/// Files ingested per background job.
const MAX_FILES_PER_JOB: usize = 24;
/// The numbers are re-queried at least this often even if no log changed (week rollover,
/// expiring rate-limit snapshots).
const RESUMMARIZE_AFTER: Duration = Duration::from_secs(10 * 60);
/// Benchmark switch: treat the window as always focused (see `bench/README.md`).
const ASSUME_ACTIVE_ENV: &str = "SELACHI_BENCH_USAGE_ACTIVE";

/// A rate-limit percentage that Claude Code itself reported and that has not expired yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Official {
    /// Share of the weekly limit used, 0..=100.
    pub percent: f64,
    /// When the window resets (unix ms); the snapshot is stale after this.
    pub resets_at_ms: i64,
    /// When Claude Code wrote the snapshot (unix ms).
    pub observed_ms: i64,
}

/// Claude tokens of one period, by kind (summed from the local logs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
}

impl TokenBreakdown {
    /// What is billed: input + output + cache creation. Cache *reads* are served from the
    /// prompt cache at a fraction of the price and would dwarf the real usage, so they are not
    /// part of the headline number.
    pub fn billable(&self) -> u64 {
        self.input + self.output + self.cache_create
    }

    /// Everything, cache reads included.
    pub fn total(&self) -> u64 {
        self.billable() + self.cache_read
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageInfo {
    /// Claude tokens this calendar week, by kind.
    pub week: TokenBreakdown,
    /// Official weekly (`seven_day`) rate-limit snapshot.
    pub official: Option<Official>,
    /// Official 5-hour window snapshot, when Claude Code reported one.
    pub five_hour: Option<Official>,
}

impl UsageInfo {
    /// The official snapshot, unless its window has already reset.
    pub fn fresh_official(&self) -> Option<&Official> {
        self.official.as_ref().filter(|o| o.resets_at_ms > now_ms())
    }

    /// The 5-hour snapshot, unless its window has already reset.
    pub fn fresh_five_hour(&self) -> Option<&Official> {
        self.five_hour
            .as_ref()
            .filter(|o| o.resets_at_ms > now_ms())
    }

    /// Is the weekly token total drawn? Without a fresh official snapshot it is only an estimate
    /// from the local logs, so `[agents] show_estimates = false` hides it.
    pub fn week_total_visible(&self, show_estimates: bool) -> bool {
        show_estimates || self.fresh_official().is_some()
    }
}

/// The first day of the week `[agents] usage_week_start` names.
pub fn weekday_of(w: store::config::WeekStart) -> Weekday {
    use store::config::WeekStart as W;
    match w {
        W::Monday => Weekday::Mon,
        W::Tuesday => Weekday::Tue,
        W::Wednesday => Weekday::Wed,
        W::Thursday => Weekday::Thu,
        W::Friday => Weekday::Fri,
        W::Saturday => Weekday::Sat,
        W::Sunday => Weekday::Sun,
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `12_400_000` -> `12.4M`, `842_000` -> `842K`, `999` -> `999`.
pub fn compact_tokens(n: u64) -> String {
    let f = n as f64;
    let (v, suffix) = match n {
        0..=999 => return n.to_string(),
        1_000..=999_999 => (f / 1e3, "K"),
        1_000_000..=999_999_999 => (f / 1e6, "M"),
        _ => (f / 1e9, "B"),
    };
    // One decimal below 100, none above (`842K`, `12.4M`).
    if v >= 100.0 {
        format!("{v:.0}{suffix}")
    } else {
        format!("{v:.1}{suffix}").replace(".0", "")
    }
}

/// Hover text of the usage pill: the billable breakdown, the cache reads that are *not* counted,
/// and whether the meter is official (a fresh rate-limit snapshot from Claude Code) or just an
/// estimate from the local logs.
pub fn usage_tooltip(info: &UsageInfo, now: i64) -> String {
    let w = &info.week;
    let mut lines = vec![
        "Claude · esta semana".to_string(),
        format!(
            "Facturables {} = entrada {} + salida {} + caché creada {}",
            compact_tokens(w.billable()),
            compact_tokens(w.input),
            compact_tokens(w.output),
            compact_tokens(w.cache_create)
        ),
        format!(
            "Lecturas de caché {} (no cuentan)",
            compact_tokens(w.cache_read)
        ),
    ];
    lines.push(match info.fresh_official() {
        Some(o) => {
            let mins = ((now - o.observed_ms).max(0) / 60_000).max(0);
            format!(
                "oficial · {:.0} % del límite semanal · hace {mins} min",
                o.percent
            )
        }
        None => "estimado · suma de los registros locales".to_string(),
    });
    lines.join("\n")
}

pub(crate) fn db_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Selachi").join("selachi.db"))
}

fn open(db: &Option<PathBuf>, cfg: &IngestConfig) -> anyhow::Result<Ingestor> {
    let path = db
        .clone()
        .ok_or_else(|| anyhow::anyhow!("APPDATA is not set"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ingestor::open(&path, cfg.clone())
}

/// What a background job does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Work {
    /// Only read the numbers from the database (no log is touched).
    Summary,
    /// Ingest everything (initial state, overflow, after a failure).
    Full,
    /// Ingest just these files.
    Paths(Vec<PathBuf>),
}

struct ScanOutcome {
    /// Handed back so the next job reuses the connection; `None` if it could not be opened.
    ingestor: Option<Ingestor>,
    /// `Some` = the numbers were (re)computed; `None` = nothing new, keep what is shown.
    info: Option<Option<UsageInfo>>,
    /// New events, touches or rate-limit snapshots were stored (or a full scan ran).
    data_changed: bool,
    /// The ingest itself failed: the dirty state must be restored.
    failed: bool,
}

/// Blocking: open (if needed), ingest `work`, query. Only call it off the UI thread.
fn scan_job(
    existing: Option<Ingestor>,
    db: &Option<PathBuf>,
    cfg: &IngestConfig,
    work: &Work,
    resummarize: bool,
    week_start: Weekday,
) -> ScanOutcome {
    let mut ing = match existing {
        Some(i) => i,
        None => match open(db, cfg) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("usage database unavailable: {e:#}");
                return ScanOutcome {
                    ingestor: None,
                    info: Some(None),
                    data_changed: false,
                    failed: true,
                };
            }
        },
    };
    ing.store_mut().set_week_start(week_start);
    let mut data_changed = false;
    let mut failed = false;
    let report = match work {
        Work::Summary => None,
        Work::Full => {
            data_changed = true;
            Some(ing.scan())
        }
        Work::Paths(paths) => Some(ing.scan_paths(paths)),
    };
    match report {
        Some(Ok(r)) => {
            for e in &r.errors {
                log::debug!("usage scan: {e}");
            }
            data_changed |= r.events_inserted
                + r.events_updated
                + r.touches_inserted
                + r.rate_snapshots_inserted
                > 0;
        }
        Some(Err(e)) => {
            log::warn!("usage scan failed: {e:#}");
            failed = true;
        }
        None => {}
    }
    let recompute = data_changed || resummarize || failed || *work == Work::Summary;
    let info = recompute.then(|| summarize(&ing));
    ScanOutcome {
        ingestor: Some(ing),
        info,
        data_changed,
        failed,
    }
}

fn summarize(ing: &Ingestor) -> Option<UsageInfo> {
    let store = ing.store();
    let week = store
        .usage_summary(&Range::ThisWeek)
        .ok()?
        .iter()
        .filter(|u| u.agent == Agent::Claude)
        .fold(TokenBreakdown::default(), |acc, u| TokenBreakdown {
            input: acc.input + u.input,
            output: acc.output + u.output,
            cache_create: acc.cache_create + u.cache_create,
            cache_read: acc.cache_read + u.cache_read,
        });
    let now = now_ms();
    let limits = store.latest_rate_limits().ok().unwrap_or_default();
    let official_for = |window: &str| {
        limits
            .iter()
            .find(|s| s.agent == Agent::Claude && s.window == window)
            .and_then(|s| {
                let resets_at_ms = s.resets_at_ms.filter(|&r| r > now)?;
                (s.used_percent.is_finite() && (0.0..=100.0).contains(&s.used_percent)).then_some(
                    Official {
                        percent: s.used_percent,
                        resets_at_ms,
                        observed_ms: s.ts_ms,
                    },
                )
            })
    };
    let official = official_for("seven_day");
    let five_hour = official_for("five_hour");
    (week.total() > 0 || official.is_some() || five_hour.is_some()).then_some(UsageInfo {
        week,
        official,
        five_hour,
    })
}

pub struct UsageMonitor {
    info: Option<UsageInfo>,
    ingestor: Option<Ingestor>,
    db: Option<PathBuf>,
    cfg: IngestConfig,
    /// Started on the background executor together with the first (read-only) job.
    watcher: Option<Arc<LogWatcher>>,
    in_flight: bool,
    active: bool,
    /// Benchmark switch: behave as if the window were always focused.
    pinned_active: bool,
    last_scan: Option<Instant>,
    last_summary: Instant,
    /// The usage panel's model: told after every ingest that stored something (it re-reads only
    /// if visible).
    model: Option<gpui::Entity<ui_usage::UsageModel>>,
    /// Exists only while the window is focused; dropping it cancels the timer.
    tick: Option<Task<()>>,
    /// `[agents] usage_week_start`: the first day of the week the weekly total sums from.
    week_start: Weekday,
    /// `[agents] show_estimates`: hide the figures that are only estimates.
    show_estimates: bool,
    /// The week start changed while a job was running: read the numbers again when it ends.
    week_dirty: bool,
}

impl UsageMonitor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let pinned_active = std::env::var_os(ASSUME_ACTIVE_ENV).is_some();
        let mut monitor = Self {
            info: None,
            ingestor: None,
            db: db_path(),
            cfg: IngestConfig::from_home(),
            watcher: None,
            in_flight: true, // the init job below
            active: false,
            pinned_active,
            last_scan: None,
            last_summary: Instant::now(),
            model: cx
                .try_global::<crate::services::Services>()
                .map(|s| s.usage.clone()),
            tick: None,
            week_start: Weekday::Mon,
            show_estimates: true,
            week_dirty: false,
        };
        // Init job: install the watchers and read the numbers already in the database. No log
        // is touched here; the first focus (or the pin) triggers the full scan.
        // (Tests of the shell must not watch or scan the developer's agent logs.)
        #[cfg(not(test))]
        {
            let (db, cfg) = (monitor.db.clone(), monitor.cfg.clone());
            let week_start = monitor.week_start;
            cx.spawn(async move |this, cx| {
                let (watcher, out) = cx
                    .background_executor()
                    .spawn(async move {
                        let watcher = LogWatcher::start(&cfg);
                        let out = scan_job(None, &db, &cfg, &Work::Summary, true, week_start);
                        (watcher, out)
                    })
                    .await;
                let _ = this.update(cx, |m, cx| {
                    m.watcher = Some(Arc::new(watcher));
                    m.in_flight = false;
                    m.apply(out, false, cx);
                    if std::mem::take(&mut m.week_dirty) {
                        m.start(Work::Summary, true, cx);
                    }
                    m.check(ACTIVATION_GAP, cx);
                });
            })
            .detach();
        }
        #[cfg(test)]
        {
            monitor.in_flight = false;
        }
        if pinned_active {
            monitor.set_active(true, cx);
        }
        monitor
    }

    pub fn info(&self) -> Option<&UsageInfo> {
        self.info.as_ref()
    }

    /// `[agents] show_estimates`.
    pub fn show_estimates(&self) -> bool {
        self.show_estimates
    }

    #[cfg(test)]
    pub fn week_start(&self) -> Weekday {
        self.week_start
    }

    /// Applies `[agents]` at startup and on every config change: the first day of the week (the
    /// numbers are read again, in the background, only when it changed) and whether estimated
    /// figures show. Also tells the usage panel's model.
    pub fn apply_agents_config(&mut self, agents: &store::config::Agents, cx: &mut Context<Self>) {
        let week_start = weekday_of(agents.usage_week_start);
        if let Some(model) = &self.model {
            model.update(cx, |m, cx| {
                m.set_week_start(week_start, cx);
                m.set_show_estimates(agents.show_estimates, cx);
            });
        }
        if self.show_estimates != agents.show_estimates {
            self.show_estimates = agents.show_estimates;
            cx.notify();
        }
        if self.week_start != week_start {
            self.week_start = week_start;
            if self.in_flight {
                self.week_dirty = true;
            } else {
                self.start(Work::Summary, true, cx);
            }
        }
    }

    /// Window focus changed. Regaining focus ingests whatever changed while it was away.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let active = active || self.pinned_active;
        if active == self.active {
            return;
        }
        self.active = active;
        if !active {
            self.tick = None;
            return;
        }
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |m, cx| m.on_tick(cx)).is_err() {
                    break;
                }
            }
        }));
        self.check(ACTIVATION_GAP, cx);
    }

    fn on_tick(&mut self, cx: &mut Context<Self>) {
        if self.watcher.as_ref().is_some_and(|w| w.has_changes()) {
            self.check(TICK_GAP, cx);
        } else if self.active && !self.in_flight && self.last_summary.elapsed() >= RESUMMARIZE_AFTER
        {
            self.start(Work::Summary, true, cx);
        }
    }

    /// Ingests what the watcher marked, if the numbers can be seen and `min_gap` has passed.
    fn check(&mut self, min_gap: Duration, cx: &mut Context<Self>) {
        if !self.active || self.in_flight {
            return;
        }
        let Some(watcher) = self.watcher.clone() else {
            return;
        };
        if !watcher.has_changes() || self.last_scan.is_some_and(|t| t.elapsed() < min_gap) {
            return;
        }
        let changes = watcher.take_changes(MAX_FILES_PER_JOB);
        let work = if changes.all {
            Work::Full
        } else if changes.paths.is_empty() {
            return;
        } else {
            Work::Paths(changes.paths)
        };
        self.start(work, false, cx);
    }

    /// Starts a background job unless one is already running.
    fn start(&mut self, work: Work, resummarize: bool, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        let ingestor = self.ingestor.take();
        let (db, cfg) = (self.db.clone(), self.cfg.clone());
        let week_start = self.week_start;
        let ingest = work != Work::Summary;
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { scan_job(ingestor, &db, &cfg, &work, resummarize, week_start) })
                .await;
            let _ = this.update(cx, |m, cx| {
                m.in_flight = false;
                m.apply(out, ingest, cx);
                if std::mem::take(&mut m.week_dirty) {
                    m.start(Work::Summary, true, cx);
                }
                // A backlog (capped job): continue after a pause. Plain background timer.
                if ingest && m.watcher.as_ref().is_some_and(|w| w.has_changes()) {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SLICE_PAUSE).await;
                        let _ = this.update(cx, |m, cx| m.check(Duration::ZERO, cx));
                    })
                    .detach();
                }
            });
        })
        .detach();
    }

    fn apply(&mut self, out: ScanOutcome, ingest: bool, cx: &mut Context<Self>) {
        if ingest {
            self.last_scan = Some(Instant::now());
            if out.failed
                && let Some(w) = &self.watcher
            {
                w.mark_all(); // retried on the next tick
            }
        }
        let opened = out.ingestor.is_some();
        self.ingestor = out.ingestor;
        if let Some(info) = out.info {
            self.last_summary = Instant::now();
            if self.info != info {
                self.info = info;
                cx.notify(); // the only repaint request: the displayed numbers changed
            }
        }
        if opened
            && out.data_changed
            && let Some(model) = &self.model
        {
            model.update(cx, |u, cx| u.notify_scanned(cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("selachi-usage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn jobs_ingest_only_named_files_and_recompute_only_on_news() {
        let dir = temp_dir("job");
        let log = dir.join("claude").join("p").join("s.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let line = r#"{"type":"assistant","timestamp":"2026-03-02T10:00:00.000Z","sessionId":"s","cwd":"C:/p","gitBranch":"main","requestId":"r1","uuid":"u1","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"content":[]}}"#;
        std::fs::write(
            &log,
            format!(
                "{line}
"
            ),
        )
        .unwrap();
        let cfg = IngestConfig {
            claude_projects: Some(dir.join("claude")),
            ..Default::default()
        };
        let db = Some(dir.join("t.db"));

        // Summary: touches no log, but computes the numbers.
        let out = scan_job(None, &db, &cfg, &Work::Summary, true, Weekday::Mon);
        assert!(!out.data_changed && !out.failed && out.info.is_some());
        // Naming the file ingests it.
        let out = scan_job(
            out.ingestor,
            &db,
            &cfg,
            &Work::Paths(vec![log.clone()]),
            false,
            Weekday::Mon,
        );
        assert!(out.data_changed && out.info.is_some());
        // Same file again, unchanged: nothing stored, numbers not recomputed.
        let out = scan_job(
            out.ingestor,
            &db,
            &cfg,
            &Work::Paths(vec![log]),
            false,
            Weekday::Mon,
        );
        assert!(!out.data_changed && out.info.is_none());
        // An unrelated path is ignored.
        let out = scan_job(
            out.ingestor,
            &db,
            &cfg,
            &Work::Paths(vec![dir.join("claude").join("x.txt")]),
            false,
            Weekday::Mon,
        );
        assert!(!out.data_changed && out.info.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compact_formatting() {
        assert_eq!(compact_tokens(0), "0");
        assert_eq!(compact_tokens(999), "999");
        assert_eq!(compact_tokens(1_000), "1K");
        assert_eq!(compact_tokens(1_500), "1.5K");
        assert_eq!(compact_tokens(842_000), "842K");
        assert_eq!(compact_tokens(12_400_000), "12.4M");
        assert_eq!(compact_tokens(1_234_000_000), "1.2B");
    }

    #[test]
    fn expired_official_snapshot_is_not_fresh() {
        let past = Official {
            percent: 40.0,
            resets_at_ms: now_ms() - 1,
            observed_ms: 0,
        };
        let future = Official {
            percent: 40.0,
            resets_at_ms: now_ms() + 60_000,
            observed_ms: 0,
        };
        let week = TokenBreakdown {
            input: 1,
            ..Default::default()
        };
        assert!(
            UsageInfo {
                week,
                official: Some(past),
                five_hour: None
            }
            .fresh_official()
            .is_none()
        );
        assert!(
            UsageInfo {
                week,
                official: Some(future),
                five_hour: None
            }
            .fresh_official()
            .is_some()
        );
    }

    #[test]
    fn billable_excludes_cache_reads() {
        let w = TokenBreakdown {
            input: 1_000_000,
            output: 2_000_000,
            cache_create: 9_400_000,
            cache_read: 84_000_000,
        };
        assert_eq!(w.billable(), 12_400_000);
        assert_eq!(w.total(), 96_400_000);
        assert_eq!(compact_tokens(w.billable()), "12.4M");
    }

    #[test]
    fn tooltip_lists_breakdown_and_labels_estimate() {
        let week = TokenBreakdown {
            input: 1_000_000,
            output: 2_000_000,
            cache_create: 9_400_000,
            cache_read: 84_000_000,
        };
        let est = usage_tooltip(
            &UsageInfo {
                week,
                official: None,
                five_hour: None,
            },
            now_ms(),
        );
        assert!(est.contains("Facturables 12.4M"));
        assert!(est.contains("Lecturas de caché 84M (no cuentan)"));
        assert!(est.contains("estimado"));
        assert!(!est.contains("oficial"));

        let official = Official {
            percent: 42.0,
            resets_at_ms: now_ms() + 60_000,
            observed_ms: now_ms() - 120_000,
        };
        let off = usage_tooltip(
            &UsageInfo {
                week,
                official: Some(official),
                five_hour: None,
            },
            now_ms(),
        );
        assert!(off.contains("oficial · 42 % del límite semanal · hace 2 min"));
        assert!(!off.contains("estimado"));
    }

    #[test]
    fn the_weekly_total_sums_from_the_configured_first_day() {
        use agents_svc::chrono::{Datelike as _, Duration, Local, TimeZone as _};
        use agents_svc::{TokenCounts, UsageEvent};

        let dir = temp_dir("weekstart");
        let db = Some(dir.join("t.db"));
        let cfg = IngestConfig::default();
        let ing = open(&db, &cfg).unwrap();
        // One Claude event at noon yesterday (local time).
        let today = Local::now().date_naive();
        let yesterday = today - Duration::days(1);
        let noon = Local
            .from_local_datetime(&yesterday.and_hms_opt(12, 0, 0).unwrap())
            .earliest()
            .unwrap();
        ing.store()
            .upsert_usage(&UsageEvent {
                agent: Agent::Claude,
                session_id: "s".into(),
                ts_ms: noon.timestamp_millis(),
                model: "claude-opus-5-5".into(),
                tokens: TokenCounts {
                    input: 1_000,
                    output: 500,
                    cache_create: 0,
                    cache_create_1h: 0,
                    cache_read: 0,
                },
                cwd: None,
                git_branch: None,
                msg_key: "k1".into(),
            })
            .unwrap();
        // The week began today: yesterday is last week, so there is nothing to show.
        let out = scan_job(Some(ing), &db, &cfg, &Work::Summary, true, today.weekday());
        assert_eq!(out.info, Some(None));
        // The week began yesterday: the event is in it.
        let out = scan_job(
            out.ingestor,
            &db,
            &cfg,
            &Work::Summary,
            true,
            yesterday.weekday(),
        );
        let info = out.info.flatten().expect("numbers");
        assert_eq!(info.week.billable(), 1_500);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn estimated_totals_are_hidden_unless_claude_reported_them() {
        let week = TokenBreakdown {
            input: 10,
            output: 5,
            cache_create: 0,
            cache_read: 0,
        };
        let estimate_only = UsageInfo {
            week,
            official: None,
            five_hour: None,
        };
        assert!(estimate_only.week_total_visible(true));
        assert!(!estimate_only.week_total_visible(false));
        let official = UsageInfo {
            official: Some(Official {
                percent: 10.0,
                resets_at_ms: now_ms() + 3_600_000,
                observed_ms: now_ms(),
            }),
            ..estimate_only
        };
        assert!(
            official.week_total_visible(false),
            "an official figure is not an estimate"
        );
    }

    #[test]
    fn the_sidebar_meter_says_estimado_only_when_estimates_are_shown() {
        use crate::sidebar_view::usage_of;
        let info = UsageInfo {
            week: TokenBreakdown {
                input: 10,
                output: 5,
                cache_create: 0,
                cache_read: 0,
            },
            official: None,
            five_hour: None,
        };
        assert_eq!(usage_of(Some(&info), true).label.as_ref(), "estimado");
        assert_eq!(usage_of(Some(&info), false).label.as_ref(), "—");
    }

    #[test]
    fn every_configured_day_maps_to_its_weekday() {
        use store::config::WeekStart as W;
        assert_eq!(weekday_of(W::Monday), Weekday::Mon);
        assert_eq!(weekday_of(W::Saturday), Weekday::Sat);
        assert_eq!(weekday_of(W::Sunday), Weekday::Sun);
    }
}
