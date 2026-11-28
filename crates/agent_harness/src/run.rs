//! The engine: one delegated run from the policy checks to the JSON result.
//!
//! ```text
//! policy (permission, depth, agent) -> budgets (daily tokens, weekly usage) -> slot (max N runs)
//!   -> child thread in chat.db (parent_thread_id / depth / delegated_by) [+ git worktree]
//!   -> checkpoint -> agent in a Job Object -> wait (timeout, cancel, token budget, approvals)
//!   -> diff since the checkpoint + usage/cost -> RunResult (+ registry record)
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chat_core::{
    ApprovalDecision as ChatDecision, ChatError, ChatStore, DriverFactory, PolicyDenial, ThreadId,
    ThreadLink, ThreadRuntime, ThreadSpec, ThreadStatus, ToolStatus,
};
use chrono::{Local, TimeZone};
use parking_lot::Mutex;

use crate::agents::{agent_name, AgentResolver, SystemResolver};
use crate::approval_policy::{ApprovalMode, Gate, OwnedScope};
use crate::approvals::{ApprovalBroker, ApprovalRequest, Decision, PipeBroker, PROTOCOL_VERSION};
use crate::events::{ApprovalOutcome, EventLog, Recorder};
use crate::jobs::JobHooks;
use crate::models::{DiskModels, ModelSource};
use crate::output::{cap_chars, diff_stat_of, CostOut, RunResult, RunStatus, UsageOut};
use crate::policy::{
    Caller, DelegationConfig, Permission, Refusal, RefusalCode, ENV_DELEGATION_DEPTH,
    ENV_PARENT_PERMISSION, ENV_PARENT_TAB, ENV_PARENT_THREAD,
};
use crate::profiles::prepare::{claude_tool_args, ExtraArgs, ExtraArgsFactory};
use crate::registry::{Registry, RunRecord, MAX_FINAL_PROMPT_CHARS};
use crate::slots;
use crate::turns::{self, Denial, FollowKind, Next, Planner};
use crate::usage::{AgentsSvcProbe, UsageProbe};

/// Longest summary kept in the result (characters).
pub const MAX_SUMMARY_CHARS: usize = 8000;
/// How often the wait loop wakes up on its own (cancel marker, timeout). Events wake it sooner.
const POLL: Duration = Duration::from_millis(250);

/// What to run.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// `claude | codex | opencode | gemini | agy | acp:<command line>`. Empty = the agent of `profile`.
    pub agent: String,
    pub prompt: String,
    /// Project directory (default: the current directory).
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// Reasoning effort (`low | medium | high | xhigh | max`), separate from the model: the harness
    /// maps the pair to the agent's own id (Antigravity `gemini-4-neon` + `medium` =
    /// `gemini-4-neon-medium`) and refuses what the agent does not offer. With a profile, this
    /// overrides the profile's.
    pub effort: Option<String>,
    /// Default read-only.
    pub permission: Permission,
    /// The caller asked for `permission` itself. With a `profile`, an explicit permission can only
    /// narrow the profile's; without it the profile's permission applies. Ignored without a profile.
    pub permission_explicit: bool,
    /// Name of a subagent profile (PLAN section 21.3): its agent, model, instructions, context files
    /// and limits apply, never above `[delegation]` and the calling session.
    pub profile: Option<String>,
    /// Run in a new git worktree + branch (`selachi/<slug>`) so the parent's tree is untouched.
    pub worktree: bool,
    /// `None`: `[delegation] timeout_secs` (15 min).
    pub timeout: Option<Duration>,
    /// Stop the agent when it has used this many billable tokens.
    pub budget_tokens: Option<u64>,
    /// Approval mode asked for (`--approvals`, MCP `approvals`). It can only lower the mode of
    /// `[delegation]` and of the profile, never raise it.
    pub approvals: Option<ApprovalMode>,
    /// A turn that ends while a command is still running (or says it is waiting for one) gets one
    /// automatic follow-up, and is an error when it is still unfinished, never `done`.
    /// `None` = the profile's setting, else `true`. `Some(false)` can only be asked on purpose.
    pub wait_for_background: Option<bool>,
}

impl RunRequest {
    pub fn new(
        agent: impl Into<String>,
        prompt: impl Into<String>,
        cwd: impl Into<PathBuf>,
    ) -> RunRequest {
        RunRequest {
            agent: agent.into(),
            prompt: prompt.into(),
            cwd: cwd.into(),
            model: None,
            effort: None,
            permission: Permission::ReadOnly,
            permission_explicit: false,
            profile: None,
            worktree: false,
            timeout: None,
            budget_tokens: None,
            approvals: None,
            wait_for_background: None,
        }
    }
}

/// Builds a [`Harness`]. Everything has a production default; tests inject fakes.
#[derive(Default)]
pub struct HarnessBuilder {
    data_dir: Option<PathBuf>,
    config: Option<DelegationConfig>,
    caller: Option<Caller>,
    factory: Option<Arc<dyn DriverFactory>>,
    resolver: Option<Arc<dyn AgentResolver>>,
    usage: Option<Arc<dyn UsageProbe>>,
    models: Option<Arc<dyn ModelSource>>,
    broker: Option<Arc<dyn ApprovalBroker>>,
    export_env: bool,
    on_started: Option<StartedHook>,
}

/// Called with the run id as soon as the run exists (record saved, agent not started yet).
pub type StartedHook = Arc<dyn Fn(&str) + Send + Sync>;

