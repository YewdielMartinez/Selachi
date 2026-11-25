//! From a [`RunRequest`] (plus, optionally, a profile) to what will actually run: the effective limits
//! and the exact final prompt. Shared by `run` and by `profiles dry-run`, so the dry run shows
//! precisely what a real run would send.
//!
//! # Effective limits
//!
//! With a profile, each limit is the **minimum** of every party that has an opinion; a profile can
//! only narrow, never widen:
//!
//! | limit | combination |
//! |---|---|
//! | permission | `min(profile (default read-only), explicit request, parent session, [delegation] max_permission)`; `full` is never delegable |
//! | timeout | `min(profile, request, [delegation] timeout_secs)` |
//! | budget_tokens | `min(profile, request)` (the daily budget applies on top) |
//! | worktree | `profile OR request` (isolation is never a risk) |
//! | depth | not the profile's to set: `[delegation] max_depth` and the caller's depth |
//!
//! A permission that had to be lowered is not a refusal: the run goes ahead with the lower one and a
//! note says so. A request that does not use a profile behaves exactly as before.
//!
//! # The final prompt
//!
//! ```text
//! === SELACHI PROFILE: <name> ===            (instructions of the profile)
//! === CONTEXT FILE: <path> ===               (one block per context file, redacted, <= 200 KiB in all)
//! === OUTPUT FORMAT ===                      (json / diff handoff, with the JSON Schema when there is one)
//! === TOOL POLICY (advisory) ===             (only for agents that cannot enforce allowed/denied tools)
//! === TASK ===
//! <the prompt the caller sent>
//! ```
//!
//! Everything goes through `store::redact` as a whole before it is stored or sent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agents_driver::{AgentKind, ThreadConfig};
use chat_core::{DriverFactory, JobHandle, SpawnedAgent};
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::{Library, OutputFormat, Profile, Scope, MAX_CONTEXT_BYTES};
use crate::agents::canonical_name;
use crate::policy::{Caller, DelegationConfig, Permission, Policy, Refusal, RefusalCode};
use crate::registry::RunLimits;
use crate::run::{Harness, RunRequest};

/// Biggest JSON Schema file read for `output_schema`.
const MAX_SCHEMA_BYTES: u64 = 64 * 1024;

/// One attached context file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInfo {
    /// Project-relative, forward slashes.
    pub path: String,
    /// Bytes attached (after truncation).
    pub bytes: usize,
    pub original_bytes: u64,
    pub truncated: bool,
}

/// Everything decided before the agent starts.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// Normalized project directory.
    pub project_root: PathBuf,
    pub profile: Option<String>,
    pub profile_scope: Option<Scope>,
    pub profile_path: Option<PathBuf>,
    /// Agent as given by the request or the profile (not yet resolved).
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: Permission,
    pub worktree: bool,
    pub timeout: Duration,
    pub budget_tokens: Option<u64>,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    /// The agent can enforce the tool lists (`true` only for Claude).
    pub tools_enforced: bool,
    pub output_format: Option<OutputFormat>,
    /// The caller's prompt, redacted.
    pub task_prompt: String,
    /// What the agent receives, redacted.
    pub final_prompt: String,
    pub context_files: Vec<ContextInfo>,
    /// Things worth telling the caller (a lowered permission, a redaction, an unsupported option).
    pub notes: Vec<String>,
    pub limits: RunLimits,
}

impl Prepared {
    /// JSON view for `profiles dry-run --json`.
    pub fn to_value(&self) -> Value {
        json!({
            "profile": self.profile,
            "profile_scope": self.profile_scope.map(|s| s.as_str()),
            "profile_path": self.profile_path.as_ref().map(|p| store::paths::clean_display(&p.to_string_lossy())),
            "agent": self.agent,
            "model": self.model,
            "effort": self.effort,
            "output_format": self.output_format.map(|f| f.as_str()),
            "limits": self.limits,
            "context_files": self.context_files.iter().map(|c| json!({
                "path": c.path, "bytes": c.bytes, "original_bytes": c.original_bytes, "truncated": c.truncated,
            })).collect::<Vec<_>>(),
            "final_prompt": self.final_prompt,
            "notes": self.notes,
        })
    }

