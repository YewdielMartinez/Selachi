//! Google Antigravity CLI (`agy`) specifics: locating the executable, the ACP command line, the
//! model / agent lists and the effort mapping.
//!
//! # Verified facts (agy 1.5.15, Windows 11, 2026-11-23)
//!
//! * **Transport: ACP over stdio** (`agy acp`), driven by the generic client in [`crate::acp`]; no
//!   protocol code of its own. The `agy -p --input-format stream-json --output-format stream-json`
//!   headless mode was **not needed**: the handshake already offers models, modes, thinking
//!   levels, permissions and session resume, so it stays documented as the fallback only.
//!   Live handshake recorded (anonymized) in `tests/fixtures/antigravity_acp_handshake.jsonl`.
//! * `initialize` -> `agentInfo{name:"Antigravity ACP Server",version}`, `agentCapabilities{
//!   loadSession, promptCapabilities{image,embeddedContext}, sessionCapabilities{resume,close,load,
//!   list,additionalDirectories}, auth{logout}, _meta.antigravity{tier,systemPrompt}}`, and four
//!   auth methods (`oauth-personal`, `oauth-business`, `gemini-api-key`, `agent-platform`).
//!   `session/new` succeeds **before** any login (auth is lazy), so the handshake cannot tell
//!   whether the user is signed in; see [`login_files`].
//! * `session/new` -> `sessionId` (UUID), `models.availableModels` (**effort baked into the id**:
//!   `gemini-4-neon-high`, `claude-opus-5-5-medium`, ...), `modes` (`default`, `accept-edits`,
//!   `auto`, `plan`) and `configOptions`: `model` (base ids: `gemini-4-neon`, `claude-opus-5-5`),
//!   `thought_level` (`low|medium|high|xhigh`) and `mode`. So **effort and model are session config
//!   options, not CLI flags**: `agy acp --effort high` prints the usage text and exits (verified),
//!   `--effort` / `--model` exist only for the interactive / `-p` mode.
//! * `agy acp` flags that matter: `--disable-telemetry`, `--agent <name>`, `--add-dir <dir>`
//!   (repeatable), `--dangerously-skip-permissions` (the `Full` mode, never otherwise), `--sandbox`,
//!   `--gemini-dir`, `--app-data-dir`.
//! * `agy models`: first line `Fetching available models...` (on stdout), then one
//!   `<id>\t<label>` line per model, effort included in both (`claude-opus-5-5-low\tClaude Opus 5.5
//!   (Low)`). [`parse_models_output`] folds them into one [`ModelInfo`] per base model with its
//!   efforts, matching what the ACP `model` + `thought_level` options give.
//! * `agy agents` / `agy agent`: prints one line per agent; empty on a machine without custom
//!   agents (the only case verified), so [`parse_agents_output`] is lenient.
//! * `%LOCALAPPDATA%\agy\bin` is **not** added to the PATH of an already open session, so
//!   [`resolve`] also looks there.
//!
//! # Mapping of [`ThreadConfig`]
//!
//! | Config                  | Antigravity                                                          |
//! |-------------------------|----------------------------------------------------------------------|
//! | `PermissionMode::Ask`   | mode `default` (every `session/request_permission` is surfaced)      |
//! | `AcceptEdits`           | ACP mode `accept-edits` (+ edits auto-allowed by the client)         |
//! | `ReadOnly`              | ACP mode `plan` (+ edit/execute auto-rejected by the client)         |
//! | `Full`                  | `--dangerously-skip-permissions` (+ everything auto-allowed)         |
//! | `model`                 | `session/set_config_option model` (`<base>-<effort>` ids are split)  |
//! | `effort`                | `session/set_config_option thought_level` (`max` -> `xhigh`)         |
//! | extra dirs / `--agent`  | `--add-dir` / `--agent` in [`acp_command_line`]                      |

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::proc;
use crate::types::{
    AgentKind, DriverError, ModelInfo, PermissionMode, Result, ThreadConfig, EFFORT_LEVELS,
};

/// `agy` writes its installer here and does not touch the PATH of the running session.
pub fn default_bin() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let name = if cfg!(windows) { "agy.exe" } else { "agy" };
    let p = base.join("agy").join("bin").join(name);
    p.is_file().then_some(p)
}

/// The `agy` executable: `agy` on PATH, else `%LOCALAPPDATA%\agy\bin\agy.exe`.
pub fn resolve() -> Option<PathBuf> {
    proc::find_on_path("agy").or_else(default_bin)
}

/// Files whose **existence** hints at a Google sign-in (contents are never read): the OAuth file
/// and the account registry of the shared `~/.gemini` directory. Only a hint: agy may keep its
/// token elsewhere (the handshake works without any login), so a miss means "unknown".
pub fn login_files(home: &Path) -> Vec<PathBuf> {
    let g = home.join(".gemini");
    vec![g.join("oauth_creds.json"), g.join("google_accounts.json")]
}

