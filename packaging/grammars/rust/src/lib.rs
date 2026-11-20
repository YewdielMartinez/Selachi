//! Selachi grammar plugin: Rust.
//!
//! The crate only exists to link `tree-sitter-rust` (whose C parser defines
//! `tree_sitter_rust`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_rust::LANGUAGE as GRAMMAR;
