//! Selachi grammar plugin: C#.
//!
//! The crate only exists to link `tree-sitter-c-sharp` (whose C parser defines
//! `tree_sitter_c_sharp`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_c_sharp::LANGUAGE as GRAMMAR;
