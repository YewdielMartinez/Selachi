//! Projects: directories recognised by their marker files, plus the user's
//! pinned/recent state for them.

use super::{now_ms, StateDb};
use crate::error::Result;
use crate::paths::{clean_display, normalize_key};
use rusqlite::params;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectKind {
    Git,
    Node,
    Rust,
    Dotnet,
    Python,
    Other,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Git => "git",
            ProjectKind::Node => "node",
            ProjectKind::Rust => "rust",
            ProjectKind::Dotnet => "dotnet",
            ProjectKind::Python => "python",
            ProjectKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> ProjectKind {
        match s {
            "git" => ProjectKind::Git,
            "node" => ProjectKind::Node,
            "rust" => ProjectKind::Rust,
            "dotnet" => ProjectKind::Dotnet,
            "python" => ProjectKind::Python,
            _ => ProjectKind::Other,
        }
    }
}

/// A project found on disk (not yet, or not necessarily, in the database).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredProject {
    pub path: PathBuf,
    pub name: String,
    /// The most specific marker: Rust, Node, .NET or Python win over a bare `.git`.
    pub kind: ProjectKind,
    /// The folder also contains `.git` (so the UI can show a branch).
    pub is_git: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub kind: ProjectKind,
    pub pinned: bool,
    /// Unix ms.
    pub last_opened: Option<i64>,
}

/// Directories never entered while scanning.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "bin",
    "obj",
    "dist",
    "build",
    "out",
    "venv",
    "__pycache__",
    "$recycle.bin",
    "system volume information",
    "appdata",
];

/// Classify a single directory by its marker files; `None` if it is not a project.
pub fn detect_project(dir: &Path) -> Option<DiscoveredProject> {
    let entries = std::fs::read_dir(dir).ok()?;
    let (mut git, mut node, mut rust, mut dotnet, mut python) = (false, false, false, false, false);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().to_lowercase();
        match name.as_str() {
            ".git" => git = true,
            "package.json" => node = true,
            "cargo.toml" => rust = true,
            "pyproject.toml" => python = true,
            _ if name.ends_with(".sln") => dotnet = true,
            _ => {}
        }
    }
    let kind = if rust {
        ProjectKind::Rust
    } else if node {
        ProjectKind::Node
    } else if dotnet {
        ProjectKind::Dotnet
    } else if python {
        ProjectKind::Python
    } else if git {
        ProjectKind::Git
    } else {
        return None;
    };
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    Some(DiscoveredProject {
        path: dir.to_path_buf(),
        name,
        kind,
        is_git: git,
    })
}

/// Scan `roots` down to `max_depth` levels (0 = the roots themselves) for
/// projects. A folder that is a project is reported and not searched further
/// (nested packages of a monorepo belong to their root). Symlinks are not
/// followed; hidden folders and build output (`node_modules`, `target`, `.git`,
/// ...) are skipped. Blocking filesystem work: call from a background thread.
/// Results are sorted by path.
pub fn discover_projects(roots: &[PathBuf], max_depth: usize) -> Vec<DiscoveredProject> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        scan(root, 0, max_depth, &mut seen, &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn scan(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<DiscoveredProject>,
) {
    if !seen.insert(normalize_key(&dir.to_string_lossy())) {
        return;
    }
    if let Some(p) = detect_project(dir) {
        out.push(p);
        return;
    }
    if depth >= max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            !n.starts_with('.') && !SKIP_DIRS.contains(&n.as_str())
        })
        .map(|e| e.path())
        .collect();
    subdirs.sort();
    for sub in subdirs {
        scan(&sub, depth + 1, max_depth, seen, out);
    }
}

