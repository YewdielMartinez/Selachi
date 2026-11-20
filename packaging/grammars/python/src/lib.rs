//! Selachi grammar plugin: Python.
//!
//! The crate only exists to link `tree-sitter-python` (whose C parser defines
//! `tree_sitter_python`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_python::LANGUAGE as GRAMMAR;
