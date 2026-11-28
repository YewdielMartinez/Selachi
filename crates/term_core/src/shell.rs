//! Default shell detection and PowerShell shell integration.

use std::io;
use std::path::{Path, PathBuf};

/// The PowerShell integration script (OSC 7 cwd + OSC 133 prompt marks, chains the user's prompt).
pub const PWSH_INTEGRATION: &str = include_str!("integration.ps1");

/// Returns `true` if `exe` can be found in one of the `PATH` directories.
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| Path::new(&dir).join(exe).is_file()))
        .unwrap_or(false)
}

/// Does `program` name something that can be started: an existing file (absolute or relative path,
/// with or without its extension) or an executable found on `PATH` (`pwsh`, `wsl.exe`, ...).
pub fn program_exists(program: &str) -> bool {
    let program = program.trim().trim_matches('"');
    if program.is_empty() {
        return false;
    }
    const EXTS: [&str; 5] = ["", ".exe", ".cmd", ".bat", ".com"];
    let found_in = |base: &Path| {
        EXTS.iter().any(|ext| {
            let mut name = base.as_os_str().to_os_string();
            name.push(ext);
            Path::new(&name).is_file()
        })
    };
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return found_in(path);
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| found_in(&dir.join(program))))
        .unwrap_or(false)
}

/// Directory where Selachi keeps the integration scripts (`%LOCALAPPDATA%\Selachi\shell`).
fn integration_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Selachi")
        .join("shell")
}

/// Writes the integration script (only when its content changed) and returns its path.
///
/// The user's profile is never touched: the script is loaded by an extra `-Command` argument.
pub fn install_pwsh_integration() -> io::Result<PathBuf> {
    install_pwsh_integration_in(&integration_dir())
}

fn install_pwsh_integration_in(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("selachi-integration.ps1");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(PWSH_INTEGRATION) {
        std::fs::write(&path, PWSH_INTEGRATION)?;
    }
    Ok(path)
}

/// Arguments that start PowerShell with the user's profile and then load `script`.
///
/// The script is read and run as a scriptblock instead of being dot-sourced, so it works under
/// any execution policy. Single quotes in the path are doubled for the PowerShell literal.
pub fn pwsh_integration_args(script: &Path) -> Vec<String> {
    let literal = script.display().to_string().replace('\'', "''");
    vec![
        "-NoLogo".to_string(),
        "-NoExit".to_string(),
        "-Command".to_string(),
        format!(". ([scriptblock]::Create([IO.File]::ReadAllText('{literal}')))"),
    ]
}

/// Picks the default shell: `pwsh.exe` if on `PATH`, else `powershell.exe`.
///
/// Returns the program and its arguments: `-NoLogo`, plus the shell integration (cwd and prompt
/// marks, see [`install_pwsh_integration`]) when the script could be written.
pub fn default_shell() -> (String, Vec<String>) {
    let program = if on_path("pwsh.exe") {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    let args = match install_pwsh_integration() {
        Ok(script) => pwsh_integration_args(&script),
        Err(e) => {
            log::warn!("shell integration disabled, could not write the script: {e}");
            vec!["-NoLogo".to_string()]
        }
    };
    (program.to_string(), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_exists_finds_files_and_path_entries_only() {
        let exe = std::env::current_exe().unwrap();
        assert!(program_exists(&exe.display().to_string()));
        // without the extension, like typing `cmd`
        assert!(program_exists(
            &exe.with_extension("").display().to_string()
        ));
        assert!(!program_exists(r"C:\no\such\dir\ghost.exe"));
        assert!(!program_exists("selachi-no-such-shell-xyz"));
        assert!(!program_exists("  "));
        #[cfg(windows)]
        assert!(program_exists("cmd.exe") && program_exists("cmd"));
    }

    #[test]
    fn args_quote_the_script_path() {
        let args = pwsh_integration_args(Path::new("C:\\Users\\o'brien\\a b\\s.ps1"));
        assert_eq!(&args[..3], ["-NoLogo", "-NoExit", "-Command"]);
        assert!(
            args[3].contains("'C:\\Users\\o''brien\\a b\\s.ps1'"),
            "{}",
            args[3]
        );
        assert!(
            !args[3].contains('"'),
            "no double quotes: they would be escaped by CreateProcess"
        );
    }

    #[test]
    fn script_is_ascii_and_installs_idempotently() {
        assert!(
            PWSH_INTEGRATION.is_ascii(),
            "Windows PowerShell 5.1 reads BOM-less files as ANSI"
        );
        let dir = std::env::temp_dir().join(format!("selachi-test-{}", std::process::id()));
        let a = install_pwsh_integration_in(&dir).unwrap();
        let first = std::fs::metadata(&a).unwrap().modified().unwrap();
        let b = install_pwsh_integration_in(&dir).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            std::fs::metadata(&b).unwrap().modified().unwrap(),
            first,
            "unchanged file is not rewritten"
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), PWSH_INTEGRATION);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
