//! The model catalog: which models (and reasoning efforts) each agent offers, **asked to the agent
//! itself**, cached on disk with a TTL and refreshed on demand.
//!
//! # Where each list comes from
//!
//! | agent | source | what it gives |
//! |---|---|---|
//! | `agy` | `agy models` (no session, no prompt) | `<family>-<effort>` ids folded into one entry per family with its efforts |
//! | `codex` | app-server `model/list` | models, supported efforts, default effort |
//! | `opencode` | `opencode models` (falls back to the ACP `session/new` handshake) | `provider/model` ids |
//! | `gemini` | `gemini --list-models` (falls back to the ACP handshake) | model ids |
//! | `claude` | `claude --help` (the `--model` aliases and the `--effort` levels it documents), else the built-in aliases | aliases + efforts for every model; any full model name is accepted too |
//!
//! Nothing here runs at idle and nothing runs on its own: [`cached`] / [`load`] only read
//! `models-catalog.json` in the data folder, [`refresh`] / [`ensure`] start the agent CLI and block
//! (call them from a background thread, never from a UI thread or a render).
//!
//! # Models and efforts
//!
//! Antigravity bakes the effort into the model id (`gemini-4-neon-medium`); the catalog folds those
//! into a **family** (`gemini-4-neon`, efforts `low|medium|high|xhigh`). [`AgentModels::resolve`]
//! takes a model and an effort given **separately** (or a full id such as `gemini-4-neon-medium`)
//! and returns the pair the agent wants plus the real id, or a [`SelectionError`] that lists the
//! valid options.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::antigravity::{split_effort_suffix, thought_level_for};
use crate::types::{AgentKind, DriverError, ModelInfo, Result, EFFORT_LEVELS};

/// File name inside the data folder.
pub const CATALOG_FILE: &str = "models-catalog.json";
/// How long a fetched list is trusted (`ensure` refetches after this).
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);
/// How long one fetch may take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(25);
/// The agents the catalog knows.
pub const AGENTS: [&str; 5] = ["claude", "codex", "opencode", "gemini", "agy"];
/// `claude --model` aliases when `claude --help` cannot be read.
pub const CLAUDE_ALIASES: [&str; 3] = ["sonnet", "opus", "haiku"];

fn yes() -> bool {
    true
}

/// One entry of an agent's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    /// What to pass back as the model (a family id for Antigravity: `gemini-4-neon`).
    pub id: String,
    /// Human name (`Gemini 4 Neon`).
    pub label: String,
    /// Efforts this model supports (empty = not selectable).
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    /// The agent's own default model (known only when the agent says so).
    #[serde(default)]
    pub default: bool,
    /// Offered right now (`false` for a model the agent lists but cannot use).
    #[serde(default = "yes")]
    pub available: bool,
}

impl CatalogModel {
    pub fn from_info(i: &ModelInfo) -> CatalogModel {
        CatalogModel {
            id: i.id.clone(),
            label: i.label.clone(),
            efforts: i.efforts.clone(),
            default_effort: i.default_effort.clone(),
            default: false,
            available: true,
        }
    }

    pub fn to_info(&self) -> ModelInfo {
        ModelInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            efforts: self.efforts.clone(),
            default_effort: self.default_effort.clone(),
        }
    }
}

/// What one agent offers, as of `fetched_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModels {
    pub agent: String,
    /// Unix ms of the last successful fetch (0 = never).
    pub fetched_ms: i64,
    /// Where the list came from (`agy models`, `codex model/list`...).
    pub source: String,
    pub models: Vec<CatalogModel>,
    /// Efforts that apply to every model when the models carry none (Claude `--effort`).
    #[serde(default)]
    pub efforts: Vec<String>,
    /// The agent also accepts model names that are not listed (Claude full names), so an unknown
    /// name is passed through instead of refused.
    #[serde(default)]
    pub open: bool,
    /// Why the last refresh failed (the models are then the previous ones).
    #[serde(default)]
    pub error: Option<String>,
}

