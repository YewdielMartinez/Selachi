//! Subagent profiles (PLAN section 21.3): named, versionable specializations of a delegation
//! ("Revisor de seguridad", "Tests", "Docs", "Investigador").
//!
//! # Files
//!
//! One TOML file per profile, in two places; a project profile overrides a global one **by name**
//! (case-insensitive):
//!
//! * global: `%APPDATA%\Selachi\agents\*.toml` ([`store::paths::agent_profiles_dir_in`]);
//! * project: `<project>\.selachi\agents\*.toml` ([`store::paths::project_agent_profiles_dir`]), meant to
//!   be committed. `[agent_profiles] project_profiles = false` in `config.toml` ignores them.
//!
//! ```toml
//! name = "Tests"                       # required, <= 64 characters; the identity of the profile
//! description = "Write and fix tests"  # shown by list_profiles so a model can pick
//! agent = "claude"                     # required: claude | codex | opencode | gemini | agy | acp:<command>
//! model = "sonnet"                     # optional
//! effort = "high"                      # optional: low | medium | high | xhigh | max
//! permission = "accept-edits"          # optional (default read-only): read-only | ask | accept-edits
//! worktree = true                      # optional (default false)
//! timeout = "20m"                      # optional: 90 | "90s" | "15m" | "1h"
//! budget_tokens = 400000               # optional: billable tokens
//! instructions = """                   # optional Markdown, prepended to the task in a delimited block
//! Write tests only; never touch production code.
//! """
//! allowed_tools = ["Read", "Grep"]     # optional, Claude only (--allowedTools); others ignore it
//! denied_tools  = ["WebFetch"]         # optional, Claude only (--disallowedTools)
//! approvals = "allowlist"              # optional: ask | allowlist | auto (never above [delegation] approvals)
//! allowed_commands = ["cargo *", "git diff *"]  # optional: shell commands approved on their own (approvals = "allowlist")
//! context_files = ["docs/testing.md"]  # optional, inside the project, <= 200 KiB in total, redacted
//! output_format = "json"               # optional: text (default) | json | diff
//! wait_for_background = true           # optional (default true): a turn that ends while a command is still running
//!                                      # gets one automatic follow-up; if it still is not finished the run is an error
//! agent_preamble = true                # optional (default true): the built-in rules for agents that need them (agy)
//! output_schema = "schemas/out.json"   # optional, json only: a JSON Schema, relative to this file's folder
//! ```
//!
//! Unknown keys are an error (a typo must not silently drop a restriction).
//!
//! # Limits
//!
//! A profile can only **narrow** a delegation: the effective permission, timeout and budget are the
//! minimum of the profile, `[delegation]` and the calling session, and the depth is never the
//! profile's to set (see [`prepare`]).

pub mod cli;
pub mod import;
pub mod prepare;
pub mod templates;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use store::AgentProfiles;

use crate::agents::canonical_name;
use crate::approval_policy::{pattern_problem, ApprovalMode};
use crate::cli::parse_duration;
use crate::policy::Permission;

/// Hard ceiling of the context attached to one run (`context_files`), in bytes.
pub const MAX_CONTEXT_BYTES: usize = 200 * 1024;
/// Longest `instructions` text, in characters.
pub const MAX_INSTRUCTIONS_CHARS: usize = 32_000;
/// Biggest profile file read.
pub const MAX_PROFILE_FILE_BYTES: u64 = 256 * 1024;
/// Most profiles read from one folder (a runaway folder must not stall `list`).
pub const MAX_PROFILES_PER_DIR: usize = 200;
/// Longest `name`.
pub const MAX_NAME_CHARS: usize = 64;

/// Where a profile came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// `%APPDATA%\Selachi\agents`.
    Global,
    /// `<project>\.selachi\agents`.
    Project,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
        }
    }
}

/// What the profile wants back from the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Text,
    Json,
    Diff,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
            OutputFormat::Diff => "diff",
        }
    }
}

/// `timeout` as written in the file: seconds or `"15m"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TimeoutValue {
    Secs(u64),
    Text(String),
}

