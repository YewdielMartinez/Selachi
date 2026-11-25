//! Who may delegate what: permissions, depth, allowed agents and the `[delegation]` settings.
//!
//! Everything here is pure (no processes, no I/O except [`DelegationConfig::load`]) so the rules are
//! unit-testable. The rules are PLAN section 20 "Seguridad y limites":
//!
//! * a delegation never has more permission than the session that asked for it
//!   (`SELACHI_PARENT_PERMISSION`, default read-only) nor more than `[delegation] max_permission`;
//! * `full` is never delegable;
//! * the delegation chain is at most `max_depth` hops deep (`SELACHI_DELEGATION_DEPTH`);
//! * only the agents in `allowed_agents` (empty = all) can be targets.

use std::fmt;
use std::path::Path;

use agents_driver::PermissionMode;
use serde::{Deserialize, Serialize};

use crate::approval_policy::ApprovalMode;

/// Environment variable: permission of the session that is calling the harness.
pub const ENV_PARENT_PERMISSION: &str = "SELACHI_PARENT_PERMISSION";
/// Environment variable: delegation depth of the calling session (0 / unset = top level).
pub const ENV_DELEGATION_DEPTH: &str = "SELACHI_DELEGATION_DEPTH";
/// Environment variable: chat thread id of the calling session, when it is a Selachi chat thread.
pub const ENV_PARENT_THREAD: &str = "SELACHI_PARENT_THREAD";
/// Environment variable: opaque id of the terminal tab of the calling session.
pub const ENV_PARENT_TAB: &str = "SELACHI_PARENT_TAB";

/// Environment variable: how the call arrived (`cli` | `mcp`); recorded as `delegated_by` on the child thread.
pub const ENV_CHANNEL: &str = "SELACHI_CHANNEL";

/// Permission level of an agent session, ordered by capability:
/// `ReadOnly < Ask < AcceptEdits < Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Permission {
    ReadOnly,
    Ask,
    AcceptEdits,
    Full,
}

impl Permission {
    /// Accepts `read-only | read_only | readonly`, `ask`, `accept-edits | accept_edits`, `full`
    /// (case-insensitive).
    pub fn parse(s: &str) -> Option<Permission> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "read-only" | "readonly" | "ro" => Some(Permission::ReadOnly),
            "ask" => Some(Permission::Ask),
            "accept-edits" | "acceptedits" => Some(Permission::AcceptEdits),
            "full" => Some(Permission::Full),
            _ => None,
        }
    }

    /// CLI spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ReadOnly => "read-only",
            Permission::Ask => "ask",
            Permission::AcceptEdits => "accept-edits",
            Permission::Full => "full",
        }
    }

    /// The `agents_driver` / `chat_core` mode.
    pub fn to_driver(self) -> PermissionMode {
        match self {
            Permission::ReadOnly => PermissionMode::ReadOnly,
            Permission::Ask => PermissionMode::Ask,
            Permission::AcceptEdits => PermissionMode::AcceptEdits,
            Permission::Full => PermissionMode::Full,
        }
    }

    /// From a stored `chat_core` mode.
    pub fn from_driver(m: PermissionMode) -> Permission {
        match m {
            PermissionMode::ReadOnly => Permission::ReadOnly,
            PermissionMode::Ask => Permission::Ask,
            PermissionMode::AcceptEdits => Permission::AcceptEdits,
            PermissionMode::Full => Permission::Full,
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Permission {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Permission::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown permission {s:?}")))
    }
}

/// Why a run was refused before it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// `[delegation] enabled = false`.
    Disabled,
    BadRequest,
    UnknownAgent,
    AgentNotInstalled,
    AgentNotAllowed,
    /// `full` was requested: it is never delegable.
    PermissionFull,
    PermissionExceedsParent,
    PermissionExceedsConfig,
    DepthExceeded,
    /// Already `max_concurrent` runs in flight.
    Concurrency,
    /// The daily token budget is spent.
    DailyBudget,
    /// The agent's weekly usage is over the configured threshold.
    WeeklyUsage,
    NotAGitRepo,
    /// `profile` names no usable profile (or profiles are turned off).
    UnknownProfile,
    /// The profile exists but cannot be applied (a context file outside the project, a bad schema...).
    InvalidProfile,
}

