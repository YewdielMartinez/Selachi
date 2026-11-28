//! The result of a run (what `selachi-agent run --json` prints) and its text rendering.
//!
//! ```json
//! {
//!   "run_id": "12", "status": "done", "summary": "...",
//!   "files_changed": ["src/a.rs"],
//!   "diff_stat": {"files": 1, "insertions": 3, "deletions": 1, "text": " src/a.rs | 4 +++-\n 1 file changed, ..."},
//!   "usage": {"input": 10, "output": 20, "cache_create": 0, "cache_read": 0, "total": 30, "billable": 30},
//!   "cost": {"usd": 0.01, "exact": false, "source": "estimated"},
//!   "worktree_path": "C:\\repo.worktrees\\fix-tests"
//! }
//! ```
//!
//! Extra fields (`agent`, `permission`, `thread_id`, `reason`, `code`, `notes`, `profile`) are additive.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::policy::{Refusal, RefusalCode};

/// Lifecycle of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// In flight (the harness process is alive).
    Running,
    /// The agent finished its turn.
    Done,
    /// The agent failed (could not start, crashed, reported an error).
    Error,
    /// `selachi-agent cancel`.
    Cancelled,
    /// The timeout elapsed; the agent was interrupted.
    TimedOut,
    /// `--budget-tokens` was exceeded; the agent was interrupted.
    BudgetExceeded,
    /// A policy rule stopped it before it started (no thread was created).
    Refused,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Done => "done",
            RunStatus::Error => "error",
            RunStatus::Cancelled => "cancelled",
            RunStatus::TimedOut => "timed_out",
            RunStatus::BudgetExceeded => "budget_exceeded",
            RunStatus::Refused => "refused",
        }
    }

    pub fn is_final(self) -> bool {
        self != RunStatus::Running
    }

    /// Process exit code of `selachi-agent run`.
    pub fn exit_code(self) -> i32 {
        match self {
            RunStatus::Done | RunStatus::Running => 0,
            RunStatus::Error => 1,
            RunStatus::Refused => 3,
            RunStatus::TimedOut => 4,
            RunStatus::Cancelled => 5,
            RunStatus::BudgetExceeded => 6,
        }
    }
}

/// Totals of `git diff --stat` style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
    /// Per-file lines plus the summary, ready to print ("" when nothing changed).
    pub text: String,
}

/// Token usage of the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageOut {
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    /// Everything, cache reads included.
    pub total: u64,
    /// `input + output + cache_create`: what the budgets count (cache reads are nearly free).
    pub billable: u64,
}

impl UsageOut {
    pub fn new(input: u64, output: u64, cache_create: u64, cache_read: u64) -> UsageOut {
        UsageOut {
            input,
            output,
            cache_create,
            cache_read,
            total: input + output + cache_create + cache_read,
            billable: input + output + cache_create,
        }
    }
}

/// Cost of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostOut {
    pub usd: f64,
    /// Every cent was reported by the agent itself.
    pub exact: bool,
    /// `reported`, `estimated` (local price table) or `none`.
    pub source: String,
}

impl Default for CostOut {
    fn default() -> Self {
        CostOut {
            usd: 0.0,
            exact: false,
            source: "none".into(),
        }
    }
}

/// What `run` returns and `result` replays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// The thread id of the child thread as text; `null` when the run was refused.
    pub run_id: Option<String>,
    pub status: RunStatus,
    /// Final answer of the agent (redacted, capped), or the reason when there is none.
    pub summary: String,
    pub files_changed: Vec<String>,
    pub diff_stat: DiffStat,
    pub usage: UsageOut,
    pub cost: CostOut,
    /// Where the changes live when the run used `--worktree`.
    pub worktree_path: Option<String>,
    // ---- additive fields ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    /// Why it did not finish normally (refusal, error, timeout...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<RefusalCode>,
    /// Things worth knowing (an approval that was refused, a redaction, a checkpoint that failed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Name of the subagent profile the run used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Why `permission` is what it is (default, caller's cap, parent thread's current permission,
    /// profile), so the caller does not retry blindly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_reason: Option<String>,
}

