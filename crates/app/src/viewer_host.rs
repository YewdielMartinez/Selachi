//! Host side of the file viewer: the grammar plugin registry, "Abrir en editor" and the choice of
//! project root for a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use viewer_svc::grammars::{GrammarRegistry, set_plugin_registry};

/// `%APPDATA%\Selachi\plugins\grammars`.
pub fn grammars_dir() -> Option<PathBuf> {
    store::paths::app_data_dir_path()
        .ok()
        .map(|d| d.join("plugins").join("grammars"))
}

/// Opens the plugin registry and installs it process-wide. Blocking (it scans the folder and
/// applies deferred installs): call it off the UI thread. Returns the ids of the grammars that are
/// available, for the doctor.
pub fn init_grammars() -> Vec<String> {
    let Some(dir) = grammars_dir() else {
        return Vec::new();
    };
    let registry = Arc::new(GrammarRegistry::new(dir));
    let ids = registry.available().into_iter().map(|g| g.id).collect();
    set_plugin_registry(Some(registry));
    ids
}

/// The argv of "Abrir en editor". `editor` is `[git] external_editor` (`code -g`, `notepad++`,
/// `"C:\\Program Files\\X\\x.exe" -n`...): its words come first and the file (as `path:line`
/// when the line is known and the editor is VS Code) goes last. Empty = `code -g` (VS Code's
/// launcher is `code.cmd` on Windows).
pub fn editor_argv(path: &Path, line: Option<usize>, editor: &str) -> Vec<String> {
    let words = split_command(editor);
    let vscode_like = words.is_empty()
        || words[0]
            .to_lowercase()
            .trim_end_matches(".cmd")
            .ends_with("code");
    let target = match line {
        Some(l) if vscode_like => format!("{}:{l}", path.display()),
        _ => path.display().to_string(),
    };
    if words.is_empty() {
        let exe = if cfg!(windows) { "code.cmd" } else { "code" };
        return vec![exe.to_owned(), "-g".to_owned(), target];
    }
    let mut argv = words;
    if vscode_like && !argv.iter().any(|a| a == "-g" || a == "--goto") {
        argv.push("-g".to_owned());
    }
    argv.push(target);
    argv
}

/// Splits a command line into words; `"double quotes"` group.
fn split_command(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut has) = (false, false);
    for ch in text.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Spawns the editor detached and without a console window. Blocking only for the spawn itself.
pub fn spawn_editor(path: &Path, line: Option<usize>, editor: &str) -> Result<(), String> {
    let argv = editor_argv(path, line, editor);
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
    cmd.spawn()
        .map(drop)
        .map_err(|e| format!("No se pudo abrir el editor ({}): {e}", argv[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_goes_to_the_line() {
        let a = editor_argv(Path::new(r"C:\p\a.rs"), Some(12), "");
        assert_eq!(&a[1..], ["-g", r"C:\p\a.rs:12"]);
        let b = editor_argv(Path::new(r"C:\p\a.rs"), None, "");
        assert_eq!(b[2], r"C:\p\a.rs");
    }

    #[test]
    fn a_configured_editor_comes_first() {
        let a = editor_argv(Path::new(r"C:\p\a.rs"), Some(3), "code -g");
        assert_eq!(a, ["code", "-g", r"C:\p\a.rs:3"]);
        let b = editor_argv(
            Path::new(r"C:\p\a.rs"),
            Some(3),
            r#""C:\Program Files\N++\n.exe" -multiInst"#,
        );
        assert_eq!(
            b,
            [r"C:\Program Files\N++\n.exe", "-multiInst", r"C:\p\a.rs"]
        );
    }
}
