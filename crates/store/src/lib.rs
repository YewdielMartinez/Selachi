//! Selachi persistence, UI-agnostic (no gpui).
//!
//! * [`config`]: `%APPDATA%\Selachi\config.toml`, typed, comment-preserving save, hot reload.
//! * [`db`]: `%APPDATA%\Selachi\state.db`: command history (FTS5), path frecency,
//!   projects, favorites, session layout, retention.
//! * [`paths`]: well-known locations.
//!
//! `state.db` is deliberately a different file from `agents_svc`'s `selachi.db`:
//! each owns its schema, `PRAGMA user_version` counter, writer and retention.
//! Scrollback and secrets are never stored.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod db;
pub mod error;
pub mod paths;

pub use config::{
    layout_changed, wireframe, BottomBar, BuiltinPreset, Config, ConfigEvent, ConfigLoad,
    ConfigWatcher, CustomPreset, Density, GroupBy, LayoutConfig, LayoutOverrides, PanelId,
    PresetId, ProjectLayoutOverride, RailVisibility, ResolvedLayout, RetentionConfig,
    SidebarLayout, SidebarOverrides, SidebarSection, SidebarSide, Splits, TabsPosition, WireRect,
    Zone,
};
pub use db::{
    detect_project, discover_projects, DiscoveredProject, Favorite, HistoryEntry, NewCommand,
    PathEntry, Project, ProjectKind, PruneStats, SessionLayout, StateDb,
};
pub use error::{Result, StoreError};
