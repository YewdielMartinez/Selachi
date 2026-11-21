//! `viewer_svc`: the read-only file viewing engine behind Selachi's file viewer
//! (PLAN section 13). No UI dependencies.
//!
//! * [`detect`]: what kind of file is this (extension + 8 KB content sniff).
//! * [`Document`]: memory-mapped file with a lazily built line index, lossy line
//!   access, hex preview, minimap data and tree-sitter highlighting by visible range.
//! * [`diff_text`] / [`diff_against_git`]: hunks with inline word diffs.
//! * [`json_tree`] / [`toml_tree`] / [`yaml_tree`]: collapsible [`Node`] trees.
//! * [`markdown_blocks`]: a small block/inline model for rendering.
//! * [`grammars`]: language grammars as installable plugins (registry, index, installer).
//! * [`find_paths`] / [`resolve`]: Ctrl+click path detection for terminal output.

mod detect;
mod diff;
mod document;
mod error;
#[cfg(feature = "plugins")]
pub mod grammars;
mod highlight;
mod markdown;
mod paths;
mod structured;

pub use detect::{
    detect, detect_bytes, detect_with, Encoding, FileKind, ImageFormat, Lang, SNIFF_BYTES,
};
pub use diff::{diff_against_git, diff_text, DiffLine, GitBase, Hunk, LineKind, CONTEXT_LINES};
pub use document::{minimap_from_lengths, Document, HexLine, IndexProgress, Limits, HEX_ROW};
pub use error::{Error, Result};
pub use highlight::{available_languages, grammar_available, highlight_text, HighlightKind};
pub use markdown::{markdown_blocks, plain_text, MdAlign, MdBlock, MdInline, MdListItem, MdTable};
pub use paths::{find_paths, resolve, PathMatch, Resolved};
pub use structured::{
    json_tree, toml_tree, yaml_tree, Format, Node, NodeKind, Structured, MAX_CHILDREN, NODE_BUDGET,
};
