//! Copying text out of the chat (PLAN 21.1): what each "Copiar" produces, the redaction policy
//! ("Ocultar secretos al copiar") and the hint shown when redaction removed something. Pure: no
//! gpui, so every rule is unit-tested.

use chat_core::{Block, Message, Role, ToolClass};
use viewer_svc::{MdBlock, MdInline, MdListItem};

use crate::md::flatten;
use crate::rows::command_text;

/// Which setting decides whether a copy is redacted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// Output of a command (and everything a tool printed): sensitive by default.
    Command,
    /// What the user and the assistant wrote: not redacted by default.
    Text,
}

/// The "Ocultar secretos al copiar" toggles.
///
/// TODO(integrator): persist these in `store::Config` (e.g. `[chat] redact_copy_commands` /
/// `redact_copy_text`) and push them with `ChatHub::set_copy_settings` on load and on hot-reload.
/// The store has no such field yet, so they live in the hub for the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopySettings {
    /// Redact when copying command output (default on).
    pub redact_commands: bool,
    /// Redact when copying assistant / user text (default off).
    pub redact_text: bool,
}

impl Default for CopySettings {
    fn default() -> Self {
        Self {
            redact_commands: true,
            redact_text: false,
        }
    }
}

impl CopySettings {
    pub fn redacts(&self, kind: CopyKind) -> bool {
        match kind {
            CopyKind::Command => self.redact_commands,
            CopyKind::Text => self.redact_text,
        }
    }

    pub fn toggled(mut self, kind: CopyKind) -> Self {
        match kind {
            CopyKind::Command => self.redact_commands = !self.redact_commands,
            CopyKind::Text => self.redact_text = !self.redact_text,
        }
        self
    }
}

/// Text ready for the clipboard plus how many secrets were hidden in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub text: String,
    pub hidden: usize,
}

/// Applies the redaction policy of `kind` to `text`.
pub fn prepare(text: &str, kind: CopyKind, settings: &CopySettings) -> Prepared {
    if !settings.redacts(kind) {
        return Prepared {
            text: text.to_string(),
            hidden: 0,
        };
    }
    let r = store::redact(text);
    Prepared {
        hidden: r.count,
        text: r.text,
    }
}

/// `"incluye 2 secretos ocultos"` (singular for one).
pub fn hidden_hint(hidden: usize) -> String {
    if hidden == 1 {
        "incluye 1 secreto oculto".to_string()
    } else {
        format!("incluye {hidden} secretos ocultos")
    }
}

/// The toast text after a copy: `"Copiado"` or `"Copiado · incluye N secretos ocultos"`.
pub fn copied_label(hidden: usize) -> String {
    if hidden == 0 {
        "Copiado".to_string()
    } else {
        format!("Copiado · {}", hidden_hint(hidden))
    }
}

// ---------------------------------------------------------------------------------------------
// Markdown to plain text
// ---------------------------------------------------------------------------------------------

fn inline_text(inlines: &[MdInline]) -> String {
    flatten(inlines).text
}

fn plain_list(items: &[MdListItem], ordered: bool, start: u64, depth: usize, out: &mut Vec<String>) {
    for (i, item) in items.iter().enumerate() {
        let indent = "  ".repeat(depth);
        let marker = match item.checked {
            Some(true) => "[x]".to_string(),
            Some(false) => "[ ]".to_string(),
            None if ordered => format!("{}.", start + i as u64),
            None => "-".to_string(),
        };
        let mut first = true;
        for b in &item.blocks {
            match b {
                MdBlock::List {
                    ordered,
                    start,
                    items,
                } => plain_list(items, *ordered, *start, depth + 1, out),
                other => {
                    let text = plain_block(other);
                    for line in text.lines() {
                        if first {
                            out.push(format!("{indent}{marker} {line}"));
                            first = false;
                        } else {
                            out.push(format!("{indent}  {line}"));
                        }
                    }
                }
            }
        }
        if first {
            out.push(format!("{indent}{marker}"));
        }
    }
}