/// The pair an agent is started with, and the real id it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// What goes to the agent as the model (a family id for Antigravity); `None` = the agent's default.
    pub model: Option<String>,
    /// What goes to the agent as the effort.
    pub effort: Option<String>,
    /// The agent's own id for the pair (`gemini-4-neon-medium`); equals `model` when effort is not
    /// part of the id.
    pub real_id: Option<String>,
    /// Things worth telling the caller (`max` mapped to `xhigh`, an unlisted Claude name...).
    pub notes: Vec<String>,
}

/// A model or effort the agent does not offer. The text lists what is valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionError(pub String);

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SelectionError {}

/// Unix ms now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// Levenshtein-free closeness: shared prefix / containment score, enough to suggest `gemini-4-neon`
/// for `gemini-4-neo`.
fn closeness(want: &str, have: &str) -> usize {
    let (w, h) = (norm(want), norm(have));
    if h.contains(&w) || w.contains(&h) {
        return 1000 + h.len().min(w.len());
    }
    w.chars().zip(h.chars()).take_while(|(a, b)| a == b).count()
}

impl AgentModels {
    pub fn empty(agent: &str) -> AgentModels {
        AgentModels {
            agent: agent.to_string(),
            fetched_ms: 0,
            source: String::new(),
            models: Vec::new(),
            efforts: Vec::new(),
            open: false,
            error: None,
        }
    }

    /// Fetched within `ttl` of `now_ms` (and at least once).
    pub fn is_fresh(&self, now_ms: i64, ttl: Duration) -> bool {
        self.fetched_ms > 0
            && !self.models.is_empty()
            && now_ms.saturating_sub(self.fetched_ms) < ttl.as_millis() as i64
    }

    /// Ids a person may type, for messages.
    pub fn ids(&self) -> Vec<&str> {
        self.models
            .iter()
            .filter(|m| m.available)
            .map(|m| m.id.as_str())
            .collect()
    }

    /// The entry for `id`: the family or model id, a full `<family>-<effort>` id, or the label,
    /// case-insensitively. The effort carried by a full id is returned with it.
    pub fn find(&self, id: &str) -> Option<(&CatalogModel, Option<String>)> {
        let want = norm(id);
        if want.is_empty() {
            return None;
        }
        if let Some(m) = self.models.iter().find(|m| norm(&m.id) == want) {
            return Some((m, None));
        }
        if let Some((base, eff)) = split_effort_suffix(&want) {
            if let Some(m) = self
                .models
                .iter()
                .find(|m| norm(&m.id) == base && m.efforts.iter().any(|e| e == eff))
            {
                return Some((m, Some(eff.to_string())));
            }
        }
        self.models
            .iter()
            .find(|m| norm(&m.label) == want)
            .map(|m| (m, None))
    }

