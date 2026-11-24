//! The panel: header (tabs + "Limpiar huérfanos"), the inline confirmation, the agent -> tab ->
//! process tree and the ports list. It adapts to [`WidthClass`] and never looks at its zone.

use std::collections::HashSet;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, ElementId, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px, svg,
};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Radius, Theme};
use ui_kit::{
    Button, GhostButton, IconButton, Segment, SegmentedControl, Spinner, Tooltip, WireFill,
    WireRect, Wireframe, icon,
};

use crate::assets::{ARROW_UP_RIGHT, runtime_icon_path};
use crate::fmt::{format_bytes, format_cpu, format_uptime, plural};
use crate::model::{AgentGroup, OwnerGroup, PortPill, PortRow, ProcRow, RowState, Snapshot};
use crate::plan::{PlanItem, StopPlan};
use crate::state::{Dialog, ProcsModel};

/// Panel id stored in `layout.right_panel`.
pub const PANEL_ID: &str = "procs";

/// Rows listed in a confirmation before "... y N más".
const PREVIEW_ROWS: usize = 8;

/// Registers as `procs`.
pub struct ProcsPanelProvider {
    model: Entity<ProcsModel>,
}

impl ProcsPanelProvider {
    pub fn new(model: Entity<ProcsModel>) -> Self {
        Self { model }
    }
}

impl PanelProvider for ProcsPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> gpui::SharedString {
        "Procesos".into()
    }

    fn icon(&self) -> Icon {
        Icon::Procs
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[
            PanelZone::Sidebar,
            PanelZone::Right,
            PanelZone::Main,
            PanelZone::Bottom,
        ]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let model = self.model.clone();
        cx.new(|cx| ProcsPanel::new(ctx, model, cx)).into()
    }

    fn context_changed(
        &self,
        view: &AnyView,
        ctx: &PanelCtx,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Ok(panel) = view.clone().downcast::<ProcsPanel>() else {
            return false;
        };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

/// 1-bit thumbnail for the layout panel: segmented header, action pill and a tree of rows.
pub fn thumbnail(theme: &Theme) -> Wireframe {
    use WireFill::{Dither, Solid};
    let r = WireRect::new;
    Wireframe::new(theme).rects([
        r(1, 1, 9, 2, Solid),
        r(21, 1, 10, 2, Solid),
        r(1, 5, 12, 1, Solid),
        r(3, 7, 3, 1, Solid),
        r(7, 7, 14, 1, Dither),
        r(3, 9, 3, 1, Solid),
        r(7, 9, 11, 1, Dither),
        r(25, 7, 6, 1, Solid),
        r(1, 12, 10, 1, Solid),
        r(3, 14, 3, 1, Solid),
        r(7, 14, 16, 1, Dither),
        r(3, 16, 3, 1, Solid),
        r(7, 16, 9, 1, Dither),
    ])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Tree,
    Ports,
}

pub struct ProcsPanel {
    ctx: PanelCtx,
    model: Entity<ProcsModel>,
    tab: Tab,
    collapsed: HashSet<String>,
    show_other_ports: bool,
    _observe: Subscription,
    _release: Subscription,
}

impl ProcsPanel {
    /// Builds the view over a shared model.
    pub fn new(ctx: PanelCtx, model: Entity<ProcsModel>, cx: &mut Context<Self>) -> Self {
        model.update(cx, |m, _| m.attach_view());
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        let for_release = model.clone();
        let release = cx.on_release(move |_, cx| {
            for_release.update(cx, |m, cx| m.detach_view(cx));
        });
        Self {
            ctx,
            model,
            tab: Tab::Tree,
            collapsed: HashSet::new(),
            show_other_ports: false,
            _observe: observe,
            _release: release,
        }
    }

    /// The shell moved / resized the panel.
    pub fn retarget(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        self.ctx = ctx;
        cx.notify();
    }

    /// Shows the "Puertos" sub-view (`true`) or the tree (`false`).
    pub fn select_ports(&mut self, ports: bool, cx: &mut Context<Self>) {
        self.tab = if ports { Tab::Ports } else { Tab::Tree };
        cx.notify();
    }

    pub fn is_compact(&self) -> bool {
        self.ctx.width_class == WidthClass::Compact
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        cx.notify();
    }
}

// ---------------------------------------------------------------------------------------------
// Small elements
// ---------------------------------------------------------------------------------------------

/// Character-based truncation. gpui `text_ellipsis` does not truncate `nowrap` text that was
/// first measured at max-content (its measure cache returns the untruncated size), so long
/// commands are clipped here and the full text goes in the tooltip.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn probable_label(compact: bool) -> &'static str {
    if compact {
        "probable"
    } else {
        "atribución probable"
    }
}

