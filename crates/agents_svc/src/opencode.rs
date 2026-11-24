//! OpenCode usage: reads OpenCode's own local session storage (read-only, offline).
//!
//! # Where OpenCode keeps sessions (verified on opencode 1.14.48, Windows)
//!
//! `opencode debug paths` -> data dir `C:\Users\<user>\.local\share\opencode`
//! (`$XDG_DATA_HOME/opencode` when set). Two layouts exist:
//!
//! * **SQLite** (current): `<data>/opencode.db` (WAL mode; `opencode-<channel>.db` for non-stable
//!   channels, `OPENCODE_DB` overrides). Tables `project`, `session`
//!   (`id, project_id, parent_id, directory, title, workspace_id, ..., time_created,
//!   time_updated`), `message` (`id, session_id, time_created, time_updated, data` JSON),
//!   `part`, `workspace` (`id, branch, directory`), `todo`, `event`, ... Created by the CLI itself on
//!   first run ("one time database migration", which also imports the legacy JSON).
//! * **Legacy JSON** (before the SQLite migration, and kept by it): one file per record,
//!   `<data>/storage/message/<sessionID>/<messageID>.json` (same JSON as the `data` column
//!   plus `id` / `sessionID`).
//!
//! The `message.data` JSON of an assistant message (opencode `MessageV2.Assistant`, source
//! `packages/opencode/src/session/message-v2.ts` at tag v1.14.48) is:
//!
//! ```json
//! { "role": "assistant", "time": {"created": 1, "completed": 2}, "parentID": "msg_..",
//!   "modelID": "claude-sonnet-4-5", "providerID": "anthropic", "mode": "build", "agent": "build",
//!   "path": {"cwd": "C:\\work\\proj", "root": "C:\\work\\proj"},
//!   "cost": 0.0123,
//!   "tokens": {"total": 100, "input": 10, "output": 20, "reasoning": 5,
//!              "cache": {"read": 60, "write": 5}},
//!   "finish": "stop" }
//! ```
//!
//! Token semantics (`packages/opencode/src/session/session.ts` `getUsage`): `input` is already the
//! NON-cached input (cache read/write subtracted), `output` EXCLUDES reasoning, `cost` is computed
//! from the models.dev catalogue and is `0` for free or unpriced models. Totals are accumulated
//! across the steps of the message and rewritten in place, hence "last write wins"
//! ([`crate::Store::upsert_usage_replace`]) and the `time_updated` cursor.
//!
//! # Mapping to [`UsageEvent`]
//!
//! `input` -> `input`; `output + reasoning` -> `output` (reasoning also kept in
//! [`UsageExtras::reasoning`]); `cache.read` -> `cache_read`; `cache.write` -> `cache_create`;
//! `cost > 0` -> reported cost (source `opencode`), else none; key `opencode:<message id>`;
//! `ts` = `time.completed` (else `time.created`); `cwd` = `path.cwd` (else the session's
//! `directory`); git branch = `workspace.branch` of the session's workspace when it has one
//! (OpenCode does not record the branch of ordinary sessions).
//!
//! Not read: `session_v2` / `session_part` (the in-development v2 schema keeps per-session
//! aggregates only), credentials (`account`, `control_account`) and message/part text.

use crate::model::{Agent, TokenCounts, UsageEvent, UsageExtras};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `cost_source` written for OpenCode-reported costs.
pub const COST_SOURCE: &str = "opencode";

/// Result of parsing one stored message.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Usage(Box<(UsageEvent, UsageExtras)>),
    /// A user message (or anything that is not an assistant message).
    NotAssistant,
    /// An assistant message without any tokens or cost yet (still starting, or failed early).
    Empty,
}

fn num(v: Option<&Value>) -> u64 {
    match v {
        Some(v) => v
            .as_u64()
            .or_else(|| {
                v.as_f64()
                    .filter(|f| f.is_finite() && *f > 0.0)
                    .map(|f| f.round() as u64)
            })
            .unwrap_or(0),
        None => 0,
    }
}

