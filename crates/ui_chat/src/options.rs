//! What the composer pills offer: agents, models and permission modes. Pure.

use agents_driver::{AgentKind, PermissionMode};
use gpui::SharedString;
use ui_kit::AgentMark;

/// An agent the user can start or switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentOption {
    pub kind: AgentKind,
    pub label: SharedString,
    /// The CLI was found on this machine (greyed out and not selectable otherwise).
    pub available: bool,
    pub version: Option<String>,
}

impl AgentOption {
    pub fn new(kind: AgentKind) -> Self {
        Self { label: chat_core::agent_label(&kind).into(), kind, available: true, version: None }
    }

    pub fn mark(&self) -> AgentMark {
        agent_mark(&self.kind)
    }
}

/// Agents offered before detection finishes (or when the host gives none).
pub fn default_agents() -> Vec<AgentOption> {
    vec![AgentOption::new(AgentKind::ClaudeCode), AgentOption::new(AgentKind::Codex), AgentOption::new(AgentKind::OpenCode)]
}

/// The mark of an agent kind.
pub fn agent_mark(kind: &AgentKind) -> AgentMark {
    match kind {
        AgentKind::ClaudeCode => AgentMark::Claude,
        AgentKind::Codex => AgentMark::Codex,
        AgentKind::OpenCode => AgentMark::OpenCode,
        AgentKind::Acp { command } => AgentMark::from_name(command),
    }
}

/// Name of the agent for the `TabIcon::Agent` and `group_by = agent`.
pub fn agent_name(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude",
        AgentKind::Codex => "codex",
        AgentKind::OpenCode => "opencode",
        AgentKind::Acp { .. } => "acp",
    }
}

/// A model the agent is known to accept (`--model <id>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub id: &'static str,
    pub label: &'static str,
}

const CLAUDE_MODELS: &[ModelOption] = &[
    ModelOption { id: "opus", label: "Opus" },
    ModelOption { id: "sonnet", label: "Sonnet" },
    ModelOption { id: "haiku", label: "Haiku" },
];

/// Models listed for an agent. Only Claude has stable aliases; the others take free text (the
/// handshake of the agent is the real source: see the report, `agents_driver` does not expose it).
pub fn models_for(kind: &AgentKind) -> &'static [ModelOption] {
    match kind {
        AgentKind::ClaudeCode => CLAUDE_MODELS,
        _ => &[],
    }
}

/// `claude-opus-4-5-20251101` -> `Opus 4.5`; `claude-3-5-sonnet-20241022` -> `Sonnet 3.5`;
/// aliases are capitalized; anything else is returned as is.
pub fn pretty_model(id: &str) -> String {
    let id = id.trim();
    let lower = id.to_ascii_lowercase();
    let bare = lower.strip_prefix("claude-").unwrap_or(&lower);
    let bare = bare.split('[').next().unwrap_or(bare);
    let mut family = None;
    let mut nums: Vec<&str> = Vec::new();
    for part in bare.split('-') {
        match part {
            "opus" | "sonnet" | "haiku" => family = Some(part),
            p if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() && p.len() < 6 => nums.push(p),
            _ => {}
        }
    }
    match family {
        Some(f) => {
            let mut name = f[..1].to_ascii_uppercase() + &f[1..];
            if !nums.is_empty() {
                name.push(' ');
                name.push_str(&nums.join("."));
            }
            name
        }
        None => id.to_string(),
    }
}

/// Label of a model slot (`None` = the agent's default).
pub fn model_label(model: Option<&str>) -> String {
    match model {
        Some(m) if !m.trim().is_empty() => pretty_model(m),
        _ => "Modelo".to_string(),
    }
}

/// The permission pills in menu order.
pub const PERMISSION_ORDER: [PermissionMode; 4] =
    [PermissionMode::Ask, PermissionMode::AcceptEdits, PermissionMode::ReadOnly, PermissionMode::Full];

pub fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Preguntar",
        PermissionMode::AcceptEdits => "Aceptar ediciones",
        PermissionMode::ReadOnly => "Solo lectura",
        PermissionMode::Full => "Completo",
    }
}

/// One line for the menu; `Full` carries the warning.
pub fn permission_hint(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Pide permiso antes de editar o ejecutar.",
        PermissionMode::AcceptEdits => "Edita archivos sin preguntar; pregunta para ejecutar.",
        PermissionMode::ReadOnly => "Solo lee y planifica; no edita ni ejecuta.",
        PermissionMode::Full => {
            "Cuidado: ejecuta y edita todo sin pedir permiso (equivale a --dangerously-skip-permissions). Úsalo solo en repos de confianza."
        }
    }
}

pub fn is_dangerous(mode: PermissionMode) -> bool {
    mode == PermissionMode::Full
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_are_prettified() {
        assert_eq!(pretty_model("claude-opus-4-5-20251101"), "Opus 4.5");
        assert_eq!(pretty_model("claude-3-5-sonnet-20241022"), "Sonnet 3.5");
        assert_eq!(pretty_model("opus"), "Opus");
        assert_eq!(pretty_model("opus[1m]"), "Opus");
        assert_eq!(pretty_model("gpt-5-codex"), "gpt-5-codex");
        assert_eq!(model_label(None), "Modelo");
        assert_eq!(model_label(Some("sonnet")), "Sonnet");
    }

    #[test]
    fn agents_and_permissions() {
        assert_eq!(agent_mark(&AgentKind::ClaudeCode), AgentMark::Claude);
        assert_eq!(agent_mark(&AgentKind::Acp { command: "\"C:/x/gemini.exe\" --acp".into() }), AgentMark::Gemini);
        assert_eq!(default_agents().len(), 3);
        assert!(models_for(&AgentKind::ClaudeCode).len() >= 3);
        assert!(models_for(&AgentKind::Codex).is_empty());
        assert!(is_dangerous(PermissionMode::Full));
        assert!(permission_hint(PermissionMode::Full).contains("dangerously"));
        assert_eq!(PERMISSION_ORDER.len(), 4);
        assert_eq!(permission_label(PermissionMode::AcceptEdits), "Aceptar ediciones");
    }
}
