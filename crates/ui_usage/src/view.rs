//! The usage panel: `PanelProvider` id `"usage"`, and the view that adapts to its width class.
//!
//! Regular: range selector, headline, per-agent cards, official limits with dither meters,
//! forecast, cost per branch and the 14-day chart (two columns in the main / bottom zones).
//! Compact (< 320 px): range pill, headline tokens and the meters only.

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Radius, Theme};
use ui_kit::{
    Card, CardTone, DitherMeter, ListRow, Pill, SectionLabel, Segment, SegmentedControl, Tooltip,
    WireFill, WireRect, Wireframe, agent_icon,
};

use crate::chart::{bar_chart, slot_tone};
use crate::data::{
    AGENT_ORDER, AgentTotals, LimitState, LimitWindow, UsageRange, UsageSnapshot, agent_mark,
    agent_name, breakdown_tooltip, cost_label, cost_short, forecast_lines, freshness_label,
    limit_agents, limit_state, local_now, now_ms, reset_label,
};
use crate::fmt::{compact_tokens, events_label};
use crate::model::UsageModel;

/// Panel id for the registry.
pub const PANEL_ID: &str = "usage";

/// Branch rows shown before "Ver todas".
const BRANCHES_COLLAPSED: usize = 6;
/// Hard cap of rows when expanded (a project can have hundreds of branches).
const BRANCHES_MAX: usize = 60;

/// Registers as `usage`. Hand it the shared [`UsageModel`].
pub struct UsagePanelProvider {
    model: Entity<UsageModel>,
}

impl UsagePanelProvider {
    pub fn new(model: Entity<UsageModel>) -> Self {
        Self { model }
    }
}

impl PanelProvider for UsagePanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Uso".into()
    }

    fn icon(&self) -> Icon {
        Icon::Usage
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Right, PanelZone::Sidebar, PanelZone::Main, PanelZone::Bottom]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let model = self.model.clone();
        cx.new(|cx| UsagePanel::new(ctx, model, cx)).into()
    }

    fn context_changed(&self, view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        let Ok(panel) = view.clone().downcast::<UsagePanel>() else { return false };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

/// 1-bit thumbnail for the layout panel: range pill, a card with two meters and the bar chart.
pub fn thumbnail(theme: &Theme) -> Wireframe {
    use WireFill::{Dither, Outline, Solid};
    let r = WireRect::new;
    Wireframe::new(theme).rects([
        r(1, 1, 12, 2, Solid),
        r(1, 4, 30, 5, Outline),
        r(3, 5, 12, 1, Dither),
        r(3, 7, 8, 1, Dither),
        r(18, 6, 11, 1, Solid),
        r(1, 10, 3, 7, Solid),
        r(6, 12, 3, 5, Solid),
        r(11, 11, 3, 6, Dither),
        r(16, 13, 3, 4, Dither),
        r(21, 9, 3, 8, Solid),
        r(26, 12, 3, 5, Dither),
    ])
}

pub struct UsagePanel {
    ctx: PanelCtx,
    model: Entity<UsageModel>,
    branches_expanded: bool,
    _observe: Subscription,
    _release: Subscription,
}

impl UsagePanel {
    pub fn new(ctx: PanelCtx, model: Entity<UsageModel>, cx: &mut Context<Self>) -> Self {
        let root = project_root_of(&ctx);
        model.update(cx, |m, cx| {
            m.attach_view();
            m.set_project_root(root, cx);
        });
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        let for_release = model.clone();
        let release = cx.on_release(move |_, cx| for_release.update(cx, |m, cx| m.detach_view(cx)));
        Self { ctx, model, branches_expanded: false, _observe: observe, _release: release }
    }

    /// New width class, zone, theme or project root without rebuilding the view.
    pub fn retarget(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        let root = project_root_of(&ctx);
        self.ctx = ctx;
        self.model.update(cx, |m, cx| m.set_project_root(root, cx));
        cx.notify();
    }

    pub fn is_compact(&self) -> bool {
        self.ctx.width_class == WidthClass::Compact
    }
}

/// The project whose branches are listed: the project root, or the cwd when there is none.
fn project_root_of(ctx: &PanelCtx) -> Option<String> {
    ctx.project_root
        .as_ref()
        .or(ctx.cwd.as_ref())
        .map(|p| p.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------------------------
// Small building blocks
// ---------------------------------------------------------------------------------------------

fn muted_text(theme: &Theme, size: f32, text: impl Into<SharedString>) -> gpui::Div {
    div().text_size(px(size)).text_color(theme.colors.muted).child(text.into())
}

fn card(theme: &Theme) -> Card {
    Card::new(theme).tone(CardTone::High, theme).radius(Radius::SM).p(px(12.0)).flex().flex_col().gap(px(6.0))
}

fn section(theme: &Theme, label: &'static str) -> gpui::Div {
    div().flex().flex_col().gap(px(6.0)).child(SectionLabel::new(theme, label))
}

/// Dimmed state line ("Leyendo…", "Sin sesión activa").
fn notice(theme: &Theme, title: &str, detail: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(div().text_size(px(theme.typography.ui_size_lg)).child(title.to_string()))
        .child(muted_text(theme, theme.typography.ui_size_sm, detail.to_string()))
        .into_any_element()
}

fn headline(theme: &Theme, snap: &UsageSnapshot, range: UsageRange) -> AnyElement {
    if snap.session_missing {
        return notice(theme, "Sin sesión activa", "Abre un agente en esta pestaña para ver su sesión.");
    }
    if snap.agents.is_empty() {
        return notice(theme, &format!("Sin actividad {}", range.phrase()), "Ningún agente registró tokens en este periodo.");
    }
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(theme.typography.ui_size_display))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.colors.text)
                .child(compact_tokens(snap.total_billable())),
        )
        .child(muted_text(theme, theme.typography.ui_size_sm, format!("tokens facturables · {}", range.phrase())))
        .into_any_element()
}