impl RunResult {
    /// Skeleton with nothing filled in.
    pub fn empty(status: RunStatus) -> RunResult {
        RunResult {
            run_id: None,
            status,
            summary: String::new(),
            files_changed: Vec::new(),
            diff_stat: DiffStat::default(),
            usage: UsageOut::default(),
            cost: CostOut::default(),
            worktree_path: None,
            agent: None,
            permission: None,
            thread_id: None,
            reason: None,
            code: None,
            notes: Vec::new(),
            profile: None,
            permission_reason: None,
        }
    }

    pub fn refused(r: &Refusal) -> RunResult {
        RunResult {
            summary: format!("refused: {}", r.reason),
            reason: Some(r.reason.clone()),
            code: Some(r.code),
            ..RunResult::empty(RunStatus::Refused)
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|e| format!("{{\"status\":\"error\",\"reason\":\"{e}\"}}"))
    }

    /// Human-readable rendering (the default output of `run` / `result`).
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let id = self.run_id.as_deref().unwrap_or("-");
        let agent = self.agent.as_deref().unwrap_or("?");
        let _ = match &self.profile {
            Some(p) => writeln!(
                s,
                "run {id} · {} · {agent} · profile {p}",
                self.status.as_str()
            ),
            None => writeln!(s, "run {id} · {} · {agent}", self.status.as_str()),
        };
        if let (Some(p), Some(why)) = (&self.permission, &self.permission_reason) {
            let _ = writeln!(s, "permission: {p} ({why})");
        }
        if let Some(reason) = &self.reason {
            let _ = writeln!(s, "reason: {reason}");
        }
        if !self.summary.is_empty()
            && self.reason.as_deref() != Some(self.summary.as_str())
            && self.status != RunStatus::Refused
        {
            let _ = writeln!(s, "\n{}\n", self.summary.trim_end());
        }
        if !self.files_changed.is_empty() {
            let _ = writeln!(s, "files changed ({}):", self.files_changed.len());
            for f in &self.files_changed {
                let _ = writeln!(s, "  {f}");
            }
            if !self.diff_stat.text.is_empty() {
                let _ = writeln!(s, "{}", self.diff_stat.text.trim_end());
            }
        }
        if self.status != RunStatus::Refused {
            let _ = writeln!(
                s,
                "usage: {} tokens ({} billable) · cost ${:.4} ({})",
                self.usage.total, self.usage.billable, self.cost.usd, self.cost.source
            );
        }
        if let Some(wt) = &self.worktree_path {
            let _ = writeln!(s, "worktree: {wt}");
            if let Some(id) = &self.run_id {
                let _ = writeln!(
                    s,
                    "next: selachi-agent diff {id} | selachi-agent apply {id}"
                );
            }
        }
        for n in &self.notes {
            let _ = writeln!(s, "note: {n}");
        }
        s
    }
}

/// Cap `text` to `max` characters, marking the cut.
pub fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("\n[...truncated; see the thread for the full answer]");
    out
}

