//! How a tab's leading mark is painted: the kind icon, or the mark of the agent that runs in it
//! (`TabIcon::Agent("claude-code")` -> the Claude mark, unknown agents -> the generic one).

use gpui::{Hsla, Svg};
use panel_api::TabIcon;
use ui_kit::{AgentMark, agent_icon, icon};

/// The mark of `tab_icon` at `size` px.
pub fn lead_icon(tab_icon: &TabIcon, size: f32, color: impl Into<Hsla>) -> Svg {
    match tab_icon {
        TabIcon::Kind(i) => icon(*i, size, color),
        TabIcon::Agent(name) => agent_icon(AgentMark::from_name(name), size, color),
    }
}

/// `Some(mark)` when the tab runs an agent.
pub fn agent_of(tab_icon: &TabIcon) -> Option<AgentMark> {
    match tab_icon {
        TabIcon::Agent(name) => Some(AgentMark::from_name(name)),
        TabIcon::Kind(_) => None,
    }
}