fn agent_card(theme: &Theme, t: &AgentTotals) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .id(SharedString::from(format!("usage-agent-{}", t.agent.as_str())))
        .tooltip(Tooltip::text(theme, breakdown_tooltip(t)))
        .child(
            card(theme)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(agent_icon(agent_mark(t.agent), 18.0, c.text))
                        .child(div().flex_1().min_w_0().text_size(px(ty.ui_size_md)).font_weight(FontWeight::MEDIUM).child(agent_name(t.agent)))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(theme.typography.ui_size_xl))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(compact_tokens(t.billable())),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_x(px(12.0))
                        .gap_y(px(2.0))
                        .child(muted_text(
                            theme,
                            ty.ui_size_sm,
                            format!("tokens facturables · {}", events_label(t.events)),
                        ))
                        .child(muted_text(theme, ty.ui_size_sm, cost_label(&t.cost))),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Official limits
// ---------------------------------------------------------------------------------------------

fn limit_tooltip(state: &LimitState, window: LimitWindow, agent: agents_svc::Agent) -> String {
    let who = format!("{} · {}", agent_name(agent), window.label());
    match state {
        LimitState::Fresh { source, age_ms, .. } => format!(
            "{who}\nPorcentaje oficial informado por el proveedor\nfuente {source} · {}",
            crate::fmt::format_age(*age_ms)
        ),
        LimitState::Stale { age_ms } => format!(
            "{who}\nÚltimo dato oficial {} (ya caducó): no se muestra un porcentaje",
            crate::fmt::format_age(*age_ms)
        ),
        LimitState::Missing => format!("{who}\nSelachi aún no ha recibido datos oficiales de este límite"),
    }
}

/// `oficial · hace 2 min` (freshest window) or `sin datos oficiales`.
fn group_freshness(states: &[LimitState]) -> String {
    states
        .iter()
        .filter_map(|s| match s {
            LimitState::Fresh { age_ms, .. } => Some(*age_ms),
            _ => None,
        })
        .min()
        .map_or_else(|| "sin datos oficiales".to_string(), freshness_label)
}

