//! Plain data types shared across the crate.

use std::fmt;

/// Which coding agent produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Token counters of one billable class set. Conventions (identical for both agents so the
/// pricing formula is uniform):
/// * `input` is NON-cached input tokens (Codex reports cached tokens inside `input_tokens`;
///   they are subtracted at ingest and stored in `cache_read`).
/// * `output` includes reasoning/thinking tokens (both agents already count them there).
/// * `cache_create_1h` is the part of `cache_create` written with the 1-hour TTL (Claude only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_create_1h: u64,
    pub cache_read: u64,
}

impl TokenCounts {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }

    pub fn is_zero(&self) -> bool {
        self.total() == 0
    }

    pub fn add(&mut self, o: &TokenCounts) {
        self.input += o.input;
        self.output += o.output;
        self.cache_create += o.cache_create;
        self.cache_create_1h += o.cache_create_1h;
        self.cache_read += o.cache_read;
    }
}

/// One accounted model response / turn.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEvent {
    pub agent: Agent,
    pub session_id: String,
    /// Unix milliseconds (UTC).
    pub ts_ms: i64,
    pub model: String,
    pub tokens: TokenCounts,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// Globally unique dedupe key.
    pub msg_key: String,
}

/// A file the agent edited/created (path only, never content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTouch {
    pub agent: Agent,
    pub session_id: String,
    pub ts_ms: i64,
    pub path: String,
    pub tool: String,
    /// Unique key (tool-use id + index) so re-ingesting never duplicates.
    pub touch_key: String,
}

/// A rate-limit observation. `used_percent` is 0..=100.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitSnapshot {
    pub agent: Agent,
    pub ts_ms: i64,
    /// `five_hour`, `seven_day`, or `<N>m` for other window lengths.
    pub window: String,
    pub used_percent: f64,
    /// Unix milliseconds when the window resets, if known.
    pub resets_at_ms: Option<i64>,
    /// Where the number came from, e.g. `codex-rollout`, `claude-statusline`.
    pub source: String,
}

/// Label a window of `minutes` length.
pub fn window_label(minutes: u64) -> String {
    match minutes {
        300 => "five_hour".to_string(),
        10080 => "seven_day".to_string(),
        m => format!("{m}m"),
    }
}

/// Length of a window label in milliseconds (inverse of [`window_label`]).
pub fn window_len_ms(label: &str) -> Option<i64> {
    match label {
        "five_hour" => Some(5 * 3_600_000),
        "seven_day" => Some(7 * 86_400_000),
        other => other
            .strip_suffix('m')
            .and_then(|n| n.parse::<i64>().ok())
            .map(|m| m * 60_000),
    }
}

pub(crate) fn parse_ts_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Normalise a path for prefix matching: forward slashes, lowercase, no trailing slash.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/").to_lowercase();
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}