    /// The agent's default model, when it said which.
    pub fn default_model(&self) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.default && m.available)
    }

    /// Efforts selectable for `model` (or, without one, for any model / the agent in general),
    /// low to high.
    pub fn effort_options(&self, model: Option<&str>) -> Vec<String> {
        let mut v: Vec<String> = match model.and_then(|m| self.find(m)) {
            Some((m, _)) if !m.efforts.is_empty() => m.efforts.clone(),
            Some(_) => self.efforts.clone(),
            None => {
                let mut all = self.efforts.clone();
                for m in &self.models {
                    for e in &m.efforts {
                        if !all.contains(e) {
                            all.push(e.clone());
                        }
                    }
                }
                all
            }
        };
        v.sort_by_key(|e| {
            EFFORT_LEVELS
                .iter()
                .position(|l| l == e)
                .unwrap_or(usize::MAX)
        });
        v.dedup();
        v
    }

    /// Up to `n` listed ids closest to `wanted`.
    pub fn suggestions(&self, wanted: &str, n: usize) -> Vec<String> {
        let mut scored: Vec<(usize, &str)> = self
            .models
            .iter()
            .filter(|m| m.available)
            .map(|m| (closeness(wanted, &m.id), m.id.as_str()))
            .collect();
        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        scored
            .into_iter()
            .take(n)
            .map(|(_, id)| id.to_string())
            .collect()
    }

    /// `true` when the effort is part of the model id the agent knows (Antigravity families).
    fn effort_in_id(&self, m: &CatalogModel) -> bool {
        self.agent == "agy" && !m.efforts.is_empty()
    }

    fn list_for_message(&self) -> String {
        let ids = self.ids();
        if ids.is_empty() {
            return "(the agent listed no models)".into();
        }
        let shown: Vec<&str> = ids.iter().take(40).copied().collect();
        let more = ids.len().saturating_sub(shown.len());
        format!(
            "{}{}",
            shown.join(", "),
            if more > 0 {
                format!(" … and {more} more")
            } else {
                String::new()
            }
        )
    }

    /// Validate `model` and `effort` (given separately, or a full id such as
    /// `gemini-4-neon-medium`) against this list.
    ///
    /// * no model and no effort: the agent's defaults (`Ok` with `None`s);
    /// * an unknown model: refused with the closest ids and the whole list, unless the agent is
    ///   [`open`](Self::open) (Claude accepts full names), which passes it through with a note;
    /// * an effort the model does not offer: refused with the efforts it does (`max` is accepted
    ///   where only `xhigh` exists, with a note);
    /// * an effort for a model or agent that has no selectable effort: dropped with a note.
    pub fn resolve(
        &self,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> std::result::Result<Selection, SelectionError> {
        let model = model.map(str::trim).filter(|m| !m.is_empty());
        let effort = effort.map(norm).filter(|e| !e.is_empty());
        let mut notes: Vec<String> = Vec::new();
        let mut out = Selection {
            model: None,
            effort: None,
            real_id: None,
            notes: Vec::new(),
        };
        let mut entry: Option<&CatalogModel> = None;
        let mut effort = effort;
        if let Some(m) = model {
            match self.find(m) {
                Some((e, from_id)) => {
                    if let Some(f) = from_id {
                        match &effort {
                            Some(given) if *given != f => notes.push(format!(
                                "model {m} carries effort {f} but effort {given} was asked: {given} wins"
                            )),
                            Some(_) => {}
                            None => effort = Some(f),
                        }
                    }
                    out.model = Some(e.id.clone());
                    entry = Some(e);
                    if !e.available {
                        return Err(SelectionError(format!(
                            "{} lists {} but it is not available right now; pick another: {}",
                            self.agent,
                            e.id,
                            self.list_for_message()
                        )));
                    }
                }
                None if self.open => {
                    out.model = Some(m.to_string());
                    notes.push(format!(
                        "{m} is not one of the {} names {} documents; it is passed through as is",
                        self.models.len(),
                        self.agent
                    ));
                }
                None if self.models.is_empty() => {
                    out.model = Some(m.to_string());
                    notes.push(format!(
                        "{} reported no models, so {m} could not be checked",
                        self.agent
                    ));
                }
                None => {
                    let sugg = self.suggestions(m, 3);
                    return Err(SelectionError(format!(
                        "{} has no model {m:?}{}. Valid models: {}. \
                         Refresh the list with `selachi-agent list --models --refresh` if the agent got new ones.",
                        self.agent,
                        if sugg.is_empty() {
                            String::new()
                        } else {
                            format!(" (did you mean {}?)", sugg.join(" or "))
                        },
                        self.list_for_message()
                    )));
                }
            }
        }
        if let Some(e) = effort {
            let options = self.effort_options(out.model.as_deref());
            let supported = !options.is_empty();
            if !supported {
                notes.push(format!(
                    "{} has no selectable effort{}; effort {e} was ignored",
                    self.agent,
                    out.model
                        .as_deref()
                        .map(|m| format!(" for {m}"))
                        .unwrap_or_default()
                ));
            } else if let Some(level) = options.iter().find(|o| **o == e) {
                out.effort = Some(level.clone());
            } else if e == "max" {
                match thought_level_for("max", &options) {
                    Some(level) => {
                        notes.push(format!(
                            "effort max mapped to {level} (the highest offered)"
                        ));
                        out.effort = Some(level.to_string());
                    }
                    None => out.effort = None,
                }
            } else {
                return Err(SelectionError(format!(
                    "{}{} does not offer effort {e:?}; valid efforts: {}",
                    self.agent,
                    out.model
                        .as_deref()
                        .map(|m| format!(" model {m}"))
                        .unwrap_or_default(),
                    options.join(", ")
                )));
            }
        }
        out.real_id = match (entry, out.effort.as_deref()) {
            (Some(m), Some(e)) if self.effort_in_id(m) => Some(format!("{}-{e}", m.id)),
            (Some(m), None) if self.effort_in_id(m) => match &m.default_effort {
                Some(d) => Some(format!("{}-{d}", m.id)),
                None => Some(m.id.clone()),
            },
            _ => out.model.clone(),
        };
        out.notes = notes;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Parsers (pure)
// ---------------------------------------------------------------------------

/// `opencode models`: one `provider/model` per line.
pub fn parse_opencode_models(stdout: &str) -> Vec<CatalogModel> {
    stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.contains(char::is_whitespace) && l.contains('/'))
        .map(|id| CatalogModel {
            id: id.to_string(),
            label: id.to_string(),
            efforts: Vec::new(),
            default_effort: None,
            default: false,
            available: true,
        })
        .collect()
}

/// `gemini --list-models`: `id (Label)` lines at column 0, each followed by indented details.
pub fn parse_gemini_models(stdout: &str) -> Vec<CatalogModel> {
    stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty() && !l.starts_with(char::is_whitespace))
        .filter_map(|l| {
            let (id, rest) = l.split_once(' ')?;
            let label = rest.strip_prefix('(')?.strip_suffix(')')?;
            (!id.is_empty() && !id.contains(['(', ':'])).then(|| CatalogModel {
                id: id.to_string(),
                label: if label.is_empty() { id } else { label }.to_string(),
                efforts: Vec::new(),
                default_effort: None,
                default: false,
                available: true,
            })
        })
        .collect()
}

