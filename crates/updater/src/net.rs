//! The minimal HTTPS client of the updater.
//!
//! * **https only**: any other scheme is refused before a socket opens, and `ureq` refuses `http://`
//!   on every redirect hop (`https_only(true)`).
//! * **Size caps**: a declared `Content-Length` above the cap fails before any byte is read; a body
//!   that grows past it fails mid-stream.
//! * **Timeouts**: connect 10 s plus a whole-request timeout the caller picks.
//!
//! [`Fetcher`] is a trait so tests serve a local mock release index. `file://` support exists only in
//! `LocalFetcher`, compiled for tests: a release build cannot read a local URL.

use std::io::Read;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CHUNK: usize = 64 * 1024;

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
    /// A local file could not be read (tests only).
    Io(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NotHttps(u) => write!(f, "Solo se admiten URL https:// ({u})"),
            FetchError::TooLarge { cap } => {
                write!(
                    f,
                    "La respuesta supera el límite de {} MB",
                    cap / (1024 * 1024)
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
            .max_redirects(5)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(timeout))
            .user_agent(concat!("Selachi-updater/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut response = agent
            .get(url)
            .header(
                "Accept",
                "application/vnd.github+json, application/octet-stream, */*",
            )
            .call()
            .map_err(map_error)?;
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

/// Serves a mock release index from `file:///...` and maps asset URLs (`https://.../name`) onto a
/// local folder by file name. Tests only.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct LocalFetcher {
    pub assets_dir: std::path::PathBuf,
    /// Every URL asked for, in order.
    pub seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(test)]
impl LocalFetcher {
    pub fn new(assets_dir: &std::path::Path) -> Self {
        Self {
            assets_dir: assets_dir.to_path_buf(),
            seen: Default::default(),
        }
    }
}

#[cfg(test)]
impl Fetcher for LocalFetcher {
    fn get(
        &self,
        url: &str,
        cap: u64,
        _timeout: Duration,
        progress: Progress<'_>,
    ) -> Result<Vec<u8>, FetchError> {
        self.seen.lock().unwrap().push(url.to_string());
        let path = if let Some(p) = url.strip_prefix("file:///") {
            std::path::PathBuf::from(p.replace('/', std::path::MAIN_SEPARATOR_STR))
        } else {
            check_https(url)?;
            self.assets_dir.join(url.rsplit('/').next().unwrap_or(""))
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
        assert!(check_https("https://api.github.com/repos/a/b/releases/latest").is_ok());
        for bad in [
            "http://example.com/x",
            "file:///C:/x",
            "ftp://example.com/x",
            "//example.com/x",
            "https://",
            " https://example.com/x",
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
    fn the_real_client_never_opens_a_non_https_url() {
        let mut calls = 0;
        for url in ["http://127.0.0.1:1/x", "file:///C:/Windows/win.ini"] {
            let r = HttpFetcher.get(url, 1000, Duration::from_secs(1), &mut |_, _| calls += 1);
            assert!(matches!(r, Err(FetchError::NotHttps(_))), "{url}");
        }
        assert_eq!(calls, 0);
    }

    #[test]
    fn caps_stop_oversized_bodies() {
        let body = vec![7u8; 10_000];
        assert_eq!(
            read_capped(&body[..], 10_000, Some(10_000), &mut |_, _| {})
                .unwrap()
                .len(),
            10_000
        );
        assert_eq!(
            read_capped(&body[..], 9_999, None, &mut |_, _| {}).unwrap_err(),
            FetchError::TooLarge { cap: 9_999 }
        );
    }
}
