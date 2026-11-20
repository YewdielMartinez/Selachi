//! Selachi grammar plugin: JavaScript.
//!
//! The crate only exists to link `tree-sitter-javascript` (whose C parser defines
//! `tree_sitter_javascript`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_javascript::LANGUAGE as GRAMMAR;