/// `claude --help`: the model aliases the `--model` option documents (quoted words) and the effort
/// levels of `--effort <level> (low, medium, ...)`.
pub fn parse_claude_help(help: &str) -> (Vec<String>, Vec<String>) {
    let mut aliases: Vec<String> = Vec::new();
    let mut efforts: Vec<String> = Vec::new();
    let lines: Vec<&str> = help.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("--model") || t.contains(" --model <") {
            // the option's description runs over the following indented lines
            let mut text = t.to_string();
            for next in lines.iter().skip(i + 1) {
                if next.trim_start().starts_with('-') || next.trim().is_empty() {
                    break;
                }
                text.push(' ');
                text.push_str(next.trim());
            }
            let mut rest = text.as_str();
            while let Some(start) = rest.find('\'') {
                let after = &rest[start + 1..];
                let Some(end) = after.find('\'') else { break };
                let word = &after[..end];
                if !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
                    && !aliases.iter().any(|a| a == word)
                {
                    aliases.push(word.to_string());
                }
                rest = &after[end + 1..];
            }
        }
        if t.starts_with("--effort") {
            let mut text = t.to_string();
            for next in lines.iter().skip(i + 1) {
                if next.trim_start().starts_with('-') || next.trim().is_empty() {
                    break;
                }
                text.push(' ');
                text.push_str(next.trim());
            }
            if let (Some(a), Some(b)) = (text.find('('), text.find(')')) {
                if a < b {
                    for e in text[a + 1..b].split(',').map(norm) {
                        if EFFORT_LEVELS.contains(&e.as_str()) && !efforts.contains(&e) {
                            efforts.push(e);
                        }
                    }
                }
            }
        }
    }
    (aliases, efforts)
}

/// Claude's list from the (optional) help text: aliases and efforts, the built-in ones when the help
/// could not be read.
pub fn claude_models(help: Option<&str>) -> AgentModels {
    let (aliases, efforts) = help.map(parse_claude_help).unwrap_or_default();
    let (aliases, source) = if aliases.is_empty() {
        (
            CLAUDE_ALIASES.iter().map(|s| s.to_string()).collect(),
            "built-in aliases",
        )
    } else {
        (aliases, "claude --help")
    };
    let efforts = if efforts.is_empty() {
        EFFORT_LEVELS.iter().map(|s| s.to_string()).collect()
    } else {
        efforts
    };
    AgentModels {
        agent: "claude".into(),
        fetched_ms: now_ms(),
        source: source.into(),
        models: aliases
            .into_iter()
            .map(|a| CatalogModel {
                label: a.clone(),
                id: a,
                efforts: Vec::new(),
                default_effort: None,
                default: false,
                available: true,
            })
            .collect(),
        efforts,
        open: true,
        error: None,
    }
}

