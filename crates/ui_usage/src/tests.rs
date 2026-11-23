use std::sync::Arc;

use agents_svc::{
    Agent, AgentUsage, RateLimitSnapshot, Range, ReportedCost, Store, TokenCounts, UsageEvent,
};
use chrono::{FixedOffset, Local, TimeZone};
use gpui::{AppContext as _, TestAppContext};
use panel_api::{PanelCtx, PanelZone, WidthClass};
use theme::Theme;

use crate::data::*;
use crate::fmt::format_when;
use crate::model::UsageModel;
use crate::model::testing::FakeSource;
use crate::source::load_from;
use crate::view::UsagePanel;

const H: i64 = 3_600_000;
const MIN: i64 = 60_000;

fn now() -> i64 {
    now_ms()
}

fn event(agent: Agent, key: &str, ts_ms: i64, t: TokenCounts, branch: Option<&str>, cwd: &str) -> UsageEvent {
    UsageEvent {
        agent,
        session_id: "sess-1".into(),
        ts_ms,
        model: match agent {
            Agent::Claude => "claude-opus-5-5",
            Agent::Codex => "gpt-6.1-sol",
            Agent::OpenCode => "mystery-model",
            Agent::Antigravity => "gemini-4-neon",
        }
        .into(),
        tokens: t,
        cwd: Some(cwd.into()),
        git_branch: branch.map(str::to_string),
        msg_key: key.into(),
    }
}

fn tok(input: u64, output: u64) -> TokenCounts {
    TokenCounts { input, output, cache_create: 0, cache_create_1h: 0, cache_read: 0 }
}

fn snapshot_row(agent: Agent, window: &str, ts_ms: i64, pct: f64, resets: Option<i64>) -> RateLimitSnapshot {
    RateLimitSnapshot {
        agent,
        ts_ms,
        window: window.into(),
        used_percent: pct,
        resets_at_ms: resets,
        source: "claude-statusline".into(),
    }
}

// ----- range math ------------------------------------------------------------------------------

#[test]
fn range_selector_maps_to_agents_svc_ranges() {
    assert_eq!(UsageRange::ALL.map(UsageRange::label), ["Sesión", "Hoy", "Semana", "7 días"]);
    assert_eq!(UsageRange::Today.to_range(None), Some(Range::Today));
    assert_eq!(UsageRange::Week.to_range(None), Some(Range::ThisWeek));
    assert_eq!(UsageRange::Last7Days.to_range(None), Some(Range::Last7Days));
    assert_eq!(UsageRange::Session.to_range(Some("abc")), Some(Range::Session("abc".into())));
    // No session id: no range, so the panel says "sin sesión activa" instead of a zero.
    assert_eq!(UsageRange::Session.to_range(None), None);
    // Cycling covers all four and wraps; indexes round-trip.
    let mut r = UsageRange::Session;
    let mut seen = Vec::new();
    for _ in 0..5 {
        seen.push(r);
        r = r.next();
    }
    assert_eq!(
        seen,
        [UsageRange::Session, UsageRange::Today, UsageRange::Week, UsageRange::Last7Days, UsageRange::Session]
    );
    assert!(UsageRange::ALL.iter().all(|r| UsageRange::from_index(r.index()) == *r));
}

#[test]
fn chart_days_are_fourteen_contiguous_local_days_ending_today() {
    let tz = FixedOffset::east_opt(2 * 3600).unwrap();
    let now = tz.with_ymd_and_hms(2026, 11, 21, 9, 30, 0).unwrap();
    let days = chart_day_bounds(&now);
    assert_eq!(days.len(), CHART_DAYS);
    for w in days.windows(2) {
        assert_eq!(w[0].2, w[1].1, "end of a day is the start of the next");
        assert_eq!((w[1].0 - w[0].0).num_days(), 1);
    }
    let (date, start, end) = days[CHART_DAYS - 1];
    assert_eq!(date, now.date_naive());
    assert!(start <= now.timestamp_millis() && now.timestamp_millis() < end);
    assert_eq!(end - start, 24 * H);
}

// ----- official limits: never an invented percentage ----------------------------------------------

