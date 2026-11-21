//! OpenCode (<https://opencode.ai>, sst/anomalyco `opencode`) specifics: locating the
//! real executable on Windows, its data/credential paths and the `acp` command line.
//!
//! # Verified facts (opencode 1.14.48, Windows 11, npm global install)
//!
//! * `opencode acp` speaks the Agent Client Protocol (protocol version 1) as
//!   newline-delimited JSON-RPC 2.0 on stdin/stdout, no network port
//!   (<https://opencode.ai/docs/acp/>; live handshake recorded in
//!   `tests/fixtures/opencode_acp_handshake.jsonl`). Flags: `--cwd`, `--print-logs`,
//!   `--pure`, `--log-level`, plus `--port/--hostname/--mdns/--cors` (unused by ACP).
//! * `initialize` -> `agentInfo{name:"OpenCode",version}`, `agentCapabilities{loadSession,
//!   promptCapabilities{image,embeddedContext}, mcpCapabilities{http,sse},
//!   sessionCapabilities{close,fork,list,resume}}`, one auth method `opencode-login`.
//! * `session/new` -> `sessionId` (`ses_...`), `models`, `modes` (`build`, `plan`) and
//!   `configOptions` (`model`, `mode`, `effort` when the model has variants).
//! * Server side it emits `usage_update{used,size,cost{amount,currency}}` and puts
//!   `usage{inputTokens,outputTokens,thoughtTokens,cachedReadTokens,cachedWriteTokens}` on
//!   the `session/prompt` response (taken from the **last** assistant message of the turn only,
//!   so multi-step turns under-report: `agents_svc` reads the exact figures from OpenCode's
//!   own database).
//! * Alternative transport, not used: `opencode serve` (HTTP + SSE, `/api/session`, `/api/event`).
//!
//! # Windows packaging
//!
//! The npm global install puts `opencode.cmd` on PATH. It runs
//! `node node_modules/opencode-ai/bin/opencode`, a launcher that picks
//! `node_modules/opencode-ai/node_modules/opencode-windows-x64[-baseline]/bin/opencode.exe`
//! (baseline when the CPU lacks AVX2). The generic `.cmd` shim parser in [`crate::proc`]
//! cannot follow that hop, so [`exe_from_shim`] mirrors the launcher's lookup and we spawn
//! the real `.exe` directly: no `cmd.exe` + `node` in the middle, stdin pipes and
//! `taskkill /T` behave.

use std::path::{Path, PathBuf};

use crate::proc;
use crate::types::ThreadConfig;

/// `OPENCODE_BIN_PATH` is honoured by the official launcher; so do we.
pub const BIN_ENV: &str = "OPENCODE_BIN_PATH";

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    }
}

/// Package names of the platform binary, preferred first (mirrors the npm launcher).
pub fn platform_packages() -> Vec<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        a => a,
    };
    let base = format!("opencode-{os}-{arch}");
    if arch == "x64" {
        let baseline = format!("{base}-baseline");
        if has_avx2() {
            vec![base, baseline]
        } else {
            vec![baseline, base]
        }
    } else {
        vec![base]
    }
}

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}
#[cfg(not(target_arch = "x86_64"))]
fn has_avx2() -> bool {
    false
}

/// Given an npm shim (`.../npm/opencode.cmd`), find the real executable the launcher
/// would run. Pure filesystem lookups, no process is started.
pub fn exe_from_shim(shim: &Path) -> Option<PathBuf> {
    let dir = shim.parent()?;
    let pkg = dir.join("node_modules").join("opencode-ai");
    // The launcher's own cache (`bin/.opencode`) wins when present.
    let cached = pkg.join("bin").join(".opencode");
    if cached.is_file() {
        return Some(cached);
    }
    let names = platform_packages();
    for modules in [pkg.join("node_modules"), dir.join("node_modules")] {
        for n in &names {
            let cand = modules.join(n).join("bin").join(binary_name());
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// The real OpenCode executable: `OPENCODE_BIN_PATH`, else `opencode` on PATH (npm `.cmd`
/// shims are resolved to the platform `.exe`; if that fails the shim itself is returned),
/// else the default installer location (`~/.opencode/bin`).
pub fn resolve() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(BIN_ENV).map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(found) = proc::find_on_path("opencode") {
        let is_shim = found
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
        if is_shim {
            return Some(exe_from_shim(&found).unwrap_or(found));
        }
        return Some(found);
    }
    let home = home_dir()?;
    let cand = home.join(".opencode").join("bin").join(binary_name());
    cand.is_file().then_some(cand)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// OpenCode's data directory (`opencode debug paths` -> `data`): `$XDG_DATA_HOME/opencode`
/// or `~/.local/share/opencode` (also on Windows, verified).
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home_dir().map(|h| h.join(".local").join("share")))?;
    Some(base.join("opencode"))
}

/// Legacy credential file (`<data>/auth.json`). Only its existence is ever checked: on
/// v2 installs credentials live in OpenCode's SQLite database, which we never open.
pub fn auth_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join("auth.json"))
}

/// Environment variables that, when set, give OpenCode a usable provider.
pub const CREDENTIAL_ENV: &[&str] = &[
    "OPENCODE_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "DEEPSEEK_API_KEY",
    "XAI_API_KEY",
    "MISTRAL_API_KEY",
];

/// Command-line arguments (after the executable) to start the ACP server for `cfg`.
pub fn acp_args(cfg: &ThreadConfig) -> Vec<String> {
    let mut v = vec!["acp".to_string(), "--cwd".to_string(), cfg.cwd.to_string_lossy().into_owned()];
    v.extend(cfg.extra_args.iter().cloned());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_resolves_nested_platform_package() {
        let dir = std::env::temp_dir().join("agents_driver_opencode_shim");
        let _ = std::fs::remove_dir_all(&dir);
        let first = platform_packages().remove(0);
        let bin = dir
            .join("node_modules")
            .join("opencode-ai")
            .join("node_modules")
            .join(&first)
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(binary_name());
        std::fs::write(&exe, b"").unwrap();
        let shim = dir.join("opencode.cmd");
        assert_eq!(exe_from_shim(&shim), Some(exe));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shim_falls_back_to_baseline_when_only_it_exists() {
        if std::env::consts::ARCH != "x86_64" {
            return;
        }
        let dir = std::env::temp_dir().join("agents_driver_opencode_baseline");
        let _ = std::fs::remove_dir_all(&dir);
        let name = platform_packages().into_iter().find(|n| n.ends_with("-baseline")).unwrap();
        let bin = dir.join("node_modules").join(&name).join("bin"); // hoisted layout
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(binary_name());
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(exe_from_shim(&dir.join("opencode.cmd")), Some(exe));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shim_without_packages_is_none() {
        let dir = std::env::temp_dir().join("agents_driver_opencode_none");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(exe_from_shim(&dir.join("opencode.cmd")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn acp_args_carry_cwd_and_extras() {
        let mut cfg = ThreadConfig::new("C:/work/proj");
        cfg.extra_args = vec!["--pure".into()];
        assert_eq!(acp_args(&cfg), vec!["acp", "--cwd", "C:/work/proj", "--pure"]);
    }

    #[test]
    fn auth_file_lives_under_data_dir() {
        if let (Some(d), Some(a)) = (data_dir(), auth_file()) {
            assert_eq!(a, d.join("auth.json"));
            assert!(d.ends_with("opencode"));
        }
    }
}
