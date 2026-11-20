//! `grammars.toml`: the index of installable grammar plugins, published with Selachi's
//! releases. Pure data and verification, no network code (downloads belong to the UI).
//!
//! ```toml
//! [[grammar]]
//! lang = "csharp"
//! version = "0.23.5"
//! url = "https://github.com/OWNER/selachi/releases/download/grammars-v1/csharp-0.23.5.zip"
//! sha256 = "<sha256 of the .zip>"
//! size = 2100000                  # bytes of the .zip
//! display_name = "C#"             # optional, for the UI before installing
//! file_extensions = ["cs", "csx"] # optional, to suggest the plugin for a file
//! ```

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::GrammarError;
use super::manifest::valid_id;

/// One installable grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub lang: String,
    pub version: String,
    pub url: String,
    /// Lowercase hex sha256 of the `.zip` at `url`.
    pub sha256: String,
    /// Size of the `.zip` in bytes.
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
}

/// The parsed index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrammarIndex {
    #[serde(default, rename = "grammar")]
    pub grammars: Vec<IndexEntry>,
}

impl GrammarIndex {
    /// Parse and validate `grammars.toml` text.
    pub fn parse(text: &str) -> Result<GrammarIndex, GrammarError> {
        let mut index: GrammarIndex =
            toml::from_str(text).map_err(|e| GrammarError::Index(e.to_string()))?;
        for e in &mut index.grammars {
            e.sha256.make_ascii_lowercase();
            for x in &mut e.file_extensions {
                *x = x.trim().trim_start_matches('.').to_ascii_lowercase();
            }
            if !valid_id(&e.lang) {
                return Err(GrammarError::InvalidId(e.lang.clone()));
            }
            if e.sha256.len() != 64 || !e.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(GrammarError::Index(format!(
                    "`{}`: sha256 must be 64 hex digits",
                    e.lang
                )));
            }
            // Only TLS downloads are ever offered.
            if !e.url.starts_with("https://") {
                return Err(GrammarError::Index(format!(
                    "`{}`: url must be https",
                    e.lang
                )));
            }
        }
        Ok(index)
    }

    /// Serialise (build tooling and tests).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Entry for a language id.
    pub fn entry(&self, lang: &str) -> Option<&IndexEntry> {
        self.grammars.iter().find(|e| e.lang == lang)
    }

    /// Entry that declares a file extension (for "install the C# plugin?" hints).
    pub fn entry_for_extension(&self, ext: &str) -> Option<&IndexEntry> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        self.grammars
            .iter()
            .find(|e| e.file_extensions.contains(&ext))
    }
}

/// Lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Check a downloaded archive against the sha256 pinned in the index.
pub fn verify_archive(bytes: &[u8], expected_sha256: &str) -> Result<(), GrammarError> {
    let actual = sha256_hex(bytes);
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if actual == expected {
        Ok(())
    } else {
        Err(GrammarError::ArchiveHash { expected, actual })
    }
}
