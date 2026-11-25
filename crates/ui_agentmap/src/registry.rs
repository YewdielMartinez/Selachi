//! Reads the harness run registry (`<data dir>\agent-runs\<run id>.json`, one file per run).
//!
//! The harness owns the format and keeps adding fields (`profile`, `final_prompt`,
//! `effective_limits`, `launcher`...), so nothing here is a strict `Deserialize`: every field is
//! read on its own from the JSON value and a missing or ill-typed one just stays `None`. A file
//! that is not JSON, or has neither `run_id` nor `thread_id`, is skipped.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// How a run stands according to the registry (`RunStatus` of the harness, `snake_case`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Done,
    Error,
    Cancelled,
    TimedOut,
    BudgetExceeded,
    Refused,
}

impl RunStatus {
    pub fn parse(s: &str) -> Option<RunStatus> {
        Some(
            match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                "running" => RunStatus::Running,
                "done" => RunStatus::Done,
                "error" => RunStatus::Error,
                "cancelled" | "canceled" => RunStatus::Cancelled,
                "timed_out" | "timedout" => RunStatus::TimedOut,
                "budget_exceeded" => RunStatus::BudgetExceeded,
                "refused" => RunStatus::Refused,
                _ => return None,
            },
        )
    }
}

/// `git diff --stat` totals of a run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffStatInfo {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
    /// Per-file lines plus the summary, ready to show.
    pub text: String,
}

/// An approval the child asked for and what the user decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalEntry {
    pub tool: String,
    pub detail: String,
    /// `allowed`, `denied`, `always`, `expired` or `pending`.
    pub decision: String,
    pub at_ms: Option<i64>,
}

/// The `result` of a finished run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultInfo {
    pub summary: Option<String>,
    pub files_changed: Vec<String>,
    pub diff_stat: Option<DiffStatInfo>,
    pub billable_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_exact: Option<bool>,
    pub worktree_path: Option<PathBuf>,
    pub reason: Option<String>,
    pub notes: Vec<String>,
}

/// Everything the registry knows about one run. Every field but the id is optional.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunInfo {
    /// The run id (the child thread id as text).
    pub run_id: String,
    pub thread_id: Option<i64>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `read-only | ask | accept-edits`.
    pub permission: Option<String>,
    pub cwd: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    pub worktree_path: Option<PathBuf>,
    /// Redacted, capped prompt as the harness stored it.
    pub prompt: Option<String>,
    /// The exact prompt sent to the child (system instructions of the profile included).
    pub final_prompt: Option<String>,
    pub parent_thread_id: Option<i64>,
    pub parent_tab: Option<String>,
    pub depth: Option<u32>,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
    pub deadline_ms: Option<i64>,
    pub status: Option<RunStatus>,
    pub applied_ms: Option<i64>,
    pub profile: Option<String>,
    /// `cli` or `mcp`.
    pub launcher: Option<String>,
    /// Limits the policy applied, as the harness recorded them: `(key, value)` scalars.
    pub effective_limits: Vec<(String, String)>,
    /// `limits.approvals` of the harness record: `ask | allowlist | auto` (the mode in force).
    pub approval_mode: Option<String>,
    /// `limits.auto_approved`: commands the harness approved without asking (allowlist or auto).
    pub auto_approved: Option<u32>,
    /// `limits.blocked`: requests denied by the harness' fixed blocklist without asking.
    pub blocked: Option<u32>,
    pub approvals: Vec<ApprovalEntry>,
    pub result: Option<ResultInfo>,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

fn i(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| {
        x.as_i64()
            .or_else(|| x.as_f64().map(|f| f as i64))
            .or_else(|| x.as_str().and_then(|t| t.trim().parse().ok()))
    })
}

fn u(v: &Value, key: &str) -> Option<u64> {
    i(v, key).and_then(|n| u64::try_from(n).ok())
}

