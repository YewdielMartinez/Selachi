//! Claude usage for the status bar, from `agents_svc` (local logs, offline).
//!
//! The database lives in `%APPDATA%\Selachi\selachi.db`. Opening it and every `scan()` run on
//! gpui's background executor. The `Ingestor` is *moved* into the job and handed back with the
//! result, so scans can never overlap and no lock is held across an await. A scan runs at startup
//! and then every 60 s, but only while the window is focused (regaining focus after a longer
//! pause scans right away).
//!
//! Nothing is invented: a percentage is shown only when a fresh official rate-limit snapshot
//! exists (written by Claude Code itself); otherwise only the local token total is shown and it
//! is labelled "estimado". If the ingest or the queries fail, the monitor reports nothing.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agents_svc::{Agent, IngestConfig, Ingestor, Range};
use gpui::{Context, Task};

const SCAN_EVERY: Duration = Duration::from_secs(60);

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

#[derive(Clone, Debug, PartialEq)]
pub struct UsageInfo {
    /// Claude tokens this calendar week (input + output + cache), summed from local logs.
    pub week_tokens: u64,
    pub official: Option<Official>,
}

impl UsageInfo {
    /// The official snapshot, unless its window has already reset.
    pub fn fresh_official(&self) -> Option<&Official> {
        self.official.as_ref().filter(|o| o.resets_at_ms > now_ms())
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

fn db_path() -> Option<PathBuf> {
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
}

/// Blocking: open (if needed), scan, query. Only call it off the UI thread.
fn scan_job(existing: Option<Ingestor>) -> ScanOutcome {
    let mut ing = match existing {
        Some(i) => i,
        None => match open() {
            Ok(i) => i,
            Err(e) => {
                log::warn!("usage database unavailable: {e:#}");
                return ScanOutcome { ingestor: None, info: None };
            },
        },
    };
    let info = match ing.scan() {
        Ok(report) => {
            for e in &report.errors {
                log::debug!("usage scan: {e}");
            }
            summarize(&ing)
        },
        Err(e) => {
            log::warn!("usage scan failed: {e:#}");
            None
        },
    };
    ScanOutcome { ingestor: Some(ing), info }
}

fn summarize(ing: &Ingestor) -> Option<UsageInfo> {
    let store = ing.store();
    let week_tokens = store
        .usage_summary(&Range::ThisWeek)
        .ok()?
        .iter()
        .filter(|u| u.agent == Agent::Claude)
        .map(agents_svc::AgentUsage::total_tokens)
        .sum();
    let now = now_ms();
    let official = store
        .latest_rate_limits()
        .ok()
        .and_then(|all| all.into_iter().find(|s| s.agent == Agent::Claude && s.window == "seven_day"))
        .and_then(|s| {
            let resets_at_ms = s.resets_at_ms.filter(|&r| r > now)?;
            (s.used_percent.is_finite() && (0.0..=100.0).contains(&s.used_percent)).then_some(Official {
                percent: s.used_percent,
                resets_at_ms,
                observed_ms: s.ts_ms,
            })
        });
    (week_tokens > 0 || official.is_some()).then_some(UsageInfo { week_tokens, official })
}

pub struct UsageMonitor {
    info: Option<UsageInfo>,
    ingestor: Option<Ingestor>,
    in_flight: bool,
    active: bool,
    last_scan: Option<Instant>,
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
        self.scan_if_due(cx);
    }

    fn scan_if_due(&mut self, cx: &mut Context<Self>) {
        let due = self.last_scan.is_none_or(|t| t.elapsed() >= SCAN_EVERY);
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
                m.ingestor = out.ingestor;
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
        assert!(UsageInfo { week_tokens: 1, official: Some(past) }.fresh_official().is_none());
        assert!(UsageInfo { week_tokens: 1, official: Some(future) }.fresh_official().is_some());
    }
}