    /// Human rendering for `profiles dry-run`.
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        match (&self.profile, self.profile_path.as_ref()) {
            (Some(n), Some(p)) => {
                let _ = writeln!(
                    s,
                    "profile: {n} ({}: {})",
                    self.profile_scope.map(Scope::as_str).unwrap_or("?"),
                    store::paths::clean_display(&p.to_string_lossy())
                );
            }
            _ => {
                let _ = writeln!(s, "profile: (none)");
            }
        }
        let _ = writeln!(
            s,
            "agent: {}   model: {}   effort: {}",
            self.agent,
            self.model.as_deref().unwrap_or("(default)"),
            self.effort.as_deref().unwrap_or("(default)")
        );
        let l = &self.limits;
        let _ = writeln!(
            s,
            "\nEFFECTIVE LIMITS (the minimum of profile, [delegation] and the calling session)"
        );
        let _ = writeln!(
            s,
            "  permission: {}  (profile: {}, parent session: {}, config max: {})",
            l.permission,
            l.profile_permission.as_deref().unwrap_or("-"),
            l.parent_permission,
            l.config_max_permission
        );
        let _ = writeln!(s, "  depth: {} of {}", l.depth, l.max_depth);
        let _ = writeln!(s, "  timeout: {} s", l.timeout_secs);
        let _ = writeln!(
            s,
            "  budget_tokens: {}   daily budget: {}   max concurrent: {}",
            l.budget_tokens
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".into()),
            if l.daily_token_budget == 0 {
                "unlimited".to_string()
            } else {
                l.daily_token_budget.to_string()
            },
            l.max_concurrent
        );
        let _ = writeln!(s, "  worktree: {}", l.worktree);
        if !l.allowed_tools.is_empty() || !l.denied_tools.is_empty() {
            let _ = writeln!(
                s,
                "  allowed_tools: [{}]  denied_tools: [{}]  ({})",
                l.allowed_tools.join(", "),
                l.denied_tools.join(", "),
                if l.tools_enforced {
                    "enforced by the agent"
                } else {
                    "NOT enforced: this agent cannot"
                }
            );
        }
        if !self.context_files.is_empty() {
            let _ = writeln!(s, "  context files:");
            for c in &self.context_files {
                let _ = writeln!(
                    s,
                    "    {} ({} bytes{})",
                    c.path,
                    c.bytes,
                    if c.truncated { ", truncated" } else { "" }
                );
            }
        }
        for n in &self.notes {
            let _ = writeln!(s, "note: {n}");
        }
        let _ = writeln!(
            s,
            "\nFINAL PROMPT (redacted, exactly what the agent receives)"
        );
        let _ = writeln!(
            s,
            "----------------------------------------------------------------"
        );
        let _ = writeln!(s, "{}", self.final_prompt.trim_end());
        let _ = writeln!(
            s,
            "----------------------------------------------------------------"
        );
        s
    }
}

/// The limits that are not about permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub permission: Permission,
    pub worktree: bool,
    pub timeout: Duration,
    pub budget_tokens: Option<u64>,
}

/// Combine a request, an optional profile, `[delegation]` and the caller (see the module docs).
/// Pure: pushes a note for everything that was lowered.
pub fn combine(
    req: &RunRequest,
    profile: Option<&Profile>,
    cfg: &DelegationConfig,
    caller: &Caller,
    notes: &mut Vec<String>,
) -> Effective {
    let default_timeout = Duration::from_secs(cfg.timeout_secs.max(1));
    let Some(p) = profile else {
        return Effective {
            permission: req.permission,
            worktree: req.worktree,
            timeout: req.timeout.unwrap_or(default_timeout),
            budget_tokens: req.budget_tokens,
        };
    };
    let profile_perm = p.permission.unwrap_or(Permission::ReadOnly);
    let permission = if req.permission_explicit && req.permission == Permission::Full {
        // let the policy refuse it with its own words: `full` is never delegable
        Permission::Full
    } else {
        let wanted = if req.permission_explicit {
            profile_perm.min(req.permission)
        } else {
            profile_perm
        };
        let cap = caller
            .permission
            .min(cfg.max_permission.min(Permission::AcceptEdits));
        let eff = wanted.min(cap);
        if eff < wanted {
            notes.push(format!(
                "profile {:?} asks for permission {wanted}; lowered to {eff} (calling session: {}, [delegation] max_permission: {})",
                p.name,
                caller.permission,
                cfg.max_permission.min(Permission::AcceptEdits)
            ));
        }
        eff
    };
    let mut timeout = default_timeout;
    for t in [p.timeout, req.timeout].into_iter().flatten() {
        timeout = timeout.min(t);
    }
    if let Some(t) = p.timeout.or(req.timeout) {
        if t > default_timeout {
            notes.push(format!(
                "the timeout was lowered to [delegation] timeout_secs = {} s",
                default_timeout.as_secs()
            ));
        }
    }
    let budget_tokens = match (p.budget_tokens, req.budget_tokens) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    Effective {
        permission,
        worktree: p.worktree || req.worktree,
        timeout,
        budget_tokens,
    }
}

