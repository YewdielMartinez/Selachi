//! Agent CLIs found on this machine (`agents_driver::detect`), as rows for the Agents section.
//!
//! `detect()` runs the `--version` probes and is blocking: it is called on the background
//! executor, only when the Agents section opens (or on "Volver a comprobar").

use std::path::{Path, PathBuf};

use agents_driver::{AgentKind, DetectedAgent};
use ui_kit::AgentMark;

/// How far the agent is from being usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    /// Found and a credential is present.
    Ready,
    /// Found, sign-in could not be verified (never reported as "logged out").
    Installed,
    /// Found and known to be signed out.
    LoginMissing,
    /// Not on PATH.
    NotInstalled,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Ready => "Listo",
            AgentStatus::Installed => "Instalado",
            AgentStatus::LoginMissing => "Falta iniciar sesión",
            AgentStatus::NotInstalled => "No instalado",
        }
    }
}

/// One line of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub mark: AgentMark,
    pub name: String,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
    pub status: AgentStatus,
}

/// What the section knows about detection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentsState {
    /// The section was not opened yet.
    Unknown,
    Detecting,
    Done(Vec<AgentRow>),
}

/// The CLIs `detect()` looks for, in display order.
pub const KNOWN: [(&str, AgentMark); 4] = [
    ("Claude Code", AgentMark::Claude),
    ("Codex", AgentMark::Codex),
    ("OpenCode", AgentMark::OpenCode),
    ("Gemini CLI", AgentMark::Gemini),
];

fn mark_of(kind: &AgentKind, path: &Path) -> AgentMark {
    match kind {
        AgentKind::ClaudeCode => AgentMark::Claude,
        AgentKind::Codex => AgentMark::Codex,
        AgentKind::OpenCode => AgentMark::OpenCode,
        AgentKind::Acp { command } => {
            let by_cmd = AgentMark::from_name(command);
            if by_cmd == AgentMark::Unknown {
                AgentMark::from_name(&path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
            } else {
                by_cmd
            }
        }
    }
}

/// Merge the detection result with the list of known CLIs: every known one gets a row.
pub fn rows(detected: &[DetectedAgent]) -> Vec<AgentRow> {
    let mut out: Vec<AgentRow> = KNOWN
        .iter()
        .map(|(name, mark)| {
            match detected.iter().find(|d| mark_of(&d.kind, &d.path) == *mark) {
                Some(d) => AgentRow {
                    mark: *mark,
                    name: (*name).to_string(),
                    version: d.version.clone(),
                    path: Some(d.path.clone()),
                    status: match d.logged_in {
                        Some(true) => AgentStatus::Ready,
                        Some(false) => AgentStatus::LoginMissing,
                        None => AgentStatus::Installed,
                    },
                },
                None => AgentRow {
                    mark: *mark,
                    name: (*name).to_string(),
                    version: None,
                    path: None,
                    status: AgentStatus::NotInstalled,
                },
            }
        })
        .collect();
    // Anything else `detect()` returned that is not one of the known four (future agents).
    for d in detected {
        let m = mark_of(&d.kind, &d.path);
        if !KNOWN.iter().any(|(_, k)| *k == m) {
            out.push(AgentRow {
                mark: m,
                name: d.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "agente".into()),
                version: d.version.clone(),
                path: Some(d.path.clone()),
                status: match d.logged_in {
                    Some(true) => AgentStatus::Ready,
                    Some(false) => AgentStatus::LoginMissing,
                    None => AgentStatus::Installed,
                },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(kind: AgentKind, path: &str, logged: Option<bool>) -> DetectedAgent {
        DetectedAgent { kind, path: PathBuf::from(path), version: Some("1.2.3".into()), logged_in: logged }
    }

    #[test]
    fn every_known_cli_has_a_row_and_missing_ones_say_so() {
        let r = rows(&[]);
        assert_eq!(r.len(), 4);
        assert!(r.iter().all(|r| r.status == AgentStatus::NotInstalled && r.path.is_none()));
        assert_eq!(r[0].name, "Claude Code");
    }

    #[test]
    fn detected_agents_map_to_marks_and_status() {
        let r = rows(&[
            found(AgentKind::ClaudeCode, "C:/bin/claude.exe", Some(true)),
            found(AgentKind::OpenCode, "C:/bin/opencode.exe", None),
            found(AgentKind::Acp { command: "\"C:/x y/gemini.cmd\"".into() }, "C:/x y/gemini.cmd", None),
        ]);
        assert_eq!(r[0].status, AgentStatus::Ready);
        assert_eq!(r[0].version.as_deref(), Some("1.2.3"));
        assert_eq!(r[1].status, AgentStatus::NotInstalled, "codex was not found");
        assert_eq!(r[2].status, AgentStatus::Installed, "unknown login is not reported as logged out");
        assert_eq!(r[3].mark, AgentMark::Gemini);
        assert_eq!(r[3].status, AgentStatus::Installed);
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn unknown_login_is_never_a_missing_login() {
        let r = rows(&[found(AgentKind::Codex, "C:/bin/codex.exe", None)]);
        assert_eq!(r[1].status, AgentStatus::Installed);
        assert_ne!(r[1].status, AgentStatus::LoginMissing);
        let r = rows(&[found(AgentKind::Codex, "C:/bin/codex.exe", Some(false))]);
        assert_eq!(r[1].status, AgentStatus::LoginMissing);
    }
}