/// Environment variables that give agy an API-key login (`gemini-api-key` auth method).
pub const CREDENTIAL_ENV: &[&str] = &["GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// `agy acp` command line for [`AgentKind::Acp`]: the quoted executable plus the stable flags,
/// optionally `--agent <name>` and one `--add-dir` per extra workspace directory.
pub fn acp_command_line(exe: &Path, agent: Option<&str>, add_dirs: &[PathBuf]) -> String {
    let mut s = format!("\"{}\" acp --disable-telemetry", exe.to_string_lossy());
    if let Some(a) = agent.map(str::trim).filter(|a| !a.is_empty()) {
        s.push_str(&format!(" --agent {}", quote(a)));
    }
    for d in add_dirs {
        s.push_str(&format!(" --add-dir {}", quote(&d.to_string_lossy())));
    }
    s
}

fn quote(s: &str) -> String {
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

impl AgentKind {
    /// The Antigravity CLI as an ACP preset (`agy acp --disable-telemetry ...`). Antigravity is
    /// deliberately **not** a new enum variant: `AgentKind::Acp { command }` with an `agy`
    /// executable is recognised by [`crate::is_antigravity`], so every existing exhaustive match
    /// (chat core, UI) keeps compiling and already treats it as a generic ACP agent.
    pub fn antigravity(exe: &Path, agent: Option<&str>, add_dirs: &[PathBuf]) -> AgentKind {
        AgentKind::Acp {
            command: acp_command_line(exe, agent, add_dirs),
        }
    }
}

/// Final argument list of `agy` for `cfg`, from the arguments the caller's command line carried
/// (`parts`, without the program). Guarantees `acp` first and `--disable-telemetry`, appends
/// `cfg.extra_args`, and adds `--dangerously-skip-permissions` **only** for
/// [`PermissionMode::Full`] (a copy of the flag in `parts` is dropped in every other mode).
pub fn finalize_args(mut parts: Vec<String>, cfg: &ThreadConfig) -> Vec<String> {
    let skip = "--dangerously-skip-permissions";
    parts.extend(cfg.extra_args.iter().cloned());
    parts.retain(|p| p != skip);
    if parts.first().map(String::as_str) != Some("acp") {
        parts.insert(0, "acp".to_string());
    }
    if !parts
        .iter()
        .any(|p| p == "--disable-telemetry" || p == "--disable_telemetry")
    {
        parts.insert(1, "--disable-telemetry".to_string());
    }
    if cfg.permission_mode == PermissionMode::Full {
        parts.push(skip.to_string());
    }
    parts
}

/// Effort suffixes `agy models` bakes into ids, longest first.
const EFFORT_SUFFIXES: [&str; 5] = ["xhigh", "medium", "high", "low", "max"];

/// `claude-opus-5-5-high` -> `("claude-opus-5-5", "high")`.
pub fn split_effort_suffix(id: &str) -> Option<(&str, &str)> {
    let (base, tail) = id.rsplit_once('-')?;
    (!base.is_empty() && EFFORT_SUFFIXES.contains(&tail)).then_some((base, tail))
}

/// The `thought_level` value to select for the UI effort `want` among the `offered` values:
/// itself when offered, `max` -> `xhigh`, otherwise the closest offered level below it, else
/// `None` (unknown effort, or an agent that offers no such level).
pub fn thought_level_for<'a>(want: &str, offered: &'a [String]) -> Option<&'a str> {
    let want = want.trim().to_ascii_lowercase();
    if let Some(o) = offered.iter().find(|o| **o == want) {
        return Some(o);
    }
    let rank = EFFORT_LEVELS.iter().position(|l| *l == want)?;
    EFFORT_LEVELS[..rank]
        .iter()
        .rev()
        .find_map(|l| offered.iter().find(|o| o == l))
        .map(String::as_str)
}

/// Label without the trailing ` (High)` effort tag.
fn strip_label_effort(label: &str) -> &str {
    match label.rsplit_once(" (") {
        Some((head, tail))
            if tail
                .strip_suffix(')')
                .is_some_and(|t| EFFORT_SUFFIXES.contains(&t.to_ascii_lowercase().as_str())) =>
        {
            head
        }
        _ => label,
    }
}

