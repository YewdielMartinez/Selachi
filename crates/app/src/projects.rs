//! The projects the user adds by hand, and the one that is selected (the destination of every new
//! terminal and chat). The pure part: validating a name, canonical paths, what "add this folder"
//! means next to the projects that are already listed, creating a folder without ever overwriting
//! one, and the words the sidebar uses for the destination. The shell (`app_shell/projects.rs`)
//! owns the state and the I/O around it.
//!
//! A project the user adds is a *favorite* of the state database (`StateDb::add_favorite`): it is
//! kept even with no tab open and is never trimmed like the recent ones. The selected project is a
//! `meta` key (`store::db::SELECTED_PROJECT`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::paths::same_path;

/// Longest folder name accepted for a new project (Windows allows 255; a name that long is a typo).
pub const MAX_NAME_LEN: usize = 100;

/// A project of the sidebar's list that has no tab open (or whose tabs are closed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub path: PathBuf,
    /// The label the user gave it; empty = the folder name.
    pub label: String,
    /// Added by the user (never trimmed from the list). The others are recents.
    pub explicit: bool,
}

impl Known {
    pub fn new(path: PathBuf, label: String, explicit: bool) -> Self {
        Self {
            path,
            label,
            explicit,
        }
    }
}

/// Why a name cannot be a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    /// A character Windows does not allow in a file name.
    BadChar(char),
    /// `CON`, `NUL`, `COM1`... (with or without an extension).
    Reserved(String),
    /// Ends with a dot or a space, or is only dots.
    BadEnd,
}

impl NameError {
    /// The message under the field.
    pub fn message(&self) -> String {
        match self {
            NameError::Empty => "Escribe un nombre para el proyecto.".to_string(),
            NameError::TooLong => format!("El nombre es demasiado largo (máximo {MAX_NAME_LEN})."),
            NameError::BadChar(c) if c.is_control() => {
                "El nombre tiene caracteres de control.".to_string()
            }
            NameError::BadChar(c) => {
                format!("El nombre no puede contener «{c}». Evita \\ / : * ? \" < > |")
            }
            NameError::Reserved(n) => format!("«{n}» es un nombre reservado de Windows."),
            NameError::BadEnd => "El nombre no puede terminar en punto o espacio.".to_string(),
        }
    }
}

/// The folder name of a new project: trimmed, or the reason it is not valid.
pub fn validate_name(name: &str) -> Result<&str, NameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(NameError::TooLong);
    }
    if let Some(c) = name.chars().find(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return Err(NameError::BadChar(c));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(NameError::BadEnd);
    }
    // `CON.txt` is as reserved as `CON`: the device name is what comes before the first dot.
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| {
            upper
                .strip_prefix(p)
                .is_some_and(|n| n.len() == 1 && n.chars().all(|c| ('1'..='9').contains(&c)))
        });
    if reserved {
        return Err(NameError::Reserved(stem.to_string()));
    }
    Ok(name)
}

/// The path of `path` as the rest of the app writes it: canonical (symlinks and `..` resolved),
/// without the `\\?\` prefix `canonicalize` adds on Windows.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(path)?;
    let s = c.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\UNC\") {
        Some(rest) => PathBuf::from(format!(r"\\{rest}")),
        None => PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()),
    })
}

/// What "add this folder" does, given the projects the user already added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddCheck {
    /// A new project (this canonical path).
    Add(PathBuf),
    /// Already in the list (the listed path).
    Duplicate(PathBuf),
    /// Inside a listed project: that project already covers it.
    Inside(PathBuf),
    /// The folder does not exist.
    Missing,
    /// It exists but it is a file.
    NotFolder,
}

/// Decides what adding `path` means. Touches the disk (`canonicalize`): call it off the UI thread.
/// A folder that *contains* a listed project is accepted: the user may want the parent as a
/// project of its own.
pub fn check_add(path: &Path, listed: &[PathBuf]) -> AddCheck {
    let canon = match canonical(path) {
        Ok(c) => c,
        Err(_) => return AddCheck::Missing,
    };
    if !canon.is_dir() {
        return AddCheck::NotFolder;
    }
    // Compare with the listed ones as written and as the disk spells them (a junction or a
    // different casing of the same folder).
    for l in listed {
        let spelled = canonical(l).unwrap_or_else(|_| l.clone());
        if same_path(&canon, l) || same_path(&canon, &spelled) {
            return AddCheck::Duplicate(l.clone());
        }
    }
    for l in listed {
        let spelled = canonical(l).unwrap_or_else(|_| l.clone());
        if crate::paths::is_within(&canon, l) || crate::paths::is_within(&canon, &spelled) {
            return AddCheck::Inside(l.clone());
        }
    }
    AddCheck::Add(canon)
}

