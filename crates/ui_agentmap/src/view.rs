//! `AgentMapView`: the panel. A header (orchestrator, view switch, totals), the body (compact tree,
//! graph or timeline) and, next to it or over it, the inspector of the selected node.
//!
//! The view adapts to the width it really has (its zone, measured while painting): below 760 px
//! the inspector replaces the body instead of sitting beside it, below 340 px tree rows take two
//! lines.

use std::collections::HashSet;

use gpui::{
    AnyElement, AnyView, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Window,
    div, prelude::*, px,
};
use panel_api::{PanelCtx, PanelId, PanelProvider, PanelZone};
use theme::{Radius, Theme};
use ui_kit::{
    EmptyState, Icon, MeasuredWidth, SegmentedControl, Select, SelectEvent, SelectOption,
    UniformListScrollHandle, WireFill, WireRect, Wireframe, icon,
};

use crate::events::AgentMapEvent;
use crate::fmt;
use crate::model::AgentMapModel;
use crate::tree::{NodeKey, visible_rows};

/// Id of the provider (what `layout.right_panel` stores).
pub const PANEL_ID: &str = "agentmap";
/// Icon of the map everywhere (panel, header, sidebar affordance).
/// TODO-SVG: `assets/icons/map.svg` (16x16, `currentColor`), designed by the user. Until it exists the
/// map borrows the agents icon; swap this one constant when the file is added.
pub const MAP_ICON: Icon = Icon::Agents;

/// Width from which the inspector sits beside the body instead of replacing it.
pub const SPLIT_FROM: f32 = 760.0;
/// Width of the inspector column in the split layout.
pub const INSPECTOR_WIDTH: f32 = 340.0;
/// Below this the tree rows use two lines.
pub const COMPACT_BELOW: f32 = 340.0;
/// Below this the tree drops the files count and the worktree pill.
pub const LITE_BELOW: f32 = 880.0;

/// How the session is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMode {
    /// Compact vertical tree (the default: fast and readable).
    Tree,
    /// Nodes in columns by depth with connector lines.
    Graph,
    /// Gantt bars over time: shows the parallelism.
    Timeline,
}

impl MapMode {
    pub const ALL: [MapMode; 3] = [MapMode::Tree, MapMode::Graph, MapMode::Timeline];

    pub fn label(self) -> &'static str {
        match self {
            MapMode::Tree => "Árbol",
            MapMode::Graph => "Grafo",
            MapMode::Timeline => "Línea de tiempo",
        }
    }

    /// The label for a narrow panel (the segmented control must not clip).
    pub fn short_label(self) -> &'static str {
        match self {
            MapMode::Timeline => "Tiempo",
            other => other.label(),
        }
    }
}

/// An action waiting for the user's confirmation inside the inspector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    Cancel(NodeKey),
    Retry { key: NodeKey, agent: String },
    StopExternal { key: NodeKey, pid: u32 },
}

/// Row layout of the tree for a given width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowLayout {
    Compact,
    Lite,
    Full,
}

impl RowLayout {
    pub(crate) fn from_width(w: f32) -> Self {
        if w < COMPACT_BELOW {
            RowLayout::Compact
        } else if w < LITE_BELOW {
            RowLayout::Lite
        } else {
            RowLayout::Full
        }
    }

    pub(crate) fn height(self) -> f32 {
        match self {
            RowLayout::Compact => 48.0,
            RowLayout::Lite | RowLayout::Full => 36.0,
        }
    }
}

fn key_value(k: &NodeKey) -> String {
    match k {
        NodeKey::Thread(t) => format!("t:{}", t.0),
        NodeKey::Tab(s) => format!("b:{s}"),
        NodeKey::External { pid, started_ms } => format!("e:{pid}:{started_ms}"),
    }
}