#[test]
fn no_percentage_without_a_fresh_snapshot() {
    let n = now();
    let none: Vec<RateLimitSnapshot> = vec![];
    assert_eq!(limit_state(&none, Agent::Claude, LimitWindow::FiveHour, n), LimitState::Missing);
    assert_eq!(LimitState::Missing.percent(), None);

    // The window already reset: the old 80 % is not shown.
    let expired = vec![snapshot_row(Agent::Claude, "five_hour", n - 6 * H, 80.0, Some(n - H))];
    let st = limit_state(&expired, Agent::Claude, LimitWindow::FiveHour, n);
    assert!(matches!(st, LimitState::Stale { .. }));
    assert_eq!(st.percent(), None);

    // Unknown reset and older than 30 minutes: stale too.
    let unknown = vec![snapshot_row(Agent::Claude, "five_hour", n - 45 * MIN, 30.0, None)];
    assert_eq!(limit_state(&unknown, Agent::Claude, LimitWindow::FiveHour, n).percent(), None);

    // A snapshot of another agent or window never leaks in.
    let other = vec![snapshot_row(Agent::Codex, "five_hour", n - MIN, 50.0, Some(n + H))];
    assert_eq!(limit_state(&other, Agent::Claude, LimitWindow::FiveHour, n), LimitState::Missing);
    assert_eq!(limit_state(&other, Agent::Codex, LimitWindow::SevenDay, n), LimitState::Missing);
}

#[test]
fn a_fresh_snapshot_shows_the_reported_percent_reset_and_freshness() {
    let n = now();
    let rows = vec![
        snapshot_row(Agent::Claude, "five_hour", n - 10 * MIN, 42.0, Some(n + 2 * H + 13 * MIN)),
        // An older reading of the same window must lose against the newer one.
        snapshot_row(Agent::Claude, "five_hour", n - 3 * H, 90.0, Some(n + 2 * H + 13 * MIN)),
    ];
    let st = limit_state(&rows, Agent::Claude, LimitWindow::FiveHour, n);
    let LimitState::Fresh { percent, resets_in_ms, age_ms, .. } = st.clone() else { panic!("{st:?}") };
    assert_eq!(percent, 42.0);
    assert_eq!(reset_label(resets_in_ms.unwrap()), "reinicia en 2 h 13 min");
    assert_eq!(freshness_label(age_ms), "oficial · hace 10 min");
    assert_eq!(st.percent(), Some(42.0));
    // Out-of-range provider numbers are clamped, not trusted blindly.
    let over = vec![snapshot_row(Agent::Codex, "seven_day", n, 130.0, Some(n + H))];
    assert_eq!(limit_state(&over, Agent::Codex, LimitWindow::SevenDay, n).percent(), Some(100.0));
}

#[test]
fn limit_blocks_exist_for_agents_with_a_snapshot_or_activity() {
    let mut snap = UsageSnapshot::default();
    assert!(limit_agents(&snap).is_empty());
    snap.limits.push(snapshot_row(Agent::Codex, "five_hour", now(), 10.0, Some(now() + H)));
    assert_eq!(limit_agents(&snap), vec![Agent::Codex]);
    snap.agents = aggregate_agents(
        &[AgentUsage {
            agent: Agent::Claude,
            model: "claude-opus-5-5".into(),
            input: 1,
            output: 1,
            cache_create: 0,
            cache_read: 0,
            events: 1,
            est_cost_usd: Some(0.1),
        }],
        &[],
    );
    // Claude has activity but no official data: its block says so (it is never hidden or guessed).
    assert_eq!(limit_agents(&snap), vec![Agent::Claude, Agent::Codex]);
}

// ----- forecast gating -------------------------------------------------------------------------------

fn seeded_store(snapshot: Option<RateLimitSnapshot>) -> Store {
    let store = Store::open_in_memory().unwrap();
    let n = now();
    // Before the snapshot (calibration) and in the last hour (current pace).
    store.upsert_usage(&event(Agent::Claude, "a", n - 2 * H, tok(300_000, 100_000), Some("main"), "C:/p/selachi")).unwrap();
    store.upsert_usage(&event(Agent::Claude, "c", n - 45 * MIN, tok(200_000, 100_000), Some("main"), "C:/p/selachi")).unwrap();
    store.upsert_usage(&event(Agent::Claude, "b", n - 20 * MIN, tok(600_000, 200_000), Some("feat/x"), "C:/p/selachi")).unwrap();
    if let Some(s) = snapshot {
        store.insert_rate_limit(&s).unwrap();
    }
    store
}