/// Parse one `message.data` JSON object. `fallback_ts_ms` / `session_dir` / `branch` come from
/// the surrounding rows when the JSON lacks them.
pub fn parse_message(
    message_id: &str,
    session_id: &str,
    fallback_ts_ms: Option<i64>,
    data: &Value,
    session_dir: Option<&str>,
    branch: Option<&str>,
) -> Parsed {
    if data.get("role").and_then(Value::as_str) != Some("assistant") {
        return Parsed::NotAssistant;
    }
    let Some(tokens) = data.get("tokens").filter(|t| t.is_object()) else {
        return Parsed::Empty;
    };
    let reasoning = num(tokens.get("reasoning"));
    let counts = TokenCounts {
        input: num(tokens.get("input")),
        output: num(tokens.get("output")) + reasoning,
        cache_create: num(tokens.pointer("/cache/write")),
        cache_create_1h: 0,
        cache_read: num(tokens.pointer("/cache/read")),
    };
    let cost = data
        .get("cost")
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite() && *c > 0.0);
    if counts.is_zero() && cost.is_none() {
        return Parsed::Empty;
    }
    let ts = data
        .pointer("/time/completed")
        .or_else(|| data.pointer("/time/created"))
        .and_then(Value::as_i64)
        .or(fallback_ts_ms)
        .unwrap_or(0);
    let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    let model = data
        .get("modelID")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .unwrap_or_else(|| "unknown".to_string());
    let cwd = data
        .pointer("/path/cwd")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .or_else(|| session_dir.and_then(non_empty));
    let event = UsageEvent {
        agent: Agent::OpenCode,
        session_id: session_id.to_string(),
        ts_ms: ts,
        model,
        tokens: counts,
        cwd,
        git_branch: branch.and_then(non_empty),
        msg_key: format!("opencode:{message_id}"),
    };
    let extras = UsageExtras {
        reasoning,
        reported_cost_usd: cost,
        cost_source: cost.map(|_| COST_SOURCE.to_string()),
        provider: data
            .get("providerID")
            .and_then(Value::as_str)
            .and_then(non_empty),
    };
    Parsed::Usage(Box::new((event, extras)))
}

/// OpenCode's data directory: `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`.
pub fn default_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|h| PathBuf::from(h).join(".local").join("share"))
        })?;
    Some(base.join("opencode"))
}

