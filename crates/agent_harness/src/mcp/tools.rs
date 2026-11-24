//! `tools/call` handlers. Every handler returns a [`ToolOutput`]: structured JSON plus a short text
//! summary. Refusals of the harness become MCP tool errors (`isError: true`) that carry the stable
//! refusal `code` and the reason, so the calling model can adapt (lower the permission, pick another
//! agent, wait for the budget) instead of retrying blindly.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::agents::list_agents;
use crate::apply::{ApplyPlan, Confirmation, Risk};
use crate::approvals::{ApprovalBroker, ApprovalRequest, Decision, PipeBroker, PROTOCOL_VERSION};
use crate::output::{cap_chars, diff_stat_of, RunResult, RunStatus};
use crate::policy::{Caller, Permission};
use crate::run::Harness;

use super::runs::{self, Spec, Started};
use super::{schema, CallState};

/// Longest diff returned in one call.
pub const MAX_DIFF_CHARS: usize = 200_000;
/// `tool` name of the confirmation request sent to the Selachi window for `apply_changes`
/// (the window tells it apart from an agent's own tool approvals by this name).
pub const APPLY_TOOL: &str = "selachi:apply_changes";

/// What a tool call produced.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub is_error: bool,
    /// Short human summary (also what a client that ignores `structuredContent` shows the model).
    pub text: String,
    pub structured: Value,
}

impl ToolOutput {
    fn ok(text: impl Into<String>, structured: Value) -> ToolOutput {
        ToolOutput {
            is_error: false,
            text: text.into(),
            structured,
        }
    }
    pub fn error(text: impl Into<String>) -> ToolOutput {
        let text = text.into();
        ToolOutput {
            is_error: true,
            structured: json!({"error": text}),
            text,
        }
    }
}

/// Everything a call needs besides its arguments.
pub struct Ctx {
    pub caller: Caller,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        let mut caller = Caller::from_env();
        caller.channel = "mcp".into();
        Ctx { caller }
    }

    /// A fresh harness per call: it only reads files (config, registry), so it always sees the user's latest
    /// `[delegation]` settings, and nothing is shared between concurrent calls.
    fn harness(&self) -> Result<Harness, String> {
        let b = Harness::builder().caller(self.caller.clone());
        crate::fake::apply_env(b)?.build()
    }
}

