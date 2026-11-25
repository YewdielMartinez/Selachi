//! What the subagent map needs from the app (PLAN section 21.2). The map itself only emits
//! `AgentMapEvent`s; this module holds the pure parts of answering them so they can be tested
//! without a window:
//!
//! * [`LazySource`]: the map's `SessionSource` over a chat database that opens lazily (a session
//!   without chat never opens `chat.db`; the source answers "nothing yet" until the hub is ready);
//! * [`retry_plan`]: the `selachi-agent run` command line of "Reintentar" (a new run, never an edit
//!   of the old one);
//! * [`ApplyView`]: the plan of "Aplicar cambios" in the words of the confirmation dialog;
//! * [`profile_from_seed`]: "Duplicar como perfil".

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use agent_harness::{ApplyPlan, Risk, RunRecord};
use chat_core::{ThreadId, ThreadRecord, ThreadUpdate};
use futures::channel::mpsc::UnboundedReceiver;
use ui_agentmap::{ApprovalEntry, ChatStoreSource, ProfileSeed, SessionSource};
use ui_settings::Profile;

/// A [`SessionSource`] whose chat store arrives later. Cheap to clone (it is an `Arc` inside).
#[derive(Clone, Default)]
pub struct LazySource {
    inner: Arc<RwLock<Option<ChatStoreSource>>>,
}

impl LazySource {
    /// The chat database is open: from now on the map reads real threads.
    pub fn set(&self, source: ChatStoreSource) {
        if let Ok(mut g) = self.inner.write() {
            *g = Some(source);
        }
    }

    fn with<T>(&self, f: impl FnOnce(&ChatStoreSource) -> T) -> Option<T> {
        self.inner.read().ok().and_then(|g| g.as_ref().map(f))
    }
}

impl SessionSource for LazySource {
    fn threads(&self) -> Vec<ThreadRecord> {
        self.with(|s| s.threads()).unwrap_or_default()
    }

    fn first_user_message(&self, id: ThreadId) -> Option<String> {
        self.with(|s| s.first_user_message(id)).flatten()
    }

    fn approvals(&self, id: ThreadId) -> Vec<ApprovalEntry> {
        self.with(|s| s.approvals(id)).unwrap_or_default()
    }

    fn updates(&self) -> Option<UnboundedReceiver<ThreadUpdate>> {
        self.with(|s| s.updates()).flatten()
    }
}

// ---------------------------------------------------------------------------------------------
// Retry
// ---------------------------------------------------------------------------------------------

/// A `selachi-agent` invocation: arguments, the environment that links the new run to the same
/// parent, and the prompt (sent on stdin so a long one never meets the command-line limit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPlan {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: String,
    pub cwd: PathBuf,
}

/// "Reintentar" with `agent` (and `model`): the same task in a **new worktree**, so the first
/// attempt stays untouched and can still be compared or applied. The permission is the one the run
/// had (never more), the parent link and depth are kept, and a profile is kept too.
pub fn retry_plan(rec: &RunRecord, agent: &str, model: Option<&str>) -> RetryPlan {
    let mut args: Vec<String> = vec!["run".into(), "--agent".into(), agent.to_string()];
    if let Some(p) = rec.profile.as_deref().filter(|p| !p.trim().is_empty()) {
        args.extend(["--profile".into(), p.to_string()]);
    }
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        args.extend(["--model".into(), m.to_string()]);
    }
    args.extend([
        "--permission".into(),
        rec.permission.clone(),
        "--cwd".into(),
        rec.project_root.to_string_lossy().into_owned(),
        "--worktree".into(),
        "-".into(),
    ]);
    let mut env = vec![
        (
            "SELACHI_PARENT_PERMISSION".to_string(),
            rec.permission.clone(),
        ),
        (
            "SELACHI_DELEGATION_DEPTH".to_string(),
            rec.depth.saturating_sub(1).to_string(),
        ),
    ];
    if let Some(t) = rec.parent_thread_id {
        env.push(("SELACHI_PARENT_THREAD".to_string(), t.to_string()));
    }
    RetryPlan {
        args,
        env,
        stdin: rec.prompt.clone(),
        cwd: rec.project_root.clone(),
    }
}

