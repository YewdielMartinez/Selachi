//! The GitHub release JSON and the "is there something newer?" question.

use std::time::Duration;

use serde::Deserialize;

use crate::net::Fetcher;
use crate::version::Version;
use crate::{Config, MAX_NOTES_CHARS, MAX_RELEASE_BYTES, UpdateError};

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A newer release, with everything needed to stage it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    /// Release notes (Markdown), capped.
    pub notes: String,
    pub html_url: Option<String>,
    pub published_at: Option<String>,
    pub installer: Asset,
    pub sums: Asset,
}

impl ReleaseInfo {
    /// `Selachi-Setup-0.2.0.exe`.
    pub fn installer_name(version: &Version) -> String {
        format!("Selachi-Setup-{version}.exe")
    }
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Reads a release JSON. `Ok(None)` for a draft or a pre-release (the stable channel skips them).
/// A release with a usable version but no installer or no sums is an error, not "up to date": the
/// maintainer is probably still uploading assets.
pub fn parse_release(json: &str, cfg: &Config) -> Result<Option<ReleaseInfo>, UpdateError> {
    let raw: RawRelease = serde_json::from_str(json)
        .map_err(|e| UpdateError::BadRelease(format!("JSON ilegible ({e})")))?;
    if raw.draft || raw.prerelease {
        return Ok(None);
    }
    let version = Version::parse(&raw.tag_name).ok_or_else(|| {
        UpdateError::BadRelease(format!("etiqueta {:?} sin versión", raw.tag_name))
    })?;
    if version.is_prerelease() {
        return Ok(None);
    }
    let want = ReleaseInfo::installer_name(&version);
    let find = |name: &str| -> Result<Asset, UpdateError> {
        let a = raw
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| UpdateError::BadRelease(format!("falta el archivo {name}")))?;
        crate::net::check_https(&a.browser_download_url)?;
        if let Some(prefix) = &cfg.asset_url_prefix
            && !a.browser_download_url.starts_with(prefix.as_str())
        {
            return Err(UpdateError::BadRelease(format!(
                "la URL de {name} no es de la release oficial"
            )));
        }
        Ok(Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    };
    let installer = find(&want)?;
    let sums = find("SHA256SUMS.txt")?;
    Ok(Some(ReleaseInfo {
        version,
        tag: raw.tag_name,
        notes: cap_chars(raw.body.as_deref().unwrap_or("").trim(), MAX_NOTES_CHARS),
        html_url: raw.html_url,
        published_at: raw.published_at,
        installer,
        sums,
    }))
}

/// Looks for a release newer than `current`. `Ok(None)` = up to date.
pub fn check(
    fetcher: &dyn Fetcher,
    cfg: &Config,
    current: &Version,
) -> Result<Option<ReleaseInfo>, UpdateError> {
    let bytes = fetcher.get(
        &cfg.release_url,
        MAX_RELEASE_BYTES,
        CHECK_TIMEOUT,
        &mut |_, _| {},
    )?;
    let json = String::from_utf8_lossy(&bytes);
    Ok(parse_release(&json, cfg)?.filter(|r| r.version > *current))
}