/// Dispatch. `None` = no such tool (a protocol error, not a tool error).
pub fn call(ctx: &Ctx, name: &str, args: Value, state: &CallState) -> Option<ToolOutput> {
    let schema = schema::input_schema(name)?;
    let args = if args.is_null() { json!({}) } else { args };
    if let Err(e) = schema::validate(&schema, &args) {
        return Some(ToolOutput::error(format!(
            "invalid arguments for {name}: {e}"
        )));
    }
    let h = match ctx.harness() {
        Ok(h) => h,
        Err(e) => return Some(ToolOutput::error(e)),
    };
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    Some(match name {
        "list_agents" => list(&h, b("models")),
        "list_profiles" => list_profiles(&h, s("cwd").as_deref()),
        "run_agent" => run_agent(&h, &args, state),
        "agent_status" => status(&h, &s("run_id").unwrap_or_default()),
        "agent_result" => result(&h, &s("run_id").unwrap_or_default()),
        "cancel_agent" => cancel(&h, &s("run_id").unwrap_or_default()),
        "get_diff" => diff(&h, &s("run_id").unwrap_or_default(), b("stat")),
        "apply_changes" => apply_changes(&h, &s("run_id").unwrap_or_default()),
        _ => return None,
    })
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn list(h: &Harness, models: bool) -> ToolOutput {
    let list = list_agents(
        h.config(),
        h.caller(),
        h.usage_probe().as_ref(),
        h.data_dir(),
        models,
    );
    ToolOutput::ok(list.to_text(), to_value(&list))
}

/// `list_profiles`: every usable profile with the permission it would really get for this caller.
fn list_profiles(h: &Harness, cwd: Option<&str>) -> ToolOutput {
    let root = match cwd {
        Some(c) => match std::fs::canonicalize(c) {
            Ok(p) => Some(PathBuf::from(store::paths::clean_display(
                &p.to_string_lossy(),
            ))),
            Err(e) => {
                return ToolOutput::error(format!("cwd {c} is not an existing directory: {e}"))
            }
        },
        None => None,
    };
    let lib = h.library(root.as_deref());
    if !lib.settings.enabled {
        return ToolOutput::ok(
            "agent profiles are turned off ([agent_profiles] enabled = false); use run_agent with an explicit agent",
            json!({"profiles": [], "errors": [], "enabled": false}),
        );
    }
    let listing = lib.list();
    let cap = h
        .caller()
        .permission
        .min(h.config().max_permission.min(Permission::AcceptEdits));
    let profiles: Vec<Value> = listing
        .profiles
        .iter()
        .map(|p| {
            let mut v = p.to_value(false);
            let wanted = p.permission.unwrap_or(Permission::ReadOnly);
            v["effective_permission"] = json!(wanted.min(cap).as_str());
            v
        })
        .collect();
    let errors: Vec<Value> = listing
        .errors
        .iter()
        .map(|e| json!({"path": store::paths::clean_display(&e.path.to_string_lossy()), "problems": e.problems}))
        .collect();
    let text = if listing.profiles.is_empty() {
        format!(
            "no profiles found; run_agent works with an explicit agent.{}",
            if listing.errors.is_empty() {
                String::new()
            } else {
                format!(" ({} profile file(s) have errors)", listing.errors.len())
            }
        )
    } else {
        let mut s = String::from("profiles (pass the name as `profile` to run_agent):\n");
        for (p, v) in listing.profiles.iter().zip(&profiles) {
            s.push_str(&format!(
                "- {} [{}]: {} (agent {}, permission {}{}{})\n",
                p.name,
                p.scope.as_str(),
                if p.description.is_empty() {
                    "(no description)"
                } else {
                    &p.description
                },
                p.agent,
                v["effective_permission"].as_str().unwrap_or("read-only"),
                if p.worktree { ", worktree" } else { "" },
                match p.output_format {
                    crate::profiles::OutputFormat::Text => String::new(),
                    f => format!(", output {}", f.as_str()),
                }
            ));
        }
        s
    };
    ToolOutput::ok(
        text,
        json!({"profiles": profiles, "errors": errors, "enabled": true,
               "caller_permission": h.caller().permission.as_str()}),
    )
}

/// The text a model reads for a run result (the CLI's rendering without its CLI-only hint).
fn run_text(r: &RunResult) -> String {
    let text = r.to_text();
    let mut out: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with("next: selachi-agent"))
        .collect();
    if r.worktree_path.is_some() && r.status == RunStatus::Done && !r.files_changed.is_empty() {
        out.push(
            "next: get_diff to review, apply_changes to bring the changes in (the user confirms)",
        );
    }
    out.join("\n")
}

fn run_output(r: &RunResult) -> ToolOutput {
    let mut structured = to_value(r);
    let text = match (r.status, r.code) {
        (RunStatus::Refused, code) => {
            let code = code
                .map(|c| to_value(&c).as_str().unwrap_or("refused").to_string())
                .unwrap_or_else(|| "refused".into());
            format!(
                "refused ({code}): {}",
                r.reason.as_deref().unwrap_or("no reason given")
            )
        }
        _ => run_text(r),
    };
    if let Some(o) = structured.as_object_mut() {
        o.insert("async".into(), Value::Bool(false));
    }
    ToolOutput {
        is_error: r.status != RunStatus::Done,
        text,
        structured,
    }
}