pub struct AgentMapView {
    pub(crate) model: Entity<AgentMapModel>,
    pub(crate) theme: Theme,
    pub(crate) ctx: PanelCtx,
    pub(crate) mode: MapMode,
    pub(crate) collapsed: HashSet<NodeKey>,
    pub(crate) pending: Option<Pending>,
    root_select: Entity<Select>,
    root_options: Vec<(String, String)>,
    pub(crate) tree_scroll: UniformListScrollHandle,
    pub(crate) timeline_scroll: UniformListScrollHandle,
    pub(crate) graph_scroll: ScrollHandle,
    pub(crate) inspector_scroll: ScrollHandle,
    pub(crate) measured: MeasuredWidth,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<AgentMapEvent> for AgentMapView {}

impl Focusable for AgentMapView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AgentMapView {
    /// Builds the view and attaches it to the model: from now on the model listens to the registry
    /// and the chat store. When the view is dropped it detaches (and the model goes silent).
    pub fn new(ctx: PanelCtx, model: Entity<AgentMapModel>, cx: &mut Context<Self>) -> Self {
        ui_kit::init(cx);
        let theme = ctx.theme;
        model.update(cx, |m, cx| m.attach(cx));
        let root_select = cx.new(|cx| Select::new("agentmap-root", theme, cx).width(220.0));
        let subs = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe(&root_select, |this, select, ev: &SelectEvent, cx| {
                let SelectEvent::Changed(_) = ev;
                let Some(value) = select.read(cx).value().cloned() else {
                    return;
                };
                this.choose_root(&value, cx);
            }),
        ];
        let attached = model.clone();
        cx.on_release(move |_, cx| attached.update(cx, |m, cx| m.detach(cx)))
            .detach();
        Self {
            model,
            theme,
            ctx,
            mode: MapMode::Tree,
            collapsed: HashSet::new(),
            pending: None,
            root_select,
            root_options: Vec::new(),
            tree_scroll: UniformListScrollHandle::new(),
            timeline_scroll: UniformListScrollHandle::new(),
            graph_scroll: ScrollHandle::new(),
            inspector_scroll: ScrollHandle::new(),
            measured: MeasuredWidth::with_breaks(&[COMPACT_BELOW, LITE_BELOW, SPLIT_FROM]),
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn mode(&self) -> MapMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: MapMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
    }

    /// The panel got a new context (width class, theme).
    pub fn set_context(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        if ctx.theme != self.theme {
            self.theme = ctx.theme;
            self.root_select
                .update(cx, |s, cx| s.set_theme(ctx.theme, cx));
        }
        self.ctx = ctx;
        cx.notify();
    }

    pub fn selected(&self, cx: &App) -> Option<NodeKey> {
        self.model.read(cx).selected().cloned()
    }

    pub fn select(&mut self, key: Option<NodeKey>, cx: &mut Context<Self>) {
        self.pending = None;
        self.inspector_scroll
            .set_offset(gpui::point(px(0.0), px(0.0)));
        self.model.update(cx, |m, cx| m.select(key, cx));
    }

    pub(crate) fn toggle_collapsed(&mut self, key: NodeKey, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        cx.notify();
    }

    fn choose_root(&mut self, value: &str, cx: &mut Context<Self>) {
        let key = self
            .model
            .read(cx)
            .root_choices()
            .into_iter()
            .map(|(k, _, _)| k)
            .find(|k| key_value(k) == value);
        if let Some(k) = key {
            self.pending = None;
            self.model.update(cx, |m, cx| {
                m.select(None, cx);
                m.set_root(Some(k), cx);
            });
        }
    }

    /// Emits an inspector action on the view and forwards it to the model (the host subscribes to
    /// whichever it holds).
    pub(crate) fn emit_action(&mut self, ev: AgentMapEvent, cx: &mut Context<Self>) {
        cx.emit(ev.clone());
        self.model.update(cx, |_, cx| cx.emit(ev));
    }

    fn sync_root_select(&mut self, cx: &mut Context<Self>) {
        let (choices, current) = {
            let m = self.model.read(cx);
            (m.root_choices(), m.root_index())
        };
        let options: Vec<(String, String)> = choices
            .iter()
            .map(|(k, title, active)| {
                let label = if *active {
                    format!("{title} · en marcha")
                } else {
                    title.clone()
                };
                (key_value(k), label)
            })
            .collect();
        let current_value = current
            .and_then(|i| {
                self.model
                    .read(cx)
                    .forest()
                    .nodes
                    .get(i)
                    .map(|n| key_value(&n.key))
            })
            .unwrap_or_default();
        let changed = options != self.root_options;
        if changed {
            self.root_options = options.clone();
        }
        let theme = self.theme;
        self.root_select.update(cx, |s, cx| {
            s.set_theme(theme, cx);
            if changed {
                s.set_options(
                    options
                        .iter()
                        .map(|(v, l)| SelectOption::new(v.clone(), l.clone())),
                    cx,
                );
            }
            s.select_value(&current_value, cx);
        });
    }

