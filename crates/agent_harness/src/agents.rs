//! Which agents exist on this machine and how to start them: the name <-> `AgentKind` mapping
//! and `selachi-agent list`.

use std::path::{Path, PathBuf};

use agents_driver::catalog::{self, CatalogModel};
use agents_driver::AgentKind;
use serde::Serialize;

use crate::policy::{Caller, DelegationConfig, Refusal, RefusalCode};
use crate::usage::{LimitRow, UsageProbe};

/// Agents the harness knows by name.
pub const KNOWN_AGENTS: [&str; 5] = ["claude", "codex", "opencode", "gemini", "agy"];

/// Aliases the Claude Code CLI documents for `--model` when `claude --help` cannot be read.
pub const CLAUDE_ALIASES: [&str; 3] = catalog::CLAUDE_ALIASES;

/// Flag that switches the Gemini CLI into ACP mode. Version dependent (older builds used
/// `--experimental-acp`); override the whole command with `acp:<command line>`.
pub const GEMINI_ACP_FLAG: &str = "--experimental-acp";

/// Short stable name of a backend (`claude`, `codex`, `opencode`, `agy`, or the program name of
/// another ACP agent such as `gemini`).
pub fn agent_name(kind: &AgentKind) -> String {
    match kind {
        AgentKind::ClaudeCode => "claude".into(),
        AgentKind::Codex => "codex".into(),
        AgentKind::OpenCode => "opencode".into(),
        AgentKind::Acp { command } if agents_driver::is_antigravity(command) => "agy".into(),
        other => {
            let label = chat_core::agent_label(other);
            label.trim_end_matches(" (ACP)").to_ascii_lowercase()
        }
    }
}

/// Canonical name for what the user typed (`Claude-Code` -> `claude`, `antigravity` -> `agy`).
pub fn canonical_name(input: &str) -> Option<&'static str> {
    match input.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claudecode" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "gemini" | "gemini-cli" => Some("gemini"),
        "agy" | "antigravity" => Some("agy"),
        _ => None,
    }
}

/// Turns an agent name into something spawnable (injectable: tests use fakes).
pub trait AgentResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Result<AgentKind, Refusal>;
}

/// Resolves against the CLIs installed on this machine (`agents_driver::detect`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

impl AgentResolver for SystemResolver {
    fn resolve(&self, name: &str) -> Result<AgentKind, Refusal> {
        let trimmed = name.trim();
        if let Some(cmd) = trimmed.strip_prefix("acp:") {
            let cmd = cmd.trim();
            if cmd.is_empty() {
                return Err(Refusal::new(
                    RefusalCode::BadRequest,
                    "acp: needs a command line, e.g. acp:\"C:\\tools\\agent.exe\" --acp",
                ));
            }
            return Ok(AgentKind::Acp {
                command: cmd.to_string(),
            });
        }
        let Some(canon) = canonical_name(trimmed) else {
            return Err(Refusal::new(
                RefusalCode::UnknownAgent,
                format!(
                    "unknown agent `{trimmed}`; use one of {} or acp:<command>",
                    KNOWN_AGENTS.join(", ")
                ),
            ));
        };
        let found = agents_driver::detect()
            .into_iter()
            .find(|d| agent_name(&d.kind) == canon);
        let Some(found) = found else {
            return Err(Refusal::new(
                RefusalCode::AgentNotInstalled,
                format!(
                    "{canon} is not installed (not found on PATH or in its default install folder)"
                ),
            ));
        };
        Ok(match (canon, found.kind) {
            ("gemini", AgentKind::Acp { command }) => AgentKind::Acp {
                command: format!("{command} {GEMINI_ACP_FLAG}"),
            },
            (_, kind) => kind,
        })
    }
}

/// One entry of `selachi-agent list`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentRow {
    pub name: String,
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// `true` when there is credential evidence, `null` when unknown (never "logged out").
    pub logged_in: Option<bool>,
    /// The harness may delegate to it (`[delegation] allowed_agents`).
    pub delegable: bool,
    /// Only with `--models`: what the agent itself reported (id, label, efforts, default, available),
    /// from the catalog (cached, refreshed with `--refresh`).
    pub models: Option<Vec<CatalogModel>>,
    /// Efforts that apply to every model of this agent when the models carry none (Claude).
    pub model_efforts: Vec<String>,
    /// The agent also accepts model names it does not list (Claude full names).
    pub models_open: bool,
    /// Where the list came from (`agy models`, `codex model/list`...).
    pub models_source: Option<String>,
    /// Unix ms of the fetch behind `models`.
    pub models_fetched_ms: Option<i64>,
    /// Why the last refresh failed (the models, if any, are the previous ones).
    pub models_error: Option<String>,
    /// Current usage windows from the providers' own numbers.
    pub limits: Vec<LimitRow>,
}

