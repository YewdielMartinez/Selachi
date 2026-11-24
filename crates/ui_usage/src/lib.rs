//! `ui_usage`: the usage panel (PLAN sections 6 and 12), read-only over `agents_svc`.
//!
//! * [`UsageModel`]: the data, refreshed **on demand**. No timers: the host calls
//!   [`UsageModel::notify_scanned`] after each scan of the agents' logs (the app scans every 60 s
//!   while focused), and queries run on the background executor only while the panel is visible.
//! * [`UsagePanelProvider`]: the `PanelProvider` with id `"usage"` (Right | Sidebar | Main |
//!   Bottom). Range selector, per-agent cards, official 5 h / 7 day limits with dither meters
//!   (never an invented percentage), forecast, cost per branch and a 14-day pixel chart; below
//!   320 px only the headline tokens and the meters.
//! * [`thumbnail`]: the 1-bit wireframe for the layout panel.
//!
//! ```ignore
//! let model = cx.new(|_| UsageModel::new());
//! model.update(cx, |m, cx| m.set_db_path(appdata.join("Selachi").join("selachi.db"), cx));
//! registry.register(UsagePanelProvider::new(model.clone()));
//! // after every successful scan:
//! model.update(cx, |m, cx| m.notify_scanned(cx));
//! ```
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod chart;
mod data;
mod fmt;
mod model;
mod source;
mod view;

pub use chart::{bar_geometry, slot_tone};
pub use data::{
    AGENT_ORDER, AgentTotals, CHART_DAYS, CostInfo, CostKind, DayUsage, ForecastLine, LIMIT_AGENTS,
    LimitState, LimitWindow, UNKNOWN_RESET_MAX_AGE_MS, UsageQuery, UsageRange, UsageSnapshot,
    aggregate_agents, breakdown_tooltip, chart_day_bounds, cost_label, cost_short, day_tooltip,
    forecast_line, forecast_lines, freshness_label, limit_agents, limit_state, reset_label,
    stack_cells,
};
pub use fmt::{
    compact_tokens, day_label, events_label, format_age, format_cost, format_duration, format_when,
    full_tokens,
};
pub use model::UsageModel;
pub use source::{FORECAST_RECENT, StoreSource, UsageSource, load_from};
pub use view::{PANEL_ID, UsagePanel, UsagePanelProvider, thumbnail};

#[cfg(test)]
mod tests;