/// Starts the run as its own process (it outlives the window, writes its record in `agent-runs`
/// and the map hears about it from the folder watcher). Returns before the run ends; a thread waits
/// for the exit so no zombie is left. Errors are about starting only.
pub fn spawn_retry(exe: &std::path::Path, plan: &RetryPlan) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Stdio};
    /// `CREATE_NO_WINDOW`: no console flashes on screen.
    const NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(exe);
    cmd.args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(NO_WINDOW);
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn()?;
    let stdin = plan.stdin.clone();
    std::thread::Builder::new()
        .name("selachi-retry".into())
        .spawn(move || {
            if let Some(mut pipe) = child.stdin.take() {
                let _ = pipe.write_all(stdin.as_bytes());
            }
            let _ = child.wait();
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------------------------

/// What the confirmation dialog of "Aplicar cambios" shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyView {
    pub run_id: String,
    pub target: String,
    /// `(Spanish change word, path)`.
    pub files: Vec<(&'static str, String)>,
    /// `git diff --stat` summary line(s).
    pub stat: String,
    /// Things to read before pressing the button.
    pub warnings: Vec<String>,
    /// Why the button is disabled (the patch does not apply, nothing to apply, applied already,
    /// needs a terminal), if it is.
    pub blocked: Option<String>,
}

impl ApplyView {
    pub fn can_apply(&self) -> bool {
        self.blocked.is_none()
    }
}

fn change_word(c: &str) -> &'static str {
    match c {
        "added" => "añadido",
        "deleted" => "borrado",
        "type_changed" => "tipo cambiado",
        _ => "modificado",
    }
}

/// The plan of `apply` as the dialog shows it. A `Full`-risk plan is never applied from here: the
/// harness refuses a GUI confirmation for those and says to use a terminal.
pub fn apply_view(plan: &ApplyPlan) -> ApplyView {
    let mut warnings = Vec::new();
    if !plan.overlaps_uncommitted.is_empty() {
        warnings.push(format!(
            "Estos archivos también tienen cambios sin confirmar en el proyecto y se mezclarían: {}.",
            plan.overlaps_uncommitted.join(", ")
        ));
    }
    warnings.extend(
        plan.risk_reasons
            .iter()
            .map(|r| format!("Riesgo alto: {r}.")),
    );
    let blocked = if plan.already_applied {
        Some("Estos cambios ya se aplicaron.".to_string())
    } else if plan.empty {
        Some("La ejecución no cambió nada: no hay nada que aplicar.".to_string())
    } else if !plan.applies_cleanly {
        Some(format!(
            "El parche no se aplica limpio: {}",
            plan.conflict.clone().unwrap_or_default()
        ))
    } else if plan.risk == Risk::Full {
        Some(format!(
            "Este plan es de riesgo alto: confírmalo en una terminal con `selachi agent apply {}`.",
            plan.run_id
        ))
    } else {
        None
    };
    ApplyView {
        run_id: plan.run_id.clone(),
        target: plan.target.clone(),
        files: plan
            .files
            .iter()
            .map(|f| (change_word(f.change), f.path.clone()))
            .collect(),
        stat: plan.diff_stat.text.trim_end().to_string(),
        warnings,
        blocked,
    }
}

// ---------------------------------------------------------------------------------------------
// Duplicate as profile
// ---------------------------------------------------------------------------------------------

/// A new profile that starts as the run was: agent, model, effort, permission, worktree and the
/// task as the instructions (the prompt the orchestrator sent, already redacted).
pub fn profile_from_seed(seed: &ProfileSeed, run_id: &str) -> Profile {
    let permission = match seed.permission.as_deref() {
        Some(p @ ("read-only" | "ask" | "accept-edits")) => p.to_string(),
        _ => "read-only".to_string(),
    };
    Profile {
        name: seed.name.chars().take(64).collect(),
        description: format!("Copiado de la ejecución {run_id} del mapa de subagentes."),
        agent: seed.agent.clone(),
        model: seed.model.clone(),
        effort: seed.effort.clone(),
        permission,
        worktree: seed.worktree,
        instructions: seed.prompt.clone().unwrap_or_default(),
        ..Profile::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_harness::{RunResult, RunStatus};

    fn record() -> RunRecord {
        RunRecord {
            run_id: "12".into(),
            thread_id: 12,
            agent: "codex".into(),
            model: Some("gpt-5".into()),
            permission: "ask".into(),
            cwd: "C:/work/wt".into(),
            project_root: "C:/work/proj".into(),
            worktree_path: Some("C:/work/wt".into()),
            checkpoint_id: Some("cp1".into()),
            prompt: "Escribe tests de expiración".into(),
            parent_thread_id: Some(3),
            depth: 2,
            started_ms: 0,
            finished_ms: Some(1),
            status: RunStatus::Error,
            runner_pid: 1,
            runner_start_ms: None,
            deadline_ms: 10,
            result: Some(RunResult::empty(RunStatus::Error)),
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
    fn a_retry_is_a_new_worktree_run_that_keeps_the_task_the_permission_and_the_parent() {
        let p = retry_plan(&record(), "gemini", Some("gemini-3-pro"));
        assert_eq!(
            p.args,
            [
                "run",
                "--agent",
                "gemini",
                "--model",
                "gemini-3-pro",
                "--permission",
                "ask",
                "--cwd",
                "C:/work/proj",
                "--worktree",
                "-"
            ]
        );
        assert_eq!(p.stdin, "Escribe tests de expiración");
        let env: std::collections::HashMap<_, _> = p.env.iter().cloned().collect();
        assert_eq!(env["SELACHI_PARENT_PERMISSION"], "ask");
        // the run was at depth 2: its caller was at depth 1
        assert_eq!(env["SELACHI_DELEGATION_DEPTH"], "1");
        assert_eq!(env["SELACHI_PARENT_THREAD"], "3");
        assert_eq!(p.cwd, PathBuf::from("C:/work/proj"));
        // the prompt never travels on the command line
        assert!(!p.args.iter().any(|a| a.contains("tests de")));
    }

    #[test]
    fn a_retry_keeps_the_profile_and_skips_an_empty_model() {
        let mut rec = record();
        rec.profile = Some("Tests".into());
        let p = retry_plan(&rec, "codex", Some(" "));
        assert!(p.args.windows(2).any(|w| w == ["--profile", "Tests"]));
        assert!(!p.args.contains(&"--model".to_string()));
        let mut top = record();
        top.depth = 0;
        top.parent_thread_id = None;
        let env = retry_plan(&top, "codex", None).env;
        assert!(env.contains(&("SELACHI_DELEGATION_DEPTH".into(), "0".into())));
        assert!(!env.iter().any(|(k, _)| k == "SELACHI_PARENT_THREAD"));
    }

    fn plan() -> ApplyPlan {
        use agent_harness::DiffStat;
        use agent_harness::apply::PlanFile;
        ApplyPlan {
            run_id: "12".into(),
            source: "C:/work/wt".into(),
            target: "C:/work/proj".into(),
            files: vec![
                PlanFile {
                    path: "a.rs".into(),
                    change: "modified",
                },
                PlanFile {
                    path: "b.rs".into(),
                    change: "added",
                },
            ],
            diff_stat: DiffStat {
                text: " 2 files changed
"
                .into(),
                ..DiffStat::default()
            },
            risk: Risk::Normal,
            risk_reasons: Vec::new(),
            applies_cleanly: true,
            conflict: None,
            overlaps_uncommitted: Vec::new(),
            already_applied: false,
            empty: false,
            requires_confirmation: true,
            can_apply: true,
            dry_run: false,
        }
    }

    #[test]
    fn a_clean_plan_can_be_applied_and_is_listed_in_spanish() {
        let v = apply_view(&plan());
        assert!(v.can_apply() && v.warnings.is_empty());
        assert_eq!(
            v.files,
            [
                ("modificado", "a.rs".to_string()),
                ("añadido", "b.rs".to_string())
            ]
        );
        assert_eq!(v.stat, " 2 files changed");
        assert_eq!(v.target, "C:/work/proj");
    }

    #[test]
    fn a_plan_that_cannot_be_applied_says_why_and_warns_about_mixing() {
        let mut p = plan();
        p.overlaps_uncommitted = vec!["a.rs".into()];
        let v = apply_view(&p);
        assert!(v.can_apply());
        assert!(v.warnings[0].contains("a.rs") && v.warnings[0].contains("sin confirmar"));

        let mut p = plan();
        p.applies_cleanly = false;
        p.conflict = Some("a.rs: patch does not apply".into());
        assert!(
            apply_view(&p)
                .blocked
                .unwrap()
                .contains("no se aplica limpio")
        );

        let mut p = plan();
        p.already_applied = true;
        assert!(apply_view(&p).blocked.unwrap().contains("ya se aplicaron"));

        let mut p = plan();
        p.empty = true;
        assert!(!apply_view(&p).can_apply());

        // a full-risk plan is never applied from the window: the harness needs a terminal for it
        let mut p = plan();
        p.risk = Risk::Full;
        p.risk_reasons = vec!["touches .github/workflows".into()];
        let v = apply_view(&p);
        assert!(!v.can_apply());
        assert!(v.blocked.unwrap().contains("selachi agent apply 12"));
        assert!(v.warnings[0].starts_with("Riesgo alto"));
    }

    #[test]
    fn duplicating_a_run_seeds_a_valid_profile() {
        let seed = ProfileSeed {
            name: "Revisar los tests de auth".into(),
            agent: "codex".into(),
            model: Some("gpt-5".into()),
            effort: Some("high".into()),
            permission: Some("accept-edits".into()),
            worktree: true,
            prompt: Some("Revisa los tests".into()),
        };
        let p = profile_from_seed(&seed, "12");
        assert_eq!(
            (p.agent.as_str(), p.permission.as_str()),
            ("codex", "accept-edits")
        );
        assert!(p.worktree && p.instructions == "Revisa los tests");
        assert_eq!(p.model.as_deref(), Some("gpt-5"));
        // an unknown permission falls back to the safe one; a long title is cut to 64 characters
        let odd = ProfileSeed {
            name: "x".repeat(100),
            permission: Some("full".into()),
            ..seed
        };
        let p = profile_from_seed(&odd, "13");
        assert_eq!(p.permission, "read-only");
        assert_eq!(p.name.chars().count(), 64);
    }
}