/// `true` when `child` is `root` or inside it (both already canonical).
fn inside(root: &Path, child: &Path) -> bool {
    let key = |p: &Path| store::paths::normalize_key(&p.to_string_lossy());
    let (r, c) = (key(root), key(child));
    c == r || c.starts_with(&format!("{}/", r.trim_end_matches('/')))
}

/// Files that look like they hold secrets are never attached, whatever the profile says.
fn looks_like_secret_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".pfx")
        || name.ends_with(".p12")
        || name.ends_with(".kdbx")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name == ".npmrc"
        || name == ".netrc"
        || name.starts_with("credentials")
}

/// A context file ready to be put in the prompt.
#[derive(Debug, Clone)]
pub struct ContextBlock {
    pub info: ContextInfo,
    pub text: String,
}

/// Read `files` (paths inside `project_root`) up to `max_total` bytes in all.
///
/// A path that resolves outside the project (`..`, an absolute path elsewhere, a symlink), a missing
/// file, a directory, a binary file or a file that looks like it holds secrets is an error: a profile
/// must not silently attach less than it says. Past the cap the rest is truncated or left out, with a
/// note.
pub fn collect_context(
    project_root: &Path,
    files: &[String],
    max_total: usize,
    notes: &mut Vec<String>,
) -> Result<Vec<ContextBlock>, String> {
    let root = std::fs::canonicalize(project_root)
        .map_err(|e| format!("cannot resolve the project directory: {e}"))?;
    let mut out: Vec<ContextBlock> = Vec::new();
    let mut used = 0usize;
    for f in files {
        let raw = Path::new(f.trim());
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            root.join(raw)
        };
        let canon = std::fs::canonicalize(&joined).map_err(|e| {
            format!(
                "context file `{f}` cannot be read ({}): {}",
                if e.kind() == std::io::ErrorKind::NotFound {
                    "it does not exist"
                } else {
                    "I/O error"
                },
                e
            )
        })?;
        if !inside(&root, &canon) {
            return Err(format!(
                "context file `{f}` is outside the project ({}); context_files must stay inside the project",
                store::paths::clean_display(&root.to_string_lossy())
            ));
        }
        if !canon.is_file() {
            return Err(format!("context file `{f}` is not a file"));
        }
        if looks_like_secret_file(&canon) {
            return Err(format!(
                "context file `{f}` looks like it holds secrets (.env, keys, credentials) and is never attached"
            ));
        }
        let rel = canon
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| f.clone());
        let original = std::fs::metadata(&canon).map(|m| m.len()).unwrap_or(0);
        let remaining = max_total.saturating_sub(used);
        if remaining == 0 {
            notes.push(format!(
                "context file {rel} was left out: the {} KiB context limit is used up",
                max_total / 1024
            ));
            continue;
        }
        let mut buf = Vec::new();
        {
            use std::io::Read as _;
            let file = std::fs::File::open(&canon)
                .map_err(|e| format!("context file `{f}` cannot be opened: {e}"))?;
            file.take(remaining as u64 + 1)
                .read_to_end(&mut buf)
                .map_err(|e| format!("context file `{f}` cannot be read: {e}"))?;
        }
        if buf.iter().take(8192).any(|b| *b == 0) {
            return Err(format!(
                "context file `{f}` is binary; only text files can be attached"
            ));
        }
        let truncated = buf.len() > remaining;
        buf.truncate(remaining);
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            // a cut in the middle of a character leaves a replacement char at the very end
            while text.ends_with('\u{fffd}') {
                text.pop();
            }
            notes.push(format!(
                "context file {rel} was truncated at the {} KiB context limit",
                max_total / 1024
            ));
        }
        used += buf.len();
        out.push(ContextBlock {
            info: ContextInfo {
                path: rel,
                bytes: buf.len(),
                original_bytes: original,
                truncated,
            },
            text,
        });
    }
    Ok(out)
}

