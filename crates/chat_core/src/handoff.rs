//! Handoff message for switching agent or model mid-thread (B7).
//!
//! Deterministic and local: built from the stored messages only (no LLM call). The text is in
//! Spanish and has three facts: the last thing the user asked for, the files the previous agent
//! touched and the plan items still open.

use std::collections::HashSet;

use agents_driver::{AgentKind, TaskState};

use crate::model::{agent_label, Role};
use crate::state::{redact_text, touched_paths, truncate_chars, Message, Plan, ToolStatus};

/// Longest quoted user goal.
pub const MAX_GOAL_CHARS: usize = 800;
/// Most files listed.
pub const MAX_FILES: usize = 25;

pub struct HandoffInput<'a> {
    pub from: &'a AgentKind,
    pub to: &'a AgentKind,
    pub to_model: Option<&'a str>,
    pub messages: &'a [Message],
    pub plan: Option<&'a Plan>,
}

/// Files edited or written by the thread's tool calls (failed calls excluded), in order of first
/// touch, without duplicates.
pub fn files_touched(messages: &[Message]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for run in messages.iter().flat_map(Message::tool_runs) {
        if run.status == ToolStatus::Failed {
            continue;
        }
        for p in touched_paths(run) {
            if seen.insert(p.to_ascii_lowercase()) {
                out.push(p);
            }
        }
    }
    out
}

/// The most recent user message text.
pub fn last_user_goal(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.plain_text())
        .filter(|t| !t.trim().is_empty())
}

/// Build the handoff text, or `None` when the thread has no user message yet (nothing to hand
/// over).
pub fn build_handoff(input: &HandoffInput<'_>) -> Option<String> {
    let goal = last_user_goal(input.messages)?;
    let mut out = String::new();

    let mut to = agent_label(input.to);
    if let Some(m) = input.to_model.filter(|m| !m.is_empty()) {
        to.push_str(&format!(" · {m}"));
    }
    out.push_str(&format!(
        "Traspaso de hilo: hasta ahora trabajaba {}; ahora continúas tú ({to}).\n\n",
        agent_label(input.from)
    ));

    out.push_str("Último objetivo del usuario:\n");
    for line in truncate_chars(goal.trim(), MAX_GOAL_CHARS).lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }

    out.push_str("\nArchivos tocados hasta ahora:\n");
    let files = files_touched(input.messages);
    if files.is_empty() {
        out.push_str("- ninguno todavía\n");
    } else {
        for f in files.iter().take(MAX_FILES) {
            out.push_str(&format!("- {f}\n"));
        }
        if files.len() > MAX_FILES {
            out.push_str(&format!("- … y {} más\n", files.len() - MAX_FILES));
        }
    }

    out.push_str("\nPlan abierto:\n");
    let open: Vec<_> = input
        .plan
        .map(|p| p.open_items().collect())
        .unwrap_or_default();
    if open.is_empty() {
        out.push_str("- sin elementos abiertos\n");
    } else {
        for item in open {
            let tag = if item.state == TaskState::InProgress {
                "en curso"
            } else {
                "pendiente"
            };
            out.push_str(&format!("- [{tag}] {}\n", item.text));
        }
    }

    out.push_str(
        "\nRevisa el estado real del repositorio (git status y git diff) antes de seguir y continúa con el \
         objetivo sin repetir lo que ya está hecho.",
    );
    Some(redact_text(&out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Block, PlanItem, ToolClass, ToolRun};
    use agents_driver::ToolKind;
    use serde_json::json;

    fn msg(seq: u32, role: Role, blocks: Vec<Block>) -> Message {
        Message {
            seq,
            role,
            created_ms: 0,
            blocks,
            usage: None,
            cost: None,
            attachments: vec![],
            checkpoint: None,
            streaming: false,
        }
    }

    fn edit(id: &str, path: &str, status: ToolStatus) -> Block {
        Block::Tool(ToolRun {
            id: id.into(),
            kind: ToolKind::Edit,
            class: ToolClass::Edit,
            title: path.into(),
            input: json!({ "file_path": path }),
            status,
            summary: None,
            started_ms: 0,
            finished_ms: None,
        })
    }

    #[test]
    fn handoff_text_is_spanish_deterministic_and_complete() {
        let messages = vec![
            msg(
                0,
                Role::User,
                vec![Block::Text {
                    text: "Primero arregla el parser".into(),
                }],
            ),
            msg(
                1,
                Role::Assistant,
                vec![edit("a", "src/parser.rs", ToolStatus::Ok)],
            ),
            msg(
                2,
                Role::User,
                vec![Block::Text {
                    text: "Ahora añade tests\ny documenta".into(),
                }],
            ),
            msg(
                3,
                Role::Assistant,
                vec![
                    edit("b", "src/lib.rs", ToolStatus::Ok),
                    edit("c", "SRC/PARSER.rs", ToolStatus::Ok), // duplicate, other case
                    edit("d", "src/fail.rs", ToolStatus::Failed),
                ],
            ),
        ];
        let plan = Plan {
            items: vec![
                PlanItem {
                    text: "Escribir tests".into(),
                    state: TaskState::InProgress,
                },
                PlanItem {
                    text: "Publicar".into(),
                    state: TaskState::Pending,
                },
                PlanItem {
                    text: "Parser".into(),
                    state: TaskState::Done,
                },
            ],
        };
        let input = HandoffInput {
            from: &AgentKind::ClaudeCode,
            to: &AgentKind::Codex,
            to_model: Some("gpt-6"),
            messages: &messages,
            plan: Some(&plan),
        };
        let text = build_handoff(&input).unwrap();
        assert_eq!(text, build_handoff(&input).unwrap(), "deterministic");
        assert!(text.starts_with("Traspaso de hilo: hasta ahora trabajaba Claude Code; ahora continúas tú (Codex · gpt-6)."));
        assert!(text.contains("> Ahora añade tests\n> y documenta\n"));
        assert!(!text.contains("Primero arregla"), "only the last goal");
        assert!(text.contains("- src/parser.rs\n- src/lib.rs\n"));
        assert!(!text.contains("fail.rs"));
        assert_eq!(text.matches("parser.rs").count(), 1);
        assert!(text.contains("- [en curso] Escribir tests\n- [pendiente] Publicar\n"));
        assert!(!text.contains("- [pendiente] Parser") && !text.contains("] Parser"));
        assert!(text.contains("git status"));
    }

    #[test]
    fn handoff_without_files_or_plan_and_without_user_messages() {
        let messages = vec![msg(
            0,
            Role::User,
            vec![Block::Text {
                text: "hola".into(),
            }],
        )];
        let input = HandoffInput {
            from: &AgentKind::Codex,
            to: &AgentKind::OpenCode,
            to_model: None,
            messages: &messages,
            plan: None,
        };
        let t = build_handoff(&input).unwrap();
        assert!(t.contains("- ninguno todavía") && t.contains("- sin elementos abiertos"));
        let none = HandoffInput {
            messages: &[],
            ..input
        };
        assert!(build_handoff(&none).is_none());
    }
}
