//! The graph (nodes in columns by depth, elbow connectors painted with quads) and the timeline
//! (a simple Gantt: one bar per delegation over time, which shows the parallelism).

use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, canvas, div, fill, point, prelude::*, px,
    relative, size,
};
use theme::{Radius, Theme};
use ui_kit::{EmptyState, Icon, VirtualList};

use crate::fmt;
use crate::parts::{external_badge, lead_mark, metrics, node_status_color, status_mark};
use crate::tree::{Forest, NodeKey, graph_layout, timeline};
use crate::tree_ui::{node_element_id, node_id_string};
use crate::view::AgentMapView;

pub(crate) const NODE_W: f32 = 212.0;
pub(crate) const NODE_H: f32 = 64.0;
pub(crate) const COL_GAP: f32 = 56.0;
pub(crate) const ROW_GAP: f32 = 14.0;
const PAD: f32 = 12.0;
/// Connector thickness.
const LINE: f32 = 2.0;
/// Height of a timeline row.
pub(crate) const TL_ROW: f32 = 30.0;

fn node_card(
    theme: &Theme,
    forest: &Forest,
    node: usize,
    selected: bool,
    now_ms: i64,
    this: &Entity<AgentMapView>,
) -> gpui::Stateful<gpui::Div> {
    let c = theme.colors;
    let ty = theme.typography;
    let n = &forest.nodes[node];
    let external = n.is_external();
    let key = n.key.clone();
    let m = metrics(n, now_ms);
    let line: Vec<String> = [m[0].clone(), m[1].clone(), m[2].clone()]
        .into_iter()
        .flatten()
        .collect();
    let this = this.clone();
    let click = key.clone();
    let card_bg = match (selected, external) {
        (true, true) => c.surface_3,
        (true, false) => c.surface_4,
        (false, true) => c.surface_1,
        (false, false) => c.surface_2,
    };
    let hover_bg = if external { c.surface_2 } else { c.surface_3 };
    div()
        .id(node_element_id("agentmap-node", &key))
        .debug_selector({
            let s = node_id_string("agentmap-node", &key);
            move || s
        })
        .occlude()
        .w(px(NODE_W))
        .h(px(NODE_H))
        .px(px(10.0))
        .py(px(8.0))
        .flex()
        .flex_col()
        .justify_between()
        .rounded(px(Radius::SM))
        .bg(card_bg)
        .when(!selected, |d| d.hover(move |s| s.bg(hover_bg)))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let k = click.clone();
            this.update(cx, |v, cx| v.select(Some(k), cx));
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(lead_mark(theme, n, 16.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(ty.ui_size_md))
                        .font_weight(if n.is_root() {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if external { c.muted } else { c.text })
                        .child(SharedString::from(n.title.clone())),
                )
                .when_some(status_mark(theme, n.status), |d, s| d.child(s)),
        )
        .child(
            div()
                .min_w(px(0.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(SharedString::from(
                    fmt::model_effort(n.model.as_deref(), n.effort.as_deref())
                        .unwrap_or_else(|| crate::tree::agent_display(&n.agent)),
                )),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .when(external, |d| d.child(external_badge(theme)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(ty.ui_size_xs))
                        .text_color(c.muted)
                        .child(SharedString::from(line.join(" · "))),
                ),
        )
}

impl AgentMapView {
    /// Nodes in columns by depth with elbow connectors. Scrolls both ways; only the cards inside
    /// the viewport are built.
    pub(crate) fn graph_body(
        &mut self,
        forest: Arc<Forest>,
        root: usize,
        selected: Option<NodeKey>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let layout = graph_layout(&forest, root, &self.collapsed);
        let cols = layout.cols.max(1) as f32;
        let rows = layout.rows.max(1.0).ceil();
        let width = PAD * 2.0 + cols * NODE_W + (cols - 1.0) * COL_GAP;
        let height = PAD * 2.0 + rows * NODE_H + (rows - 1.0) * ROW_GAP;
        let pos = |col: usize, row: f32| {
            (
                PAD + col as f32 * (NODE_W + COL_GAP),
                PAD + row * (NODE_H + ROW_GAP),
            )
        };

        // What is on screen (nothing measured yet: everything).
        let view = self.graph_scroll.bounds();
        let off = self.graph_scroll.offset();
        let (vw, vh) = (f32::from(view.size.width), f32::from(view.size.height));
        let (vx, vy) = (-f32::from(off.x), -f32::from(off.y));
        let cull = vw > 0.0 && vh > 0.0;
        let margin = NODE_W;
        let visible = |x: f32, y: f32| {
            !cull
                || (x + NODE_W >= vx - margin
                    && x <= vx + vw + margin
                    && y + NODE_H >= vy - margin
                    && y <= vy + vh + margin)
        };

        let this = cx.entity();
        let now = store::now_ms();
        let mut inner = div().relative().w(px(width)).h(px(height));

        // Connectors first (under the cards).
        let edges: Vec<(f32, f32, f32, f32, bool, bool)> = layout
            .edges
            .iter()
            .map(|(p, ch)| {
                let (a, b) = (layout.placed[*p], layout.placed[*ch]);
                let (ax, ay) = pos(a.col, a.row);
                let (bx, by) = pos(b.col, b.row);
                let child_node = &forest.nodes[b.node];
                let active = child_node.status.is_active();
                let external = child_node.is_external();
                (
                    ax + NODE_W,
                    ay + NODE_H / 2.0,
                    bx,
                    by + NODE_H / 2.0,
                    active,
                    external,
                )
            })
            .collect();
        let (line, line_live, line_ext) = (c.outline, c.accent, c.outline_soft);
        inner = inner.child(
            canvas(
                |_, _, _| (),
                move |bounds: Bounds<gpui::Pixels>, (), window, _| {
                    let o = bounds.origin;
                    let rect = |x: f32, y: f32, w: f32, h: f32| {
                        Bounds::new(point(o.x + px(x), o.y + px(y)), size(px(w), px(h)))
                    };
                    for (x0, y0, x1, y1, live, ext) in &edges {
                        let color = if *ext {
                            line_ext
                        } else if *live {
                            line_live
                        } else {
                            line
                        };
                        let mid = (x0 + (x1 - x0) / 2.0).floor();
                        window.paint_quad(fill(rect(*x0, y0 - LINE / 2.0, mid - x0, LINE), color));
                        let (top, bottom) = (y0.min(*y1), y0.max(*y1));
                        window.paint_quad(fill(
                            rect(
                                mid - LINE / 2.0,
                                top - LINE / 2.0,
                                LINE,
                                bottom - top + LINE,
                            ),
                            color,
                        ));
                        window.paint_quad(fill(rect(mid, y1 - LINE / 2.0, x1 - mid, LINE), color));
                    }
                },
            )
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .w(px(width))
            .h(px(height)),
        );

        for p in &layout.placed {
            let (x, y) = pos(p.col, p.row);
            if !visible(x, y) {
                continue;
            }
            let sel = selected.as_ref() == Some(&forest.nodes[p.node].key);
            inner = inner.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .child(node_card(&theme, &forest, p.node, sel, now, &this)),
            );
        }

        div()
            .id("agentmap-graph")
            .flex_1()
            .min_h(px(0.0))
            .overflow_scroll()
            .track_scroll(&self.graph_scroll)
            .child(inner)
            .into_any_element()
    }

    /// One bar per delegation over time.
    pub(crate) fn timeline_body(
        &mut self,
        forest: Arc<Forest>,
        root: usize,
        selected: Option<NodeKey>,
        width: f32,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let now = store::now_ms();
        let tl = Arc::new(timeline(&forest, root, now));
        if tl.bars.is_empty() {
            return div()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    EmptyState::new(&theme, "Sin ejecuciones con hora de inicio")
                        .icon(Icon::History)
                        .fill(true),
                )
                .into_any_element();
        }
        let label_w = if width < 460.0 { 110.0 } else { 168.0 };
        let this = cx.entity();
        let span = tl.span_ms();
        let axis = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(22.0))
            .text_size(px(ty.ui_size_xs))
            .text_color(c.muted)
            .child(
                div()
                    .w(px(label_w))
                    .flex_none()
                    .child(SharedString::from(format!(
                        "Paralelismo máx. {}",
                        tl.peak_parallel
                    ))),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child("0 s")
                    .child(SharedString::from(fmt::duration_ms(span / 2)))
                    .child(SharedString::from(fmt::duration_ms(span))),
            );

        let forest_rows = forest;
        let tl_rows = tl.clone();
        let list = VirtualList::new(
            "agentmap-timeline",
            tl.bars.len(),
            TL_ROW,
            move |range, _, _| {
                range
                    .filter_map(|i| tl_rows.bars.get(i).copied())
                    .map(|bar| {
                        let node = &forest_rows.nodes[bar.node];
                        let sel = selected.as_ref() == Some(&node.key);
                        let this = this.clone();
                        let key = node.key.clone();
                        let start = tl_rows.frac(bar.start_ms);
                        let end = tl_rows.frac(bar.end_ms);
                        let dur = node.duration_ms(now).map(fmt::duration_ms);
                        div()
                            .id(node_element_id("agentmap-bar", &node.key))
                            .debug_selector({
                                let s = node_id_string("agentmap-bar", &node.key);
                                move || s
                            })
                            .occlude()
                            .w_full()
                            .h(px(TL_ROW - 4.0))
                            .my(px(2.0))
                            .px(px(6.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .rounded(px(Radius::PILL))
                            .cursor_pointer()
                            .when(sel, |d| d.bg(c.surface_3))
                            .when(!sel, |d| d.hover(|s| s.bg(c.surface_2)))
                            .on_click(move |_, _, cx| {
                                let k = key.clone();
                                this.update(cx, |v, cx| v.select(Some(k), cx));
                            })
                            .child(
                                div()
                                    .w(px(label_w - 6.0))
                                    .flex_none()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(lead_mark(&theme, node, 16.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(ty.ui_size_sm))
                                            .text_color(if node.is_external() {
                                                c.muted
                                            } else {
                                                c.text
                                            })
                                            .child(SharedString::from(node.title.clone())),
                                    ),
                            )
                            .when(node.is_external() && width >= 460.0, |d| {
                                d.child(external_badge(&theme))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(14.0))
                                    .relative()
                                    .rounded(px(Radius::CHIP))
                                    .bg(c.surface_2)
                                    .child(
                                        div()
                                            .absolute()
                                            .top(px(0.0))
                                            .h_full()
                                            .left(relative(start))
                                            .w(relative((end - start).max(0.0)))
                                            .min_w(px(4.0))
                                            .rounded(px(Radius::CHIP))
                                            .bg(node_status_color(&theme, node)),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(52.0))
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .text_size(px(ty.ui_size_xs))
                                    .text_color(c.muted)
                                    .when_some(dur, |d, t| d.child(SharedString::from(t))),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
        )
        .track_scroll(self.timeline_scroll.clone())
        .flex_1();

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(axis)
            .child(list)
            .into_any_element()
    }
}