fn plain_block(b: &MdBlock) -> String {
    match b {
        MdBlock::Heading { inlines, .. } | MdBlock::Paragraph(inlines) => inline_text(inlines),
        MdBlock::CodeBlock { text, .. } => text.trim_end_matches('\n').to_string(),
        MdBlock::List {
            ordered,
            start,
            items,
        } => {
            let mut lines = Vec::new();
            plain_list(items, *ordered, *start, 0, &mut lines);
            lines.join("\n")
        }
        MdBlock::Quote(inner) => plain_blocks(inner),
        MdBlock::Rule => String::new(),
        MdBlock::Table(t) => {
            let row = |cells: &[Vec<MdInline>]| {
                cells
                    .iter()
                    .map(|c| inline_text(c))
                    .collect::<Vec<_>>()
                    .join("\t")
            };
            let mut lines = vec![row(&t.header)];
            lines.extend(t.rows.iter().map(|r| row(r)));
            lines.join("\n")
        }
        MdBlock::Image { alt, .. } => format!("[imagen: {alt}]"),
        MdBlock::Html(h) => h.trim_end().to_string(),
    }
}

fn plain_blocks(blocks: &[MdBlock]) -> String {
    blocks
        .iter()
        .map(plain_block)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The message as plain text: no Markdown syntax, no code fences (what "Copiar como texto
/// plano" gives). Lists keep their bullets and numbers, table cells are tab separated.
pub fn markdown_to_plain(md: &str) -> String {
    plain_blocks(&viewer_svc::markdown_blocks(md))
}

// ---------------------------------------------------------------------------------------------
// Whole conversation
// ---------------------------------------------------------------------------------------------

/// Heading of a role in the copied conversation.
pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "Usuario",
        Role::Assistant => "Asistente",
        Role::System => "Sistema",
    }
}

fn fence_for(body: &str) -> String {
    // A fence longer than any backtick run inside, so the block survives pasting.
    let longest = body
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
        .max(2);
    "`".repeat(longest + 1)
}

fn fenced(body: &str) -> String {
    let f = fence_for(body);
    format!("{f}\n{}\n{f}", body.trim_end_matches('\n'))
}

/// The conversation as Markdown, with a heading per role. Text blocks are copied as written;
/// commands appear as a fenced `$ command` block with their output. Redaction follows
/// `settings` (text and command output separately); the count of hidden secrets is returned.
pub fn conversation_markdown(messages: &[Message], settings: &CopySettings) -> Prepared {
    let mut hidden = 0;
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        let mut body: Vec<String> = Vec::new();
        for b in &m.blocks {
            match b {
                Block::Text { text } if !text.trim().is_empty() => {
                    let p = prepare(text.trim_end(), CopyKind::Text, settings);
                    hidden += p.hidden;
                    body.push(p.text);
                }
                Block::Command(run) => {
                    let mut block = format!("$ {}", command_text(run));
                    if let Some(out) = run.summary.as_deref().filter(|s| !s.trim().is_empty()) {
                        block.push('\n');
                        block.push_str(out.trim_end());
                    }
                    let p = prepare(&block, CopyKind::Command, settings);
                    hidden += p.hidden;
                    body.push(fenced(&p.text));
                }
                Block::Tool(run) if run.class == ToolClass::Edit => {
                    body.push(format!("_{}_", run.title));
                }
                _ => {}
            }
        }
        if m.role == Role::User && body.is_empty() {
            let t = m.plain_text();
            if !t.trim().is_empty() {
                let p = prepare(t.trim_end(), CopyKind::Text, settings);
                hidden += p.hidden;
                body.push(p.text);
            }
        }
        if body.is_empty() {
            continue;
        }
        parts.push(format!("## {}\n\n{}", role_label(m.role), body.join("\n\n")));
    }
    Prepared {
        text: parts.join("\n\n"),
        hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    #[test]
    fn redaction_follows_the_policy_of_each_kind() {
        let s = CopySettings::default();
        let out = format!("token: {KEY}\nok");
        let cmd = prepare(&out, CopyKind::Command, &s);
        assert_eq!(cmd.hidden, 1);
        assert!(!cmd.text.contains(KEY) && cmd.text.contains("ok"));
        let text = prepare(&out, CopyKind::Text, &s);
        assert_eq!(text.hidden, 0);
        assert!(text.text.contains(KEY), "assistant text is not redacted by default");
        let on = s.toggled(CopyKind::Text);
        assert_eq!(prepare(&out, CopyKind::Text, &on).hidden, 1);
        let off = s.toggled(CopyKind::Command);
        assert_eq!(prepare(&out, CopyKind::Command, &off).hidden, 0);
    }

    #[test]
    fn the_hint_counts_and_agrees_in_number() {
        assert_eq!(copied_label(0), "Copiado");
        assert_eq!(copied_label(1), "Copiado · incluye 1 secreto oculto");
        assert_eq!(copied_label(3), "Copiado · incluye 3 secretos ocultos");
    }

    #[test]
    fn plain_text_drops_syntax_and_fences_but_keeps_structure() {
        let md = "# Title\n\nSome **bold** and `code` with [a link](https://x.dev).\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n```rust\nfn a() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let t = markdown_to_plain(md);
        assert!(t.starts_with("Title\n\nSome bold and code with a link."), "{t}");
        assert!(t.contains("- one\n- two\n  - nested"), "{t}");
        assert!(t.contains("1. first\n2. second"), "{t}");
        assert!(t.contains("fn a() {}") && !t.contains("```"), "{t}");
        assert!(t.contains("a\tb\n1\t2"), "{t}");
        assert!(!t.contains("**") && !t.contains('`'), "{t}");
    }

    #[test]
    fn fences_grow_past_backticks_in_the_body() {
        assert_eq!(fenced("ls"), "```\nls\n```");
        assert_eq!(fenced("echo ```x```"), "````\necho ```x```\n````");
    }
}

