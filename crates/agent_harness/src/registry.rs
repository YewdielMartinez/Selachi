//! Run registry: one small JSON file per run in `<data dir>\agent-runs`.
//!
//! `chat.db` keeps the conversation (as a child thread); this keeps what the harness needs to
//! find a run again from another process: the checkpoint, the worktree, the runner's pid, the
//! final result, and the cancel marker. A run is identified by its thread id.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::output::{RunResult, RunStatus};

/// Everything known about one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub thread_id: i64,
    pub agent: String,
    pub model: Option<String>,
    /// `read-only | ask | accept-edits`.
    pub permission: String,
    /// Directory the agent works in (the worktree when there is one).
    pub cwd: PathBuf,
    /// Project the delegation was asked on.
    pub project_root: PathBuf,
    pub worktree_path: Option<PathBuf>,
    /// Checkpoint taken in `cwd` before the agent ran (what `diff` compares against).
    pub checkpoint_id: Option<String>,
    /// Redacted, capped copy of the prompt.
    pub prompt: String,
    pub parent_thread_id: Option<i64>,
    pub depth: u32,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub status: RunStatus,
    /// Pid of the `selachi-agent` process that runs it, and when that process started (to tell a
    /// reused pid from the real one).
    pub runner_pid: u32,
    pub runner_start_ms: Option<i64>,
    pub deadline_ms: i64,
    pub result: Option<RunResult>,
    /// Set by `apply`.
    pub applied_ms: Option<i64>,
    // ---- additive fields (PLAN section 21.2 / 21.3: the map inspector reads them; records written
    // before they existed load with the defaults) ----
    /// Name of the subagent profile the run used, if any.
    #[serde(default)]
    pub profile: Option<String>,
    /// The exact prompt the agent received (profile instructions + context files + output format +
    /// task), redacted and capped at [`MAX_FINAL_PROMPT_CHARS`]. For a run without a profile it is the
    /// redacted task prompt.
    #[serde(default)]
    pub final_prompt: Option<String>,
    /// The limits that were in force (after taking the minimum of profile, `[delegation]` and caller).
    #[serde(default)]
    pub limits: Option<RunLimits>,
    /// Who launched the run: `cli` (a shell command) or `mcp` (the MCP server).
    #[serde(default)]
    pub launcher: Option<String>,
    /// Reasoning effort requested by the profile, if any.
    #[serde(default)]
    pub effort: Option<String>,
    /// `text`, `json` or `diff` (the profile's `output_format`); `None` without a profile.
    #[serde(default)]
    pub output_format: Option<String>,
    /// Context files that were attached (project-relative, forward slashes).
    #[serde(default)]
    pub context_files: Vec<String>,
}

/// Longest `final_prompt` kept in a run record (characters).
pub const MAX_FINAL_PROMPT_CHARS: usize = 300_000;

/// Limits applied to a run: the minimum of the profile, `[delegation]` and the calling session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLimits {
    /// Permission the agent ran with: `read-only | ask | accept-edits`.
    pub permission: String,
    /// Permission of the delegating session (`SELACHI_PARENT_PERMISSION`).
    pub parent_permission: String,
    /// `[delegation] max_permission`.
    pub config_max_permission: String,
    /// What the profile asked for (`null` without a profile or when it says nothing).
    pub profile_permission: Option<String>,
    /// Depth of this run (1 = delegated by a top-level session).
    pub depth: u32,
    /// `[delegation] max_depth`.
    pub max_depth: u32,
    pub timeout_secs: u64,
    /// `null` = no per-run token budget (the daily budget still applies).
    pub budget_tokens: Option<u64>,
    pub daily_token_budget: u64,
    pub max_concurrent: usize,
    pub worktree: bool,
    /// Tool lists of the profile (Claude only enforces them: see `tools_enforced`).
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    /// `true` when the agent was started with the tool restrictions (`--allowedTools` /
    /// `--disallowedTools`); `false` when the profile has none or the agent cannot enforce them.
    pub tools_enforced: bool,
    // ---- ola 8D (additive; records written before it load with the defaults) ----
    /// Effective approval mode: the minimum of `[delegation] approvals`, the profile's and the
    /// request's (`ask | allowlist | auto`); `auto` also after "No volver a preguntar en esta ejecución".
    #[serde(default = "default_approvals")]
    pub approvals: String,
    /// The profile's `allowed_commands` (only used when the mode is `allowlist`).
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// Commands approved without asking so far (allowlist matches and the `auto` mode).
    #[serde(default)]
    pub auto_approved: u32,
    /// The last [`MAX_AUTO_APPROVED_LOG`] of them, redacted and shortened.
    #[serde(default)]
    pub auto_approved_commands: Vec<String>,
    /// Requests denied by the hard blocklist, without asking.
    #[serde(default)]
    pub blocked: u32,
}

fn default_approvals() -> String {
    "ask".to_string()
}

/// How many auto-approved commands a record keeps.
pub const MAX_AUTO_APPROVED_LOG: usize = 50;

