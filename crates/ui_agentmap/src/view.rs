//! `AgentMapView`: the panel. A header (orchestrator, placement, view switch, totals), the toolbar
//! (search, filters, organization), the body (compact tree, graph or timeline) and, next to it or
//! over it, the inspector of the selected node.
//!
//! The view adapts to the width it really has (its zone, measured while painting): below 760 px
//! the inspector replaces the body instead of sitting beside it, below 340 px tree rows take two
//! lines. How the map is organized (view, grouping, order, filter, collapsed groups, placement)
//! lives in the model's [`MapPrefs`] so every view shares it and the host can remember it.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Window,
    div, prelude::*, px,
};
use panel_api::{PanelCtx, PanelId, PanelProvider, PanelZone};
use theme::{Radius, Theme};
use ui_kit::{
    EmptyState, Icon, MeasuredWidth, Select, SelectEvent, TextField, TextFieldEvent,
    UniformListScrollHandle, WireFill, WireRect, Wireframe,
};

use crate::activity_ui::ActivityUi;
use crate::camera::Cam;
use crate::events::AgentMapEvent;
use crate::model::{AgentMapModel, InspectorTab};
use crate::organize::{ListRow, Query, Scope, group_collapse_id, list_rows, node_collapse_id};
use crate::prefs::{MapPrefs, MapSession, Placement as MapPlacement};
use crate::tree::{Forest, NodeKey};

/// Id of the provider (what `layout.right_panel` stores).
pub const PANEL_ID: &str = "agentmap";
/// Icon of the map everywhere (panel, header, sidebar affordance).
/// `assets/icons/map.svg`, designed by the user.
pub const MAP_ICON: Icon = Icon::Map;

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
    /// Gantt bars over time, one lane per agent: shows the parallelism.
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

    /// Row height. While some visible row has a status line (a running node) the one-line layouts
    /// grow to make room for it under the name; the compact one already has two lines.
    pub(crate) fn height(self, live: bool) -> f32 {
        match self {
            RowLayout::Compact => 48.0,
            RowLayout::Lite | RowLayout::Full if live => 46.0,
            RowLayout::Lite | RowLayout::Full => 36.0,
        }
    }
}

pub(crate) fn key_value(k: &NodeKey) -> String {
    k.id_string()
}

/// What the view shows right now: the scope, the filters and the lines, computed once per use.
pub(crate) struct Shown {
    pub forest: Arc<Forest>,
    pub scope: Option<Scope>,
    pub prefs: MapPrefs,
    pub session: MapSession,
    pub now: i64,
}

impl Shown {
    pub(crate) fn query(&self) -> Query<'_> {
        Query {
            filter: self.prefs.filter,
            text: &self.session.query,
            hide_older_ms: self.session.hide_older_ms(),
            now_ms: self.now,
        }
    }

    pub(crate) fn rows(&self) -> Vec<ListRow> {
        match self.scope {
            Some(scope) => list_rows(&self.forest, scope, &self.prefs, &self.query()),
            None => Vec::new(),
        }
    }
}

/// A drag of the graph in progress (pointer events only).
#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphDrag {
    pub start: (f32, f32),
    pub pan0: (f32, f32),
    pub moved: bool,
}

