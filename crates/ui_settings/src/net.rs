//! The minimal HTTPS client of the plugin manager.
//!
//! Rules (docs/PLAN.md section 18, "Seguridad"):
//! * **https only.** Any other scheme is rejected before a socket is opened, and `ureq` itself
//!   refuses `http://` on every redirect hop (`https_only(true)`).
//! * **Size caps.** The index is read up to [`MAX_INDEX_BYTES`] (1 MB), an archive up to
//!   [`MAX_ARCHIVE_BYTES`] (20 MB). A declared `Content-Length` above the cap fails before any
//!   byte is read; a body that grows past it fails mid-stream.
//! * **Timeouts.** Connect 10 s; whole request 20 s for the index, 120 s for an archive.
//! * Nothing here knows about grammars: the caller verifies the sha256 pinned in the index.
//!
//! [`Fetcher`] is a trait so tests and the demo can serve bytes from memory or a folder.
//! `file://` support exists only in [`LocalFetcher`], compiled for tests and the `dev-file-urls`
//! feature.

use std::io::Read;
use std::time::Duration;

/// Cap for `grammars.toml`.
pub const MAX_INDEX_BYTES: u64 = 1024 * 1024;
/// Cap for one plugin archive.
pub const MAX_ARCHIVE_BYTES: u64 = 20 * 1024 * 1024;
/// Whole-request timeout of the index download.
pub const INDEX_TIMEOUT: Duration = Duration::from_secs(20);
/// Whole-request timeout of an archive download.
pub const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CHUNK: usize = 32 * 1024;

/// What can go wrong downloading something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The URL is not `https://`.
    NotHttps(String),
    /// The body is (or declares to be) larger than the cap.
    TooLarge { cap: u64 },
    /// DNS, TLS, timeout, connection reset...
    Network(String),
    /// The server answered with an error status.
    Status(u16),
    /// A local file could not be read (dev `file://` only).
    Io(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NotHttps(u) => write!(f, "Solo se admiten URL https:// ({u})"),
            FetchError::TooLarge { cap } => {
                write!(
                    f,
                    "La respuesta supera el límite de {}",
                    crate::fmt::bytes(*cap)
                )
            }
            FetchError::Network(m) => write!(f, "Sin conexión o respuesta inválida: {m}"),
            FetchError::Status(s) => write!(f, "El servidor respondió {s}"),
            FetchError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Progress callback: bytes received so far, total if the server said.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// Downloads one URL into memory under a size cap.
pub trait Fetcher: Send + Sync + 'static {
    fn get(
        &self,
        url: &str,
        cap: u64,
        timeout: Duration,
        progress: Progress<'_>,
    ) -> Result<Vec<u8>, FetchError>;
}

/// `Ok` only for `https://` URLs with a host.
pub fn check_https(url: &str) -> Result<(), FetchError> {
    let rest = url.trim().strip_prefix("https://").unwrap_or("");
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || url.trim() != url || url.chars().any(char::is_control) {
        return Err(FetchError::NotHttps(url.chars().take(80).collect()));
    }
    Ok(())
}

/// The real client: `ureq` + rustls, https only.
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn get(
        &self,
        url: &str,
        cap: u64,
        timeout: Duration,
        progress: Progress<'_>,
    ) -> Result<Vec<u8>, FetchError> {
        check_https(url)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(4)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(timeout))
            .user_agent("Selachi-plugins/1")
            .build()
            .into();
        let mut response = agent.get(url).call().map_err(map_error)?;
        let total = response.body().content_length();
        if total.is_some_and(|n| n > cap) {
            return Err(FetchError::TooLarge { cap });
        }
        read_capped(response.body_mut().as_reader(), cap, total, progress)
    }
}

fn map_error(e: ureq::Error) -> FetchError {
    match e {
        ureq::Error::StatusCode(s) => FetchError::Status(s),
        ureq::Error::RequireHttpsOnly(u) => FetchError::NotHttps(u),
        ureq::Error::BodyExceedsLimit(cap) => FetchError::TooLarge { cap },
        other => FetchError::Network(other.to_string()),
    }
}

/// Read `reader` to the end, failing as soon as more than `cap` bytes have arrived.
pub fn read_capped(
    mut reader: impl Read,
    cap: u64,
    total: Option<u64>,
    progress: Progress<'_>,
) -> Result<Vec<u8>, FetchError> {
    let mut out: Vec<u8> = Vec::with_capacity(total.map_or(16 * 1024, |t| t.min(cap)) as usize);
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            if e.to_string().contains("exceeds limit") {
                FetchError::TooLarge { cap }
            } else {
                FetchError::Network(e.to_string())
            }
        })?;
        if n == 0 {
            break;
        }
        if out.len() as u64 + n as u64 > cap {
            return Err(FetchError::TooLarge { cap });
        }
        out.extend_from_slice(&buf[..n]);
        progress(out.len() as u64, total);
    }
    Ok(out)
}