impl RunLimits {
    /// Counts one command approved on its own and keeps it in the (bounded) log.
    pub fn note_auto_approved(&mut self, command: &str) {
        self.auto_approved = self.auto_approved.saturating_add(1);
        let one_line = command.trim().replace(['\r', '\n'], " ");
        let shown: String = if one_line.chars().count() > 300 {
            one_line.chars().take(300).chain(Some('…')).collect()
        } else {
            one_line
        };
        self.auto_approved_commands.push(shown);
        let extra = self
            .auto_approved_commands
            .len()
            .saturating_sub(MAX_AUTO_APPROVED_LOG);
        if extra > 0 {
            self.auto_approved_commands.drain(..extra);
        }
    }
}

impl RunRecord {
    /// Billable tokens of a finished run (0 while running).
    pub fn billable_tokens(&self) -> u64 {
        self.result.as_ref().map_or(0, |r| r.usage.billable)
    }
}

/// `<data dir>\agent-runs`.
#[derive(Debug, Clone)]
pub struct Registry {
    dir: PathBuf,
}

impl Registry {
    pub fn new(data_dir: &Path) -> Registry {
        Registry {
            dir: data_dir.join("agent-runs"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, run_id: &str) -> PathBuf {
        self.dir.join(format!("{run_id}.json"))
    }

    fn cancel_file(&self, run_id: &str) -> PathBuf {
        self.dir.join(format!("{run_id}.cancel"))
    }

    /// Run ids are decimal thread ids; anything else could point outside the directory.
    pub fn valid_id(run_id: &str) -> bool {
        !run_id.is_empty() && run_id.len() <= 18 && run_id.bytes().all(|b| b.is_ascii_digit())
    }

    /// Write (atomically: temp file + rename) the record.
    pub fn save(&self, rec: &RunRecord) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{}.json.tmp", rec.run_id));
        let json = serde_json::to_vec_pretty(rec).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.file(&rec.run_id))
    }

    pub fn load(&self, run_id: &str) -> Result<RunRecord, String> {
        if !Registry::valid_id(run_id) {
            return Err(format!(
                "`{run_id}` is not a run id (run ids are numbers, see `selachi-agent run`)"
            ));
        }
        let text = std::fs::read_to_string(self.file(run_id)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("no run {run_id} in {}", self.dir.display()),
            _ => format!("cannot read run {run_id}: {e}"),
        })?;
        serde_json::from_str(&text).map_err(|e| format!("run {run_id} is corrupt: {e}"))
    }

    /// Every readable record, newest first.
    pub fn list(&self) -> Vec<RunRecord> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<RunRecord> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.started_ms));
        out
    }

    /// Ask the runner of `run_id` to stop (it checks between events).
    pub fn request_cancel(&self, run_id: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.cancel_file(run_id), b"cancel")
    }

    pub fn cancel_requested(&self, run_id: &str) -> bool {
        self.cancel_file(run_id).exists()
    }

    pub fn clear_cancel(&self, run_id: &str) {
        let _ = std::fs::remove_file(self.cancel_file(run_id));
    }

    /// Billable tokens of the runs that started in `[start_ms, end_ms)` (the daily budget).
    pub fn billable_tokens_between(&self, start_ms: i64, end_ms: i64) -> u64 {
        self.list()
            .iter()
            .filter(|r| r.started_ms >= start_ms && r.started_ms < end_ms)
            .map(RunRecord::billable_tokens)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn rec(id: &str, started_ms: i64, billable: u64) -> RunRecord {
        let mut result = RunResult::empty(RunStatus::Done);
        result.usage = crate::output::UsageOut::new(billable, 0, 0, 0);
        RunRecord {
            run_id: id.into(),
            thread_id: id.parse().unwrap(),
            agent: "codex".into(),
            model: None,
            permission: "read-only".into(),
            cwd: "C:/p".into(),
            project_root: "C:/p".into(),
            worktree_path: None,
            checkpoint_id: None,
            prompt: "p".into(),
            parent_thread_id: None,
            depth: 1,
            started_ms,
            finished_ms: Some(started_ms + 1),
            status: RunStatus::Done,
            runner_pid: 1,
            runner_start_ms: None,
            deadline_ms: started_ms + 1000,
            result: Some(result),
            applied_ms: None,
            profile: None,
            final_prompt: None,
            limits: None,
            launcher: None,
            effort: None,
            output_format: None,
            context_files: Vec::new(),
        }
    }

    #[test]
    fn save_load_list_and_budget_window() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        assert!(reg.list().is_empty());
        reg.save(&rec("1", 100, 10)).unwrap();
        reg.save(&rec("2", 200, 20)).unwrap();
        reg.save(&rec("3", 300, 30)).unwrap();
        assert_eq!(reg.load("2").unwrap().started_ms, 200);
        assert_eq!(
            reg.list()
                .iter()
                .map(|r| r.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["3", "2", "1"]
        );
        assert_eq!(reg.billable_tokens_between(150, 301), 50);
        assert!(reg.load("9").unwrap_err().contains("no run 9"));
    }

    #[test]
    fn ids_cannot_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        for bad in [
            "",
            "..",
            "..\\x",
            "1/2",
            "a1",
            "1.json",
            "12345678901234567890",
        ] {
            assert!(!Registry::valid_id(bad), "{bad}");
            assert!(reg.load(bad).is_err());
        }
        assert!(Registry::valid_id("42"));
    }

    #[test]
    fn cancel_marker() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        assert!(!reg.cancel_requested("5"));
        reg.request_cancel("5").unwrap();
        assert!(reg.cancel_requested("5"));
        reg.clear_cancel("5");
        assert!(!reg.cancel_requested("5"));
    }
}
