//! Claude Code `statusLine` payload capture (PLAN section 6.1, layer 2).
//!
//! Claude Code runs the configured statusLine command and writes a JSON document to its stdin. The
//! exact field names are NOT verified against a live payload on this machine (no hook is installed),
//! so the parser is deliberately tolerant: every field is optional and several spellings are accepted.
//!
//! Accepted shapes (first match wins):
//!
//! * model: `model` as string, or `model.id` / `model.display_name`
//! * session id: `session_id` or `sessionId`
//! * cost (USD): `cost.total_cost_usd`, `cost.totalCostUSD` or `total_cost_usd`
//! * context window: `context_window.context_window_size` plus `context_window.used_percentage`
//!   (or `used_percent`); without a percentage it is derived from `total_input_tokens` and
//!   `total_output_tokens`
//! * rate limits: `rate_limits.five_hour` and `rate_limits.seven_day` (also camelCase), each
//!   `{used_percentage | used_percent | utilization, resets_at | resetsAt}`; `resets_at` may be epoch
//!   seconds, epoch milliseconds or an RFC 3339 string. Percentages are taken as-is (0..100 scale);
//!   a 0..1 fraction is NOT rescaled.

use crate::db::Store;
use crate::model::{parse_ts_ms, Agent, RateLimitSnapshot};
use anyhow::{Context, Result};
use rusqlite::params;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatuslineLimit {
    pub used_percent: f64,
    /// Unix milliseconds.
    pub resets_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatuslineData {
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub context_window_size: Option<u64>,
    pub context_used_percent: Option<f64>,
    pub five_hour: Option<StatuslineLimit>,
    pub seven_day: Option<StatuslineLimit>,
}

fn pick<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|x| !x.is_null())
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| {
        v.as_str()
            .and_then(|s| s.trim_end_matches('%').parse().ok())
    })
}

fn parse_reset(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_f64() {
        // Heuristic: values below 1e11 are epoch seconds.
        return Some(if n < 1e11 {
            (n * 1000.0) as i64
        } else {
            n as i64
        });
    }
    v.as_str().and_then(parse_ts_ms)
}

fn parse_limit(v: Option<&Value>) -> Option<StatuslineLimit> {
    let v = v?;
    let pct = as_f64(pick(
        v,
        &["used_percentage", "used_percent", "utilization"],
    )?)?;
    Some(StatuslineLimit {
        used_percent: pct,
        resets_at_ms: pick(v, &["resets_at", "resetsAt"]).and_then(parse_reset),
    })
}

/// Tolerant parser; only fails on invalid JSON (or a non-object document).
pub fn parse_statusline_payload(json: &str) -> Result<StatuslineData> {
    let v: Value = serde_json::from_str(json).context("statusline payload is not valid JSON")?;
    anyhow::ensure!(v.is_object(), "statusline payload is not a JSON object");
    let model = match pick(&v, &["model"]) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(m @ Value::Object(_)) => pick(m, &["id", "display_name", "name"])
            .and_then(Value::as_str)
            .map(String::from),
        _ => None,
    };
    let mut d = StatuslineData {
        model,
        session_id: pick(&v, &["session_id", "sessionId"])
            .and_then(Value::as_str)
            .map(String::from),
        cost_usd: pick(&v, &["cost"])
            .and_then(|c| pick(c, &["total_cost_usd", "totalCostUSD", "total_cost"]))
            .or_else(|| pick(&v, &["total_cost_usd"]))
            .and_then(as_f64),
        ..Default::default()
    };

    if let Some(cw) = pick(&v, &["context_window", "contextWindow"]) {
        d.context_window_size =
            pick(cw, &["context_window_size", "size", "max_tokens"]).and_then(Value::as_u64);
        d.context_used_percent = pick(cw, &["used_percentage", "used_percent"])
            .and_then(as_f64)
            .or_else(|| {
                let size = d.context_window_size? as f64;
                let used = pick(cw, &["total_input_tokens"]).and_then(Value::as_f64)?
                    + pick(cw, &["total_output_tokens"])
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                (size > 0.0).then(|| used / size * 100.0)
            });
    }

    if let Some(rl) = pick(&v, &["rate_limits", "rateLimits"]) {
        d.five_hour = parse_limit(pick(rl, &["five_hour", "fiveHour", "5h"]));
        d.seven_day = parse_limit(pick(rl, &["seven_day", "sevenDay", "7d"]));
    }
    Ok(d)
}

/// Persist a captured payload: real rate-limit snapshots (source `claude-statusline`) plus a
/// cost/context row. Returns the number of rows written.
pub fn record_statusline(store: &Store, data: &StatuslineData, ts_ms: i64) -> Result<usize> {
    let mut n = 0;
    for (window, lim) in [
        ("five_hour", &data.five_hour),
        ("seven_day", &data.seven_day),
    ] {
        if let Some(l) = lim {
            if store.insert_rate_limit(&RateLimitSnapshot {
                agent: Agent::Claude,
                ts_ms,
                window: window.to_string(),
                used_percent: l.used_percent,
                resets_at_ms: l.resets_at_ms,
                source: "claude-statusline".to_string(),
            })? {
                n += 1;
            }
        }
    }
    if data.cost_usd.is_some() || data.context_used_percent.is_some() || data.model.is_some() {
        store.conn.execute(
            "INSERT INTO statusline_snapshots(ts, session_id, model, cost_usd, context_used_percent, context_window_size) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                ts_ms,
                data.session_id,
                data.model,
                data.cost_usd,
                data.context_used_percent,
                data.context_window_size.map(|v| v as i64)
            ],
        )?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tolerantly() {
        let d = parse_statusline_payload(
            r#"{"session_id":"abc","model":{"id":"claude-opus-5-5","display_name":"Opus"},
                "cost":{"total_cost_usd":1.5},
                "context_window":{"context_window_size":200000,"total_input_tokens":50000,"total_output_tokens":0},
                "rate_limits":{"five_hour":{"used_percentage":42.0,"resets_at":1790000000},
                               "seven_day":{"used_percent":"7%","resets_at":"2026-12-01T00:00:00Z"}}}"#,
        )
        .unwrap();
        assert_eq!(d.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(d.cost_usd, Some(1.5));
        assert_eq!(d.context_used_percent, Some(25.0));
        assert_eq!(
            d.five_hour.as_ref().unwrap().resets_at_ms,
            Some(1_790_000_000_000)
        );
        assert_eq!(d.seven_day.as_ref().unwrap().used_percent, 7.0);
        assert!(d.seven_day.unwrap().resets_at_ms.is_some());
    }

    #[test]
    fn missing_fields_are_none_and_bad_json_errors() {
        let d = parse_statusline_payload("{}").unwrap();
        assert_eq!(d, StatuslineData::default());
        assert!(parse_statusline_payload("nope").is_err());
        assert!(parse_statusline_payload("[1]").is_err());
    }

    #[test]
    fn record_writes_snapshots() {
        let s = Store::open_in_memory().unwrap();
        let d = parse_statusline_payload(
            r#"{"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":1790000000}},"cost":{"total_cost_usd":0.2}}"#,
        )
        .unwrap();
        assert_eq!(record_statusline(&s, &d, 1_700_000_000_000).unwrap(), 2);
        assert_eq!(s.latest_rate_limits().unwrap().len(), 1);
    }
}
