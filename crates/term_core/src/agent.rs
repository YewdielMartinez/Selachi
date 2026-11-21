//! Helpers built on [`Block`]: long-command notifications (A4) and the "fix it with an agent"
//! prompt (A3).

use std::path::PathBuf;
use std::time::Duration;

use crate::block::{Block, BlockState};
use crate::redact::redact;

/// When a finished command deserves a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LongCommandPolicy {
    /// Commands that ran at least this long notify (default 10 s).
    pub min_duration: Duration,
}

impl Default for LongCommandPolicy {
    fn default() -> Self {
        Self { min_duration: Duration::from_secs(10) }
    }
}

/// `true` when `block` finished, ran for at least `policy.min_duration`, and the window is not
/// focused (the user is looking elsewhere).
pub fn should_notify(block: &Block, window_focused: bool, policy: &LongCommandPolicy) -> bool {
    !window_focused
        && block.state != BlockState::Running
        && block.duration.is_some_and(|d| d >= policy.min_duration)
}

/// Everything an agent needs to help with a command. Command and output are already redacted.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentContext {
    /// The command line (redacted).
    pub command: String,
    /// Working directory when it ran.
    pub cwd: Option<PathBuf>,
    /// Exit code, if the shell reported one.
    pub exit_code: Option<i32>,
    /// How long it ran.
    pub duration: Option<Duration>,
    /// Git branch of the working directory, if known.
    pub branch: Option<String>,
    /// Output text (redacted).
    pub output: String,
    /// How many secrets were hidden in command and output together.
    pub redactions: usize,
}

/// Builds the context for `block`; `output` is the text from `Session::block_output_text`.
pub fn agent_context(block: &Block, output: &str, git_branch: Option<&str>) -> AgentContext {
    let (command, n1) = redact(block.command.as_deref().unwrap_or(""));
    let (output, n2) = redact(output);
    AgentContext {
        command,
        cwd: block.cwd.clone(),
        exit_code: block.exit_code,
        duration: block.duration,
        branch: git_branch.map(str::to_owned),
        output,
        redactions: n1 + n2,
    }
}

/// Longest run of consecutive backticks in `s`.
fn longest_backtick_run(s: &str) -> usize {
    let (mut best, mut cur) = (0, 0);
    for c in s.chars() {
        cur = if c == '`' { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

/// Wraps `s` as inline code, using a longer fence when it contains backticks.
fn inline_code(s: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(s) + 1);
    if s.starts_with('`') || s.ends_with('`') {
        format!("{fence} {s} {fence}")
    } else {
        format!("{fence}{s}{fence}")
    }
}

impl AgentContext {
    /// A Spanish prompt: what ran, where, how it ended, the output, and the request.
    pub fn to_prompt_markdown(&self) -> String {
        let command = if self.command.is_empty() { "(sin texto)" } else { &self.command };
        let outcome = match self.exit_code {
            Some(0) => "terminó con código 0".to_string(),
            Some(n) => format!("falló con código {n}"),
            None => "terminó sin código de salida".to_string(),
        };
        let mut out = format!("El comando {} {outcome}", inline_code(command));
        if let Some(cwd) = &self.cwd {
            out.push_str(&format!(" en {}", inline_code(&cwd.display().to_string())));
        }
        if let Some(branch) = &self.branch {
            out.push_str(&format!(" (rama {})", inline_code(branch)));
        }
        out.push('.');
        if let Some(d) = self.duration {
            out.push_str(&format!(" Duró {:.1} s.", d.as_secs_f64()));
        }
        if self.output.trim().is_empty() {
            out.push_str(" No produjo salida.");
        } else {
            let fence = "`".repeat(longest_backtick_run(&self.output).max(2) + 1);
            out.push_str(&format!(" Salida:\n{fence}\n{}\n{fence}\n", self.output.trim_end()));
        }
        out.push_str(" Ayúdame a arreglarlo.");
        out
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Instant, SystemTime};

    use super::*;
    use crate::block::{BlockId, BlockState};

    fn block(state: BlockState, secs: Option<u64>) -> Block {
        Block {
            id: BlockId(1),
            command: Some("cargo test".into()),
            cwd: Some(PathBuf::from("C:\\proj")),
            started_at: Instant::now(),
            started_wall: SystemTime::now(),
            finished_at: None,
            finished_wall: None,
            duration: secs.map(Duration::from_secs),
            exit_code: Some(101),
            state,
            prompt_line: 0,
            command_line: 0,
            output_start: 1,
            output_end: Some(5),
        }
    }

    #[test]
    fn notification_policy() {
        let p = LongCommandPolicy::default();
        assert_eq!(p.min_duration, Duration::from_secs(10));
        let long = block(BlockState::Failed, Some(12));
        assert!(should_notify(&long, false, &p));
        assert!(!should_notify(&long, true, &p), "window focused");
        assert!(!should_notify(&block(BlockState::Succeeded, Some(9)), false, &p), "too short");
        assert!(should_notify(&block(BlockState::Succeeded, Some(10)), false, &p), "threshold is inclusive");
        assert!(!should_notify(&block(BlockState::Running, None), false, &p));
        assert!(!should_notify(&block(BlockState::Unknown, None), false, &p));
        let strict = LongCommandPolicy { min_duration: Duration::from_secs(60) };
        assert!(!should_notify(&long, false, &strict));
    }

    #[test]
    fn prompt_is_spanish_and_redacted() {
        let mut b = block(BlockState::Failed, Some(3));
        b.command = Some("deploy --token abc123".into());
        let ctx = agent_context(&b, "error: boom\nAPI_KEY=hunter2\n", Some("feature/x"));
        assert_eq!(ctx.redactions, 2);
        let md = ctx.to_prompt_markdown();
        assert!(md.starts_with("El comando `deploy --token [REDACTED]` falló con código 101 en `C:\\proj` (rama `feature/x`)."), "{md}");
        assert!(md.contains("Salida:\n```\nerror: boom\nAPI_KEY=[REDACTED]\n```\n"), "{md}");
        assert!(md.ends_with("Ayúdame a arreglarlo."), "{md}");
        assert!(!md.contains("hunter2") && !md.contains("abc123"));
    }

    #[test]
    fn prompt_handles_odd_inputs() {
        let mut b = block(BlockState::Unknown, None);
        b.command = Some("echo `date`".into());
        b.cwd = None;
        b.exit_code = None;
        let ctx = agent_context(&b, "has ``` fence inside", None);
        let md = ctx.to_prompt_markdown();
        assert!(md.contains("`` echo `date` ``"), "{md}");
        assert!(md.contains("terminó sin código de salida"));
        assert!(md.contains("````\nhas ``` fence inside\n````"), "{md}");
        // No output at all.
        let md = agent_context(&b, "  \n", None).to_prompt_markdown();
        assert!(md.contains("No produjo salida.") && !md.contains("Salida:"));
        // Success and missing command.
        b.command = None;
        b.exit_code = Some(0);
        let md = agent_context(&b, "", None).to_prompt_markdown();
        assert!(md.contains("`(sin texto)` terminó con código 0"), "{md}");
    }
}