fn limit_row(
    theme: &Theme,
    id: (&'static str, usize),
    agent: agents_svc::Agent,
    window: LimitWindow,
    state: &LimitState,
    compact: bool,
) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    let pct = state.percent();
    let pct_text = pct.map_or_else(|| "—".to_string(), |p| format!("{p:.0} %"));
    let meter = DitherMeter::new(theme).segments(if compact { 8 } else { 12 }).value(pct.map(|p| p / 100.0));
    let detail = match state {
        LimitState::Fresh { resets_in_ms: Some(ms), .. } => reset_label(*ms),
        LimitState::Fresh { .. } => String::new(),
        _ => "sin datos oficiales".to_string(),
    };
    div()
        .id(id)
        .tooltip(Tooltip::text(theme, limit_tooltip(state, window, agent)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(px(ty.ui_size_sm))
        .child(div().flex_none().w(px(if compact { 34.0 } else { 44.0 })).text_color(c.muted).child(window.label()))
        .child(meter)
        .child(
            div()
                .flex_none()
                .w(px(36.0))
                .text_right()
                .text_size(px(ty.ui_size_md))
                .text_color(if pct.is_some() { c.text } else { c.muted })
                .child(pct_text),
        )
        .when(!compact, |d| {
            d.child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(c.muted).child(detail))
        })
        .into_any_element()
}

fn limits_block(theme: &Theme, snap: &UsageSnapshot, now_ms: i64, compact: bool) -> Option<AnyElement> {
    let agents = limit_agents(snap);
    if agents.is_empty() {
        return None;
    }
    let c = theme.colors;
    let ty = theme.typography;
    let groups = agents.into_iter().enumerate().map(|(gi, agent)| {
        let states: Vec<LimitState> =
            LimitWindow::ALL.iter().map(|w| limit_state(&snap.limits, agent, *w, now_ms)).collect();
        let freshness = group_freshness(&states);
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(agent_icon(agent_mark(agent), 16.0, c.text))
            .child(div().flex_1().min_w_0().text_size(px(ty.ui_size_md)).font_weight(FontWeight::MEDIUM).child(agent_name(agent)))
            .when(!compact, |d| d.child(muted_text(theme, ty.ui_size_sm, freshness.clone())));
        let rows = LimitWindow::ALL.iter().zip(&states).enumerate().map(|(wi, (w, s))| {
            limit_row(theme, ("usage-limit", gi * 2 + wi), agent, *w, s, compact)
        });
        let body = div().flex().flex_col().gap(px(6.0)).child(header).children(rows);
        if compact {
            // Compact: no card chrome, one muted freshness line at the end.
            body.child(muted_text(theme, ty.ui_size_sm, freshness)).into_any_element()
        } else {
            card(theme).child(body).into_any_element()
        }
    });
    Some(section(theme, "Límites oficiales").children(groups).into_any_element())
}

fn forecast_block(theme: &Theme, snap: &UsageSnapshot) -> Option<AnyElement> {
    let lines = forecast_lines(&snap.forecasts, &local_now(), 2);
    if lines.is_empty() {
        return None;
    }
    let ty = theme.typography;
    let rows = lines.into_iter().map(|l| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().text_size(px(ty.ui_size_md)).child(l.text))
            .child(muted_text(
                theme,
                ty.ui_size_sm,
                format!("{} · {} · {} · {}", agent_name(l.agent), l.window, l.rate, l.label),
            ))
    });
    Some(section(theme, "Pronóstico").child(card(theme).children(rows)).into_any_element())
}

// ---------------------------------------------------------------------------------------------
// Per branch
// ---------------------------------------------------------------------------------------------

fn branches_block(
    theme: &Theme,
    snap: &UsageSnapshot,
    expanded: bool,
    toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let ty = theme.typography;
    let head = section(theme, "Por rama");
    let Some(branches) = &snap.branches else {
        return head
            .child(muted_text(theme, ty.ui_size_sm, "Abre una pestaña dentro de un proyecto para ver el coste por rama."))
            .into_any_element();
    };
    if branches.is_empty() {
        return head
            .child(muted_text(theme, ty.ui_size_sm, format!("Sin actividad de agentes en este proyecto {}.", snap.query.range.phrase())))
            .into_any_element();
    }
    let shown = if expanded { BRANCHES_MAX } else { BRANCHES_COLLAPSED };
    let rows = branches.iter().take(shown).map(|b| {
        let name = b.git_branch.clone().unwrap_or_else(|| "sin rama".to_string());
        let billable = b.input + b.output + b.cache_create;
        let partial = !b.cost_complete;
        let mut row = div().id(SharedString::from(format!("usage-branch-{name}"))).child(
            ListRow::new(SharedString::from(format!("usage-branch-row-{name}")), theme)
                .icon(Icon::GitBranch)
                .title(name)
                .subtitle(format!("{} tokens · {}", compact_tokens(billable), events_label(b.events)))
                .meta(cost_short(b.est_cost_usd, b.cost_complete)),
        );
        if partial {
            row = row.tooltip(Tooltip::text(theme, "Coste parcial: falta el precio de algún modelo usado en esta rama (estimado)"));
        }
        row.into_any_element()
    });
    let hidden = branches.len().saturating_sub(BRANCHES_COLLAPSED);
    let more = (hidden > 0).then(|| {
        let label = if expanded { "Ver menos".to_string() } else { format!("Ver las {} ramas", branches.len()) };
        div().flex().child(
            Pill::new("usage-branches-toggle", theme)
                .label(label)
                .on_click(move |_, window, cx| toggle(window, cx)),
        )
    });
    head.child(div().flex().flex_col().children(rows)).children(more).into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Chart
// ---------------------------------------------------------------------------------------------

/// The agents with tokens in `days`, with their stack slot, in [`AGENT_ORDER`] order.
pub(crate) fn legend_agents(days: &[crate::data::DayUsage]) -> Vec<(usize, agents_svc::Agent)> {
    AGENT_ORDER
        .iter()
        .copied()
        .enumerate()
        .filter(|(slot, _)| days.iter().any(|d| d.per_agent[*slot] > 0))
        .collect()
}

fn chart_block(theme: &Theme, snap: &UsageSnapshot, tall: bool) -> Option<AnyElement> {
    if snap.days.is_empty() {
        return None;
    }
    let c = theme.colors;
    let ty = theme.typography;
    let today = snap.days.last().map(|d| d.date)?;
    let chart = bar_chart(theme, &snap.days, today, if tall { 120.0 } else { 88.0 }, true);
    let legend = legend_agents(&snap.days)
        .into_iter()
        .map(|(slot, agent)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.0))
                .child(div().size(px(8.0)).rounded(px(2.0)).bg(slot_tone(theme, slot, false)))
                .child(muted_text(theme, ty.ui_size_sm, agent_name(agent)))
        });
    let total: u64 = snap.days.iter().map(|d| d.total()).sum();
    Some(
        section(theme, "Últimos 14 días")
            .child(
                card(theme)
                    .child(chart)
                    .child(div().flex().flex_row().items_center().gap(px(12.0)).children(legend).child(
                        div().flex_1().text_right().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(format!("{} facturables", compact_tokens(total))),
                    )),
            )
            .into_any_element(),
    )
}