/// A profile file as it is on disk (strings still unvalidated). Also what the importer and the
/// templates write.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileFile {
    pub name: String,
    pub description: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    pub worktree: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<TimeoutValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
    pub instructions: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    /// `ask | allowlist | auto`; a profile can only lower `[delegation] approvals`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approvals: Option<String>,
    /// Glob patterns of shell commands approved on their own when the mode is `allowlist`.
    pub allowed_commands: Vec<String>,
    pub context_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<String>,
    /// `false` = report a turn that ends with a command still running as done (default `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_background: Option<bool>,
    /// `false` = do not add the built-in per-agent rules to the prompt (default `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_preamble: Option<bool>,
}

/// A validated profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    /// As written (`claude`, `codex`, ..., `acp:<command>`); the resolver turns it into a spawnable agent.
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `None` = the profile says nothing (read-only applies).
    pub permission: Option<Permission>,
    pub worktree: bool,
    pub timeout: Option<Duration>,
    pub budget_tokens: Option<u64>,
    pub instructions: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    /// `None` = the profile says nothing (`[delegation] approvals` applies).
    pub approvals: Option<ApprovalMode>,
    pub allowed_commands: Vec<String>,
    pub context_files: Vec<String>,
    pub output_format: OutputFormat,
    /// Path of the JSON Schema, relative to the profile file's folder.
    pub output_schema: Option<String>,
    /// A turn that ends while a command is still running is followed up, never reported as done.
    pub wait_for_background: bool,
    /// The built-in rules for the agent (`agy`: no background commands, report what changed) are added.
    pub agent_preamble: bool,
    pub scope: Scope,
    /// The file it was read from.
    pub path: PathBuf,
    /// A project profile that replaced a global one of the same name.
    pub shadows_global: bool,
}

/// Why a profile file is not usable: every problem found, each a sentence that says what to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError {
    pub path: PathBuf,
    pub problems: Vec<String>,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.problems.join("; "))
    }
}

impl std::error::Error for ProfileError {}

