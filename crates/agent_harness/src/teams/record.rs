//! What a team run leaves in the registry: `agent-runs\team-<id>.json` (one file per team run) and
//! the `team-<id>.cancel` marker. Each role run is an ordinary run with its own `<run_id>.json`
//! whose [`TeamTag`] says which team run it belongs to; this file adds what the runs alone cannot
//! say (roles that were skipped and why, the phase of each run, the synthesis).
//!
//! The file has no `run_id` / `thread_id` key on purpose: readers of the run registry (the subagent
//! map, [`crate::Registry::list`]) must not take it for a run.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::registry::Registry;

/// Prefix of the team files inside `agent-runs`.
pub const TEAM_FILE_PREFIX: &str = "team-";

/// Which team run a role run belongs to. Written into the `RunRecord` of every run of the team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamTag {
    /// Id of the team run (`team-<id>.json`).
    pub team_run_id: String,
    /// Name of the team.
    pub team: String,
    /// Name of the role (the key of the role file).
    pub role: String,
    /// `role` (fan-out), `executor`, `review` or `synthesis`.
    pub phase: String,
}

/// Where in a team run a role is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Fan-out: the same task, from the role's point of view.
    Role,
    /// Review mode: the one role that changes the code.
    Executor,
    /// Review mode: a reviewer of the executor's diff.
    Review,
    /// The role that merges the reports.
    Synthesis,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Role => "role",
            Phase::Executor => "executor",
            Phase::Review => "review",
            Phase::Synthesis => "synthesis",
        }
    }

    pub fn parse(s: &str) -> Option<Phase> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "role" => Phase::Role,
            "executor" => Phase::Executor,
            "review" => Phase::Review,
            "synthesis" => Phase::Synthesis,
            _ => return None,
        })
    }
}

/// State of one role of a team run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleState {
    /// Waiting for a free slot or for an earlier phase.
    Pending,
    Running,
    Done,
    Error,
    Cancelled,
    TimedOut,
    BudgetExceeded,
    /// The harness refused it (permission, depth, weekly usage...).
    Refused,
    /// It was not run: `when` did not match, the executor failed, the team budget was spent.
    Skipped,
}

impl RoleState {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleState::Pending => "pending",
            RoleState::Running => "running",
            RoleState::Done => "done",
            RoleState::Error => "error",
            RoleState::Cancelled => "cancelled",
            RoleState::TimedOut => "timed_out",
            RoleState::BudgetExceeded => "budget_exceeded",
            RoleState::Refused => "refused",
            RoleState::Skipped => "skipped",
        }
    }

    pub fn is_final(self) -> bool {
        !matches!(self, RoleState::Pending | RoleState::Running)
    }

    /// The role produced a report that counts.
    pub fn is_ok(self) -> bool {
        self == RoleState::Done
    }
}

/// One role in a team run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleEntry {
    /// Role name (the key).
    pub role: String,
    /// Visible name.
    pub label: String,
    pub phase: Phase,
    pub state: RoleState,
    /// The run of this role (the child thread id as text), once it started.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Why it did not run, or why it did not finish.
    #[serde(default)]
    pub reason: Option<String>,
    /// The role's report (redacted, capped).
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files_changed: Vec<String>,
    #[serde(default)]
    pub billable_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    /// Tokens this role was allowed (the team budget split with the role's own budget).
    #[serde(default)]
    pub budget_tokens: Option<u64>,
    #[serde(default)]
    pub started_ms: Option<i64>,
    #[serde(default)]
    pub finished_ms: Option<i64>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// Effective permission of the role's run.
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

impl RoleEntry {
    pub fn new(role: &str, label: &str, phase: Phase) -> RoleEntry {
        RoleEntry {
            role: role.to_string(),
            label: label.to_string(),
            phase,
            state: RoleState::Pending,
            run_id: None,
            reason: None,
            summary: None,
            files_changed: Vec::new(),
            billable_tokens: 0,
            cost_usd: 0.0,
            budget_tokens: None,
            started_ms: None,
            finished_ms: None,
            worktree_path: None,
            permission: None,
            agent: None,
        }
    }
}

