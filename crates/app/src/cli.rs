//! The command line: `selachi [--cwd <dir> | <dir>] [--version]`, and `selachi agent ...` / `selachi mcp`
//! (forwarded to `selachi-agent.exe`, see [`forwarded_args`]).
//!
//! * `--cwd <dir>` (or `--cwd=<dir>`, or a bare `<dir>`): the folder the first tab opens in. The
//!   Explorer entry "Abrir en Selachi" runs `selachi.exe --cwd "%V"`. A file is taken as its
//!   folder; a path that does not exist is ignored (the app still starts).
//! * `--version` / `-V`: print the version and exit, without a window.
//!
//! The executable is a Windows GUI program (no console), so [`print_version`] writes to the
//! console of the process that launched it (or to the redirected stdout), and to nobody when it
//! has none.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What the command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// Folder of the first tab.
    pub cwd: Option<PathBuf>,
    /// Print the version and exit.
    pub version: bool,
}

/// The folder a launch argument names: the directory itself, the parent of a file, `None` when it
/// does not exist. Relative paths are resolved against `base`.
fn folder_of(arg: &Path, base: &Path) -> Option<PathBuf> {
    let p = if arg.is_absolute() {
        arg.to_path_buf()
    } else {
        base.join(arg)
    };
    if p.is_dir() {
        Some(p)
    } else if p.is_file() {
        p.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// Parses `args` (without the program name). Unknown flags are ignored: a newer launcher must not
/// stop an older build from starting.
pub fn parse(args: impl IntoIterator<Item = OsString>, base: &Path) -> Args {
    let mut out = Args::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let text = a.to_string_lossy();
        match text.as_ref() {
            "--version" | "-V" => out.version = true,
            "--cwd" => {
                if let Some(v) = it.next() {
                    out.cwd = folder_of(Path::new(&v), base).or(out.cwd);
                }
            }
            other if other.starts_with("--cwd=") => {
                let v = &other["--cwd=".len()..];
                out.cwd = folder_of(Path::new(v), base).or(out.cwd);
            }
            other if other.starts_with('-') => {}
            _ => out.cwd = folder_of(Path::new(&a), base).or(out.cwd),
        }
    }
    out
}

/// The text `--version` prints.
pub fn version_line() -> String {
    format!("selachi {}", env!("CARGO_PKG_VERSION"))
}

/// Prints [`version_line`] where the user can see it (see the module docs).
#[cfg(windows)]
pub fn print_version() {
    use std::io::Write as _;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    let line = version_line();
    // SAFETY: plain Win32 calls with no pointers.
    let std_out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if !std_out.is_null() && std_out != INVALID_HANDLE_VALUE {
        // Redirected (`selachi --version > file`, a pipe): the inherited handle works.
        let _ = writeln!(std::io::stdout(), "{line}");
        return;
    }
    // SAFETY: as above.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        // No parent console: nobody to tell.
        return;
    }
    if let Ok(mut con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let _ = writeln!(con, "{line}");
    }
}

#[cfg(not(windows))]
pub fn print_version() {
    println!("{}", version_line());
}

// ------------------------------------------------------------------------------------
// `selachi.exe agent ...` / `selachi.exe mcp` -> `selachi-agent.exe`
// ------------------------------------------------------------------------------------

/// File name of the console binary of the harness, next to `selachi.exe`.
pub const AGENT_EXE: &str = "selachi-agent.exe";

/// The arguments for `selachi-agent` when `argv` (without the program name) is a harness
/// invocation: `agent <args...>` forwards `<args...>` and `mcp [args...]` forwards `mcp [args...]`.
/// `None` for everything else (the GUI starts).
pub fn forwarded_args(argv: &[OsString]) -> Option<Vec<OsString>> {
    let first = argv.first()?.to_string_lossy().to_ascii_lowercase();
    match first.as_str() {
        "agent" => Some(argv[1..].to_vec()),
        "mcp" => Some(argv.to_vec()),
        _ => None,
    }
}

/// `selachi-agent.exe` in the folder of the running executable.
pub fn agent_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("the executable has no folder"))?;
    Ok(dir.join(AGENT_EXE))
}

