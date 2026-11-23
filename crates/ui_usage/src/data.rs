//! Plain data of the panel: what a query returns ([`UsageSnapshot`]) and the pure derivations the
//! view paints (limit freshness, forecast line, cost label, chart buckets). Nothing here touches
//! gpui, so all of it is unit-tested.

use agents_svc::{
    Agent, AgentUsage, BranchUsage, Forecast, RateLimitSnapshot, ReportedCost,
};
use chrono::{DateTime, Local, NaiveDate, TimeZone, Weekday};

use crate::fmt::{compact_tokens, days_back, format_duration, format_when};

/// Days in the bar chart (today included).
pub const CHART_DAYS: usize = 14;

/// A rate-limit snapshot with an unknown reset time is only trusted for this long.
pub const UNKNOWN_RESET_MAX_AGE_MS: i64 = 30 * 60_000;

/// Spanish for `agents_svc::COST_LABEL` ("estimated"), shown next to every extrapolated figure.
pub const ESTIMATED: &str = "estimado";

/// The agents that report official limits, in display order.
pub const LIMIT_AGENTS: [Agent; 2] = [Agent::Claude, Agent::Codex];

/// Display order of agents everywhere (cards, chart stack, legend).
pub const AGENT_ORDER: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::OpenCode, Agent::Antigravity];

pub fn agent_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude",
        Agent::Codex => "Codex",
        Agent::OpenCode => "OpenCode",
        Agent::Antigravity => "Antigravity",
    }
}

pub fn agent_mark(agent: Agent) -> ui_kit::AgentMark {
    match agent {
        Agent::Claude => ui_kit::AgentMark::Claude,
        Agent::Codex => ui_kit::AgentMark::Codex,
        Agent::OpenCode => ui_kit::AgentMark::OpenCode,
        Agent::Antigravity => ui_kit::AgentMark::Antigravity,
    }
}

// ---------------------------------------------------------------------------------------------
// Range selector
// ---------------------------------------------------------------------------------------------

/// The four periods of the selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum UsageRange {
    /// The active agent session (needs a session id from the host).
    Session,
    #[default]
    Today,
    /// Calendar week.
    Week,
    /// Rolling 7 days.
    Last7Days,
}

impl UsageRange {
    pub const ALL: [UsageRange; 4] =
        [UsageRange::Session, UsageRange::Today, UsageRange::Week, UsageRange::Last7Days];

    pub fn label(self) -> &'static str {
        match self {
            UsageRange::Session => "Sesión",
            UsageRange::Today => "Hoy",
            UsageRange::Week => "Semana",
            UsageRange::Last7Days => "7 días",
        }
    }

    /// Lowercase form for sentences ("tokens facturables · hoy").
    pub fn phrase(self) -> &'static str {
        match self {
            UsageRange::Session => "esta sesión",
            UsageRange::Today => "hoy",
            UsageRange::Week => "esta semana",
            UsageRange::Last7Days => "últimos 7 días",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }

    pub fn from_index(ix: usize) -> Self {
        Self::ALL.get(ix).copied().unwrap_or_default()
    }

    /// The next range, wrapping (compact widths cycle with one pill).
    pub fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }

    /// The `agents_svc` range. `None` for [`UsageRange::Session`] without a session id: the panel
    /// then says "sin sesión activa" instead of showing a made-up zero.
    pub fn to_range(self, session_id: Option<&str>) -> Option<agents_svc::Range> {
        match self {
            UsageRange::Session => session_id.map(|s| agents_svc::Range::Session(s.to_string())),
            UsageRange::Today => Some(agents_svc::Range::Today),
            UsageRange::Week => Some(agents_svc::Range::ThisWeek),
            UsageRange::Last7Days => Some(agents_svc::Range::Last7Days),
        }
    }
}

/// What the model asks a [`crate::UsageSource`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UsageQuery {
    pub range: UsageRange,
    /// Active agent session (for [`UsageRange::Session`]).
    pub session_id: Option<String>,
    /// Project root whose branches are listed ("Por rama").
    pub project_root: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------------------------

/// Where a cost comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CostKind {
    /// Computed with the local price table (`COST_LABEL`: "estimated").
    Estimated,
    /// Every event carried a cost the agent reported itself (OpenCode); the string is its source.
    Reported(String),
    /// Some events reported their cost, the rest was estimated.
    Mixed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CostInfo {
    /// Sum of the costs that could be determined; `None` when nothing could be priced.
    pub usd: Option<f64>,
    /// False when some model had no price (the sum is then a lower bound).
    pub complete: bool,
    pub kind: CostKind,
}

/// One agent's usage in the selected range.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentTotals {
    pub agent: Agent,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    pub events: u64,
    pub cost: CostInfo,
}

