//! Error type shared by the whole crate.

use std::path::PathBuf;

/// Everything that can go wrong in `store`.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid config: {0}")]
    ConfigParse(String),
    #[error("could not serialize config: {0}")]
    ConfigSerialize(String),
    #[error("file watcher error: {0}")]
    Watch(#[from] notify::Error),
    #[error("state db writer thread is gone")]
    WriterClosed,
    #[error("could not locate the user data directory")]
    NoDataDir,
    #[error("{0}")]
    Invalid(String),
}

impl StoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        StoreError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