/// A refusal: a stable code plus a sentence that says what to change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct Refusal {
    pub code: RefusalCode,
    pub reason: String,
}

impl Refusal {
    pub fn new(code: RefusalCode, reason: impl Into<String>) -> Refusal {
        Refusal {
            code,
            reason: reason.into(),
        }
    }
}

/// The session that calls the harness (read from the environment by the CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Permission the calling session itself has. Default read-only (nothing inherited = least privilege).
    pub permission: Permission,
    /// Delegation depth of the calling session (0 = a top-level session, not itself delegated).
    pub depth: u32,
    /// Chat thread id of the calling session, when it is a Selachi chat thread.
    pub parent_thread: Option<i64>,
    /// Terminal tab id of the calling session.
    pub parent_tab: Option<String>,
    /// How the call arrived: `cli`, `mcp`.
    pub channel: String,
}

impl Default for Caller {
    fn default() -> Self {
        Caller {
            permission: Permission::ReadOnly,
            depth: 0,
            parent_thread: None,
            parent_tab: None,
            channel: "cli".into(),
        }
    }
}

impl Caller {
    /// Read the process environment (`SELACHI_PARENT_PERMISSION`, `SELACHI_DELEGATION_DEPTH`,
    /// `SELACHI_PARENT_THREAD`, `SELACHI_PARENT_TAB`).
    pub fn from_env() -> Caller {
        Caller::from_vars(|k| std::env::var(k).ok())
    }

    /// [`Caller::from_env`] over any variable source (tests).
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Caller {
        let non_empty = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Caller {
            // An unparsable value fails closed to read-only, never open.
            permission: non_empty(ENV_PARENT_PERMISSION)
                .map(|v| Permission::parse(&v).unwrap_or(Permission::ReadOnly))
                .unwrap_or(Permission::ReadOnly),
            depth: non_empty(ENV_DELEGATION_DEPTH)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            parent_thread: non_empty(ENV_PARENT_THREAD).and_then(|v| v.parse().ok()),
            parent_tab: non_empty(ENV_PARENT_TAB),
            // Only the known channels are accepted; anything else is a plain CLI call.
            channel: if non_empty(ENV_CHANNEL).as_deref() == Some("mcp") {
                "mcp".into()
            } else {
                "cli".into()
            },
        }
    }
}

/// `[delegation]` in `config.toml`. Not part of `store::Config` yet (see the crate docs: the
/// integrator moves it there); every field has a default, unknown keys are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct DelegationConfig {
    /// Master switch (Ajustes -> Agentes -> Delegacion).
    pub enabled: bool,
    /// Agents that may be delegated to (`claude`, `codex`, `opencode`, `gemini`, `agy`). Empty = all.
    pub allowed_agents: Vec<String>,
    /// Highest permission a delegation may have (`full` is clamped to `accept-edits`).
    pub max_permission: Permission,
    /// Runs in flight at once.
    pub max_concurrent: usize,
    /// Longest delegation chain. 2 lets a delegated agent delegate once more; 1 forbids re-delegation.
    pub max_depth: u32,
    /// Default timeout of a run.
    pub timeout_secs: u64,
    /// Billable tokens (input + output + cache writes) all delegations together may use per local day. 0 = unlimited.
    pub daily_token_budget: u64,
    /// Refuse delegating to an agent whose weekly limit is used above this percent. 0 = never refuse.
    pub weekly_usage_threshold_percent: f64,
    /// How long to wait for the user to answer a forwarded approval before denying it.
    pub approval_timeout_secs: u64,
    /// How the commands of a delegated agent are approved: `ask` | `allowlist` | `auto` (see
    /// [`crate::approval_policy`]). A profile or a single run can only lower it.
    pub approvals: ApprovalMode,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        DelegationConfig {
            enabled: true,
            allowed_agents: Vec::new(),
            max_permission: Permission::AcceptEdits,
            max_concurrent: 3,
            // PLAN §20: the parent delegates, a child cannot delegate again (raise in config).
            max_depth: 1,
            timeout_secs: 15 * 60,
            daily_token_budget: 2_000_000,
            weekly_usage_threshold_percent: 90.0,
            approval_timeout_secs: 120,
            approvals: ApprovalMode::Ask,
        }
    }
}

