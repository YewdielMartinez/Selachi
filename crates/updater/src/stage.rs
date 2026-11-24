//! Downloading the installer and proving it is the one the release lists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::authenticode::{Authenticode, authenticode_status};
use crate::net::{Fetcher, Progress};
use crate::release::ReleaseInfo;
use crate::version::Version;
use crate::{Config, MAX_INSTALLER_BYTES, MAX_SUMS_BYTES, UpdateError};

const SUMS_TIMEOUT: Duration = Duration::from_secs(20);
const INSTALLER_TIMEOUT: Duration = Duration::from_secs(300);

/// An installer that is on disk and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    pub version: Version,
    pub sha256: String,
    pub size: u64,
    pub authenticode: Authenticode,
}

/// Lower-case hex SHA256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// `SHA256SUMS.txt` (`<hash>  <name>`, `sha256sum` style; a `*` before the name marks binary mode).
/// File name -> lower-case hash. Malformed lines are skipped.
pub fn parse_sums(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((hash, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim().trim_start_matches('*');
        if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty() {
            out.insert(name.to_string(), hash.to_ascii_lowercase());
        }
    }
    out
}

/// A plain file name: nothing that could point outside the updates folder.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && name.chars().all(|c| !c.is_control())
}

/// Downloads and verifies the installer of `release` into `dir` (created when missing).
///
/// A copy that is already there and matches the published hash is reused without downloading it
/// again. The file appears under its final name only after the hash matched: a partial or wrong
/// download is never left where [`crate::launch`] could run it.
pub fn stage(
    fetcher: &dyn Fetcher,
    cfg: &Config,
    release: &ReleaseInfo,
    dir: &Path,
    progress: Progress<'_>,
) -> Result<Staged, UpdateError> {
    let name = &release.installer.name;
    if !safe_name(name) || *name != ReleaseInfo::installer_name(&release.version) {
        return Err(UpdateError::BadRelease(format!(
            "nombre de instalador inesperado: {name}"
        )));
    }
    if release.installer.size > MAX_INSTALLER_BYTES {
        return Err(crate::FetchError::TooLarge {
            cap: MAX_INSTALLER_BYTES,
        }
        .into());
    }
    let sums_bytes = fetcher.get(
        &release.sums.url,
        MAX_SUMS_BYTES,
        SUMS_TIMEOUT,
        &mut |_, _| {},
    )?;
    let sums = parse_sums(&String::from_utf8_lossy(&sums_bytes));
    let expected = sums
        .get(name)
        .cloned()
        .ok_or_else(|| UpdateError::NotInSums(name.clone()))?;

    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(name);
    let size = if final_path.is_file() && sha256_file(&final_path).is_ok_and(|h| h == expected) {
        std::fs::metadata(&final_path)?.len()
    } else {
        let bytes = fetcher.get(
            &release.installer.url,
            MAX_INSTALLER_BYTES,
            INSTALLER_TIMEOUT,
            progress,
        )?;
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(UpdateError::HashMismatch { expected, actual });
        }
        let part = dir.join(format!("{name}.part"));
        std::fs::write(&part, &bytes)?;
        if final_path.exists() {
            let _ = std::fs::remove_file(&final_path);
        }
        std::fs::rename(&part, &final_path)?;
        bytes.len() as u64
    };

    let authenticode = authenticode_status(&final_path);
    match &authenticode {
        Authenticode::Invalid(why) => {
            let _ = std::fs::remove_file(&final_path);
            return Err(UpdateError::Signature(why.clone()));
        }
        Authenticode::Unsigned if cfg.require_signature => {
            let _ = std::fs::remove_file(&final_path);
            return Err(UpdateError::Signature(
                "el instalador no está firmado".into(),
            ));
        }
        _ => {}
    }
    Ok(Staged {
        path: final_path,
        version: release.version.clone(),
        sha256: expected,
        size,
        authenticode,
    })
}

/// Deletes every staged installer except the one of `keep` (and partial downloads). Best effort.
pub fn cleanup(dir: &Path, keep: Option<&Version>) {
    let keep_name = keep.map(ReleaseInfo::installer_name);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with("Selachi-Setup-")
            && (name.ends_with(".exe") || name.ends_with(".part"));
        if ours && Some(&name) != keep_name.as_ref() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
