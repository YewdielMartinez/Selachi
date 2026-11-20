//! Selachi grammar plugin: YAML.
//!
//! The crate only exists to link `tree-sitter-yaml` (whose C parser defines
//! `tree_sitter_yaml`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_yaml::LANGUAGE as GRAMMAR;
