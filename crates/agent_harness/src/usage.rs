//! Usage limits as `agents_svc` knows them (`selachi.db`, written by the GUI's log ingestion).
//!
//! The harness only *reads* the numbers the providers themselves reported (rate-limit snapshots
//! from Claude's statusline, Codex rollouts...). Nothing is estimated here and nothing is ingested:
//! if the GUI has never run, there are no snapshots and no weekly refusal can happen.

use std::path::{Path, PathBuf};

use agents_svc::Store;

/// One current limit window of an agent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LimitRow {
    pub agent: String,
    /// `five_hour`, `seven_day`, `<N>m`.
    pub window: String,
    pub used_percent: f64,
    pub resets_at_ms: Option<i64>,
    pub observed_ms: i64,
    /// Where the number comes from (`claude-statusline`, `codex-rollout`).
    pub source: String,
}

/// Source of limit numbers (injectable for tests).
pub trait UsageProbe: Send + Sync {
    /// Windows that have not reset yet, newest observation per (agent, window).
    fn limits(&self, now_ms: i64) -> Vec<LimitRow>;

    /// Highest percent among the agent's weekly-or-longer windows (`seven_day`, or any window of
    /// 7 days or more: Codex reports some plans as a 30-day `43200m` window).
    fn weekly_used_percent(&self, agent: &str, now_ms: i64) -> Option<LimitRow> {
        const WEEK_MS: i64 = 7 * 86_400_000;
        self.limits(now_ms)
            .into_iter()
            .filter(|l| {
                l.agent.eq_ignore_ascii_case(agent)
                    && (l.window == "seven_day"
                        || agents_svc::model::window_len_ms(&l.window)
                            .is_some_and(|ms| ms >= WEEK_MS))
            })
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    }
}

/// Reads `<data dir>\selachi.db` through `agents_svc`.
#[derive(Debug, Clone)]
pub struct AgentsSvcProbe {
    db: PathBuf,
}

impl AgentsSvcProbe {
    pub fn new(data_dir: &Path) -> AgentsSvcProbe {
        AgentsSvcProbe {
            db: data_dir.join("selachi.db"),
        }
    }
}

impl UsageProbe for AgentsSvcProbe {
    fn limits(&self, now_ms: i64) -> Vec<LimitRow> {
        // Never create the database just to read it.
        if !self.db.is_file() {
            return Vec::new();
        }
        let store = match Store::open(&self.db) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("agent_harness: cannot open {}: {e:#}", self.db.display());
                return Vec::new();
            }
        };
        match store.latest_rate_limits() {
            Ok(rows) => rows
                .into_iter()
                .filter(|r| r.resets_at_ms.is_none_or(|t| t > now_ms))
                .map(|r| LimitRow {
                    agent: r.agent.as_str().to_string(),
                    window: r.window,
                    used_percent: r.used_percent,
                    resets_at_ms: r.resets_at_ms,
                    observed_ms: r.ts_ms,
                    source: r.source,
                })
                .collect(),
            Err(e) => {
                log::warn!("agent_harness: reading rate limits failed: {e:#}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<LimitRow>);
    impl UsageProbe for Fixed {
        fn limits(&self, _now_ms: i64) -> Vec<LimitRow> {
            self.0.clone()
        }
    }

    fn row(agent: &str, window: &str, pct: f64) -> LimitRow {
        LimitRow {
            agent: agent.into(),
            window: window.into(),
            used_percent: pct,
            resets_at_ms: None,
            observed_ms: 0,
            source: "t".into(),
        }
    }

    #[test]
    fn weekly_picks_the_seven_day_window_of_that_agent() {
        let p = Fixed(vec![
            row("claude", "five_hour", 99.0),
            row("claude", "seven_day", 40.0),
            row("codex", "seven_day", 95.0),
            row("gemini", "43200m", 70.0),
            row("gemini", "300m", 99.0),
        ]);
        assert_eq!(
            p.weekly_used_percent("gemini", 0).unwrap().used_percent,
            70.0,
            "a 30-day window counts, a 5 h one does not"
        );
        assert_eq!(
            p.weekly_used_percent("claude", 0).unwrap().used_percent,
            40.0
        );
        assert_eq!(
            p.weekly_used_percent("CODEX", 0).unwrap().used_percent,
            95.0
        );
        assert!(p.weekly_used_percent("opencode", 0).is_none());
    }

    #[test]
    fn missing_database_means_no_limits_and_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let p = AgentsSvcProbe::new(dir.path());
        assert!(p.limits(0).is_empty());
        assert!(!dir.path().join("selachi.db").exists());
    }

    #[test]
    fn reads_real_snapshots_and_skips_reset_windows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("selachi.db");
        {
            let s = Store::open(&db).unwrap();
            let snap =
                |window: &str, pct: f64, resets: Option<i64>| agents_svc::RateLimitSnapshot {
                    agent: agents_svc::Agent::Codex,
                    ts_ms: 1_000,
                    window: window.into(),
                    used_percent: pct,
                    resets_at_ms: resets,
                    source: "codex-rollout".into(),
                };
            s.insert_rate_limit(&snap("seven_day", 93.0, Some(10_000)))
                .unwrap();
            s.insert_rate_limit(&snap("five_hour", 10.0, Some(500)))
                .unwrap();
        }
        let p = AgentsSvcProbe::new(dir.path());
        let rows = p.limits(2_000);
        assert_eq!(
            rows.len(),
            1,
            "the five_hour window already reset: {rows:?}"
        );
        assert_eq!(
            p.weekly_used_percent("codex", 2_000).unwrap().used_percent,
            93.0
        );
        assert!(
            p.weekly_used_percent("codex", 20_000).is_none(),
            "the week reset"
        );
    }
}
