//! Selachi grammar plugin: Bash.
//!
//! The crate only exists to link `tree-sitter-bash` (whose C parser defines
//! `tree_sitter_bash`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_bash::LANGUAGE as GRAMMAR;