impl HarnessBuilder {
    /// Directory of `chat.db`, `config.toml`, `agent-runs`, `agent-locks` (default `%APPDATA%\Selachi`,
    /// `SELACHI_DATA_DIR` overrides).
    pub fn data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }
    /// Delegation settings (default: `[delegation]` of `config.toml`, else the built-in defaults).
    pub fn config(mut self, cfg: DelegationConfig) -> Self {
        self.config = Some(cfg);
        self
    }
    /// The delegating session (default: [`Caller::from_env`]).
    pub fn caller(mut self, caller: Caller) -> Self {
        self.caller = Some(caller);
        self
    }
    /// How agents are spawned (default: the real CLIs through `agents_driver`).
    pub fn factory(mut self, factory: Arc<dyn DriverFactory>) -> Self {
        self.factory = Some(factory);
        self
    }
    pub fn resolver(mut self, resolver: Arc<dyn AgentResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }
    pub fn usage_probe(mut self, usage: Arc<dyn UsageProbe>) -> Self {
        self.usage = Some(usage);
        self
    }
    /// Where the models of each agent come from (default: the catalog in the data folder, asked to the
    /// agent itself; tests pass fixed lists or [`crate::models::NoModels`]).
    pub fn models(mut self, models: Arc<dyn ModelSource>) -> Self {
        self.models = Some(models);
        self
    }
    /// Where the child's approvals go (default: the Selachi GUI's named pipe, deny when it is not there).
    pub fn broker(mut self, broker: Arc<dyn ApprovalBroker>) -> Self {
        self.broker = Some(broker);
        self
    }
    /// Export `SELACHI_PARENT_PERMISSION`, `SELACHI_DELEGATION_DEPTH` and `SELACHI_PARENT_THREAD` into this
    /// process's environment before the agent starts, so the agent inherits them and cannot escalate
    /// when it delegates in turn. Process-global: only the one-run-per-process CLI turns it on.
    pub fn export_child_env(mut self, on: bool) -> Self {
        self.export_env = on;
        self
    }
    /// Be told the run id as soon as it exists (the MCP server needs it for `async` runs).
    pub fn on_started(mut self, hook: StartedHook) -> Self {
        self.on_started = Some(hook);
        self
    }

    pub fn build(self) -> Result<Harness, String> {
        let data_dir = match self.data_dir {
            Some(d) => d,
            None => store::paths::app_data_dir()
                .map_err(|e| format!("cannot find the Selachi data folder: {e}"))?,
        };
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        let (cfg, warning) = match self.config {
            Some(c) => (c, None),
            None => DelegationConfig::load(&data_dir),
        };
        Ok(Harness {
            models: self
                .models
                .unwrap_or_else(|| Arc::new(DiskModels::new(&data_dir))),
            registry: Registry::new(&data_dir),
            usage: self
                .usage
                .unwrap_or_else(|| Arc::new(AgentsSvcProbe::new(&data_dir))),
            resolver: self.resolver.unwrap_or_else(|| Arc::new(SystemResolver)),
            broker: self
                .broker
                .unwrap_or_else(|| Arc::new(PipeBroker::for_current_user())),
            caller: self.caller.unwrap_or_else(Caller::from_env),
            config_warning: warning,
            data_dir,
            cfg,
            store: Mutex::new(None),
            factory: self.factory,
            hooks: Arc::new(JobHooks::default()),
            export_env: self.export_env,
            on_started: self.on_started,
            extra_args: Arc::new(ExtraArgs::default()),
        })
    }
}

/// The delegation engine. One instance per process is enough; runs are independent.
pub struct Harness {
    pub(crate) data_dir: PathBuf,
    pub(crate) cfg: DelegationConfig,
    pub(crate) caller: Caller,
    store: Mutex<Option<ChatStore>>,
    factory: Option<Arc<dyn DriverFactory>>,
    pub(crate) registry: Registry,
    pub(crate) resolver: Arc<dyn AgentResolver>,
    pub(crate) usage: Arc<dyn UsageProbe>,
    pub(crate) models: Arc<dyn ModelSource>,
    pub(crate) broker: Arc<dyn ApprovalBroker>,
    pub(crate) hooks: Arc<JobHooks>,
    export_env: bool,
    on_started: Option<StartedHook>,
    config_warning: Option<String>,
    /// Tool restrictions of the runs in flight (Claude only), see [`ExtraArgs`].
    extra_args: Arc<ExtraArgs>,
}

enum Stop {
    Refused(Refusal),
    Failed(String),
}

impl From<Refusal> for Stop {
    fn from(r: Refusal) -> Stop {
        Stop::Refused(r)
    }
}

/// Unix ms of the last local midnight before `now_ms`.
pub fn local_day_start_ms(now_ms: i64) -> i64 {
    let Some(now) = Local.timestamp_millis_opt(now_ms).single() else {
        return now_ms - now_ms.rem_euclid(86_400_000);
    };
    let midnight = now.date_naive().and_hms_opt(0, 0, 0);
    midnight
        .and_then(|m| Local.from_local_datetime(&m).earliest())
        .map(|d| d.timestamp_millis())
        .unwrap_or(now_ms - now_ms.rem_euclid(86_400_000))
}