#[test]
fn forecast_only_with_a_real_unexpired_snapshot() {
    let n = now();
    let query = UsageQuery { range: UsageRange::Today, ..UsageQuery::default() };

    // No snapshot: no forecast and no limit rows.
    let s = load_from(&seeded_store(None), &query).unwrap();
    assert!(s.forecasts.is_empty() && s.limits.is_empty());
    assert!(forecast_lines(&s.forecasts, &Local::now(), 2).is_empty());

    // Expired snapshot: the store returns None, and the state is Stale (no %).
    let expired = snapshot_row(Agent::Claude, "five_hour", n - 30 * MIN, 40.0, Some(n - MIN));
    let s = load_from(&seeded_store(Some(expired)), &query).unwrap();
    assert!(s.forecasts.is_empty());
    assert_eq!(limit_state(&s.limits, Agent::Claude, LimitWindow::FiveHour, n).percent(), None);

    // Real, unexpired snapshot: one forecast anchored on it.
    let fresh = snapshot_row(Agent::Claude, "five_hour", n - 30 * MIN, 40.0, Some(n + 4 * H));
    let s = load_from(&seeded_store(Some(fresh)), &query).unwrap();
    assert_eq!(s.forecasts.len(), 1);
    assert_eq!(s.forecasts[0].snapshot_source, "claude-statusline");
    let lines = forecast_lines(&s.forecasts, &Local::now(), 2);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].text.starts_with("Al ritmo actual"), "{}", lines[0].text);
    assert!(lines[0].rate.starts_with("ritmo ") && lines[0].rate.ends_with("tokens/h"));
    assert_eq!(lines[0].label, "estimado");
}

#[test]
fn forecast_text_names_the_day_and_hour() {
    let tz = FixedOffset::east_opt(0).unwrap();
    let now = tz.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).unwrap(); // Wednesday
    let eta = tz.with_ymd_and_hms(2026, 11, 19, 14, 0, 0).unwrap().timestamp_millis();
    let f = agents_svc::Forecast {
        agent: Agent::Claude,
        window: "seven_day".into(),
        snapshot_used_percent: 40.0,
        snapshot_ts_ms: now.timestamp_millis(),
        snapshot_source: "claude-statusline".into(),
        resets_at_ms: eta + 40 * H,
        tokens_per_percent: 1.0,
        est_used_percent_now: 50.0,
        recent_billable_tokens_per_hour: 412_000.0,
        eta_ms: eta - now.timestamp_millis(),
        eta_at_ms: eta,
        reaches_limit_before_reset: true,
        label: "estimated",
    };
    let line = forecast_line(&f, &now);
    assert_eq!(line.text, "Al ritmo actual llegas al 100 % mañana 14:00");
    assert_eq!(line.rate, "ritmo 412K tokens/h");
    assert_eq!(format_when(&now, eta + 24 * H), "el viernes 14:00");
    // A projection past the reset says the opposite, truthfully.
    let calm = agents_svc::Forecast { reaches_limit_before_reset: false, ..f.clone() };
    assert_eq!(
        forecast_line(&calm, &now).text,
        "Al ritmo actual no llegas al límite de 7 días antes de que reinicie"
    );
    // The soonest limit-hitting forecast comes first and `max` caps the list.
    let other = agents_svc::Forecast { agent: Agent::Codex, eta_at_ms: eta - H, ..f.clone() };
    let lines = forecast_lines(&[calm, f, other], &now, 2);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].agent, Agent::Codex);
    assert!(lines[1].reaches_limit);
}

// ----- chart bucketing ------------------------------------------------------------------------------------