fn run_agent(h: &Harness, args: &Value, state: &CallState) -> ToolOutput {
    let get = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let spec = Spec {
        agent: get("agent").filter(|a| !a.trim().is_empty()),
        profile: get("profile").filter(|p| !p.trim().is_empty()),
        prompt: get("prompt").unwrap_or_default(),
        cwd: PathBuf::from(get("cwd").unwrap_or_default()),
        model: get("model").filter(|m| !m.trim().is_empty()),
        // not given: the profile's permission applies (read-only without a profile)
        permission: get("permission"),
        worktree: args
            .get("worktree")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        timeout_s: args.get("timeout_s").and_then(Value::as_u64),
        budget_tokens: args.get("budget_tokens").and_then(Value::as_u64),
        background: args.get("async").and_then(Value::as_bool).unwrap_or(false),
    };
    if spec.agent.is_none() && spec.profile.is_none() {
        return ToolOutput::error(
            "invalid arguments for run_agent: `agent` is required unless `profile` is given (see list_profiles)",
        );
    }
    match runs::start(&spec, state, h.registry()) {
        Ok(Started::Finished(r)) => run_output(&r),
        Ok(Started::Background { run_id }) => ToolOutput::ok(
            format!("run {run_id} started in the background; poll with agent_status / agent_result (cancel_agent stops it)"),
            json!({
                "run_id": run_id,
                "status": "running",
                "async": true,
                "agent": spec.agent,
                "profile": spec.profile,
                "permission": spec.permission,
            }),
        ),
        Err(e) => ToolOutput::error(e),
    }
}

fn status(h: &Harness, id: &str) -> ToolOutput {
    match h.status(id) {
        Ok(s) => ToolOutput::ok(s.to_text(), to_value(&s)),
        Err(e) => ToolOutput::error(e),
    }
}

fn result(h: &Harness, id: &str) -> ToolOutput {
    let rec = match h.record(id) {
        Ok(r) => r,
        Err(e) => return ToolOutput::error(e),
    };
    if !rec.status.is_final() {
        return ToolOutput::ok(
            format!("run {id} is still running; call agent_result again later or use agent_status"),
            json!({"run_id": id, "status": "running", "agent": rec.agent}),
        );
    }
    match h.result(id) {
        // the result of a finished run is information, not a tool failure: only `run_agent` marks a
        // non-"done" outcome as an error
        Ok(r) => {
            let mut o = run_output(&r);
            o.is_error = false;
            o
        }
        Err(e) => ToolOutput::error(e),
    }
}

fn cancel(h: &Harness, id: &str) -> ToolOutput {
    match h.cancel(id) {
        Ok(out) => {
            let mut v = to_value(&out);
            if let Some(o) = v.as_object_mut() {
                o.insert("run_id".into(), json!(id));
            }
            let text = match &out {
                crate::control::CancelOutcome::AlreadyFinished { status } => {
                    format!("run {id} had already finished ({})", status.as_str())
                }
                crate::control::CancelOutcome::Cancelled => format!("run {id} cancelled"),
                crate::control::CancelOutcome::Killed => {
                    format!("run {id} cancelled (its runner had to be killed)")
                }
            };
            ToolOutput::ok(text, v)
        }
        Err(e) => ToolOutput::error(e),
    }
}

fn diff(h: &Harness, id: &str, stat_only: bool) -> ToolOutput {
    let raw = match h.diff(id) {
        Ok(d) => d,
        Err(e) => return ToolOutput::error(e),
    };
    let stat = diff_stat_of(&raw);
    if stat_only {
        let text = if stat.text.is_empty() {
            "no changes".to_string()
        } else {
            stat.text.clone()
        };
        return ToolOutput::ok(text, json!({"run_id": id, "diff_stat": stat}));
    }
    // what is shown is redacted; `apply` uses the exact patch
    let shown = store::redact(&raw).text;
    let truncated = shown.chars().count() > MAX_DIFF_CHARS;
    let diff = cap_chars(&shown, MAX_DIFF_CHARS);
    let text = if diff.trim().is_empty() {
        "no changes".to_string()
    } else {
        diff.clone()
    };
    ToolOutput::ok(
        text,
        json!({"run_id": id, "diff": diff, "truncated": truncated, "diff_stat": stat}),
    )
}

/// How to apply from a terminal (`<exe> apply <id>`), with the executable quoted.
fn terminal_command(id: &str) -> String {
    let exe = runs::agent_exe().ok();
    match exe {
        Some(p) => format!("\"{}\" apply {id}", p.display()),
        None => format!("selachi-agent apply {id}"),
    }
}

