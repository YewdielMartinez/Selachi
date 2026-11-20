//! Read-side API: summaries, per-branch cost, burn rate, limit forecast, touched files.
//!
//! Every cost is an estimate from the pricing table ([`crate::pricing::COST_LABEL`]); `None` means the
//! model (or a needed price class) is not in the table.

use crate::db::Store;
use crate::model::{normalize_path, window_len_ms, Agent, TokenCounts};
use crate::range::Range;
use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use rusqlite::params;
use std::collections::BTreeMap;
use std::time::Duration;

/// Usage of one (agent, model) pair.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentUsage {
    pub agent: Agent,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    pub events: u64,
    /// Estimated cost (pricing table). `None` when the model/price is unknown - never a guess.
    pub est_cost_usd: Option<f64>,
}

impl AgentUsage {
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }
}

/// Usage attributed to one git branch.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchUsage {
    /// `None` = events with no recorded branch (e.g. Codex, detached HEAD).
    pub git_branch: Option<String>,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    pub events: u64,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
    /// Sum of the priced portions only; `None` when nothing could be priced.
    pub est_cost_usd: Option<f64>,
    /// False when some models on this branch had no price (cost is then a lower bound).
    pub cost_complete: bool,
}

/// Tokens per hour over a trailing window.
#[derive(Debug, Clone, PartialEq)]
pub struct BurnRate {
    pub window_secs: u64,
    /// input + output + cache_create + cache_read in the window.
    pub tokens: u64,
    /// input + output + cache_create (cache reads excluded; closer to what limits meter).
    pub billable_tokens: u64,
    pub tokens_per_hour: f64,
    pub billable_tokens_per_hour: f64,
}

/// Estimated time until a rate-limit window reaches 100 %.
#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    pub agent: Agent,
    pub window: String,
    /// Percent reported by the snapshot this forecast is anchored on (a real provider number).
    pub snapshot_used_percent: f64,
    pub snapshot_ts_ms: i64,
    pub snapshot_source: String,
    pub resets_at_ms: i64,
    /// Calibration: billable tokens that moved the meter by 1 percentage point in this window.
    pub tokens_per_percent: f64,
    /// Estimated percent used right now (snapshot + local tokens since the snapshot).
    pub est_used_percent_now: f64,
    pub recent_billable_tokens_per_hour: f64,
    /// Milliseconds from `now` until the estimated 100 % point.
    pub eta_ms: i64,
    pub eta_at_ms: i64,
    /// True if the projected 100 % point is before the window resets.
    pub reaches_limit_before_reset: bool,
    /// Always "estimated": anchored on a real snapshot but extrapolated locally.
    pub label: &'static str,
}

/// A file touched by an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTouched {
    pub agent: Agent,
    pub session_id: String,
    pub ts_ms: i64,
    pub path: String,
    pub tool: String,
}