impl ProfileError {
    fn one(path: &Path, problem: impl Into<String>) -> ProfileError {
        ProfileError {
            path: path.to_path_buf(),
            problems: vec![problem.into()],
        }
    }
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// A relative path with no `..`, no drive and no leading separator: it can only point inside the
/// folder it is resolved against.
pub(crate) fn relative_and_inside(p: &str) -> bool {
    let path = Path::new(p);
    !p.trim().is_empty()
        && !path.is_absolute()
        && !p.starts_with(['/', '\\'])
        && !p.contains(':')
        && !path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn validate_tools(field: &str, list: &[String], problems: &mut Vec<String>) {
    for (i, t) in list.iter().enumerate() {
        let t = t.trim();
        if t.is_empty() || t.len() > 200 || has_control(t) {
            problems.push(format!(
                "{field}[{i}] must be a tool name or pattern (1-200 characters, no line breaks)"
            ));
        } else if t.starts_with('-') {
            problems.push(format!(
                "{field}[{i}] = {t:?} must not start with `-` (it would be read as a command-line flag)"
            ));
        }
    }
}

impl ProfileFile {
    /// Parse a file's text (strict: unknown keys fail).
    pub fn parse(text: &str) -> Result<ProfileFile, String> {
        toml::from_str(text).map_err(|e| {
            // keep the message to the point: no source excerpt
            let msg = e.message().to_string();
            match e.span() {
                Some(span) => {
                    let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
                    format!("line {line}: {msg}")
                }
                None => msg,
            }
        })
    }

    /// Validate every field; `path` / `scope` say where it was read.
    pub fn validate(&self, path: &Path, scope: Scope) -> Result<Profile, ProfileError> {
        let mut problems: Vec<String> = Vec::new();

        let name = self.name.trim().to_string();
        if name.is_empty() {
            problems.push("`name` is required".into());
        } else if name.chars().count() > MAX_NAME_CHARS || has_control(&name) {
            problems.push(format!(
                "`name` must be 1-{MAX_NAME_CHARS} characters with no line breaks"
            ));
        }
        let agent = self.agent.trim().to_string();
        if agent.is_empty() {
            problems.push(
                "`agent` is required (claude, codex, opencode, gemini, agy or acp:<command>)"
                    .into(),
            );
        } else if canonical_name(&agent).is_none() {
            match agent.strip_prefix("acp:") {
                Some(cmd) if !cmd.trim().is_empty() && !has_control(cmd) => {}
                _ => problems.push(format!(
                    "`agent` = {agent:?} is not claude, codex, opencode, gemini, agy or acp:<command>"
                )),
            }
        }
        let model = self
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        if let Some(m) = &model {
            if m.len() > 100 || has_control(m) || m.starts_with('-') {
                problems.push(
                    "`model` must be a model name (<= 100 characters, not starting with `-`)"
                        .into(),
                );
            }
        }
        let effort = self
            .effort
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        if let Some(e) = &effort {
            if !agents_driver::EFFORT_LEVELS.contains(&e.as_str()) {
                problems.push(format!(
                    "`effort` = {e:?} must be one of {}",
                    agents_driver::EFFORT_LEVELS.join(", ")
                ));
            }
        }
        let permission = match self.permission.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(p) => match Permission::parse(p) {
                Some(Permission::Full) => {
                    problems.push(
                        "`permission` = \"full\" is never delegable; use read-only, ask or accept-edits"
                            .into(),
                    );
                    None
                }
                Some(perm) => Some(perm),
                None => {
                    problems.push(format!(
                        "`permission` = {p:?} must be read-only, ask or accept-edits"
                    ));
                    None
                }
            },
        };
        let timeout = match &self.timeout {
            None => None,
            Some(TimeoutValue::Secs(0)) => {
                problems.push("`timeout` must be greater than zero".into());
                None
            }
            Some(TimeoutValue::Secs(s)) => Some(Duration::from_secs(*s)),
            Some(TimeoutValue::Text(t)) => match parse_duration(t) {
                Ok(d) => Some(d),
                Err(e) => {
                    problems.push(format!("`timeout`: {e}"));
                    None
                }
            },
        };
        if timeout.is_some_and(|d| d.as_secs() > 86_400) {
            problems.push("`timeout` must be at most 24 hours".into());
        }
        if self.budget_tokens == Some(0) {
            problems.push("`budget_tokens` must be at least 1".into());
        }
        if self.instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
            problems.push(format!(
                "`instructions` is longer than {MAX_INSTRUCTIONS_CHARS} characters; move the detail into a context file"
            ));
        }
        validate_tools("allowed_tools", &self.allowed_tools, &mut problems);
        validate_tools("denied_tools", &self.denied_tools, &mut problems);
        let approvals = match self.approvals.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(a) => match ApprovalMode::parse(a) {
                Some(m) => Some(m),
                None => {
                    problems.push(format!(
                        "`approvals` = {a:?} must be ask, allowlist or auto"
                    ));
                    None
                }
            },
        };
        for (i, c) in self.allowed_commands.iter().enumerate() {
            if let Some(why) = pattern_problem(c) {
                problems.push(format!("allowed_commands[{i}] = {c:?} {why}"));
            }
        }
        if self.allowed_commands.len() > 200 {
            problems.push("allowed_commands has more than 200 entries".into());
        }
        for (i, f) in self.context_files.iter().enumerate() {
            if f.trim().is_empty() || has_control(f) {
                problems.push(format!("context_files[{i}] must be a file path"));
            } else if f.contains(['*', '?']) {
                problems.push(format!(
                    "context_files[{i}] = {f:?}: globs are not supported, list the files"
                ));
            }
        }
        let output_format = match self.output_format.as_deref().map(str::trim) {
            None | Some("") | Some("text") => OutputFormat::Text,
            Some("json") => OutputFormat::Json,
            Some("diff") => OutputFormat::Diff,
            Some(o) => {
                problems.push(format!(
                    "`output_format` = {o:?} must be text, json or diff"
                ));
                OutputFormat::Text
            }
        };
        let output_schema = self
            .output_schema
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(s) = &output_schema {
            if output_format != OutputFormat::Json {
                problems
                    .push("`output_schema` only makes sense with output_format = \"json\"".into());
            } else if !relative_and_inside(s) {
                problems.push(format!(
                    "`output_schema` = {s:?} must be a relative path inside this file's folder (no `..`, no drive)"
                ));
            }
        }

