//! What the composer pills offer: agents, models and permission modes. Pure.

use agents_driver::{AgentKind, ModelInfo, PermissionMode};
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

/// A model the agent accepts (`--model <id>`): what to pass and what to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

/// Claude Code's models: (full id passed to `--model`, name shown, alias the CLI also accepts).
/// `claude --help`: "--model: an alias for the latest model (e.g. 'fable', 'opus', 'sonnet') or a
/// model's full name". We pass the full id and show the versioned name.
const CLAUDE_MODELS: [(&str, &str, &str); 4] = [
    ("claude-opus-5-5", "Opus 5.5", "opus"),
    ("claude-sonnet-5-5", "Sonnet 5.5", "sonnet"),
    ("claude-haiku-5-5", "Haiku 5.5", "haiku"),
    ("claude-fable-5-2", "Fable 5.2", "fable"),
];

/// Models listed for an agent: Claude's fixed catalogue; for every other agent what it told us
/// (`catalog`: Codex `model/list`, ACP `session/new`, see `agents_driver::probe_models`). Empty
/// until the agent answered: the picker then offers the default and a free-text field.
pub fn models_for(kind: &AgentKind, catalog: &[ModelInfo]) -> Vec<ModelOption> {
    match kind {
        AgentKind::ClaudeCode => CLAUDE_MODELS
            .iter()
            .map(|(id, label, _)| ModelOption { id: (*id).to_string(), label: (*label).to_string() })
            .collect(),
        _ => catalog.iter().map(|m| ModelOption { id: m.id.clone(), label: m.label.clone() }).collect(),
    }
}