/// Read and check the JSON Schema of `output_schema` (relative to the profile file's folder, must
/// stay inside it). Returns it compact.
pub fn read_schema(profile: &Profile) -> Result<Option<String>, String> {
    let Some(rel) = &profile.output_schema else {
        return Ok(None);
    };
    let dir = std::fs::canonicalize(profile.dir()).map_err(|e| {
        format!(
            "cannot resolve the folder of profile {:?}: {e}",
            profile.name
        )
    })?;
    let path = std::fs::canonicalize(dir.join(rel)).map_err(|e| {
        format!(
            "output_schema `{rel}` of profile {:?} cannot be read: {e}",
            profile.name
        )
    })?;
    if !inside(&dir, &path) {
        return Err(format!(
            "output_schema `{rel}` of profile {:?} is outside the profile's folder",
            profile.name
        ));
    }
    let meta = std::fs::metadata(&path).map_err(|e| format!("output_schema `{rel}`: {e}"))?;
    if !meta.is_file() || meta.len() > MAX_SCHEMA_BYTES {
        return Err(format!(
            "output_schema `{rel}` must be a file of at most {} KiB",
            MAX_SCHEMA_BYTES / 1024
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("output_schema `{rel}`: {e}"))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("output_schema `{rel}` is not valid JSON: {e}"))?;
    Ok(Some(v.to_string()))
}

fn output_block(format: OutputFormat, schema: Option<&str>) -> Option<String> {
    match format {
        OutputFormat::Text => None,
        OutputFormat::Json => Some(match schema {
            Some(s) => format!(
                "Reply with ONLY one JSON document: no prose before or after it and no code fences. \
                 It must be valid against this JSON Schema:\n{s}"
            ),
            None => "Reply with ONLY one valid JSON document: no prose before or after it and no code fences."
                .to_string(),
        }),
        OutputFormat::Diff => Some(
            "Reply with ONLY a unified diff (git format, with `diff --git` headers) of every change you \
             propose, and nothing else."
                .to_string(),
        ),
    }
}

/// Assemble the final prompt (not yet redacted). With nothing to add it is the task itself.
pub fn build_final_prompt(
    profile: &Profile,
    context: &[ContextBlock],
    schema: Option<&str>,
    tool_policy: Option<&str>,
    task: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !profile.instructions.is_empty() {
        parts.push(format!(
            "=== SELACHI PROFILE: {} ===\n{}\n=== END SELACHI PROFILE ===",
            profile.name, profile.instructions
        ));
    }
    for c in context {
        parts.push(format!(
            "=== CONTEXT FILE: {} ===\n{}\n=== END CONTEXT FILE ===",
            c.info.path,
            c.text.trim_end_matches(['\r', '\n'])
        ));
    }
    if let Some(o) = output_block(profile.output_format, schema) {
        parts.push(format!(
            "=== OUTPUT FORMAT ===\n{o}\n=== END OUTPUT FORMAT ==="
        ));
    }
    if let Some(t) = tool_policy {
        parts.push(format!(
            "=== TOOL POLICY (advisory) ===\n{t}\n=== END TOOL POLICY ==="
        ));
    }
    if parts.is_empty() {
        return task.to_string();
    }
    parts.push(format!("=== TASK ===\n{task}"));
    parts.join("\n\n")
}

/// Advisory text for agents that cannot enforce the tool lists.
fn advisory_tool_policy(profile: &Profile) -> Option<String> {
    if profile.allowed_tools.is_empty() && profile.denied_tools.is_empty() {
        return None;
    }
    let mut s = String::from("This is not enforced by the runtime, so respect it yourself.");
    if !profile.allowed_tools.is_empty() {
        s.push_str(&format!(
            " Use only these tools: {}.",
            profile.allowed_tools.join(", ")
        ));
    }
    if !profile.denied_tools.is_empty() {
        s.push_str(&format!(" Never use: {}.", profile.denied_tools.join(", ")));
    }
    Some(s)
}

/// The Claude command-line arguments for the tool lists. They go last (after the driver's own flags),
/// because both options are variadic.
pub fn claude_tool_args(allowed: &[String], denied: &[String]) -> Vec<String> {
    let mut a = Vec::new();
    if !allowed.is_empty() {
        a.push("--allowedTools".to_string());
        a.extend(allowed.iter().cloned());
    }
    if !denied.is_empty() {
        a.push("--disallowedTools".to_string());
        a.extend(denied.iter().cloned());
    }
    a
}

/// `None` when `reply` is one JSON document (a surrounding code fence is tolerated); otherwise what is
/// wrong, as a note for the caller.
pub fn json_reply_problem(reply: &str) -> Option<String> {
    let mut t = reply.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        if let Some(body) = rest.trim().strip_suffix("```") {
            t = body.trim();
        }
    }
    match serde_json::from_str::<Value>(t) {
        Ok(_) => None,
        Err(e) => Some(format!(
            "the agent's final reply is not valid JSON ({e}); the summary holds its raw text"
        )),
    }
}

fn refusal(code: RefusalCode, msg: impl Into<String>) -> Refusal {
    Refusal::new(code, msg)
}