        if !problems.is_empty() {
            return Err(ProfileError {
                path: path.to_path_buf(),
                problems,
            });
        }
        Ok(Profile {
            name,
            description: self.description.trim().to_string(),
            agent,
            model,
            effort,
            permission,
            worktree: self.worktree,
            timeout,
            budget_tokens: self.budget_tokens,
            instructions: self.instructions.trim().to_string(),
            allowed_tools: self
                .allowed_tools
                .iter()
                .map(|t| t.trim().to_string())
                .collect(),
            denied_tools: self
                .denied_tools
                .iter()
                .map(|t| t.trim().to_string())
                .collect(),
            approvals,
            allowed_commands: self
                .allowed_commands
                .iter()
                .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect(),
            context_files: self
                .context_files
                .iter()
                .map(|t| t.trim().to_string())
                .collect(),
            output_format,
            output_schema,
            wait_for_background: self.wait_for_background.unwrap_or(true),
            agent_preamble: self.agent_preamble.unwrap_or(true),
            scope,
            path: path.to_path_buf(),
            shadows_global: false,
        })
    }
}

impl Profile {
    /// Parse and validate the text of a profile file.
    pub fn from_toml(text: &str, path: &Path, scope: Scope) -> Result<Profile, ProfileError> {
        ProfileFile::parse(text)
            .map_err(|e| ProfileError::one(path, format!("not a valid profile: {e}")))?
            .validate(path, scope)
    }

    /// Read and validate one file.
    pub fn load(path: &Path, scope: Scope) -> Result<Profile, ProfileError> {
        let meta = std::fs::metadata(path)
            .map_err(|e| ProfileError::one(path, format!("cannot read it: {e}")))?;
        if meta.len() > MAX_PROFILE_FILE_BYTES {
            return Err(ProfileError::one(
                path,
                format!(
                    "the file is larger than {} KiB; a profile is a short TOML file",
                    MAX_PROFILE_FILE_BYTES / 1024
                ),
            ));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProfileError::one(path, format!("cannot read it: {e}")))?;
        Profile::from_toml(&text, path, scope)
    }

    /// File name without extension (`tests` for `tests.toml`).
    pub fn slug(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The folder `output_schema` is relative to.
    pub fn dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// JSON view (what `profiles show --json` and `list_profiles` print). `instructions` is included
    /// only when `full`.
    pub fn to_value(&self, full: bool) -> Value {
        let mut v = json!({
            "name": self.name,
            "description": self.description,
            "agent": self.agent,
            "model": self.model,
            "effort": self.effort,
            "permission": self.permission.map(|p| p.as_str()),
            "worktree": self.worktree,
            "timeout_secs": self.timeout.map(|d| d.as_secs()),
            "budget_tokens": self.budget_tokens,
            "allowed_tools": self.allowed_tools,
            "denied_tools": self.denied_tools,
            "approvals": self.approvals.map(|a| a.as_str()),
            "allowed_commands": self.allowed_commands,
            "context_files": self.context_files,
            "output_format": self.output_format.as_str(),
            "output_schema": self.output_schema,
            "wait_for_background": self.wait_for_background,
            "agent_preamble": self.agent_preamble,
            "has_instructions": !self.instructions.is_empty(),
            "scope": self.scope.as_str(),
            "path": store::paths::clean_display(&self.path.to_string_lossy()),
            "shadows_global": self.shadows_global,
        });
        if full {
            v["instructions"] = json!(self.instructions);
        }
        v
    }
}

/// What a folder scan found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    /// Valid profiles, project ones first replacing global ones of the same name; sorted by name.
    pub profiles: Vec<Profile>,
    /// Files that could not be used (they never stop the others).
    pub errors: Vec<ProfileError>,
}

/// The two profile folders of a project plus the `[agent_profiles]` settings.
#[derive(Debug, Clone)]
pub struct Library {
    pub global_dir: PathBuf,
    pub project_dir: Option<PathBuf>,
    pub settings: AgentProfiles,
}

/// `[agent_profiles]` of `<data_dir>/config.toml`, read directly from the file (like `[delegation]`):
/// defaults when the file or the section is missing; a section that does not parse gives the defaults
/// and a warning.
pub fn load_settings(data_dir: &Path) -> (AgentProfiles, Option<String>) {
    let path = store::paths::config_path_in(data_dir);
    let Ok(text) = std::fs::read_to_string(path) else {
        return (AgentProfiles::default(), None);
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return (AgentProfiles::default(), None); // DelegationConfig already warns about a broken file
    };
    let Some(section) = value.get("agent_profiles") else {
        return (AgentProfiles::default(), None);
    };
    match section.clone().try_into::<AgentProfiles>() {
        Ok(mut s) => {
            s.max_context_kb = s
                .max_context_kb
                .clamp(1, store::config::MAX_PROFILE_CONTEXT_KB);
            (s, None)
        }
        Err(e) => (
            AgentProfiles::default(),
            Some(format!(
                "[agent_profiles] in config.toml is invalid ({e}); using the defaults"
            )),
        ),
    }
}