/// `git diff --stat` style summary of a unified diff (rename-free, as `git_svc` produces it).
pub fn diff_stat_of(diff: &str) -> DiffStat {
    let mut files: Vec<(String, usize, usize)> = Vec::new();
    let mut old_left = 0usize;
    let mut new_left = 0usize;
    for line in diff.lines() {
        if old_left > 0 || new_left > 0 {
            // inside a hunk: count by the header's line numbers so `--- x` / `+++ x` content lines
            // are not mistaken for file headers
            match line.as_bytes().first() {
                Some(b'+') => {
                    new_left = new_left.saturating_sub(1);
                    if let Some(f) = files.last_mut() {
                        f.1 += 1;
                    }
                }
                Some(b'-') => {
                    old_left = old_left.saturating_sub(1);
                    if let Some(f) = files.last_mut() {
                        f.2 += 1;
                    }
                }
                Some(b'\\') => {}
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/<path> b/<path>`: the path is what follows the last ` b/`
            let path = rest.rsplit_once(" b/").map(|(_, p)| p).unwrap_or(rest);
            files.push((path.trim_matches('"').to_string(), 0, 0));
        } else if line.starts_with("@@") {
            // @@ -a,b +c,d @@
            let mut parts = line.split_whitespace();
            let _ = parts.next();
            let count = |p: Option<&str>| -> usize {
                p.map(|p| p.trim_start_matches(['-', '+']))
                    .map(|p| p.split_once(',').map(|(_, n)| n).unwrap_or("1"))
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1)
            };
            old_left = count(parts.next());
            new_left = count(parts.next());
        }
    }
    let insertions: usize = files.iter().map(|f| f.1).sum();
    let deletions: usize = files.iter().map(|f| f.2).sum();
    let mut text = String::new();
    if !files.is_empty() {
        let width = files
            .iter()
            .map(|f| f.0.chars().count())
            .max()
            .unwrap_or(0)
            .min(60);
        let max_changes = files.iter().map(|f| f.1 + f.2).max().unwrap_or(0).max(1);
        for (path, ins, del) in &files {
            let n = ins + del;
            let bar_len = if max_changes > 40 {
                (n * 40).div_ceil(max_changes)
            } else {
                n
            };
            let plus = if n == 0 {
                0
            } else {
                (ins * bar_len).div_ceil(n).min(bar_len)
            };
            let minus = bar_len - plus;
            let _ = writeln!(
                text,
                " {path:<width$} | {n:>4} {}{}",
                "+".repeat(plus),
                "-".repeat(minus)
            );
        }
        let _ = write!(
            text,
            " {} file{} changed, {insertions} insertion{}(+), {deletions} deletion{}(-)",
            files.len(),
            if files.len() == 1 { "" } else { "s" },
            if insertions == 1 { "" } else { "s" },
            if deletions == 1 { "" } else { "s" },
        );
    }
    DiffStat {
        files: files.len(),
        insertions,
        deletions,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/a.txt b/a.txt
index 111..222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,4 @@
 alpha
-beta
+BETA
+gamma
 delta
diff --git a/new.rs b/new.rs
new file mode 100644
index 0000000..333
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+fn a() {}
+// -- not a header
";

    #[test]
    fn stat_counts_hunk_lines_only() {
        let s = diff_stat_of(DIFF);
        assert_eq!((s.files, s.insertions, s.deletions), (2, 4, 1));
        assert!(s.text.contains("a.txt"));
        assert!(
            s.text
                .contains("2 files changed, 4 insertions(+), 1 deletion(-)"),
            "{}",
            s.text
        );
        assert_eq!(diff_stat_of(""), DiffStat::default());
    }

    #[test]
    fn stat_does_not_mistake_deleted_dashes_for_headers() {
        let d = "diff --git a/x.sql b/x.sql\n--- a/x.sql\n+++ b/x.sql\n@@ -1,2 +1,1 @@\n--- a comment\n keep\n";
        let s = diff_stat_of(d);
        assert_eq!((s.files, s.insertions, s.deletions), (1, 0, 1));
    }

    #[test]
    fn result_json_has_the_documented_fields() {
        let mut r = RunResult::empty(RunStatus::Done);
        r.run_id = Some("12".into());
        r.summary = "ok".into();
        r.usage = UsageOut::new(1, 2, 3, 4);
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        for k in [
            "run_id",
            "status",
            "summary",
            "files_changed",
            "diff_stat",
            "usage",
            "cost",
            "worktree_path",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert_eq!(v["status"], "done");
        assert_eq!(v["usage"]["total"], 10);
        assert_eq!(v["usage"]["billable"], 6);
        assert!(v["worktree_path"].is_null());
        assert!(
            v.get("reason").is_none(),
            "optional fields are omitted when empty"
        );
        let back: RunResult = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn refused_result_and_exit_codes() {
        let r = RunResult::refused(&Refusal::new(RefusalCode::Concurrency, "3 runs in flight"));
        assert_eq!(r.status, RunStatus::Refused);
        assert!(r.run_id.is_none());
        assert_eq!(r.status.exit_code(), 3);
        assert!(r.to_text().contains("3 runs in flight"));
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(v["code"], "concurrency");
        assert!(v["run_id"].is_null());
    }

    #[test]
    fn caps_long_text() {
        assert_eq!(cap_chars("short", 10), "short");
        assert!(cap_chars(&"x".repeat(50), 10).starts_with("xxxxxxxxxx\n[...truncated"));
    }
}