/// State of a whole team run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    Running,
    /// Every role that had to run finished well.
    Done,
    /// Some roles finished well and some did not.
    Partial,
    /// No role produced a report.
    Error,
    Cancelled,
}

impl TeamStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TeamStatus::Running => "running",
            TeamStatus::Done => "done",
            TeamStatus::Partial => "partial",
            TeamStatus::Error => "error",
            TeamStatus::Cancelled => "cancelled",
        }
    }

    pub fn is_final(self) -> bool {
        self != TeamStatus::Running
    }

    /// Process exit code of `team run`.
    pub fn exit_code(self) -> i32 {
        match self {
            TeamStatus::Done | TeamStatus::Running => 0,
            TeamStatus::Partial | TeamStatus::Error => 1,
            TeamStatus::Cancelled => 5,
        }
    }
}

/// How the reports were merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Synthesis {
    /// `orchestrator` (the caller merges; `prompt` has what to ask) or the name of the role that did.
    pub by: String,
    /// The merged report (a role's answer). `None` for `orchestrator` or while it runs.
    #[serde(default)]
    pub report: Option<String>,
    /// For `orchestrator`: the reports and what to do with them, ready to read.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// `team-<id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamRecord {
    pub team_run_id: String,
    pub team: String,
    /// `broadcast`, `review` or `synthesis`.
    pub mode: String,
    /// Redacted, capped copy of the task.
    pub task: String,
    pub project_root: PathBuf,
    #[serde(default)]
    pub parent_thread_id: Option<i64>,
    #[serde(default)]
    pub parent_tab: Option<String>,
    /// `cli`, `mcp` or `app`.
    #[serde(default)]
    pub launcher: Option<String>,
    pub started_ms: i64,
    #[serde(default)]
    pub finished_ms: Option<i64>,
    pub status: TeamStatus,
    /// The `selachi-agent team run` process that coordinates it.
    pub runner_pid: u32,
    #[serde(default)]
    pub runner_start_ms: Option<i64>,
    /// Sum of billable tokens the team may use (`null` = no team budget).
    #[serde(default)]
    pub budget_tokens: Option<u64>,
    #[serde(default)]
    pub tokens_used: u64,
    /// Runs at once.
    pub max_concurrent: usize,
    pub entries: Vec<RoleEntry>,
    #[serde(default)]
    pub synthesis: Option<Synthesis>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl TeamRecord {
    pub fn entry(&self, role: &str) -> Option<&RoleEntry> {
        self.entries.iter().find(|e| e.role == role)
    }

    pub fn billable_tokens(&self) -> u64 {
        self.entries.iter().map(|e| e.billable_tokens).sum()
    }

    pub fn cost_usd(&self) -> f64 {
        self.entries.iter().map(|e| e.cost_usd).sum()
    }

    /// The status the entries add up to once every one is final.
    pub fn final_status(&self, cancelled: bool) -> TeamStatus {
        if cancelled {
            return TeamStatus::Cancelled;
        }
        let ran: Vec<&RoleEntry> = self
            .entries
            .iter()
            .filter(|e| e.state != RoleState::Skipped)
            .collect();
        let done = ran.iter().filter(|e| e.state.is_ok()).count();
        if ran.is_empty() {
            // nothing had to run (every `when` skipped) is not a failure
            TeamStatus::Done
        } else if done == ran.len() {
            TeamStatus::Done
        } else if done == 0 {
            TeamStatus::Error
        } else {
            TeamStatus::Partial
        }
    }

    /// Text for a person (`team result`).
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "team run {} · {} · {} · {}",
            self.team_run_id,
            self.team,
            self.mode,
            self.status.as_str()
        );
        let _ = writeln!(
            s,
            "tokens: {} billable{} · cost ${:.4}",
            self.billable_tokens(),
            self.budget_tokens
                .map(|b| format!(" of {b}"))
                .unwrap_or_default(),
            self.cost_usd()
        );
        for e in &self.entries {
            let _ = writeln!(
                s,
                "\n== {} [{}] · {}{}{}",
                e.label,
                e.phase.as_str(),
                e.state.as_str(),
                e.run_id
                    .as_ref()
                    .map(|r| format!(" · run {r}"))
                    .unwrap_or_default(),
                e.reason
                    .as_ref()
                    .map(|r| format!(" · {r}"))
                    .unwrap_or_default()
            );
            if let Some(sum) = &e.summary {
                let _ = writeln!(s, "{}", sum.trim_end());
            }
            if !e.files_changed.is_empty() {
                let _ = writeln!(s, "files: {}", e.files_changed.join(", "));
            }
        }
        if let Some(syn) = &self.synthesis {
            let _ = writeln!(s, "\n== SYNTHESIS (by {})", syn.by);
            if let Some(r) = &syn.report {
                let _ = writeln!(s, "{}", r.trim_end());
            } else if let Some(p) = &syn.prompt {
                let _ = writeln!(s, "(the orchestrator merges the reports)\n{}", p.trim_end());
            }
        }
        for n in &self.notes {
            let _ = writeln!(s, "note: {n}");
        }
        s
    }
}