fn scan_dir(dir: &Path, scope: Scope) -> (Vec<Profile>, Vec<ProfileError>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
        })
        .collect();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    let mut profiles: Vec<Profile> = Vec::new();
    let mut errors = Vec::new();
    for path in files.into_iter().take(MAX_PROFILES_PER_DIR) {
        match Profile::load(&path, scope) {
            Ok(p) => {
                if let Some(first) = profiles
                    .iter()
                    .find(|q| q.name.eq_ignore_ascii_case(&p.name))
                {
                    errors.push(ProfileError::one(
                        &path,
                        format!(
                            "the name {:?} is already used by {}; names must be unique within a folder",
                            p.name,
                            first.path.display()
                        ),
                    ));
                } else {
                    profiles.push(p);
                }
            }
            Err(e) => errors.push(e),
        }
    }
    (profiles, errors)
}

impl Library {
    /// Library for a project (`project_root` = its directory; `None` = only the global profiles).
    pub fn new(data_dir: &Path, project_root: Option<&Path>, settings: AgentProfiles) -> Library {
        Library {
            global_dir: store::paths::agent_profiles_dir_in(data_dir),
            project_dir: project_root.map(store::paths::project_agent_profiles_dir),
            settings,
        }
    }

    /// Every usable profile (project replaces global by name) and every file that is not usable.
    pub fn list(&self) -> Listing {
        if !self.settings.enabled {
            return Listing::default();
        }
        let (mut profiles, mut errors) = match (&self.project_dir, self.settings.project_profiles) {
            (Some(dir), true) => scan_dir(dir, Scope::Project),
            _ => (Vec::new(), Vec::new()),
        };
        let (global, gerrors) = scan_dir(&self.global_dir, Scope::Global);
        errors.extend(gerrors);
        for g in global {
            if let Some(p) = profiles
                .iter_mut()
                .find(|p| p.name.eq_ignore_ascii_case(&g.name))
            {
                p.shadows_global = true;
            } else {
                profiles.push(g);
            }
        }
        profiles.sort_by_key(|p| p.name.to_lowercase());
        Listing { profiles, errors }
    }

    /// Find a profile by name (case-insensitive) or by file name (`tests` for `tests.toml`).
    pub fn find(&self, name: &str) -> Result<Profile, String> {
        if !self.settings.enabled {
            return Err(
                "agent profiles are turned off ([agent_profiles] enabled = false in config.toml)"
                    .into(),
            );
        }
        let want = name.trim().to_lowercase();
        let listing = self.list();
        if let Some(p) = listing
            .profiles
            .iter()
            .find(|p| p.name.to_lowercase() == want)
            .or_else(|| {
                listing
                    .profiles
                    .iter()
                    .find(|p| p.slug().to_lowercase() == want)
            })
        {
            return Ok(p.clone());
        }
        let names: Vec<&str> = listing.profiles.iter().map(|p| p.name.as_str()).collect();
        let mut msg = format!("no profile named `{}`", name.trim());
        if names.is_empty() {
            msg.push_str(&format!(
                " (none found in {}{}; `selachi-agent profiles init` writes starter profiles)",
                self.global_dir.display(),
                self.project_dir
                    .as_ref()
                    .filter(|_| self.settings.project_profiles)
                    .map(|d| format!(" or {}", d.display()))
                    .unwrap_or_default()
            ));
        } else {
            msg.push_str(&format!(" (available: {})", names.join(", ")));
        }
        if !listing.errors.is_empty() {
            msg.push_str(&format!(
                "; {} profile file(s) have errors, see `selachi-agent profiles list`",
                listing.errors.len()
            ));
        }
        Err(msg)
    }
}

