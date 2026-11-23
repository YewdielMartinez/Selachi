//! `ui_viewer`: the file viewer panels of Selachi (docs/PLAN.md section 13, "ver, no editar").
//!
//! | Piece | What it is |
//! |---|---|
//! | [`FileTreeProvider`] / [`FileTreePanel`] | `PanelProvider` id `files` (Sidebar | Right): lazy project tree, ignore-aware, git markers, quick filter |
//! | [`ViewerTab`] / [`FileViewer`] | `TabModel` + content view for the Main zone: code, diff, markdown, JSON/TOML/YAML tree, image, hex, bar minimap |
//! | [`AgentTouchesView`] / [`AgentTouchesTab`] | "Que toco el agente": files a session wrote, diff size per file, token cost |
//!
//! All of them talk to the host through [`ViewerEvent`]; none of them reaches outside itself.
//!
//! # Integration
//!
//! ```ignore
//! ui_viewer::bind_keys(cx);                                    // once: Ctrl+F, Ctrl+G, page keys, tree arrows, text fields
//! let files = FileTreeProvider::new().on_event(|ev, cx| shell.on_viewer_event(ev, cx));
//! registry.register(files.clone());                            // PanelProvider "files"
//! files.set_git_status(marks, cx);                             // optional M / A / ? markers
//!
//! // on ViewerEvent::OpenFile { path }:
//! let tab = ui_viewer::open_tab(path, ViewerConfig { project_root, ..Default::default() }, theme, cx);
//! // on ViewerEvent::OpenDiff { path, base }: same with `open_in_diff: true, diff_base: GitBase::Commit(..)`
//! ```
//!
//! Zero idle cost: no watchers, no timers. Background work (directory listings, the quick filter,
//! `Document::open`, diffs, markdown / tree parsing, find) ends in `cx.notify()`; the only timer is a
//! short poll while a file's line index and highlight are still being built.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod data_tree;
mod diff_rows;
mod event;
mod file_icon;
mod format;
mod image_info;
mod md_inline;
mod md_view;
mod minimap;
mod minimap_view;
mod rows;
mod snippet;
mod spans;
mod state;
mod syntax;
mod tab;
mod touches;
mod tree_model;
mod tree_panel;
mod viewer;

pub use data_tree::{DataRow, DataTree};
pub use diff_rows::{SplitRow, UnifiedRow, split_rows, unified_rows};
pub use event::ViewerEvent;
pub use file_icon::{FileGlyph, needed_svgs};
pub use format::{display_path, find_repo_root, format_bytes};
pub use image_info::image_dimensions;
pub use minimap::{BarSpec, bars, changed_flags, row_at, viewport_rect};
pub use spans::{PreparedLine, find_ranges, overlay, prepare_line};
pub use state::{
    Body, FindState, Goto, PluginHint, default_body, parse_goto, plugin_hint, reading_cap, search_document, source_body,
    supports_diff,
};
pub use syntax::color_for;
pub use tab::{ViewerTab, open_tab};
pub use touches::{
    AgentSession, AgentTouchesTab, AgentTouchesView, DiffStat, FileEntry, Touch, group_touches,
};
pub use tree_model::{Entry, FileTree, FilterHit, GitMark, Toggle, filter_files, list_dir};
pub use tree_panel::{FileTreePanel, FileTreeProvider, PANEL_ID as FILES_PANEL_ID};
pub use viewer::{FileViewer, ViewerConfig, bind_keys, checker_dark_cells};
