//! Selachi grammar plugin: PowerShell.
//!
//! The crate only exists to link `tree-sitter-powershell` (whose C parser defines
//! `tree_sitter_powershell`) into a `cdylib` that exports that symbol; see `build.rs`.

// Keeps the grammar crate (and its static parser library) in the link.
pub use tree_sitter_powershell::LANGUAGE as GRAMMAR;
