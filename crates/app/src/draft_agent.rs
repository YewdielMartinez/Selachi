//! "Redactar con agente" (backlog B3): one read-only turn of the user's agent to draft a commit
//! message or a PR description.
//!
//! It runs as a short-lived chat thread (so it goes through the same driver, usage accounting and
//! scripted agent as the chat: `SELACHI_CHAT_FAKE=1` answers without a model), is read-only, never
//! uses a worktree, and the thread is **deleted** when the answer is in: a draft is not a
//! conversation worth keeping. Tokens are spent only when the user pressed the button; the prompt
//! (diff included) was already redacted by `ui_git`.

use std::path::PathBuf;
use std::time::Duration;

use agents_driver::{AgentKind, PermissionMode};
use chat_core::{Block, ChatStore, DeleteOptions, Role, ThreadSpec, ThreadState, ThreadStatus};

/// How long the agent has to answer.
pub const DRAFT_TIMEOUT: Duration = Duration::from_secs(120);

/// The final assistant text of a finished turn: the text blocks of the last assistant message.
pub fn last_assistant_text(state: &ThreadState) -> Option<String> {
    let msg = state
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)?;
    let text: String = msg
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Drops a wrapping code fence some models add around the whole answer.
pub fn strip_fence(text: &str) -> String {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix("```") {
        // skip the language tag on the first line
        let body = inner.split_once('\n').map_or("", |(_, b)| b);
        if let Some(body) = body.trim_end().strip_suffix("```") {
            return body.trim().to_string();
        }
    }
    t.to_string()
}

/// Runs the draft synchronously (call it from the background executor). `Err` is a message for the
/// user, in Spanish.
pub fn run(
    store: &ChatStore,
    agent: AgentKind,
    root: PathBuf,
    prompt: &str,
) -> Result<String, String> {
    let mut spec = ThreadSpec::new(root, agent);
    spec.title = Some("Borrador de git".to_string());
    spec.permission = PermissionMode::ReadOnly;
    spec.use_worktree = false;
    spec.checkpoint_before_turn = false;
    let rec = store
        .create_thread(spec)
        .map_err(|e| format!("no se pudo crear el hilo del borrador: {e}"))?;
    let id = rec.id;
    let result = (|| {
        let rt = store.open(id).map_err(|e| e.to_string())?;
        rt.send(prompt, Vec::new()).map_err(|e| e.to_string())?;
        let finished = rt.wait_for(DRAFT_TIMEOUT, |s| {
            matches!(
                s.status,
                ThreadStatus::Done | ThreadStatus::Error | ThreadStatus::WaitingForInput
            )
        });
        let (status, text) = rt.with_state(|s| (s.status, last_assistant_text(s)));
        if !finished {
            let _ = rt.interrupt();
            return Err("el agente no respondió a tiempo".to_string());
        }
        match status {
            ThreadStatus::WaitingForInput => {
                let _ = rt.interrupt();
                Err("el agente pidió una aprobación; el borrador es de solo lectura".to_string())
            }
            ThreadStatus::Error => {
                Err(text.unwrap_or_else(|| "el agente terminó con un error".into()))
            }
            _ => text
                .map(|t| strip_fence(&t))
                .filter(|t| !t.is_empty())
                .ok_or_else(|| "el agente no devolvió texto".to_string()),
        }
    })();
    let _ = store.delete_thread(id, DeleteOptions::default());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapping_fence_is_dropped() {
        assert_eq!(strip_fence("```\nfix: x\n\nbody\n```"), "fix: x\n\nbody");
        assert_eq!(strip_fence("```text\nfix: x\n```"), "fix: x");
        assert_eq!(
            strip_fence("fix: x\n\n```rs\ncode\n```"),
            "fix: x\n\n```rs\ncode\n```"
        );
        assert_eq!(strip_fence("  plain  "), "plain");
    }
}