    fn header(&mut self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (root, forest, loaded) = {
            let m = self.model.read(cx);
            (m.root_index(), m.forest().clone(), m.is_loaded())
        };
        let many = forest.roots.len() > 1;
        let totals = root.map(|r| forest.totals(r));
        let summary: Option<String> = totals.filter(|_| loaded).map(|t| {
            let mut parts: Vec<String> = Vec::new();
            if t.runs == 0 && t.external == 0 {
                parts.push("Sin delegaciones".to_string());
            } else {
                if t.runs > 0 {
                    parts.push(match t.runs {
                        1 => "1 delegación".to_string(),
                        n => format!("{n} delegaciones"),
                    });
                }
                if t.external > 0 {
                    parts.push(format!("{} fuera de Selachi", t.external));
                }
            }
            if t.running > 0 {
                parts.push(format!("{} en marcha", t.running));
            }
            if t.waiting > 0 {
                parts.push(format!("{} esperan aprobación", t.waiting));
            }
            if t.failed > 0 {
                parts.push(format!("{} fallaron", t.failed));
            }
            if t.tokens > 0 {
                parts.push(format!("{} tokens", fmt::tokens(t.tokens)));
            }
            if t.cost_usd > 0.0 {
                parts.push(fmt::cost(t.cost_usd));
            }
            parts.join(" · ")
        });
        let this = cx.entity();
        let modes = SegmentedControl::new(
            "agentmap-mode",
            &t,
            MapMode::ALL
                .iter()
                .map(|m| SharedString::from(if narrow { m.short_label() } else { m.label() })),
        )
        .selected(
            MapMode::ALL
                .iter()
                .position(|m| *m == self.mode)
                .unwrap_or(0),
        )
        .track(c.surface_2)
        .on_change(move |ix, _, cx| {
            let mode = MapMode::ALL.get(ix).copied().unwrap_or(MapMode::Tree);
            this.update(cx, |v, cx| v.set_mode(mode, cx));
        });
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(MAP_ICON, 16.0, c.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(ty.ui_size_lg))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child("Mapa de subagentes"),
                    )
                    .when(many && !narrow, |d| d.child(self.root_select.clone())),
            )
            .when(many && narrow, |d| d.child(self.root_select.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(modes)
                    .when_some(summary, |d, s| {
                        d.child(
                            div()
                                .min_w(px(0.0))
                                .text_size(px(ty.ui_size_sm))
                                .text_color(c.muted)
                                .child(SharedString::from(s)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn empty(&self, loaded: bool) -> AnyElement {
        let t = self.theme;
        let (title, body) = if loaded {
            (
                "Sin delegaciones todavía",
                "Cuando un agente delegue trabajo en otro con `selachi agent run` o el servidor MCP, \
                 el árbol de la sesión aparece aquí.",
            )
        } else {
            ("Leyendo la sesión…", "")
        };
        let mut e = EmptyState::new(&t, title).icon(Icon::Agents).fill(true);
        if !body.is_empty() {
            e = e.body(body);
        }
        div().flex_1().min_h(px(0.0)).child(e).into_any_element()
    }
}

impl Render for AgentMapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let root_w = self
            .measured
            .width()
            .unwrap_or_else(|| f32::from(window.viewport_size().width));
        let narrow = root_w < 460.0;
        let split = root_w >= SPLIT_FROM;
        self.sync_root_select(cx);

        let (root, forest, loaded, selected_key) = {
            let m = self.model.read(cx);
            (
                m.root_index(),
                m.forest().clone(),
                m.is_loaded(),
                m.selected().cloned(),
            )
        };
        let selected = selected_key
            .as_ref()
            .and_then(|k| forest.index_of(k).map(|i| (k.clone(), i)));

        let header = self.header(narrow, cx);
        let main_w = if split && selected.is_some() {
            root_w - INSPECTOR_WIDTH - 12.0
        } else {
            root_w
        };

        let body: AnyElement = match root {
            Some(root) if !forest.is_empty() => match self.mode {
                MapMode::Tree => {
                    let rows = visible_rows(&forest, root, &self.collapsed);
                    self.tree_body(forest, rows, selected_key.clone(), main_w, cx)
                }
                MapMode::Graph => self.graph_body(forest, root, selected_key.clone(), cx),
                MapMode::Timeline => {
                    self.timeline_body(forest, root, selected_key.clone(), main_w, cx)
                }
            },
            _ => self.empty(loaded),
        };

        let inspector = selected.map(|(key, idx)| self.inspector(key, idx, split, cx));

        let content: AnyElement = match (inspector, split) {
            (Some(ins), true) => div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_row()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .min_h(px(0.0))
                        .flex()
                        .flex_col()
                        .child(body),
                )
                .child(
                    div()
                        .w(px(INSPECTOR_WIDTH))
                        .flex_none()
                        .min_h(px(0.0))
                        .rounded(px(Radius::MD))
                        .bg(c.surface_2)
                        .child(ins),
                )
                .into_any_element(),
            // Narrow: the inspector takes the place of the body (it has its own "back" button).
            (Some(ins), false) => div()
                .flex_1()
                .min_h(px(0.0))
                .rounded(px(Radius::MD))
                .bg(c.surface_2)
                .child(ins)
                .into_any_element(),
            (None, _) => div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .child(body)
                .into_any_element(),
        };

        let hide_header = !split && selected_key.is_some() && root.is_some();
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(if narrow { 10.0 } else { 14.0 }))
            .font_family(t.typography.ui_family)
            .text_size(px(t.typography.ui_size_md))
            .text_color(c.text)
            .track_focus(&self.focus)
            .child(self.measured.probe())
            .when(!hide_header, |d| d.child(header))
            .child(content)
    }
}

// ---------------------------------------------------------------------------------------------
// Panel provider
// ---------------------------------------------------------------------------------------------

/// `PanelProvider` of the map (id `agentmap`, zones Main and Right).
pub struct AgentMapPanelProvider {
    model: Entity<AgentMapModel>,
}

impl AgentMapPanelProvider {
    pub fn new(model: Entity<AgentMapModel>) -> Self {
        Self { model }
    }
}

impl PanelProvider for AgentMapPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Mapa de subagentes".into()
    }

