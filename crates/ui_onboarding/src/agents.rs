//! Step 2: the agent CLIs, their detection state and the commands to log in or install them.
//!
//! Verified on 2026-11-21 (see the crate report):
//! * `claude --help` lists `auth`; `claude auth --help` lists `login`, so the login command is
//!   `claude auth login` (it opens the browser). Plain `claude` + `/login` also works.
//! * `opencode auth --help` lists `login`.
//! * `npm view` resolves all four packages. `codex login` and bare `gemini` could not be checked
//!   locally (not installed): they come from the vendors' documentation.

use std::path::PathBuf;

use agents_driver::{AgentKind, DetectedAgent};
use ui_kit::AgentMark;

/// The CLIs the wizard knows (the ones `agents_driver::detect` probes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentCli {
    Claude,
    Codex,
    OpenCode,
    Gemini,
}

impl AgentCli {
    pub const ALL: [AgentCli; 4] = [
        AgentCli::Claude,
        AgentCli::Codex,
        AgentCli::OpenCode,
        AgentCli::Gemini,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            AgentCli::Claude => "Claude Code",
            AgentCli::Codex => "Codex",
            AgentCli::OpenCode => "OpenCode",
            AgentCli::Gemini => "Gemini CLI",
        }
    }

    /// Executable name on PATH.
    pub const fn binary(self) -> &'static str {
        match self {
            AgentCli::Claude => "claude",
            AgentCli::Codex => "codex",
            AgentCli::OpenCode => "opencode",
            AgentCli::Gemini => "gemini",
        }
    }

    pub const fn mark(self) -> AgentMark {
        match self {
            AgentCli::Claude => AgentMark::Claude,
            AgentCli::Codex => AgentMark::Codex,
            AgentCli::OpenCode => AgentMark::OpenCode,
            AgentCli::Gemini => AgentMark::Gemini,
        }
    }

    /// What to type in a terminal to sign in.
    pub const fn login_command(self) -> &'static str {
        match self {
            AgentCli::Claude => "claude auth login",
            AgentCli::Codex => "codex login",
            AgentCli::OpenCode => "opencode auth login",
            AgentCli::Gemini => "gemini",
        }
    }

    /// The npm package that installs the CLI.
    pub const fn npm_package(self) -> &'static str {
        match self {
            AgentCli::Claude => "@anthropic-ai/claude-code",
            AgentCli::Codex => "@openai/codex",
            AgentCli::OpenCode => "opencode-ai",
            AgentCli::Gemini => "@google/gemini-cli",
        }
    }

    /// The copyable install command.
    pub fn install_command(self) -> String {
        format!("npm i -g {}", self.npm_package())
    }

    /// Which CLI a detected agent is (`detect` yields Gemini as an ACP agent).
    pub fn from_kind(kind: &AgentKind) -> AgentCli {
        match kind {
            AgentKind::ClaudeCode => AgentCli::Claude,
            AgentKind::Codex => AgentCli::Codex,
            AgentKind::OpenCode => AgentCli::OpenCode,
            AgentKind::Acp { .. } => AgentCli::Gemini,
        }
    }
}

/// What the row on the right says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    /// Detection still running (spinner).
    Checking,
    /// Installed and a credential was found.
    Ready,
    /// Installed and known not to be signed in.
    NeedsLogin,
    /// Installed, but the login state could not be told (`logged_in: None`): never claimed
    /// as signed out, the "Iniciar sesión" button stays available.
    Unverified,
    /// Not on PATH: show the install command.
    NotInstalled,
}

impl AgentStatus {
    /// Text of the status ("Comprobando…", "Listo", ...).
    pub const fn label(self) -> &'static str {
        match self {
            AgentStatus::Checking => "Comprobando…",
            AgentStatus::Ready => "Listo",
            AgentStatus::NeedsLogin => "Falta iniciar sesión",
            AgentStatus::Unverified => "Instalado",
            AgentStatus::NotInstalled => "No instalado",
        }
    }

    /// Whether the "Iniciar sesión" button is offered.
    pub const fn offers_login(self) -> bool {
        matches!(self, AgentStatus::NeedsLogin | AgentStatus::Unverified)
    }
}

/// Maps `detect()` output for one agent to its status.
pub fn status_of(detected: &DetectedAgent) -> AgentStatus {
    match detected.logged_in {
        Some(true) => AgentStatus::Ready,
        Some(false) => AgentStatus::NeedsLogin,
        None => AgentStatus::Unverified,
    }
}

/// One row of the agents list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub cli: AgentCli,
    pub status: AgentStatus,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
}

impl AgentRow {
    pub fn checking(cli: AgentCli) -> Self {
        Self {
            cli,
            status: AgentStatus::Checking,
            version: None,
            path: None,
        }
    }
}