impl Harness {
    /// The profile library of a project (`[agent_profiles]` from `config.toml`).
    pub fn library(&self, project_root: Option<&Path>) -> Library {
        let (settings, _) = super::load_settings(&self.data_dir);
        Library::new(&self.data_dir, project_root, settings)
    }

    /// Decide everything about a run without starting anything: profile, effective limits (policy
    /// rules included), final prompt. See the module docs.
    pub fn prepare(&self, req: &RunRequest) -> Result<Prepared, Refusal> {
        if req.prompt.trim().is_empty() {
            return Err(refusal(RefusalCode::BadRequest, "the prompt is empty"));
        }
        let cwd = crate::run::normalize_dir(&req.cwd).ok_or_else(|| {
            refusal(
                RefusalCode::BadRequest,
                format!("--cwd {} is not an existing directory", req.cwd.display()),
            )
        })?;
        let profile: Option<Profile> = match req.profile.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(name) => Some(
                self.library(Some(&cwd))
                    .find(name)
                    .map_err(|e| refusal(RefusalCode::UnknownProfile, e))?,
            ),
        };
        let agent = {
            let given = req.agent.trim();
            if !given.is_empty() {
                given.to_string()
            } else if let Some(p) = &profile {
                p.agent.clone()
            } else {
                return Err(refusal(
                    RefusalCode::BadRequest,
                    "no agent: name one (claude, codex, opencode, gemini, agy) or use a profile",
                ));
            }
        };
        let mut notes: Vec<String> = Vec::new();
        let eff = combine(req, profile.as_ref(), &self.cfg, &self.caller, &mut notes);

        // the static rules, on the effective permission
        let policy_name = canonical_name(&agent).unwrap_or(agent.as_str()).to_string();
        let permission = Policy {
            cfg: &self.cfg,
            caller: &self.caller,
        }
        .check(&policy_name, eff.permission)?;

        let is_claude = canonical_name(&agent) == Some("claude");
        let (settings, _) = super::load_settings(&self.data_dir);
        let max_context = ((settings.max_context_kb as usize) * 1024).min(MAX_CONTEXT_BYTES);

        let (final_raw, context, effort, model, tools_enforced, output_format) = match &profile {
            None => (
                req.prompt.clone(),
                Vec::new(),
                None,
                req.model.clone(),
                false,
                None,
            ),
            Some(p) => {
                let context = collect_context(&cwd, &p.context_files, max_context, &mut notes)
                    .map_err(|e| {
                        refusal(
                            RefusalCode::InvalidProfile,
                            format!("profile {:?}: {e}", p.name),
                        )
                    })?;
                let schema = read_schema(p).map_err(|e| refusal(RefusalCode::InvalidProfile, e))?;
                let has_tools = !p.allowed_tools.is_empty() || !p.denied_tools.is_empty();
                let enforced = has_tools && is_claude;
                if has_tools && !is_claude {
                    notes.push(format!(
                        "allowed_tools / denied_tools are only enforced for Claude; {agent} cannot, so they are advisory text in the prompt"
                    ));
                }
                let advisory = (has_tools && !is_claude)
                    .then(|| advisory_tool_policy(p))
                    .flatten();
                let final_raw = build_final_prompt(
                    p,
                    &context,
                    schema.as_deref(),
                    advisory.as_deref(),
                    &req.prompt,
                );
                (
                    final_raw,
                    context.into_iter().map(|c| c.info).collect::<Vec<_>>(),
                    p.effort.clone(),
                    req.model.clone().or_else(|| p.model.clone()),
                    enforced,
                    Some(p.output_format),
                )
            }
        };