#[test]
fn stack_cells_split_proportionally_and_never_overflow() {
    assert_eq!(stack_cells(&[0, 0, 0], 100, 10), vec![0, 0, 0]);
    assert_eq!(stack_cells(&[100, 0, 0], 100, 10), vec![10, 0, 0]);
    assert_eq!(stack_cells(&[50, 50, 0], 100, 10), vec![5, 5, 0]);
    // A tiny non-zero day still shows one cell, in its own tone.
    assert_eq!(stack_cells(&[1, 0, 0], 1_000_000, 10), vec![1, 0, 0]);
    for values in [[7u64, 3, 2], [1, 1, 1], [90, 5, 5], [0, 1, 99]] {
        let total: u64 = values.iter().sum();
        let cells = stack_cells(&values, total * 2, 12);
        assert_eq!(cells.iter().sum::<usize>(), 6, "{values:?} half-height bar");
        let full = stack_cells(&values, total, 12);
        assert_eq!(full.iter().sum::<usize>(), 12);
        // A present agent keeps at least a cell when there is room for everyone.
        assert!(values.iter().zip(&full).all(|(v, c)| *v == 0 || *c >= 1), "{values:?} -> {full:?}");
    }
    // Rows smaller than the number of agents: the stack still fits.
    assert!(stack_cells(&[5, 5, 5], 15, 2).iter().sum::<usize>() <= 2);
}

#[test]
fn days_bucket_billable_tokens_per_agent_without_cache_reads() {
    let store = Store::open_in_memory().unwrap();
    let local = Local::now();
    let bounds = chart_day_bounds(&local);
    let mid = |i: usize| (bounds[i].1 + bounds[i].2) / 2;
    // Today: Claude with a huge cache read that must not count + Codex.
    let big_cache = TokenCounts { input: 100, output: 50, cache_create: 25, cache_create_1h: 0, cache_read: 9_000_000 };
    store.upsert_usage(&event(Agent::Claude, "t1", mid(13), big_cache, None, "C:/p/a")).unwrap();
    store.upsert_usage(&event(Agent::Codex, "t2", mid(13), tok(1_000, 500), None, "C:/p/a")).unwrap();
    // Three days ago and an event older than the window (not in the chart).
    store.upsert_usage(&event(Agent::Claude, "t3", mid(10), tok(10, 5), None, "C:/p/a")).unwrap();
    // Antigravity (tokens only) is a fourth column of the stack, not a hidden agent.
    store.upsert_usage(&event(Agent::Antigravity, "t4", mid(13), tok(300, 40), None, "C:/p/a")).unwrap();
    store.upsert_usage(&event(Agent::Claude, "old", bounds[0].1 - 5 * 24 * H, tok(77, 77), None, "C:/p/a")).unwrap();

    let snap = load_from(&store, &UsageQuery::default()).unwrap();
    assert_eq!(snap.days.len(), CHART_DAYS);
    assert_eq!(snap.days[13].per_agent, [175, 1_500, 0, 340]);
    assert_eq!(snap.days[10].per_agent, [15, 0, 0, 0]);
    assert_eq!(snap.days[13].total(), 2_015);
    assert!(snap.days.iter().enumerate().all(|(i, d)| i == 13 || i == 10 || d.total() == 0));
    assert_eq!(snap.days[13].date, local.date_naive());
    // Tooltip: date, total, and only the agents that have tokens.
    let tip = day_tooltip(&snap.days[13], local.date_naive());
    assert!(tip.contains("hoy") && tip.contains("2K") && tip.contains("Claude 175") && tip.contains("Codex 1.5K"), "{tip}");
    assert!(tip.contains("Antigravity 340"), "{tip}");
    assert!(!tip.contains("OpenCode"));
    assert!(day_tooltip(&snap.days[0], local.date_naive()).ends_with("sin actividad"));
}

#[test]
fn the_chart_stack_has_a_slot_for_every_agent() {
    // Exhaustive on purpose: a new `Agent` variant stops compiling here until the chart is told.
    let slot = |a: Agent| match a {
        Agent::Claude | Agent::Codex | Agent::OpenCode | Agent::Antigravity => AGENT_ORDER.iter().position(|x| *x == a),
    };
    for a in [Agent::Claude, Agent::Codex, Agent::OpenCode, Agent::Antigravity] {
        assert!(slot(a).is_some(), "{a:?} is not in AGENT_ORDER, so its usage would be hidden");
    }
    assert_eq!(AGENT_ORDER.len(), DayUsage { date: chrono::NaiveDate::MIN, per_agent: Default::default() }.per_agent.len());
    // Four distinct tones, each softer than the one before and never fully transparent.
    let t = Theme::dark();
    let tones: Vec<f32> = (0..AGENT_ORDER.len()).map(|i| crate::chart::slot_tone(&t, i, false).a).collect();
    assert!(tones.windows(2).all(|w| w[0] > w[1]) && tones.iter().all(|a| *a > 0.15), "{tones:?}");
    // Four agents on a short bar still keep one cell each when there is room.
    assert_eq!(stack_cells(&[10, 10, 10, 10], 40, 8), vec![2, 2, 2, 2]);
    assert!(stack_cells(&[10, 10, 10, 10], 40, 3).iter().sum::<usize>() <= 3);
}