fn text_ellipsis_box() -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
}

fn state_color(theme: &Theme, state: RowState) -> gpui::Rgba {
    match state {
        RowState::Active => theme.colors.accent,
        RowState::Orphan => theme.colors.warning,
        RowState::PortZombie => theme.colors.error,
    }
}

fn badge(theme: &Theme, state: RowState) -> impl IntoElement {
    let c = theme.colors;
    let color = state_color(theme, state);
    div()
        .h(px(18.0))
        .flex_none()
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .bg(c.surface_3)
        .text_size(px(theme.typography.ui_size_xs))
        .text_color(if state == RowState::Active {
            c.muted
        } else {
            color
        })
        .child(div().size(px(6.0)).flex_none().rounded_full().bg(color))
        .child(state.label())
}

fn tag(theme: &Theme, text: impl Into<SharedString>) -> impl IntoElement {
    let c = theme.colors;
    div()
        .h(px(18.0))
        .flex_none()
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded_full()
        .bg(c.surface_2)
        .text_size(px(theme.typography.ui_size_xs))
        .text_color(c.muted)
        .child(text.into())
}

fn runtime_icon(theme: &Theme, kind: proc_svc::RuntimeKind) -> impl IntoElement {
    svg()
        .path(runtime_icon_path(kind))
        .flex_none()
        .size(px(16.0))
        .text_color(theme.colors.text)
}

/// `:5173 ↗`: opens `http://localhost:PORT` for TCP; UDP is a plain, inert chip.
fn port_pill(theme: &Theme, pid: u32, pill: PortPill) -> impl IntoElement + use<> {
    let c = theme.colors;
    let id: SharedString = format!("pill-{pid}-{}-{}", pill.port, u8::from(pill.tcp)).into();
    let base = div()
        .id(ElementId::from(id))
        .h(px(20.0))
        .flex_none()
        .px(px(7.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .rounded(px(Radius::CONTROL))
        .bg(c.surface_2)
        .font_family(theme.typography.mono_family)
        .text_size(px(theme.typography.ui_size_xs));
    match pill.url() {
        Some(url) => {
            let tip = if pill.exposed {
                format!("Abrir {url}\nEscucha en todas las interfaces (visible en la red)")
            } else {
                format!("Abrir {url}")
            };
            base.text_color(c.accent)
                .cursor_pointer()
                .hover(move |s| s.bg(c.surface_3))
                .occlude()
                .tooltip(Tooltip::text(theme, tip))
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(pill.label())
                .child(
                    svg()
                        .path(ARROW_UP_RIGHT)
                        .flex_none()
                        .size(px(10.0))
                        .text_color(c.accent),
                )
                .into_any_element()
        }
        None => base
            .text_color(c.muted)
            .child(pill.label())
            .into_any_element(),
    }
}

fn chevron(theme: &Theme, collapsed: bool) -> impl IntoElement {
    icon(
        if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        },
        16.0,
        theme.colors.muted,
    )
}

pub(crate) fn metrics_text(
    row_ram: u64,
    cpu: Option<f32>,
    uptime: Option<std::time::Duration>,
    compact: bool,
) -> String {
    let ram = format_bytes(row_ram);
    if compact {
        return ram;
    }
    let cpu = cpu.map_or_else(|| "—".to_string(), format_cpu);
    let up = uptime.map_or_else(|| "—".to_string(), format_uptime);
    format!("{ram} · {cpu} · {up}")
}

// ---------------------------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------------------------

impl Render for ProcsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Heartbeat: a painted panel is a visible panel (starts / keeps the 2 s sampling).
        self.model.update(cx, |m, cx| m.touch(cx));

        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let compact = self.is_compact();
        let model = self.model.read(cx);
        let snap = model.snapshot().clone();
        let dialog = model.dialog().clone();
        let pad = if compact { 12.0 } else { 16.0 };

        let body = match self.tab {
            Tab::Tree => self.tree_body(&snap, cx),
            Tab::Ports => self.ports_body(&snap, cx),
        };

        let mut col = div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .pt(px(pad))
            .px(px(pad))
            .font_family(ty.ui_family)
            .text_color(c.text)
            .child(self.header(&snap, &dialog, cx))
            .child(summary_line(&theme, &snap, compact));
        if let Some(card) = self.dialog_card(&dialog) {
            col = col.child(card);
        }
        col.child(
            div()
                .id("procs-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb(px(pad))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(body),
        )
    }
}

