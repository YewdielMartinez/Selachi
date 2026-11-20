//! Error type shared by every module of the crate.

use std::path::PathBuf;

/// Everything that can go wrong in `viewer_svc`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem error, tagged with the path when known.
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A structured document (JSON / TOML / YAML) could not be parsed.
    #[error("{format} parse error: {message}")]
    Parse {
        format: &'static str,
        message: String,
    },
    /// `git` could not be started or reported a failure.
    #[error("git: {0}")]
    Git(String),
    /// The caller passed something unusable (bad revision, path outside the repo...).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