/// Fold flat `(id, label)` model rows (effort baked into the id) into one entry per base model.
/// A model with a single variant, or without an effort suffix, keeps its own id.
pub fn group_models(rows: &[(String, String)]) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = Vec::new();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (id, _) in rows {
        if let Some((base, _)) = split_effort_suffix(id) {
            match counts.iter_mut().find(|(b, _)| b == base) {
                Some((_, n)) => *n += 1,
                None => counts.push((base.to_string(), 1)),
            }
        }
    }
    for (id, label) in rows {
        let grouped =
            split_effort_suffix(id).filter(|(b, _)| counts.iter().any(|(c, n)| c == b && *n > 1));
        match grouped {
            Some((base, eff)) => {
                let pos = out.iter().position(|m| m.id == base).unwrap_or_else(|| {
                    out.push(ModelInfo {
                        id: base.to_string(),
                        label: strip_label_effort(label).to_string(),
                        efforts: Vec::new(),
                        default_effort: None,
                    });
                    out.len() - 1
                });
                out[pos].efforts.push(eff.to_string());
            }
            None => out.push(ModelInfo {
                id: id.clone(),
                label: if label.is_empty() {
                    id.clone()
                } else {
                    label.clone()
                },
                efforts: Vec::new(),
                default_effort: None,
            }),
        }
    }
    for m in &mut out {
        m.efforts.sort_by_key(|e| {
            EFFORT_LEVELS
                .iter()
                .position(|l| l == e)
                .unwrap_or(usize::MAX)
        });
    }
    out
}

/// Parse the stdout of `agy models`: `Fetching ...` progress lines are skipped, every other
/// `<id>\t<label>` line is a model.
pub fn parse_models_output(stdout: &str) -> Vec<ModelInfo> {
    let rows: Vec<(String, String)> = stdout
        .lines()
        .filter_map(|l| {
            let l = l.trim_end_matches('\r');
            let (id, label) = l.split_once('\t')?;
            let id = id.trim();
            (!id.is_empty() && !id.contains(char::is_whitespace))
                .then(|| (id.to_string(), label.trim().to_string()))
        })
        .collect();
    group_models(&rows)
}

/// An agent offered by `agy agents` (`--agent <id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub description: String,
}

/// Parse the stdout of `agy agents`: one agent per line, `<id>` optionally followed by a tab (or
/// two spaces, or ` - `) and a description; `Fetching ...` progress lines and blanks are skipped.
pub fn parse_agents_output(stdout: &str) -> Vec<AgentProfile> {
    stdout
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .filter(|l| !l.is_empty() && !l.to_ascii_lowercase().starts_with("fetching"))
        .filter_map(|l| {
            let (id, rest) = l
                .split_once('\t')
                .or_else(|| l.split_once("  "))
                .or_else(|| l.split_once(" - "))
                .unwrap_or((l, ""));
            let id = id.trim();
            (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| AgentProfile {
                id: id.to_string(),
                description: rest.trim().to_string(),
            })
        })
        .collect()
}

fn run(exe: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut cmd = proc::command_for(exe);
    cmd.args(args);
    proc::run_capture(cmd, timeout).ok_or_else(|| {
        DriverError::Protocol(format!("`agy {}` gave no output in time", args.join(" ")))
    })
}

/// `agy models` (no prompt, no session): the models grouped by base id with their efforts.
/// Blocking: call it from a background thread.
pub fn list_models(timeout: Duration) -> Result<Vec<ModelInfo>> {
    let exe = resolve().ok_or_else(|| DriverError::NotFound("agy".into()))?;
    let models = parse_models_output(&run(&exe, &["models"], timeout)?);
    if models.is_empty() {
        return Err(DriverError::Protocol("`agy models` listed nothing".into()));
    }
    Ok(models)
}

