//! Claude Code transcript parser (`~/.claude/projects/**/*.jsonl`).
//!
//! Schema (verified on real transcripts; only assistant entries matter):
//! `{type:"assistant", timestamp, sessionId, cwd, gitBranch, requestId, uuid,
//!   message:{id, model, usage:{input_tokens, output_tokens, cache_creation_input_tokens,
//!   cache_read_input_tokens, cache_creation:{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}},
//!   content:[{type:"tool_use", id, name, input:{file_path|notebook_path}}]}, quotaLimits?}`
//!
//! The same `message.id` + `requestId` is written several times while a response streams;
//! `msg_key` makes them one event (the store keeps the chunk with the most output tokens).

use crate::model::{parse_ts_ms, Agent, FileTouch, RateLimitSnapshot, TokenCounts, UsageEvent};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct ClaudeLine {
    pub usage: Option<UsageEvent>,
    pub touches: Vec<FileTouch>,
    pub rate_limit: Option<RateLimitSnapshot>,
    /// `<synthetic>` / all-zero usage entries that were deliberately not counted.
    pub skipped_synthetic: bool,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    uuid: Option<String>,
    message: Option<RawMessage>,
    #[serde(rename = "quotaLimits")]
    quota_limits: Option<Value>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
    content: Option<Value>,
}

#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation: Option<RawCacheCreation>,
}

#[derive(Deserialize, Default)]
struct RawCacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: Option<u64>,
}

const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Cheap structural check used before parsing (and for lines we skip without parsing).
pub(crate) fn looks_like_object(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('{') && t.ends_with('}')
}

/// Parse one transcript line. `Err` means malformed JSON; `Ok(None)` means "not relevant".
pub fn parse_line(line: &str) -> Result<Option<ClaudeLine>, serde_json::Error> {
    // Only assistant entries carry usage. Skip the (huge) others without a full parse, but
    // still reject lines that are not even shaped like a JSON object.
    if !line.contains("\"assistant\"") {
        return if looks_like_object(line) {
            Ok(None)
        } else {
            Err(serde::de::Error::custom("not a JSON object"))
        };
    }
    let raw: Raw = serde_json::from_str(line)?;
    if raw.kind.as_deref() != Some("assistant") {
        return Ok(None);
    }
    let Some(ts_ms) = raw.timestamp.as_deref().and_then(parse_ts_ms) else {
        return Ok(None);
    };
    let session_id = raw.session_id.clone().unwrap_or_default();
    let mut out = ClaudeLine::default();

    // Real rate-limit rejection recorded by Claude Code in the transcript.
    if let Some(q) = &raw.quota_limits {
        if q.get("status").and_then(Value::as_str) == Some("rejected") {
            let window = q
                .get("rateLimitType")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let resets = q.get("resetsAt").and_then(Value::as_i64).map(|s| s * 1000);
            out.rate_limit = Some(RateLimitSnapshot {
                agent: Agent::Claude,
                ts_ms,
                window,
                used_percent: 100.0,
                resets_at_ms: resets,
                source: "claude-transcript-rejected".to_string(),
            });
        }
    }

    let Some(msg) = raw.message else {
        return Ok(Some(out));
    };

    if let Some(Value::Array(blocks)) = &msg.content {
        for b in blocks {
            if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(name) = b.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !EDIT_TOOLS.contains(&name) {
                continue;
            }
            let input = b.get("input");
            let path = input
                .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")))
                .and_then(Value::as_str);
            if let Some(path) = path {
                let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                let key = if id.is_empty() {
                    format!(
                        "claude:{}:{}:{}",
                        raw.uuid.as_deref().unwrap_or(""),
                        name,
                        path
                    )
                } else {
                    format!("claude:{id}")
                };
                out.touches.push(FileTouch {
                    agent: Agent::Claude,
                    session_id: session_id.clone(),
                    ts_ms,
                    path: path.to_string(),
                    tool: name.to_string(),
                    touch_key: key,
                });
            }
        }
    }

    if let Some(u) = msg.usage {
        let model = msg.model.unwrap_or_default();
        let cache_create = u.cache_creation_input_tokens.unwrap_or(0);
        let tokens = TokenCounts {
            input: u.input_tokens.unwrap_or(0),
            output: u.output_tokens.unwrap_or(0),
            cache_create,
            cache_create_1h: u
                .cache_creation
                .and_then(|c| c.ephemeral_1h_input_tokens)
                .unwrap_or(0)
                .min(cache_create),
            cache_read: u.cache_read_input_tokens.unwrap_or(0),
        };
        if model.is_empty() || model == "<synthetic>" || tokens.is_zero() {
            out.skipped_synthetic = true;
        } else {
            let key = match (&msg.id, &raw.uuid) {
                (Some(id), _) => {
                    format!("claude:{}:{}", id, raw.request_id.as_deref().unwrap_or(""))
                }
                (None, Some(uuid)) => format!("claude:uuid:{uuid}"),
                (None, None) => {
                    // Nothing stable to dedupe on: fall back to a content fingerprint.
                    format!(
                        "claude:fp:{session_id}:{ts_ms}:{}:{}:{}:{}",
                        tokens.input, tokens.output, tokens.cache_create, tokens.cache_read
                    )
                }
            };
            out.usage = Some(UsageEvent {
                agent: Agent::Claude,
                session_id,
                ts_ms,
                model,
                tokens,
                cwd: raw.cwd,
                git_branch: raw.git_branch.filter(|b| !b.is_empty() && b != "HEAD"),
                msg_key: key,
            });
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"type":"assistant","timestamp":"2026-03-02T10:00:00.000Z","sessionId":"s1","cwd":"C:\\p\\a","gitBranch":"feat/x","requestId":"req1","uuid":"u1","message":{"id":"msg1","model":"claude-opus-5-5","usage":{"input_tokens":3,"output_tokens":50,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"cache_creation":{"ephemeral_1h_input_tokens":100,"ephemeral_5m_input_tokens":0}},"content":[{"type":"tool_use","id":"tu1","name":"Edit","input":{"file_path":"C:\\p\\a\\x.rs","old_string":"a","new_string":"b"}}]}}"#;

    #[test]
    fn parses_usage_and_touch() {
        let l = parse_line(LINE).unwrap().unwrap();
        let u = l.usage.unwrap();
        assert_eq!(u.msg_key, "claude:msg1:req1");
        assert_eq!(u.tokens.output, 50);
        assert_eq!(u.tokens.cache_create_1h, 100);
        assert_eq!(u.git_branch.as_deref(), Some("feat/x"));
        assert_eq!(l.touches.len(), 1);
        assert_eq!(l.touches[0].path, "C:\\p\\a\\x.rs");
    }

    #[test]
    fn malformed_and_irrelevant() {
        assert!(parse_line("not json").is_err());
        assert!(parse_line("{\"type\":\"assistant\", oops").is_err());
        assert!(parse_line("{\"type\":\"user\"}").unwrap().is_none());
    }
}