fn ms_of(t: std::time::SystemTime) -> Option<i64> {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Absolute, existing directory without the `\\?\` prefix.
pub(crate) fn normalize_dir(p: &Path) -> Option<PathBuf> {
    let c = std::fs::canonicalize(p).ok()?;
    let clean = PathBuf::from(store::paths::clean_display(&c.to_string_lossy()));
    clean.is_dir().then_some(clean)
}

fn thread_title(agent: &str, prompt: &str) -> String {
    let first = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let short: String = first.chars().take(48).collect();
    format!("{agent}: {short}")
}

fn redacted(text: &str) -> String {
    store::redact(text).text
}

impl Harness {
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    pub fn config(&self) -> &DelegationConfig {
        &self.cfg
    }

    /// A problem found while reading `config.toml` (the defaults are in effect).
    pub fn config_warning(&self) -> Option<&str> {
        self.config_warning.as_deref()
    }

    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The chat store the child threads live in. Opened on first use (`status`, `diff`, `apply` and
    /// `list` never touch `chat.db`) and without resetting the busy status of the app's threads.
    pub fn store(&self) -> Result<ChatStore, String> {
        let mut slot = self.store.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let mut sb = ChatStore::builder()
            .path(self.data_dir.join("chat.db"))
            .hooks(self.hooks.clone())
            .keep_busy_status();
        let inner: Arc<dyn DriverFactory> = self
            .factory
            .clone()
            .unwrap_or_else(|| Arc::new(chat_core::DefaultDriverFactory));
        sb = sb.factory(Arc::new(ExtraArgsFactory {
            inner,
            extra: self.extra_args.clone(),
        }));
        let store = sb.open().map_err(|e| format!("cannot open chat.db: {e}"))?;
        *slot = Some(store.clone());
        Ok(store)
    }

    pub fn usage_probe(&self) -> &Arc<dyn UsageProbe> {
        &self.usage
    }

    /// Job Objects created for runs of this harness (tests and diagnostics).
    pub fn jobs(&self) -> &JobHooks {
        &self.hooks
    }

    /// Run `req` to completion (blocking). Never panics on a refusal: it returns a result whose
    /// status is `refused` and whose `run_id` is `null`.
    pub fn run(&self, req: RunRequest) -> RunResult {
        match self.run_inner(req) {
            Ok(r) => r,
            Err(Stop::Refused(r)) => RunResult::refused(&r),
            Err(Stop::Failed(msg)) => RunResult {
                summary: msg.clone(),
                reason: Some(msg),
                ..RunResult::empty(RunStatus::Error)
            },
        }
    }

    fn check_budgets(&self, agent: &str, now: i64) -> Result<(), Refusal> {
        if self.cfg.daily_token_budget > 0 {
            let used = self
                .registry
                .billable_tokens_between(local_day_start_ms(now), i64::MAX);
            if used >= self.cfg.daily_token_budget {
                return Err(Refusal::new(
                    RefusalCode::DailyBudget,
                    format!(
                        "the daily delegation budget is spent ({used} of {} billable tokens today); it resets at local midnight \
                         ([delegation] daily_token_budget)",
                        self.cfg.daily_token_budget
                    ),
                ));
            }
        }
        let threshold = self.cfg.weekly_usage_threshold_percent;
        if threshold > 0.0 {
            if let Some(l) = self.usage.weekly_used_percent(agent, now) {
                if l.used_percent > threshold {
                    return Err(Refusal::new(
                        RefusalCode::WeeklyUsage,
                        format!(
                            "{agent} has used {:.0} % of its weekly limit ({}), above the {threshold:.0} % threshold \
                             ([delegation] weekly_usage_threshold_percent); pick another agent or wait for the reset",
                            l.used_percent, l.source
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The calling session as it is **now**. The environment of a chat thread's agent is fixed when
    /// the process starts, so a permission lowered afterwards (the pill) is read from `chat.db`: the
    /// caller gets the minimum of both, and a parent thread that cannot be found fails closed to
    /// read-only. A caller without a parent thread (a terminal, a script) is what the environment
    /// says. The strings are notes for the result.
    fn caller_now(&self) -> (Caller, Vec<String>, Option<String>) {
        let mut c = self.caller.clone();
        let mut notes = Vec::new();
        let Some(p) = c.parent_thread else {
            return (c, notes, None);
        };
        let parent = self
            .store()
            .ok()
            .and_then(|s| s.get_thread(ThreadId(p)).ok().flatten());
        let mut lowered = None;
        match parent {
            Some(rec) => {
                let now = Permission::from_driver(rec.permission).min(Permission::AcceptEdits);
                if now < c.permission {
                    let why = format!(
                        "el hilo padre {p} tiene ahora el permiso {now} (la sesión empezó con {}); la delegación queda limitada a {now}",
                        c.permission
                    );
                    notes.push(why.clone());
                    lowered = Some(why);
                    c.permission = now;
                }
            }
            None => {
                notes.push(format!("{ENV_PARENT_THREAD}={p} is not a thread of this chat database; the run was not linked to a parent; its permission is limited to read-only (unknown parent)"));
                c.parent_thread = None;
                c.permission = Permission::ReadOnly;
            }
        }
        (c, notes, lowered)
    }

    /// Reads the driver's refusals so far (events log + the list the agent is told about).
    fn note_denials(rt: &ThreadRuntime, ctx: &mut ApprovalCtx, rec: &mut Recorder) {
        let (total, kept) = rt.with_state(|s| (s.policy_denied_total, s.policy_denials.clone()));
        Self::log_denials(total, &kept, ctx, rec);
    }

    /// Records the refusals the driver made on its own that the events log has not seen yet.
    fn log_denials(total: u32, kept: &[PolicyDenial], ctx: &mut ApprovalCtx, rec: &mut Recorder) {
        if total <= ctx.denials_logged {
            return;
        }
        let new = (total - ctx.denials_logged) as usize;
        let skip = kept.len().saturating_sub(new);
        for d in &kept[skip..] {
            let outcome = if d.blocked {
                ApprovalOutcome::Blocked
            } else {
                ApprovalOutcome::Denied
            };
            rec.approval(&d.command, outcome, &ctx.mode.to_string(), Some(&d.reason));
            ctx.turn_denials.push(Denial {
                command: redacted(&d.command),
                reason: redacted(&d.reason),
            });
        }
        ctx.denials_logged = total;
    }

    fn run_inner(&self, req: RunRequest) -> Result<RunResult, Stop> {
        let now = chat_core::now_ms();

        // 0. the caller as it is now (the parent thread's current permission)
        let (caller, mut caller_notes, parent_lowered) = self.caller_now();

        // 1. the profile (if any), the effective limits (the minimum of profile, config and caller),
        //    the static rules (enabled, allowed agent, permission <= parent, never Full, depth) and
        //    the final prompt, redacted
        let prep = self.prepare_as(&req, &caller)?;
        let cwd = prep.project_root.clone();
        let permission = prep.permission;

        // 2. which agent exactly
        let kind = self.resolver.resolve(&prep.agent)?;
        let agent = agent_name(&kind);

        // 3. daily budget and the agent's weekly usage
        self.check_budgets(&agent, now)?;

        // 4. at most N runs at once, across processes
        let slot = match slots::acquire(&self.data_dir, self.cfg.slots()) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Err(Refusal::new(
                    RefusalCode::Concurrency,
                    format!(
                        "{} delegated runs are already in flight (max_concurrent = {}); wait for one to finish \
                         (`selachi-agent status <run_id>`) or cancel it",
                        self.cfg.slots(),
                        self.cfg.slots()
                    ),
                )
                .into())
            }
            Err(e) => return Err(Stop::Failed(format!("cannot take a run slot: {e}"))),
        };

        // 5. the prompt was redacted in `prepare`; its notes (lowered permission, redactions) travel on
        let prompt = prep.final_prompt.clone();
        let mut notes: Vec<String> = prep.notes.clone();
        notes.append(&mut caller_notes);
        let permission_reason = permission_reason(&req, &prep, &caller, parent_lowered.as_deref());

        // 6. child thread (+ worktree)
        if prep.worktree && !matches!(git_svc::Repo::discover(&cwd), Ok(Some(_))) {
            return Err(Refusal::new(
                RefusalCode::NotAGitRepo,
                format!(
                    "--worktree needs a git repository and {} is not inside one",
                    cwd.display()
                ),
            )
            .into());
        }
        let depth = caller.depth.saturating_add(1);
        let store = self.store().map_err(Stop::Failed)?;
        // A parent id that is not a thread of this database (stale or mistyped variable) must not
        // sink the run: `caller_now` already linked it to nothing, limited it to read-only and said so.
        let parent_thread = caller.parent_thread;
        let mut spec = ThreadSpec::new(&cwd, kind.clone());
        spec.title = Some(thread_title(&agent, &prep.task_prompt));
        spec.model = prep.model.clone();
        spec.effort = prep
            .effort
            .clone()
            .filter(|_| agents_driver::supports_effort(&kind));
        spec.permission = permission.to_driver();
        spec.use_worktree = prep.worktree;
        spec.link = Some(ThreadLink {
            parent_thread_id: parent_thread.map(ThreadId),
            parent_tab: caller.parent_tab.clone(),
            depth,
            delegated_by: Some(caller.channel.clone()),
        });
        let thread = match store.create_thread(spec) {
            Ok(t) => t,
            Err(ChatError::NotAGitRepo(p)) => {
                return Err(Refusal::new(
                    RefusalCode::NotAGitRepo,
                    format!("{} is not inside a git repository", p.display()),
                )
                .into())
            }
            Err(e) => return Err(Stop::Failed(format!("cannot create the child thread: {e}"))),
        };
        let run_id = thread.id.0.to_string();
        let run_dir = thread.cwd().to_path_buf();

        // 7. checkpoint: what `diff` and `apply` compare against
        let mut checkpoint_id: Option<String> = None;
        if let Ok(Some(repo)) = git_svc::Repo::discover(&run_dir) {
            match repo.create_checkpoint(&format!("delegacion {run_id} a {agent}")) {
                Ok(cp) => checkpoint_id = Some(cp.id),
                Err(e) => notes.push(format!(
                    "no checkpoint could be taken ({e}); the diff of this run will be unavailable"
                )),
            }
        }

        let timeout = prep.timeout;
        let started_ms = chat_core::now_ms();
        let mut record = RunRecord {
            run_id: run_id.clone(),
            thread_id: thread.id.0,
            agent: agent.clone(),
            model: prep.model.clone(),
            permission: permission.to_string(),
            cwd: run_dir.clone(),
            project_root: cwd,
            worktree_path: thread.worktree_path.clone(),
            checkpoint_id,
            prompt: cap_chars(&prep.task_prompt, 500),
            parent_thread_id: parent_thread,
            depth,
            started_ms,
            finished_ms: None,
            status: RunStatus::Running,
            runner_pid: std::process::id(),
            runner_start_ms: proc_svc::start_time_of(std::process::id()).and_then(ms_of),
            deadline_ms: started_ms + timeout.as_millis() as i64,
            result: None,
            applied_ms: None,
            profile: prep.profile.clone(),
            final_prompt: Some(cap_chars(&prompt, MAX_FINAL_PROMPT_CHARS)),
            limits: Some(prep.limits.clone()),
            launcher: Some(caller.channel),
            effort: prep.effort.clone(),
            output_format: prep.output_format.map(|f| f.as_str().to_string()),
            context_files: prep.context_files.iter().map(|c| c.path.clone()).collect(),
        };
        self.registry.clear_cancel(&run_id);
        if let Err(e) = self.registry.save(&record) {
            return Err(Stop::Failed(format!("cannot write the run record: {e}")));
        }

        if let Some(hook) = &self.on_started {
            hook(&run_id);
        }

        // 8. start the agent (inside its Job Object, through the chat runtime)
        if self.export_env {
            std::env::set_var(ENV_PARENT_PERMISSION, permission.as_str());
            std::env::set_var(ENV_DELEGATION_DEPTH, depth.to_string());
            std::env::set_var(ENV_PARENT_THREAD, run_id.as_str());
            std::env::remove_var(ENV_PARENT_TAB);
        }
        // Claude only: the profile's tool lists become command-line flags of this run's agent
        let _tool_args = if prep.tools_enforced {
            match self.extra_args.register(
                &run_dir,
                claude_tool_args(&prep.allowed_tools, &prep.denied_tools),
            ) {
                Ok(g) => Some(g),
                Err(e) => return Err(Stop::Failed(e)),
            }
        } else {
            None
        };
        let mut recorder = Recorder::new(
            EventLog::open(self.registry.dir(), &run_id),
            &[record.project_root.as_path(), run_dir.as_path()],
        );
        let rt = match store.open(thread.id) {
            Ok(rt) => rt,
            Err(e) => return Err(Stop::Failed(format!("cannot open the child thread: {e}"))),
        };
        let mut ctx = ApprovalCtx {
            mode: prep.approvals,
            allowed: prep.allowed_commands.clone(),
            scope: OwnedScope::new(&record.project_root, &run_dir),
            profile_path: prep.profile_path.clone(),
            profile_scope: prep.profile_scope,
            denials_logged: 0,
            refused: 0,
            last_refused: None,
            turn_denials: Vec::new(),
        };
        let run_start = Instant::now();
        let mut outcome = match rt.send(prompt, Vec::new()) {
            Ok(()) => self.wait(
                &rt,
                &mut record,
                &mut ctx,
                &mut recorder,
                prep.budget_tokens,
                timeout,
                &mut notes,
            ),
            Err(e) => Outcome::Failed(format!("cannot send the prompt: {e}")),
        };

        // 8b. A turn that ends is not always a task that is done: an agent that backgrounded its
        // command, or whose request was refused and never learned why, ends its turn early. Ask it
        // once per cause to carry on (`turns`), then judge the last turn.
        let mut planner = Planner::new(prep.wait_for_background);
        let mut turn_start = 0usize;
        let mut prior_answer = String::new();
        let last_obs = loop {
            Self::note_denials(&rt, &mut ctx, &mut recorder);
            let obs = rt.with_state(|s| turns::observe(s, turn_start, &ctx.turn_denials));
            if !matches!(outcome, Outcome::Done) {
                break obs;
            }
            let Next::FollowUp { kind, text } = planner.after_turn(&obs) else {
                break obs;
            };
            let left = timeout.saturating_sub(run_start.elapsed());
            if left.is_zero() {
                outcome = Outcome::TimedOut;
                break obs;
            }
            if !obs.answer.trim().is_empty() {
                prior_answer = obs.answer.clone();
            }
            notes.push(match kind {
                FollowKind::Background => "el agente terminó el turno con un comando sin terminar; se le pidió esperar y responder con el resultado final".to_string(),
                FollowKind::Denial => format!(
                    "el agente terminó el turno tras {} rechazo(s); se le dijo por qué y se le pidió continuar sin ese comando",
                    obs.denials.len()
                ),
            });
            recorder.sync(&rt);
            recorder.follow_up(kind.as_str(), &text);
            turn_start = rt.with_state(|s| s.messages.len());
            ctx.begin_turn();
            outcome = match rt.send(text, Vec::new()) {
                Ok(()) => {
                    // the user message is in the state (and the thread Working) before we wait
                    rt.wait_for(Duration::from_secs(10), |s| s.messages.len() > turn_start);
                    self.wait(
                        &rt,
                        &mut record,
                        &mut ctx,
                        &mut recorder,
                        prep.budget_tokens,
                        left,
                        &mut notes,
                    )
                }
                Err(e) => Outcome::Failed(format!("cannot send the follow-up: {e}")),
            };
        };

        // 9. stop the agent (kills its tree; the Job Object catches stragglers) and collect
        store.close_thread(thread.id);
        let state = rt.snapshot();
        let totals = state.usage;
        let usage = UsageOut::new(
            totals.input,
            totals.output,
            totals.cache_create,
            totals.cache_read,
        );
        let usd = totals.cost_usd();
        let cost = CostOut {
            usd,
            exact: usd > 0.0 && totals.cost_is_exact(),
            source: match (
                totals.cost_reported_usd > 0.0,
                totals.cost_estimated_usd > 0.0,
            ) {
                (true, false) => "reported",
                (false, true) => "estimated",
                (true, true) => "mixed",
                (false, false) => "none",
            }
            .into(),
        };
        let answer = if last_obs.answer.trim().is_empty() {
            prior_answer
        } else {
            last_obs.answer.clone()
        };
        let (mut status, mut reason) = match &outcome {
            Outcome::Done => (RunStatus::Done, None),
            Outcome::Failed(msg) => (RunStatus::Error, Some(redacted(state.error.as_deref().unwrap_or(msg)))),
            Outcome::Cancelled => (RunStatus::Cancelled, Some("cancelled with `selachi-agent cancel`".to_string())),
            Outcome::TimedOut => (RunStatus::TimedOut, Some(format!("no result within {} s; the agent was interrupted", timeout.as_secs()))),
            Outcome::OverBudget(limit) => (
                RunStatus::BudgetExceeded,
                Some(format!("the run used {} billable tokens, over --budget-tokens {limit}; the agent was interrupted", usage.billable)),
            ),
        };
        // the events log: whatever the last observation missed (the thread was closed since), the
        // refusals the driver made on its own, then the final status
        recorder.observe(&state);
        recorder.commit();
        Self::log_denials(
            state.policy_denied_total,
            &state.policy_denials,
            &mut ctx,
            &mut recorder,
        );
        // An agent that ended its last turn waiting for a command (or with one still running) did not
        // finish: an error that says so, never `done`.
        let mut incomplete = false;
        if status == RunStatus::Done {
            if let Some(problem) = turns::final_problem(prep.wait_for_background, &last_obs) {
                status = RunStatus::Error;
                reason = Some(problem);
                incomplete = true;
            }
        }
        // An agent that ended its turn without saying anything did not do the task, whatever the
        // protocol says: it is an error with what happened and how to fix it, never a silent success.
        if status == RunStatus::Done && answer.trim().is_empty() {
            status = RunStatus::Error;
            reason = Some(empty_answer_error(&EmptyAnswer {
                denied: state.policy_denied_total,
                last_denied: state.policy_denials.last(),
                refused: ctx.refused,
                last_refused: ctx.last_refused.as_deref(),
                stop_reason: state.stop_reason.as_deref(),
                permission,
                thread_id: thread.id.0,
            }));
        }
        recorder.finish(status.as_str());
        if state.policy_denied_total > 0 {
            if let Some(last) = state.policy_denials.last() {
                notes.push(denied_note(state.policy_denied_total, last));
            }
        }
        if let Some(sr) = &state.stop_reason {
            notes.push(format!(
                "el agente terminó el turno con el motivo de parada `{sr}`"
            ));
        }
        let unfinished = state
            .last_assistant()
            .map(|m| {
                m.tool_runs()
                    .filter(|t| t.status == ToolStatus::Cancelled)
                    .count()
            })
            .unwrap_or(0);
        if unfinished > 0 {
            notes.push(format!(
                "{unfinished} herramienta(s) quedaron sin resultado cuando el agente terminó el turno"
            ));
        }

        let summary = if incomplete {
            format!(
                "{}

Último mensaje del agente:
{}",
                reason.clone().unwrap_or_default(),
                cap_chars(&redacted(&answer), MAX_SUMMARY_CHARS / 2)
            )
        } else if answer.trim().is_empty() {
            reason.clone().unwrap_or_default()
        } else {
            cap_chars(&redacted(&answer), MAX_SUMMARY_CHARS)
        };

        if status == RunStatus::Done
            && prep.output_format == Some(crate::profiles::OutputFormat::Json)
        {
            if let Some(problem) = crate::profiles::prepare::json_reply_problem(&answer) {
                notes.push(format!("output_format = json: {problem}"));
            }
        }

        if let Some(l) = record.limits.as_ref().filter(|l| l.auto_approved > 0) {
            notes.push(format!(
                "{} request(s) were approved without asking (approvals = {}); the last ones are in the run record (limits.auto_approved_commands)",
                l.auto_approved, l.approvals
            ));
        }

        let (files_changed, diff_stat) =
            match (&record.checkpoint_id, git_svc::Repo::discover(&run_dir)) {
                (Some(cp), Ok(Some(repo))) => {
                    let files = repo
                        .changed_files_since(cp)
                        .map(|v| {
                            v.into_iter()
                                .map(|c| c.path.to_string_lossy().replace('\\', "/"))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let stat = repo
                        .diff_since_checkpoint(cp)
                        .map(|d| diff_stat_of(&d))
                        .unwrap_or_default();
                    (files, stat)
                }
                _ => (Vec::new(), Default::default()),
            };

        let result = RunResult {
            run_id: Some(run_id.clone()),
            status,
            summary,
            files_changed,
            diff_stat,
            usage,
            cost,
            worktree_path: record
                .worktree_path
                .as_ref()
                .map(|p| store::paths::clean_display(&p.to_string_lossy())),
            agent: Some(agent),
            permission: Some(permission.to_string()),
            thread_id: Some(thread.id.0),
            reason,
            code: None,
            notes,
            profile: prep.profile,
            permission_reason: Some(permission_reason),
        };
        record.status = status;
        record.finished_ms = Some(chat_core::now_ms());
        record.result = Some(result.clone());
        if let Err(e) = self.registry.save(&record) {
            log::warn!("agent_harness: cannot write the final run record: {e}");
        }
        self.registry.clear_cancel(&run_id);
        drop(slot);
        Ok(result)
    }

    /// Persists what changed while the run is in flight (the map inspector reads the record).
    fn save_progress(&self, record: &RunRecord) {
        if let Err(e) = self.registry.save(record) {
            log::warn!("agent_harness: cannot update the run record: {e}");
        }
    }

    /// Wait for the turn to end, answering approvals and enforcing timeout, cancel and token budget.
    #[allow(clippy::too_many_arguments)]
    fn wait(
        &self,
        rt: &ThreadRuntime,
        record: &mut RunRecord,
        ctx: &mut ApprovalCtx,
        rec: &mut Recorder,
        budget_tokens: Option<u64>,
        timeout: Duration,
        notes: &mut Vec<String>,
    ) -> Outcome {
        let start = Instant::now();
        let deadline = start + timeout;
        let handled: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
        loop {
            rec.sync(rt);
            let (total, kept) =
                rt.with_state(|s| (s.policy_denied_total, s.policy_denials.clone()));
            Self::log_denials(total, &kept, ctx, rec);
            if self.registry.cancel_requested(&record.run_id) {
                let _ = rt.interrupt();
                return Outcome::Cancelled;
            }
            if Instant::now() >= deadline {
                let _ = rt.interrupt();
                return Outcome::TimedOut;
            }
            if let Some(limit) = budget_tokens {
                let used = rt.with_state(|s| s.usage.input + s.usage.output + s.usage.cache_create);
                if used > limit {
                    let _ = rt.interrupt();
                    return Outcome::OverBudget(limit);
                }
            }
            let silent = rt.with_state(turns::trailing_silent_failures);
            if silent >= turns::MAX_SILENT_FAILURES {
                let _ = rt.interrupt();
                return Outcome::Failed(format!(
                    "Detenido: las últimas {silent} llamadas de herramienta del agente fallaron sin ningún mensaje (cada lectura o edición respondió solo con su ruta).                      Suele pasar después de un comando rechazado o con una ruta que el agente no puede usar; abre el hilo {} para ver qué intentaba.",
                    record.thread_id
                ));
            }
            let pending: Vec<(String, String, String)> = {
                let seen = handled.lock();
                rt.with_state(|s| {
                    s.pending_approvals()
                        .into_iter()
                        .filter(|a| !seen.contains(&a.request_id))
                        .map(|a| (a.request_id.clone(), a.tool.clone(), a.detail.clone()))
                        .collect()
                })
            };
            for (request_id, tool, detail) in pending {
                handled.lock().insert(request_id.clone());
                self.answer_approval(
                    rt,
                    record,
                    ctx,
                    rec,
                    &request_id,
                    &tool,
                    &detail,
                    deadline,
                    notes,
                );
            }
            let (status, error) = rt.with_state(|s| (s.status, s.error.clone()));
            match status {
                ThreadStatus::Done => return Outcome::Done,
                ThreadStatus::Error => {
                    return Outcome::Failed(
                        error.unwrap_or_else(|| "the agent reported an error".into()),
                    )
                }
                _ => {}
            }
            rt.wait_for(POLL, |s| {
                matches!(s.status, ThreadStatus::Done | ThreadStatus::Error)
                    || s.pending_approvals()
                        .iter()
                        .any(|a| !handled.lock().contains(&a.request_id))
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn answer_approval(
        &self,
        rt: &ThreadRuntime,
        record: &mut RunRecord,
        ctx: &mut ApprovalCtx,
        rec: &mut Recorder,
        request_id: &str,
        tool: &str,
        detail: &str,
        deadline: Instant,
        notes: &mut Vec<String>,
    ) {
        let shell = crate::approval_policy::shell_command(tool, detail);
        let shown = shell.clone().unwrap_or_else(|| format!("{tool}: {detail}"));
        let mode_when_asked = ctx.mode.to_string();
        // The hard blocklist first, in every mode; then the mode decides between answering on its
        // own and sending the request to a Selachi window.
        match ctx.scope.gate(ctx.mode, &ctx.allowed, tool, detail) {
            Gate::Block(reason) => {
                notes.push(format!(
                    "approval denied for {}: blocked without asking, {reason}",
                    cap_chars(&redacted(tool), 100)
                ));
                rec.approval(
                    &shown,
                    ApprovalOutcome::Blocked,
                    &mode_when_asked,
                    Some(&reason),
                );
                if let Some(l) = record.limits.as_mut() {
                    l.blocked = l.blocked.saturating_add(1);
                }
                ctx.refused += 1;
                ctx.last_refused = Some(redacted(&shown));
                ctx.turn_denials.push(Denial {
                    command: redacted(&shown),
                    reason: redacted(&reason),
                });
                self.save_progress(record);
                if let Err(e) = rt.approve(request_id, ChatDecision::Deny) {
                    notes.push(format!("could not deliver the approval answer: {e}"));
                }
                return;
            }
            Gate::Auto => {
                if let Some(l) = record.limits.as_mut() {
                    l.note_auto_approved(&redacted(&shown));
                }
                rec.approval(&shown, ApprovalOutcome::Auto, &mode_when_asked, None);
                self.save_progress(record);
                if let Err(e) = rt.approve(request_id, ChatDecision::Allow) {
                    notes.push(format!("could not deliver the approval answer: {e}"));
                }
                return;
            }
            Gate::Ask => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
        let wait = self.cfg.approval_timeout_secs.min(remaining.max(1));
        let req = ApprovalRequest {
            v: PROTOCOL_VERSION,
            // Agents number their requests per session (ACP ids start at 0/1), so two runs asking
            // at once sent the same id and Selachi's queue treated the second as a retry of the
            // first, denying it ("superseded"). Scope the wire id by run.
            request_id: format!("run{}:{request_id}", record.run_id),
            run_id: record.run_id.clone(),
            thread_id: record.thread_id,
            parent_thread_id: record.parent_thread_id,
            agent: record.agent.clone(),
            // Agents put whole shell commands in the title; Selachi refuses titles over 256 bytes
            // (100 chars + the truncation note stays under it even with multi-byte text).
            // Keep a short title and move the full command to the detail.
            tool: cap_chars(&redacted(tool), 100),
            detail: if tool.chars().count() > 100 {
                cap_chars(&format!("{}\n\n{}", redacted(tool), redacted(detail)), 4000)
            } else {
                cap_chars(&redacted(detail), 4000)
            },
            permission: record.permission.clone(),
            depth: record.depth,
            timeout_secs: wait,
        };
        let verdict = self.broker.decide(&req);
        let chat = match verdict.decision {
            Decision::Allow => ChatDecision::Allow,
            Decision::Deny => ChatDecision::Deny,
            Decision::Always => ChatDecision::Always,
            // allowed now; the rest of the run is approved by the harness (see below)
            Decision::AlwaysRun | Decision::AlwaysProfile => ChatDecision::Allow,
        };
        let outcome = match verdict.decision {
            Decision::Allow | Decision::Always => ApprovalOutcome::Allowed,
            Decision::Deny => ApprovalOutcome::Denied,
            Decision::AlwaysRun => ApprovalOutcome::AlwaysRun,
            Decision::AlwaysProfile => ApprovalOutcome::AlwaysProfile,
        };
        let mut event_reason = verdict.reason.clone();
        if verdict.decision == Decision::AlwaysRun && ctx.mode != ApprovalMode::Auto {
            ctx.mode = ApprovalMode::Auto;
            if let Some(l) = record.limits.as_mut() {
                l.approvals = ApprovalMode::Auto.to_string();
            }
            self.save_progress(record);
            notes.push(
                "the user chose \"don't ask again in this run\" in Selachi: the rest of the run is approved automatically (the blocklist still applies)"
                    .to_string(),
            );
        }
        if verdict.decision == Decision::AlwaysProfile {
            let note = self.remember_in_profile(ctx, record, shell.as_deref());
            event_reason = Some(note.clone());
            notes.push(note);
        }
        match verdict.decision {
            Decision::Deny => notes.push(format!(
                "approval denied for {}: {}",
                req.tool,
                verdict
                    .reason
                    .as_deref()
                    .unwrap_or("denied by the user in Selachi")
            )),
            _ => notes.push(format!(
                "approval for {} was granted from Selachi",
                req.tool
            )),
        }
        if outcome == ApprovalOutcome::Denied {
            ctx.refused += 1;
            ctx.last_refused = Some(redacted(&shown));
            if event_reason.is_none() {
                event_reason = Some("denied by the user in Selachi".into());
            }
            ctx.turn_denials.push(Denial {
                command: redacted(&shown),
                reason: verdict
                    .reason
                    .as_deref()
                    .map(redacted)
                    .unwrap_or_else(|| "el usuario lo rechazó en Selachi".to_string()),
            });
        }
        rec.approval(&shown, outcome, &mode_when_asked, event_reason.as_deref());
        if let Err(e) = rt.approve(request_id, chat) {
            notes.push(format!("could not deliver the approval answer: {e}"));
        }
    }

    /// `always_profile`: save the command's pattern in the run's profile and let the rest of the run
    /// use it. Returns the sentence for the run's notes (what happened, or why it was only a single
    /// allow).
    fn remember_in_profile(
        &self,
        ctx: &mut ApprovalCtx,
        record: &mut RunRecord,
        command: Option<&str>,
    ) -> String {
        const SINGLE: &str = "allowed this once";
        let (Some(path), Some(scope)) = (ctx.profile_path.clone(), ctx.profile_scope) else {
            return format!(
                "the user chose \"always allow in this profile\" but this run has no profile, so it was {SINGLE}; use a profile (`--profile`) to keep such permissions"
            );
        };
        let Some(command) = command else {
            return format!(
                "the user chose \"always allow in this profile\" but the request is not a shell command, so it was {SINGLE}"
            );
        };
        let pattern = match crate::always_profile::normalize_pattern(command, &ctx.scope) {
            Ok(p) => p,
            Err(why) => {
                return format!(
                    "the user chose \"always allow in this profile\" but a pattern for `{}` cannot be saved ({why}), so it was {SINGLE}",
                    cap_chars(&redacted(command), 80)
                )
            }
        };
        let saved = match crate::always_profile::add_to_profile(&path, scope, &pattern) {
            Ok(crate::always_profile::Added::Added) => "added to",
            Ok(crate::always_profile::Added::AlreadyThere) => "was already in",
            Err(e) => {
                return format!(
                    "the user chose \"always allow in this profile\" but the profile could not be updated ({e}), so it was {SINGLE}"
                )
            }
        };
        if !ctx.allowed.iter().any(|a| a.eq_ignore_ascii_case(&pattern)) {
            ctx.allowed.push(pattern.clone());
        }
        if ctx.mode < ApprovalMode::Allowlist {
            ctx.mode = ApprovalMode::Allowlist;
        }
        if let Some(l) = record.limits.as_mut() {
            l.approvals = ctx.mode.to_string();
            l.allowed_commands = ctx.allowed.clone();
        }
        self.save_progress(record);
        format!(
            "the user chose \"always allow in this profile\": `{pattern}` {saved} allowed_commands of {}; matching commands are approved without asking from now on (the blocklist still applies)",
            store::paths::clean_display(&path.to_string_lossy())
        )
    }
}

/// What decides the approvals of one run.
struct ApprovalCtx {
    /// Current mode (the effective one, or `auto` after "no volver a preguntar").
    mode: ApprovalMode,
    allowed: Vec<String>,
    scope: OwnedScope,
    /// The run's profile file and where it lives (for `always_profile`).
    profile_path: Option<PathBuf>,
    profile_scope: Option<crate::profiles::Scope>,
    /// Refusals of the driver (read-only mode, credentials) already written to the events log.
    denials_logged: u32,
    /// Requests the harness itself refused (blocklist) or the user denied in Selachi.
    refused: u32,
    last_refused: Option<String>,
    /// Requests refused during the current turn (what the agent is told if it ends the turn early).
    turn_denials: Vec<Denial>,
}

impl ApprovalCtx {
    /// A new turn starts: the driver's refusal counters restart with it.
    fn begin_turn(&mut self) {
        self.denials_logged = 0;
        self.turn_denials.clear();
    }
}

/// What `empty_answer_error` needs to explain a run that said nothing.
struct EmptyAnswer<'a> {
    denied: u32,
    last_denied: Option<&'a PolicyDenial>,
    refused: u32,
    last_refused: Option<&'a str>,
    stop_reason: Option<&'a str>,
    permission: Permission,
    thread_id: i64,
}

fn short_cmd(c: &str) -> String {
    let one: String = c.split_whitespace().collect::<Vec<_>>().join(" ");
    cap_chars(&redacted(&one), 160)
        .replace("\n[...truncated; see the thread for the full answer]", "…")
}

fn denied_note(n: u32, last: &PolicyDenial) -> String {
    let who = if n == 1 {
        "1 comando fue denegado".to_string()
    } else {
        format!("{n} comandos fueron denegados")
    };
    format!(
        "{who} automáticamente, sin preguntar; el último: `{}` ({})",
        short_cmd(&last.command),
        redacted(&last.reason)
    )
}

/// Spanish explanation of a run whose agent ended its turn without any answer text.
fn empty_answer_error(e: &EmptyAnswer<'_>) -> String {
    let mut s = String::from("El agente terminó el turno sin ninguna respuesta.");
    if let (true, Some(last)) = (e.denied > 0, e.last_denied) {
        s.push(' ');
        s.push_str(&denied_note(e.denied, last));
        s.push('.');
    }
    if e.refused > 0 {
        s.push_str(&format!(
            " {} petición(es) fueron rechazadas por la lista de bloqueo o por el usuario en Selachi{}.",
            e.refused,
            e.last_refused
                .map(|l| format!(" (la última: `{}`)", short_cmd(l)))
                .unwrap_or_default()
        ));
    }
    if let Some(sr) = e.stop_reason {
        s.push_str(&format!(" Motivo de parada del agente: `{sr}`."));
    }
    if e.denied > 0 && e.permission == Permission::ReadOnly {
        s.push_str(" Cómo arreglarlo: en solo lectura solo se ejecutan comandos de consulta (listar, leer, buscar, git log/diff/status); \
            relanza con permission=ask o accept_edits (si la sesión que llama lo permite; un perfil también puede subirlo) o pide una tarea que no necesite ejecutar comandos.");
    } else if e.refused > 0 {
        s.push_str(" Cómo arreglarlo: revisa el modo de aprobación (approvals = ask | allowlist | auto, `allowed_commands` del perfil) y responde las peticiones en la cola de atención de Selachi.");
    } else if e.denied == 0 {
        s.push_str(&format!(
            " Puede que el modelo no pudiera responder (límite de uso, sesión del agente caída o un error que no llegó): abre el hilo {} en Selachi para ver qué ocurrió.",
            e.thread_id
        ));
    }
    s
}

/// Why `prep.permission` is what it is, for the caller that must not retry blindly.
fn permission_reason(
    req: &RunRequest,
    prep: &crate::profiles::prepare::Prepared,
    caller: &Caller,
    parent_lowered: Option<&str>,
) -> String {
    if let Some(why) = parent_lowered {
        if prep.permission == caller.permission {
            return why.to_string();
        }
    }
    if let Some(p) = &prep.profile {
        return format!("lo fija el perfil «{p}» (nunca por encima de la sesión que llama ni de [delegation] max_permission)");
    }
    if !req.permission_explicit {
        return "valor por defecto de run_agent; pide permission=ask o accept_edits para más, si la sesión que llama lo permite".to_string();
    }
    if caller.permission == Permission::ReadOnly && prep.permission == Permission::ReadOnly {
        return "pedido; además la sesión que llama tiene permiso de solo lectura, el máximo para sus delegaciones".to_string();
    }
    "pedido por el llamador (como máximo el permiso de la sesión que llama y [delegation] max_permission)".to_string()
}

enum Outcome {
    Done,
    Failed(String),
    Cancelled,
    TimedOut,
    OverBudget(u64),
}
