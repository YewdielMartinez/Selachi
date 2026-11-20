//! Codex CLI rollout parser (`~/.codex/sessions/**/rollout-*.jsonl[.zst]`).
//!
//! Schema (verified on real rollouts): every line is `{timestamp, type, payload, ordinal}`.
//! * `session_meta` -> `payload.{id, session_id, cwd, model_provider, cli_version}`; the FIRST one in a
//!   file describes that file's thread (later ones are parent-thread copies). `session_id` is the
//!   root session (sub-agent threads share it); `id` is this file's thread id.
//! * `turn_context` -> `payload.{cwd, model, effort, ...}` (updates cwd/model).
//! * `event_msg` with `payload.type == "token_count"` ->
//!   `payload.info.{total_token_usage, last_token_usage}` each
//!   `{input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens,
//!     reasoning_output_tokens, total_tokens}` (cached is a subset of input; reasoning a subset of
//!   output) and `payload.rate_limits` =
//!   `{primary:{used_percent, window_minutes, resets_at(epoch s)}, secondary, plan_type, ...}`
//!   (present on nearly every token_count; `null` occasionally).
//! * `response_item` / `custom_tool_call` named `apply_patch` -> `payload.input` patch text, from which
//!   only `*** Add/Update/Delete File:` paths are extracted. (Most Codex edits go through an `exec`
//!   JS tool whose arguments are opaque, so file-touch coverage for Codex is partial.)
//!
//! `total_token_usage` is cumulative and strictly increasing; `last_token_usage` is the delta of the
//! turn. We count `last_token_usage` once per distinct cumulative total (the same event is often
//! repeated when only rate limits changed).