        // input is redacted before it is stored or sent
        let redaction = store::redact(&final_raw);
        if redaction.count > 0 {
            notes.push(format!(
                "{} secret(s) were hidden in the prompt before sending it ({})",
                redaction.count,
                redaction.kinds.join(", ")
            ));
        }
        let task_prompt = if profile.is_some() {
            store::redact(&req.prompt).text
        } else {
            redaction.text.clone()
        };
        let (allowed_tools, denied_tools) = profile
            .as_ref()
            .map(|p| (p.allowed_tools.clone(), p.denied_tools.clone()))
            .unwrap_or_default();
        let limits = RunLimits {
            permission: permission.to_string(),
            parent_permission: self.caller.permission.to_string(),
            config_max_permission: self
                .cfg
                .max_permission
                .min(Permission::AcceptEdits)
                .to_string(),
            profile_permission: profile
                .as_ref()
                .map(|p| p.permission.unwrap_or(Permission::ReadOnly).to_string()),
            depth: self.caller.depth.saturating_add(1),
            max_depth: self.cfg.max_depth,
            timeout_secs: eff.timeout.as_secs(),
            budget_tokens: eff.budget_tokens,
            daily_token_budget: self.cfg.daily_token_budget,
            max_concurrent: self.cfg.slots(),
            worktree: eff.worktree,
            allowed_tools: allowed_tools.clone(),
            denied_tools: denied_tools.clone(),
            tools_enforced,
        };
        Ok(Prepared {
            project_root: cwd,
            profile_scope: profile.as_ref().map(|p| p.scope),
            profile_path: profile.as_ref().map(|p| p.path.clone()),
            profile: profile.as_ref().map(|p| p.name.clone()),
            agent,
            model,
            effort,
            permission,
            worktree: eff.worktree,
            timeout: eff.timeout,
            budget_tokens: eff.budget_tokens,
            allowed_tools,
            denied_tools,
            tools_enforced,
            output_format,
            task_prompt,
            final_prompt: redaction.text,
            context_files: context,
            notes,
            limits,
        })
    }
}

// ---------------------------------------------------------------------------
// Per-run extra command-line arguments for Claude
// ---------------------------------------------------------------------------

/// Extra agent arguments of the runs in flight, keyed by the directory the agent works in. The chat
/// runtime builds each `ThreadConfig` itself and has no per-thread extra arguments, so the harness
/// wraps the driver factory and adds them when it spawns Claude in that directory.
#[derive(Default)]
pub struct ExtraArgs {
    map: Mutex<HashMap<String, (Vec<String>, usize)>>,
}

/// Removes the registration when the run ends.
pub struct ExtraArgsGuard {
    owner: Arc<ExtraArgs>,
    key: String,
}

impl Drop for ExtraArgsGuard {
    fn drop(&mut self) {
        let mut m = self.owner.map.lock();
        if let Some(e) = m.get_mut(&self.key) {
            e.1 -= 1;
            if e.1 == 0 {
                m.remove(&self.key);
            }
        }
    }
}

fn dir_key(dir: &Path) -> String {
    store::paths::normalize_key(&dir.to_string_lossy())
}

impl ExtraArgs {
    /// Register `args` for the agent that will run in `dir`. Two concurrent runs in the same
    /// directory with different arguments cannot be told apart, so the second is refused.
    pub fn register(
        self: &Arc<Self>,
        dir: &Path,
        args: Vec<String>,
    ) -> Result<ExtraArgsGuard, String> {
        let key = dir_key(dir);
        let mut m = self.map.lock();
        match m.get_mut(&key) {
            Some((existing, n)) if *existing == args => *n += 1,
            Some(_) => {
                return Err(format!(
                    "another delegated run with different tool restrictions is already working in {}; use --worktree to give this one its own directory",
                    dir.display()
                ))
            }
            None => {
                m.insert(key.clone(), (args, 1));
            }
        }
        Ok(ExtraArgsGuard {
            owner: self.clone(),
            key,
        })
    }

    fn for_dir(&self, dir: &Path) -> Option<Vec<String>> {
        self.map.lock().get(&dir_key(dir)).map(|(a, _)| a.clone())
    }
}

/// Wraps a [`DriverFactory`]: Claude gets the registered extra arguments of its directory.
pub struct ExtraArgsFactory {
    pub inner: Arc<dyn DriverFactory>,
    pub extra: Arc<ExtraArgs>,
}