/// SQLite databases in the data dir (`opencode.db`, `opencode-<channel>.db`, `$OPENCODE_DB`).
pub fn find_dbs(data_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("OPENCODE_DB").map(PathBuf::from) {
        let p = if p.is_absolute() { p } else { data_dir.join(p) };
        if p.is_file() {
            out.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(data_dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("opencode") && name.ends_with(".db") && e.path().is_file() {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Legacy `storage/message/<session>/<message>.json` files.
pub fn legacy_message_files(data_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(sessions) = std::fs::read_dir(data_dir.join("storage").join("message")) else {
        return out;
    };
    for s in sessions.flatten() {
        if !s.path().is_dir() {
            continue;
        }
        let Ok(msgs) = std::fs::read_dir(s.path()) else {
            continue;
        };
        out.extend(
            msgs.flatten()
                .map(|m| m.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json") && p.is_file()),
        );
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant() -> Value {
        json!({
            "role": "assistant",
            "time": {"created": 1000, "completed": 2000},
            "parentID": "msg_u1", "modelID": "claude-sonnet-4-5", "providerID": "anthropic",
            "mode": "build", "agent": "build",
            "path": {"cwd": "C:\\work\\proj", "root": "C:\\work\\proj"},
            "cost": 0.0123,
            "tokens": {"total": 100, "input": 10, "output": 20, "reasoning": 5, "cache": {"read": 60, "write": 5}},
            "finish": "stop"
        })
    }

    fn usage(p: Parsed) -> (UsageEvent, UsageExtras) {
        match p {
            Parsed::Usage(b) => *b,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn maps_tokens_cost_and_context() {
        let (e, x) = usage(parse_message(
            "msg_1",
            "ses_1",
            Some(5),
            &assistant(),
            Some("C:\\other"),
            Some("main"),
        ));
        assert_eq!(e.agent, Agent::OpenCode);
        assert_eq!(e.msg_key, "opencode:msg_1");
        assert_eq!(e.session_id, "ses_1");
        assert_eq!(e.ts_ms, 2000, "completed time wins");
        assert_eq!(e.model, "claude-sonnet-4-5");
        assert_eq!(
            e.tokens,
            TokenCounts {
                input: 10,
                output: 25,
                cache_create: 5,
                cache_create_1h: 0,
                cache_read: 60
            }
        );
        assert_eq!(
            e.cwd.as_deref(),
            Some("C:\\work\\proj"),
            "message path beats session dir"
        );
        assert_eq!(e.git_branch.as_deref(), Some("main"));
        assert_eq!(x.reasoning, 5);
        assert_eq!(x.reported_cost_usd, Some(0.0123));
        assert_eq!(x.cost_source.as_deref(), Some("opencode"));
        assert_eq!(x.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn zero_cost_means_unreported_and_falls_back_to_session_dir() {
        let mut d = assistant();
        d["cost"] = json!(0);
        d["path"] = json!({});
        d["time"] = json!({"created": 7});
        let (e, x) = usage(parse_message(
            "m",
            "s",
            Some(99),
            &d,
            Some("D:\\sess"),
            None,
        ));
        assert_eq!(x.reported_cost_usd, None);
        assert_eq!(x.cost_source, None);
        assert_eq!(e.cwd.as_deref(), Some("D:\\sess"));
        assert_eq!(e.ts_ms, 7);
        assert_eq!(e.git_branch, None);
    }

    #[test]
    fn user_and_empty_messages_are_skipped() {
        assert_eq!(
            parse_message(
                "u",
                "s",
                None,
                &json!({"role":"user","time":{"created":1}}),
                None,
                None
            ),
            Parsed::NotAssistant
        );
        let mut d = assistant();
        d["tokens"] = json!({"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}});
        d["cost"] = json!(0);
        assert_eq!(parse_message("m", "s", None, &d, None, None), Parsed::Empty);
        d.as_object_mut().unwrap().remove("tokens");
        assert_eq!(parse_message("m", "s", None, &d, None, None), Parsed::Empty);
    }

    #[test]
    fn tolerates_floats_missing_cache_and_unknown_model() {
        let d = json!({"role":"assistant","time":{"created":3},"tokens":{"input":10.0,"output":2.6,"reasoning":0}});
        let (e, _) = usage(parse_message("m", "s", None, &d, None, None));
        assert_eq!(e.tokens.input, 10);
        assert_eq!(e.tokens.output, 3);
        assert_eq!(e.tokens.cache_read, 0);
        assert_eq!(e.model, "unknown");
    }

    #[test]
    fn find_dbs_and_legacy_files() {
        let dir = std::env::temp_dir().join("agents_svc_oc_find");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("storage").join("message").join("ses_1")).unwrap();
        std::fs::write(dir.join("opencode.db"), b"").unwrap();
        std::fs::write(dir.join("opencode-dev.db"), b"").unwrap();
        std::fs::write(dir.join("other.db"), b"").unwrap();
        std::fs::write(
            dir.join("storage")
                .join("message")
                .join("ses_1")
                .join("msg_1.json"),
            b"{}",
        )
        .unwrap();
        std::fs::write(
            dir.join("storage")
                .join("message")
                .join("ses_1")
                .join("note.txt"),
            b"x",
        )
        .unwrap();
        let dbs = find_dbs(&dir);
        assert_eq!(dbs.len(), 2);
        assert!(dbs.iter().all(|p| p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("opencode")));
        assert_eq!(legacy_message_files(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