/// Mark the agent's own default (`current` as the handshake reported it, possibly with an effort suffix).
pub fn mark_default(models: &mut [CatalogModel], current: Option<&str>) {
    let Some(cur) = current.map(norm).filter(|c| !c.is_empty()) else {
        return;
    };
    let base = split_effort_suffix(&cur).map(|(b, _)| b.to_string());
    for m in models.iter_mut() {
        m.default = norm(&m.id) == cur || base.as_deref().is_some_and(|b| norm(&m.id) == b);
    }
}

// ---------------------------------------------------------------------------
// Live fetch (blocking)
// ---------------------------------------------------------------------------

fn run_cli(exe: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = crate::proc::command_for(exe);
    cmd.args(args);
    crate::proc::run_capture(cmd, timeout)
}

fn from_infos(
    agent: &str,
    source: &str,
    infos: Vec<ModelInfo>,
    current: Option<&str>,
) -> AgentModels {
    let mut models: Vec<CatalogModel> = infos.iter().map(CatalogModel::from_info).collect();
    mark_default(&mut models, current);
    AgentModels {
        agent: agent.to_string(),
        fetched_ms: now_ms(),
        source: source.to_string(),
        models,
        efforts: Vec::new(),
        open: false,
        error: None,
    }
}

fn from_models(agent: &str, source: &str, models: Vec<CatalogModel>) -> AgentModels {
    AgentModels {
        agent: agent.to_string(),
        fetched_ms: now_ms(),
        source: source.to_string(),
        models,
        efforts: Vec::new(),
        open: false,
        error: None,
    }
}

/// Ask the agent itself, now. Blocking (starts the CLI, at most `timeout` per attempt).
pub fn fetch(agent: &str, timeout: Duration) -> Result<AgentModels> {
    let name = agent.trim().to_ascii_lowercase();
    if !AGENTS.contains(&name.as_str()) {
        return Err(DriverError::Unsupported(format!(
            "no model list for agent `{agent}`"
        )));
    }
    let exe = crate::detect::locate(&name).ok_or_else(|| DriverError::NotFound(name.clone()))?;
    let cwd = std::env::temp_dir();
    let acp = |kind: AgentKind| -> Result<(Vec<ModelInfo>, Option<String>)> {
        crate::probe_models_with_current(kind, &cwd, timeout)
    };
    let got = match name.as_str() {
        "claude" => claude_models(run_cli(&exe, &["--help"], timeout).as_deref()),
        "agy" => {
            let infos = crate::antigravity::list_models(timeout)?;
            from_infos("agy", "agy models", infos, None)
        }
        "opencode" => {
            let listed = run_cli(&exe, &["models"], timeout)
                .map(|s| parse_opencode_models(&s))
                .unwrap_or_default();
            if listed.is_empty() {
                let (infos, cur) = acp(AgentKind::OpenCode)?;
                from_infos("opencode", "ACP session/new", infos, cur.as_deref())
            } else {
                from_models("opencode", "opencode models", listed)
            }
        }
        "gemini" => {
            let listed = run_cli(&exe, &["--list-models"], timeout)
                .map(|s| parse_gemini_models(&s))
                .unwrap_or_default();
            if listed.is_empty() {
                let kind = crate::detect::kind_of("gemini")
                    .ok_or_else(|| DriverError::NotFound("gemini".into()))?;
                let (infos, cur) = acp(kind)?;
                from_infos("gemini", "ACP session/new", infos, cur.as_deref())
            } else {
                from_models("gemini", "gemini --list-models", listed)
            }
        }
        _ => {
            let (infos, cur) = acp(AgentKind::Codex)?;
            from_infos("codex", "codex model/list", infos, cur.as_deref())
        }
    };
    if got.models.is_empty() {
        return Err(DriverError::Protocol(format!(
            "{name} listed no models ({})",
            got.source
        )));
    }
    Ok(got)
}