fn p(v: &Value, key: &str) -> Option<PathBuf> {
    s(v, key).map(PathBuf::from)
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(t) => Some(t.clone()),
        Value::Bool(b) => Some(if *b { "sí".into() } else { "no".into() }),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().filter_map(scalar).collect();
            (!items.is_empty()).then(|| items.join(", "))
        }
        _ => None,
    }
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_diff(v: &Value) -> Option<DiffStatInfo> {
    let d = v.get("diff_stat")?;
    d.is_object().then(|| DiffStatInfo {
        files: u(d, "files").unwrap_or(0) as usize,
        insertions: u(d, "insertions").unwrap_or(0) as usize,
        deletions: u(d, "deletions").unwrap_or(0) as usize,
        text: d
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn parse_result(v: &Value) -> Option<ResultInfo> {
    let r = v.get("result")?;
    if !r.is_object() {
        return None;
    }
    let usage = r.get("usage");
    let cost = r.get("cost");
    Some(ResultInfo {
        summary: s(r, "summary"),
        files_changed: strings(r, "files_changed"),
        diff_stat: parse_diff(r),
        billable_tokens: usage.and_then(|x| u(x, "billable")),
        total_tokens: usage.and_then(|x| u(x, "total")),
        cost_usd: cost.and_then(|c| c.get("usd")).and_then(Value::as_f64),
        cost_exact: cost.and_then(|c| c.get("exact")).and_then(Value::as_bool),
        worktree_path: p(r, "worktree_path"),
        reason: s(r, "reason"),
        notes: strings(r, "notes"),
    })
}

fn parse_approvals(v: &Value) -> Vec<ApprovalEntry> {
    v.get("approvals")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|x| x.is_object())
                .map(|x| ApprovalEntry {
                    tool: s(x, "tool").unwrap_or_default(),
                    detail: s(x, "detail").unwrap_or_default(),
                    decision: s(x, "decision").unwrap_or_else(|| "pending".to_string()),
                    at_ms: i(x, "decided_ms").or_else(|| i(x, "at_ms")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Keys of the harness's `limits` object that the inspector shows, in display order (the rest,
/// such as `parent_permission` or `tools_enforced`, are bookkeeping).
const SHOWN_LIMITS: [&str; 6] = [
    "permission",
    "depth",
    "timeout_secs",
    "budget_tokens",
    "worktree",
    "approvals",
];

fn parse_limits(v: &Value) -> Vec<(String, String)> {
    if let Some(obj) = v.get("effective_limits").and_then(Value::as_object) {
        return obj
            .iter()
            .filter_map(|(k, val)| scalar(val).map(|t| (k.clone(), t)))
            .collect();
    }
    // What agent_harness actually writes (`RunRecord.limits`).
    let Some(obj) = v.get("limits").and_then(Value::as_object) else {
        return Vec::new();
    };
    SHOWN_LIMITS
        .iter()
        .filter_map(|k| obj.get(*k).and_then(scalar).map(|t| ((*k).to_string(), t)))
        .collect()
}

impl RunInfo {
    /// Parses one registry file. `None` when it is not an object or names no run.
    pub fn from_value(v: &Value) -> Option<RunInfo> {
        if !v.is_object() {
            return None;
        }
        let thread_id = i(v, "thread_id");
        let run_id = s(v, "run_id").or_else(|| thread_id.map(|t| t.to_string()))?;
        let thread_id = thread_id.or_else(|| run_id.parse().ok());
        let result = parse_result(v);
        let mut status = s(v, "status").and_then(|t| RunStatus::parse(&t));
        if status.is_none() && result.is_some() && i(v, "finished_ms").is_some() {
            status = Some(RunStatus::Done);
        }
        let mut approvals = parse_approvals(v);
        if approvals.is_empty()
            && let Some(r) = v.get("result")
        {
            approvals = parse_approvals(r);
        }
        Some(RunInfo {
            run_id,
            thread_id,
            agent: s(v, "agent"),
            model: s(v, "model"),
            effort: s(v, "effort"),
            permission: s(v, "permission"),
            cwd: p(v, "cwd"),
            project_root: p(v, "project_root"),
            worktree_path: p(v, "worktree_path"),
            prompt: s(v, "prompt"),
            final_prompt: s(v, "final_prompt"),
            parent_thread_id: i(v, "parent_thread_id"),
            parent_tab: s(v, "parent_tab"),
            depth: u(v, "depth").map(|d| d as u32),
            started_ms: i(v, "started_ms"),
            finished_ms: i(v, "finished_ms"),
            deadline_ms: i(v, "deadline_ms"),
            status,
            applied_ms: i(v, "applied_ms"),
            profile: s(v, "profile"),
            launcher: s(v, "launcher"),
            effective_limits: parse_limits(v),
            approval_mode: v.get("limits").and_then(|l| s(l, "approvals")),
            auto_approved: v
                .get("limits")
                .and_then(|l| u(l, "auto_approved"))
                .map(|n| n.min(u64::from(u32::MAX)) as u32),
            blocked: v
                .get("limits")
                .and_then(|l| u(l, "blocked"))
                .map(|n| n.min(u64::from(u32::MAX)) as u32),
            approvals,
            result,
        })
    }

    pub fn from_json(text: &str) -> Option<RunInfo> {
        RunInfo::from_value(&serde_json::from_str(text).ok()?)
    }

    /// Where the run's changes live: the worktree of the record, else the one of the result.
    pub fn worktree(&self) -> Option<&Path> {
        self.worktree_path.as_deref().or_else(|| {
            self.result
                .as_ref()
                .and_then(|r| r.worktree_path.as_deref())
        })
    }
}

/// Reads every `*.json` of `dir` (cancel markers and temp files are ignored), newest first.
/// A missing folder is an empty registry.
pub fn read_runs(dir: &Path) -> Vec<RunInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<RunInfo> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|path| path.extension().is_some_and(|x| x == "json"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| RunInfo::from_json(&text))
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.started_ms.unwrap_or(0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_record_parses() {
        let r = RunInfo::from_json(
            r#"{"run_id":"12","thread_id":12,"agent":"codex","model":"gpt-5","permission":"read-only",
            "cwd":"C:/p","project_root":"C:/p","worktree_path":"C:/p.wt/x","prompt":"hola",
            "parent_thread_id":3,"depth":1,"started_ms":1000,"finished_ms":5000,"status":"done",
            "result":{"summary":"ok","files_changed":["a.rs","b.rs"],
              "diff_stat":{"files":2,"insertions":5,"deletions":1,"text":"a"},
              "usage":{"input":1,"output":2,"total":9,"billable":3},
              "cost":{"usd":0.25,"exact":true,"source":"reported"},"notes":["n"]},
            "profile":"Tests","final_prompt":"fp","launcher":"mcp",
            "effective_limits":{"permission":"read-only","timeout_secs":900,"budget_tokens":null}}"#,
        )
        .unwrap();
        assert_eq!(r.run_id, "12");
        assert_eq!(r.status, Some(RunStatus::Done));
        assert_eq!(r.profile.as_deref(), Some("Tests"));
        assert_eq!(r.launcher.as_deref(), Some("mcp"));
        assert_eq!(r.final_prompt.as_deref(), Some("fp"));
        assert_eq!(r.effective_limits.len(), 2, "null limits are skipped");
        let res = r.result.unwrap();
        assert_eq!(res.files_changed.len(), 2);
        assert_eq!(res.diff_stat.unwrap().insertions, 5);
        assert_eq!(res.billable_tokens, Some(3));
        assert_eq!(res.cost_usd, Some(0.25));
    }

    #[test]
    fn partial_and_odd_records_do_not_fail() {
        // only the id; numbers as strings; unknown status; wrong types
        let r = RunInfo::from_json(
            r#"{"run_id":"7","started_ms":"42","status":"weird","depth":"x","result":5}"#,
        )
        .unwrap();
        assert_eq!(r.thread_id, Some(7));
        assert_eq!(r.started_ms, Some(42));
        assert_eq!(r.status, None);
        assert_eq!(r.depth, None);
        assert!(r.result.is_none());
        // thread id only
        assert_eq!(
            RunInfo::from_json(r#"{"thread_id":9}"#).unwrap().run_id,
            "9"
        );
        // not a run
        assert!(RunInfo::from_json(r#"{"agent":"codex"}"#).is_none());
        assert!(RunInfo::from_json("[1]").is_none());
        assert!(RunInfo::from_json("{not json").is_none());
    }

    #[test]
    fn a_finished_record_without_status_counts_as_done() {
        let r = RunInfo::from_json(r#"{"run_id":"1","finished_ms":9,"result":{"summary":"x"}}"#)
            .unwrap();
        assert_eq!(r.status, Some(RunStatus::Done));
    }

    #[test]
    fn read_runs_skips_junk_and_sorts_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let w = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        w("1.json", r#"{"run_id":"1","started_ms":100}"#);
        w("2.json", r#"{"run_id":"2","started_ms":200}"#);
        w("3.json", "{broken");
        w("2.cancel", "cancel");
        w("4.json.tmp", r#"{"run_id":"4"}"#);
        let ids: Vec<_> = read_runs(dir.path())
            .into_iter()
            .map(|r| r.run_id)
            .collect();
        assert_eq!(ids, vec!["2", "1"]);
        assert!(read_runs(&dir.path().join("nope")).is_empty());
    }
}
