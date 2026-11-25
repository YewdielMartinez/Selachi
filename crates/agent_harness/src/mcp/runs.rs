//! Starting delegated runs for the MCP server: one `selachi-agent run --json` **child process per
//! request**.
//!
//! Why a child process and not `Harness::run` in-process:
//!
//! * **Environment isolation.** A delegated agent must inherit `SELACHI_PARENT_PERMISSION`,
//!   `SELACHI_DELEGATION_DEPTH` and `SELACHI_PARENT_THREAD` of *its own run* (so it cannot escalate when it
//!   delegates in turn). `agents_driver` has no per-spawn environment yet, and exporting variables into the
//!   MCP server's own process is process-global and races between concurrent runs. The CLI already does
//!   exactly this once per process (`export_child_env`), so one process per run is correct by construction.
//! * **Crash and kill isolation.** A stuck or crashing run cannot take the MCP server down, and
//!   `cancel` / timeouts use the same registry + kill paths as the CLI (`runner_pid` is the child's).
//! * **One code path.** Policy, budgets, slots, redaction and the JSON schema stay in the CLI; the MCP layer
//!   only forwards and parses `RunResult`.
//!
//! The cost is a process start per delegation, negligible next to an agent run.

use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::output::{RunResult, RunStatus};
use crate::policy::ENV_CHANNEL;
use crate::registry::Registry;

use super::CallState;

/// Marker the CLI prints on stderr (`run --announce-run-id`) as soon as the run exists.
pub const RUN_ID_MARKER: &str = "SELACHI_RUN_ID=";

/// How long an `async` call waits for the run to be created before giving up.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(120);
/// After a cancel request, how long the runner gets to stop by itself before it is killed.
const CANCEL_GRACE: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

/// What `run_agent` asked for (already validated).
#[derive(Debug, Clone)]
pub struct Spec {
    /// `None` only with a profile.
    pub agent: Option<String>,
    pub profile: Option<String>,
    pub prompt: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// `None` = not given (with a profile, the profile's permission applies).
    pub permission: Option<String>,
    pub worktree: bool,
    pub timeout_s: Option<u64>,
    pub budget_tokens: Option<u64>,
    /// `approvals`: can only lower the configured approval mode.
    pub approvals: Option<crate::approval_policy::ApprovalMode>,
    pub background: bool,
}

pub enum Started {
    /// The run ended (or was refused) before this call returned.
    Finished(Box<RunResult>),
    /// `async`: the run is in flight; poll with `agent_status` / `agent_result`.
    Background { run_id: String },
}

#[derive(Default)]
struct Announce {
    run_id: Mutex<Option<String>>,
    cv: Condvar,
}

/// The `selachi-agent` executable to start (this very process image).
pub fn agent_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot locate the selachi-agent executable: {e}"))
}

/// Command-line arguments of the child. `--flag=value` forms and `--` keep a hostile value
/// (an agent name starting with `--`) from being read as a flag.
pub fn child_args(spec: &Spec) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "--json".into(), "--announce-run-id".into()];
    a.push(format!("--cwd={}", spec.cwd.display()));
    if let Some(p) = &spec.permission {
        a.push(format!("--permission={p}"));
    }
    if let Some(p) = &spec.profile {
        a.push(format!("--profile={p}"));
    }
    if let Some(m) = &spec.model {
        a.push(format!("--model={m}"));
    }
    if spec.worktree {
        a.push("--worktree".into());
    }
    if let Some(t) = spec.timeout_s {
        a.push(format!("--timeout={t}s"));
    }
    if let Some(b) = spec.budget_tokens {
        a.push(format!("--budget-tokens={b}"));
    }
    if let Some(m) = spec.approvals {
        a.push(format!("--approvals={m}"));
    }
    match &spec.agent {
        // with a profile the agent is a flag too, so the prompt marker is the only positional
        Some(agent) if spec.profile.is_some() => {
            a.push(format!("--agent={agent}"));
            a.push("--".into());
        }
        Some(agent) => {
            a.push("--".into());
            a.push(agent.clone());
        }
        None => a.push("--".into()),
    }
    // The prompt travels on stdin: no command-line length limit, no quoting problems.
    a.push("-".into());
    a
}