impl DelegationConfig {
    /// Read `[delegation]` from `<data_dir>/config.toml`. A missing file or section gives the defaults; a
    /// file that does not parse also gives the defaults, with the problem returned as a warning.
    pub fn load(data_dir: &Path) -> (DelegationConfig, Option<String>) {
        let path = store::paths::config_path_in(data_dir);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (DelegationConfig::default(), None);
        };
        DelegationConfig::from_toml(&text)
    }

    /// [`DelegationConfig::load`] over the text of a `config.toml`.
    pub fn from_toml(text: &str) -> (DelegationConfig, Option<String>) {
        let value: toml::Value = match text.parse() {
            Ok(v) => v,
            Err(e) => {
                return (
                    DelegationConfig::default(),
                    Some(format!(
                        "config.toml is not valid TOML ({e}); using the default delegation limits"
                    )),
                )
            }
        };
        let Some(section) = value.get("delegation") else {
            return (DelegationConfig::default(), None);
        };
        match section.clone().try_into::<DelegationConfig>() {
            Ok(c) => (c, None),
            Err(e) => (
                DelegationConfig::default(),
                Some(format!("[delegation] in config.toml is invalid ({e}); using the default delegation limits")),
            ),
        }
    }

    /// Slots the concurrency lock hands out (at least 1, at most 16).
    pub fn slots(&self) -> usize {
        self.max_concurrent.clamp(1, 16)
    }
}

/// The static rules (everything that needs no I/O).
pub struct Policy<'a> {
    pub cfg: &'a DelegationConfig,
    pub caller: &'a Caller,
}

