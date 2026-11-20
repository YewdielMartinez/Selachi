//! `manifest.toml`: describes one installed grammar and pins the sha256 of every file.
//!
//! ```toml
//! name = "csharp"                 # == folder name, lowercase [a-z0-9_-]
//! version = "0.23.5"
//! display_name = "C#"
//! file_extensions = ["cs", "csx"]
//! names = ["c#", "cs"]            # optional: fenced-code info strings
//! tree_sitter_abi = 14
//! symbol = "tree_sitter_c_sharp"  # exported by grammar.dll
//! size_bytes = 5400000            # informational (UI)
//!
//! [files]
//! "grammar.dll" = "<sha256>"
//! "highlights.scm" = "<sha256>"
//! "injections.scm" = "<sha256>"   # optional
//! "locals.scm" = "<sha256>"       # optional
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::GrammarError;

/// Manifest file name inside a plugin folder.
pub const MANIFEST_FILE: &str = "manifest.toml";
/// The compiled tree-sitter grammar.
pub const DLL_FILE: &str = "grammar.dll";
/// Mandatory highlight query.
pub const HIGHLIGHTS_FILE: &str = "highlights.scm";
/// Optional injection query.
pub const INJECTIONS_FILE: &str = "injections.scm";
/// Optional locals query.
pub const LOCALS_FILE: &str = "locals.scm";

/// Largest query file accepted.
pub(crate) const MAX_QUERY_BYTES: u64 = 2 * 1024 * 1024;
/// Largest file (the DLL) accepted.
pub(crate) const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// Parsed `manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub display_name: String,
    #[serde(default)]
    pub file_extensions: Vec<String>,
    #[serde(default)]
    pub names: Vec<String>,
    pub tree_sitter_abi: u32,
    pub symbol: String,
    /// File name -> lowercase hex sha256.
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub size_bytes: u64,
}

/// Plugin / language ids: lowercase ASCII letters, digits, `_` and `-`.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A plain file name: no separators, no `..`, no drive letters.
pub(crate) fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn valid_symbol(s: &str) -> bool {
    s.as_bytes().first().is_some_and(|b| !b.is_ascii_digit())
        && s.len() <= 128
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Manifest {
    /// Parse and validate. `origin` is only used in error messages.
    pub fn parse(text: &str, origin: &Path) -> Result<Manifest, GrammarError> {
        let mut m: Manifest =
            toml::from_str(text).map_err(|e| GrammarError::manifest(origin, e.to_string()))?;
        m.normalize();
        m.validate(origin)?;
        Ok(m)
    }

    /// Read `<dir>/manifest.toml`.
    pub fn read(dir: &Path) -> Result<Manifest, GrammarError> {
        let path = dir.join(MANIFEST_FILE);
        let meta = std::fs::metadata(&path).map_err(|e| GrammarError::io(&path, &e))?;
        if meta.len() > 256 * 1024 {
            return Err(GrammarError::manifest(&path, "manifest is too large"));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| GrammarError::io(&path, &e))?;
        Manifest::parse(&text, &path)
    }

    /// Serialise back to TOML (tests, tooling).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    fn normalize(&mut self) {
        for e in &mut self.file_extensions {
            *e = e.trim().trim_start_matches('.').to_ascii_lowercase();
        }
        self.file_extensions.retain(|e| !e.is_empty());
        for n in &mut self.names {
            *n = n.trim().to_ascii_lowercase();
        }
        self.names.retain(|n| !n.is_empty());
        for h in self.files.values_mut() {
            h.make_ascii_lowercase();
        }
    }

    fn validate(&self, origin: &Path) -> Result<(), GrammarError> {
        let bad = |msg: String| Err(GrammarError::manifest(origin, msg));
        if !valid_id(&self.name) {
            return Err(GrammarError::InvalidId(self.name.clone()));
        }
        if self.version.trim().is_empty() || self.version.len() > 64 {
            return bad("`version` is empty or too long".into());
        }
        if self.display_name.trim().is_empty() || self.display_name.len() > 64 {
            return bad("`display_name` is empty or too long".into());
        }
        if !valid_symbol(&self.symbol) {
            return bad(format!(
                "`symbol` `{}` is not a valid C identifier",
                self.symbol
            ));
        }
        if self.file_extensions.len() > 64 || self.names.len() > 64 {
            return bad("too many extensions or names".into());
        }
        for key in [DLL_FILE, HIGHLIGHTS_FILE] {
            if !self.files.contains_key(key) {
                return bad(format!("`files` must list `{key}`"));
            }
        }
        if self.files.len() > 16 {
            return bad("too many files".into());
        }
        for (name, sha) in &self.files {
            if !valid_file_name(name) || name == MANIFEST_FILE {
                return bad(format!("`{name}` is not an allowed file name"));
            }
            if !valid_sha(sha) {
                return bad(format!("sha256 of `{name}` is not 64 hex digits"));
            }
        }
        Ok(())
    }
}
