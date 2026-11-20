//! Detect installed agent CLIs (`claude`, `codex`, `gemini`) on PATH.
//!
//! Login status is a *cheap, offline* heuristic: we only check that a credential
//! file exists (or an API-key env var is set). File contents are never read.
//! `None` means "unknown" (we could not tell), never "logged out".

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::proc;
use crate::types::AgentKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedAgent {
    pub kind: AgentKind,
    /// Resolved executable (`.exe`, or the `.cmd` shim if that is all there is).
    pub path: PathBuf,
    /// First line of `--version`, if it answered within the timeout.
    pub version: Option<String>,
    pub logged_in: Option<bool>,
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `Some(true)` if any credential file exists or any of `env_vars` is set; else `None`.
pub fn logged_in_from(files: &[PathBuf], env_vars: &[&str]) -> Option<bool> {
    let by_file = files.iter().any(|f| f.is_file());
    let by_env = env_vars
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    (by_file || by_env).then_some(true)
}

fn credential_files(name: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home else { return Vec::new() };
    match name {
        "claude" => vec![home.join(".claude").join(".credentials.json")],
        "codex" => {
            let base = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            vec![base.join("auth.json")]
        }
        "gemini" => vec![home.join(".gemini").join("oauth_creds.json")],
        _ => Vec::new(),
    }
}

fn credential_env(name: &str) -> &'static [&'static str] {
    match name {
        "claude" => &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        "codex" => &["OPENAI_API_KEY", "CODEX_API_KEY"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        _ => &[],
    }
}

fn kind_for(name: &str, path: &Path) -> AgentKind {
    match name {
        "claude" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        // Gemini CLI speaks ACP; the exact ACP flag is left to the (future) ACP driver.
        _ => AgentKind::Acp {
            command: path.to_string_lossy().into_owned(),
        },
    }
}

fn probe(name: &'static str) -> Option<DetectedAgent> {
    let path = proc::resolve_executable(name)?;
    let mut cmd = proc::command_for(&path);
    cmd.arg("--version");
    let version = proc::run_capture(cmd, VERSION_TIMEOUT)
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty());
    let home = home_dir();
    let logged_in = logged_in_from(&credential_files(name, home.as_deref()), credential_env(name));
    Some(DetectedAgent {
        kind: kind_for(name, &path),
        path,
        version,
        logged_in,
    })
}

/// Find the supported agent CLIs on PATH. Runs the `--version` probes in
/// parallel (hidden windows, 5 s timeout each); blocking, call it off the UI thread.
pub fn detect() -> Vec<DetectedAgent> {
    let handles: Vec<_> = ["claude", "codex", "gemini"]
        .into_iter()
        .map(|n| std::thread::spawn(move || probe(n)))
        .collect();
    handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logged_in_requires_evidence() {
        assert_eq!(logged_in_from(&[], &[]), None);
        let missing = std::env::temp_dir().join("agents_driver_no_such_cred.json");
        assert_eq!(logged_in_from(&[missing], &["AGENTS_DRIVER_NO_SUCH_ENV"]), None);
        let f = std::env::temp_dir().join("agents_driver_cred_probe.json");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(logged_in_from(std::slice::from_ref(&f), &[]), Some(true));
        let _ = std::fs::remove_file(f);
    }

    #[test]
    fn credential_paths_are_per_agent() {
        let home = PathBuf::from("H");
        assert!(credential_files("claude", Some(&home))[0].ends_with(".credentials.json"));
        assert!(credential_files("gemini", Some(&home))[0].ends_with("oauth_creds.json"));
        assert!(credential_files("claude", None).is_empty());
    }

    /// Live: needs the CLIs installed. `cargo test -p agents_driver -- --ignored`
    #[test]
    #[ignore]
    fn live_detect_prints_agents() {
        let found = detect();
        for a in &found {
            eprintln!("{a:?}");
        }
        assert!(found.iter().all(|a| a.path.is_file()));
    }
}