/// `team-<id>.json` of `dir`.
pub fn team_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{TEAM_FILE_PREFIX}{id}.json"))
}

fn cancel_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{TEAM_FILE_PREFIX}{id}.cancel"))
}

/// Team run ids are decimal numbers (a millisecond clock plus a counter), like run ids.
pub fn valid_team_id(id: &str) -> bool {
    Registry::valid_id(id)
}

impl Registry {
    /// A new, unused team run id.
    pub fn new_team_run_id(&self) -> String {
        let mut n = chat_core::now_ms().max(1);
        while team_file(self.dir(), &n.to_string()).exists() {
            n += 1;
        }
        n.to_string()
    }

    /// Write (atomically) the team record.
    pub fn save_team(&self, rec: &TeamRecord) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir())?;
        let tmp = self
            .dir()
            .join(format!("{TEAM_FILE_PREFIX}{}.json.tmp", rec.team_run_id));
        let json = serde_json::to_vec_pretty(rec).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, team_file(self.dir(), &rec.team_run_id))
    }

    /// The record of a team run. A run that says `running` whose coordinator is gone is reconciled to
    /// an error (and saved), so a crash never leaves a team "running" forever.
    pub fn load_team(&self, id: &str) -> Result<TeamRecord, String> {
        if !valid_team_id(id) {
            return Err(format!(
                "`{id}` is not a team run id (they are numbers, see `selachi-agent team run`)"
            ));
        }
        let text = std::fs::read_to_string(team_file(self.dir(), id)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("no team run {id} in {}", self.dir().display()),
            _ => format!("cannot read team run {id}: {e}"),
        })?;
        let mut rec: TeamRecord =
            serde_json::from_str(&text).map_err(|e| format!("team run {id} is corrupt: {e}"))?;
        if rec.status == TeamStatus::Running && !coordinator_alive(&rec) {
            rec.status = TeamStatus::Error;
            rec.finished_ms = Some(chat_core::now_ms());
            for e in &mut rec.entries {
                if !e.state.is_final() {
                    e.state = RoleState::Error;
                    e.reason =
                        Some("the process coordinating the team ended without finishing".into());
                }
            }
            rec.notes
                .push("the process that coordinated the team ended without a result".into());
            if let Err(e) = self.save_team(&rec) {
                log::warn!("agent_harness: cannot save the reconciled team run {id}: {e}");
            }
        }
        Ok(rec)
    }

    /// Every readable team record, newest first.
    pub fn list_teams(&self) -> Vec<TeamRecord> {
        let Ok(rd) = std::fs::read_dir(self.dir()) else {
            return Vec::new();
        };
        let mut out: Vec<TeamRecord> = rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with(TEAM_FILE_PREFIX) && n.ends_with(".json")
            })
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.started_ms));
        out
    }

    /// Ask the coordinator of `id` to cancel the whole team.
    pub fn request_team_cancel(&self, id: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir())?;
        std::fs::write(cancel_file(self.dir(), id), b"cancel")
    }

    pub fn team_cancel_requested(&self, id: &str) -> bool {
        cancel_file(self.dir(), id).exists()
    }

    pub fn clear_team_cancel(&self, id: &str) {
        let _ = std::fs::remove_file(cancel_file(self.dir(), id));
    }
}

