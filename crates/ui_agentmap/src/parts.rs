//! Small pieces shared by the tree, the graph, the timeline and the inspector.

use gpui::{AnyElement, IntoElement, ParentElement, SharedString, Styled, div, px};
use theme::{Radius, Rgba, Theme};
use ui_kit::{Icon, StatusGlyph, icon};

use crate::fmt;
use crate::tree::{MapNode, NodeStatus};

/// Color that carries a status (glyphs, timeline bars).
pub fn status_color(theme: &Theme, status: NodeStatus) -> Rgba {
    let c = theme.colors;
    match status {
        NodeStatus::Running => c.accent,
        NodeStatus::Waiting => c.warning,
        NodeStatus::Done => c.success,
        NodeStatus::Failed | NodeStatus::TimedOut | NodeStatus::OverBudget | NodeStatus::Refused => {
            c.error
        }
        NodeStatus::Cancelled | NodeStatus::Idle => c.muted,
    }
}

/// The status mark of a row: the kit glyph, or the status word for states without a glyph
/// (cancelled).
pub fn status_mark(theme: &Theme, status: NodeStatus) -> Option<AnyElement> {
    let c = theme.colors;
    let ty = theme.typography;
    if let Some(g) = status.glyph() {
        return Some(StatusGlyph::new(theme, g).into_any_element());
    }
    (status == NodeStatus::Cancelled).then(|| {
        div()
            .flex_none()
            .text_size(px(ty.ui_size_xs))
            .text_color(c.muted)
            .child(status.label())
            .into_any_element()
    })
}

/// The leading mark of a node: the agent's, or the terminal icon for a terminal tab.
pub fn lead_mark(theme: &Theme, node: &MapNode, size: f32) -> AnyElement {
    let c = theme.colors;
    let color = if node.is_root() { c.text } else { c.muted };
    match &node.key {
        crate::tree::NodeKey::Tab(_) => icon(Icon::Terminal, size, color).into_any_element(),
        _ => ui_kit::agent_icon(node.mark, size, color).into_any_element(),
    }
}

/// `worktree` chip: folder icon and the folder name.
pub fn worktree_pill(theme: &Theme, path: &std::path::Path) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex_none()
        .max_w(px(132.0))
        .h(px(20.0))
        .px(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .rounded(px(Radius::CHIP))
        .bg(c.surface_3)
        .text_size(px(ty.ui_size_xs))
        .text_color(c.muted)
        .child(icon(Icon::Folder, 12.0, c.muted))
        .child(
            div()
                .min_w(px(0.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(SharedString::from(fmt::leaf(path))),
        )
        .into_any_element()
}

/// A rounded text chip (`launcher`, `perfil`).
pub fn chip(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex_none()
        .h(px(20.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .rounded(px(Radius::CHIP))
        .bg(c.surface_3)
        .text_size(px(ty.ui_size_xs))
        .text_color(c.text)
        .child(text.into())
        .into_any_element()
}

/// `45 k tokens`-style metrics of a node in display order: duration, tokens, cost, files.
pub fn metrics(node: &MapNode, now_ms: i64) -> [Option<String>; 4] {
    [
        node.duration_ms(now_ms).map(fmt::duration_ms),
        node.tokens.filter(|t| *t > 0).map(fmt::tokens),
        node.cost_usd.filter(|c| *c > 0.0).map(fmt::cost),
        (node.files > 0).then(|| fmt::files(node.files)),
    ]
}
