//! Well-known locations. Everything lives under `%APPDATA%\Selachi`.
//!
//! `SELACHI_DATA_DIR` overrides the directory (portable installs, tests, CI).

use crate::error::{Result, StoreError};
use std::path::{Path, PathBuf};

/// Environment variable that overrides [`app_data_dir`].
pub const DATA_DIR_ENV: &str = "SELACHI_DATA_DIR";

/// Resolve (without creating) the app data directory.
pub fn app_data_dir_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    // On Windows `config_dir()` is the roaming AppData folder (%APPDATA%).
    dirs::config_dir()
        .map(|d| d.join("Selachi"))
        .ok_or(StoreError::NoDataDir)
}

/// `%APPDATA%\Selachi`, created if missing.
pub fn app_data_dir() -> Result<PathBuf> {
    let dir = app_data_dir_path()?;
    std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
    Ok(dir)
}

/// `%APPDATA%\Selachi\config.toml` (parent directory created, file not).
pub fn config_path() -> Result<PathBuf> {
    Ok(config_path_in(&app_data_dir()?))
}

/// `%APPDATA%\Selachi\state.db` (parent directory created, file not).
///
/// A separate file from `selachi.db` (owned by `agents_svc`): each crate keeps
/// its own `PRAGMA user_version` migration counter, writer and retention policy,
/// so neither can break the other's schema or block its writes.
pub fn state_db_path() -> Result<PathBuf> {
    Ok(state_db_path_in(&app_data_dir()?))
}

pub fn config_path_in(dir: &Path) -> PathBuf {
    dir.join("config.toml")
}

pub fn state_db_path_in(dir: &Path) -> PathBuf {
    dir.join("state.db")
}

/// Normalize a filesystem path into a comparison key: forward slashes, no
/// verbatim (`\?\`) prefix, no trailing slash (except a bare root), lowercase
/// (Windows paths are case-insensitive).
pub fn normalize_key(path: &str) -> String {
    let mut s = path.trim().replace('\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    s.to_lowercase()
}

/// Display form of a path: native-agnostic cleanup (no verbatim prefix, no
/// trailing separator) but original case preserved.
pub fn clean_display(path: &str) -> String {
    let mut s = path.trim().to_string();
    if let Some(rest) = s.strip_prefix(r"\?\") {
        s = rest.to_string();
    }
    while s.len() > 1 && (s.ends_with('\') || s.ends_with('/')) && !s.ends_with(":\\") && !s.ends_with(":/") {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(normalize_key(r"C:\Users\Me\Proj\"), "c:/users/me/proj");
        assert_eq!(normalize_key(r"\?\C:\Foo"), "c:/foo");
        assert_eq!(normalize_key("C:\\"), "c:/");
        assert_eq!(normalize_key("/home/a/"), "/home/a");
    }

    #[test]
    fn display() {
        assert_eq!(clean_display(r"\?\C:\Foo\"), r"C:\Foo");
        assert_eq!(clean_display(r"C:\"), r"C:\");
    }
}