impl Policy<'_> {
    /// Check `agent` and the requested `permission`; returns the permission the run will have
    /// (what was requested: the rules only ever refuse, they never silently downgrade).
    pub fn check(&self, agent: &str, requested: Permission) -> Result<Permission, Refusal> {
        if !self.cfg.enabled {
            return Err(Refusal::new(
                RefusalCode::Disabled,
                "delegation is turned off ([delegation] enabled = false in config.toml, or Settings > Agents > Delegation)",
            ));
        }
        if !self.cfg.allowed_agents.is_empty()
            && !self
                .cfg
                .allowed_agents
                .iter()
                .any(|a| a.eq_ignore_ascii_case(agent))
        {
            return Err(Refusal::new(
                RefusalCode::AgentNotAllowed,
                format!(
                    "{agent} is not in the allowed agents ({})",
                    self.cfg.allowed_agents.join(", ")
                ),
            ));
        }
        if requested == Permission::Full {
            return Err(Refusal::new(
                RefusalCode::PermissionFull,
                "the `full` permission is never delegable; use read-only, ask or accept-edits",
            ));
        }
        if requested > self.caller.permission {
            return Err(Refusal::new(
                RefusalCode::PermissionExceedsParent,
                format!(
                    "requested permission {requested} exceeds the delegating session's {} ({ENV_PARENT_PERMISSION}; \
                     unset means read-only). A delegation can never have more permission than its parent",
                    self.caller.permission
                ),
            ));
        }
        let cap = self.cfg.max_permission.min(Permission::AcceptEdits);
        if requested > cap {
            return Err(Refusal::new(
                RefusalCode::PermissionExceedsConfig,
                format!(
                    "requested permission {requested} exceeds [delegation] max_permission = {cap}"
                ),
            ));
        }
        let child_depth = self.caller.depth.saturating_add(1);
        if child_depth > self.cfg.max_depth {
            return Err(Refusal::new(
                RefusalCode::DepthExceeded,
                format!(
                    "delegation depth {child_depth} exceeds the maximum of {} ({ENV_DELEGATION_DEPTH}={}); \
                     a delegated agent cannot delegate any further",
                    self.cfg.max_depth, self.caller.depth
                ),
            ));
        }
        Ok(requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_check(
        caller: Caller,
        cfg: DelegationConfig,
        agent: &str,
        p: Permission,
    ) -> Result<Permission, Refusal> {
        Policy {
            cfg: &cfg,
            caller: &caller,
        }
        .check(agent, p)
    }

    #[test]
    fn the_approvals_mode_is_read_from_the_delegation_section() {
        assert_eq!(DelegationConfig::default().approvals, ApprovalMode::Ask);
        let (c, w) = DelegationConfig::from_toml(
            "[delegation]
approvals = \"allowlist\"
",
        );
        assert_eq!((c.approvals, w), (ApprovalMode::Allowlist, None));
        // an unknown value falls back to the defaults (ask) with a warning, never to something looser
        let (c, w) = DelegationConfig::from_toml(
            "[delegation]
approvals = \"yolo\"
",
        );
        assert_eq!(c, DelegationConfig::default());
        assert!(w.unwrap().contains("approvals"));
    }

    #[test]
    fn permission_order_and_parse() {
        assert!(
            Permission::ReadOnly < Permission::Ask && Permission::Ask < Permission::AcceptEdits
        );
        assert!(Permission::AcceptEdits < Permission::Full);
        for p in [
            Permission::ReadOnly,
            Permission::Ask,
            Permission::AcceptEdits,
            Permission::Full,
        ] {
            assert_eq!(Permission::parse(p.as_str()), Some(p));
            assert_eq!(Permission::from_driver(p.to_driver()), p);
        }
        assert_eq!(Permission::parse("READ_ONLY"), Some(Permission::ReadOnly));
        assert_eq!(
            Permission::parse("accept_edits"),
            Some(Permission::AcceptEdits)
        );
        assert_eq!(Permission::parse("yolo"), None);
    }

    #[test]
    fn caller_defaults_to_read_only_and_fails_closed() {
        let c = Caller::from_vars(|_| None);
        assert_eq!(
            (c.permission, c.depth, c.parent_thread),
            (Permission::ReadOnly, 0, None)
        );
        let c = Caller::from_vars(|k| match k {
            ENV_PARENT_PERMISSION => Some("accept-edits".into()),
            ENV_DELEGATION_DEPTH => Some("1".into()),
            ENV_PARENT_THREAD => Some("42".into()),
            ENV_PARENT_TAB => Some("tab-3".into()),
            _ => None,
        });
        assert_eq!(
            (
                c.permission,
                c.depth,
                c.parent_thread,
                c.parent_tab.as_deref()
            ),
            (Permission::AcceptEdits, 1, Some(42), Some("tab-3"))
        );
        let c = Caller::from_vars(|k| (k == ENV_PARENT_PERMISSION).then(|| "garbage".to_string()));
        assert_eq!(c.permission, Permission::ReadOnly);
    }

    #[test]
    fn permission_never_exceeds_the_parent() {
        let parent_ask = Caller {
            permission: Permission::Ask,
            ..Caller::default()
        };
        let cfg = DelegationConfig::default();
        assert!(policy_check(
            parent_ask.clone(),
            cfg.clone(),
            "codex",
            Permission::ReadOnly
        )
        .is_ok());
        assert!(policy_check(parent_ask.clone(), cfg.clone(), "codex", Permission::Ask).is_ok());
        let e =
            policy_check(parent_ask, cfg.clone(), "codex", Permission::AcceptEdits).unwrap_err();
        assert_eq!(e.code, RefusalCode::PermissionExceedsParent);
        assert!(e.reason.contains(ENV_PARENT_PERMISSION));
        // default parent (nothing in the environment) is read-only
        let e = policy_check(Caller::default(), cfg, "codex", Permission::Ask).unwrap_err();
        assert_eq!(e.code, RefusalCode::PermissionExceedsParent);
    }

    #[test]
    fn full_is_never_delegable_even_from_a_full_parent() {
        let parent = Caller {
            permission: Permission::Full,
            ..Caller::default()
        };
        let e = policy_check(
            parent.clone(),
            DelegationConfig::default(),
            "claude",
            Permission::Full,
        )
        .unwrap_err();
        assert_eq!(e.code, RefusalCode::PermissionFull);
        // ... and a config that says `full` is still clamped to accept-edits
        let cfg = DelegationConfig {
            max_permission: Permission::Full,
            ..DelegationConfig::default()
        };
        assert!(policy_check(parent, cfg, "claude", Permission::AcceptEdits).is_ok());
    }

    #[test]
    fn config_cap_depth_and_allowed_agents() {
        let parent = Caller {
            permission: Permission::AcceptEdits,
            ..Caller::default()
        };
        let cfg = DelegationConfig {
            max_permission: Permission::Ask,
            ..DelegationConfig::default()
        };
        assert_eq!(
            policy_check(parent, cfg, "codex", Permission::AcceptEdits)
                .unwrap_err()
                .code,
            RefusalCode::PermissionExceedsConfig
        );

        let cfg = DelegationConfig::default(); // max_depth 1: a child cannot re-delegate
        let at = |d| Caller {
            depth: d,
            permission: Permission::AcceptEdits,
            ..Caller::default()
        };
        assert!(policy_check(at(0), cfg.clone(), "codex", Permission::ReadOnly).is_ok());
        let e = policy_check(at(1), cfg, "codex", Permission::ReadOnly).unwrap_err();
        assert_eq!(e.code, RefusalCode::DepthExceeded);
        let two = DelegationConfig {
            max_depth: 2,
            ..DelegationConfig::default()
        };
        assert!(policy_check(at(1), two.clone(), "codex", Permission::ReadOnly).is_ok());
        assert_eq!(
            policy_check(at(2), two, "codex", Permission::ReadOnly)
                .unwrap_err()
                .code,
            RefusalCode::DepthExceeded
        );

        let cfg = DelegationConfig {
            allowed_agents: vec!["Codex".into()],
            ..DelegationConfig::default()
        };
        assert!(policy_check(
            Caller::default(),
            cfg.clone(),
            "codex",
            Permission::ReadOnly
        )
        .is_ok());
        assert_eq!(
            policy_check(Caller::default(), cfg, "gemini", Permission::ReadOnly)
                .unwrap_err()
                .code,
            RefusalCode::AgentNotAllowed
        );
        let off = DelegationConfig {
            enabled: false,
            ..DelegationConfig::default()
        };
        assert_eq!(
            policy_check(Caller::default(), off, "codex", Permission::ReadOnly)
                .unwrap_err()
                .code,
            RefusalCode::Disabled
        );
    }

    #[test]
    fn config_toml_section() {
        let (c, w) = DelegationConfig::from_toml("[agents]\nusage_week_start = \"monday\"\n");
        assert_eq!((c, w), (DelegationConfig::default(), None));
        let (c, w) = DelegationConfig::from_toml(
            "[delegation]\nmax_concurrent = 2\nmax_permission = \"read_only\"\nallowed_agents = [\"codex\"]\ndaily_token_budget = 10\nunknown = 1\n",
        );
        assert!(w.is_none(), "{w:?}");
        assert_eq!(
            (c.max_concurrent, c.max_permission, c.daily_token_budget),
            (2, Permission::ReadOnly, 10)
        );
        assert_eq!(c.allowed_agents, vec!["codex"]);
        assert_eq!(c.timeout_secs, 900, "unlisted fields keep their defaults");
        let (c, w) = DelegationConfig::from_toml("[delegation]\nmax_permission = \"root\"\n");
        assert_eq!(c, DelegationConfig::default());
        assert!(w.unwrap().contains("invalid"));
        let (_, w) = DelegationConfig::from_toml("= not toml");
        assert!(w.is_some());
    }
}
