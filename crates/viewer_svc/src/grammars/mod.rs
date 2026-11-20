//! Language grammars as plugins (PLAN section 18).
//!
//! Selachi ships highlighting for JSON, TOML, CSS, HTML and Markdown inside the binary.
//! Every other language is a *grammar plugin*: a folder
//! `<plugins_dir>/<lang>/` with `grammar.dll` (a compiled tree-sitter grammar), the
//! highlight queries and a `manifest.toml` that pins the sha256 of every file.
//!
//! * [`GrammarRegistry`] discovers, installs, uninstalls and lazily loads plugins. A
//!   plugin is loaded the first time a file of its language is highlighted; before the
//!   DLL is mapped, every file is hashed against the manifest and the ABI is checked.
//!   Any failure is a typed [`GrammarError`], cached, and the file simply shows without
//!   colour. Loaded DLLs stay mapped for the process lifetime.
//! * [`GrammarIndex`] models the `grammars.toml` index of installable plugins
//!   (`lang`, `version`, `url`, `sha256` of a `.zip`, `size`). This crate contains no
//!   network code: the UI downloads the archive and hands the bytes to
//!   [`GrammarRegistry::install_from_archive`].
//! * [`set_plugin_registry`] makes a registry the process-wide one that
//!   [`crate::Document`] highlighting consults after the compiled-in grammars.
//!
//! Security: a grammar DLL is native code. Only archives whose sha256 is pinned in the
//! index (or a folder the user explicitly installs in developer mode) are installed, and
//! nothing is ever installed without the caller asking for it.

mod error;
mod index;
mod loader;
mod manifest;
mod registry;

pub use error::GrammarError;
pub use index::{sha256_hex, verify_archive, GrammarIndex, IndexEntry};
pub use manifest::{
    Manifest, DLL_FILE, HIGHLIGHTS_FILE, INJECTIONS_FILE, LOCALS_FILE, MANIFEST_FILE,
};
pub use registry::{
    Grammar, GrammarInfo, GrammarRegistry, GrammarSource, GrammarState, InstallOutcome,
    UninstallOutcome,
};

use std::sync::{Arc, RwLock};

static GLOBAL: RwLock<Option<Arc<GrammarRegistry>>> = RwLock::new(None);

/// Install `registry` as the process-wide plugin registry used by document highlighting
/// (`None` removes it). Typically called once at startup with
/// `GrammarRegistry::new(<appdata>/Selachi/plugins/grammars)`.
pub fn set_plugin_registry(registry: Option<Arc<GrammarRegistry>>) {
    *GLOBAL.write().unwrap_or_else(|e| e.into_inner()) = registry;
}

/// The process-wide plugin registry, if one was set.
pub fn plugin_registry() -> Option<Arc<GrammarRegistry>> {
    GLOBAL.read().unwrap_or_else(|e| e.into_inner()).clone()
}
