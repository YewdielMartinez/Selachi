//! Usage of an agent that Selachi did not launch (ola 9B): "which ingested session was that
//! `codex` process in this terminal, and what did it cost?".
//!
//! The agents' own logs are already ingested into `usage_events` (Claude / Codex JSONL, OpenCode and
//! Antigravity SQLite). [`Store::usage_for_external`] looks for the session of `agent` that worked in
//! `cwd` (or below it) during `[start_ms, end_ms]` (the lifetime of the process Selachi saw) and sums
//! what it spent *inside that window*. Nothing is guessed: no matching event means `None`, and the
//! cost is the agents' own reported cost when there is one, else the local price table (estimated),
//! else `None` for a model without a price.

use std::collections::BTreeMap;

use crate::db::Store;
use crate::model::{normalize_path, Agent, TokenCounts};
use crate::query::combine_cost;

/// What an external run used.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEstimate {
    /// Summed over the window, all classes.
    pub tokens: TokenCounts,
    /// USD: reported by the agent when it reports, else priced from the local table (an estimate,
    /// see [`crate::pricing::COST_LABEL`]). `None` when a model in the session has no price.
    pub cost: Option<f64>,
    /// The ingested session the numbers come from.
    pub session_id: String,
    /// Model responses accounted in the window.
    pub events: u64,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
    /// `false` when the agent logs no working directory (Antigravity) and the session was picked
    /// because it was the only one of that agent in the window.
    pub cwd_matched: bool,
}

impl UsageEstimate {
    /// input + output + cache writes + cache reads.
    pub fn total_tokens(&self) -> u64 {
        self.tokens.total()
    }

    /// input + output + cache writes (what limits meter; cache reads excluded).
    pub fn billable_tokens(&self) -> u64 {
        self.tokens.input + self.tokens.output + self.tokens.cache_create
    }
}

struct Row {
    session: String,
    model: String,
    unreported: TokenCounts,
    all: TokenCounts,
    reported: Option<f64>,
    events: u64,
    first: i64,
    last: i64,
}

fn u(v: i64) -> u64 {
    v.max(0) as u64
}

impl Store {
    /// The session of `agent` that ran in `cwd` (or below) between `start_ms` and `end_ms` (unix ms,
    /// inclusive), with what it used in that window. With several sessions the one that used the most
    /// tokens wins. `None` when nothing matches or the database cannot be read.
    pub fn usage_for_external(
        &self,
        agent: Agent,
        cwd: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Option<UsageEstimate> {
        match self.usage_for_external_inner(agent, cwd, start_ms, end_ms) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("agents_svc: usage_for_external failed: {e:#}");
                None
            }
        }
    }

    fn external_rows(
        &self,
        agent: Agent,
        extra_where: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> anyhow::Result<Vec<Row>> {
        let sql = format!(
            "SELECT session_id, model, \
                    SUM(CASE WHEN reported_cost IS NULL THEN input END), \
                    SUM(CASE WHEN reported_cost IS NULL THEN output END), \
                    SUM(CASE WHEN reported_cost IS NULL THEN cache_create END), \
                    SUM(CASE WHEN reported_cost IS NULL THEN cache_create_1h END), \
                    SUM(CASE WHEN reported_cost IS NULL THEN cache_read END), \
                    SUM(input), SUM(output), SUM(cache_create), SUM(cache_create_1h), SUM(cache_read), \
                    SUM(reported_cost), COUNT(*), MIN(ts), MAX(ts) \
             FROM usage_events WHERE agent = '{}' AND ts >= ?1 AND ts <= ?2 {extra_where} \
             GROUP BY session_id, model",
            agent.as_str()
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(args, |r| {
            let n = |i: usize| -> rusqlite::Result<u64> {
                Ok(u(r.get::<_, Option<i64>>(i)?.unwrap_or(0)))
            };
            Ok(Row {
                session: r.get(0)?,
                model: r.get(1)?,
                unreported: TokenCounts {
                    input: n(2)?,
                    output: n(3)?,
                    cache_create: n(4)?,
                    cache_create_1h: n(5)?,
                    cache_read: n(6)?,
                },
                all: TokenCounts {
                    input: n(7)?,
                    output: n(8)?,
                    cache_create: n(9)?,
                    cache_create_1h: n(10)?,
                    cache_read: n(11)?,
                },
                reported: r.get::<_, Option<f64>>(12)?,
                events: u(r.get(13)?),
                first: r.get(14)?,
                last: r.get(15)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn usage_for_external_inner(
        &self,
        agent: Agent,
        cwd: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Option<UsageEstimate>> {
        if end_ms < start_ms {
            return Ok(None);
        }
        let prefix = normalize_path(cwd);
        let like = format!(
            "{}/%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
                .trim_end_matches('/')
        );
        let mut rows = self.external_rows(
            agent,
            "AND (cwd_norm = ?3 OR cwd_norm LIKE ?4 ESCAPE '\\')",
            &[&start_ms, &end_ms, &prefix, &like],
        )?;
        let mut cwd_matched = true;
        if rows.is_empty() {
            // an agent that logs no directory: take the window's only session
            let anon = self.external_rows(agent, "AND cwd_norm IS NULL", &[&start_ms, &end_ms])?;
            let sessions: std::collections::BTreeSet<&str> =
                anon.iter().map(|r| r.session.as_str()).collect();
            if sessions.len() == 1 {
                rows = anon;
                cwd_matched = false;
            }
        }
        // one entry per session
        let mut by: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
        for r in &rows {
            by.entry(r.session.clone()).or_default().push(r);
        }
        let best = by.into_iter().max_by_key(|(_, rs)| {
            (
                rs.iter().map(|r| r.all.total()).sum::<u64>(),
                rs.iter().map(|r| r.last).max(),
            )
        });
        let Some((session_id, rs)) = best else {
            return Ok(None);
        };
        let mut tokens = TokenCounts::default();
        let mut cost = Some(0.0f64);
        let (mut events, mut first, mut last) = (0, i64::MAX, i64::MIN);
        for r in rs {
            tokens.add(&r.all);
            events += r.events;
            first = first.min(r.first);
            last = last.max(r.last);
            cost = match (
                cost,
                combine_cost(&self.pricing, agent, &r.model, r.reported, &r.unreported),
            ) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            };
        }
        if tokens.is_zero() {
            return Ok(None);
        }
        Ok(Some(UsageEstimate {
            tokens,
            cost,
            session_id,
            events,
            first_ts_ms: first,
            last_ts_ms: last,
            cwd_matched,
        }))
    }
}

/// [`Store::usage_for_external`] over the database file at `db` (never created: a missing file is
/// `None`). For callers that do not hold a [`Store`].
pub fn usage_for_external(
    db: &std::path::Path,
    agent: Agent,
    cwd: &str,
    start_ms: i64,
    end_ms: i64,
) -> Option<UsageEstimate> {
    if !db.is_file() {
        return None;
    }
    let store = match Store::open(db) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("agents_svc: cannot open {}: {e:#}", db.display());
            return None;
        }
    };
    store.usage_for_external(agent, cwd, start_ms, end_ms)
}
