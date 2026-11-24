//! `agents_svc` - real, local, offline token-usage accounting for Claude Code, Codex, OpenCode and Antigravity.
//!
//! Nothing here is invented: every figure comes from the agents' own log files (or, for limits, from
//! rate-limit numbers the providers themselves wrote into those logs / the statusLine payload), and each
//! cost is labelled "estimated" ([`pricing::COST_LABEL`]) because it applies a local price table.
//!
//! ```no_run
//! use agents_svc::{Ingestor, IngestConfig, Range};
//! let mut ing = Ingestor::open(std::path::Path::new("selachi.db"), IngestConfig::from_home())?;
//! let report = ing.scan()?;
//! let week = ing.store().usage_summary(&Range::ThisWeek)?;
//! # let _ = (report, week);
//! # Ok::<(), anyhow::Error>(())
//! ```
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod db;
pub mod ingest;
pub mod model;
pub mod opencode;
pub mod pricing;
pub mod query;
pub mod range;
pub mod statusline;
pub mod watch;

pub use db::{Store, UpsertOutcome};
pub use ingest::{IngestConfig, Ingestor, ScanReport, SourceKind};
pub use model::{Agent, FileTouch, RateLimitSnapshot, TokenCounts, UsageEvent, UsageExtras};
pub use pricing::{ModelPrice, PricingTable, COST_LABEL};
pub use query::{AgentUsage, BranchUsage, BurnRate, FileTouched, Forecast, ReportedCost};
pub use range::{Bounds, Range};
pub use statusline::{
    parse_statusline_payload, record_statusline, StatuslineData, StatuslineLimit,
};
pub use watch::{Changes, LogWatcher};
