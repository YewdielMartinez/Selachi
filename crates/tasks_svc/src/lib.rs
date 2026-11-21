//! Project task launcher backend (A6), UI-agnostic.
//!
//! [`discover_tasks`] reads a project folder and returns the runnable tasks it
//! declares: package.json scripts (npm/pnpm/yarn/bun, with workspaces), .NET
//! solutions and projects, Cargo, Makefile, justfile, pyproject, Docker Compose
//! and `.vscode/tasks.json`. It only reads files (the single exception is an
//! optional `just --list`), so a scan takes a few milliseconds. [`TaskCache`]
//! remembers results keyed by the mtimes of every file consulted, and
//! [`rank_tasks`] orders them by what the user actually ran in the history.
//!
//! Blocking filesystem work: call it from a background thread.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cache;
mod cargo;
mod dotnet;
mod make;
mod misc;
mod model;
mod node;
mod rank;
mod scan;

pub use cache::TaskCache;
pub use model::{DiscoverOptions, TaskDef, TaskKind};
pub use rank::{rank_tasks, RankedTask};

use std::path::{Path, PathBuf};

/// Tasks of the project at `root`, in a stable order (package scripts, .NET,
/// Cargo, Make, Just, Python, Docker, VS Code).
pub fn discover_tasks(root: &Path) -> Vec<TaskDef> {
    discover_tasks_with(root, &DiscoverOptions::default())
}

/// [`discover_tasks`] with options.
pub fn discover_tasks_with(root: &Path, opts: &DiscoverOptions) -> Vec<TaskDef> {
    scan_project(root, opts).0
}

/// Tasks plus every path consulted (what the cache stats).
pub(crate) fn scan_project(root: &Path, opts: &DiscoverOptions) -> (Vec<TaskDef>, Vec<PathBuf>) {
    let mut s = scan::Scan::new(root, opts);
    node::detect(&mut s);
    dotnet::detect(&mut s);
    cargo::detect(&mut s);
    make::detect(&mut s);
    misc::detect_python(&mut s);
    misc::detect_docker(&mut s);
    misc::detect_vscode(&mut s);
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn opts() -> DiscoverOptions {
        DiscoverOptions {
            use_just_cli: false,
        }
    }

    #[test]
    fn polyglot_project_gets_every_kind() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"dev":"vite"}}"#);
        write(
            d.path(),
            "App.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk.Web"/>"#,
        );
        write(d.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        write(d.path(), "Makefile", "all:\n\techo\n");
        write(d.path(), "justfile", "dev:\n    echo\n");
        write(d.path(), "pytest.ini", "[pytest]\n");
        write(d.path(), "compose.yaml", "services:\n  a:\n    image: x\n");
        write(
            d.path(),
            ".vscode/tasks.json",
            r#"{"tasks":[{"label":"x","type":"shell","command":"echo hi"}]}"#,
        );
        let tasks = discover_tasks_with(d.path(), &opts());
        for kind in [
            TaskKind::Npm,
            TaskKind::Dotnet,
            TaskKind::Cargo,
            TaskKind::Make,
            TaskKind::Just,
            TaskKind::Python,
            TaskKind::Docker,
            TaskKind::Custom,
        ] {
            assert!(tasks.iter().any(|t| t.kind == kind), "missing {kind:?}");
        }
        // ids are unique and stable
        let mut ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n);
        let again = discover_tasks_with(d.path(), &opts());
        assert_eq!(tasks, again);
    }

    #[test]
    fn empty_and_missing_folders() {
        let d = tempfile::tempdir().unwrap();
        assert!(discover_tasks_with(d.path(), &opts()).is_empty());
        assert!(discover_tasks_with(&d.path().join("nope"), &opts()).is_empty());
    }

    #[test]
    fn source_file_and_cwd_are_absolute_inside_root() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"b"}}"#);
        let t = &discover_tasks_with(d.path(), &opts())[0];
        assert_eq!(t.source_file, d.path().join("package.json"));
        assert_eq!(t.cwd, d.path());
        assert_eq!(t.id, "npm:package.json:a");
    }

    #[test]
    fn synthetic_repo_scans_fast() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "package.json",
            r#"{"workspaces":["packages/*"],"scripts":{"dev":"x","build":"y","test":"z"}}"#,
        );
        for i in 0..20 {
            write(
                d.path(),
                &format!("packages/p{i}/package.json"),
                r#"{"name":"p","scripts":{"dev":"a","build":"b","test":"c"}}"#,
            );
        }
        write(d.path(), "Shop.sln", "");
        for i in 0..8 {
            write(
                d.path(),
                &format!("src/P{i}/P{i}.csproj"),
                r#"<Project Sdk="Microsoft.NET.Sdk.Web"/>"#,
            );
        }
        let mk: String = (0..50).map(|i| format!("t{i}:\n\techo\n")).collect();
        write(d.path(), "Makefile", &mk);
        write(d.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        let _ = discover_tasks_with(d.path(), &opts()); // warm the OS cache
        let t = std::time::Instant::now();
        let tasks = discover_tasks_with(d.path(), &opts());
        let elapsed = t.elapsed();
        assert!(tasks.len() > 100);
        // Target is <30 ms; allow generous slack for debug builds on busy CI.
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "took {elapsed:?}"
        );
    }
}