/// Run (or start) the delegation. Blocks until the run ends unless `spec.background`.
pub fn start(spec: &Spec, state: &CallState, registry: &Registry) -> Result<Started, String> {
    let exe = agent_exe()?;
    let mut cmd = Command::new(&exe);
    cmd.args(child_args(spec))
        .env(ENV_CHANNEL, "mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    agents_driver::proc::hide_window(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let pid = child.id();

    // prompt -> stdin (on its own thread: a large prompt must not block the loop below)
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = spec.prompt.clone();
        std::thread::spawn(move || {
            // an error here means the child already exited; its output says why
            let _ = stdin.write_all(prompt.as_bytes());
        });
    }
    let announce = Arc::new(Announce::default());
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(err) = child.stderr.take() {
        let (announce, tail) = (announce.clone(), tail.clone());
        std::thread::spawn(move || {
            // line by line, lossy: the run id must be seen while the run is still going
            for raw in std::io::BufReader::new(err).split(b'\n') {
                let Ok(raw) = raw else { break };
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches('\r');
                if let Some(id) = line.strip_prefix(RUN_ID_MARKER) {
                    *announce.run_id.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(id.trim().to_string());
                    announce.cv.notify_all();
                } else {
                    let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                    if t.len() >= 20 {
                        t.pop_front();
                    }
                    t.push_back(line.to_string());
                }
            }
        });
    }
    let stdout = child.stdout.take();
    let out_thread = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(mut o) = stdout {
            let _ = o.read_to_end(&mut s);
        }
        String::from_utf8_lossy(&s).into_owned()
    });

    let started = Instant::now();
    let mut cancel_at: Option<Instant> = None;
    let mut announced_run: Option<String> = None;
    loop {
        if announced_run.is_none() {
            announced_run = announce
                .run_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Err(format!("cannot wait for the runner: {e}")),
        }
        if spec.background {
            if let Some(id) = &announced_run {
                // The run outlives this call: keep reaping the child, nothing else to do with it.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(Started::Background { run_id: id.clone() });
            }
            if started.elapsed() > ANNOUNCE_TIMEOUT {
                let _ = proc_svc::terminate_tree(pid, Duration::from_millis(500));
                return Err("the run did not start within 120 s; it was stopped".into());
            }
        }
        if state.is_cancelled() {
            match (&announced_run, cancel_at) {
                (Some(id), None) => {
                    let _ = registry.request_cancel(id);
                    cancel_at = Some(Instant::now() + CANCEL_GRACE);
                }
                (None, None) => {
                    let _ = proc_svc::terminate_tree(pid, Duration::from_millis(500));
                    cancel_at = Some(Instant::now() + CANCEL_GRACE);
                }
                (_, Some(deadline)) if Instant::now() > deadline => {
                    let _ = proc_svc::terminate_tree(pid, Duration::from_millis(500));
                }
                _ => {}
            }
        }
        std::thread::sleep(POLL);
    }
    let code = child.wait().ok().and_then(|s| s.code());
    let stdout = out_thread.join().unwrap_or_default();
    let stderr_tail = tail
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    match serde_json::from_str::<RunResult>(stdout.trim()) {
        Ok(r) => Ok(Started::Finished(Box::new(r))),
        Err(_) => {
            let reason = format!(
                "the runner exited{} without a result{}",
                code.map(|c| format!(" with code {c}")).unwrap_or_default(),
                if stderr_tail.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr_tail.trim())
                }
            );
            Ok(Started::Finished(Box::new(RunResult {
                summary: reason.clone(),
                reason: Some(reason),
                run_id: announced_run,
                ..RunResult::empty(RunStatus::Error)
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_args_are_flag_injection_proof() {
        let spec = Spec {
            agent: Some("--json".into()),
            profile: None,
            prompt: "x".into(),
            cwd: PathBuf::from("C:/my project"),
            model: Some("--worktree".into()),
            permission: Some("read_only".into()),
            worktree: true,
            timeout_s: Some(30),
            budget_tokens: Some(1000),
            approvals: None,
            background: false,
        };
        let a = child_args(&spec);
        let dd = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(
            &a[dd + 1..],
            ["--json", "-"],
            "agent and prompt marker come after `--`"
        );
        assert!(a.contains(&"--model=--worktree".to_string()));
        assert!(a.contains(&"--cwd=C:/my project".to_string()));
        assert!(a.contains(&"--timeout=30s".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--approvals")));
        let with = Spec {
            approvals: Some(crate::approval_policy::ApprovalMode::Allowlist),
            ..spec
        };
        assert!(child_args(&with).contains(&"--approvals=allowlist".to_string()));
    }

    #[test]
    fn child_args_with_a_profile() {
        let spec = Spec {
            agent: None,
            profile: Some("--force".into()),
            prompt: "x".into(),
            cwd: PathBuf::from("C:/p"),
            model: None,
            permission: None,
            worktree: false,
            timeout_s: None,
            budget_tokens: None,
            approvals: None,
            background: false,
        };
        let a = child_args(&spec);
        assert!(a.contains(&"--profile=--force".to_string()));
        assert!(
            !a.iter().any(|x| x.starts_with("--permission")),
            "no permission flag unless the caller gave one: the profile's applies"
        );
        let dd = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(&a[dd + 1..], ["-"]);
        let with_agent = child_args(&Spec {
            agent: Some("codex".into()),
            ..spec
        });
        assert!(with_agent.contains(&"--agent=codex".to_string()));
        let dd = with_agent.iter().position(|x| x == "--").unwrap();
        assert_eq!(&with_agent[dd + 1..], ["-"]);
    }
}