fn approval_request(h: &Harness, plan: &ApplyPlan) -> Option<ApprovalRequest> {
    let rec = h.record(&plan.run_id).ok()?;
    Some(ApprovalRequest {
        v: PROTOCOL_VERSION,
        request_id: format!("apply-{}-{}", plan.run_id, chat_core::now_ms()),
        run_id: plan.run_id.clone(),
        thread_id: rec.thread_id,
        parent_thread_id: rec.parent_thread_id,
        agent: rec.agent,
        tool: APPLY_TOOL.into(),
        detail: cap_chars(&plan.to_text(), 4000),
        permission: rec.permission,
        depth: rec.depth,
        timeout_secs: h.config().approval_timeout_secs,
    })
}

/// `apply_changes`: never silent. Plan only, unless a Selachi window is reachable on the approvals pipe and
/// its user presses allow (a normal-risk plan only; deny on timeout or any failure).
fn apply_changes(h: &Harness, id: &str) -> ToolOutput {
    apply_with(h, id, &PipeBroker::for_current_user(), None)
}

/// [`apply_changes`] with the pipe broker injected (`None` ping = ask the broker's own `ping`).
pub fn apply_with(
    h: &Harness,
    id: &str,
    pipe: &PipeBroker,
    gui_reachable: Option<bool>,
) -> ToolOutput {
    let mut plan = match h.apply_plan(id) {
        Ok(p) => p,
        Err(e) => return ToolOutput::error(e),
    };
    plan.dry_run = true;
    let command = terminal_command(id);
    let plan_json = to_value(&plan);
    let pending = |why: &str| {
        let mut v = json!({
            "run_id": id,
            "applied": false,
            "confirmation": "required",
            "how_to_confirm": format!("the user confirms in the Selachi window or runs: {command}"),
            "plan": plan_json,
        });
        if let Some(o) = v.as_object_mut() {
            o.insert("note".into(), json!(why));
        }
        v
    };

    if plan.already_applied {
        return ToolOutput::ok(
            format!("run {id} was already applied; nothing to do"),
            pending("already applied"),
        );
    }
    if !plan.can_apply {
        let why = plan.conflict.clone().unwrap_or_else(|| {
            if plan.empty {
                "nothing to apply".into()
            } else {
                "cannot be applied".into()
            }
        });
        return ToolOutput::ok(
            format!("NOT applied: {why}\n\n{}", plan.to_text()),
            pending(&why),
        );
    }
    if plan.risk == Risk::Full {
        let why = format!(
            "full-risk changes need a person at a terminal: {}",
            plan.risk_reasons.join("; ")
        );
        return ToolOutput::ok(
            format!(
                "NOT applied. {why}\nAsk the user to review and run: {command}\n\n{}",
                plan.to_text()
            ),
            pending(&why),
        );
    }

    let reachable = gui_reachable.unwrap_or_else(|| pipe.ping());
    if reachable {
        if let Some(req) = approval_request(h, &plan) {
            let verdict = pipe.decide(&req);
            match verdict.decision {
                Decision::Allow | Decision::Always if verdict.forwarded => {
                    return match h.apply(id, Confirmation::Gui) {
                        Ok(report) => ToolOutput::ok(
                            format!(
                                "APPLIED {} file(s) to {} after the user confirmed in Selachi",
                                report.files.len(),
                                report.target
                            ),
                            json!({"run_id": id, "applied": true, "confirmed_by": "selachi_window", "report": to_value(&report)}),
                        ),
                        Err(e) => ToolOutput::error(format!(
                            "the user confirmed but the apply failed: {e}"
                        )),
                    };
                }
                _ => {
                    let why = verdict
                        .reason
                        .unwrap_or_else(|| "the user denied it in Selachi".into());
                    let mut v = pending(&why);
                    if let Some(o) = v.as_object_mut() {
                        o.insert("denied".into(), json!(true));
                    }
                    return ToolOutput::ok(
                        format!("NOT applied: {why}\nThe user can still run: {command}"),
                        v,
                    );
                }
            }
        }
    }
    ToolOutput::ok(
        format!(
            "NOT applied (apply_changes never applies silently). The user must confirm: open the run in Selachi, or run\n  {command}\n\n{}",
            plan.to_text()
        ),
        pending("no Selachi window is reachable to confirm; showing the plan only"),
    )
}
