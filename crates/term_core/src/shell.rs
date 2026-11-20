//! Default shell detection.

use std::path::Path;

/// Returns `true` if `exe` can be found in one of the `PATH` directories.
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| Path::new(&dir).join(exe).is_file()))
        .unwrap_or(false)
}

/// Picks the default shell: `pwsh.exe` if on `PATH`, else `powershell.exe`.
///
/// Returns the program and its arguments (`-NoLogo`).
pub fn default_shell() -> (String, Vec<String>) {
    let program = if on_path("pwsh.exe") { "pwsh.exe" } else { "powershell.exe" };
    (program.to_string(), vec!["-NoLogo".to_string()])
}