/// Serves the index from a `file://` URL and maps archive URLs (`https://.../name.zip`) onto a
/// local folder by file name. For tests and the demo only (`dev-file-urls`).
#[cfg(any(test, feature = "dev-file-urls"))]
#[derive(Debug, Clone)]
pub struct LocalFetcher {
    /// Folder holding the `.zip` files the index entries point at.
    pub archives_dir: std::path::PathBuf,
}

#[cfg(any(test, feature = "dev-file-urls"))]
impl Fetcher for LocalFetcher {
    fn get(
        &self,
        url: &str,
        cap: u64,
        _timeout: Duration,
        progress: Progress<'_>,
    ) -> Result<Vec<u8>, FetchError> {
        let path = if let Some(p) = url.strip_prefix("file:///") {
            std::path::PathBuf::from(p.replace('/', std::path::MAIN_SEPARATOR_STR))
        } else {
            check_https(url)?;
            let name = url.rsplit('/').next().unwrap_or("");
            self.archives_dir.join(name)
        };
        let file = std::fs::File::open(&path)
            .map_err(|e| FetchError::Io(format!("{}: {e}", path.display())))?;
        let total = file.metadata().ok().map(|m| m.len());
        if total.is_some_and(|n| n > cap) {
            return Err(FetchError::TooLarge { cap });
        }
        read_capped(file, cap, total, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_pass() {
        assert!(check_https("https://example.com/grammars.toml").is_ok());
        assert!(check_https("https://example.com").is_ok());
        for bad in [
            "http://example.com/g.toml",
            "file:///C:/g.toml",
            "ftp://example.com/g.toml",
            "//example.com/g.toml",
            "example.com/g.toml",
            "https://",
            "https:///path",
            " https://example.com/g.toml",
            "https://exa\nmple.com/",
            "",
        ] {
            assert!(
                matches!(check_https(bad), Err(FetchError::NotHttps(_))),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn http_fetcher_refuses_other_schemes_without_touching_the_network() {
        let mut calls = 0;
        let r = HttpFetcher.get(
            "http://127.0.0.1:1/x",
            MAX_INDEX_BYTES,
            INDEX_TIMEOUT,
            &mut |_, _| calls += 1,
        );
        assert!(matches!(r, Err(FetchError::NotHttps(_))));
        let r = HttpFetcher.get(
            "file:///C:/Windows/win.ini",
            MAX_INDEX_BYTES,
            INDEX_TIMEOUT,
            &mut |_, _| {},
        );
        assert!(matches!(r, Err(FetchError::NotHttps(_))));
        assert_eq!(calls, 0);
    }

    #[test]
    fn caps_stop_oversized_bodies() {
        let body = vec![7u8; 10_000];
        let ok = read_capped(&body[..], 10_000, Some(10_000), &mut |_, _| {}).unwrap();
        assert_eq!(ok.len(), 10_000);
        let err = read_capped(&body[..], 9_999, None, &mut |_, _| {}).unwrap_err();
        assert_eq!(err, FetchError::TooLarge { cap: 9_999 });
        // The documented caps.
        assert_eq!(MAX_INDEX_BYTES, 1_048_576);
        assert_eq!(MAX_ARCHIVE_BYTES, 20 * 1_048_576);
    }

    #[test]
    fn progress_is_reported_monotonically() {
        let body = vec![1u8; CHUNK * 3 + 5];
        let mut seen = Vec::new();
        read_capped(
            &body[..],
            MAX_ARCHIVE_BYTES,
            Some(body.len() as u64),
            &mut |n, t| seen.push((n, t)),
        )
        .unwrap();
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(seen.last().unwrap().0, body.len() as u64);
        assert!(seen.iter().all(|(_, t)| *t == Some(body.len() as u64)));
    }

    #[test]
    fn local_fetcher_maps_archive_urls_and_enforces_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a-1.zip"), vec![0u8; 100]).unwrap();
        let f = LocalFetcher {
            archives_dir: dir.path().to_path_buf(),
        };
        let got = f
            .get(
                "https://example.invalid/dl/a-1.zip",
                1000,
                ARCHIVE_TIMEOUT,
                &mut |_, _| {},
            )
            .unwrap();
        assert_eq!(got.len(), 100);
        let err = f
            .get(
                "https://example.invalid/dl/a-1.zip",
                50,
                ARCHIVE_TIMEOUT,
                &mut |_, _| {},
            )
            .unwrap_err();
        assert_eq!(err, FetchError::TooLarge { cap: 50 });
        // Even the local fetcher refuses plain http.
        assert!(matches!(
            f.get(
                "http://example.invalid/a-1.zip",
                1000,
                ARCHIVE_TIMEOUT,
                &mut |_, _| {}
            ),
            Err(FetchError::NotHttps(_))
        ));
        assert!(matches!(
            f.get(
                "https://example.invalid/missing.zip",
                1000,
                ARCHIVE_TIMEOUT,
                &mut |_, _| {}
            ),
            Err(FetchError::Io(_))
        ));
    }
}