pub struct AgentMapView {
    pub(crate) model: Entity<AgentMapModel>,
    pub(crate) theme: Theme,
    pub(crate) ctx: PanelCtx,
    pub(crate) pending: Option<Pending>,
    pub(crate) root_select: Entity<Select>,
    root_options: Vec<(String, String)>,
    pub(crate) search: Entity<TextField>,
    /// The organization panel (grouping, order, hide old) is open.
    pub(crate) options_open: bool,
    pub(crate) tree_scroll: UniformListScrollHandle,
    pub(crate) timeline_scroll: UniformListScrollHandle,
    pub(crate) inspector_scroll: ScrollHandle,
    pub(crate) tree_focus: FocusHandle,
    pub(crate) graph_focus: FocusHandle,
    /// Camera of the graph, its measured viewport and content size, and the drag in progress.
    pub(crate) cam: Cam,
    pub(crate) g_view: Rc<Cell<(f32, f32)>>,
    pub(crate) g_content: Rc<Cell<(f32, f32)>>,
    pub(crate) g_origin: Rc<Cell<(f32, f32)>>,
    pub(crate) g_drag: Rc<Cell<Option<GraphDrag>>>,
    /// The Actividad tab (feed list, filter, expanded rows).
    pub(crate) act: ActivityUi,
    pub(crate) measured: MeasuredWidth,
    focus: FocusHandle,
    /// Attached to the model (listening). A view hosted by a hidden tab is not.
    attached: bool,
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
        let initial = model.read(cx).session().query.clone();
        let search = cx.new(|cx| {
            TextField::new(theme, cx)
                .placeholder("Buscar por tarea, agente, modelo, proyecto…")
                .with_text(&initial)
        });
        let subs = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe(&root_select, |this, select, ev: &SelectEvent, cx| {
                let SelectEvent::Changed(_) = ev;
                let Some(value) = select.read(cx).value().cloned() else {
                    return;
                };
                this.choose_root(&value, cx);
            }),
            cx.subscribe(&search, |this, field, ev: &TextFieldEvent, cx| {
                if let TextFieldEvent::Changed = ev {
                    let text = field.read(cx).text().to_string();
                    this.model
                        .update(cx, |m, cx| m.update_session(|s| s.query = text, cx));
                }
            }),
        ];
        let attached = model.clone();
        cx.on_release(move |this, cx| {
            if this.attached {
                attached.update(cx, |m, cx| m.detach(cx));
            }
        })
        .detach();
        Self {
            model,
            theme,
            ctx,
            pending: None,
            root_select,
            root_options: Vec::new(),
            search,
            options_open: false,
            tree_scroll: UniformListScrollHandle::new(),
            timeline_scroll: UniformListScrollHandle::new(),
            inspector_scroll: ScrollHandle::new(),
            tree_focus: cx.focus_handle(),
            graph_focus: cx.focus_handle(),
            cam: Cam::default(),
            g_view: Rc::new(Cell::new((0.0, 0.0))),
            g_content: Rc::new(Cell::new((0.0, 0.0))),
            g_origin: Rc::new(Cell::new((0.0, 0.0))),
            g_drag: Rc::new(Cell::new(None)),
            act: ActivityUi::new(cx),
            measured: MeasuredWidth::with_breaks(&[COMPACT_BELOW, LITE_BELOW, SPLIT_FROM]),
            focus: cx.focus_handle(),
            attached: true,
            _subs: subs,
        }
    }

    /// Opens or closes the organization panel (grouping, order, hide old).
    pub fn set_options_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.options_open != open {
            self.options_open = open;
            cx.notify();
        }
    }

    /// Puts `text` in the search box (it filters the map as if it was typed).
    pub fn set_search_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_text(text, cx));
        let text = text.to_string();
        self.model
            .update(cx, |m, cx| m.update_session(|s| s.query = text, cx));
    }

    /// Attaches or detaches the view from the model. A view that is not on screen (a map tab behind
    /// another tab) must not keep the watchers alive: the host calls this when its visibility
    /// changes. Idempotent.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if active == self.attached {
            return;
        }
        self.attached = active;
        self.model.update(cx, |m, cx| {
            if active {
                m.attach(cx)
            } else {
                m.detach(cx)
            }
        });
        cx.notify();
    }

    pub fn is_active(&self) -> bool {
        self.attached
    }

    pub fn mode(&self, cx: &App) -> MapMode {
        self.model.read(cx).prefs().mode
    }

    pub fn set_mode(&mut self, mode: MapMode, cx: &mut Context<Self>) {
        if self.model.read(cx).prefs().mode != mode {
            self.cam = Cam::default();
            self.model.update(cx, |m, cx| m.update_prefs(|p| p.mode = mode, cx));
        }
    }

    /// The panel got a new context (width class, theme).
    pub fn set_context(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        if ctx.theme != self.theme {
            self.theme = ctx.theme;
            self.root_select
                .update(cx, |s, cx| s.set_theme(ctx.theme, cx));
            self.search.update(cx, |s, cx| s.set_theme(ctx.theme, cx));
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

    /// The inspector tab of the shown node.
    pub fn inspector_tab(&self, cx: &App) -> InspectorTab {
        self.model.read(cx).inspector_tab()
    }

    pub fn set_inspector_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        self.inspector_scroll
            .set_offset(gpui::point(px(0.0), px(0.0)));
        self.model.update(cx, |m, cx| m.set_inspector_tab(tab, cx));
    }

    /// Collapses or expands a node of the tree (remembered).
    pub(crate) fn toggle_node(&mut self, key: &NodeKey, cx: &mut Context<Self>) {
        self.toggle_collapse_id(&node_collapse_id(key), cx);
    }

    /// Collapses or expands a node or a group by its id (remembered).
    pub(crate) fn toggle_collapse_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.model
            .update(cx, |m, cx| m.update_prefs(|p| drop(p.toggle_collapsed(&id)), cx));
    }

    /// «Plegar todo» / «Desplegar todo» over what the list shows now.
    pub(crate) fn collapse_all(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let shown = self.shown(cx);
        let mut ids: Vec<String> = Vec::new();
        for r in shown.rows() {
            match r {
                ListRow::Group(g) => ids.push(g.id),
                ListRow::Node(n) if n.has_children => {
                    ids.push(node_collapse_id(&shown.forest.nodes[n.node].key));
                }
                ListRow::Node(_) => {}
            }
        }
        if collapsed {
            // collapsing a parent hides its subtree: the ones already hidden are not listed
            // above, so a second pass is not needed for the first level
        }
        self.model.update(cx, |m, cx| {
            m.update_prefs(|p| p.set_all_collapsed(ids, collapsed), cx)
        });
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
            self.cam = Cam::default();
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

    /// Everything the body needs, read once.
    pub(crate) fn shown(&self, cx: &App) -> Shown {
        let m = self.model.read(cx);
        let forest = m.forest().clone();
        let prefs = m.prefs().clone();
        let scope = if prefs.all_sessions {
            Some(Scope::All)
        } else {
            m.root_index().map(Scope::Root)
        };
        Shown {
            forest,
            scope: scope.filter(|_| !m.forest().is_empty()),
            prefs,
            session: m.session().clone(),
            now: store::now_ms(),
        }
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
                        .map(|(v, l)| ui_kit::SelectOption::new(v.clone(), l.clone())),
                    cx,
                );
            }
            s.select_value(&current_value, cx);
        });
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

    /// Nothing matches the filters.
    pub(crate) fn no_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let this = cx.entity();
        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(
                EmptyState::new(&t, "Sin resultados")
                    .icon(Icon::Search)
                    .body("Ninguna delegación coincide con la búsqueda o los filtros."),
            )
            .child(
                ui_kit::Button::tonal("agentmap-clear", &t, "Quitar filtros").on_click(
                    move |_, _, cx| this.update(cx, |v, cx| v.clear_filters(cx)),
                ),
            )
            .into_any_element()
    }

    /// Search text, status filter and «ocultar terminados» back to nothing.
    pub(crate) fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_text("", cx));
        self.model.update(cx, |m, cx| {
            m.update_session(
                |s| {
                    s.query.clear();
                    s.hide_older_min = None;
                },
                cx,
            );
            m.update_prefs(|p| p.filter = crate::prefs::StatusFilter::All, cx);
        });
    }

    /// Keys of the tree: up / down move the selection, left / right fold, Home / End jump.
    pub(crate) fn tree_key(&mut self, key: &str, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let shown = self.shown(cx);
        let rows = shown.rows();
        let nodes: Vec<(usize, &crate::tree::Row)> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                ListRow::Node(n) => Some((i, n)),
                ListRow::Group(_) => None,
            })
            .collect();
        if nodes.is_empty() {
            return false;
        }
        let selected = self.model.read(cx).selected().cloned();
        let cur = selected
            .as_ref()
            .and_then(|k| nodes.iter().position(|(_, n)| &shown.forest.nodes[n.node].key == k));
        let go = |this: &mut Self, at: usize, cx: &mut Context<Self>| {
            let (row_ix, n) = nodes[at];
            this.tree_scroll
                .scroll_to_item(row_ix, gpui::ScrollStrategy::Center);
            this.select(Some(shown.forest.nodes[n.node].key.clone()), cx);
        };
        match key {
            "down" => go(self, cur.map_or(0, |c| (c + 1).min(nodes.len() - 1)), cx),
            "up" => go(self, cur.map_or(0, |c| c.saturating_sub(1)), cx),
            "home" => go(self, 0, cx),
            "end" => go(self, nodes.len() - 1, cx),
            "left" | "right" => {
                let Some(c) = cur else { return false };
                let n = nodes[c].1;
                let id = node_collapse_id(&shown.forest.nodes[n.node].key);
                if n.has_children && (key == "left") != n.collapsed {
                    self.toggle_collapse_id(&id, cx);
                } else if key == "left" && n.depth > 0 {
                    // to the parent
                    if let Some(p) = shown.forest.nodes[n.node].parent
                        && let Some(at) = nodes.iter().position(|(_, x)| x.node == p)
                    {
                        go(self, at, cx);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// The id of a group of the current grouping (tests and the host use it to fold one).
    pub fn group_id(prefix: &str, key: &str) -> String {
        group_collapse_id(prefix, key)
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

        let (loaded, selected_key) = {
            let m = self.model.read(cx);
            (m.is_loaded(), m.selected().cloned())
        };
        let shown = self.shown(cx);
        let forest = shown.forest.clone();
        let selected = selected_key
            .as_ref()
            .and_then(|k| forest.index_of(k).map(|i| (k.clone(), i)));

        let header = self.header(&shown, narrow, cx);
        let main_w = if split && selected.is_some() {
            root_w - INSPECTOR_WIDTH - 12.0
        } else {
            root_w
        };

        let body: AnyElement = match shown.scope {
            Some(scope) => match shown.prefs.mode {
                MapMode::Tree => {
                    let rows = shown.rows();
                    if rows.is_empty() {
                        self.no_results(cx)
                    } else {
                        self.tree_body(forest, rows, selected_key.clone(), main_w, cx)
                    }
                }
                MapMode::Graph => self.graph_body(&shown, scope, selected_key.clone(), cx),
                MapMode::Timeline => {
                    self.timeline_body(&shown, scope, selected_key.clone(), main_w, cx)
                }
            },
            None => self.empty(loaded),
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

        let hide_header = !split && selected_key.is_some() && shown.scope.is_some();
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

/// Where a map view lives, as the placement preference says it (re-exported for the host).
pub fn placement_of(prefs: &MapPrefs) -> MapPlacement {
    prefs.placement
}


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