/// The catalog name of a spawnable kind (`agy`, `codex`, `opencode`, `gemini`, `claude`), `None` for a
/// custom ACP command the catalog does not know.
pub fn agent_of_kind(kind: &AgentKind) -> Option<&'static str> {
    match kind {
        AgentKind::ClaudeCode => Some("claude"),
        AgentKind::Codex => Some("codex"),
        AgentKind::OpenCode => Some("opencode"),
        AgentKind::Acp { command } if crate::is_antigravity(command) => Some("agy"),
        AgentKind::Acp { command } => {
            let first = crate::acp::split_command(command).into_iter().next()?;
            let stem = Path::new(&first)
                .file_stem()?
                .to_string_lossy()
                .to_ascii_lowercase();
            (stem == "gemini").then_some("gemini")
        }
    }
}

/// The models of `kind` for a picker: the cached catalog when it is fresh, else the agent is asked and
/// the answer stored. Blocking (call from a background task, never from a render). A custom ACP
/// command falls back to a plain handshake.
pub fn models_for_kind(
    data_dir: &Path,
    kind: &AgentKind,
    cwd: &Path,
    timeout: Duration,
) -> Result<Vec<ModelInfo>> {
    let Some(agent) = agent_of_kind(kind) else {
        return crate::probe_models(kind.clone(), cwd, timeout);
    };
    let m = ensure(data_dir, agent, DEFAULT_TTL, timeout);
    if m.models.is_empty() {
        return Err(DriverError::Protocol(
            m.error
                .unwrap_or_else(|| format!("{agent} listed no models")),
        ));
    }
    Ok(m.models.iter().map(CatalogModel::to_info).collect())
}

// ---------------------------------------------------------------------------
// The cache file
// ---------------------------------------------------------------------------

/// The whole file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub agents: Vec<AgentModels>,
}

impl Catalog {
    pub fn get(&self, agent: &str) -> Option<&AgentModels> {
        self.agents
            .iter()
            .find(|a| a.agent.eq_ignore_ascii_case(agent.trim()))
    }

    fn put(&mut self, m: AgentModels) {
        self.version = 1;
        match self.agents.iter_mut().find(|a| a.agent == m.agent) {
            Some(slot) => *slot = m,
            None => self.agents.push(m),
        }
    }
}

pub fn catalog_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CATALOG_FILE)
}

/// Read the cache. Missing or unreadable = empty (never an error, never starts anything).
pub fn load(data_dir: &Path) -> Catalog {
    std::fs::read_to_string(catalog_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, c: &Catalog) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = catalog_path(data_dir);
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(c).unwrap_or_default())?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// What is on disk for `agent` (stale or not); never fetches.
pub fn cached(data_dir: &Path, agent: &str) -> Option<AgentModels> {
    load(data_dir).get(agent).cloned()
}

/// Store the outcome of a fetch: a success replaces the entry; a failure keeps the previous models
/// and records the error (an agent with nothing cached gets an empty entry carrying it).
pub fn record(
    data_dir: &Path,
    agent: &str,
    outcome: std::result::Result<AgentModels, String>,
) -> AgentModels {
    let mut c = load(data_dir);
    let entry = match outcome {
        Ok(m) => m,
        Err(e) => {
            let mut prev = c
                .get(agent)
                .cloned()
                .unwrap_or_else(|| AgentModels::empty(agent));
            prev.error = Some(e);
            prev
        }
    };
    c.put(entry.clone());
    if let Err(e) = save(data_dir, &c) {
        log::warn!("model catalog: cannot write {CATALOG_FILE}: {e}");
    }
    entry
}

/// Ask the agent now and store the answer. Blocking.
pub fn refresh(data_dir: &Path, agent: &str, timeout: Duration) -> AgentModels {
    record(
        data_dir,
        agent,
        fetch(agent, timeout).map_err(|e| e.to_string()),
    )
}

/// The cached list when it is younger than `ttl`, else a refresh. Blocking only when it refreshes.
pub fn ensure(data_dir: &Path, agent: &str, ttl: Duration, timeout: Duration) -> AgentModels {
    ensure_with(data_dir, agent, ttl, now_ms(), || fetch(agent, timeout))
}