// ---------------------------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------------------------

impl Render for UsagePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.model.update(cx, |m, cx| m.touch(cx));
        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let compact = self.is_compact();
        let wide = !compact && matches!(self.ctx.zone, PanelZone::Main | PanelZone::Bottom);
        let pad = if compact { 12.0 } else { 16.0 };
        let (range, snap, error, has_source) = {
            let m = self.model.read(cx);
            (m.range(), m.snapshot().cloned(), m.error().cloned(), m.has_source())
        };
        let model = self.model.clone();
        let view = cx.entity();

        let selector: AnyElement = if compact {
            let m = model;
            Pill::new("usage-range", &theme)
                .label(range.label())
                .active(true)
                .tooltip("Cambiar periodo")
                .on_click(move |_, _, cx| m.update(cx, |m, cx| m.set_range(m.range().next(), cx)))
                .into_any_element()
        } else {
            let m = model;
            SegmentedControl::new("usage-range", &theme, UsageRange::ALL.map(|r| Segment::new(r.label())))
                .selected(range.index())
                .track(c.surface_2)
                .on_change(move |ix, _, cx| m.update(cx, |m, cx| m.set_range(UsageRange::from_index(ix), cx)))
                .into_any_element()
        };
        let header = div().flex().flex_row().flex_none().child(selector);

        let mut left: Vec<AnyElement> = Vec::new();
        let mut right: Vec<AnyElement> = Vec::new();
        match &snap {
            None => {
                let (title, detail) = match (&error, has_source) {
                    (Some(e), _) => ("No se pudo leer el uso", e.to_string()),
                    (None, false) => ("Sin datos de uso", "La base de datos de uso no está conectada.".to_string()),
                    (None, true) => ("Leyendo uso…", "Solo se consulta mientras el panel está abierto.".to_string()),
                };
                left.push(notice(&theme, title, &detail));
            }
            Some(snap) => {
                let now = now_ms();
                // The snapshot's range, not the selector's, labels the numbers until the new one lands.
                left.push(headline(&theme, snap, snap.query.range));
                if !compact {
                    left.extend(snap.agents.iter().map(|t| agent_card(&theme, t)));
                }
                left.extend(limits_block(&theme, snap, now, compact));
                if !compact {
                    left.extend(forecast_block(&theme, snap));
                    let toggle_view = view;
                    let branches = branches_block(&theme, snap, self.branches_expanded, move |_, cx| {
                        toggle_view.update(cx, |p, cx| {
                            p.branches_expanded = !p.branches_expanded;
                            cx.notify();
                        });
                    });
                    right.push(branches);
                    right.extend(chart_block(&theme, snap, wide));
                    if let Some(e) = &error {
                        right.push(muted_text(&theme, ty.ui_size_sm, format!("No se pudo actualizar: {e}")).into_any_element());
                    }
                }
            }
        }

        let column = || div().flex().flex_col().gap(px(12.0));
        let body = if wide {
            // Main / bottom: two columns.
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(16.0))
                .child(column().flex_1().min_w_0().children(left))
                .child(column().flex_1().min_w_0().children(right))
        } else {
            column().w_full().children(left).children(right)
        };

        // Reading panels are capped at 860 px and centered (DESIGN.md, responsive).
        let inner = div()
            .w_full()
            .when(wide, |d| d.max_w(px(860.0)))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(header)
            .child(div().id("usage-scroll").flex_1().min_h_0().overflow_y_scroll().pb(px(pad)).child(body));

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(pad))
            .px(px(pad))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .text_color(c.text)
            .child(inner)
    }
}
