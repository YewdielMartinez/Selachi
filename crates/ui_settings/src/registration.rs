//! "Registrar en Claude Code" / "Registrar en Antigravity": adds the `selachi` MCP server to those
//! clients so they can delegate through Selachi (`selachi-agent.exe mcp`, stdio, no network).
//!
//! The exact command is shown first and **nothing runs until the user confirms** in the dialog;
//! afterwards the result (the client's own output) is reported. Tests and screenshots never run it:
//! the view takes a [`CommandRunner`], and the default one ([`SystemRunner`]) is only reached by the
//! confirm button.
//!
//! | Client | Command |
//! |---|---|
//! | Claude Code | `claude mcp add -s user selachi -- "<install dir>\selachi-agent.exe" mcp` |
//! | Antigravity | `agy mcp add selachi "<install dir>\selachi-agent.exe" mcp` |

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

/// Which client the MCP server is registered in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum McpTarget {
    ClaudeCode,
    Antigravity,
}

impl McpTarget {
    pub const ALL: [McpTarget; 2] = [McpTarget::ClaudeCode, McpTarget::Antigravity];

    /// Name in the button and the dialog.
    pub fn label(self) -> &'static str {
        match self {
            McpTarget::ClaudeCode => "Claude Code",
            McpTarget::Antigravity => "Antigravity",
        }
    }

    /// The program (`claude`, `agy`).
    pub fn program(self) -> &'static str {
        match self {
            McpTarget::ClaudeCode => "claude",
            McpTarget::Antigravity => "agy",
        }
    }

    /// Stable id fragment.
    pub fn id(self) -> &'static str {
        match self {
            McpTarget::ClaudeCode => "claude",
            McpTarget::Antigravity => "agy",
        }
    }

    /// The arguments after the program, with the real path of `selachi-agent.exe`.
    pub fn args(self, agent_exe: &Path) -> Vec<String> {
        let exe = agent_exe.display().to_string();
        let mut a: Vec<String> = match self {
            McpTarget::ClaudeCode => ["mcp", "add", "-s", "user", "selachi", "--"]
                .map(String::from)
                .into(),
            McpTarget::Antigravity => ["mcp", "add", "selachi"].map(String::from).into(),
        };
        a.push(exe);
        a.push("mcp".to_string());
        a
    }

    /// The command as the user would type it (what the dialog shows): arguments with spaces are
    /// double-quoted.
    pub fn display(self, agent_exe: &Path) -> String {
        let mut s = self.program().to_string();
        let exe = agent_exe.display().to_string();
        for a in self.args(agent_exe) {
            s.push(' ');
            // The path is always quoted (the documented form); other arguments only when needed.
            if a == exe || a.contains(' ') {
                s.push('"');
                s.push_str(&a);
                s.push('"');
            } else {
                s.push_str(&a);
            }
        }
        s
    }
}

/// Runs the registration. A trait so tests never start a process.
pub trait CommandRunner: Send + Sync + 'static {
    /// `Ok(output)` when the client accepted it, `Err(reason)` otherwise. Blocking: the view calls
    /// it from the background executor.
    fn run(&self, target: McpTarget, agent_exe: &Path) -> Result<String, String>;
}

/// The real runner: finds the client (PATH, then the folders its installer uses) and runs it with a
/// 30 s limit.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the client's executable is, if it can be found.
pub fn find_program(target: McpTarget) -> Option<PathBuf> {
    let want = target.program();
    // `agents_driver::detect` knows the PATH and the installers' default folders.
    agents_driver::detect().into_iter().find_map(|d| {
        let stem = d.path.file_stem()?.to_string_lossy().to_ascii_lowercase();
        (stem == want).then_some(d.path)
    })
}

impl CommandRunner for SystemRunner {
    fn run(&self, target: McpTarget, agent_exe: &Path) -> Result<String, String> {
        if !agent_exe.is_file() {
            return Err(format!(
                "No se encontró {}. Reinstala Selachi.",
                agent_exe.display()
            ));
        }
        let program = find_program(target).ok_or_else(|| {
            format!(
                "No se encontró «{}» en el PATH. Instálalo o ejecuta el comando a mano.",
                target.program()
            )
        })?;
        let child = Command::new(&program)
            .args(target.args(agent_exe))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("No se pudo ejecutar {}: {e}", program.display()))?;
        // Wait on a helper thread so the timeout can give up (and kill) a client that hangs.
        let pid = child.id();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        match rx.recv_timeout(RUN_TIMEOUT) {
            Ok(Ok(out)) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let text = text.trim().chars().take(600).collect::<String>();
                if out.status.success() {
                    Ok(if text.is_empty() {
                        "Registrado.".to_string()
                    } else {
                        text
                    })
                } else {
                    Err(if text.is_empty() {
                        format!("{} terminó con {}", target.program(), out.status)
                    } else {
                        text
                    })
                }
            }
            Ok(Err(e)) => Err(format!("Falló la ejecución: {e}")),
            Err(_) => {
                kill(pid);
                Err(format!(
                    "{} no respondió en {} s",
                    target.program(),
                    RUN_TIMEOUT.as_secs()
                ))
            }
        }
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill(_pid: u32) {}

/// Where the dialog is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegPhase {
    /// Showing the command, waiting for the user.
    Confirm,
    Running,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_are_the_documented_ones() {
        let exe = Path::new(r"C:\Users\me\AppData\Local\Programs\Selachi\selachi-agent.exe");
        assert_eq!(
            McpTarget::ClaudeCode.display(exe),
            r#"claude mcp add -s user selachi -- "C:\Users\me\AppData\Local\Programs\Selachi\selachi-agent.exe" mcp"#
        );
        assert_eq!(
            McpTarget::Antigravity.display(exe),
            r#"agy mcp add selachi "C:\Users\me\AppData\Local\Programs\Selachi\selachi-agent.exe" mcp"#
        );
        // the path is quoted even without spaces
        assert_eq!(
            McpTarget::Antigravity.display(Path::new(r"C:\s\selachi-agent.exe")),
            r#"agy mcp add selachi "C:\s\selachi-agent.exe" mcp"#
        );
    }

    #[test]
    fn the_arguments_keep_the_path_as_one_argument() {
        let exe = Path::new(r"C:\Program Files\Selachi\selachi-agent.exe");
        let a = McpTarget::ClaudeCode.args(exe);
        assert_eq!(a.last().map(String::as_str), Some("mcp"));
        assert!(a.contains(&exe.display().to_string()));
        assert_eq!(a[..6], ["mcp", "add", "-s", "user", "selachi", "--"]);
    }

    #[test]
    fn a_missing_agent_exe_is_reported_without_running_anything() {
        let r = SystemRunner.run(
            McpTarget::ClaudeCode,
            Path::new(r"C:\no\such\selachi-agent.exe"),
        );
        assert!(r.unwrap_err().contains("Reinstala"));
    }
}
