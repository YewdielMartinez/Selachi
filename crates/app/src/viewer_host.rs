//! Host side of the file viewer: the grammar plugin registry, "Abrir en editor" and the choice of
//! project root for a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use viewer_svc::grammars::{GrammarRegistry, set_plugin_registry};

/// `%APPDATA%\Selachi\plugins\grammars`.
pub fn grammars_dir() -> Option<PathBuf> {
    store::paths::app_data_dir_path().ok().map(|d| d.join("plugins").join("grammars"))
}

/// Opens the plugin registry and installs it process-wide. Blocking (it scans the folder and
/// applies deferred installs): call it off the UI thread. Returns the ids of the grammars that are
/// available, for the doctor.
pub fn init_grammars() -> Vec<String> {
    let Some(dir) = grammars_dir() else { return Vec::new() };
    let registry = Arc::new(GrammarRegistry::new(dir));
    let ids = registry.available().into_iter().map(|g| g.id).collect();
    set_plugin_registry(Some(registry));
    ids
}

/// The argv of "Abrir en editor": `code -g path:line` (VS Code's launcher is `code.cmd` on
/// Windows).
pub fn editor_argv(path: &Path, line: Option<usize>) -> Vec<String> {
    let exe = if cfg!(windows) { "code.cmd" } else { "code" };
    let target = match line {
        Some(l) => format!("{}:{l}", path.display()),
        None => path.display().to_string(),
    };
    vec![exe.to_owned(), "-g".to_owned(), target]
}

/// Spawns the editor detached and without a console window. Blocking only for the spawn itself.
pub fn spawn_editor(path: &Path, line: Option<usize>) -> Result<(), String> {
    let argv = editor_argv(path, line);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn().map(drop).map_err(|e| format!("No se pudo abrir el editor ({}): {e}", argv[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_goes_to_the_line() {
        let a = editor_argv(Path::new(r"C:\p\a.rs"), Some(12));
        assert_eq!(&a[1..], ["-g", r"C:\p\a.rs:12"]);
        let b = editor_argv(Path::new(r"C:\p\a.rs"), None);
        assert_eq!(b[2], r"C:\p\a.rs");
    }
}