impl AgentTotals {
    /// What counts against limits and bills: input + output + cache creation. Cache *reads* are
    /// served from the prompt cache and would dwarf the real usage, so they are not part of it.
    pub fn billable(&self) -> u64 {
        self.input + self.output + self.cache_create
    }
}

/// Billable tokens of one local day, per agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayUsage {
    pub date: NaiveDate,
    /// In [`AGENT_ORDER`] order (agents with 0 included, so indexes are stable).
    pub per_agent: [u64; AGENT_ORDER.len()],
}

impl DayUsage {
    pub fn total(&self) -> u64 {
        self.per_agent.iter().sum()
    }
}

/// Everything one refresh produces. Cheap to clone (the model hands out an `Arc`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct UsageSnapshot {
    pub query: UsageQuery,
    /// The range needs a session id and the host gave none.
    pub session_missing: bool,
    /// Agents with activity in the range, in [`AGENT_ORDER`] order.
    pub agents: Vec<AgentTotals>,
    /// Latest official snapshot per agent and window (may be stale: see [`limit_state`]).
    pub limits: Vec<RateLimitSnapshot>,
    /// Only the forecasts `agents_svc` could compute from a real snapshot, soonest first.
    pub forecasts: Vec<Forecast>,
    /// `None` = no project root was given (as opposed to a project without activity).
    pub branches: Option<Vec<BranchUsage>>,
    /// Oldest first, exactly [`CHART_DAYS`] entries when the query succeeded.
    pub days: Vec<DayUsage>,
}

impl UsageSnapshot {
    pub fn total_billable(&self) -> u64 {
        self.agents.iter().map(AgentTotals::billable).sum()
    }
}

// ---------------------------------------------------------------------------------------------
// Aggregation of agents_svc rows
// ---------------------------------------------------------------------------------------------

/// Folds the per-(agent, model) rows of `usage_summary` and the agent-reported costs of
/// `reported_costs` into one [`AgentTotals`] per agent that had events.
pub fn aggregate_agents(rows: &[AgentUsage], reported: &[ReportedCost]) -> Vec<AgentTotals> {
    AGENT_ORDER
        .iter()
        .filter_map(|&agent| {
            let mine: Vec<&AgentUsage> = rows.iter().filter(|r| r.agent == agent).collect();
            let events: u64 = mine.iter().map(|r| r.events).sum();
            if events == 0 {
                return None;
            }
            let sum = |f: fn(&AgentUsage) -> u64| mine.iter().map(|r| f(r)).sum::<u64>();
            let priced: Vec<f64> = mine.iter().filter_map(|r| r.est_cost_usd).collect();
            let reported_events: u64 =
                reported.iter().filter(|r| r.agent == agent).map(|r| r.events).sum();
            let source = reported
                .iter()
                .find(|r| r.agent == agent)
                .map(|r| r.source.clone())
                .unwrap_or_else(|| agent.as_str().to_string());
            let kind = if reported_events == 0 {
                CostKind::Estimated
            } else if reported_events >= events {
                CostKind::Reported(source)
            } else {
                CostKind::Mixed(source)
            };
            Some(AgentTotals {
                agent,
                input: sum(|r| r.input),
                output: sum(|r| r.output),
                cache_create: sum(|r| r.cache_create),
                cache_read: sum(|r| r.cache_read),
                events,
                cost: CostInfo {
                    usd: if priced.is_empty() { None } else { Some(priced.iter().sum()) },
                    complete: priced.len() == mine.len(),
                    kind,
                },
            })
        })
        .collect()
}

/// One line about a cost, always saying where it comes from:
/// `$4.10 · estimado`, `≥ $4.10 · estimado (faltan precios)`, `$1.20 · reportado por opencode`.
pub fn cost_label(cost: &CostInfo) -> String {
    use crate::fmt::format_cost;
    let origin = match &cost.kind {
        CostKind::Estimated => "estimado".to_string(),
        CostKind::Reported(s) => format!("reportado por {s}"),
        CostKind::Mixed(s) => format!("reportado por {s} + estimado"),
    };
    match (cost.usd, cost.complete) {
        (None, _) => "coste: sin precio para el modelo".to_string(),
        (Some(usd), true) => format!("{} · {origin}", format_cost(usd)),
        (Some(usd), false) => format!("≥ {} · {origin} (faltan precios)", format_cost(usd)),
    }
}

/// Short cost for a list row meta: `$4.10`, `≥ $4.10`, `—`.
pub fn cost_short(usd: Option<f64>, complete: bool) -> String {
    use crate::fmt::format_cost;
    match (usd, complete) {
        (None, _) => "—".to_string(),
        (Some(u), true) => format_cost(u),
        (Some(u), false) => format!("≥ {}", format_cost(u)),
    }
}

