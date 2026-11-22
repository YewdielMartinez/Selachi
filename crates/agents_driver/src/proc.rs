//! Process helpers: PATH lookup (incl. Windows `.cmd`/`.exe` shims), hidden-window
//! spawning, process-tree kill and "run with timeout".

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `CREATE_NO_WINDOW` process creation flag (Windows).
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Never pop a console window for a child process (no-op off Windows).
pub fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(windows)]
const EXTS: &[&str] = &[".exe", ".cmd", ".bat", ".com"];
#[cfg(not(windows))]
const EXTS: &[&str] = &[""];

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Find `name` in `dirs` (first hit wins; on Windows `.exe` beats `.cmd`).
/// Extension-less files are ignored on Windows: npm drops POSIX `sh` shims
/// next to the `.cmd` ones and those cannot be spawned.
pub fn find_in_dirs(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let has_ext = Path::new(name).extension().is_some();
    for dir in dirs {
        if cfg!(windows) && has_ext {
            let p = dir.join(name);
            if is_executable(&p) {
                return Some(p);
            }
            continue;
        }
        for ext in EXTS {
            let p = dir.join(format!("{name}{ext}"));
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_dirs(name, std::env::split_paths(&path))
}

/// `name_or_path` may be a bare command (PATH lookup) or a path.
pub fn resolve_executable(name_or_path: &str) -> Option<PathBuf> {
    let p = Path::new(name_or_path);
    if p.components().count() > 1 {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        let parent = p.parent()?.to_path_buf();
        let stem = p.file_name()?.to_str()?;
        return find_in_dirs(stem, [parent]);
    }
    find_on_path(name_or_path)
}

fn is_batch(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("cmd") | Some("bat")
    )
}

/// npm-style `.cmd` shims end with `"%dp0%\...\tool.exe" %*`. Extract that `.exe`
/// so we can spawn the real binary directly (no `cmd.exe` in the middle: faster,
/// and `taskkill /T` / stdin pipes behave).
pub fn parse_shim_target(shim_text: &str, shim_dir: &Path) -> Option<PathBuf> {
    let lower = shim_text.to_ascii_lowercase();
    let mut search_end = lower.len();
    while let Some(pos) = lower[..search_end].rfind(".exe\"") {
        let end = pos + 4;
        if let Some(start) = shim_text[..pos].rfind('"') {
            let token = &shim_text[start + 1..end];
            let dir = shim_dir.to_string_lossy();
            let dir = dir.trim_end_matches(['\\', '/']);
            let replaced = token
                .replace("%dp0%", &format!("{dir}\\"))
                .replace("%~dp0", &format!("{dir}\\"));
            let candidate = PathBuf::from(replaced.replace("\\\\", "\\"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        search_end = pos;
    }
    None
}

/// Build a hidden-window `Command` for `exe`. For `.cmd`/`.bat` shims it first
/// tries the real `.exe` they point at; otherwise Rust's std runs the batch file
/// through `cmd.exe` with correct escaping.
pub fn command_for(exe: &Path) -> Command {
    let mut target = exe.to_path_buf();
    if is_batch(exe) {
        if let (Ok(text), Some(dir)) = (std::fs::read_to_string(exe), exe.parent()) {
            if let Some(real) = parse_shim_target(&text, dir) {
                target = real;
            }
        }
    }
    let mut cmd = Command::new(target);
    hide_window(&mut cmd);
    cmd
}

/// Spawn `cmd`. With `job` (raw Job Object `HANDLE`, Windows only) the child is created
/// suspended, assigned to the job and resumed, so it and all its descendants are members (see
/// the `job` module source for the rationale). Assignment failures are logged, not returned.
/// Without a job (or off Windows) this is plain `cmd.spawn()`.
pub fn spawn_in_job(cmd: &mut Command, job: Option<usize>) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    if let Some(job) = job {
        return crate::job::spawn_in_job(cmd, job);
    }
    let _ = job;
    cmd.spawn()
}

/// Kill a process and its children. Windows: `taskkill /T /F`.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut c);
        let _ = c.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Run `cmd`, capture stdout, give up (and kill) after `timeout`.
pub fn run_capture(mut cmd: Command, timeout: Duration) -> Option<String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let deadline = Instant::now() + timeout;
    loop {
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(s) => {
                let _ = child.wait();
                return Some(s);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.wait();
                return None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    kill_tree(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
}

/// Char-boundary-safe truncation with an ellipsis.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
        assert_eq!(truncate("ab", 5), "ab");
    }

    #[test]
    fn shim_target_resolves_npm_cmd() {
        let dir = std::env::temp_dir().join("agents_driver_shim_test");
        let exe_dir = dir.join("node_modules").join("pkg").join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("tool.exe");
        std::fs::write(&exe, b"").unwrap();
        let shim = "@ECHO off\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\pkg\\bin\\tool.exe\"   %*\r\n";
        if cfg!(windows) {
            let got = parse_shim_target(shim, &dir).expect("target");
            assert!(got.ends_with("tool.exe"));
            assert!(got.is_file());
        }
        assert_eq!(
            parse_shim_target("@ECHO off\r\nnode \"%dp0%\\x.js\" %*", &dir),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_in_dirs_prefers_exe_over_cmd() {
        let dir = std::env::temp_dir().join("agents_driver_find_test");
        std::fs::create_dir_all(&dir).unwrap();
        if cfg!(windows) {
            std::fs::write(dir.join("zzagent.cmd"), b"").unwrap();
            // npm's POSIX sh shim: must be ignored on Windows.
            std::fs::write(dir.join("zzagent"), b"").unwrap();
            assert!(find_in_dirs("zzagent", [dir.clone()])
                .unwrap()
                .ends_with("zzagent.cmd"));
            std::fs::write(dir.join("zzagent.exe"), b"").unwrap();
            assert!(find_in_dirs("zzagent", [dir.clone()])
                .unwrap()
                .ends_with("zzagent.exe"));
        }
        assert!(find_in_dirs("definitely-not-here", [dir.clone()]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
