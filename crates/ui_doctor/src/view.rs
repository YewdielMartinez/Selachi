//! The doctor panel: tonal metric tiles with dither sparklines and "Copiar diagnóstico".

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Bounds, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, canvas, div,
    fill, point, px, size,
};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Radius, Rgba, Theme};
use ui_kit::{Button, WireFill, WireRect, Wireframe};

use crate::health::{
    ChildProc, HISTORY, Health, format_bytes, format_count, format_ms, format_pct, format_uptime,
};
use crate::model::DoctorModel;

/// Panel id for the registry.
pub const PANEL_ID: &str = "doctor";

/// Registers as `doctor`.
pub struct DoctorPanelProvider {
    model: Entity<DoctorModel>,
    version: SharedString,
}

impl DoctorPanelProvider {
    /// `version` is printed in the copied diagnosis (`env!("CARGO_PKG_VERSION")` of the app).
    pub fn new(model: Entity<DoctorModel>, version: impl Into<SharedString>) -> Self {
        Self { model, version: version.into() }
    }
}

impl PanelProvider for DoctorPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Doctor".into()
    }

    fn icon(&self) -> Icon {
        Icon::Doctor
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Sidebar, PanelZone::Right, PanelZone::Main, PanelZone::Bottom]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let (model, version) = (self.model.clone(), self.version.clone());
        cx.new(|cx| DoctorPanel::new(ctx, model, version, cx)).into()
    }

    fn context_changed(&self, view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        let Ok(panel) = view.clone().downcast::<DoctorPanel>() else { return false };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

/// 1-bit thumbnail for the layout panel: two sparkline tiles and a button.
pub fn thumbnail(theme: &Theme) -> Wireframe {
    use WireFill::{Dither, Outline, Solid};
    let r = WireRect::new;
    Wireframe::new(theme).rects([
        r(21, 1, 10, 2, Solid),
        r(1, 4, 14, 6, Outline),
        r(2, 7, 12, 2, Dither),
        r(17, 4, 14, 6, Outline),
        r(18, 8, 12, 1, Dither),
        r(1, 11, 14, 6, Outline),
        r(3, 13, 6, 1, Solid),
        r(17, 11, 14, 6, Outline),
        r(19, 13, 8, 1, Solid),
    ])
}

pub struct DoctorPanel {
    ctx: PanelCtx,
    model: Entity<DoctorModel>,
    version: SharedString,
    copied: bool,
    _observe: Subscription,
    _release: Subscription,
}

impl DoctorPanel {
    pub fn new(ctx: PanelCtx, model: Entity<DoctorModel>, version: SharedString, cx: &mut Context<Self>) -> Self {
        model.update(cx, |m, _| m.attach_view());
        // The "Copiado" confirmation lasts until the next sample.
        let observe = cx.observe(&model, |this, _, cx| {
            this.copied = false;
            cx.notify();
        });
        let for_release = model.clone();
        let release = cx.on_release(move |_, cx| for_release.update(cx, |m, cx| m.detach_view(cx)));
        Self { ctx, model, version, copied: false, _observe: observe, _release: release }
    }

    pub fn retarget(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        self.ctx = ctx;
        cx.notify();
    }

    pub fn is_compact(&self) -> bool {
        self.ctx.width_class == WidthClass::Compact
    }
}

/// Dither sparkline: one column per sample, solid top cell, checkerboard below. `max` is the
/// value of a full-height column.
fn sparkline(theme: &Theme, values: Vec<f32>, max: f32, color: Rgba) -> impl IntoElement + use<> {
    let track = theme.colors.surface_3;
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<gpui::Pixels>, (), window, _| {
            let col_w = (f32::from(bounds.size.width) / HISTORY as f32).floor().max(2.0);
            let cell = (col_w - 1.0).max(1.0);
            let rows = (f32::from(bounds.size.height) / cell).floor().max(1.0) as usize;
            let left = bounds.origin.x + bounds.size.width - px(col_w * HISTORY as f32);
            let bottom = bounds.origin.y + bounds.size.height;
            for i in 0..HISTORY {
                let x = left + px(col_w * i as f32);
                // Baseline pixel for slots without data, so the strip never looks empty.
                let slot = (i + values.len()).checked_sub(HISTORY);
                let Some(v) = slot.and_then(|s| values.get(s)) else {
                    window.paint_quad(fill(Bounds::new(point(x, bottom - px(cell)), size(px(cell), px(cell))), track));
                    continue;
                };
                let n = ((v / max.max(f32::EPSILON)).clamp(0.0, 1.0) * rows as f32).ceil().max(1.0) as usize;
                for r in 0..n {
                    let solid = r + 1 == n || r == 0;
                    if solid || (i + r) % 2 == 0 {
                        let y = bottom - px(cell * (r + 1) as f32);
                        window.paint_quad(fill(Bounds::new(point(x, y), size(px(cell), px(cell))), color));
                    }
                }
            }
        },
    )
    .w_full()
    .h(px(28.0))
}

fn tile(theme: &Theme, label: &'static str) -> gpui::Div {
    let c = theme.colors;
    div()
        .flex_none()
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .rounded(px(Radius::SM))
        .bg(c.surface_2)
        .child(div().text_size(px(11.0)).text_color(c.muted).child(label))
}

fn big(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(22.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.colors.text)
        .child(text.into())
}

fn sub(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div().text_size(px(theme.typography.ui_size_sm)).text_color(theme.colors.muted).child(text.into())
}