/// `claude-opus-5-5` -> `Opus 5.5`; the alias `opus` -> `Opus 5.5` (the catalogue's latest);
/// `claude-opus-4-5-20251101` -> `Opus 4.5`; `claude-3-5-sonnet-20241022` -> `Sonnet 3.5`;
/// anything else is returned as is.
pub fn pretty_model(id: &str) -> String {
    let id = id.trim();
    let lower = id.to_ascii_lowercase();
    let bare = lower.strip_prefix("claude-").unwrap_or(&lower);
    let bare = bare.split('[').next().unwrap_or(bare);
    if let Some((_, label, _)) = CLAUDE_MODELS.iter().find(|(full, _, alias)| *full == lower.split('[').next().unwrap_or(&lower) || *alias == bare) {
        return (*label).to_string();
    }
    let mut family = None;
    let mut nums: Vec<&str> = Vec::new();
    for part in bare.split('-') {
        match part {
            "opus" | "sonnet" | "haiku" | "fable" => family = Some(part),
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

/// Effort levels in menu order: (value stored and passed to the agent, label).
pub const EFFORTS: [(&str, &str); 5] =
    [("low", "Bajo"), ("medium", "Medio"), ("high", "Alto"), ("xhigh", "Muy alto"), ("max", "Máx")];

/// Label of an effort value (`high` -> `Alto`); unknown values are shown as they are.
pub fn effort_label(effort: &str) -> String {
    EFFORTS.iter().find(|(v, _)| *v == effort).map_or_else(|| effort.to_string(), |(_, l)| (*l).to_string())
}

/// The efforts to offer for `kind` + `model`; **empty = the agent has no such knob** (the pill is
/// hidden). Claude: all five. Codex: what the model lists (its `xhigh` is our "Muy alto"; "Máx"
/// is Claude's / Antigravity's), else low..xhigh. Antigravity (`agy`): all five. OpenCode and other
/// ACP agents: none.
pub fn efforts_for(kind: &AgentKind, model: Option<&str>, catalog: &[ModelInfo]) -> Vec<(&'static str, &'static str)> {
    if !agents_driver::supports_effort(kind) {
        return Vec::new();
    }
    if *kind == AgentKind::Codex {
        let listed: Vec<&str> = model
            .and_then(|m| catalog.iter().find(|c| c.id == m))
            .map(|m| m.efforts.iter().map(String::as_str).collect())
            .unwrap_or_default();
        return EFFORTS
            .iter()
            .copied()
            .filter(|(v, _)| *v != "max" && (listed.is_empty() || listed.contains(v)))
            .collect();
    }
    EFFORTS.to_vec()
}

/// `Opus 5.5 · Alto` (the model alone without an effort).
pub fn model_effort_label(model: Option<&str>, effort: Option<&str>) -> Option<String> {
    let m = model.filter(|m| !m.trim().is_empty()).map(pretty_model);
    let e = effort.filter(|e| !e.is_empty()).map(effort_label);
    match (m, e) {
        (Some(m), Some(e)) => Some(format!("{m} · {e}")),
        (Some(m), None) => Some(m),
        (None, Some(e)) => Some(e),
        (None, None) => None,
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
        assert_eq!(pretty_model("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(pretty_model("claude-fable-5-2"), "Fable 5.2");
        assert_eq!(pretty_model("opus"), "Opus 5.5", "aliases show the version they resolve to");
        assert_eq!(pretty_model("opus[1m]"), "Opus 5.5");
        assert_eq!(pretty_model("claude-haiku-5-5-20260101"), "Haiku 5.5");
        assert_eq!(pretty_model("gpt-5-codex"), "gpt-5-codex");
        assert_eq!(model_label(None), "Modelo");
        assert_eq!(model_label(Some("sonnet")), "Sonnet 5.5");
    }

    #[test]
    fn agents_and_permissions() {
        assert_eq!(agent_mark(&AgentKind::ClaudeCode), AgentMark::Claude);
        assert_eq!(agent_mark(&AgentKind::Acp { command: "\"C:/x/gemini.exe\" --acp".into() }), AgentMark::Gemini);
        assert_eq!(default_agents().len(), 3);
        let claude = models_for(&AgentKind::ClaudeCode, &[]);
        assert_eq!(claude.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(), ["Opus 5.5", "Sonnet 5.5", "Haiku 5.5", "Fable 5.2"]);
        assert_eq!(claude[0].id, "claude-opus-5-5");
        assert!(models_for(&AgentKind::Codex, &[]).is_empty());
        let cat = [ModelInfo { id: "gpt-5.6".into(), label: "GPT-5.6".into(), efforts: vec!["low".into(), "high".into()], default_effort: None }];
        assert_eq!(models_for(&AgentKind::Codex, &cat)[0].label, "GPT-5.6");
        assert!(is_dangerous(PermissionMode::Full));
        assert!(permission_hint(PermissionMode::Full).contains("dangerously"));
        assert_eq!(PERMISSION_ORDER.len(), 4);
        assert_eq!(permission_label(PermissionMode::AcceptEdits), "Aceptar ediciones");
    }

    #[test]
    fn effort_pill_only_where_the_agent_has_the_knob() {
        let all = |k: &AgentKind| efforts_for(k, None, &[]).iter().map(|(v, _)| *v).collect::<Vec<_>>();
        assert_eq!(all(&AgentKind::ClaudeCode), ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(all(&AgentKind::Codex), ["low", "medium", "high", "xhigh"]);
        assert!(all(&AgentKind::OpenCode).is_empty());
        assert!(all(&AgentKind::Acp { command: "gemini --acp".into() }).is_empty());
        assert_eq!(all(&AgentKind::Acp { command: "agy acp".into() }).len(), 5);
        // Codex narrows to what the chosen model lists.
        let cat = [ModelInfo { id: "m".into(), label: "M".into(), efforts: vec!["low".into(), "high".into()], default_effort: None }];
        let v: Vec<_> = efforts_for(&AgentKind::Codex, Some("m"), &cat).iter().map(|(v, _)| *v).collect();
        assert_eq!(v, ["low", "high"]);
    }

    #[test]
    fn model_and_effort_read_as_one_label() {
        assert_eq!(model_effort_label(Some("claude-opus-5-5"), Some("high")).as_deref(), Some("Opus 5.5 · Alto"));
        assert_eq!(model_effort_label(Some("claude-opus-5-5"), None).as_deref(), Some("Opus 5.5"));
        assert_eq!(model_effort_label(None, None), None);
        assert_eq!(effort_label("xhigh"), "Muy alto");
        assert_eq!(effort_label("max"), "Máx");
    }
}
