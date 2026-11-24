//! `apply`: bring the changes of a worktree run into the project.
//!
//! Never silent. [`Harness::apply_plan`] computes and returns exactly what would happen (files, diff
//! stat, whether the patch applies cleanly, which of the project's uncommitted files it overlaps,
//! and the risk level); [`Harness::apply`] needs a [`Confirmation`] and refuses `--yes` style
//! confirmation for Full-risk operations. The CLI shows the plan and asks on a terminal.
//!
//! The patch is `git diff` between the pre-run checkpoint and the worktree as it is now
//! (untracked files included), applied with `git apply` to the project's working tree: nothing is
//! committed or staged. Limitation: binary files are not carried (the diff is text only).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git_svc::DeltaKind;
use serde::Serialize;

use crate::output::{diff_stat_of, DiffStat, RunStatus};
use crate::run::Harness;

/// How dangerous applying is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Normal,
    /// Operations that must be confirmed by a person at a terminal: `--yes` is refused.
    Full,
}

/// One file of the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanFile {
    pub path: String,
    /// `added | modified | deleted | type_changed`.
    pub change: &'static str,
}

/// What `apply` would do.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyPlan {
    pub run_id: String,
    /// Where the changes come from.
    pub source: String,
    /// Where they would go.
    pub target: String,
    pub files: Vec<PlanFile>,
    pub diff_stat: DiffStat,
    pub risk: Risk,
    pub risk_reasons: Vec<String>,
    /// `git apply --check` accepts the patch in the target.
    pub applies_cleanly: bool,
    /// Why it does not (git's message).
    pub conflict: Option<String>,
    /// Files the plan touches that also have uncommitted changes in the target (they would be mixed).
    pub overlaps_uncommitted: Vec<String>,
    pub already_applied: bool,
    /// Nothing to bring over.
    pub empty: bool,
    /// Always `true`: `apply` is never silent.
    pub requires_confirmation: bool,
    /// Nothing blocks it (applies cleanly, not empty, not applied yet).
    pub can_apply: bool,
    pub dry_run: bool,
}

impl ApplyPlan {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Preview shown before asking (redaction is the caller's: the plan has paths only).
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "apply run {}: {} -> {}",
            self.run_id, self.source, self.target
        );
        if self.files.is_empty() {
            let _ = writeln!(s, "  nothing to apply");
        }
        for f in &self.files {
            let _ = writeln!(s, "  {:<12} {}", f.change, f.path);
        }
        if !self.diff_stat.text.is_empty() {
            let _ = writeln!(s, "{}", self.diff_stat.text.trim_end());
        }
        if self.already_applied {
            let _ = writeln!(s, "WARNING: this run was already applied");
        }
        if !self.overlaps_uncommitted.is_empty() {
            let _ = writeln!(
                s,
                "WARNING: these files also have uncommitted changes in the project: {}",
                self.overlaps_uncommitted.join(", ")
            );
        }
        if let Some(c) = &self.conflict {
            let _ = writeln!(s, "BLOCKED: the patch does not apply cleanly: {c}");
        }
        for r in &self.risk_reasons {
            let _ = writeln!(s, "RISK (full): {r}");
        }
        s
    }
}

/// How the person confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Typed on a terminal after seeing the preview.
    Interactive,
    /// `--yes`: allowed for normal-risk plans only.
    Flag,
    /// A person pressed "allow" in a Selachi window that showed the plan (MCP `apply_changes`).
    /// Like `Flag`, never accepted for Full-risk plans: those need a terminal.
    Gui,
}

/// What `apply` did.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub run_id: String,
    pub target: String,
    pub files: Vec<PlanFile>,
    pub diff_stat: DiffStat,
}

fn change_name(k: &DeltaKind) -> &'static str {
    match k {
        DeltaKind::Added => "added",
        DeltaKind::Deleted => "deleted",
        DeltaKind::TypeChanged => "type_changed",
        _ => "modified",
    }
}

fn git(target: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::io::Result<(bool, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(target)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    agents_driver::proc::hide_window(&mut cmd);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A write error here means git already rejected the input; its message is on stderr.
        let _ = pipe.write_all(data);
    }
    let out = child.wait_with_output()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok((out.status.success(), text.trim().to_string()))
}

fn sensitive_reason(path: &str) -> Option<String> {
    let p = path.replace('\\', "/");
    let lower = p.to_ascii_lowercase();
    if p.split('/').any(|c| c == "..") || p.starts_with('/') || p.chars().nth(1) == Some(':') {
        return Some(format!("{path} points outside the project"));
    }
    if lower == ".git" || lower.starts_with(".git/") || lower.contains("/.git/") {
        return Some(format!("{path} is inside .git"));
    }
    if lower.starts_with(".github/workflows/") {
        return Some(format!(
            "{path} is a CI workflow (it runs with your repository's secrets)"
        ));
    }
    None
}

