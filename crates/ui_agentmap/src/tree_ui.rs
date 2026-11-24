//! The compact vertical tree: one virtualized row per visible node.

use std::sync::Arc;

use gpui::{
    AnyElement, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use theme::{Radius, Theme};
use ui_kit::{Icon, VirtualList, icon};

use crate::fmt;
use crate::parts::{lead_mark, metrics, status_mark, worktree_pill};
use crate::tree::{Forest, NodeKey, Row, Totals};
use crate::view::{AgentMapView, RowLayout};

/// Indent per nesting level.
const INDENT: f32 = 16.0;

pub(crate) fn node_id_string(prefix: &str, key: &NodeKey) -> String {
    let tail = match key {
        NodeKey::Thread(t) => t.0.to_string(),
        NodeKey::Tab(s) => format!("tab-{s}"),
    };
    format!("{prefix}-{tail}")
}

pub(crate) fn node_element_id(prefix: &str, key: &NodeKey) -> ElementId {
    ElementId::Name(SharedString::from(node_id_string(prefix, key)))
}

#[allow(clippy::too_many_arguments)]
fn tree_row(
    theme: &Theme,
    forest: &Forest,
    row: Row,
    selected: bool,
    layout: RowLayout,
    root_totals: Totals,
    this: &Entity<AgentMapView>,
    now_ms: i64,
) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    let node = &forest.nodes[row.node];
    let key = node.key.clone();
    let is_root = row.depth == 0;

    let mut m = metrics(node, now_ms);
    if is_root {
        // The orchestrator shows what its whole session cost.
        m[0] = None;
        m[1] = (root_totals.tokens > 0).then(|| fmt::tokens(root_totals.tokens));
        m[2] = (root_totals.cost_usd > 0.0).then(|| fmt::cost(root_totals.cost_usd));
        m[3] = None;
    }
    let model = fmt::model_effort(node.model.as_deref(), node.effort.as_deref());

    let chevron: AnyElement = if row.has_children {
        let this = this.clone();
        let k = key.clone();
        div()
            .id(node_element_id("agentmap-toggle", &key))
            .flex_none()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(Radius::CHIP))
            .cursor_pointer()
            .hover(|s| s.bg(c.surface_3))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                this.update(cx, |v, cx| v.toggle_collapsed(k.clone(), cx));
            })
            .child(icon(
                if row.collapsed {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                },
                16.0,
                c.muted,
            ))
            .into_any_element()
    } else {
        div().flex_none().size(px(18.0)).into_any_element()
    };

    let title = div()
        .min_w(px(0.0))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_size(px(ty.ui_size_md))
        .font_weight(if is_root {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .text_color(c.text)
        .child(SharedString::from(node.title.clone()));

    let mut body = div().flex_1().min_w(px(0.0)).flex().flex_col();
    match layout {
        RowLayout::Compact => {
            let sub: Vec<String> = model
                .iter()
                .cloned()
                .chain(m.iter().take(3).flatten().cloned())
                .collect();
            body = body.child(title).child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(ty.ui_size_xs))
                    .text_color(c.muted)
                    .child(SharedString::from(sub.join(" · "))),
            );
        }
        _ => body = body.child(title),
    }

    let cell = |text: Option<String>, w: f32| {
        div()
            .flex_none()
            .w(px(w))
            .flex()
            .justify_end()
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .when_some(text, |d, t| d.child(SharedString::from(t)))
    };

    let this_click = this.clone();
    let click_key = key.clone();
    let mut el = div()
        .id(node_element_id("agentmap-row", &key))
        .debug_selector({
            let s = node_id_string("agentmap-row", &key);
            move || s
        })
        .occlude()
        .w_full()
        .h(px(layout.height() - 4.0))
        .my(px(2.0))
        .pl(px(8.0 + INDENT * row.depth as f32))
        .pr(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .rounded(px(Radius::PILL))
        .cursor_pointer()
        .when(selected, |d| d.bg(c.surface_3))
        .when(!selected, |d| d.hover(|s| s.bg(c.surface_2)))
        .on_click(move |_, _, cx| {
            let k = click_key.clone();
            this_click.update(cx, |v, cx| v.select(Some(k), cx));
        })
        .child(chevron)
        .child(lead_mark(theme, node, 16.0))
        .child(body);

    if layout != RowLayout::Compact {
        el = el.child(
            div()
                .flex_none()
                .w(px(if layout == RowLayout::Full { 168.0 } else { 128.0 }))
                .min_w(px(0.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .when_some(model, |d, t| d.child(SharedString::from(t))),
        );
    }
    el = el.when_some(status_mark(theme, node.status), |d, s| d.child(s));
    if layout != RowLayout::Compact {
        el = el
            .child(cell(m[0].clone(), 58.0))
            .child(cell(m[1].clone(), 48.0))
            .child(cell(m[2].clone(), 52.0));
    }
    if layout == RowLayout::Full {
        el = el.child(cell(m[3].clone(), 72.0)).child(
            div()
                .flex_none()
                .w(px(132.0))
                .flex()
                .justify_end()
                .when_some(node.worktree.as_ref(), |d, p| d.child(worktree_pill(theme, p))),
        );
    }
    el.into_any_element()
}

impl AgentMapView {
    /// The virtualized rows of the tree.
    pub(crate) fn tree_body(
        &mut self,
        forest: Arc<Forest>,
        rows: Vec<Row>,
        selected: Option<NodeKey>,
        width: f32,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let layout = RowLayout::from_width(width);
        let this = cx.entity();
        let totals = rows
            .first()
            .map(|r| forest.totals(r.node))
            .unwrap_or_default();
        let now = store::now_ms();
        let rows = Arc::new(rows);
        let count = rows.len();
        VirtualList::new("agentmap-tree", count, layout.height(), move |range, _, _| {
            range
                .filter_map(|i| rows.get(i).copied())
                .map(|row| {
                    let sel = selected.as_ref() == Some(&forest.nodes[row.node].key);
                    tree_row(&theme, &forest, row, sel, layout, totals, &this, now)
                })
                .collect()
        })
        .track_scroll(self.tree_scroll.clone())
        .flex_1()
        .into_any_element()
    }
}
