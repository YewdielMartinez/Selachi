//! Per-agent marks (`assets/icons/agents/<name>.svg`): 16 x 16 pixel grids tinted through
//! `text_color`, like the rest of the icon set. Unknown agents fall back to [`Icon::Agents`].

use gpui::{Hsla, Svg, prelude::*, px, svg};

use crate::icon::{Icon, icon};

/// The agents Selachi knows a mark for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMark {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Cursor,
    Copilot,
    Windsurf,
    /// Any other agent: the generic [`Icon::Agents`] mark.
    Unknown,
}

impl AgentMark {
    /// Every mark that has its own file.
    pub const ALL: [AgentMark; 7] = [
        AgentMark::Claude,
        AgentMark::Codex,
        AgentMark::Gemini,
        AgentMark::OpenCode,
        AgentMark::Cursor,
        AgentMark::Copilot,
        AgentMark::Windsurf,
    ];

    /// Case-insensitive match on an agent's name or command: `"claude-code"` and
    /// `"Claude Code"` are Claude, `"opencode"` is OpenCode, `"github-copilot"` is Copilot...
    pub fn from_name(name: &str) -> Self {
        let n = name.trim().to_lowercase();
        let has = |s: &str| n.contains(s);
        if has("claude") {
            Self::Claude
        } else if has("codex") {
            Self::Codex
        } else if has("gemini") {
            Self::Gemini
        } else if has("opencode") || has("open-code") || has("open code") {
            Self::OpenCode
        } else if has("cursor") {
            Self::Cursor
        } else if has("copilot") {
            Self::Copilot
        } else if has("windsurf") {
            Self::Windsurf
        } else {
            Self::Unknown
        }
    }

    /// File stem of the svg, `None` for [`AgentMark::Unknown`].
    pub const fn name(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Gemini => Some("gemini"),
            Self::OpenCode => Some("opencode"),
            Self::Cursor => Some("cursor"),
            Self::Copilot => Some("copilot"),
            Self::Windsurf => Some("windsurf"),
            Self::Unknown => None,
        }
    }

    /// Asset path served by [`crate::Assets`] (the generic agents icon for `Unknown`).
    pub fn path(self) -> String {
        match self.name() {
            Some(n) => format!("icons/agents/{n}.svg"),
            None => Icon::Agents.path(),
        }
    }
}

/// The mark of `mark` drawn at `size` logical px in `color`.
pub fn agent_icon(mark: AgentMark, size: f32, color: impl Into<Hsla>) -> Svg {
    match mark.name() {
        Some(_) => svg().path(mark.path()).flex_none().size(px(size)).text_color(color),
        None => icon(Icon::Agents, size, color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_map_to_marks() {
        assert_eq!(AgentMark::from_name("claude-code"), AgentMark::Claude);
        assert_eq!(AgentMark::from_name("Claude"), AgentMark::Claude);
        assert_eq!(AgentMark::from_name("opencode"), AgentMark::OpenCode);
        assert_eq!(AgentMark::from_name("OpenCode"), AgentMark::OpenCode);
        assert_eq!(AgentMark::from_name("codex"), AgentMark::Codex);
        assert_eq!(AgentMark::from_name("gemini-cli"), AgentMark::Gemini);
        assert_eq!(AgentMark::from_name("cursor-agent"), AgentMark::Cursor);
        assert_eq!(AgentMark::from_name("GitHub Copilot"), AgentMark::Copilot);
        assert_eq!(AgentMark::from_name("windsurf"), AgentMark::Windsurf);
        assert_eq!(AgentMark::from_name("aider"), AgentMark::Unknown);
        assert_eq!(AgentMark::from_name(""), AgentMark::Unknown);
    }

    #[test]
    fn unknown_uses_the_generic_icon() {
        assert_eq!(AgentMark::Unknown.path(), Icon::Agents.path());
        assert_eq!(AgentMark::Claude.path(), "icons/agents/claude.svg");
    }
}