impl DriverFactory for ExtraArgsFactory {
    fn spawn(
        &self,
        kind: &AgentKind,
        cfg: &ThreadConfig,
        job: Option<JobHandle>,
    ) -> agents_driver::Result<SpawnedAgent> {
        if matches!(kind, AgentKind::ClaudeCode) {
            if let Some(extra) = self.extra.for_dir(&cfg.cwd) {
                let mut c = cfg.clone();
                c.extra_args.extend(extra);
                return self.inner.spawn(kind, &c, job);
            }
        }
        self.inner.spawn(kind, cfg, job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileFile;

    fn profile(toml: &str) -> Profile {
        Profile::from_toml(
            toml,
            Path::new("C:/p/.selachi/agents/t.toml"),
            Scope::Project,
        )
        .unwrap()
    }

    fn req() -> RunRequest {
        RunRequest::new("", "do it", ".")
    }

    fn caller(p: Permission) -> Caller {
        Caller {
            permission: p,
            ..Caller::default()
        }
    }

    #[test]
    fn limits_are_the_minimum() {
        let p = profile("name=\"t\"\nagent=\"claude\"\npermission=\"accept-edits\"\ntimeout=\"30m\"\nbudget_tokens=1000\n");
        let cfg = DelegationConfig::default(); // 15 min, accept-edits
        let mut notes = Vec::new();
        // the parent is read-only: lowered, with a note
        let e = combine(
            &req(),
            Some(&p),
            &cfg,
            &caller(Permission::ReadOnly),
            &mut notes,
        );
        assert_eq!(e.permission, Permission::ReadOnly);
        assert!(notes.iter().any(|n| n.contains("lowered to read-only")));
        // timeout: profile 30m > config 15m -> 15m
        assert_eq!(e.timeout, Duration::from_secs(900));
        // budget: the smaller of profile and request
        let mut r = req();
        r.budget_tokens = Some(400);
        assert_eq!(
            combine(
                &r,
                Some(&p),
                &cfg,
                &caller(Permission::AcceptEdits),
                &mut Vec::new()
            )
            .budget_tokens,
            Some(400)
        );
        r.budget_tokens = Some(5000);
        assert_eq!(
            combine(
                &r,
                Some(&p),
                &cfg,
                &caller(Permission::AcceptEdits),
                &mut Vec::new()
            )
            .budget_tokens,
            Some(1000)
        );
        // config max_permission lowers too
        let low = DelegationConfig {
            max_permission: Permission::Ask,
            ..DelegationConfig::default()
        };
        assert_eq!(
            combine(
                &req(),
                Some(&p),
                &low,
                &caller(Permission::AcceptEdits),
                &mut Vec::new()
            )
            .permission,
            Permission::Ask
        );
        // everyone agrees: the profile's permission applies
        assert_eq!(
            combine(
                &req(),
                Some(&p),
                &cfg,
                &caller(Permission::AcceptEdits),
                &mut Vec::new()
            )
            .permission,
            Permission::AcceptEdits
        );
        // an explicit request can narrow but never widen
        let mut narrow = req();
        narrow.permission = Permission::ReadOnly;
        narrow.permission_explicit = true;
        assert_eq!(
            combine(
                &narrow,
                Some(&p),
                &cfg,
                &caller(Permission::AcceptEdits),
                &mut Vec::new()
            )
            .permission,
            Permission::ReadOnly
        );
        let ro = profile("name=\"t\"\nagent=\"claude\"\n");
        let mut wide = req();
        wide.permission = Permission::AcceptEdits;
        wide.permission_explicit = true;
        assert_eq!(
            combine(
                &wide,
                Some(&ro),
                &cfg,
                &caller(Permission::AcceptEdits),
                &mut Vec::new()
            )
            .permission,
            Permission::ReadOnly,
            "a profile that says nothing is read-only and the request cannot raise it"
        );
        // full is left for the policy to refuse
        let mut full = req();
        full.permission = Permission::Full;
        full.permission_explicit = true;
        assert_eq!(
            combine(
                &full,
                Some(&p),
                &cfg,
                &caller(Permission::Full),
                &mut Vec::new()
            )
            .permission,
            Permission::Full
        );
        // worktree is a union
        let wt = profile("name=\"t\"\nagent=\"claude\"\nworktree=true\n");
        assert!(combine(&req(), Some(&wt), &cfg, &Caller::default(), &mut Vec::new()).worktree);
    }

    #[test]
    fn without_a_profile_nothing_changes() {
        let cfg = DelegationConfig::default();
        let mut r = RunRequest::new("codex", "x", ".");
        r.permission = Permission::Ask;
        r.timeout = Some(Duration::from_secs(3600));
        r.budget_tokens = Some(9);
        let e = combine(&r, None, &cfg, &Caller::default(), &mut Vec::new());
        assert_eq!(
            e,
            Effective {
                permission: Permission::Ask,
                worktree: false,
                timeout: Duration::from_secs(3600),
                budget_tokens: Some(9)
            }
        );
    }

    #[test]
    fn prompt_layout_and_handoffs() {
        let p = profile(
            "name=\"Rev\"\nagent=\"claude\"\ninstructions=\"Be strict.\"\noutput_format=\"json\"\n",
        );
        let ctx = vec![ContextBlock {
            info: ContextInfo {
                path: "docs/a.md".into(),
                bytes: 3,
                original_bytes: 3,
                truncated: false,
            },
            text: "abc\n".into(),
        }];
        let s = build_final_prompt(
            &p,
            &ctx,
            Some("{\"type\":\"object\"}"),
            Some("do not edit"),
            "review auth",
        );
        let order: Vec<usize> = [
            "=== SELACHI PROFILE: Rev ===",
            "Be strict.",
            "=== END SELACHI PROFILE ===",
            "=== CONTEXT FILE: docs/a.md ===",
            "=== OUTPUT FORMAT ===",
            "{\"type\":\"object\"}",
            "=== TOOL POLICY (advisory) ===",
            "=== TASK ===\nreview auth",
        ]
        .iter()
        .map(|n| s.find(n).unwrap_or_else(|| panic!("{n} missing in {s}")))
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{s}");
        // a profile with nothing to add leaves the task alone
        let bare = profile("name=\"t\"\nagent=\"claude\"\n");
        assert_eq!(build_final_prompt(&bare, &[], None, None, "task"), "task");
        let diff = profile("name=\"t\"\nagent=\"claude\"\noutput_format=\"diff\"\n");
        assert!(build_final_prompt(&diff, &[], None, None, "t").contains("unified diff"));
    }

    #[test]
    fn json_replies_are_checked() {
        assert!(json_reply_problem("{\"a\": 1}").is_none());
        assert!(json_reply_problem(
            "```json
[1,2]
```"
        )
        .is_none());
        assert!(json_reply_problem("Sure! {\"a\": 1}")
            .unwrap()
            .contains("not valid JSON"));
        assert!(json_reply_problem("").is_some());
    }

    #[test]
    fn claude_args_for_tools() {
        assert_eq!(
            claude_tool_args(&["Read".into(), "Bash(git log:*)".into()], &["Edit".into()]),
            [
                "--allowedTools",
                "Read",
                "Bash(git log:*)",
                "--disallowedTools",
                "Edit"
            ]
        );
        assert!(claude_tool_args(&[], &[]).is_empty());
        assert_eq!(
            claude_tool_args(&[], &["Edit".into()]),
            ["--disallowedTools", "Edit"]
        );
    }

    #[test]
    fn context_files_stay_inside_and_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/a.md"), "alpha\n").unwrap();
        std::fs::write(root.join("big.txt"), "x".repeat(5000)).unwrap();
        std::fs::write(root.join("b.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(root.join(".env"), "TOKEN=1").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "secret").unwrap();
        let mut notes = Vec::new();
        let ok = collect_context(
            &root,
            &["docs/a.md".into(), "big.txt".into(), "docs\\a.md".into()],
            3000,
            &mut notes,
        )
        .unwrap();
        assert_eq!(ok[0].info.path, "docs/a.md");
        assert_eq!(ok[0].text, "alpha\n");
        assert!(ok[1].info.truncated && ok[1].info.bytes == 3000 - 6);
        assert_eq!(
            ok.len(),
            2,
            "the third file found the budget used up: {ok:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("truncated"))
                && notes.iter().any(|n| n.contains("left out"))
        );
        for (bad, why) in [
            ("../outside.txt", "outside the project"),
            ("docs/../../outside.txt", "outside the project"),
            ("nope.txt", "does not exist"),
            ("docs", "not a file"),
            ("b.bin", "binary"),
            (".env", "secrets"),
        ] {
            let e = collect_context(&root, &[bad.into()], 1000, &mut Vec::new()).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        let abs = dir.path().join("outside.txt");
        let e = collect_context(
            &root,
            &[abs.to_string_lossy().into_owned()],
            1000,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(e.contains("outside the project"), "{e}");
        // an absolute path inside the project is fine
        let inside_abs = root.join("docs").join("a.md");
        assert_eq!(
            collect_context(
                &root,
                &[inside_abs.to_string_lossy().into_owned()],
                1000,
                &mut Vec::new()
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn schema_must_be_json_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(agents.join("schemas")).unwrap();
        std::fs::write(agents.join("schemas/ok.json"), "{ \"type\": \"object\" }").unwrap();
        std::fs::write(agents.join("schemas/bad.json"), "{nope").unwrap();
        std::fs::write(dir.path().join("secret.json"), "{}").unwrap();
        let mk = |schema: &str| {
            let f = ProfileFile::parse(&format!(
                "name=\"t\"\nagent=\"claude\"\noutput_format=\"json\"\noutput_schema=\"{schema}\"\n"
            ))
            .unwrap();
            f.validate(&agents.join("t.toml"), Scope::Global).unwrap()
        };
        assert_eq!(
            read_schema(&mk("schemas/ok.json")).unwrap().unwrap(),
            "{\"type\":\"object\"}"
        );
        assert!(read_schema(&mk("schemas/bad.json"))
            .unwrap_err()
            .contains("not valid JSON"));
        assert!(read_schema(&mk("schemas/missing.json"))
            .unwrap_err()
            .contains("cannot be read"));
    }
}
