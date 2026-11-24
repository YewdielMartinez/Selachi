//! The update schedule and flow against an in-memory mock release (no network).

use std::sync::Mutex;

use gpui::{AppContext as _, TestAppContext};
use updater::FetchError;

use super::*;

const INSTALLER: &[u8] = b"MZ pretend installer";

/// Serves a mock release: the JSON index, `SHA256SUMS.txt` and the installer, by URL suffix.
struct Mock {
    tag: &'static str,
    hits: Mutex<Vec<String>>,
}

impl Mock {
    fn new(tag: &'static str) -> Arc<Self> {
        Arc::new(Self {
            tag,
            hits: Mutex::new(Vec::new()),
        })
    }

    fn count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
}

impl Fetcher for Mock {
    fn get(
        &self,
        url: &str,
        _cap: u64,
        _timeout: Duration,
        _progress: updater::Progress<'_>,
    ) -> Result<Vec<u8>, FetchError> {
        self.hits.lock().unwrap().push(url.to_string());
        let version = self.tag.trim_start_matches('v');
        let name = format!("Selachi-Setup-{version}.exe");
        if url.ends_with("latest") {
            let json = serde_json::json!({
                "tag_name": self.tag,
                "body": "## Novedades\n- Delegación entre agentes\n- Actualizaciones",
                "html_url": "https://example.invalid/r",
                "assets": [
                    {"name": name, "browser_download_url": format!("https://example.invalid/dl/{name}"), "size": INSTALLER.len()},
                    {"name": "SHA256SUMS.txt", "browser_download_url": "https://example.invalid/dl/SHA256SUMS.txt", "size": 100},
                ],
            });
            Ok(json.to_string().into_bytes())
        } else if url.ends_with("SHA256SUMS.txt") {
            Ok(format!("{}  {name}\n", updater::sha256_hex(INSTALLER)).into_bytes())
        } else if url.ends_with(&name) {
            Ok(INSTALLER.to_vec())
        } else {
            Err(FetchError::Status(404))
        }
    }
}

fn host(
    mock: &Arc<Mock>,
    current: &str,
    updates: &store::Updates,
    dir: &std::path::Path,
) -> UpdateHost {
    let cfg = UpdaterConfig {
        release_url: "https://example.invalid/latest".into(),
        asset_url_prefix: None,
        require_signature: false,
    };
    UpdateHost::new(current, updates).with_fetcher(mock.clone(), cfg, dir.to_path_buf())
}

#[gpui::test]
fn nothing_happens_before_thirty_seconds_then_the_update_is_downloaded(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new("v0.2.0");
    let entity = cx.new(|cx| {
        let mut h = host(&mock, "0.1.0", &store::Updates::default(), dir.path());
        h.start(cx);
        h
    });
    cx.executor().advance_clock(Duration::from_secs(29));
    cx.run_until_parked();
    assert_eq!(mock.count(), 0, "no network before the first 30 s");
    assert_eq!(
        entity.read_with(cx, |h, _| h.state().clone()),
        UpdateState::Idle
    );

    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let state = entity.read_with(cx, |h, _| h.state().clone());
    let UpdateState::Ready(ready) = state else {
        panic!("expected Ready, got {state:?}");
    };
    assert_eq!(ready.version.to_string(), "0.2.0");
    assert!(ready.notes.contains("Delegación"));
    assert!(ready.staged.path.is_file());
    assert_eq!(std::fs::read(&ready.staged.path).unwrap(), INSTALLER);
    assert!(entity.read_with(cx, |h, _| h.visible_update().is_some()));
}

#[gpui::test]
fn a_current_version_is_up_to_date_and_the_check_repeats_daily(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new("v0.1.0");
    let entity = cx.new(|cx| {
        let mut h = host(&mock, "0.1.0", &store::Updates::default(), dir.path());
        h.start(cx);
        h
    });
    cx.executor().advance_clock(Duration::from_secs(31));
    cx.run_until_parked();
    assert_eq!(
        entity.read_with(cx, |h, _| h.state().clone()),
        UpdateState::UpToDate
    );
    let after_first = mock.count();
    assert_eq!(after_first, 1, "one request: the release index");

    // 23 h later: still nothing (no polling), at 24 h: exactly one more check.
    cx.executor().advance_clock(Duration::from_secs(23 * 3600));
    cx.run_until_parked();
    assert_eq!(mock.count(), after_first);
    cx.executor().advance_clock(Duration::from_secs(3600 + 5));
    cx.run_until_parked();
    assert_eq!(mock.count(), after_first + 1);
}