#[test]
fn the_legend_lists_only_agents_that_have_tokens_antigravity_included() {
    let d = |per_agent| DayUsage { date: chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(), per_agent };
    let days = [d([5, 0, 0, 0]), d([0, 0, 0, 9])];
    let names: Vec<_> = crate::view::legend_agents(&days).into_iter().map(|(slot, a)| (slot, agent_name(a))).collect();
    assert_eq!(names, vec![(0, "Claude"), (3, "Antigravity")]);
    assert!(matches!(agent_mark(Agent::Antigravity), ui_kit::AgentMark::Antigravity));
}

// ----- agents, costs, branches ----------------------------------------------------------------------------

fn usage_row(agent: Agent, model: &str, events: u64, cost: Option<f64>) -> AgentUsage {
    AgentUsage {
        agent,
        model: model.into(),
        input: 1_000,
        output: 500,
        cache_create: 250,
        cache_read: 99_000,
        events,
        est_cost_usd: cost,
    }
}

#[test]
fn agent_cards_count_billable_tokens_and_label_every_cost() {
    let rows = vec![
        usage_row(Agent::Claude, "claude-opus-5-5", 4, Some(1.5)),
        usage_row(Agent::Claude, "claude-haiku-5-5", 2, Some(0.5)),
        usage_row(Agent::OpenCode, "x", 3, Some(0.8)),
        usage_row(Agent::Codex, "unknown", 1, None),
    ];
    let reported = vec![ReportedCost { agent: Agent::OpenCode, model: "x".into(), source: "opencode".into(), cost_usd: 0.8, events: 3 }];
    let agents = aggregate_agents(&rows, &reported);
    assert_eq!(agents.iter().map(|a| a.agent).collect::<Vec<_>>(), vec![Agent::Claude, Agent::Codex, Agent::OpenCode]);
    // Antigravity rows get their own card, in the fixed order.
    let with_agy = aggregate_agents(&[usage_row(Agent::Antigravity, "gemini-4-neon", 2, None), usage_row(Agent::Claude, "a", 1, Some(1.0))], &[]);
    assert_eq!(with_agy.iter().map(|a| a.agent).collect::<Vec<_>>(), vec![Agent::Claude, Agent::Antigravity]);

    let claude = &agents[0];
    assert_eq!((claude.input, claude.output, claude.cache_create, claude.cache_read, claude.events), (2_000, 1_000, 500, 198_000, 6));
    assert_eq!(claude.billable(), 3_500, "cache reads are not billable");
    assert_eq!(cost_label(&claude.cost), "$2.00 · estimado");

    // Nothing could be priced: no number, no guess.
    assert_eq!(cost_label(&agents[1].cost), "coste: sin precio para el modelo");
    // Reported by the agent itself: labelled with its source, not "estimado".
    assert_eq!(cost_label(&agents[2].cost), "$0.80 · reportado por opencode");

    // Partial pricing is a lower bound and says so.
    let partial = aggregate_agents(
        &[usage_row(Agent::Claude, "a", 1, Some(1.0)), usage_row(Agent::Claude, "b", 1, None)],
        &[],
    );
    assert_eq!(cost_label(&partial[0].cost), "≥ $1.00 · estimado (faltan precios)");
    // Mixed reported + estimated.
    let reported_part = vec![ReportedCost { agent: Agent::OpenCode, model: "x".into(), source: "opencode".into(), cost_usd: 0.1, events: 1 }];
    let mixed = aggregate_agents(&[usage_row(Agent::OpenCode, "x", 3, Some(0.8))], &reported_part);
    assert_eq!(cost_label(&mixed[0].cost), "$0.80 · reportado por opencode + estimado");
    // Agents without events do not get a card.
    assert!(aggregate_agents(&[usage_row(Agent::Codex, "m", 0, None)], &[]).is_empty());

    let tip = breakdown_tooltip(claude);
    assert!(tip.contains("Facturables 3.500 = entrada 2.000 + salida 1.000 + caché creada 500"), "{tip}");
    assert!(tip.contains("Lecturas de caché 198.000 (no cuentan)"), "{tip}");
}