/// `agy agents`: the custom agents usable with `--agent`. Blocking.
pub fn list_agents(timeout: Duration) -> Result<Vec<AgentProfile>> {
    let exe = resolve().ok_or_else(|| DriverError::NotFound("agy".into()))?;
    Ok(parse_agents_output(&run(&exe, &["agents"], timeout)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: &str = "Fetching available models...\r\n\
gemini-4-neon-high\tGemini 4 Neon (High)\r\n\
gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)\r\n\
gemini-4-neon-medium\tGemini 4 Neon (Medium)\r\n\
gemini-4-neon-low\tGemini 4 Neon (Low)\r\n\
gemini-pro-orchestrator-neon\tOrchestrator (Neon)\r\n\
claude-opus-5-5-low\tClaude Opus 5.5 (Low)\r\n\
claude-opus-5-5-high\tClaude Opus 5.5 (High)\r\n\
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\r\n";

    #[test]
    fn models_output_is_grouped_by_base_model() {
        let m = parse_models_output(MODELS);
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].id, "gemini-4-neon");
        assert_eq!(m[0].label, "Gemini 4 Neon");
        assert_eq!(m[0].efforts, ["low", "medium", "high", "xhigh"]);
        // no effort suffix: kept as is
        assert_eq!(
            (m[1].id.as_str(), m[1].efforts.len()),
            ("gemini-pro-orchestrator-neon", 0)
        );
        assert_eq!(
            (m[2].id.as_str(), m[2].efforts.as_slice()),
            (
                "claude-opus-5-5",
                ["low".to_string(), "high".to_string()].as_slice()
            )
        );
        // a single variant is not a family: the full id is what the agent wants back
        assert_eq!(
            (m[3].id.as_str(), m[3].label.as_str()),
            ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)")
        );
        assert!(parse_models_output("Fetching available models...\n").is_empty());
    }

    #[test]
    fn agents_output_is_parsed_leniently() {
        assert!(parse_agents_output("").is_empty());
        let a = parse_agents_output(
            "Fetching agents...\nreviewer\tReviews diffs\nbuilder\nplanner  Plans work\n",
        );
        assert_eq!(a.len(), 3);
        assert_eq!(
            a[0],
            AgentProfile {
                id: "reviewer".into(),
                description: "Reviews diffs".into()
            }
        );
        assert_eq!(a[1].description, "");
        assert_eq!(a[2].id, "planner");
    }

    #[test]
    fn effort_suffix_and_thought_level_mapping() {
        assert_eq!(
            split_effort_suffix("claude-opus-5-5-high"),
            Some(("claude-opus-5-5", "high"))
        );
        assert_eq!(
            split_effort_suffix("gemini-4-neon-xhigh"),
            Some(("gemini-4-neon", "xhigh"))
        );
        assert_eq!(split_effort_suffix("gemini-4-neon"), None);
        let offered: Vec<String> = ["low", "medium", "high", "xhigh"]
            .map(String::from)
            .to_vec();
        assert_eq!(thought_level_for("high", &offered), Some("high"));
        assert_eq!(thought_level_for("max", &offered), Some("xhigh"));
        assert_eq!(thought_level_for("MAX", &offered[..3]), Some("high"));
        assert_eq!(thought_level_for("turbo", &offered), None);
        assert_eq!(thought_level_for("low", &[]), None);
        for l in EFFORT_LEVELS {
            assert!(thought_level_for(l, &offered).is_some(), "{l}");
        }
    }

    #[test]
    fn command_line_carries_flags_and_quotes() {
        let s = acp_command_line(
            Path::new("C:/Users/a b/agy.exe"),
            Some("my agent"),
            &[PathBuf::from("D:/x y"), PathBuf::from("E:/z")],
        );
        assert_eq!(
            s,
            r#""C:/Users/a b/agy.exe" acp --disable-telemetry --agent "my agent" --add-dir "D:/x y" --add-dir E:/z"#
        );
        assert!(crate::is_antigravity(&s));
        let parts = crate::acp::split_command(&s);
        assert_eq!(parts[0], "C:/Users/a b/agy.exe");
        assert_eq!(parts[4], "my agent");
        assert!(
            matches!(AgentKind::antigravity(Path::new("agy.exe"), None, &[]), AgentKind::Acp { command } if command == "\"agy.exe\" acp --disable-telemetry")
        );
    }

    #[test]
    fn skip_permissions_only_in_full_mode() {
        let mut cfg = ThreadConfig::new(".");
        let args = |c: &ThreadConfig, p: &[&str]| {
            finalize_args(p.iter().map(|s| s.to_string()).collect(), c)
        };
        for mode in [
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::ReadOnly,
        ] {
            cfg.permission_mode = mode;
            let a = args(&cfg, &["--dangerously-skip-permissions"]);
            assert_eq!(a, ["acp", "--disable-telemetry"], "{mode:?}");
        }
        cfg.permission_mode = PermissionMode::Full;
        cfg.extra_args = vec!["--add-dir".into(), "D:/x".into()];
        let a = args(&cfg, &["acp", "--disable-telemetry"]);
        assert_eq!(
            a,
            [
                "acp",
                "--disable-telemetry",
                "--add-dir",
                "D:/x",
                "--dangerously-skip-permissions"
            ]
        );
        // a bare `agy` command line still becomes `agy acp --disable-telemetry`
        cfg.permission_mode = PermissionMode::Ask;
        cfg.extra_args.clear();
        assert_eq!(args(&cfg, &[]), ["acp", "--disable-telemetry"]);
        assert_eq!(
            args(&cfg, &["acp", "--disable_telemetry"]),
            ["acp", "--disable_telemetry"]
        );
    }

    #[test]
    fn login_markers_are_in_the_shared_gemini_dir() {
        let f = login_files(Path::new("H"));
        assert!(f
            .iter()
            .all(|p| p.starts_with(Path::new("H").join(".gemini"))));
        assert!(f.iter().any(|p| p.ends_with("oauth_creds.json")));
        assert!(CREDENTIAL_ENV.contains(&"GEMINI_API_KEY"));
    }

    #[test]
    fn default_bin_lives_under_localappdata() {
        if let Some(p) = default_bin() {
            assert!(
                p.ends_with(Path::new("agy").join("bin").join(if cfg!(windows) {
                    "agy.exe"
                } else {
                    "agy"
                }))
            );
        }
    }
}