impl StateDb {
    /// Insert or refresh discovered projects (name/kind updated; pinned and
    /// last-opened state kept). Returns how many were new.
    pub fn save_projects(&self, found: &[DiscoveredProject]) -> Result<usize> {
        let rows: Vec<(String, String, String, &'static str)> = found
            .iter()
            .map(|p| {
                let display = clean_display(&p.path.to_string_lossy());
                (
                    normalize_key(&display),
                    display,
                    p.name.clone(),
                    p.kind.as_str(),
                )
            })
            .collect();
        self.exec_sync(move |conn| {
            let mut added = 0;
            let mut stmt = conn.prepare(
                "INSERT INTO projects(key, path, name, kind) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(key) DO UPDATE SET path = excluded.path, name = excluded.name, kind = excluded.kind",
            )?;
            let mut exists = conn.prepare("SELECT 1 FROM projects WHERE key = ?1")?;
            for (key, path, name, kind) in &rows {
                let was = exists.exists(params![key])?;
                stmt.execute(params![key, path, name, kind])?;
                if !was {
                    added += 1;
                }
            }
            Ok(added)
        })
    }

    /// Projects: pinned first, then most recently opened, then by name.
    pub fn projects(&self) -> Result<Vec<Project>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, path, name, kind, pinned, last_opened FROM projects
                 ORDER BY pinned DESC, last_opened IS NULL, last_opened DESC, name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(Project {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    kind: ProjectKind::parse(&r.get::<_, String>(3)?),
                    pinned: r.get::<_, i64>(4)? != 0,
                    last_opened: r.get(5)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
    }

    pub fn set_project_pinned(&self, path: &str, pinned: bool) -> Result<()> {
        let key = normalize_key(path);
        self.exec_sync(move |conn| {
            conn.execute(
                "UPDATE projects SET pinned = ?1 WHERE key = ?2",
                params![i64::from(pinned), key],
            )?;
            Ok(())
        })
    }

    /// Stamp a project as opened now (queued). Registers the folder as a project
    /// of kind `other` if it was not known.
    pub fn mark_project_opened(&self, path: &str) -> Result<()> {
        let display = clean_display(path);
        let key = normalize_key(&display);
        let name = display
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&display)
            .to_string();
        let ts = now_ms();
        self.exec_async("mark_project_opened", move |conn| {
            conn.execute(
                "INSERT INTO projects(key, path, name, kind, last_opened) VALUES (?1, ?2, ?3, 'other', ?4)
                 ON CONFLICT(key) DO UPDATE SET last_opened = excluded.last_opened",
                params![key, display, name, ts],
            )?;
            Ok(())
        })
    }

    pub fn remove_project(&self, path: &str) -> Result<()> {
        let key = normalize_key(path);
        self.exec_sync(move |conn| {
            conn.execute("DELETE FROM projects WHERE key = ?1", params![key])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    #[test]
    fn discovery_on_temp_tree() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("a-git/.git")).unwrap();
        touch(&r.join("b-rust/Cargo.toml"));
        fs::create_dir_all(r.join("b-rust/.git")).unwrap(); // rust wins over git
        touch(&r.join("b-rust/crates/inner/Cargo.toml")); // not searched: root is a project
        touch(&r.join("c-node/package.json"));
        touch(&r.join("c-node/node_modules/dep/package.json")); // never entered
        touch(&r.join("deep/er/d-dotnet/App.sln"));
        touch(&r.join("e-python/pyproject.toml"));
        touch(&r.join("plain/readme.txt"));
        touch(&r.join("target/x/Cargo.toml")); // skipped dir
        touch(&r.join("node_modules/y/package.json")); // skipped dir
        touch(&r.join(".hidden/z/package.json")); // hidden
        touch(&r.join("l1/l2/l3/l4/too-deep/package.json")); // beyond depth

        let found = discover_projects(&[r.to_path_buf()], 3);
        let got: Vec<(&str, ProjectKind, bool)> = found
            .iter()
            .map(|p| (p.name.as_str(), p.kind, p.is_git))
            .collect();
        assert_eq!(
            got,
            [
                ("a-git", ProjectKind::Git, true),
                ("b-rust", ProjectKind::Rust, true),
                ("c-node", ProjectKind::Node, false),
                ("d-dotnet", ProjectKind::Dotnet, false),
                ("e-python", ProjectKind::Python, false),
            ]
        );

        // Depth 0 only inspects the root itself; depth 1 misses `deep/er/d-dotnet`.
        assert!(discover_projects(&[r.to_path_buf()], 0).is_empty());
        assert_eq!(discover_projects(&[r.to_path_buf()], 1).len(), 4);
        // Root that is itself a project; duplicate roots are scanned once.
        let own = discover_projects(&[r.join("c-node"), r.join("c-node")], 2);
        assert_eq!(own.len(), 1);
        // Missing root is ignored.
        assert!(discover_projects(&[r.join("nope")], 2).is_empty());
    }

    #[test]
    fn save_list_pin_open() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("alpha/Cargo.toml"));
        touch(&d.path().join("Beta/package.json"));
        let (_dd, db) = temp_db();
        let found = discover_projects(&[d.path().to_path_buf()], 2);
        assert_eq!(db.save_projects(&found).unwrap(), 2);
        assert_eq!(db.save_projects(&found).unwrap(), 0); // idempotent

        let list = db.projects().unwrap();
        assert_eq!(
            list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "Beta"]
        );
        assert_eq!(list[0].kind, ProjectKind::Rust);

        let beta = list[1].path.clone();
        db.set_project_pinned(&beta, true).unwrap();
        assert_eq!(db.projects().unwrap()[0].name, "Beta");

        db.mark_project_opened(&list[0].path).unwrap();
        db.mark_project_opened("C:\\new\\thing").unwrap();
        db.flush().unwrap();
        let list = db.projects().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "Beta"); // pinned stays first
        assert!(list[1].last_opened.is_some());
        db.remove_project("C:\\new\\thing").unwrap();
        assert_eq!(db.projects().unwrap().len(), 2);
    }
}