/// The multi-line tooltip of an agent card: the billable breakdown and the cache reads that do
/// not count.
pub fn breakdown_tooltip(t: &AgentTotals) -> String {
    use crate::fmt::full_tokens;
    format!(
        "{name}\nFacturables {total} = entrada {inp} + salida {out} + caché creada {cc}\nLecturas de caché {cr} (no cuentan)\n{events} eventos\n{cost}",
        name = agent_name(t.agent),
        total = full_tokens(t.billable()),
        inp = full_tokens(t.input),
        out = full_tokens(t.output),
        cc = full_tokens(t.cache_create),
        cr = full_tokens(t.cache_read),
        events = crate::fmt::events_label(t.events),
        cost = cost_label(&t.cost),
    )
}

// ---------------------------------------------------------------------------------------------
// Official limits
// ---------------------------------------------------------------------------------------------

/// The two windows the panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitWindow {
    FiveHour,
    SevenDay,
}

impl LimitWindow {
    pub const ALL: [LimitWindow; 2] = [LimitWindow::FiveHour, LimitWindow::SevenDay];

    /// The `agents_svc` window key.
    pub fn key(self) -> &'static str {
        match self {
            LimitWindow::FiveHour => "five_hour",
            LimitWindow::SevenDay => "seven_day",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LimitWindow::FiveHour => "5 h",
            LimitWindow::SevenDay => "7 días",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.key() == key)
    }
}

/// What the panel may say about one (agent, window).
#[derive(Clone, Debug, PartialEq)]
pub enum LimitState {
    /// A real provider number that is still valid.
    Fresh {
        /// 0..=100 as reported.
        percent: f32,
        /// Milliseconds until the window resets, when known.
        resets_in_ms: Option<i64>,
        age_ms: i64,
        source: String,
    },
    /// There is an old snapshot (its window already reset, or too old): no percentage is shown.
    Stale { age_ms: i64 },
    /// Never seen.
    Missing,
}

impl LimitState {
    /// The share to draw in a meter. `None` unless the snapshot is fresh: **never invented**.
    pub fn percent(&self) -> Option<f32> {
        match self {
            LimitState::Fresh { percent, .. } => Some(*percent),
            _ => None,
        }
    }
}

/// Decides if the latest snapshot of a window can still be shown. It can when its window has not
/// reset yet; a snapshot without a reset time is trusted for [`UNKNOWN_RESET_MAX_AGE_MS`] only.
pub fn limit_state(
    limits: &[RateLimitSnapshot],
    agent: Agent,
    window: LimitWindow,
    now_ms: i64,
) -> LimitState {
    let latest = limits
        .iter()
        .filter(|l| l.agent == agent && l.window == window.key())
        .max_by_key(|l| l.ts_ms);
    let Some(s) = latest else { return LimitState::Missing };
    let age_ms = (now_ms - s.ts_ms).max(0);
    let valid = match s.resets_at_ms {
        Some(r) => r > now_ms,
        None => age_ms <= UNKNOWN_RESET_MAX_AGE_MS,
    };
    if !valid || !s.used_percent.is_finite() {
        return LimitState::Stale { age_ms };
    }
    LimitState::Fresh {
        percent: s.used_percent.clamp(0.0, 100.0) as f32,
        resets_in_ms: s.resets_at_ms.map(|r| r - now_ms),
        age_ms,
        source: s.source.clone(),
    }
}

/// `oficial · hace 2 min` for a fresh state.
pub fn freshness_label(age_ms: i64) -> String {
    format!("oficial · {}", crate::fmt::format_age(age_ms))
}

/// `reinicia en 2 h 13 min`.
pub fn reset_label(resets_in_ms: i64) -> String {
    format!("reinicia en {}", format_duration(resets_in_ms))
}