use crate::model::{
    parse_ts_ms, window_label, Agent, FileTouch, RateLimitSnapshot, TokenCounts, UsageEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parser state carried across lines (and, for plain `.jsonl`, across scans).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CodexCtx {
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub meta_seen: bool,
    pub last_total: u64,
    /// Fingerprint of the last recorded rate-limit state (avoids one row per event).
    pub last_rl: Option<String>,
}

#[derive(Debug)]
pub enum CodexItem {
    Usage(UsageEvent),
    Touch(FileTouch),
    RateLimit(RateLimitSnapshot),
    /// token_count whose cumulative total did not advance (repeat / rate-limit-only).
    NoNewUsage,
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn tokens_from(v: &Value) -> TokenCounts {
    let input = u(v, "input_tokens");
    let cached = u(v, "cached_input_tokens").min(input);
    TokenCounts {
        input: input - cached,
        output: u(v, "output_tokens"),
        cache_create: u(v, "cache_write_input_tokens"),
        cache_create_1h: 0,
        cache_read: cached,
    }
}

/// Parse a line, updating `ctx`. `Err` = malformed JSON. `fallback_session` (e.g. the file stem) is
/// used only when the rollout carries no `session_meta`.
pub fn parse_line(
    ctx: &mut CodexCtx,
    line: &str,
    fallback_session: &str,
) -> Result<Vec<CodexItem>, serde_json::Error> {
    let interesting = line.contains("session_meta")
        || line.contains("turn_context")
        || line.contains("token_count")
        || line.contains("apply_patch");
    if !interesting {
        return if crate::claude::looks_like_object(line) {
            Ok(vec![])
        } else {
            Err(serde::de::Error::custom("not a JSON object"))
        };
    }
    let v: Value = serde_json::from_str(line)?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = v.get("payload").unwrap_or(&Value::Null);
    let ts_ms = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts_ms);
    let mut out = vec![];

    match kind {
        "session_meta" if !ctx.meta_seen => {
            ctx.meta_seen = true;
            ctx.thread_id = payload.get("id").and_then(Value::as_str).map(String::from);
            ctx.session_id = payload
                .get("session_id")
                .and_then(Value::as_str)
                .or_else(|| payload.get("id").and_then(Value::as_str))
                .map(String::from);
            if let Some(c) = payload.get("cwd").and_then(Value::as_str) {
                ctx.cwd = Some(c.to_string());
            }
            if let Some(m) = payload.get("model").and_then(Value::as_str) {
                ctx.model = Some(m.to_string());
            }
        }
        "turn_context" => {
            if let Some(c) = payload.get("cwd").and_then(Value::as_str) {
                ctx.cwd = Some(c.to_string());
            }
            if let Some(m) = payload.get("model").and_then(Value::as_str) {
                ctx.model = Some(m.to_string());
            }
        }
        "event_msg" if payload.get("type").and_then(Value::as_str) == Some("token_count") => {
            let Some(ts_ms) = ts_ms else {
                return Ok(out);
            };
            let session = ctx
                .session_id
                .clone()
                .unwrap_or_else(|| fallback_session.to_string());
            let thread = ctx
                .thread_id
                .clone()
                .unwrap_or_else(|| fallback_session.to_string());

            // Rate limits (real, provided by the Codex backend and echoed in the rollout).
            if let Some(rl) = payload.get("rate_limits").filter(|r| r.is_object()) {
                let mut fp = String::new();
                let mut snaps = vec![];
                for slot in ["primary", "secondary"] {
                    let Some(w) = rl.get(slot).filter(|w| w.is_object()) else {
                        continue;
                    };
                    let Some(pct) = w.get("used_percent").and_then(Value::as_f64) else {
                        continue;
                    };
                    let mins = w.get("window_minutes").and_then(Value::as_u64);
                    let resets = w.get("resets_at").and_then(Value::as_i64).map(|s| s * 1000);
                    let window = mins.map(window_label).unwrap_or_else(|| slot.to_string());
                    fp.push_str(&format!("{slot}:{pct}:{resets:?}:{window};"));
                    snaps.push(RateLimitSnapshot {
                        agent: Agent::Codex,
                        ts_ms,
                        window,
                        used_percent: pct,
                        resets_at_ms: resets,
                        source: "codex-rollout".to_string(),
                    });
                }
                if !snaps.is_empty() && ctx.last_rl.as_deref() != Some(fp.as_str()) {
                    ctx.last_rl = Some(fp);
                    out.extend(snaps.into_iter().map(CodexItem::RateLimit));
                }
            }

            if let Some(info) = payload.get("info").filter(|i| i.is_object()) {
                let total = info
                    .get("total_token_usage")
                    .map(|t| u(t, "total_tokens"))
                    .unwrap_or(0);
                if total > ctx.last_total {
                    ctx.last_total = total;
                    let tokens = info
                        .get("last_token_usage")
                        .map(tokens_from)
                        .unwrap_or_default();
                    // Resumed/forked rollouts start with a snapshot whose per-turn components are
                    // zero; nothing to account for there.
                    if !tokens.is_zero() {
                        out.push(CodexItem::Usage(UsageEvent {
                            agent: Agent::Codex,
                            session_id: session,
                            ts_ms,
                            model: ctx.model.clone().unwrap_or_else(|| "unknown".to_string()),
                            tokens,
                            cwd: ctx.cwd.clone(),
                            git_branch: None,
                            msg_key: format!("codex:{thread}:{total}"),
                        }));
                    }
                } else {
                    out.push(CodexItem::NoNewUsage);
                }
            }
        }
        "response_item"
            if payload.get("type").and_then(Value::as_str) == Some("custom_tool_call")
                && payload.get("name").and_then(Value::as_str) == Some("apply_patch") =>
        {
            if let (Some(ts_ms), Some(input)) =
                (ts_ms, payload.get("input").and_then(Value::as_str))
            {
                let call = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                let session = ctx
                    .session_id
                    .clone()
                    .unwrap_or_else(|| fallback_session.to_string());
                for (i, path) in patch_paths(input).into_iter().enumerate() {
                    out.push(CodexItem::Touch(FileTouch {
                        agent: Agent::Codex,
                        session_id: session.clone(),
                        ts_ms,
                        path,
                        tool: "apply_patch".to_string(),
                        touch_key: format!("codex:{call}:{i}"),
                    }));
                }
            }
        }
        _ => {}
    }
    Ok(out)
}

/// Paths named in an `apply_patch` envelope (paths only; patch bodies are never stored).
pub fn patch_paths(patch: &str) -> Vec<String> {
    const MARKERS: &[&str] = &[
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    patch
        .lines()
        .filter_map(|l| {
            MARKERS
                .iter()
                .find_map(|m| l.strip_prefix(m))
                .map(|p| p.trim().to_string())
        })
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(ts: &str, total: u64, inp: u64, cached: u64, out: u64, rl: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{inp},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":1,"total_tokens":{}}}}},"rate_limits":{rl}}}}}"#,
            inp + out
        )
    }

    #[test]
    fn counts_each_total_once_and_splits_cached() {
        let mut ctx = CodexCtx::default();
        let meta = r#"{"timestamp":"2026-03-02T10:00:00.000Z","type":"session_meta","payload":{"id":"th1","session_id":"root1","cwd":"C:\\p"}}"#;
        parse_line(&mut ctx, meta, "f").unwrap();
        let turn = r#"{"timestamp":"2026-03-02T10:00:01.000Z","type":"turn_context","payload":{"model":"gpt-6.1-sol","cwd":"C:\\p"}}"#;
        parse_line(&mut ctx, turn, "f").unwrap();
        let l = tc("2026-03-02T10:00:02.000Z", 120, 100, 40, 20, "null");
        let r1 = parse_line(&mut ctx, &l, "f").unwrap();
        let r2 = parse_line(&mut ctx, &l, "f").unwrap();
        match &r1[0] {
            CodexItem::Usage(e) => {
                assert_eq!(e.tokens.input, 60);
                assert_eq!(e.tokens.cache_read, 40);
                assert_eq!(e.tokens.output, 20);
                assert_eq!(e.session_id, "root1");
                assert_eq!(e.model, "gpt-6.1-sol");
            }
            o => panic!("{o:?}"),
        }
        assert!(matches!(r2[0], CodexItem::NoNewUsage));
    }

    #[test]
    fn rate_limits_recorded_on_change_only() {
        let mut ctx = CodexCtx::default();
        let rl = r#"{"primary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1790000000},"secondary":null}"#;
        let a = parse_line(
            &mut ctx,
            &tc("2026-03-02T10:00:02.000Z", 10, 5, 0, 5, rl),
            "f",
        )
        .unwrap();
        assert!(matches!(a[0], CodexItem::RateLimit(_)));
        let b = parse_line(
            &mut ctx,
            &tc("2026-03-02T10:00:03.000Z", 20, 5, 0, 5, rl),
            "f",
        )
        .unwrap();
        assert!(!b.iter().any(|i| matches!(i, CodexItem::RateLimit(_))));
    }

    #[test]
    fn patch_paths_extracted() {
        let p = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: b.txt\n+z\n*** End Patch";
        assert_eq!(patch_paths(p), vec!["src/a.rs", "b.txt"]);
    }
}
