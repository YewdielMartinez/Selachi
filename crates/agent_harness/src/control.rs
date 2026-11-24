//! Looking at a run from another process: `status`, `result`, `cancel`, `diff`.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::output::{RunResult, RunStatus};
use crate::registry::RunRecord;
use crate::run::Harness;

/// How long `cancel` waits for the runner to stop by itself before it kills it.
const CANCEL_GRACE: Duration = Duration::from_secs(10);

/// `selachi-agent status`.
#[derive(Debug, Clone, Serialize)]
pub struct StatusOut {
    pub run_id: String,
    pub status: RunStatus,
    pub agent: String,
    pub model: Option<String>,
    pub permission: String,
    pub prompt: String,
    pub thread_id: i64,
    pub parent_thread_id: Option<i64>,
    pub depth: u32,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub elapsed_secs: i64,
    /// While running: when the timeout will fire.
    pub deadline_ms: i64,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub applied: bool,
    pub runner_pid: u32,
    pub reason: Option<String>,
}

impl StatusOut {
    fn of(rec: &RunRecord) -> StatusOut {
        let end = rec.finished_ms.unwrap_or_else(chat_core::now_ms);
        StatusOut {
            run_id: rec.run_id.clone(),
            status: rec.status,
            agent: rec.agent.clone(),
            model: rec.model.clone(),
            permission: rec.permission.clone(),
            prompt: rec.prompt.clone(),
            thread_id: rec.thread_id,
            parent_thread_id: rec.parent_thread_id,
            depth: rec.depth,
            started_ms: rec.started_ms,
            finished_ms: rec.finished_ms,
            elapsed_secs: ((end - rec.started_ms) / 1000).max(0),
            deadline_ms: rec.deadline_ms,
            cwd: store::paths::clean_display(&rec.cwd.to_string_lossy()),
            worktree_path: rec
                .worktree_path
                .as_ref()
                .map(|p| store::paths::clean_display(&p.to_string_lossy())),
            applied: rec.applied_ms.is_some(),
            runner_pid: rec.runner_pid,
            reason: rec.result.as_ref().and_then(|r| r.reason.clone()),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn to_text(&self) -> String {
        format!(
            "run {} · {} · {} · {} s{}{}\n  {}\n",
            self.run_id,
            self.status.as_str(),
            self.agent,
            self.elapsed_secs,
            self.worktree_path
                .as_ref()
                .map(|w| format!(" · worktree {w}"))
                .unwrap_or_default(),
            if self.applied { " · applied" } else { "" },
            self.prompt.lines().next().unwrap_or(""),
        )
    }
}

/// What `cancel` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CancelOutcome {
    /// The run had already ended.
    AlreadyFinished { status: RunStatus },
    /// The runner saw the request and stopped the agent.
    Cancelled,
    /// The runner did not react in time and was killed (its Job Object ends the agent's tree).
    Killed,
}

/// Is the process that was running `rec` still the same one?
fn runner_alive(rec: &RunRecord) -> bool {
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

impl Harness {
    /// The record of `run_id`. A run that says `running` but whose runner is gone is reconciled to
    /// `error` (and saved), so a crash never leaves a run "running" forever.
    pub fn record(&self, run_id: &str) -> Result<RunRecord, String> {
        let mut rec = self.registry.load(run_id)?;
        if rec.status == RunStatus::Running && !runner_alive(&rec) {
            let reason = "the process that was running it ended without reporting a result (crashed or was killed)".to_string();
            rec.status = RunStatus::Error;
            rec.finished_ms = Some(chat_core::now_ms());
            rec.result = Some(RunResult {
                run_id: Some(rec.run_id.clone()),
                summary: reason.clone(),
                reason: Some(reason),
                agent: Some(rec.agent.clone()),
                permission: Some(rec.permission.clone()),
                thread_id: Some(rec.thread_id),
                worktree_path: rec
                    .worktree_path
                    .as_ref()
                    .map(|p| store::paths::clean_display(&p.to_string_lossy())),
                ..RunResult::empty(RunStatus::Error)
            });
            if let Err(e) = self.registry.save(&rec) {
                log::warn!("agent_harness: cannot save the reconciled record of run {run_id}: {e}");
            }
            self.registry.clear_cancel(run_id);
        }
        Ok(rec)
    }

    pub fn status(&self, run_id: &str) -> Result<StatusOut, String> {
        self.record(run_id).map(|r| StatusOut::of(&r))
    }

    /// The final result; an error while the run is still going.
    pub fn result(&self, run_id: &str) -> Result<RunResult, String> {
        let rec = self.record(run_id)?;
        match rec.result {
            Some(r) if rec.status.is_final() => Ok(r),
            _ => Err(format!(
                "run {run_id} is still running; check `selachi-agent status {run_id}`"
            )),
        }
    }

    /// Stop a run started by another `selachi-agent` process.
    pub fn cancel(&self, run_id: &str) -> Result<CancelOutcome, String> {
        let rec = self.record(run_id)?;
        if rec.status.is_final() {
            return Ok(CancelOutcome::AlreadyFinished { status: rec.status });
        }
        self.registry
            .request_cancel(run_id)
            .map_err(|e| format!("cannot request the cancellation: {e}"))?;
        let end = Instant::now() + CANCEL_GRACE;
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
            let now = self.record(run_id)?;
            if now.status.is_final() {
                return Ok(if now.status == RunStatus::Cancelled {
                    CancelOutcome::Cancelled
                } else {
                    CancelOutcome::AlreadyFinished { status: now.status }
                });
            }
        }
        // The runner is stuck: end it. Its Job Object (kill on close) takes the agent's tree with it.
        let mut rec = self.record(run_id)?;
        if rec.status.is_final() {
            return Ok(CancelOutcome::AlreadyFinished { status: rec.status });
        }
        if runner_alive(&rec) {
            proc_svc::terminate_tree(rec.runner_pid, Duration::from_millis(500)).map_err(|e| {
                format!(
                    "the runner (pid {}) did not stop and could not be killed: {e}",
                    rec.runner_pid
                )
            })?;
        }
        rec.status = RunStatus::Cancelled;
        rec.finished_ms = Some(chat_core::now_ms());
        let reason = "cancelled; the runner did not respond and was killed".to_string();
        rec.result = Some(RunResult {
            run_id: Some(rec.run_id.clone()),
            summary: reason.clone(),
            reason: Some(reason),
            agent: Some(rec.agent.clone()),
            permission: Some(rec.permission.clone()),
            thread_id: Some(rec.thread_id),
            ..RunResult::empty(RunStatus::Cancelled)
        });
        self.registry.save(&rec).map_err(|e| e.to_string())?;
        self.registry.clear_cancel(run_id);
        Ok(CancelOutcome::Killed)
    }

    /// Unified diff of what the run changed (against the checkpoint taken before it started).
    /// Raw: callers that print it should pass it through `store::redact`.
    pub fn diff(&self, run_id: &str) -> Result<String, String> {
        let rec = self.record(run_id)?;
        let Some(cp) = rec.checkpoint_id.as_deref() else {
            return Err(format!("run {run_id} has no checkpoint (it did not run inside a git repository), so there is no diff"));
        };
        let repo = git_svc::Repo::discover(&rec.cwd)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{} is no longer a git repository", rec.cwd.display()))?;
        repo.diff_since_checkpoint(cp).map_err(|e| e.to_string())
    }
}