/// Which agents get a block in "Límites oficiales": the limit-reporting agents that have either
/// a snapshot or activity in the range.
pub fn limit_agents(snapshot: &UsageSnapshot) -> Vec<Agent> {
    LIMIT_AGENTS
        .into_iter()
        .filter(|a| {
            snapshot.limits.iter().any(|l| l.agent == *a)
                || snapshot.agents.iter().any(|t| t.agent == *a)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Forecast
// ---------------------------------------------------------------------------------------------

/// What the forecast lines say.
#[derive(Clone, Debug, PartialEq)]
pub struct ForecastLine {
    pub agent: Agent,
    pub window: &'static str,
    /// `Al ritmo actual llegas al 100 % el jueves 14:00`.
    pub text: String,
    /// `ritmo 412K tokens/h`.
    pub rate: String,
    /// "estimado": extrapolated locally from a real snapshot.
    pub label: &'static str,
    pub reaches_limit: bool,
}

/// The line for one forecast. `now` fixes the clock and the timezone.
pub fn forecast_line<Tz: TimeZone>(f: &Forecast, now: &DateTime<Tz>) -> ForecastLine {
    let window = LimitWindow::from_key(&f.window).map_or("ventana", LimitWindow::label);
    let text = if f.reaches_limit_before_reset {
        format!("Al ritmo actual llegas al 100 % {}", format_when(now, f.eta_at_ms))
    } else {
        format!("Al ritmo actual no llegas al límite de {window} antes de que reinicie")
    };
    ForecastLine {
        agent: f.agent,
        window,
        text,
        rate: format!("ritmo {} tokens/h", compact_tokens(f.recent_billable_tokens_per_hour.round() as u64)),
        label: ESTIMATED,
        reaches_limit: f.reaches_limit_before_reset,
    }
}

/// Lines to show: the forecasts that hit the limit first, at most `max`. Empty when there is no
/// forecast (the line is simply not drawn).
pub fn forecast_lines<Tz: TimeZone>(
    forecasts: &[Forecast],
    now: &DateTime<Tz>,
    max: usize,
) -> Vec<ForecastLine> {
    let mut sorted: Vec<&Forecast> = forecasts.iter().collect();
    // Limit-hitting forecasts first, then by ETA.
    sorted.sort_by_key(|f| (!f.reaches_limit_before_reset, f.eta_at_ms));
    sorted.into_iter().take(max).map(|f| forecast_line(f, now)).collect()
}

// ---------------------------------------------------------------------------------------------
// Chart
// ---------------------------------------------------------------------------------------------

/// Local midnight bounds `[start, end)` of each of the last [`CHART_DAYS`] days, oldest first,
/// with their dates. Uses `agents_svc`'s own "today" resolution so DST is handled in one place.
pub fn chart_day_bounds<Tz: TimeZone>(now: &DateTime<Tz>) -> Vec<(NaiveDate, i64, i64)> {
    let tz = now.timezone();
    (0..CHART_DAYS as i64)
        .rev()
        .map(|back| {
            let date = days_back(now, back);
            // Resolve "today" on that date at noon (always exists, even on DST days).
            let noon = date.and_hms_opt(12, 0, 0).and_then(|t| tz.from_local_datetime(&t).earliest());
            let b = noon
                .map(|n| agents_svc::Range::Today.resolve(&n, Weekday::Mon))
                .unwrap_or_default();
            (date, b.start_ms.unwrap_or(0), b.end_ms.unwrap_or(0))
        })
        .collect()
}

/// Splits `rows` cells of a bar between its stack segments in proportion to `values`
/// (largest remainder). A non-zero total always gets at least one cell, and the stack never
/// exceeds `rows`. `max_total` is the value of a full-height bar.
pub fn stack_cells(values: &[u64], max_total: u64, rows: usize) -> Vec<usize> {
    let total: u64 = values.iter().sum();
    let mut out = vec![0usize; values.len()];
    if total == 0 || max_total == 0 || rows == 0 {
        return out;
    }
    let want = (((total as f64 / max_total as f64) * rows as f64).ceil() as usize).clamp(1, rows);
    let present: Vec<usize> = (0..values.len()).filter(|i| values[*i] > 0).collect();
    if want < present.len() {
        // Not enough cells for everyone: the biggest segments get one each.
        let mut by_size = present;
        by_size.sort_by_key(|i| std::cmp::Reverse(values[*i]));
        for i in by_size.into_iter().take(want) {
            out[i] = 1;
        }
        return out;
    }
    // Every present segment keeps one cell; the rest is shared in proportion (largest remainder).
    let spare = want - present.len();
    let mut rem: Vec<(usize, f64)> = Vec::new();
    let mut used = 0;
    for &i in &present {
        let exact = values[i] as f64 / total as f64 * spare as f64;
        out[i] = 1 + exact.floor() as usize;
        used += exact.floor() as usize;
        rem.push((i, exact - exact.floor()));
    }
    rem.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    for (i, _) in rem.iter().cycle().take(spare - used) {
        out[*i] += 1;
    }
    out
}

/// Tooltip of one chart column.
pub fn day_tooltip(day: &DayUsage, today: NaiveDate) -> String {
    let head = if day.date == today {
        format!("{} · hoy", crate::fmt::day_label(day.date))
    } else {
        crate::fmt::day_label(day.date)
    };
    if day.total() == 0 {
        return format!("{head}\nsin actividad");
    }
    let mut lines = vec![head, format!("{} tokens facturables", compact_tokens(day.total()))];
    for (agent, n) in AGENT_ORDER.iter().zip(day.per_agent) {
        if n > 0 {
            lines.push(format!("{} {}", agent_name(*agent), compact_tokens(n)));
        }
    }
    lines.join("\n")
}

/// `Local::now()` (a seam for the view).
pub fn local_now() -> DateTime<Local> {
    Local::now()
}

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