/// Gives this GUI-subsystem process the console of the program that launched it, for the three
/// standard handles that are not already redirected. Returns whether a console was found.
#[cfg(windows)]
fn attach_parent_console() -> bool {
    use std::os::windows::io::IntoRawHandle as _;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    let missing = |which| {
        // SAFETY: plain Win32 call with no pointers.
        let h = unsafe { GetStdHandle(which) };
        h.is_null() || h == INVALID_HANDLE_VALUE
    };
    let need = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(missing);
    if !need.iter().any(|m| *m) {
        return true; // all redirected (pipes, files): they are inherited as they are
    }
    // SAFETY: as above.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return false;
    }
    let open = |name: &str, write: bool| {
        std::fs::OpenOptions::new()
            .read(!write)
            .write(write)
            .open(name)
            .ok()
            .map(|f| f.into_raw_handle())
    };
    if need[0]
        && let Some(h) = open("CONIN$", false)
    {
        // SAFETY: `h` is a valid console handle we own and never close.
        unsafe { SetStdHandle(STD_INPUT_HANDLE, h) };
    }
    for (i, which) in [(1, STD_OUTPUT_HANDLE), (2, STD_ERROR_HANDLE)] {
        if need[i]
            && let Some(h) = open("CONOUT$", true)
        {
            // SAFETY: as above.
            unsafe { SetStdHandle(which, h) };
        }
    }
    true
}

/// Runs `selachi-agent.exe` with `args`, standard handles inherited, and returns its exit code
/// (the GUI never opens). Used by `selachi.exe agent ...` and `selachi.exe mcp`.
pub fn run_forwarded(args: &[OsString]) -> i32 {
    #[cfg(windows)]
    let console = attach_parent_console();
    #[cfg(not(windows))]
    let console = true;
    let report = |msg: String| {
        #[cfg(windows)]
        if console && let Ok(mut con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            use std::io::Write as _;
            let _ = writeln!(con, "{msg}");
            return;
        }
        eprintln!("{msg}");
    };
    let exe = match agent_exe() {
        Ok(p) if p.is_file() => p,
        Ok(p) => {
            report(format!(
                "selachi: no se encontró {} (se busca junto a selachi.exe). Reinstala Selachi.",
                p.display()
            ));
            return 127;
        }
        Err(e) => {
            report(format!("selachi: no se pudo localizar el ejecutable: {e}"));
            return 127;
        }
    };
    let status = std::process::Command::new(&exe)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            report(format!(
                "selachi: no se pudo ejecutar {}: {e}",
                exe.display()
            ));
            126
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("selachi-cli-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn agent_and_mcp_are_forwarded_to_the_console_binary() {
        assert_eq!(forwarded_args(&args(&[])), None);
        assert_eq!(forwarded_args(&args(&["C:/proj"])), None);
        assert_eq!(
            forwarded_args(&args(&["agent", "list", "--json"])),
            Some(args(&["list", "--json"]))
        );
        assert_eq!(forwarded_args(&args(&["AGENT"])), Some(args(&[])));
        assert_eq!(
            forwarded_args(&args(&["mcp"])),
            Some(args(&["mcp"])),
            "`selachi mcp` is `selachi-agent mcp`"
        );
        assert!(agent_exe().unwrap().ends_with(AGENT_EXE));
    }

    #[test]
    fn no_arguments_asks_for_nothing() {
        assert_eq!(parse(args(&[]), Path::new(".")), Args::default());
    }

    #[test]
    fn version_flags() {
        assert!(parse(args(&["--version"]), Path::new(".")).version);
        assert!(parse(args(&["-V"]), Path::new(".")).version);
    }

    #[test]
    fn cwd_forms_name_the_same_folder() {
        let dir = temp_dir("forms");
        let d = dir.to_string_lossy().into_owned();
        for list in [
            vec!["--cwd".to_string(), d.clone()],
            vec![format!("--cwd={d}")],
            vec![d],
        ] {
            let list: Vec<&str> = list.iter().map(String::as_str).collect();
            assert_eq!(
                parse(args(&list), Path::new(".")).cwd.as_deref(),
                Some(dir.as_path())
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_is_taken_as_its_folder_and_a_missing_path_is_ignored() {
        let dir = temp_dir("file");
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let got = parse(args(&["--cwd", &file.to_string_lossy()]), Path::new("."));
        assert_eq!(got.cwd.as_deref(), Some(dir.as_path()));
        let missing = dir.join("nope");
        assert_eq!(
            parse(args(&["--cwd", &missing.to_string_lossy()]), Path::new(".")).cwd,
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn relative_paths_resolve_against_the_base_and_unknown_flags_are_ignored() {
        let dir = temp_dir("rel");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let got = parse(args(&["--wat", "--cwd", "sub"]), &dir);
        assert_eq!(got.cwd, Some(dir.join("sub")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
