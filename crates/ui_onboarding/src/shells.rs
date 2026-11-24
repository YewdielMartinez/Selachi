//! Step 5: the shells installed on this machine.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// What kind of shell a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Pwsh,
    WindowsPowerShell,
    Cmd,
    GitBash,
    Wsl,
}

/// One detected shell, ready to become a `[[profiles]]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellInfo {
    pub kind: ShellKind,
    /// Profile name written to `config.toml` (`terminal.default_profile`).
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    /// Muted second line (path, distro).
    pub detail: String,
}

/// Index of the shell Selachi picks on its own (`default_profile = ""`): pwsh > powershell > cmd.
pub fn auto_index(shells: &[ShellInfo]) -> Option<usize> {
    [
        ShellKind::Pwsh,
        ShellKind::WindowsPowerShell,
        ShellKind::Cmd,
    ]
    .into_iter()
    .find_map(|k| shells.iter().position(|s| s.kind == k))
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// First `name` (with `.exe`) found in `path_var` (a `;`-separated PATH).
pub fn find_in_path(name: &str, path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    let file = if name.to_lowercase().ends_with(".exe") {
        name.to_string()
    } else {
        format!("{name}.exe")
    };
    std::env::split_paths(path_var)
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
}

/// `bash.exe` of Git for Windows: next to `git.exe` (`<root>\cmd\git.exe` -> `<root>\bin\bash.exe`)
/// or in the usual install folders.
pub fn find_git_bash(path_var: &std::ffi::OsStr, program_files: &[PathBuf]) -> Option<PathBuf> {
    let from_git = find_in_path("git", path_var).and_then(|git| {
        git.parent()
            .and_then(Path::parent)
            .map(|root| root.join("bin").join("bash.exe"))
    });
    from_git
        .into_iter()
        .chain(
            program_files
                .iter()
                .map(|pf| pf.join("Git").join("bin").join("bash.exe")),
        )
        .find(|p| p.is_file())
}

/// Decode `wsl -l -q` output: UTF-16LE (with or without BOM) on Windows, plain UTF-8 elsewhere.
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2
        && (bytes[..2] == [0xFF, 0xFE] || bytes.iter().skip(1).step_by(2).take(8).all(|b| *b == 0));
    if looks_utf16 {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Distro names from `wsl -l -q` output, without Docker Desktop's internal distros.
pub fn parse_wsl_list(bytes: &[u8]) -> Vec<String> {
    decode_wsl_output(bytes)
        .lines()
        .map(|l| l.trim().trim_matches('\0').to_string())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.to_lowercase().starts_with("docker-desktop"))
        .collect()
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn no_window(cmd: &mut Command) -> &mut Command {
    cmd
}

/// Run `wsl.exe -l -q` (hidden window, `timeout` seconds) and list the distros.
fn wsl_distros(timeout: Duration) -> Vec<String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let Ok(child) = cmd.spawn() else {
        return Vec::new();
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) if out.status.success() => parse_wsl_list(&out.stdout),
        Ok(_) => Vec::new(),
        Err(_) => {
            // Hung (WSL is waking up): do not wait for it.
            let mut kill = Command::new("taskkill");
            kill.args(["/F", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            no_window(&mut kill);
            let _ = kill.status();
            Vec::new()
        }
    }
}

/// Detect the installed shells, best default first. Blocking (it starts `wsl.exe`): call it from
/// the background executor.
pub fn detect_shells() -> Vec<ShellInfo> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut out = Vec::new();
    let mut push = |kind, name: &str, program: &str, args: &[&str], detail: String| {
        out.push(ShellInfo {
            kind,
            name: name.to_string(),
            program: program.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
            detail,
        });
    };
    if let Some(p) = find_in_path("pwsh", &path_var) {
        push(
            ShellKind::Pwsh,
            "PowerShell 7",
            "pwsh.exe",
            &["-NoLogo"],
            p.display().to_string(),
        );
    }
    let system_root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
    let ps = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if ps.is_file() {
        push(
            ShellKind::WindowsPowerShell,
            "Windows PowerShell",
            "powershell.exe",
            &["-NoLogo"],
            ps.display().to_string(),
        );
    }
    let cmd = system_root.join("System32").join("cmd.exe");
    if cmd.is_file() {
        push(
            ShellKind::Cmd,
            "Símbolo del sistema",
            "cmd.exe",
            &[],
            cmd.display().to_string(),
        );
    }
    let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|p| {
            let p = PathBuf::from(p);
            if p.ends_with("Local") {
                p.join("Programs")
            } else {
                p
            }
        })
        .collect();
    if let Some(bash) = find_git_bash(&path_var, &program_files) {
        let program = bash.display().to_string();
        push(
            ShellKind::GitBash,
            "Git Bash",
            &program,
            &["--login", "-i"],
            program.clone(),
        );
    }
    for distro in wsl_distros(Duration::from_secs(4)) {
        let label = format!("WSL: {distro}");
        push(
            ShellKind::Wsl,
            &label,
            "wsl.exe",
            &["-d", &distro],
            format!("Distribución {distro}"),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str, bom: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if bom {
            v.extend([0xFF, 0xFE]);
        }
        for u in s.encode_utf16() {
            v.extend(u.to_le_bytes());
        }
        v
    }

    #[test]
    fn parses_utf16_wsl_output() {
        let raw = utf16(
            "Ubuntu\r\ndocker-desktop\r\ndocker-desktop-data\r\nDebian\r\n",
            false,
        );
        assert_eq!(parse_wsl_list(&raw), ["Ubuntu", "Debian"]);
        let with_bom = utf16("Ubuntu-24.04\r\n", true);
        assert_eq!(parse_wsl_list(&with_bom), ["Ubuntu-24.04"]);
    }

    #[test]
    fn parses_plain_output_and_empty_input() {
        assert_eq!(parse_wsl_list(b"Alpine\nArch\n"), ["Alpine", "Arch"]);
        assert!(parse_wsl_list(b"").is_empty());
        assert!(parse_wsl_list(&utf16("\r\n", false)).is_empty());
    }

    #[test]
    fn finds_programs_on_a_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pwsh.exe"), "").unwrap();
        let path =
            std::env::join_paths([PathBuf::from("Z:\\nowhere"), dir.path().to_path_buf()]).unwrap();
        assert_eq!(
            find_in_path("pwsh", &path),
            Some(dir.path().join("pwsh.exe"))
        );
        assert_eq!(find_in_path("cmdx", &path), None);
    }

    #[test]
    fn git_bash_is_found_next_to_git() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("cmd")).unwrap();
        std::fs::create_dir_all(root.path().join("bin")).unwrap();
        std::fs::write(root.path().join("cmd").join("git.exe"), "").unwrap();
        std::fs::write(root.path().join("bin").join("bash.exe"), "").unwrap();
        let path = std::env::join_paths([root.path().join("cmd")]).unwrap();
        assert_eq!(
            find_git_bash(&path, &[]),
            Some(root.path().join("bin").join("bash.exe"))
        );
        assert_eq!(find_git_bash(std::ffi::OsStr::new(""), &[]), None);
    }

    #[test]
    fn auto_shell_follows_the_priority() {
        let s = |kind| ShellInfo {
            kind,
            name: String::new(),
            program: String::new(),
            args: vec![],
            detail: String::new(),
        };
        let all = [
            s(ShellKind::Cmd),
            s(ShellKind::Wsl),
            s(ShellKind::WindowsPowerShell),
            s(ShellKind::Pwsh),
        ];
        assert_eq!(auto_index(&all), Some(3));
        assert_eq!(auto_index(&all[..3]), Some(2));
        assert_eq!(auto_index(&all[..2]), Some(0));
        assert_eq!(auto_index(&all[1..2]), None);
        assert_eq!(auto_index(&[]), None);
    }
}