/// Is the process that coordinates `rec` still the same one?
fn coordinator_alive(rec: &TeamRecord) -> bool {
    let Some(started) = proc_svc::start_time_of(rec.runner_pid) else {
        return false;
    };
    let Some(expected) = rec.runner_start_ms else {
        return true;
    };
    let actual = started
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (actual - expected).abs() < 2_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str) -> TeamRecord {
        TeamRecord {
            team_run_id: id.into(),
            team: "Revision".into(),
            mode: "broadcast".into(),
            task: "t".into(),
            project_root: "C:/p".into(),
            parent_thread_id: None,
            parent_tab: None,
            launcher: Some("cli".into()),
            started_ms: 10,
            finished_ms: None,
            status: TeamStatus::Done,
            runner_pid: std::process::id(),
            runner_start_ms: None,
            budget_tokens: Some(1000),
            tokens_used: 0,
            max_concurrent: 2,
            entries: vec![RoleEntry::new("tester", "Tester", Phase::Role)],
            synthesis: None,
            notes: Vec::new(),
        }
    }

    #[test]
    fn save_load_list_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        assert!(reg.list_teams().is_empty());
        let mut a = rec("5");
        a.entries[0].state = RoleState::Done;
        reg.save_team(&a).unwrap();
        reg.save_team(&rec("6")).unwrap();
        assert_eq!(reg.load_team("5").unwrap().entries[0].state, RoleState::Done);
        assert_eq!(reg.list_teams().len(), 2);
        assert!(reg.load_team("9").unwrap_err().contains("no team run 9"));
        assert!(reg.load_team("../x").is_err());
        assert!(!reg.team_cancel_requested("5"));
        reg.request_team_cancel("5").unwrap();
        assert!(reg.team_cancel_requested("5"));
        reg.clear_team_cancel("5");
        assert!(!reg.team_cancel_requested("5"));
        // team files are not runs
        assert!(reg.list().is_empty());
        let id = reg.new_team_run_id();
        assert!(valid_team_id(&id));
    }

    #[test]
    fn a_dead_coordinator_does_not_leave_the_team_running() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        let mut r = rec("7");
        r.status = TeamStatus::Running;
        r.runner_pid = u32::MAX - 1; // no such process
        r.entries[0].state = RoleState::Running;
        reg.save_team(&r).unwrap();
        let back = reg.load_team("7").unwrap();
        assert_eq!(back.status, TeamStatus::Error);
        assert_eq!(back.entries[0].state, RoleState::Error);
        assert!(back.finished_ms.is_some());
        // and it stays that way on disk
        assert_eq!(reg.load_team("7").unwrap().status, TeamStatus::Error);
    }

    #[test]
    fn final_status_adds_the_entries_up() {
        let mut r = rec("1");
        r.entries = vec![
            RoleEntry::new("a", "A", Phase::Role),
            RoleEntry::new("b", "B", Phase::Role),
            RoleEntry::new("c", "C", Phase::Role),
        ];
        r.entries[0].state = RoleState::Done;
        r.entries[1].state = RoleState::Error;
        r.entries[2].state = RoleState::Skipped;
        assert_eq!(r.final_status(false), TeamStatus::Partial);
        r.entries[0].state = RoleState::Error;
        assert_eq!(r.final_status(false), TeamStatus::Error);
        r.entries[1].state = RoleState::Done;
        r.entries[0].state = RoleState::Done;
        assert_eq!(r.final_status(false), TeamStatus::Done);
        assert_eq!(r.final_status(true), TeamStatus::Cancelled);
        for e in &mut r.entries {
            e.state = RoleState::Skipped;
        }
        assert_eq!(r.final_status(false), TeamStatus::Done);
        assert!(r.to_text().contains("team run 1"));
    }
}