fn summary_line(theme: &Theme, snap: &Snapshot, compact: bool) -> impl IntoElement + use<> {
    let c = theme.colors;
    let mut line = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(6.0))
        .text_size(px(theme.typography.ui_size_sm))
        .text_color(c.muted);
    if snap.is_empty() {
        return line.child("Sin procesos de agentes");
    }
    line = line.child(plural(snap.process_count, "proceso", "procesos"));
    if !compact || snap.summary.count == 0 {
        line = line.child("·").child(format_bytes(snap.ram_total));
    }
    if snap.summary.count > 0 {
        line = line
            .child("·")
            .child(div().text_color(theme.colors.warning).child(plural(
                snap.summary.count,
                "huérfano",
                "huérfanos",
            )));
    }
    line
}

impl ProcsPanel {
    fn header(
        &self,
        snap: &Snapshot,
        dialog: &Dialog,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.ctx.theme;
        let compact = self.is_compact();
        let view = cx.entity();
        let model = self.model.clone();

        let tabs = SegmentedControl::new(
            "procs-tabs",
            &theme,
            [Segment::new("Árbol"), Segment::new("Puertos")],
        )
        .track(theme.colors.surface_2)
        .selected(usize::from(self.tab == Tab::Ports))
        .on_change(move |ix, _, cx| {
            view.update(cx, |p, cx| {
                p.tab = if ix == 1 { Tab::Ports } else { Tab::Tree };
                cx.notify();
            });
        });

        let busy = !matches!(
            dialog,
            Dialog::None | Dialog::Done { .. } | Dialog::Notice(_)
        );
        let orphans = snap.summary.count;
        let clean = Button::action("procs-clean", &theme, "Limpiar huérfanos")
            .no_icon()
            .disabled(orphans == 0 || busy)
            .tooltip(match snap.summary.label() {
                Some(l) => format!("{l}\nMuestra una vista previa antes de detener nada"),
                None => "No hay huérfanos".to_string(),
            })
            .on_click(move |_, _, cx| model.update(cx, |m, cx| m.request_clean_orphans(cx)));

        let row = div().flex().gap(px(8.0));
        if compact {
            row.flex_col().child(tabs).child(clean)
        } else {
            row.flex_row()
                .items_center()
                .child(tabs)
                .child(div().flex_1())
                .child(clean)
        }
    }

    // ----- tree --------------------------------------------------------------------------

    fn tree_body(&self, snap: &Snapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        if snap.is_empty() {
            return vec![empty_state(
                &theme,
                "No hay procesos de agentes en marcha",
                "Lo que lancen tus pestañas y agentes aparecerá aquí.",
            )];
        }
        let mut out = Vec::new();
        for agent in &snap.agents {
            let key = format!("ag-{}", agent.id);
            let folded = self.collapsed.contains(&key);
            out.push(self.agent_header(agent, &key, folded, cx));
            if folded {
                continue;
            }
            for owner in &agent.owners {
                let okey = owner.key.id();
                let ofolded = self.collapsed.contains(&okey);
                out.push(self.owner_header(owner, &okey, ofolded, cx));
                if !ofolded {
                    out.extend(owner.rows.iter().map(|r| self.proc_row(r)));
                }
            }
        }
        out
    }