/// [`ensure`] with the fetch injected (tests).
pub fn ensure_with(
    data_dir: &Path,
    agent: &str,
    ttl: Duration,
    now_ms: i64,
    fetch: impl FnOnce() -> Result<AgentModels>,
) -> AgentModels {
    if let Some(m) = cached(data_dir, agent) {
        if m.is_fresh(now_ms, ttl) {
            return m;
        }
    }
    record(data_dir, agent, fetch().map_err(|e| e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGY: &str = "Fetching available models...\n\
gemini-4-neon-high\tGemini 4 Neon (High)\n\
gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)\n\
gemini-4-neon-medium\tGemini 4 Neon (Medium)\n\
gemini-4-neon-low\tGemini 4 Neon (Low)\n\
gemini-4-hydrogen-high\tGemini 4 Hydrogen (High)\n\
gemini-4-hydrogen-medium\tGemini 4 Hydrogen (Medium)\n\
gemini-pro-orchestrator-neon\tOrchestrator (Neon)\n\
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n";

    fn agy() -> AgentModels {
        let infos = crate::antigravity::parse_models_output(AGY);
        from_infos("agy", "agy models", infos, None)
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("catalog_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn agy_family_plus_effort_maps_to_the_real_id() {
        let c = agy();
        let s = c.resolve(Some("gemini-4-neon"), Some("medium")).unwrap();
        assert_eq!(s.model.as_deref(), Some("gemini-4-neon"));
        assert_eq!(s.effort.as_deref(), Some("medium"));
        assert_eq!(s.real_id.as_deref(), Some("gemini-4-neon-medium"));
        // a full id splits into family + effort
        let s = c.resolve(Some("gemini-4-hydrogen-high"), None).unwrap();
        assert_eq!(s.model.as_deref(), Some("gemini-4-hydrogen"));
        assert_eq!(s.effort.as_deref(), Some("high"));
        // an explicit effort wins over the suffix, and says so
        let s = c.resolve(Some("gemini-4-neon-low"), Some("high")).unwrap();
        assert_eq!(s.effort.as_deref(), Some("high"));
        assert!(s.notes.iter().any(|n| n.contains("wins")));
        // max -> the highest offered
        let s = c.resolve(Some("gemini-4-neon"), Some("MAX")).unwrap();
        assert_eq!(s.effort.as_deref(), Some("xhigh"));
        // a model without efforts needs none
        let s = c
            .resolve(Some("gemini-pro-orchestrator-neon"), None)
            .unwrap();
        assert_eq!(s.real_id.as_deref(), Some("gemini-pro-orchestrator-neon"));
        assert_eq!(c.resolve(None, None).unwrap().model, None);
    }

    #[test]
    fn unknown_models_and_efforts_are_refused_with_the_valid_options() {
        let c = agy();
        let e = c
            .resolve(Some("gemini-4-neo"), None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("did you mean gemini-4-neon"), "{e}");
        assert!(e.contains("gemini-4-hydrogen"), "{e}");
        let e = c
            .resolve(Some("gemini-4-hydrogen"), Some("xhigh"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("medium, high"), "{e}");
        assert!(!e.contains("xhigh,"), "{e}");
        // gpt-oss lists no selectable effort: ignored, not refused
        let s = c
            .resolve(Some("gpt-oss-120b-medium"), Some("high"))
            .unwrap();
        assert_eq!(s.effort, None);
        assert!(s.notes.iter().any(|n| n.contains("ignored")));
    }

    #[test]
    fn claude_is_open_ended_and_has_agent_wide_efforts() {
        let help = "  --effort <level>                      Effort level for the current session\n\
                    \x20                                       (low, medium, high, xhigh, max)\n\
                    \x20 --model <model>                       Model for the current session. Provide\n\
                    \x20                                       an alias for the latest model (e.g.\n\
                    \x20                                       'fable', 'opus', or 'sonnet') or a\n\
                    \x20                                       model's full name.\n\
                    \x20 -n, --name <name>                     Set a display name\n";
        let (aliases, efforts) = parse_claude_help(help);
        assert_eq!(aliases, ["fable", "opus", "sonnet"]);
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max"]);
        let c = claude_models(Some(help));
        assert_eq!(c.source, "claude --help");
        assert!(c.open);
        let s = c.resolve(Some("opus"), Some("max")).unwrap();
        assert_eq!(s.effort.as_deref(), Some("max"));
        assert_eq!(s.real_id.as_deref(), Some("opus"));
        // a full name passes through with a note
        let s = c.resolve(Some("claude-opus-9"), None).unwrap();
        assert_eq!(s.model.as_deref(), Some("claude-opus-9"));
        assert!(!s.notes.is_empty());
        // help unreadable: the built-in aliases
        let c = claude_models(None);
        assert_eq!(c.source, "built-in aliases");
        assert_eq!(c.ids(), ["sonnet", "opus", "haiku"]);
    }

    #[test]
    fn opencode_and_gemini_outputs_are_parsed() {
        let m = parse_opencode_models(
            "opencode/big-pickle\nopencode/iceman-free\n\nnot a model line\nanthropic/claude-x\n",
        );
        assert_eq!(
            m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [
                "opencode/big-pickle",
                "opencode/iceman-free",
                "anthropic/claude-x"
            ]
        );
        let g = parse_gemini_models(
            "Loaded cached credentials.\nAvailable Gemini Models:\n\nauto (Auto)\n  Description:   Let Gemini CLI decide\n  Context limit: 1,048,576\n\ngemini-2.5-pro (gemini-2.5-pro)\n  Context limit: 1,048,576\n",
        );
        assert_eq!(
            g.iter()
                .map(|m| (m.id.as_str(), m.label.as_str()))
                .collect::<Vec<_>>(),
            [("auto", "Auto"), ("gemini-2.5-pro", "gemini-2.5-pro")]
        );
    }

    #[test]
    fn default_is_marked_even_when_the_handshake_says_a_full_id() {
        let mut c = agy();
        mark_default(&mut c.models, Some("gemini-4-neon-high"));
        assert_eq!(c.default_model().unwrap().id, "gemini-4-neon");
        mark_default(&mut c.models, Some("gpt-oss-120b-medium"));
        assert_eq!(c.default_model().unwrap().id, "gpt-oss-120b-medium");
    }

    #[test]
    fn the_cache_has_a_ttl_and_a_failed_refresh_keeps_the_old_list() {
        let dir = tmp("ttl");
        let t0 = 1_000_000_000_000i64;
        let mut calls = 0;
        let mut first = agy();
        first.fetched_ms = t0;
        let got = ensure_with(&dir, "agy", DEFAULT_TTL, t0, || {
            calls += 1;
            Ok(first.clone())
        });
        assert_eq!(calls, 1);
        assert_eq!(got.models.len(), 4);
        // fresh: no second fetch
        let _ = ensure_with(&dir, "agy", DEFAULT_TTL, t0 + 1000, || {
            calls += 1;
            Ok(first.clone())
        });
        assert_eq!(calls, 1);
        // stale: refetched; the fetch fails -> the old models stay, with the error
        let late = t0 + DEFAULT_TTL.as_millis() as i64 + 1;
        let got = ensure_with(&dir, "agy", DEFAULT_TTL, late, || {
            calls += 1;
            Err(DriverError::Protocol("offline".into()))
        });
        assert_eq!(calls, 2);
        assert_eq!(got.models.len(), 4);
        assert!(got.error.as_deref().unwrap().contains("offline"));
        assert_eq!(cached(&dir, "AGY").unwrap().error, got.error);
        // another agent's entry is independent
        assert!(cached(&dir, "codex").is_none());
        // a corrupt file reads as empty
        std::fs::write(catalog_path(&dir), "{nope").unwrap();
        assert!(load(&dir).agents.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn effort_options_follow_the_model_or_the_agent() {
        let c = agy();
        assert_eq!(
            c.effort_options(Some("gemini-4-hydrogen")),
            ["medium", "high"]
        );
        assert!(c.effort_options(Some("gpt-oss-120b-medium")).is_empty());
        assert_eq!(c.effort_options(None), ["low", "medium", "high", "xhigh"]);
        let cl = claude_models(None);
        assert_eq!(cl.effort_options(Some("sonnet")).len(), 5);
    }

    /// Live: needs the CLIs installed. `cargo test -p agents_driver -- --ignored live_catalog`
    #[test]
    #[ignore]
    fn live_catalog_prints_every_installed_agent() {
        for a in AGENTS {
            match fetch(a, FETCH_TIMEOUT) {
                Ok(m) => eprintln!("{a}: {} models via {}", m.models.len(), m.source),
                Err(e) => eprintln!("{a}: {e}"),
            }
        }
    }
}
