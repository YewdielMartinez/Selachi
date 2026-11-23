//! Claude usage for the status bar, from `agents_svc` (local logs, offline).
//!
//! The database lives in `%APPDATA%\Selachi\selachi.db`. Opening it and every `scan()` run on
//! gpui's background executor. The `Ingestor` is *moved* into the job and handed back with the
//! result, so scans can never overlap and no lock is held across an await. A scan runs at startup
//! and then every 60 s, but only while the window is focused (regaining focus after a longer
//! pause scans right away).
//!
//! Nothing is invented: a percentage is shown only when a fresh official rate-limit snapshot
//! exists (written by Claude Code itself); otherwise only the local billable total (input +
//! output + cache creation, no cache reads) is shown and it is labelled "estimado". If the ingest or the queries fail, the monitor reports nothing.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agents_svc::{Agent, IngestConfig, Ingestor, Range};
use gpui::{Context, Task};

const SCAN_EVERY: Duration = Duration::from_secs(60);
/// A scan that found nothing new doubles the wait up to this (a scan stats every agent log, so
/// an idle machine should not pay for it every minute); any news resets it to [`SCAN_EVERY`].
const SCAN_BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);

/// Wait until the next scan after `current`: doubled while scans find nothing, reset otherwise.
fn next_interval(current: Duration, quiet: bool) -> Duration {
    if quiet { (current * 2).min(SCAN_BACKOFF_MAX) } else { SCAN_EVERY }
}

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
        self.five_hour.as_ref().filter(|o| o.resets_at_ms > now_ms())
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
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
    if v >= 100.0 { format!("{v:.0}{suffix}") } else { format!("{v:.1}{suffix}").replace(".0", "") }
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
        format!("Lecturas de caché {} (no cuentan)", compact_tokens(w.cache_read)),
    ];
    lines.push(match info.fresh_official() {
        Some(o) => {
            let mins = ((now - o.observed_ms).max(0) / 60_000).max(0);
            format!("oficial · {:.0} % del límite semanal · hace {mins} min", o.percent)
        },
        None => "estimado · suma de los registros locales".to_string(),
    });
    lines.join("\n")
}

pub(crate) fn db_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Selachi").join("selachi.db"))
}

fn open() -> anyhow::Result<Ingestor> {
    let path = db_path().ok_or_else(|| anyhow::anyhow!("APPDATA is not set"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ingestor::open(&path, IngestConfig::from_home())
}

struct ScanOutcome {
    /// Handed back so the next scan reuses the connection; `None` if it could not be opened.
    ingestor: Option<Ingestor>,
    info: Option<UsageInfo>,
    /// No log changed since the previous scan.
    quiet: bool,
}

/// Blocking: open (if needed), scan, query. Only call it off the UI thread.
fn scan_job(existing: Option<Ingestor>) -> ScanOutcome {
    let mut ing = match existing {
        Some(i) => i,
        None => match open() {
            Ok(i) => i,
            Err(e) => {
                log::warn!("usage database unavailable: {e:#}");
                return ScanOutcome { ingestor: None, info: None, quiet: false };
            },
        },
    };
    let mut quiet = false;
    let info = match ing.scan() {
        Ok(report) => {
            for e in &report.errors {
                log::debug!("usage scan: {e}");
            }
            quiet = report.files_scanned == 0 && report.events_inserted == 0 && report.events_updated == 0;
            summarize(&ing)
        },
        Err(e) => {
            log::warn!("usage scan failed: {e:#}");
            None
        },
    };
    ScanOutcome { ingestor: Some(ing), info, quiet }
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
                (s.used_percent.is_finite() && (0.0..=100.0).contains(&s.used_percent)).then_some(Official {
                    percent: s.used_percent,
                    resets_at_ms,
                    observed_ms: s.ts_ms,
                })
            })
    };
    let official = official_for("seven_day");
    let five_hour = official_for("five_hour");
    (week.total() > 0 || official.is_some() || five_hour.is_some())
        .then_some(UsageInfo { week, official, five_hour })
}