/// File name for a profile name: lowercase ASCII letters and digits separated by `-`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in name.chars() {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' | 'Á' | 'À' | 'Ä' | 'Â' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'Ó' | 'Ò' | 'Ö' | 'Ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            other => other,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "profile".into()
    } else {
        out.chars().take(48).collect()
    }
}

/// TOML text of a profile file: single-line strings as basic strings, `instructions` as a readable
/// multi-line literal when it can be one. Round-trips through [`ProfileFile::parse`].
pub fn emit_toml(p: &ProfileFile, header: Option<&str>) -> String {
    fn s(v: &str) -> String {
        toml::Value::String(v.to_string()).to_string()
    }
    fn list(items: &[String]) -> String {
        format!(
            "[{}]",
            items.iter().map(|i| s(i)).collect::<Vec<_>>().join(", ")
        )
    }
    let mut out = String::new();
    if let Some(h) = header {
        for line in h.lines() {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&format!("name = {}\n", s(&p.name)));
    out.push_str(&format!("description = {}\n", s(&p.description)));
    out.push_str(&format!("agent = {}\n", s(&p.agent)));
    if let Some(m) = &p.model {
        out.push_str(&format!("model = {}\n", s(m)));
    }
    if let Some(e) = &p.effort {
        out.push_str(&format!("effort = {}\n", s(e)));
    }
    if let Some(perm) = &p.permission {
        out.push_str(&format!("permission = {}\n", s(perm)));
    }
    out.push_str(&format!("worktree = {}\n", p.worktree));
    match &p.timeout {
        Some(TimeoutValue::Secs(n)) => out.push_str(&format!("timeout = {n}\n")),
        Some(TimeoutValue::Text(t)) => out.push_str(&format!("timeout = {}\n", s(t))),
        None => {}
    }
    if let Some(b) = p.budget_tokens {
        out.push_str(&format!("budget_tokens = {b}\n"));
    }
    if !p.allowed_tools.is_empty() {
        out.push_str(&format!("allowed_tools = {}\n", list(&p.allowed_tools)));
    }
    if !p.denied_tools.is_empty() {
        out.push_str(&format!("denied_tools = {}\n", list(&p.denied_tools)));
    }
    if let Some(a) = &p.approvals {
        out.push_str(&format!("approvals = {}\n", s(a)));
    }
    if !p.allowed_commands.is_empty() {
        out.push_str(&format!(
            "allowed_commands = {}\n",
            list(&p.allowed_commands)
        ));
    }
    if !p.context_files.is_empty() {
        out.push_str(&format!("context_files = {}\n", list(&p.context_files)));
    }
    if let Some(f) = &p.output_format {
        out.push_str(&format!("output_format = {}\n", s(f)));
    }
    if let Some(f) = &p.output_schema {
        out.push_str(&format!("output_schema = {}\n", s(f)));
    }
    if let Some(b) = p.wait_for_background {
        out.push_str(&format!("wait_for_background = {b}\n"));
    }
    if let Some(b) = p.agent_preamble {
        out.push_str(&format!("agent_preamble = {b}\n"));
    }
    let ins = p.instructions.trim_end();
    if !ins.is_empty() {
        let literal_ok = !ins.contains("'''")
            && !ins
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t');
        // a trailing quote would merge with the closing delimiter
        if literal_ok && !ins.ends_with('\'') {
            out.push_str(&format!(
                "instructions = '''\n{}\n'''\n",
                ins.replace("\r\n", "\n")
            ));
        } else {
            out.push_str(&format!("instructions = {}\n", s(ins)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Profile, ProfileError> {
        Profile::from_toml(text, Path::new("C:/x/tests.toml"), Scope::Global)
    }

    #[test]
    fn a_full_profile_parses() {
        let p = parse(
            r#"
name = "Tests"
description = "Write tests"
agent = "Claude"
model = "sonnet"
effort = "HIGH"
permission = "accept_edits"
worktree = true
timeout = "20m"
budget_tokens = 400000
instructions = "Only tests."
allowed_tools = ["Read", "Bash(git diff:*)"]
denied_tools = ["WebFetch"]
context_files = ["docs/testing.md"]
output_format = "json"
output_schema = "schemas/out.json"
"#,
        )
        .unwrap();
        assert_eq!(p.name, "Tests");
        assert_eq!(p.permission, Some(Permission::AcceptEdits));
        assert_eq!(p.effort.as_deref(), Some("high"));
        assert_eq!(p.timeout, Some(Duration::from_secs(1200)));
        assert_eq!(p.output_format, OutputFormat::Json);
        assert!(p.worktree && p.budget_tokens == Some(400_000));
        assert_eq!(p.allowed_tools, ["Read", "Bash(git diff:*)"]);
        // numeric timeout
        assert_eq!(
            parse("name=\"a\"\nagent=\"codex\"\ntimeout = 90\n")
                .unwrap()
                .timeout,
            Some(Duration::from_secs(90))
        );
        // minimal
        let m = parse("name=\"a\"\nagent=\"acp:foo --acp\"\n").unwrap();
        assert!(m.permission.is_none() && m.output_format == OutputFormat::Text);
    }

    #[test]
    fn approvals_and_allowed_commands_are_parsed_and_validated() {
        let p = parse(
            "name=\"a\"
agent=\"codex\"
approvals = \"Allowlist\"
allowed_commands = [\"cargo *\", \"git   diff *\", \"rg *\"]
",
        )
        .unwrap();
        assert_eq!(p.approvals, Some(ApprovalMode::Allowlist));
        assert_eq!(p.allowed_commands, ["cargo *", "git diff *", "rg *"]);
        assert_eq!(p.to_value(false)["approvals"], "allowlist");
        // not set: the config's mode applies
        let m = parse(
            "name=\"a\"
agent=\"codex\"
",
        )
        .unwrap();
        assert!(m.approvals.is_none() && m.allowed_commands.is_empty());
        // problems
        let e = parse(
            "name=\"a\"
agent=\"codex\"
approvals = \"yolo\"
",
        )
        .unwrap_err();
        assert!(e
            .problems
            .iter()
            .any(|p| p.contains("ask, allowlist or auto")));
        let e = parse(
            "name=\"a\"
agent=\"codex\"
allowed_commands = [\"cargo test; rm x\", \"\", \"*\"]
",
        )
        .unwrap_err();
        assert_eq!(e.problems.len(), 3, "{:?}", e.problems);
        // an unknown key is still an error (a typo must not drop a restriction)
        assert!(parse(
            "name=\"a\"
agent=\"codex\"
allow_commands = []
"
        )
        .is_err());
        // the TOML written by the editor reads back
        let f = ProfileFile {
            name: "a".into(),
            agent: "codex".into(),
            approvals: Some("auto".into()),
            allowed_commands: vec!["cargo *".into()],
            ..ProfileFile::default()
        };
        let back = ProfileFile::parse(&emit_toml(&f, None)).unwrap();
        assert_eq!(back.approvals.as_deref(), Some("auto"));
        assert_eq!(back.allowed_commands, ["cargo *"]);
    }

    #[test]
    fn validation_collects_clear_errors() {
        let e = parse("description = \"x\"\n").unwrap_err();
        assert!(e.problems.iter().any(|p| p.contains("`name` is required")));
        assert!(e.problems.iter().any(|p| p.contains("`agent` is required")));
        let e = parse(
            "name=\"a\"\nagent=\"gpt\"\npermission=\"full\"\neffort=\"extreme\"\ntimeout=\"soon\"\nbudget_tokens=0\noutput_format=\"xml\"\nallowed_tools=[\"--x\"]\ncontext_files=[\"src/*.rs\"]\n",
        )
        .unwrap_err();
        let all = e.problems.join("\n");
        for needle in [
            "`agent` = \"gpt\"",
            "never delegable",
            "`effort`",
            "`timeout`",
            "`budget_tokens`",
            "`output_format`",
            "must not start with `-`",
            "globs are not supported",
        ] {
            assert!(all.contains(needle), "{needle} missing in {all}");
        }
        assert!(parse("name=\"a\"\nagent=\"codex\"\npermission=\"root\"\n")
            .unwrap_err()
            .to_string()
            .contains("read-only, ask or accept-edits"));
        // unknown keys fail instead of silently dropping a restriction
        let e = parse("name=\"a\"\nagent=\"codex\"\npermision=\"read-only\"\n").unwrap_err();
        assert!(e.to_string().contains("permision"), "{e}");
        assert!(parse("name = ")
            .unwrap_err()
            .to_string()
            .contains("not a valid profile"));
        // schema only with json, and only inside the folder
        assert!(parse("name=\"a\"\nagent=\"codex\"\noutput_schema=\"s.json\"\n").is_err());
        assert!(parse(
            "name=\"a\"\nagent=\"codex\"\noutput_format=\"json\"\noutput_schema=\"../s.json\"\n"
        )
        .is_err());
        assert!(parse(
            "name=\"a\"\nagent=\"codex\"\noutput_format=\"json\"\noutput_schema=\"C:/s.json\"\n"
        )
        .is_err());
    }

    #[test]
    fn project_overrides_global_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let proj = dir.path().join("proj");
        let g = store::paths::agent_profiles_dir_in(&data);
        let pd = store::paths::project_agent_profiles_dir(&proj);
        std::fs::create_dir_all(&g).unwrap();
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(g.join("tests.toml"), "name=\"Tests\"\nagent=\"codex\"\n").unwrap();
        std::fs::write(g.join("docs.toml"), "name=\"Docs\"\nagent=\"codex\"\n").unwrap();
        std::fs::write(pd.join("mine.toml"), "name=\"tests\"\nagent=\"claude\"\n").unwrap();
        std::fs::write(g.join("broken.toml"), "name=\"B\"\n").unwrap();
        std::fs::write(g.join("notes.txt"), "ignored").unwrap();
        let lib = Library::new(&data, Some(&proj), AgentProfiles::default());
        let l = lib.list();
        assert_eq!(l.profiles.len(), 2, "{:?}", l.profiles);
        assert_eq!(l.errors.len(), 1);
        let t = lib.find("TESTS").unwrap();
        assert_eq!(
            (t.agent.as_str(), t.scope, t.shadows_global),
            ("claude", Scope::Project, true)
        );
        assert_eq!(lib.find("docs").unwrap().scope, Scope::Global);
        let e = lib.find("nope").unwrap_err();
        assert!(
            e.contains("available: Docs, tests") && e.contains("1 profile file"),
            "{e}"
        );
        // project profiles can be switched off
        let off = Library::new(
            &data,
            Some(&proj),
            AgentProfiles {
                project_profiles: false,
                ..AgentProfiles::default()
            },
        );
        assert_eq!(off.find("tests").unwrap().agent, "codex");
        // and the whole feature too
        let none = Library::new(
            &data,
            Some(&proj),
            AgentProfiles {
                enabled: false,
                ..AgentProfiles::default()
            },
        );
        assert!(none.list().profiles.is_empty());
        assert!(none.find("tests").unwrap_err().contains("turned off"));
        // a duplicate name in one folder is reported, not silently merged
        std::fs::write(g.join("tests2.toml"), "name=\"tests\"\nagent=\"codex\"\n").unwrap();
        let l = Library::new(&data, None, AgentProfiles::default()).list();
        assert!(l
            .errors
            .iter()
            .any(|e| e.to_string().contains("already used")));
    }

    #[test]
    fn slugs_and_emit_round_trip() {
        assert_eq!(slugify("Revisor de seguridad"), "revisor-de-seguridad");
        assert_eq!(slugify("  Investigación #2 "), "investigacion-2");
        assert_eq!(slugify("***"), "profile");
        let f = ProfileFile {
            name: "Q\"uote".into(),
            description: "line one".into(),
            agent: "claude".into(),
            model: Some("sonnet".into()),
            permission: Some("read-only".into()),
            timeout: Some(TimeoutValue::Text("5m".into())),
            instructions: "# Title\nuse `x`\n'''triple'''\nend'".into(),
            allowed_tools: vec!["Read".into(), "Bash(git log:*)".into()],
            ..ProfileFile::default()
        };
        let text = emit_toml(&f, Some("generated"));
        let back = ProfileFile::parse(&text).unwrap();
        assert_eq!(back.name, f.name);
        assert_eq!(back.instructions.trim_end(), f.instructions.trim_end());
        assert_eq!(back.allowed_tools, f.allowed_tools);
        let f2 = ProfileFile {
            instructions: "plain\nmulti\nline".into(),
            ..f
        };
        let text = emit_toml(&f2, None);
        assert!(text.contains("instructions = '''\nplain"));
        assert_eq!(
            ProfileFile::parse(&text).unwrap().instructions.trim_end(),
            f2.instructions
        );
    }
}