fn u(v: i64) -> u64 {
    v.max(0) as u64
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Store {
    /// Usage grouped by (agent, model) for `range`, resolved in local time.
    pub fn usage_summary(&self, range: &Range) -> Result<Vec<AgentUsage>> {
        self.usage_summary_at(range, &Local::now())
    }

    /// Same, with an explicit "now" (and thereby timezone) - used by tests.
    pub fn usage_summary_at<Tz: TimeZone>(
        &self,
        range: &Range,
        now: &DateTime<Tz>,
    ) -> Result<Vec<AgentUsage>> {
        let b = range.resolve(now, self.week_start);
        let mut stmt = self.conn.prepare_cached(
            "SELECT agent, model, SUM(input), SUM(output), SUM(cache_create), SUM(cache_create_1h), \
                    SUM(cache_read), COUNT(*) \
             FROM usage_events \
             WHERE (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ts < ?2) AND (?3 IS NULL OR session_id = ?3) \
             GROUP BY agent, model ORDER BY agent, model",
        )?;
        let rows = stmt.query_map(params![b.start_ms, b.end_ms, b.session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                TokenCounts {
                    input: u(r.get(2)?),
                    output: u(r.get(3)?),
                    cache_create: u(r.get(4)?),
                    cache_create_1h: u(r.get(5)?),
                    cache_read: u(r.get(6)?),
                },
                u(r.get(7)?),
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (agent, model, t, events) = row?;
            let Some(agent) = Agent::parse(&agent) else {
                continue;
            };
            out.push(AgentUsage {
                agent,
                est_cost_usd: self.pricing.cost(agent, &model, &t),
                model,
                input: t.input,
                output: t.output,
                cache_create: t.cache_create,
                cache_read: t.cache_read,
                events,
            });
        }
        Ok(out)
    }

    /// Cost per git branch for sessions whose cwd is `cwd_prefix` or below (path-segment aware,
    /// case-insensitive, `\` and `/` equivalent). Sorted by last activity, newest first.
    pub fn usage_by_branch(&self, cwd_prefix: &str, range: &Range) -> Result<Vec<BranchUsage>> {
        self.usage_by_branch_at(cwd_prefix, range, &Local::now())
    }

    pub fn usage_by_branch_at<Tz: TimeZone>(
        &self,
        cwd_prefix: &str,
        range: &Range,
        now: &DateTime<Tz>,
    ) -> Result<Vec<BranchUsage>> {
        let b = range.resolve(now, self.week_start);
        let prefix = normalize_path(cwd_prefix);
        let like = format!(
            "{}/%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
                .trim_end_matches('/')
        );
        let mut stmt = self.conn.prepare_cached(
            "SELECT git_branch, agent, model, SUM(input), SUM(output), SUM(cache_create), \
                    SUM(cache_create_1h), SUM(cache_read), COUNT(*), MIN(ts), MAX(ts) \
             FROM usage_events \
             WHERE (cwd_norm = ?1 OR cwd_norm LIKE ?2 ESCAPE '\\') \
               AND (?3 IS NULL OR ts >= ?3) AND (?4 IS NULL OR ts < ?4) AND (?5 IS NULL OR session_id = ?5) \
             GROUP BY git_branch, agent, model",
        )?;
        let rows = stmt.query_map(
            params![prefix, like, b.start_ms, b.end_ms, b.session],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    TokenCounts {
                        input: u(r.get(3)?),
                        output: u(r.get(4)?),
                        cache_create: u(r.get(5)?),
                        cache_create_1h: u(r.get(6)?),
                        cache_read: u(r.get(7)?),
                    },
                    u(r.get(8)?),
                    r.get::<_, i64>(9)?,
                    r.get::<_, i64>(10)?,
                ))
            },
        )?;
        let mut by: BTreeMap<Option<String>, BranchUsage> = BTreeMap::new();
        for row in rows {
            let (branch, agent, model, t, events, first, last) = row?;
            let Some(agent) = Agent::parse(&agent) else {
                continue;
            };
            let e = by.entry(branch.clone()).or_insert_with(|| BranchUsage {
                git_branch: branch,
                input: 0,
                output: 0,
                cache_create: 0,
                cache_read: 0,
                events: 0,
                first_ts_ms: first,
                last_ts_ms: last,
                est_cost_usd: None,
                cost_complete: true,
            });
            e.input += t.input;
            e.output += t.output;
            e.cache_create += t.cache_create;
            e.cache_read += t.cache_read;
            e.events += events;
            e.first_ts_ms = e.first_ts_ms.min(first);
            e.last_ts_ms = e.last_ts_ms.max(last);
            match self.pricing.cost(agent, &model, &t) {
                Some(c) => e.est_cost_usd = Some(e.est_cost_usd.unwrap_or(0.0) + c),
                None => e.cost_complete = false,
            }
        }
        let mut v: Vec<_> = by.into_values().collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.last_ts_ms));
        Ok(v)
    }

    /// Tokens per hour over the trailing `window` (one agent or all).
    pub fn burn_rate(&self, agent: Option<Agent>, window: Duration) -> Result<BurnRate> {
        self.burn_rate_at(agent, window, now_ms())
    }

    pub fn burn_rate_at(
        &self,
        agent: Option<Agent>,
        window: Duration,
        now_ms: i64,
    ) -> Result<BurnRate> {
        let start = now_ms - window.as_millis() as i64;
        let ag = agent.map(|a| a.as_str());
        let (inp, out, cc, cr): (i64, i64, i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(input),0), COALESCE(SUM(output),0), COALESCE(SUM(cache_create),0), \
                    COALESCE(SUM(cache_read),0) FROM usage_events \
             WHERE ts > ?1 AND ts <= ?2 AND (?3 IS NULL OR agent = ?3)",
            params![start, now_ms, ag],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let billable = u(inp) + u(out) + u(cc);
        let total = billable + u(cr);
        let hours = window.as_secs_f64() / 3600.0;
        let per_hour = |n: u64| if hours > 0.0 { n as f64 / hours } else { 0.0 };
        Ok(BurnRate {
            window_secs: window.as_secs(),
            tokens: total,
            billable_tokens: billable,
            tokens_per_hour: per_hour(total),
            billable_tokens_per_hour: per_hour(billable),
        })
    }

    /// Project when `window` (e.g. `"five_hour"`, `"seven_day"`) hits 100 %.
    ///
    /// Returns `None` unless a REAL, unexpired rate-limit snapshot exists for the agent/window
    /// (Codex rollouts, the Claude statusLine hook, or a recorded Claude rejection) with a known reset
    /// time, 0 < used < 100, and some recent local activity. Method: calibrate tokens-per-percent from
    /// the billable tokens spent in the window up to the snapshot, then extrapolate the recent burn rate.
    pub fn forecast(
        &self,
        agent: Agent,
        window: &str,
        recent: Duration,
    ) -> Result<Option<Forecast>> {
        self.forecast_at(agent, window, recent, now_ms())
    }

    pub fn forecast_at(
        &self,
        agent: Agent,
        window: &str,
        recent: Duration,
        now_ms: i64,
    ) -> Result<Option<Forecast>> {
        let Some(wlen) = window_len_ms(window) else {
            return Ok(None);
        };
        let snap: Option<(i64, f64, i64, String)> = self
            .conn
            .prepare_cached(
                "SELECT ts, used_percent, resets_at, source FROM rate_limit_snapshots \
                 WHERE agent = ?1 AND window = ?2 AND resets_at IS NOT NULL AND ts <= ?3 \
                 ORDER BY ts DESC LIMIT 1",
            )?
            .query_row(params![agent.as_str(), window, now_ms], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        let Some((ts, used, resets_at, source)) = snap else {
            return Ok(None);
        };
        if resets_at <= now_ms || used <= 0.0 || used >= 100.0 {
            return Ok(None);
        }
        let win_start = resets_at - wlen;
        let billable_between = |from: i64, to: i64| -> Result<f64> {
            let n: i64 = self.conn.query_row(
                "SELECT COALESCE(SUM(input + output + cache_create),0) FROM usage_events \
                 WHERE agent = ?1 AND ts > ?2 AND ts <= ?3",
                params![agent.as_str(), from, to],
                |r| r.get(0),
            )?;
            Ok(n as f64)
        };
        let calib_tokens = billable_between(win_start, ts)?;
        if calib_tokens <= 0.0 {
            return Ok(None);
        }
        let tokens_per_percent = calib_tokens / used;
        let since_snapshot = billable_between(ts, now_ms)?;
        let est_now = (used + since_snapshot / tokens_per_percent).min(100.0);
        let recent_tokens = billable_between(now_ms - recent.as_millis() as i64, now_ms)?;
        let hours = recent.as_secs_f64() / 3600.0;
        if hours <= 0.0 || recent_tokens <= 0.0 {
            return Ok(None);
        }
        let rate = recent_tokens / hours;
        let remaining_tokens = (100.0 - est_now) * tokens_per_percent;
        let eta_ms = (remaining_tokens / rate * 3_600_000.0).round() as i64;
        let eta_at = now_ms + eta_ms;
        Ok(Some(Forecast {
            agent,
            window: window.to_string(),
            snapshot_used_percent: used,
            snapshot_ts_ms: ts,
            snapshot_source: source,
            resets_at_ms: resets_at,
            tokens_per_percent,
            est_used_percent_now: est_now,
            recent_billable_tokens_per_hour: rate,
            eta_ms,
            eta_at_ms: eta_at,
            reaches_limit_before_reset: eta_at < resets_at,
            label: crate::pricing::COST_LABEL,
        }))
    }

    /// Latest real rate-limit snapshot per (agent, window), newest first.
    pub fn latest_rate_limits(&self) -> Result<Vec<crate::model::RateLimitSnapshot>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT agent, ts, window, used_percent, resets_at, source FROM rate_limit_snapshots r \
             WHERE ts = (SELECT MAX(ts) FROM rate_limit_snapshots \
                         WHERE agent = r.agent AND window = r.window) \
             ORDER BY ts DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (a, ts, window, pct, resets, source) = row?;
            if let Some(agent) = Agent::parse(&a) {
                out.push(crate::model::RateLimitSnapshot {
                    agent,
                    ts_ms: ts,
                    window,
                    used_percent: pct,
                    resets_at_ms: resets,
                    source,
                });
            }
        }
        Ok(out)
    }

    /// Files touched in a session (paths only), oldest first.
    pub fn files_touched(&self, session_id: &str) -> Result<Vec<FileTouched>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT agent, session_id, ts, path, tool FROM file_touches WHERE session_id = ?1 \
             ORDER BY ts, id",
        )?;
        let rows = stmt.query_map([session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (a, session_id, ts_ms, path, tool) = row?;
            if let Some(agent) = Agent::parse(&a) {
                out.push(FileTouched {
                    agent,
                    session_id,
                    ts_ms,
                    path,
                    tool,
                });
            }
        }
        Ok(out)
    }
}