impl Harness {
    /// Compute the plan (read-only: nothing in the project changes).
    pub fn apply_plan(&self, run_id: &str) -> Result<ApplyPlan, String> {
        let rec = self.record(run_id)?;
        if !rec.status.is_final() {
            return Err(format!(
                "run {run_id} is still running; apply it when it has finished"
            ));
        }
        if rec.status == RunStatus::Refused {
            return Err(format!(
                "run {run_id} was refused; there is nothing to apply"
            ));
        }
        let Some(worktree) = rec.worktree_path.clone() else {
            return Err(format!(
                "run {run_id} did not use --worktree: the agent edited {} directly, so there is nothing to apply",
                rec.cwd.display()
            ));
        };
        let Some(cp) = rec.checkpoint_id.clone() else {
            return Err(format!(
                "run {run_id} has no checkpoint, so its changes cannot be extracted"
            ));
        };
        if !worktree.is_dir() {
            return Err(format!(
                "the worktree {} no longer exists",
                worktree.display()
            ));
        }
        let source = git_svc::Repo::discover(&worktree)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{} is not a git repository any more", worktree.display()))?;
        let target = git_svc::Repo::discover(&rec.project_root)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!(
                    "{} is not a git repository any more",
                    rec.project_root.display()
                )
            })?;

        let changes = source.changed_files_since(&cp).map_err(|e| e.to_string())?;
        let patch = source
            .diff_since_checkpoint(&cp)
            .map_err(|e| e.to_string())?;
        let files: Vec<PlanFile> = changes
            .iter()
            .map(|c| PlanFile {
                path: c.path.to_string_lossy().replace('\\', "/"),
                change: change_name(&c.kind),
            })
            .collect();

        let mut risk_reasons: Vec<String> = Vec::new();
        if rec.permission == "full" {
            risk_reasons.push("the run had the full permission".into());
        }
        risk_reasons.extend(files.iter().filter_map(|f| sensitive_reason(&f.path)));
        let risk = if risk_reasons.is_empty() {
            Risk::Normal
        } else {
            Risk::Full
        };

        let (applies_cleanly, conflict) = if patch.trim().is_empty() {
            (true, None)
        } else {
            match git(
                &target.root,
                &["apply", "--check", "--whitespace=nowarn", "-"],
                Some(patch.as_bytes()),
            ) {
                Ok((true, _)) => (true, None),
                Ok((false, msg)) => (false, Some(msg)),
                Err(e) => (false, Some(format!("cannot run git: {e}"))),
            }
        };

        let overlaps_uncommitted = match target.status() {
            Ok(st) => {
                let mut dirty: Vec<String> = st
                    .staged
                    .iter()
                    .chain(st.unstaged.iter())
                    .map(|c| c.path.to_string_lossy().replace('\\', "/"))
                    .chain(
                        st.untracked
                            .iter()
                            .map(|p| p.to_string_lossy().replace('\\', "/")),
                    )
                    .collect();
                dirty.sort();
                dirty.dedup();
                files
                    .iter()
                    .filter(|f| dirty.contains(&f.path))
                    .map(|f| f.path.clone())
                    .collect()
            }
            Err(_) => Vec::new(),
        };

        let already_applied = rec.applied_ms.is_some();
        let empty = files.is_empty() || patch.trim().is_empty();
        Ok(ApplyPlan {
            run_id: rec.run_id,
            source: store::paths::clean_display(&worktree.to_string_lossy()),
            target: store::paths::clean_display(&target.root.to_string_lossy()),
            diff_stat: diff_stat_of(&patch),
            files,
            risk,
            risk_reasons,
            applies_cleanly,
            conflict,
            overlaps_uncommitted,
            already_applied,
            empty,
            requires_confirmation: true,
            can_apply: applies_cleanly && !empty && !already_applied,
            dry_run: false,
        })
    }

    /// Apply the plan to the project's working tree. Refuses a `Flag` confirmation for Full-risk plans.
    pub fn apply(&self, run_id: &str, confirmation: Confirmation) -> Result<ApplyReport, String> {
        let plan = self.apply_plan(run_id)?;
        if plan.risk == Risk::Full && confirmation != Confirmation::Interactive {
            return Err(format!(
                "--yes is not allowed for a full-risk apply ({}); run `selachi-agent apply {run_id}` on a terminal and confirm there",
                plan.risk_reasons.join("; ")
            ));
        }
        if plan.already_applied {
            return Err(format!("run {run_id} was already applied"));
        }
        if plan.empty {
            return Err(format!(
                "run {run_id} changed nothing; there is nothing to apply"
            ));
        }
        if !plan.applies_cleanly {
            return Err(format!(
                "the patch does not apply cleanly to {}: {}",
                plan.target,
                plan.conflict.clone().unwrap_or_default()
            ));
        }
        let patch = self.diff(run_id)?;
        let target = PathBuf::from(&plan.target);
        match git(
            &target,
            &["apply", "--whitespace=nowarn", "-"],
            Some(patch.as_bytes()),
        ) {
            Ok((true, _)) => {}
            Ok((false, msg)) => return Err(format!("git apply failed: {msg}")),
            Err(e) => return Err(format!("cannot run git: {e}")),
        }
        let mut rec = self.registry.load(run_id)?;
        rec.applied_ms = Some(chat_core::now_ms());
        if let Err(e) = self.registry.save(&rec) {
            log::warn!("agent_harness: applied run {run_id} but could not record it: {e}");
        }
        Ok(ApplyReport {
            run_id: plan.run_id,
            target: plan.target,
            files: plan.files,
            diff_stat: plan.diff_stat,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_paths() {
        assert!(sensitive_reason("src/a.rs").is_none());
        assert!(sensitive_reason("docs/.github-notes.md").is_none());
        assert!(sensitive_reason(".git/hooks/pre-commit")
            .unwrap()
            .contains(".git"));
        assert!(sensitive_reason("sub/.git/config").is_some());
        assert!(sensitive_reason(".github/workflows/ci.yml")
            .unwrap()
            .contains("CI"));
        assert!(sensitive_reason("../outside.txt")
            .unwrap()
            .contains("outside"));
        assert!(sensitive_reason("C:/Windows/x").is_some());
        assert!(sensitive_reason(".github/ISSUE_TEMPLATE/bug.md").is_none());
    }
}
