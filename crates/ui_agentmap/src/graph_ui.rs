//! The graph (compact cards in columns by depth, elbow connectors painted with quads, auto-fit,
//! wheel zoom around the pointer and drag to pan, arrows to move between nodes) and the timeline
//! (a simple Gantt with one lane per agent that shows the parallelism).
//!
//! Pointer events only: the wheel and the mouse buttons. Nothing runs while nothing moves.

use std::collections::HashMap;

use gpui::{
    AnyElement, Bounds, DispatchPhase, Entity, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, canvas, div, fill, point, prelude::*, px, relative, size,
};
use theme::{Radius, Theme};
use ui_kit::{EmptyState, Icon, VirtualList};

use crate::camera::{self, Cam};
use crate::fmt;
use crate::organize::{
    Dir, LaneRow, Scope, keep_flags, nav_target, node_collapse_id, timeline_lanes,
};
use crate::parts::{external_badge, lead_mark, metrics, model_chip, node_status_color, status_dot, status_mark};
use crate::toolbar::pill_toggle;
use crate::tree::{Forest, GraphLayout, MapNode, NodeKey, graph_layout_of};
use crate::tree_ui::{group_row, node_element_id, node_id_string};
use crate::view::{AgentMapView, GraphDrag, Shown};

pub(crate) const NODE_W: f32 = 188.0;
pub(crate) const NODE_H: f32 = 52.0;
pub(crate) const COL_GAP: f32 = 40.0;
pub(crate) const ROW_GAP: f32 = 10.0;
const PAD: f32 = 12.0;
/// Connector thickness.
const LINE: f32 = 2.0;
/// Fixed line heights of the two rows of a card (title, chips): gpui would use 1.618 x the font size.
const TITLE_H: f32 = 16.0;
const META_H: f32 = 18.0;
/// Zoom from which a card has two lines, and from which it shows text at all.
const TWO_LINES_FROM: f32 = 0.8;
const TEXT_FROM: f32 = 0.45;
/// Height of a timeline row.
pub(crate) const TL_ROW: f32 = 30.0;

/// The graph layout of what the view shows now (filters, order and folds applied).
pub(crate) fn layout_for(shown: &Shown, scope: Scope) -> GraphLayout {
    let q = shown.query();
    let roots = scope.roots(&shown.forest);
    let keep = keep_flags(&shown.forest, &roots, &q);
    let roots: Vec<usize> = roots
        .into_iter()
        .filter(|r| keep.as_ref().is_none_or(|k| k[*r]))
        .collect();
    let searching = !q.text.trim().is_empty();
    let prefs = &shown.prefs;
    graph_layout_of(
        &shown.forest,
        &roots,
        &|k| !searching && prefs.is_collapsed(&node_collapse_id(k)),
        keep.as_deref(),
        prefs.sort,
        shown.now,
    )
}

fn content_size(layout: &GraphLayout) -> (f32, f32) {
    let cols = layout.cols.max(1) as f32;
    let rows = layout.rows.max(1.0).ceil();
    (
        PAD * 2.0 + cols * NODE_W + (cols - 1.0) * COL_GAP,
        PAD * 2.0 + rows * NODE_H + (rows - 1.0) * ROW_GAP,
    )
}

fn pos(col: usize, row: f32) -> (f32, f32) {
    (
        PAD + col as f32 * (NODE_W + COL_GAP),
        PAD + row * (NODE_H + ROW_GAP),
    )
}