    fn agent_header(
        &self,
        agent: &AgentGroup,
        key: &str,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let view = cx.entity();
        let k = key.to_string();
        let mut meta = format!(
            "{} · {}",
            plural(agent.process_count(), "proceso", "procesos"),
            format_bytes(agent.ram())
        );
        if self.is_compact() {
            meta = plural(agent.process_count(), "proceso", "procesos");
        }
        div()
            .id(ElementId::from(SharedString::from(key.to_string())))
            .h(px(30.0))
            .flex_none()
            .px(px(6.0))
            .mt(px(4.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::CONTROL))
            .cursor_pointer()
            .hover(move |s| s.bg(c.surface_2))
            .on_click(move |_, _, cx| view.update(cx, |p, cx| p.toggle(k.clone(), cx)))
            .child(chevron(&theme, folded))
            .child(ui_kit::agent_icon(
                ui_kit::AgentMark::from_name(&agent.id),
                16.0,
                c.muted,
            ))
            .child(
                text_ellipsis_box()
                    .text_size(px(theme.typography.ui_size_md))
                    .font_weight(FontWeight::MEDIUM)
                    .child(agent.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(theme.typography.ui_size_xs))
                    .text_color(c.muted)
                    .child(meta),
            )
            .into_any_element()
    }

    fn owner_header(
        &self,
        owner: &OwnerGroup,
        key: &str,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let view = cx.entity();
        let k = key.to_string();
        let mut el = div()
            .id(ElementId::from(SharedString::from(key.to_string())))
            .h(px(26.0))
            .flex_none()
            .pl(px(18.0))
            .pr(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(Radius::CONTROL))
            .cursor_pointer()
            .hover(move |s| s.bg(c.surface_2))
            .on_click(move |_, _, cx| view.update(cx, |p, cx| p.toggle(k.clone(), cx)))
            .child(chevron(&theme, folded))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.ui_size_sm))
                    .child(clip(&owner.label, if compact { 14 } else { 22 })),
            );
        match owner.detail.as_deref() {
            Some(detail) if !(compact && owner.probable) => {
                el = el.child(
                    text_ellipsis_box()
                        .text_size(px(theme.typography.ui_size_xs))
                        .text_color(c.muted)
                        .child(clip(detail, if compact { 12 } else { 28 })),
                );
            }
            _ => el = el.child(div().flex_1()),
        }
        if owner.probable {
            el = el.child(tag(&theme, probable_label(compact)));
        }
        if owner.tree_tracked {
            // No Job Object (for example the Microsoft Store `pwsh`): "seguimiento por árbol".
            el = el.child(tag(
                &theme,
                if compact {
                    "árbol"
                } else {
                    "seguimiento por árbol"
                },
            ));
        }
        let orphans = owner.orphan_count();
        if orphans > 0 {
            el = el.child(
                div()
                    .flex_none()
                    .text_size(px(theme.typography.ui_size_xs))
                    .text_color(theme.colors.warning)
                    .child(plural(orphans, "huérfano", "huérfanos")),
            );
        }
        el.into_any_element()
    }

    /// The "Detener" control of a row: a text button when there is room, a pixel `x` when compact.
    fn stop_control(
        &self,
        id: String,
        pid: u32,
        name: &str,
        allowed: bool,
        denied_hint: &'static str,
        denied_label: &'static str,
    ) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let model = self.model.clone();
        if !allowed {
            return div()
                .id(ElementId::from(SharedString::from(format!("denied-{id}"))))
                .flex_none()
                .text_size(px(theme.typography.ui_size_xs))
                .text_color(c.muted)
                .tooltip(Tooltip::text(&theme, denied_hint))
                .child(denied_label)
                .into_any_element();
        }
        let tip = format!("Detener {name} y sus subprocesos (con vista previa)");
        if self.is_compact() {
            IconButton::new(format!("stop-{id}"), &theme, Icon::Close)
                .size(24.0)
                .tooltip(tip)
                .on_click(move |_, _, cx| model.update(cx, |m, cx| m.request_stop(pid, cx)))
                .into_any_element()
        } else {
            GhostButton::new(format!("stop-{id}"), &theme, "Detener")
                .tooltip(tip)
                .on_click(move |_, _, cx| model.update(cx, |m, cx| m.request_stop(pid, cx)))
                .into_any_element()
        }
    }

    fn proc_row(&self, r: &ProcRow) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let pid = r.pid;
        let indent = 18.0 + 12.0 * f32::from(r.depth.min(3));

        let stop: AnyElement = if r.is_root {
            tag(&theme, "shell").into_any_element()
        } else {
            self.stop_control(
                pid.to_string(),
                pid,
                &r.name,
                !r.access_denied,
                "Sin permiso: no se puede verificar este proceso",
                "sin acceso",
            )
        };

        let l1 = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(runtime_icon(&theme, r.runtime))
            .child(
                text_ellipsis_box()
                    .text_size(px(theme.typography.ui_size_md))
                    .text_color(if r.access_denied { c.muted } else { c.text })
                    .child(clip(&r.command, if compact { 20 } else { 30 })),
            )
            .child(stop);

        let mut l2 = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(24.0));
        l2 = l2.child(badge(&theme, r.state));
        l2 = l2.children(
            r.ports
                .iter()
                .take(if compact { 1 } else { 3 })
                .map(|p| port_pill(&theme, pid, *p)),
        );
        if r.probable && !compact {
            l2 = l2.child(tag(&theme, probable_label(false)));
        }
        if let (Some(t), false) = (r.tag, compact) {
            l2 = l2.child(tag(&theme, t));
        }
        let l2 = l2.child(div().flex_1()).child(
            div()
                .flex_none()
                .text_size(px(theme.typography.ui_size_xs))
                .text_color(c.muted)
                .child(metrics_text(r.ram, r.cpu, r.uptime, compact)),
        );

        let full = r.full_command.clone().unwrap_or_else(|| r.name.clone());
        div()
            .id(ElementId::from(SharedString::from(format!("row-{pid}"))))
            .flex_none()
            .pl(px(indent))
            .pr(px(6.0))
            .py(px(5.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .rounded(px(Radius::CONTROL))
            .hover(move |s| s.bg(c.surface_2))
            .tooltip(Tooltip::text(&theme, format!("{full}\npid {pid}")))
            .child(l1)
            .child(l2)
            .into_any_element()
    }

    // ----- ports -------------------------------------------------------------------------

    fn ports_body(&self, snap: &Snapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let (mine, others): (Vec<&PortRow>, Vec<&PortRow>) =
            snap.ports.iter().partition(|p| p.is_agent_related());
        let mut out = Vec::new();
        if mine.is_empty() {
            out.push(empty_state(
                &theme,
                "Ningún agente escucha en un puerto",
                "Los dev servers de tus pestañas aparecerán aquí.",
            ));
        }
        out.extend(mine.iter().map(|p| self.port_row(p, cx)));
        if !others.is_empty() {
            let view = cx.entity();
            let open = self.show_other_ports;
            out.push(
                div()
                    .id("ports-others")
                    .h(px(30.0))
                    .flex_none()
                    .mt(px(6.0))
                    .px(px(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(Radius::CONTROL))
                    .cursor_pointer()
                    .hover(move |s| s.bg(c.surface_2))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |p, cx| {
                            p.show_other_ports = !p.show_other_ports;
                            cx.notify();
                        });
                    })
                    .child(chevron(&theme, !open))
                    .child(
                        div()
                            .text_size(px(theme.typography.ui_size_sm))
                            .text_color(c.muted)
                            .child(format!("Otros puertos del equipo ({})", others.len())),
                    )
                    .into_any_element(),
            );
            if open {
                out.extend(others.iter().map(|p| self.port_row(p, cx)));
            }
        }
        out
    }

    fn port_row(&self, p: &PortRow, _cx: &mut Context<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let pid = p.pid;
        let id_key = format!("port-{pid}-{}-{}", p.port, u8::from(p.tcp));

        let stop = self.stop_control(
            id_key.clone(),
            pid,
            &p.process,
            p.can_stop(),
            "Proceso del sistema o sin permiso: no se puede detener desde aquí",
            "protegido",
        );

        let pill = PortPill {
            port: p.port,
            tcp: p.tcp,
            exposed: p.exposed,
        };
        let l1 = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(port_pill(&theme, pid, pill))
            .child(
                text_ellipsis_box()
                    .text_size(px(theme.typography.ui_size_md))
                    .child(clip(&p.command, if compact { 14 } else { 28 })),
            )
            .child(stop);

        let mut l2 = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(2.0));
        l2 = l2.child(runtime_icon(&theme, p.runtime)).child(
            text_ellipsis_box()
                .text_size(px(theme.typography.ui_size_xs))
                .text_color(c.muted)
                .child(clip(
                    &p.origin.label(),
                    if compact && p.state.is_some_and(|s| s.is_orphan()) {
                        9
                    } else if compact {
                        16
                    } else {
                        30
                    },
                )),
        );
        if let Some(s) = p.state.filter(|s| s.is_orphan()) {
            l2 = l2.child(badge(&theme, s));
        }
        if let (Some(t), false) = (p.tag, compact) {
            l2 = l2.child(tag(&theme, t));
        }
        if p.exposed && !compact {
            l2 = l2.child(tag(&theme, "expuesto"));
        }
        if p.ram > 0 {
            l2 = l2.child(
                div()
                    .flex_none()
                    .text_size(px(theme.typography.ui_size_xs))
                    .text_color(c.muted)
                    .child(format_bytes(p.ram)),
            );
        }

        div()
            .id(ElementId::from(SharedString::from(id_key)))
            .flex_none()
            .px(px(6.0))
            .py(px(5.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .rounded(px(Radius::CONTROL))
            .hover(move |s| s.bg(c.surface_2))
            .tooltip(Tooltip::text(&theme, format!("{}\npid {pid}", p.command)))
            .child(l1)
            .child(l2)
            .into_any_element()
    }

    // ----- confirmation ---------------------------------------------------------------------

    fn dialog_card(&self, dialog: &Dialog) -> Option<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let model = self.model.clone();
        let card = || {
            div()
                .flex_none()
                .p(px(12.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_2)
        };
        let title = |t: String| {
            div()
                .text_size(px(ty.ui_size_lg))
                .font_weight(FontWeight::MEDIUM)
                .child(t)
        };
        let note = |t: String| {
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(t)
        };
        let buttons = || div().flex().flex_row().flex_wrap().gap(px(8.0));

        Some(match dialog {
            Dialog::None => return None,
            Dialog::Planning => card()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .child(Spinner::new(c.accent))
                .child(note("Preparando la vista previa…".into()))
                .into_any_element(),
            Dialog::Notice(msg) => {
                let m = model;
                card()
                    .child(note(msg.to_string()))
                    .child(
                        buttons().child(Button::tonal("dlg-close", &theme, "Cerrar").on_click(
                            move |_, _, cx| {
                                m.update(cx, |m, cx| m.cancel(cx));
                            },
                        )),
                    )
                    .into_any_element()
            }
            Dialog::Confirm(plan) => {
                let (m_cancel, m_ok) = (model.clone(), model);
                let n = plan.items.len();
                let mut el = card()
                    .child(title(plan.title()))
                    .child(note(confirm_note(plan)));
                el = el.child(plan_list(&theme, plan, self.is_compact()));
                for (item, reason) in &plan.refused {
                    el = el.child(note(format!(
                        "No se tocará {} (pid {}): {reason}",
                        item.name, item.pid
                    )));
                }
                if plan.denied > 0 {
                    el = el.child(note(format!("{} sin permiso: se omiten.", plan.denied)));
                }
                el.child(
                    buttons()
                        .child(
                            Button::tonal("dlg-cancel", &theme, "Cancelar").on_click(
                                move |_, _, cx| m_cancel.update(cx, |m, cx| m.cancel(cx)),
                            ),
                        )
                        .child(
                            Button::primary(
                                "dlg-confirm",
                                &theme,
                                format!("Detener {}", plan_roots_label(n)),
                            )
                            .disabled(plan.is_empty())
                            .on_click(move |_: &ClickEvent, _, cx| {
                                m_ok.update(cx, |m, cx| m.confirm(cx))
                            }),
                        ),
                )
                .into_any_element()
            }
            Dialog::Running(plan) => card()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .child(Spinner::new(c.accent))
                        .child(title(format!(
                            "Deteniendo {}…",
                            plural(plan.items.len(), "proceso", "procesos")
                        ))),
                )
                .child(note(
                    "Se pide el cierre y, si no responden en 3 s, se fuerzan.".into(),
                ))
                .into_any_element(),
            Dialog::Done { plan, result } => {
                let m = model;
                let mut el = card().child(title(if result.stopped() > 0 || result.is_clean() {
                    format!("{} detenidos", result.stopped())
                } else {
                    "No se detuvo nada".to_string()
                }));
                if !result.forced.is_empty() {
                    el = el.child(note(format!(
                        "{} se forzaron tras 3 s.",
                        result.forced.len()
                    )));
                }
                for (pid, reason) in result.failed.iter().chain(result.skipped.iter()) {
                    let name = plan
                        .items
                        .iter()
                        .find(|i| i.pid == *pid)
                        .map_or("proceso", |i| i.name.as_str());
                    el = el.child(
                        div()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.error)
                            .child(format!("{name} (pid {pid}): {reason}")),
                    );
                }
                el.child(
                    buttons().child(Button::tonal("dlg-close", &theme, "Cerrar").on_click(
                        move |_, _, cx| {
                            m.update(cx, |m, cx| m.cancel(cx));
                        },
                    )),
                )
                .into_any_element()
            }
        })
    }
}

