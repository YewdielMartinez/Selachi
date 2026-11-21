//! Public data types.

use std::path::PathBuf;

/// What produced a task (drives the icon and how it is grouped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Dotnet,
    Cargo,
    Make,
    Just,
    Python,
    Docker,
    /// `.vscode/tasks.json`.
    Custom,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Npm => "npm",
            TaskKind::Pnpm => "pnpm",
            TaskKind::Yarn => "yarn",
            TaskKind::Bun => "bun",
            TaskKind::Dotnet => "dotnet",
            TaskKind::Cargo => "cargo",
            TaskKind::Make => "make",
            TaskKind::Just => "just",
            TaskKind::Python => "python",
            TaskKind::Docker => "docker",
            TaskKind::Custom => "custom",
        }
    }
}

/// One runnable task of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDef {
    /// Stable across scans (`kind:source:label`); safe as a key for pins or stats.
    pub id: String,
    /// Short name for lists (`dev`, `build`, `web: dev`, `run: Api`).
    pub label: String,
    pub kind: TaskKind,
    /// The exact line to type in a PowerShell terminal.
    pub command: String,
    /// Directory to run it in.
    pub cwd: PathBuf,
    /// The file the task was read from.
    pub source_file: PathBuf,
    /// What it does (script body, doc comment, `detail`), when known.
    pub description: Option<String>,
}

/// Knobs for [`crate::discover_tasks_with`].
#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    /// Ask `just --list --unsorted` (when `just` is on `PATH`) for the recipes of
    /// a `justfile`; otherwise, or when it fails, the file is parsed directly.
    /// This is the only process discovery can spawn.
    pub use_just_cli: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self { use_just_cli: true }
    }
}