/// Why a new project could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateError {
    Name(NameError),
    /// The parent folder does not exist (it was deleted after being picked).
    ParentMissing,
    /// A folder (or file) with that name is already there: nothing was touched.
    Exists(PathBuf),
    Io(String),
}

impl CreateError {
    pub fn message(&self) -> String {
        match self {
            CreateError::Name(e) => e.message(),
            CreateError::ParentMissing => "La carpeta de destino ya no existe.".to_string(),
            CreateError::Exists(p) => format!(
                "Ya existe «{}» en esa ubicación. No se modificó nada.",
                crate::grouping::project_name(p)
            ),
            CreateError::Io(e) => format!("No se pudo crear la carpeta: {e}"),
        }
    }
}

/// Where the new project goes.
pub fn target_path(parent: &Path, name: &str) -> PathBuf {
    parent.join(name.trim())
}

/// Creates the folder `parent\name`. `create_dir` fails if the name is taken, so an existing
/// folder (or file) is never overwritten, not even by a race. Blocking: off the UI thread.
pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf, CreateError> {
    let name = validate_name(name).map_err(CreateError::Name)?;
    if !parent.is_dir() {
        return Err(CreateError::ParentMissing);
    }
    let target = target_path(parent, name);
    match std::fs::create_dir(&target) {
        Ok(()) => Ok(canonical(&target).unwrap_or(target)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(CreateError::Exists(target)),
        Err(e) => Err(CreateError::Io(e.to_string())),
    }
}

/// `git init` in `dir`, without a console window. Blocking: off the UI thread.
pub fn git_init(dir: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("init")
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("git no está instalado".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The folder the "create project" dialog starts in: next to the selected project, else the
/// user's home.
pub fn default_parent(selected: Option<&Path>) -> PathBuf {
    selected
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
        })
        .unwrap_or_default()
}

/// Where new terminals and chats go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    /// The selected project, which exists.
    Project(PathBuf),
    /// A project is selected but its folder is gone: it cannot be a destination.
    Missing(PathBuf),
    /// Nothing selected: they open in the active tab's folder, as before.
    None,
}

/// The destination for `selected`, given what is known about missing folders.
pub fn resolve(selected: Option<&Path>, missing: &HashSet<PathBuf>) -> Dest {
    match selected {
        None => Dest::None,
        Some(p) if missing.iter().any(|m| same_path(m, p)) => Dest::Missing(p.to_path_buf()),
        Some(p) => Dest::Project(p.to_path_buf()),
    }
}

