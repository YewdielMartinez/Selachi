//! Versioned per-model pricing table ($ per million tokens).

use crate::model::{Agent, TokenCounts};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Embedded default table (`pricing.toml` in the crate root).
pub const EMBEDDED_PRICING: &str = include_str!("../pricing.toml");

/// Label to show next to any cost computed from this table.
pub const COST_LABEL: &str = "estimated";

#[derive(Debug, Clone, Deserialize)]
pub struct ModelPrice {
    pub id: String,
    #[serde(default)]
    pub agent: Option<String>,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PricingFile {
    version: String,
    #[serde(default)]
    model: Vec<ModelPrice>,
}

#[derive(Debug, Clone)]
pub struct PricingTable {
    pub version: String,
    pub models: Vec<ModelPrice>,
}

/// Lowercase, drop a trailing `[...]` suffix (e.g. `[1m]`) and a `-YYYYMMDD` date suffix.
pub fn normalize_model_id(id: &str) -> String {
    let mut s = id.trim().to_lowercase();
    if let Some(i) = s.find('[') {
        s.truncate(i);
    }
    if let Some(i) = s.rfind('-') {
        let tail = &s[i + 1..];
        if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) {
            s.truncate(i);
        }
    }
    s
}

impl PricingTable {
    pub fn embedded() -> PricingTable {
        Self::from_toml(EMBEDDED_PRICING).expect("embedded pricing.toml must be valid")
    }

    pub fn from_toml(s: &str) -> Result<PricingTable> {
        let f: PricingFile = toml::from_str(s).context("parsing pricing table")?;
        Ok(PricingTable {
            version: f.version,
            models: f.model,
        })
    }

    pub fn from_path(p: &Path) -> Result<PricingTable> {
        let s = std::fs::read_to_string(p)
            .with_context(|| format!("reading pricing table {}", p.display()))?;
        Self::from_toml(&s)
    }

    /// Load the user override if given and present, else the embedded table.
    pub fn load(user_override: Option<&Path>) -> Result<PricingTable> {
        match user_override {
            Some(p) if p.exists() => Self::from_path(p),
            _ => Ok(Self::embedded()),
        }
    }

    /// Exact (normalised) match only; never a fuzzy/prefix guess.
    pub fn lookup(&self, agent: Agent, model: &str) -> Option<&ModelPrice> {
        let m = normalize_model_id(model);
        self.models.iter().find(|p| {
            normalize_model_id(&p.id) == m && p.agent.as_deref().is_none_or(|a| a == agent.as_str())
        })
    }

    /// Estimated cost in USD, or `None` when the model or a needed price is unknown.
    pub fn cost(&self, agent: Agent, model: &str, t: &TokenCounts) -> Option<f64> {
        let p = self.lookup(agent, model)?;
        let mut usd = t.input as f64 * p.input + t.output as f64 * p.output;
        let cc_1h = t.cache_create_1h.min(t.cache_create);
        let cc_5m = t.cache_create - cc_1h;
        if cc_5m > 0 {
            usd += cc_5m as f64 * p.cache_write?;
        }
        if cc_1h > 0 {
            usd += cc_1h as f64 * p.cache_write_1h.or(p.cache_write)?;
        }
        if t.cache_read > 0 {
            usd += t.cache_read as f64 * p.cache_read?;
        }
        Some(usd / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_parses_and_prices_known_models() {
        let t = PricingTable::embedded();
        let tok = TokenCounts {
            input: 1_000_000,
            output: 1_000_000,
            cache_create: 1_000_000,
            cache_create_1h: 1_000_000,
            cache_read: 1_000_000,
        };
        let c = t.cost(Agent::Claude, "claude-opus-5-5[1m]", &tok).unwrap();
        assert!((c - (4.0 + 20.0 + 8.0 + 0.2)).abs() < 1e-9);
        assert!(t.cost(Agent::Claude, "claude-unknown-9", &tok).is_none());
        // Agent restriction: a codex model never prices a claude event.
        assert!(t.cost(Agent::Claude, "gpt-6.1-sol", &tok).is_none());
    }

    #[test]
    fn missing_price_for_used_class_is_none() {
        let t = PricingTable::embedded();
        let only_in_out = TokenCounts {
            input: 10,
            output: 10,
            ..Default::default()
        };
        assert!(t
            .cost(Agent::Claude, "claude-haiku-5-5", &only_in_out)
            .is_some());
        let with_read = TokenCounts {
            cache_read: 5,
            ..only_in_out
        };
        assert!(t
            .cost(Agent::Claude, "claude-haiku-5-5", &with_read)
            .is_none());
    }

    #[test]
    fn normalizes_date_suffix() {
        assert_eq!(
            normalize_model_id("Claude-Opus-4-6-20260101"),
            "claude-opus-4-6"
        );
    }
}