#[cfg(test)]
mod conversation_tests {
    use super::*;
    use agents_driver::ToolKind;
    use chat_core::{ToolRun, ToolStatus};
    use serde_json::json;

    fn msg(seq: u32, role: Role, blocks: Vec<Block>) -> Message {
        Message {
            seq,
            role,
            created_ms: 0,
            blocks,
            usage: None,
            cost: None,
            attachments: Vec::new(),
            checkpoint: None,
            streaming: false,
        }
    }

    fn command(cmd: &str, out: &str) -> Block {
        Block::Command(ToolRun {
            id: "t".into(),
            kind: ToolKind::Command,
            class: ToolClass::Command,
            title: cmd.into(),
            input: json!({ "command": cmd }),
            status: ToolStatus::Ok,
            summary: Some(out.into()),
            started_ms: 0,
            finished_ms: Some(1),
        })
    }

    #[test]
    fn the_conversation_has_a_heading_per_role_and_redacts_commands_only() {
        let key = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let messages = vec![
            msg(0, Role::User, vec![Block::Text { text: "corre los tests".into() }]),
            msg(
                1,
                Role::Assistant,
                vec![
                    Block::Text { text: "Voy:\n\n```sh\ncargo test\n```".into() },
                    command("cargo test", &format!("GITHUB_TOKEN={key}\n14 passed")),
                    Block::Text { text: format!("Mi clave es {key}") },
                ],
            ),
        ];
        let p = conversation_markdown(&messages, &CopySettings::default());
        assert_eq!(p.hidden, 1, "only the command output is redacted by default");
        assert!(p.text.starts_with("## Usuario\n\ncorre los tests\n\n## Asistente\n\nVoy:"), "{}", p.text);
        assert!(p.text.contains("```\n$ cargo test\n"), "{}", p.text);
        assert!(p.text.contains("14 passed"), "{}", p.text);
        assert!(p.text.contains(&format!("Mi clave es {key}")), "text stays as written");
        let both = conversation_markdown(
            &messages,
            &CopySettings { redact_commands: true, redact_text: true },
        );
        assert_eq!(both.hidden, 2);
        assert!(!both.text.contains(key));
    }
}