impl Dest {
    /// The second line of "Nueva pestaña": where the tab goes.
    pub fn line(&self) -> String {
        match self {
            Dest::Project(p) => format!("en {}", crate::grouping::project_name(p)),
            Dest::Missing(p) => format!(
                "en la carpeta actual · {} no existe",
                crate::grouping::project_name(p)
            ),
            Dest::None => "en la carpeta actual".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated_like_windows_folders() {
        assert_eq!(validate_name("  mi-app  "), Ok("mi-app"));
        assert_eq!(validate_name("Proyecto ñandú 2"), Ok("Proyecto ñandú 2"));
        assert_eq!(validate_name(""), Err(NameError::Empty));
        assert_eq!(validate_name("   "), Err(NameError::Empty));
        assert_eq!(validate_name("a/b"), Err(NameError::BadChar('/')));
        assert_eq!(validate_name("a:b"), Err(NameError::BadChar(':')));
        assert_eq!(validate_name("what?"), Err(NameError::BadChar('?')));
        assert_eq!(validate_name("tab\there"), Err(NameError::BadChar('\t')));
        assert_eq!(validate_name("app."), Err(NameError::BadEnd));
        assert_eq!(validate_name(".."), Err(NameError::BadEnd));
        assert_eq!(validate_name("con"), Err(NameError::Reserved("con".into())));
        assert_eq!(
            validate_name("NUL.txt"),
            Err(NameError::Reserved("NUL".into()))
        );
        assert_eq!(
            validate_name("lpt3"),
            Err(NameError::Reserved("lpt3".into()))
        );
        assert_eq!(validate_name("COM10"), Ok("COM10"));
        assert_eq!(validate_name("console"), Ok("console"));
        assert_eq!(
            validate_name(&"x".repeat(MAX_NAME_LEN + 1)),
            Err(NameError::TooLong)
        );
        assert!(NameError::BadChar('*').message().contains('*'));
    }

    #[test]
    fn creating_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let made = create_folder(d.path(), " Nuevo ").unwrap();
        assert!(made.is_dir());
        assert!(made.ends_with("Nuevo"));
        // A file inside survives the second attempt.
        std::fs::write(made.join("keep.txt"), "x").unwrap();
        let again = create_folder(d.path(), "Nuevo");
        assert!(matches!(again, Err(CreateError::Exists(_))), "{again:?}");
        assert_eq!(std::fs::read_to_string(made.join("keep.txt")).unwrap(), "x");
        // Same name in another case is the same folder on Windows.
        if cfg!(windows) {
            assert!(matches!(
                create_folder(d.path(), "NUEVO"),
                Err(CreateError::Exists(_))
            ));
        }
        // A file with that name is not overwritten either.
        std::fs::write(d.path().join("a-file"), "y").unwrap();
        assert!(matches!(
            create_folder(d.path(), "a-file"),
            Err(CreateError::Exists(_))
        ));
        assert_eq!(
            create_folder(&d.path().join("missing"), "x"),
            Err(CreateError::ParentMissing)
        );
        assert_eq!(
            create_folder(d.path(), "a|b"),
            Err(CreateError::Name(NameError::BadChar('|')))
        );
        // Nothing was created by the failed attempts.
        let names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn adding_a_folder_handles_missing_files_duplicates_and_nesting() {
        let d = tempfile::tempdir().unwrap();
        let root = canonical(d.path()).unwrap();
        let a = root.join("Alpha");
        let inner = a.join("sub");
        std::fs::create_dir_all(&inner).unwrap();
        let file = root.join("f.txt");
        std::fs::write(&file, "x").unwrap();

        assert_eq!(check_add(&root.join("nope"), &[]), AddCheck::Missing);
        assert_eq!(check_add(&file, &[]), AddCheck::NotFolder);
        assert_eq!(check_add(&a, &[]), AddCheck::Add(a.clone()));
        // Duplicate: same folder, spelled with other separators and case.
        let listed = vec![a.clone()];
        let shouty = PathBuf::from(a.to_string_lossy().to_uppercase().replace('\\', "/"));
        if cfg!(windows) {
            assert_eq!(check_add(&shouty, &listed), AddCheck::Duplicate(a.clone()));
        }
        assert_eq!(check_add(&a, &listed), AddCheck::Duplicate(a.clone()));
        // `..` segments resolve to the same folder.
        assert_eq!(
            check_add(&inner.join("..").join("..").join("Alpha"), &listed),
            AddCheck::Duplicate(a.clone())
        );
        // Nested: covered by the listed parent.
        assert_eq!(check_add(&inner, &listed), AddCheck::Inside(a));
        // A parent of a listed project is its own project.
        assert_eq!(
            check_add(&root, &listed),
            AddCheck::Add(root.clone()),
            "the parent of a listed project may be a project"
        );
    }

    #[test]
    fn the_destination_says_where_things_go() {
        let none = HashSet::new();
        assert_eq!(resolve(None, &none), Dest::None);
        assert_eq!(Dest::None.line(), "en la carpeta actual");
        let tron = PathBuf::from(r"C:\dev\TRON Project");
        let d = resolve(Some(&tron), &none);
        assert_eq!(d, Dest::Project(tron.clone()));
        assert_eq!(d.line(), "en TRON Project");
        let missing = HashSet::from([PathBuf::from("c:/dev/tron project")]);
        let d = resolve(Some(&tron), &missing);
        assert!(
            matches!(d, Dest::Missing(_)),
            "a vanished folder is never a destination"
        );
        assert!(d.line().contains("no existe"));
    }

    #[test]
    fn the_create_dialog_starts_next_to_the_selected_project() {
        let p = default_parent(Some(Path::new(r"C:\dev\tron")));
        assert_eq!(p, PathBuf::from(r"C:\dev"));
        // No selection: the home folder (or nothing when the environment has none).
        let h = default_parent(None);
        assert!(h.as_os_str().is_empty() || h.is_absolute() || cfg!(not(windows)));
    }
}
