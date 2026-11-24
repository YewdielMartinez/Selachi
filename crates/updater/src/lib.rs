//! `updater`: Selachi's auto-update (wave 6b.2, docs/adr/0002-instalador.md). UI-agnostic.
//!
//! The flow, every step on the caller's background thread:
//!
//! 1. [`check`]: `GET` the GitHub "latest release" (https only, 1 MB cap), compare its tag with the
//!    running version ([`Version`]). A draft, a pre-release or an older tag is "up to date".
//! 2. [`stage`]: download `SHA256SUMS.txt` (64 KB cap) and `Selachi-Setup-x.y.z.exe` (80 MB cap) into
//!    the updates folder, verify the installer's SHA256 against the sums and its Authenticode status
//!    ([`Authenticode`]): an *invalid* signature is refused; an *unsigned* installer is allowed for
//!    now (the project has no certificate yet) and the status is recorded in [`Staged`].
//! 3. [`launch`]: run the installer with the documented flags ([`INSTALLER_ARGS`]), detached. Inno
//!    Setup closes the running Selachi through the Restart Manager and (for "Reiniciar y actualizar")
//!    starts it again. The caller decides when: on the user's action or at app exit.
//!
//! Nothing here keeps a timer or a thread: the app schedules the check (30 s after start, then every
//! 24 h, one timer) and calls these functions from its background executor.
//!
//! The network goes through [`Fetcher`]; the real one ([`HttpFetcher`]) refuses anything but
//! `https://`. Tests serve a local mock release index through a `file://` fetcher that only exists
//! under `cfg(test)`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod authenticode;
mod install;
mod net;
mod release;
mod stage;
mod version;

pub use authenticode::{Authenticode, authenticode_status};
pub use install::{INSTALLER_ARGS, ON_EXIT_ARGS, installer_command, launch};
pub use net::{FetchError, Fetcher, HttpFetcher, Progress, check_https, read_capped};
pub use release::{Asset, ReleaseInfo, check, parse_release};
pub use stage::{Staged, cleanup, parse_sums, sha256_hex, stage};
pub use version::Version;

use std::fmt;

/// Repository the releases come from.
pub const REPO: &str = "YewdielMartinez/Selachi";
/// The GitHub API call for the latest published (non-draft, non-prerelease) release.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/YewdielMartinez/Selachi/releases/latest";
/// Every installer asset URL must start with this (GitHub serves release assets from here and
/// redirects to its CDN).
pub const ASSET_URL_PREFIX: &str = "https://github.com/YewdielMartinez/Selachi/releases/download/";
/// Cap for the release JSON.
pub const MAX_RELEASE_BYTES: u64 = 1024 * 1024;
/// Cap for `SHA256SUMS.txt`.
pub const MAX_SUMS_BYTES: u64 = 64 * 1024;
/// Cap for the installer (the real one is ~10 MB).
pub const MAX_INSTALLER_BYTES: u64 = 80 * 1024 * 1024;
/// Cap for the release notes kept in memory and in `state.db`.
pub const MAX_NOTES_CHARS: usize = 24_000;

/// Where to look and how strict to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// URL of the "latest release" JSON.
    pub release_url: String,
    /// Required prefix of the installer / sums URLs; `None` accepts any `https://` URL (tests).
    pub asset_url_prefix: Option<String>,
    /// Refuse an installer without a valid Authenticode signature (off until the project signs).
    pub require_signature: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            release_url: LATEST_RELEASE_URL.to_string(),
            asset_url_prefix: Some(ASSET_URL_PREFIX.to_string()),
            require_signature: false,
        }
    }
}

/// What can go wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Fetch(FetchError),
    /// The release JSON is unreadable, or has no usable version / installer.
    BadRelease(String),
    /// The downloaded installer does not match `SHA256SUMS.txt`.
    HashMismatch {
        expected: String,
        actual: String,
    },
    /// `SHA256SUMS.txt` does not list the installer.
    NotInSums(String),
    /// The signature is invalid (or missing while one is required).
    Signature(String),
    Io(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Fetch(e) => write!(f, "{e}"),
            UpdateError::BadRelease(m) => write!(f, "La release no es válida: {m}"),
            UpdateError::HashMismatch { .. } => write!(
                f,
                "El instalador descargado no coincide con SHA256SUMS.txt; se descartó"
            ),
            UpdateError::NotInSums(n) => write!(f, "SHA256SUMS.txt no lista {n}"),
            UpdateError::Signature(m) => write!(f, "Firma del instalador: {m}"),
            UpdateError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<FetchError> for UpdateError {
    fn from(e: FetchError) -> Self {
        UpdateError::Fetch(e)
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}

/// The folder the installer is staged in: `<local app data>\Selachi\updates`.
pub fn default_updates_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(
        std::path::PathBuf::from(base)
            .join("Selachi")
            .join("updates"),
    )
}

#[cfg(test)]
mod tests;