#[test]
fn branches_flag_incomplete_costs_and_need_a_project() {
    let store = Store::open_in_memory().unwrap();
    let n = now();
    store.upsert_usage(&event(Agent::Claude, "1", n - H, tok(1_000_000, 100_000), Some("feat/x"), "C:/p/selachi")).unwrap();
    store.upsert_usage(&event(Agent::Claude, "2", n - 2 * H, tok(10_000, 1_000), Some("main"), "C:/p/selachi/crates")).unwrap();
    // OpenCode on a model with no price: that branch cost is partial.
    store.upsert_usage(&event(Agent::OpenCode, "3", n - 3 * H, tok(500, 500), Some("main"), "C:/p/selachi")).unwrap();
    store.upsert_usage(&event(Agent::Claude, "4", n - 3 * H, tok(500, 500), Some("other"), "C:/elsewhere")).unwrap();

    let q = UsageQuery { range: UsageRange::Last7Days, session_id: None, project_root: Some("C:\\p\\selachi".into()) };
    let snap = load_from(&store, &q).unwrap();
    let branches = snap.branches.as_ref().unwrap();
    assert_eq!(branches.len(), 2, "only this project's branches: {branches:?}");
    assert_eq!(branches[0].git_branch.as_deref(), Some("feat/x"));
    assert!(branches[0].cost_complete);
    let main = branches.iter().find(|b| b.git_branch.as_deref() == Some("main")).unwrap();
    assert!(!main.cost_complete);
    assert!(cost_short(main.est_cost_usd, main.cost_complete).starts_with('≥'));
    assert_eq!(cost_short(None, true), "—");

    // No project root: no list at all (the view explains it), not an empty one.
    let none = load_from(&store, &UsageQuery { project_root: None, ..q }).unwrap();
    assert!(none.branches.is_none());
}

#[test]
fn the_session_range_without_a_session_id_is_flagged_not_zeroed() {
    let store = seeded_store(None);
    let q = UsageQuery { range: UsageRange::Session, ..UsageQuery::default() };
    let snap = load_from(&store, &q).unwrap();
    assert!(snap.session_missing && snap.agents.is_empty() && snap.branches.is_none());
    // Chart and limits still load: they do not depend on the range.
    assert_eq!(snap.days.len(), CHART_DAYS);

    let q = UsageQuery { session_id: Some("sess-1".into()), ..q };
    let snap = load_from(&store, &q).unwrap();
    assert!(!snap.session_missing);
    assert_eq!(snap.total_billable(), 1_500_000);
}

// ----- model: on demand, nothing while hidden ----------------------------------------------------------------------

fn new_model(cx: &mut TestAppContext) -> (gpui::Entity<UsageModel>, Arc<FakeSource>) {
    let src = FakeSource::new();
    let s = src.clone();
    (cx.new(move |_| UsageModel::with_source(s)), src)
}