#[allow(clippy::too_many_arguments)]
fn node_card(
    theme: &Theme,
    n: &MapNode,
    selected: bool,
    zoom: f32,
    now_ms: i64,
    this: &Entity<AgentMapView>,
    live: Option<&str>,
) -> gpui::Stateful<gpui::Div> {
    let c = theme.colors;
    let ty = theme.typography;
    let external = n.is_external();
    let key = n.key.clone();
    let this = this.clone();
    let click = key.clone();
    let card_bg = match (selected, external) {
        (true, true) => c.surface_3,
        (true, false) => c.surface_4,
        (false, true) => c.surface_1,
        (false, false) => c.surface_2,
    };
    let hover_bg = if external { c.surface_2 } else { c.surface_3 };
    let (w, h) = (NODE_W * zoom, NODE_H * zoom);
    let base = div()
        .id(node_element_id("agentmap-node", &key))
        .debug_selector({
            let s = node_id_string("agentmap-node", &key);
            move || s
        })
        .occlude()
        .w(px(w))
        .h(px(h))
        .rounded(px(Radius::SM))
        .bg(card_bg)
        .when(!selected, |d| d.hover(move |s| s.bg(hover_bg)))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let k = click.clone();
            this.update(cx, |v, cx| v.select(Some(k), cx));
        });
    if zoom < TEXT_FROM {
        // far away: only the status color, so the shape of the session still reads
        return base.p(px(3.0)).child(
            div()
                .size_full()
                .rounded(px(Radius::CHIP))
                .bg(node_status_color(theme, n)),
        );
    }
    let title_color = if external { c.muted } else { c.text };
    let title = div()
        .flex_1()
        .min_w(px(0.0))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_size(px(ty.ui_size_md))
        .line_height(px(TITLE_H))
        .font_weight(if n.is_root() {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .text_color(title_color)
        .child(SharedString::from(n.title.clone()));
    if zoom < TWO_LINES_FROM {
        return base
            .px(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(status_dot(theme, n, 8.0))
            .child(title);
    }
    let m = metrics(n, now_ms);
    let line: Vec<String> = m.iter().take(3).flatten().cloned().collect();
    let model = fmt::model_effort(n.model.as_deref(), n.effort.as_deref())
        .unwrap_or_else(|| crate::tree::agent_display(&n.agent));
    base.px(px(10.0))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(4.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .h(px(TITLE_H))
                .child(lead_mark(theme, n, 14.0))
                .child(title)
                .when_some(status_mark(theme, n.status), |d, s| d.child(s)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(META_H))
                .when(external, |d| d.child(external_badge(theme)))
                .when(!external && live.is_none(), |d| {
                    d.child(model_chip(theme, model.clone(), 118.0))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(ty.ui_size_xs))
                        .line_height(px(META_H))
                        .text_color(if live.is_some() { c.accent } else { c.muted })
                        .child(SharedString::from(match live {
                            Some(l) => l.to_string(),
                            None => line.join(" · "),
                        })),
                ),
        )
}

impl AgentMapView {
    /// Arrow keys between nodes, `0` fits, `+` / `-` zoom. `true` when the key was used.
    pub(crate) fn graph_key(&mut self, key: &str, cx: &mut gpui::Context<Self>) -> bool {
        let dir = match key {
            "left" => Some(Dir::Left),
            "right" => Some(Dir::Right),
            "up" => Some(Dir::Up),
            "down" => Some(Dir::Down),
            _ => None,
        };
        if let Some(dir) = dir {
            let shown = self.shown(cx);
            let Some(scope) = shown.scope else {
                return false;
            };
            let layout = layout_for(&shown, scope);
            let from = self.model.read(cx).selected().cloned();
            let Some(n) = nav_target(&shown.forest, &layout, from.as_ref(), dir) else {
                return true;
            };
            if let Some(p) = layout.placed.iter().find(|p| p.node == n) {
                let (x, y) = pos(p.col, p.row);
                self.cam = camera::reveal(
                    self.cam,
                    (x, y, NODE_W, NODE_H),
                    self.g_content.get(),
                    self.g_view.get(),
                );
            }
            self.select(Some(shown.forest.nodes[n].key.clone()), cx);
            return true;
        }
        let (content, view) = (self.g_content.get(), self.g_view.get());
        match key {
            "0" | "f" => {
                self.cam = Cam::default();
                cx.notify();
            }
            "+" | "=" | "-" | "_" => {
                let factor = if matches!(key, "+" | "=") { 1.2 } else { 1.0 / 1.2 };
                self.cam = camera::zoom_at(
                    self.cam,
                    factor,
                    (view.0 / 2.0, view.1 / 2.0),
                    content,
                    view,
                );
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    /// Compact cards in columns. The camera follows the content (auto-fit) until the user zooms or
    /// pans; `0`, «Ajustar» or changing the view fits again.
    pub(crate) fn graph_body(
        &mut self,
        shown: &Shown,
        scope: Scope,
        selected: Option<NodeKey>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let forest = shown.forest.clone();
        let layout = layout_for(shown, scope);
        if layout.placed.is_empty() {
            return self.no_results(cx);
        }
        let content = content_size(&layout);
        self.g_content.set(content);
        let view = self.g_view.get();
        if view.0 > 0.0 && view.1 > 0.0 {
            if self.cam.fit {
                self.cam = camera::fit(content, view);
            } else {
                self.cam = camera::clamp_pan(self.cam, content, view);
            }
        }
        let cam = self.cam;
        let z = cam.zoom;
        // Status lines of the running nodes on screen.
        let live: HashMap<NodeKey, String> = {
            let m = self.model.read(cx);
            layout
                .placed
                .iter()
                .filter_map(|p| {
                    let k = &forest.nodes[p.node].key;
                    m.live_line(k).map(|l| (k.clone(), l.to_string()))
                })
                .collect()
        };
        let this = cx.entity();
        let now = shown.now;
        let at = |col: usize, row: f32| {
            let (x, y) = pos(col, row);
            (cam.pan.0 + x * z, cam.pan.1 + y * z)
        };
        // What is on screen (nothing measured yet: everything).
        let cull = view.0 > 0.0 && view.1 > 0.0;
        let visible = |x: f32, y: f32| {
            !cull
                || (x + NODE_W * z >= -NODE_W
                    && x <= view.0 + NODE_W
                    && y + NODE_H * z >= -NODE_H
                    && y <= view.1 + NODE_H)
        };

        let mut surface = div()
            .id("agentmap-graph")
            .relative()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .rounded(px(Radius::MD))
            .bg(c.surface_1)
            .track_focus(&self.graph_focus)
            .key_context("AgentMapGraph");

        // Pointer events: drag pans, the wheel zooms around the pointer.
        {
            let (drag, this, focus) = (self.g_drag.clone(), this.clone(), self.graph_focus.clone());
            let (origin, g_view, g_content) =
                (self.g_origin.clone(), self.g_view.clone(), self.g_content.clone());
            let this_wheel = this.clone();
            surface = surface
                .on_mouse_down(MouseButton::Left, {
                    let this = this.clone();
                    move |e: &MouseDownEvent, window, cx| {
                        window.focus(&focus);
                        let pan0 = this.read(cx).cam.pan;
                        drag.set(Some(GraphDrag {
                            start: (f32::from(e.position.x), f32::from(e.position.y)),
                            pan0,
                            moved: false,
                        }));
                    }
                })
                .on_scroll_wheel(move |e, _, cx| {
                    let delta = e.delta.pixel_delta(px(20.0));
                    let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                    let o = origin.get();
                    let (view, content) = (g_view.get(), g_content.get());
                    let anchor = (f32::from(e.position.x) - o.0, f32::from(e.position.y) - o.1);
                    this_wheel.update(cx, |v, cx| {
                        v.cam = if dx.abs() > dy.abs() {
                            camera::pan_by(v.cam, -dx, 0.0, content, view)
                        } else {
                            camera::zoom_at(v.cam, camera::wheel_factor(dy), anchor, content, view)
                        };
                        cx.notify();
                    });
                    cx.stop_propagation();
                });
            let this_key = this.clone();
            surface = surface.on_key_down(move |e, _, cx| {
                let key = e.keystroke.key.clone();
                if this_key.update(cx, |v, cx| v.graph_key(&key, cx)) {
                    cx.stop_propagation();
                }
            });
        }

        // Connectors (and the window-level listeners of the drag) first, under the cards.
        let edges: Vec<(f32, f32, f32, f32, bool, bool)> = layout
            .edges
            .iter()
            .map(|(p, ch)| {
                let (a, b) = (layout.placed[*p], layout.placed[*ch]);
                let (ax, ay) = at(a.col, a.row);
                let (bx, by) = at(b.col, b.row);
                let child = &forest.nodes[b.node];
                (
                    ax + NODE_W * z,
                    ay + NODE_H * z / 2.0,
                    bx,
                    by + NODE_H * z / 2.0,
                    child.status.is_active(),
                    child.is_external(),
                )
            })
            .collect();
        let (line, line_live, line_ext) = (c.outline, c.accent, c.outline_soft);
        let line_w = if z < 0.6 { 1.0 } else { LINE };
        let (drag, this_drag) = (self.g_drag.clone(), this.clone());
        let (g_view, g_origin) = (self.g_view.clone(), self.g_origin.clone());
        let g_content = self.g_content.clone();
        surface = surface.child(
            canvas(
                move |bounds: Bounds<gpui::Pixels>, window, _| {
                    let size = (
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    );
                    g_origin.set((f32::from(bounds.origin.x), f32::from(bounds.origin.y)));
                    let before = g_view.get();
                    g_view.set(size);
                    // the first measure (or a resize) fits again with the real size
                    if (before.0 - size.0).abs() > 0.5 || (before.1 - size.1).abs() > 0.5 {
                        window.refresh();
                    }
                },
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
                        window.paint_quad(fill(
                            rect(*x0, y0 - line_w / 2.0, mid - x0, line_w),
                            color,
                        ));
                        let (top, bottom) = (y0.min(*y1), y0.max(*y1));
                        window.paint_quad(fill(
                            rect(
                                mid - line_w / 2.0,
                                top - line_w / 2.0,
                                line_w,
                                bottom - top + line_w,
                            ),
                            color,
                        ));
                        window.paint_quad(fill(
                            rect(mid, y1 - line_w / 2.0, x1 - mid, line_w),
                            color,
                        ));
                    }
                    // Drag: the window sees the pointer even over a card.
                    let (d, t) = (drag.clone(), this_drag.clone());
                    let content = g_content.get();
                    let view = (
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    );
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        let Some(mut st) = d.get() else { return };
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        if e.pressed_button != Some(MouseButton::Left) {
                            d.set(None);
                            return;
                        }
                        let dx = f32::from(e.position.x) - st.start.0;
                        let dy = f32::from(e.position.y) - st.start.1;
                        if !st.moved && dx.abs() + dy.abs() < 4.0 {
                            return;
                        }
                        st.moved = true;
                        d.set(Some(st));
                        t.update(cx, |v, cx| {
                            v.cam = camera::pan_by(
                                Cam {
                                    pan: st.pan0,
                                    ..v.cam
                                },
                                dx,
                                dy,
                                content,
                                view,
                            );
                            cx.notify();
                        });
                    });
                    let d = drag.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, _, _, _| d.set(None));
                },
            )
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full(),
        );

        for p in &layout.placed {
            let (x, y) = at(p.col, p.row);
            if !visible(x, y) {
                continue;
            }
            let n = &forest.nodes[p.node];
            let sel = selected.as_ref() == Some(&n.key);
            let l = live.get(&n.key).map(String::as_str);
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .child(node_card(&theme, n, sel, z, now, &this, l)),
            );
        }

        // Zoom readout, «Ajustar» and the hint.
        let fit_btn = {
            let this = this.clone();
            pill_toggle("agentmap-fit", &theme, "Ajustar", cam.fit, move |_, cx| {
                this.update(cx, |v, cx| {
                    v.cam = Cam::default();
                    cx.notify();
                })
            })
        };
        surface
            .child(
                div()
                    .absolute()
                    .right(px(8.0))
                    .bottom(px(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_xs))
                            .text_color(c.muted)
                            .child(SharedString::from(format!("{:.0} %", z * 100.0))),
                    )
                    .child(fit_btn),
            )
            .child(
                div()
                    .absolute()
                    .left(px(10.0))
                    .bottom(px(10.0))
                    .text_size(px(ty.ui_size_xs))
                    .text_color(c.muted)
                    .child("Rueda: zoom · arrastrar: mover · flechas: navegar"),
            )
            .into_any_element()
    }

    /// One lane per agent, one bar per delegation over time.
    pub(crate) fn timeline_body(
        &mut self,
        shown: &Shown,
        scope: Scope,
        selected: Option<NodeKey>,
        width: f32,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let forest = shown.forest.clone();
        let now = shown.now;
        let (tl, rows) = timeline_lanes(&forest, scope, &shown.prefs, &shown.query());
        if tl.bars.is_empty() {
            if shown.query().is_active() {
                return self.no_results(cx);
            }
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

        let tl_rows = std::sync::Arc::new((tl, rows));
        let rows_for_list = tl_rows.clone();
        let list = VirtualList::new(
            "agentmap-timeline",
            tl_rows.1.len(),
            TL_ROW,
            move |range, _, _| {
                range
                    .filter_map(|i| rows_for_list.1.get(i).cloned())
                    .map(|row| {
                        let bar = match row {
                            LaneRow::Lane(g) => return group_row(&theme, &g, TL_ROW, &this),
                            LaneRow::Bar(b) => b,
                        };
                        let tl = &rows_for_list.0;
                        let node = &forest.nodes[bar.node];
                        let sel = selected.as_ref() == Some(&node.key);
                        let this = this.clone();
                        let key = node.key.clone();
                        let start = tl.frac(bar.start_ms);
                        let end = tl.frac(bar.end_ms);
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
                            .pl(px(22.0))
                            .pr(px(6.0))
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
                                    .w(px(label_w - 28.0))
                                    .flex_none()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(status_dot(&theme, node, 8.0))
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

/// Used by the tests: the graph's content size for a layout.
#[cfg(test)]
pub(crate) fn content_size_of(layout: &GraphLayout) -> (f32, f32) {
    content_size(layout)
}

#[allow(dead_code)]
fn _forest_marker(_: &Forest) {}