#[gpui::test]
fn with_checks_off_the_network_is_never_touched(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new("v0.2.0");
    let off = store::Updates {
        check: false,
        ..store::Updates::default()
    };
    let entity = cx.new(|cx| {
        let mut h = host(&mock, "0.1.0", &off, dir.path());
        h.start(cx);
        h
    });
    cx.executor()
        .advance_clock(Duration::from_secs(3 * 24 * 3600));
    cx.run_until_parked();
    assert_eq!(mock.count(), 0);
    assert_eq!(
        entity.read_with(cx, |h, _| h.state().clone()),
        UpdateState::Idle
    );

    // turning it on starts the schedule; turning it off again stops it
    entity.update(cx, |h, cx| h.apply_config(&store::Updates::default(), cx));
    cx.executor().advance_clock(Duration::from_secs(31));
    cx.run_until_parked();
    assert!(mock.count() > 0);
}

#[gpui::test]
fn a_failed_download_is_reported_not_installed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // The sums list another hash: the installer must be refused.
    struct Bad(Arc<Mock>);
    impl Fetcher for Bad {
        fn get(
            &self,
            url: &str,
            cap: u64,
            t: Duration,
            p: updater::Progress<'_>,
        ) -> Result<Vec<u8>, FetchError> {
            if url.ends_with("SHA256SUMS.txt") {
                return Ok(format!("{}  Selachi-Setup-0.2.0.exe\n", "ab".repeat(32)).into_bytes());
            }
            self.0.get(url, cap, t, p)
        }
    }
    let mock = Mock::new("v0.2.0");
    let cfg = UpdaterConfig {
        release_url: "https://example.invalid/latest".into(),
        asset_url_prefix: None,
        require_signature: false,
    };
    let entity = cx.new(|cx| {
        let mut h = UpdateHost::new("0.1.0", &store::Updates::default()).with_fetcher(
            Arc::new(Bad(mock.clone())),
            cfg,
            dir.path().to_path_buf(),
        );
        h.check_now(cx);
        h
    });
    cx.run_until_parked();
    let state = entity.read_with(cx, |h, _| h.state().clone());
    assert!(matches!(state, UpdateState::Failed(_)), "{state:?}");
    assert!(entity.read_with(cx, |h, _| h.ready().is_none()));
    assert!(!dir.path().join("Selachi-Setup-0.2.0.exe").exists());
}

#[gpui::test]
fn dismissing_hides_the_card_and_on_exit_is_remembered(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new("v0.2.0");
    let entity = cx.new(|cx| {
        let mut h = host(&mock, "0.1.0", &store::Updates::default(), dir.path());
        h.check_now(cx);
        h
    });
    cx.run_until_parked();
    assert!(entity.read_with(cx, |h, _| h.visible_update().is_some()));
    assert!(!entity.read_with(cx, |h, _| h.installs_on_exit()));
    entity.update(cx, |h, cx| h.install_when_closing(cx));
    assert!(entity.read_with(cx, |h, _| h.installs_on_exit()));
    entity.update(cx, |h, cx| h.dismiss(cx));
    assert!(entity.read_with(cx, |h, _| h.visible_update().is_none()));
    assert!(
        entity.read_with(cx, |h, _| h.ready().is_some()),
        "still ready for Settings"
    );
}

#[test]
fn texts_read_naturally() {
    let v = Version::parse("0.2.0").unwrap();
    assert_eq!(ready_text(&v), "Actualización 0.2.0 lista");
    let cur = Version::parse("0.1.0").unwrap();
    assert_eq!(
        status_text(&UpdateState::UpToDate, &cur),
        "Estás al día · versión 0.1.0"
    );
    assert_eq!(status_text(&UpdateState::Checking, &cur), "Buscando…");
}