#[gpui::test]
async fn hidden_means_no_queries_and_a_scan_while_hidden_waits_for_the_next_paint(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    cx.executor().advance_clock(std::time::Duration::from_secs(600));
    cx.run_until_parked();
    assert_eq!(src.calls(), 0, "no timer, no query");

    model.update(cx, |m, cx| {
        m.notify_scanned(cx);
        m.set_range(UsageRange::Week, cx);
        m.set_project_root(Some("C:/p".into()), cx);
    });
    cx.run_until_parked();
    assert_eq!(src.calls(), 0, "a scan or a range change while hidden does not query");

    // Becoming visible runs exactly one query for everything that changed meanwhile.
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 1);
    let snap = model.read_with(cx, |m, _| m.snapshot().cloned()).unwrap();
    assert_eq!((snap.query.range, snap.query.project_root.as_deref()), (UsageRange::Week, Some("C:/p")));

    // Visible and idle: still nothing on its own, however long it takes...
    cx.executor().advance_clock(std::time::Duration::from_secs(3_600));
    cx.run_until_parked();
    assert_eq!(src.calls(), 1);
    // ...until the host reports a scan.
    model.update(cx, |m, cx| m.notify_scanned(cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 2);

    // Hide again: scans only mark it dirty.
    model.update(cx, |m, cx| m.set_visible(false, cx));
    model.update(cx, |m, cx| m.notify_scanned(cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 2);
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 3);
    // Re-showing with nothing new does not query again.
    model.update(cx, |m, cx| {
        m.set_visible(false, cx);
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    assert_eq!(src.calls(), 3);
}

#[gpui::test]
async fn requests_during_a_query_are_coalesced_and_failures_keep_the_old_data(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| {
        m.set_visible(true, cx);
        // Three requests while the first query is still in flight.
        m.notify_scanned(cx);
        m.set_range(UsageRange::Last7Days, cx);
        m.set_project_root(Some("C:/q".into()), cx);
    });
    cx.run_until_parked();
    assert_eq!(src.calls(), 2, "one query plus a single follow-up");
    let snap = model.read_with(cx, |m, _| m.snapshot().cloned()).unwrap();
    assert_eq!(snap.query.range, UsageRange::Last7Days);

    src.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    model.update(cx, |m, cx| m.notify_scanned(cx));
    cx.run_until_parked();
    assert_eq!(model.read_with(cx, |m, _| m.error().map(|e| e.to_string())), Some("boom".to_string()));
    assert!(model.read_with(cx, |m, _| m.snapshot().is_some()), "the previous snapshot stays");
    src.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    model.update(cx, |m, cx| m.notify_scanned(cx));
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| m.error().is_none()));
}

#[gpui::test]
async fn a_model_without_a_source_never_queries_and_a_session_change_only_matters_for_the_session_range(
    cx: &mut TestAppContext,
) {
    let model = cx.new(|_| UsageModel::new());
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| m.snapshot().is_none() && !m.has_source() && !m.is_loading()));

    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 1);
    model.update(cx, |m, cx| m.set_session_id(Some("s".into()), cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 1, "Hoy does not depend on the session");
    model.update(cx, |m, cx| m.set_range(UsageRange::Session, cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 2);
    let snap = model.read_with(cx, |m, _| m.snapshot().cloned()).unwrap();
    assert_eq!(snap.query.session_id.as_deref(), Some("s"));
}

// ----- the view: compact switching ---------------------------------------------------------------------------------

#[gpui::test]
async fn the_panel_paints_in_every_width_and_zone_and_switches_to_compact(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.text_system().add_fonts(theme::fonts::embedded()).ok();
    });
    let (model, src) = new_model(cx);
    let ctx = |zone, w| PanelCtx::new(zone, w, Theme::light()).with_project_root("C:/p/selachi");
    let (panel, vcx) = {
        let m = model.clone();
        cx.add_window_view(move |_, cx| UsagePanel::new(ctx(PanelZone::Right, WidthClass::Regular), m, cx))
    };
    vcx.run_until_parked();
    assert!(!panel.read_with(vcx, |p, _| p.is_compact()));
    // Painting made the model visible and loaded once, with the panel's project.
    assert_eq!(src.calls(), 1);
    assert_eq!(model.read_with(vcx, |m, _| m.project_root().map(str::to_string)), Some("C:/p/selachi".to_string()));

    panel.update(vcx, |p, cx| p.retarget(ctx(PanelZone::Sidebar, WidthClass::Compact), cx));
    vcx.run_until_parked();
    assert!(panel.read_with(vcx, |p, _| p.is_compact()));
    panel.update(vcx, |p, cx| p.retarget(ctx(PanelZone::Main, WidthClass::Regular), cx));
    vcx.run_until_parked();
    assert!(!panel.read_with(vcx, |p, _| p.is_compact()));
    assert_eq!(src.calls(), 1, "re-targeting to the same project does not query");

    // Releasing the view hides the model: scans stop querying.
    drop(panel);
    vcx.update(|window, _| window.remove_window());
    vcx.run_until_parked();
    model.update(vcx, |m, cx| m.notify_scanned(cx));
    vcx.run_until_parked();
    assert!(!model.read_with(vcx, |m, _| m.is_visible()));
    assert_eq!(src.calls(), 1);
}
