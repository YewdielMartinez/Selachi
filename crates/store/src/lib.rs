//! Selachi persistence, UI-agnostic (no gpui).
//!
//! * [`config`]: `%APPDATA%\Selachi\config.toml`, typed, comment-preserving save, hot reload.
//! * [`db`]: `%APPDATA%\Selachi\state.db`: command history (FTS5), path frecency,
//!   projects, favorites, session layout, retention, `meta` key/value (first-run flag).
//! * [`paths`]: well-known locations.
//! * [`snippets`]: `{param}` command templates (storage lives in [`db`]).
//! * [`redact`]: secret redaction for text leaving the machine.
//!
//! `state.db` is deliberately a different file from `agents_svc`'s `selachi.db`:
//! each owns its schema, `PRAGMA user_version` counter, writer and retention.
//! Scrollback and secrets are never stored.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod db;
pub mod dirwatch;
pub mod error;
pub mod paths;
pub mod redact;
pub mod snippets;

pub use config::{
    layout_changed, wireframe, BottomBar, BuiltinPreset, Config, ConfigEvent, ConfigLoad,
    ConfigWatcher, CustomPreset, Delegation, Density, General, GroupBy, LayoutConfig,
    LayoutOverrides, PanelId, Plugins, PresetId, ProjectLayoutOverride, RailVisibility,
    ResolvedLayout, RetentionConfig, SidebarLayout, SidebarOverrides, SidebarSection, SidebarSide,
    Splits, TabsPosition, TitlebarLayout, TitlebarOverrides, Updates, WireRect, WorktreeLocation,
    Zone,
};
pub use db::{
    detect_project, discover_projects, looks_sensitive, now_ms, DiscoveredProject, Favorite,
    HistoryEntry, NewCommand, NewSnippet, PathEntry, Project, ProjectKind, PruneStats,
    SessionLayout, Snippet, SnippetScope, StateDb,
};
pub use dirwatch::DirWatcher;
pub use error::{Result, StoreError};
pub use redact::{redact, Redacted};
pub use snippets::{parse_template, render, Param, Quoting};