fn children_list(theme: &Theme, children: &[ChildProc]) -> impl IntoElement + use<> {
    let c = theme.colors;
    let mut col = div().flex().flex_col().gap(px(2.0)).pt(px(4.0));
    for ch in children.iter().take(4) {
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap(px(8.0))
                .text_size(px(11.0))
                .text_color(c.muted)
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(ch.name.clone()))
                .child(format_bytes(ch.working_set)),
        );
    }
    if children.len() > 4 {
        col = col.child(div().text_size(px(11.0)).text_color(c.muted).child(format!("… y {} más", children.len() - 4)));
    }
    col
}

fn tiles(theme: &Theme, h: &Health) -> Vec<AnyElement> {
    let c = theme.colors;
    let ram_max = h.ram_history.iter().copied().fold(32.0 * 1024.0 * 1024.0, f32::max) * 1.25;
    let cpu_max = h.cpu_history.iter().copied().fold(5.0, f32::max);
    let dash = || "—".to_string();
    let mut out: Vec<AnyElement> = Vec::new();

    out.push(
        tile(theme, "Memoria")
            .child(big(theme, format_bytes(h.working_set)))
            .child(sub(theme, format!("privada {}", format_bytes(h.private_bytes))))
            .child(sparkline(theme, h.ram_history.clone(), ram_max, c.accent))
            .into_any_element(),
    );
    out.push(
        tile(theme, "CPU")
            .child(big(theme, h.cpu.map_or_else(dash, format_pct)))
            .child(sub(theme, format!("{} hilos", h.threads)))
            .child(sparkline(theme, h.cpu_history.clone(), cpu_max, c.accent))
            .into_any_element(),
    );
    out.push(
        tile(theme, "Procesos hijos")
            .child(big(theme, format!("{} · {}", h.children.len(), format_bytes(h.children_ram()))))
            .child(children_list(theme, &h.children))
            .into_any_element(),
    );
    let frames_sub = match (h.frames.last, h.frames.slowest) {
        (Some(l), Some(s)) => format!("último {} · pico {}", format_ms(l), format_ms(s)),
        (Some(l), None) => format!("último {}", format_ms(l)),
        _ => "sin sonda de fotogramas en el shell".to_string(),
    };
    out.push(
        tile(theme, "Fotogramas desde que abriste el doctor")
            .child(big(theme, format_count(h.frames.since_open)))
            .child(sub(theme, frames_sub))
            .into_any_element(),
    );
    let (sessions, sessions_sub) = match (h.stats.shells, h.stats.agents) {
        (None, None) => (dash(), String::new()),
        (s, a) => (
            format!("{} shells", s.unwrap_or(0)),
            match a.unwrap_or(0) { 1 => "1 agente".to_string(), n => format!("{n} agentes") },
        ),
    };
    out.push(tile(theme, "Sesiones").child(big(theme, sessions)).child(sub(theme, sessions_sub)).into_any_element());
    let mut data = tile(theme, "Datos");
    for d in &h.dbs {
        data = data.child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .text_size(px(theme.typography.ui_size_md))
                .child(d.label.clone())
                .child(div().text_color(c.muted).child(d.bytes.map_or_else(|| "no existe".to_string(), format_bytes))),
        );
    }
    if h.dbs.is_empty() {
        data = data.child(sub(theme, "sin bases de datos configuradas"));
    }
    out.push(data.into_any_element());
    let grammars = if h.stats.grammars.is_empty() { "ninguna".to_string() } else { h.stats.grammars.join(", ") };
    out.push(
        tile(theme, "Gramáticas cargadas")
            .child(big(theme, h.stats.grammars.len().to_string()))
            .child(sub(theme, grammars))
            .into_any_element(),
    );
    out.push(
        tile(theme, "Tiempo activo")
            .child(big(theme, h.uptime.map_or_else(dash, format_uptime)))
            .into_any_element(),
    );
    out
}

impl Render for DoctorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.model.update(cx, |m, cx| m.touch(cx));
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let pad = if compact { 12.0 } else { 16.0 };
        let health = self.model.read(cx).health().clone();
        let model = self.model.clone();
        let version = self.version.clone();
        let view = cx.entity();
        let copied = self.copied;

        let copy = Button::action("doctor-copy", &theme, if copied { "Copiado" } else { "Copiar diagnóstico" })
            .no_icon()
            .tooltip("Copia un resumen en markdown para pegarlo en un issue")
            .on_click(move |_, _, cx| {
                let md = model.read(cx).markdown(&version);
                cx.write_to_clipboard(ClipboardItem::new_string(md));
                view.update(cx, |p, cx| {
                    p.copied = true;
                    cx.notify();
                });
            });

        let all = tiles(&theme, &health);
        let body = if compact {
            div().flex().flex_col().gap(px(8.0)).children(all)
        } else {
            // Two columns: tiles alternate left / right.
            let (mut l, mut r) = (Vec::new(), Vec::new());
            for (i, t) in all.into_iter().enumerate() {
                if i % 2 == 0 { l.push(t) } else { r.push(t) }
            }
            div()
                .flex()
                .flex_row()
                .gap(px(8.0))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(8.0)).children(l))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(8.0)).children(r))
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .pt(px(pad))
            .px(px(pad))
            .font_family(theme.typography.ui_family)
            .text_color(c.text)
            .child(div().flex().flex_row().child(copy))
            .child(div().id("doctor-scroll").flex_1().min_h_0().overflow_y_scroll().pb(px(pad)).child(body))
    }
}