/// What `list` reports about the delegation settings.
#[derive(Debug, Clone, Serialize)]
pub struct DelegationInfo {
    pub enabled: bool,
    pub max_permission: String,
    pub max_concurrent: usize,
    pub max_depth: u32,
    pub timeout_secs: u64,
    pub daily_token_budget: u64,
    pub used_today_billable_tokens: u64,
    pub weekly_usage_threshold_percent: f64,
    /// Permission of the calling session (`SELACHI_PARENT_PERMISSION`) and the highest a run may get.
    pub caller_permission: String,
    pub effective_max_permission: String,
    pub caller_depth: u32,
    pub runs_in_flight: usize,
}

/// `selachi-agent list --json`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentList {
    pub agents: Vec<AgentRow>,
    pub delegation: DelegationInfo,
}

/// How `list_agents` gets the models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelsMode {
    /// Do not list models.
    #[default]
    Off,
    /// The catalog: the cached list when it is younger than its TTL, else ask the agent.
    Cached,
    /// Ask every installed agent again.
    Refresh,
}

/// Build the listing. With models, each installed agent whose cached list is missing or stale is
/// asked (its own CLI, no prompt is ever sent; can take several seconds), the others answer from
/// `models-catalog.json`.
pub fn list_agents(
    cfg: &DelegationConfig,
    caller: &Caller,
    usage: &dyn UsageProbe,
    data_dir: &Path,
    with_models: ModelsMode,
) -> AgentList {
    let now = chat_core::now_ms();
    let limits = usage.limits(now);
    let detected = agents_driver::detect();
    let mut rows: Vec<AgentRow> = Vec::new();
    for name in KNOWN_AGENTS {
        let hit = detected.iter().find(|d| agent_name(&d.kind) == name);
        rows.push(AgentRow {
            name: name.to_string(),
            installed: hit.is_some(),
            path: hit.map(|d| d.path.clone()),
            version: hit.and_then(|d| d.version.clone()),
            logged_in: hit.and_then(|d| d.logged_in),
            delegable: cfg.enabled
                && (cfg.allowed_agents.is_empty()
                    || cfg
                        .allowed_agents
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case(name))),
            models: None,
            model_efforts: Vec::new(),
            models_open: false,
            models_source: None,
            models_fetched_ms: None,
            models_error: None,
            limits: limits
                .iter()
                .filter(|l| l.agent.eq_ignore_ascii_case(name))
                .cloned()
                .collect(),
        });
    }
    if with_models != ModelsMode::Off {
        let probes: Vec<_> = KNOWN_AGENTS
            .iter()
            .filter(|n| detected.iter().any(|d| agent_name(&d.kind) == **n))
            .map(|name| {
                let (name, dir) = (name.to_string(), data_dir.to_path_buf());
                let n2 = name.clone();
                (
                    name,
                    std::thread::spawn(move || {
                        if with_models == ModelsMode::Refresh {
                            catalog::refresh(&dir, &n2, catalog::FETCH_TIMEOUT)
                        } else {
                            catalog::ensure(&dir, &n2, catalog::DEFAULT_TTL, catalog::FETCH_TIMEOUT)
                        }
                    }),
                )
            })
            .collect();
        for (name, handle) in probes {
            let Some(row) = rows.iter_mut().find(|r| r.name == name) else {
                continue;
            };
            match handle.join() {
                Ok(m) => {
                    row.models_error = m.error.clone();
                    row.model_efforts = m.efforts.clone();
                    row.models_open = m.open;
                    if m.fetched_ms > 0 {
                        row.models_source = Some(m.source.clone());
                        row.models_fetched_ms = Some(m.fetched_ms);
                    }
                    row.models = Some(m.models);
                }
                Err(_) => row.models_error = Some("the probe panicked".into()),
            }
        }
    }
    let used = crate::registry::Registry::new(data_dir)
        .billable_tokens_between(crate::run::local_day_start_ms(now), i64::MAX);
    let cap = cfg
        .max_permission
        .min(crate::policy::Permission::AcceptEdits)
        .min(caller.permission);
    AgentList {
        agents: rows,
        delegation: DelegationInfo {
            enabled: cfg.enabled,
            max_permission: cfg.max_permission.to_string(),
            max_concurrent: cfg.max_concurrent,
            max_depth: cfg.max_depth,
            timeout_secs: cfg.timeout_secs,
            daily_token_budget: cfg.daily_token_budget,
            used_today_billable_tokens: used,
            weekly_usage_threshold_percent: cfg.weekly_usage_threshold_percent,
            caller_permission: caller.permission.to_string(),
            effective_max_permission: cap.to_string(),
            caller_depth: caller.depth,
            runs_in_flight: crate::slots::in_use(data_dir, cfg.slots()),
        },
    }
}