fn plan_roots_label(n: usize) -> String {
    plural(n, "proceso", "procesos")
}

fn confirm_note(plan: &StopPlan) -> String {
    let mut s = String::from(
        "Se pide el cierre y, si no responden en 3 s, se fuerzan. Incluye sus subprocesos.",
    );
    if plan.outside_jobs {
        s.push_str(" No pertenecen a un job de Selachi (atribución probable o elegidos por ti).");
    }
    s
}

fn plan_list(theme: &Theme, plan: &StopPlan, compact: bool) -> impl IntoElement + use<> {
    let c = theme.colors;
    let mut list = div().flex().flex_col().gap(px(2.0));
    for item in plan.items.iter().take(PREVIEW_ROWS) {
        list = list.child(plan_row(theme, item, compact));
    }
    if plan.items.len() > PREVIEW_ROWS {
        list = list.child(
            div()
                .pl(px(24.0))
                .text_size(px(theme.typography.ui_size_xs))
                .text_color(c.muted)
                .child(format!("… y {} más", plan.items.len() - PREVIEW_ROWS)),
        );
    }
    list
}

fn plan_row(theme: &Theme, item: &PlanItem, compact: bool) -> impl IntoElement + use<> {
    let c = theme.colors;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(runtime_icon(theme, item.runtime))
        .child(
            text_ellipsis_box()
                .text_size(px(theme.typography.ui_size_sm))
                .child(clip(&item.command, if compact { 18 } else { 30 })),
        )
        .children(item.ports.iter().take(2).map(|p| {
            div()
                .flex_none()
                .font_family(theme.typography.mono_family)
                .text_size(px(theme.typography.ui_size_xs))
                .text_color(c.muted)
                .child(p.label())
        }))
        .child(
            div()
                .flex_none()
                .text_size(px(theme.typography.ui_size_xs))
                .text_color(c.muted)
                .child(if compact {
                    format_bytes(item.ram)
                } else {
                    format!("pid {} · {}", item.pid, format_bytes(item.ram))
                }),
        )
}

fn empty_state(theme: &Theme, title: &'static str, hint: &'static str) -> AnyElement {
    let c = theme.colors;
    div()
        .flex_none()
        .py(px(24.0))
        .px(px(8.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(theme.typography.ui_size_md))
                .text_color(c.text)
                .child(title),
        )
        .child(
            div()
                .text_size(px(theme.typography.ui_size_sm))
                .text_color(c.muted)
                .child(hint),
        )
        .into_any_element()
}
