//! Errors of the grammar plugin system. Cloneable on purpose: a failed load is cached
//! in the registry and handed back to every caller (and to the UI) without retrying.

/// Everything that can go wrong while discovering, installing or loading a grammar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrammarError {
    /// Filesystem error.
    #[error("{path}: {message}")]
    Io { path: String, message: String },
    /// `manifest.toml` is unreadable, malformed or breaks a rule (bad id, path traversal...).
    #[error("invalid manifest {path}: {message}")]
    Manifest { path: String, message: String },
    /// A file listed in the manifest does not exist.
    #[error("file `{file}` listed in the manifest is missing")]
    MissingFile { file: String },
    /// A file does not match the sha256 recorded in the manifest. Nothing was loaded.
    #[error("sha256 mismatch for `{file}`: manifest says {expected}, file is {actual}")]
    HashMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    /// The grammar was generated for a tree-sitter ABI this build cannot run.
    #[error("tree-sitter ABI {found} is outside the supported range {min}..={max}")]
    AbiIncompatible {
        found: usize,
        min: usize,
        max: usize,
    },
    /// The operating system refused to load the library.
    #[error("could not load `{path}`: {message}")]
    Load { path: String, message: String },
    /// The library does not export the symbol the manifest names.
    #[error("symbol `{symbol}` not found in the grammar library: {message}")]
    MissingSymbol { symbol: String, message: String },
    /// The grammar function returned something unusable.
    #[error("the grammar library returned an invalid language: {0}")]
    BadLanguage(String),
    /// `highlights.scm` / `injections.scm` / `locals.scm` do not compile against the grammar.
    #[error("grammar queries do not compile: {0}")]
    Query(String),
    /// No such plugin installed.
    #[error("grammar `{0}` is not installed")]
    NotInstalled(String),
    /// Not a valid plugin / language id.
    #[error("invalid grammar id `{0}`")]
    InvalidId(String),
    /// The zip archive is malformed or unsafe.
    #[error("invalid archive: {0}")]
    Archive(String),
    /// The archive does not match the sha256 pinned in the index.
    #[error("archive sha256 mismatch: index says {expected}, download is {actual}")]
    ArchiveHash { expected: String, actual: String },
    /// `grammars.toml` is malformed.
    #[error("invalid grammar index: {0}")]
    Index(String),
}

impl GrammarError {
    pub(crate) fn io(path: &std::path::Path, e: &std::io::Error) -> Self {
        GrammarError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        }
    }

    pub(crate) fn manifest(path: &std::path::Path, message: impl Into<String>) -> Self {
        GrammarError::Manifest {
            path: path.display().to_string(),
            message: message.into(),
        }
    }
}