pub struct UsageMonitor {
    info: Option<UsageInfo>,
    ingestor: Option<Ingestor>,
    in_flight: bool,
    active: bool,
    last_scan: Option<Instant>,
    /// Wait between scans: grows while they find nothing (see [`next_interval`]).
    interval: Duration,
    /// The usage panel's model: told after every successful scan (it re-reads only if visible).
    model: Option<gpui::Entity<ui_usage::UsageModel>>,
    _timer: Task<()>,
}

impl UsageMonitor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SCAN_EVERY).await;
                if this.update(cx, |m, cx| m.scan_if_due(cx)).is_err() {
                    break;
                }
            }
        });
        let mut monitor = Self {
            info: None,
            ingestor: None,
            in_flight: false,
            active: false,
            last_scan: None,
            interval: SCAN_EVERY,
            model: cx.try_global::<crate::services::Services>().map(|s| s.usage.clone()),
            _timer: timer,
        };
        monitor.scan(cx); // startup scan, focused or not
        monitor
    }

    pub fn info(&self) -> Option<&UsageInfo> {
        self.info.as_ref()
    }

    /// Window focus changed. Regaining focus after a pause refreshes the numbers.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            // Back after a pause: news is likely, so the wait starts from the short interval.
            self.interval = SCAN_EVERY;
        }
        self.scan_if_due(cx);
    }

    fn scan_if_due(&mut self, cx: &mut Context<Self>) {
        let due = self.last_scan.is_none_or(|t| t.elapsed() >= self.interval);
        if self.active && due {
            self.scan(cx);
        }
    }

    /// Starts a background scan unless one is already running.
    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        let ingestor = self.ingestor.take();
        cx.spawn(async move |this, cx| {
            let out = cx.background_executor().spawn(async move { scan_job(ingestor) }).await;
            let _ = this.update(cx, |m, cx| {
                m.in_flight = false;
                m.last_scan = Some(Instant::now());
                m.interval = next_interval(m.interval, out.quiet);
                let scanned = out.ingestor.is_some();
                m.ingestor = out.ingestor;
                if scanned && let Some(model) = &m.model {
                    model.update(cx, |u, cx| u.notify_scanned(cx));
                }
                if m.info != out.info {
                    m.info = out.info;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_scans_back_off_and_news_resets() {
        let mut d = SCAN_EVERY;
        d = next_interval(d, true);
        assert_eq!(d, Duration::from_secs(120));
        for _ in 0..10 {
            d = next_interval(d, true);
        }
        assert_eq!(d, SCAN_BACKOFF_MAX);
        assert_eq!(next_interval(d, false), SCAN_EVERY);
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
        let past = Official { percent: 40.0, resets_at_ms: now_ms() - 1, observed_ms: 0 };
        let future = Official { percent: 40.0, resets_at_ms: now_ms() + 60_000, observed_ms: 0 };
        let week = TokenBreakdown { input: 1, ..Default::default() };
        assert!(UsageInfo { week, official: Some(past), five_hour: None }.fresh_official().is_none());
        assert!(UsageInfo { week, official: Some(future), five_hour: None }.fresh_official().is_some());
    }

    #[test]
    fn billable_excludes_cache_reads() {
        let w = TokenBreakdown { input: 1_000_000, output: 2_000_000, cache_create: 9_400_000, cache_read: 84_000_000 };
        assert_eq!(w.billable(), 12_400_000);
        assert_eq!(w.total(), 96_400_000);
        assert_eq!(compact_tokens(w.billable()), "12.4M");
    }

    #[test]
    fn tooltip_lists_breakdown_and_labels_estimate() {
        let week = TokenBreakdown { input: 1_000_000, output: 2_000_000, cache_create: 9_400_000, cache_read: 84_000_000 };
        let est = usage_tooltip(&UsageInfo { week, official: None, five_hour: None }, now_ms());
        assert!(est.contains("Facturables 12.4M"));
        assert!(est.contains("Lecturas de caché 84M (no cuentan)"));
        assert!(est.contains("estimado"));
        assert!(!est.contains("oficial"));

        let official = Official { percent: 42.0, resets_at_ms: now_ms() + 60_000, observed_ms: now_ms() - 120_000 };
        let off = usage_tooltip(&UsageInfo { week, official: Some(official), five_hour: None }, now_ms());
        assert!(off.contains("oficial · 42 % del límite semanal · hace 2 min"));
        assert!(!off.contains("estimado"));
    }
}
