//! Dropping files from Explorer onto the window (`gpui::ExternalPaths`): what each target does with
//! them. The gpui side is `ui_kit::DropZone`; this module is the pure part (quoting and routing).
//!
//! * terminal pane: the paths are typed at the prompt, shell-quoted and space-separated, **without
//!   Enter** (so the user can still edit the command);
//! * chat composer: attached to the message (and `@mentioned` when they are inside the project, see
//!   `ui_chat`);
//! * file tree / viewer: opened in the viewer;
//! * a space row of the sidebar: a new terminal tab in that folder, when the drop is a folder.

use std::path::{Path, PathBuf};

/// How a shell wants its arguments quoted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell,
    Cmd,
    Posix,
}

/// The kind of shell from its program name (`pwsh`, `powershell`, `cmd`, `bash`, `wsl`...).
pub fn shell_kind(name: &str) -> ShellKind {
    let n = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let n = n.trim_end_matches(".exe");
    match n {
        "cmd" => ShellKind::Cmd,
        "bash" | "sh" | "zsh" | "fish" | "wsl" | "nu" => ShellKind::Posix,
        _ => ShellKind::PowerShell,
    }
}

fn plain(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '_' | '-' | '.' | '\\' | '/' | ':' | '~' | '+' | ',' | '@'
        )
}

/// One path as a single word for `kind`. Paths made only of safe characters stay as they are.
pub fn quote_path(kind: ShellKind, path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if !s.is_empty() && s.chars().all(plain) {
        return s.to_string();
    }
    match kind {
        // Single quotes are literal in PowerShell ($, backticks, parentheses do nothing); a quote inside doubles.
        ShellKind::PowerShell => format!("'{}'", s.replace('\'', "''")),
        ShellKind::Cmd => format!("\"{}\"", s.replace('"', "")),
        ShellKind::Posix => format!("'{}'", s.replace('\'', r"'\''")),
    }
}

/// The text typed at the prompt for dropped `paths`: quoted, space-separated, no newline.
pub fn quote_paths(kind: ShellKind, paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| quote_path(kind, p))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A drop on a space row: the folder to open a terminal in (the first dropped folder).
pub fn folder_to_open(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_dir()).cloned()
}

/// A drop on the file tree / viewer, split into what opens now.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewerDrop {
    /// Files to open, in drop order.
    pub files: Vec<PathBuf>,
    /// Folders (shown in the tree).
    pub folders: Vec<PathBuf>,
}

pub fn split_for_viewer(paths: &[PathBuf]) -> ViewerDrop {
    let mut out = ViewerDrop::default();
    for p in paths {
        if p.is_dir() {
            out.folders.push(p.clone());
        } else if p.is_file() {
            out.files.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn safe_paths_are_left_alone() {
        assert_eq!(
            quote_path(ShellKind::PowerShell, &p(r"C:\dev\app\main.rs")),
            r"C:\dev\app\main.rs"
        );
    }

    #[test]
    fn powershell_quotes_spaces_and_metacharacters_literally() {
        assert_eq!(
            quote_path(ShellKind::PowerShell, &p(r"C:\Mis Documentos\a.txt")),
            r"'C:\Mis Documentos\a.txt'"
        );
        assert_eq!(
            quote_path(ShellKind::PowerShell, &p(r"C:\x\$env(a)&b.txt")),
            r"'C:\x\$env(a)&b.txt'"
        );
        assert_eq!(
            quote_path(ShellKind::PowerShell, &p(r"C:\it's here\f.txt")),
            r"'C:\it''s here\f.txt'"
        );
        // The verbatim prefix Windows adds to long paths is dropped.
        assert_eq!(
            quote_path(ShellKind::PowerShell, &p(r"\\?\C:\a b\c")),
            r"'C:\a b\c'"
        );
    }

    #[test]
    fn cmd_and_posix_follow_their_own_rules() {
        assert_eq!(
            quote_path(ShellKind::Cmd, &p(r"C:\a b\c.txt")),
            "\"C:\\a b\\c.txt\""
        );
        assert_eq!(
            quote_path(ShellKind::Posix, &p("/home/a b/it's")),
            r"'/home/a b/it'\''s'"
        );
        assert_eq!(shell_kind("pwsh"), ShellKind::PowerShell);
        assert_eq!(shell_kind(r"C:\Windows\System32\cmd.exe"), ShellKind::Cmd);
        assert_eq!(shell_kind("bash.exe"), ShellKind::Posix);
        assert_eq!(shell_kind("powershell"), ShellKind::PowerShell);
    }

    #[test]
    fn several_paths_are_space_separated_without_enter() {
        let t = quote_paths(
            ShellKind::PowerShell,
            &[p(r"C:\a\x.txt"), p(r"C:\b c\y.txt")],
        );
        assert_eq!(t, r"C:\a\x.txt 'C:\b c\y.txt'");
        assert!(!t.contains('\n') && !t.contains('\r'));
        assert_eq!(quote_paths(ShellKind::PowerShell, &[]), "");
    }

    #[test]
    fn routing_picks_folders_and_files() {
        let dir = std::env::temp_dir().join(format!("selachi-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let missing = dir.join("nope");
        let all = [file.clone(), dir.clone(), missing];
        assert_eq!(folder_to_open(&all), Some(dir.clone()));
        assert_eq!(
            folder_to_open(std::slice::from_ref(&file)),
            None,
            "a file does not open a terminal"
        );
        let v = split_for_viewer(&all);
        assert_eq!(v.files, vec![file]);
        assert_eq!(v.folders, vec![dir.clone()]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
