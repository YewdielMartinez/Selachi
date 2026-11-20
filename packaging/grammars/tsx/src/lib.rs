//! Selachi grammar plugin: TSX.
//!
//! The crate only exists to link `tree-sitter-typescript` (whose C parser defines
//! `tree_sitter_tsx`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_typescript::LANGUAGE_TSX as GRAMMAR;
