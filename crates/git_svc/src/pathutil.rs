//! Path helpers: comparing paths the way Windows sees them (git prints `C:/x`, std prints `C:\x`).

use std::path::{Component, Path, PathBuf};

/// A comparison key: canonical when the path exists (falls back to canonical parent + name, then
/// to the raw path), the `\\?\` prefix stripped, `/` separators, no trailing slash, lowercased on Windows.
pub(crate) fn norm_key(p: &Path) -> String {
    let canon = std::fs::canonicalize(p)
        .ok()
        .or_else(|| {
            let parent = p.parent()?;
            let name = p.file_name()?;
            std::fs::canonicalize(parent).ok().map(|c| c.join(name))
        })
        .unwrap_or_else(|| p.to_path_buf());
    let mut s = canon.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_owned();
    }
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    norm_key(a) == norm_key(b)
}

/// True when `child` is `parent` itself or lies inside it.
pub(crate) fn is_inside(child: &Path, parent: &Path) -> bool {
    let c = norm_key(child);
    let p = norm_key(parent);
    c == p || c.starts_with(&format!("{}/", p.trim_end_matches('/')))
}

/// True if the path has a `..` component.
pub(crate) fn has_parent_dir(p: &Path) -> bool {
    p.components().any(|c| matches!(c, Component::ParentDir))
}

/// Lowercase on Windows (case-insensitive file system), identity elsewhere.
pub(crate) fn fold_case(s: &str) -> String {
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// Make `p` absolute against `base` when relative.
pub(crate) fn absolutize(p: &Path, base: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}