    fn icon(&self) -> Icon {
        MAP_ICON
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Main, PanelZone::Right]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let model = self.model.clone();
        cx.new(|cx| AgentMapView::new(ctx, model, cx)).into()
    }

    fn context_changed(
        &self,
        view: &AnyView,
        ctx: &PanelCtx,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Ok(v) = view.clone().downcast::<AgentMapView>() else {
            return false;
        };
        v.update(cx, |v, cx| v.set_context(ctx.clone(), cx));
        true
    }
}

/// 1-bit thumbnail for the layout panel: header, root row and two levels of delegations.
pub fn thumbnail(theme: &Theme) -> Wireframe {
    use WireFill::{Dither, Solid};
    let r = WireRect::new;
    Wireframe::new(theme).rects([
        r(1, 1, 12, 2, Solid),
        r(18, 1, 13, 2, Solid),
        r(1, 5, 14, 1, Solid),
        r(25, 5, 6, 1, Dither),
        r(3, 8, 3, 1, Solid),
        r(7, 8, 13, 1, Dither),
        r(25, 8, 6, 1, Solid),
        r(5, 10, 3, 1, Solid),
        r(9, 10, 10, 1, Dither),
        r(25, 10, 6, 1, Solid),
        r(3, 13, 3, 1, Solid),
        r(7, 13, 11, 1, Dither),
        r(25, 13, 6, 1, Solid),
        r(3, 16, 3, 1, Solid),
        r(7, 16, 14, 1, Dither),
        r(25, 16, 6, 1, Solid),
    ])
}