impl AgentList {
    /// Human-readable table.
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for a in &self.agents {
            if !a.installed {
                let _ = writeln!(s, "{:<9} not installed", a.name);
                continue;
            }
            let login = match a.logged_in {
                Some(true) => "login ok",
                _ => "login unknown",
            };
            let _ = writeln!(
                s,
                "{:<9} {} · {login}{}",
                a.name,
                a.version.as_deref().unwrap_or("version unknown"),
                if a.delegable {
                    ""
                } else {
                    " · delegation not allowed"
                }
            );
            if let Some(models) = &a.models {
                let ids: Vec<String> = models
                    .iter()
                    .map(|m| {
                        let mut t = m.id.clone();
                        if !m.efforts.is_empty() {
                            t.push_str(&format!(" [{}]", m.efforts.join("|")));
                        }
                        if m.default {
                            t.push_str(" (default)");
                        }
                        t
                    })
                    .collect();
                let _ = writeln!(
                    s,
                    "          models{}: {}",
                    a.models_source
                        .as_deref()
                        .map(|x| format!(" ({x})"))
                        .unwrap_or_default(),
                    if ids.is_empty() {
                        "(none reported)".into()
                    } else {
                        ids.join(", ")
                    }
                );
                if !a.model_efforts.is_empty() {
                    let _ = writeln!(s, "          efforts: {}", a.model_efforts.join(", "));
                }
                if a.models_open {
                    let _ = writeln!(s, "          other model names are accepted as they are");
                }
            }
            if let Some(e) = &a.models_error {
                let _ = writeln!(s, "          models: refresh failed ({e})");
            }
            for l in &a.limits {
                let _ = writeln!(
                    s,
                    "          limit {}: {:.0} % ({})",
                    l.window, l.used_percent, l.source
                );
            }
        }
        let d = &self.delegation;
        let _ = writeln!(
            s,
            "\ndelegation: {} · max permission here: {} (session {}, config {}) · depth {}/{} · {}/{} runs · {} of {} billable tokens today",
            if d.enabled { "on" } else { "OFF" },
            d.effective_max_permission,
            d.caller_permission,
            d.max_permission,
            d.caller_depth,
            d.max_depth,
            d.runs_in_flight,
            d.max_concurrent,
            d.used_today_billable_tokens,
            if d.daily_token_budget == 0 { "unlimited".to_string() } else { d.daily_token_budget.to_string() },
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        assert_eq!(agent_name(&AgentKind::ClaudeCode), "claude");
        assert_eq!(agent_name(&AgentKind::Codex), "codex");
        assert_eq!(agent_name(&AgentKind::OpenCode), "opencode");
        assert_eq!(
            agent_name(&AgentKind::Acp {
                command: "\"C:/x y/agy.exe\" acp".into()
            }),
            "agy"
        );
        assert_eq!(
            agent_name(&AgentKind::Acp {
                command: "\"C:/x y/gemini.cmd\" --acp".into()
            }),
            "gemini"
        );
        assert_eq!(canonical_name("Claude-Code"), Some("claude"));
        assert_eq!(canonical_name("Antigravity"), Some("agy"));
        assert_eq!(canonical_name("gpt"), None);
    }

    #[test]
    fn resolver_rejects_unknown_and_blank_acp() {
        let e = SystemResolver.resolve("gpt").unwrap_err();
        assert_eq!(e.code, RefusalCode::UnknownAgent);
        assert!(e.reason.contains("codex"));
        assert_eq!(
            SystemResolver.resolve("acp:  ").unwrap_err().code,
            RefusalCode::BadRequest
        );
        assert_eq!(
            SystemResolver.resolve("acp:my-agent --acp").unwrap(),
            AgentKind::Acp {
                command: "my-agent --acp".into()
            }
        );
    }
}