/// Every row in `AgentCli::ALL` order, with `NotInstalled` for the agents `detect` did not find.
pub fn rows_from_detected(detected: &[DetectedAgent]) -> Vec<AgentRow> {
    AgentCli::ALL
        .into_iter()
        .map(|cli| {
            match detected
                .iter()
                .find(|d| AgentCli::from_kind(&d.kind) == cli)
            {
                Some(d) => AgentRow {
                    cli,
                    status: status_of(d),
                    version: d.version.clone(),
                    path: Some(d.path.clone()),
                },
                None => AgentRow {
                    cli,
                    status: AgentStatus::NotInstalled,
                    version: None,
                    path: None,
                },
            }
        })
        .collect()
}

/// All rows in the `Checking` state (before `detect` answers).
pub fn checking_rows() -> Vec<AgentRow> {
    AgentCli::ALL.into_iter().map(AgentRow::checking).collect()
}

/// `"2.1.330 (Claude Code)"` -> `"2.1.330"`: the first token that starts with a digit.
pub fn short_version(version: &str) -> String {
    version
        .split_whitespace()
        .find(|t| {
            t.trim_start_matches('v')
                .starts_with(|c: char| c.is_ascii_digit())
        })
        .map_or_else(
            || version.trim().to_string(),
            |t| t.trim_start_matches('v').to_string(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(kind: AgentKind, logged_in: Option<bool>, version: &str) -> DetectedAgent {
        DetectedAgent {
            kind,
            path: PathBuf::from("C:/x/agent.exe"),
            version: Some(version.to_string()),
            logged_in,
        }
    }

    #[test]
    fn login_state_maps_to_row_status() {
        let k = AgentKind::ClaudeCode;
        assert_eq!(
            status_of(&detected(k.clone(), Some(true), "1")),
            AgentStatus::Ready
        );
        assert_eq!(
            status_of(&detected(k.clone(), Some(false), "1")),
            AgentStatus::NeedsLogin
        );
        assert_eq!(status_of(&detected(k, None, "1")), AgentStatus::Unverified);
        assert!(AgentStatus::NeedsLogin.offers_login());
        assert!(AgentStatus::Unverified.offers_login());
        assert!(!AgentStatus::Ready.offers_login());
        assert!(!AgentStatus::NotInstalled.offers_login());
        assert!(!AgentStatus::Checking.offers_login());
    }

    #[test]
    fn rows_cover_every_agent_and_mark_missing_ones() {
        let found = [
            detected(AgentKind::OpenCode, None, "1.19.6"),
            detected(AgentKind::ClaudeCode, Some(true), "2.1.330 (Claude Code)"),
            detected(
                AgentKind::Acp {
                    command: "\"C:/g.cmd\"".into(),
                },
                Some(true),
                "0.70.0",
            ),
        ];
        let rows = rows_from_detected(&found);
        let clis: Vec<_> = rows.iter().map(|r| r.cli).collect();
        assert_eq!(
            clis,
            AgentCli::ALL,
            "stable order, whatever detect() returned"
        );
        assert_eq!(rows[0].status, AgentStatus::Ready);
        assert_eq!(rows[1].status, AgentStatus::NotInstalled);
        assert_eq!(rows[1].version, None);
        assert_eq!(rows[2].status, AgentStatus::Unverified);
        assert_eq!(rows[3].status, AgentStatus::Ready, "Acp is Gemini");
        assert!(
            rows_from_detected(&[])
                .iter()
                .all(|r| r.status == AgentStatus::NotInstalled)
        );
    }

    #[test]
    fn checking_rows_start_in_the_checking_state() {
        let rows = checking_rows();
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r.status == AgentStatus::Checking));
    }

    #[test]
    fn commands_match_the_verified_ones() {
        assert_eq!(
            AgentCli::Claude.install_command(),
            "npm i -g @anthropic-ai/claude-code"
        );
        assert_eq!(AgentCli::Codex.install_command(), "npm i -g @openai/codex");
        assert_eq!(AgentCli::OpenCode.install_command(), "npm i -g opencode-ai");
        assert_eq!(
            AgentCli::Gemini.install_command(),
            "npm i -g @google/gemini-cli"
        );
        assert_eq!(AgentCli::Claude.login_command(), "claude auth login");
        assert_eq!(AgentCli::Codex.login_command(), "codex login");
        assert_eq!(AgentCli::OpenCode.login_command(), "opencode auth login");
        assert_eq!(AgentCli::Gemini.login_command(), "gemini");
    }

    #[test]
    fn version_is_shortened() {
        assert_eq!(short_version("2.1.330 (Claude Code)"), "2.1.330");
        assert_eq!(short_version("codex-cli 0.171.0"), "0.171.0");
        assert_eq!(short_version("v1.2"), "1.2");
        assert_eq!(short_version("weird"), "weird");
    }
}
