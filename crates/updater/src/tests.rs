//! The whole flow against a local mock release index (`file://` exists only here).

use std::path::Path;

use super::*;
use crate::net::LocalFetcher;

const INSTALLER_BODY: &[u8] = b"MZ pretend installer bytes";

/// Writes the mock release: `latest.json`, the installer and `SHA256SUMS.txt` in `dir`.
fn mock_release(dir: &Path, tag: &str, sums_hash: Option<&str>, extra: &str) -> Config {
    let version = tag.trim_start_matches('v');
    let name = format!("Selachi-Setup-{version}.exe");
    std::fs::write(dir.join(&name), INSTALLER_BODY).unwrap();
    let hash = sums_hash.map_or_else(|| sha256_hex(INSTALLER_BODY), str::to_string);
    std::fs::write(
        dir.join("SHA256SUMS.txt"),
        format!("{hash}  {name}\n{}  selachi.exe\n", "0".repeat(64)),
    )
    .unwrap();
    let base = "https://example.invalid/dl";
    let mut json = serde_json::json!({
        "tag_name": tag,
        "body": "## Novedades
- Delegacion
- Auto-update",
        "html_url": "https://example.invalid/r",
        "published_at": "2026-11-24T10:00:00Z",
        "assets": [
            {"name": name, "browser_download_url": format!("{base}/{name}"), "size": INSTALLER_BODY.len()},
            {"name": "SHA256SUMS.txt", "browser_download_url": format!("{base}/SHA256SUMS.txt"), "size": 200},
        ],
    });
    // `extra` is a JSON fragment like `,"draft":true`
    if !extra.is_empty() {
        let patch: serde_json::Value =
            serde_json::from_str(&format!("{{{}}}", extra.trim_start_matches(','))).unwrap();
        json.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
    }
    let json = json.to_string();
    std::fs::write(dir.join("latest.json"), json).unwrap();
    Config {
        release_url: format!(
            "file:///{}",
            dir.join("latest.json").to_string_lossy().replace('\\', "/")
        ),
        asset_url_prefix: Some(format!("{base}/")),
        require_signature: false,
    }
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[test]
fn a_newer_release_is_found_and_an_equal_or_older_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", None, "");
    let f = LocalFetcher::new(dir.path());
    let rel = check(&f, &cfg, &v("0.1.0"))
        .unwrap()
        .expect("0.2.0 > 0.1.0");
    assert_eq!(rel.version, v("0.2.0"));
    assert_eq!(rel.installer.name, "Selachi-Setup-0.2.0.exe");
    assert!(rel.notes.contains("Auto-update"));
    assert_eq!(check(&f, &cfg, &v("0.2.0")).unwrap(), None);
    assert_eq!(check(&f, &cfg, &v("0.3.0")).unwrap(), None);
}

#[test]
fn drafts_and_prereleases_are_skipped_on_the_stable_channel() {
    let dir = tempfile::tempdir().unwrap();
    for extra in [r#","draft":true"#, r#","prerelease":true"#] {
        let cfg = mock_release(dir.path(), "v9.0.0", None, extra);
        let f = LocalFetcher::new(dir.path());
        assert_eq!(check(&f, &cfg, &v("0.1.0")).unwrap(), None, "{extra}");
    }
    let cfg = mock_release(dir.path(), "v9.0.0-rc.1", None, "");
    assert_eq!(
        check(&LocalFetcher::new(dir.path()), &cfg, &v("0.1.0")).unwrap(),
        None
    );
}

#[test]
fn a_release_without_its_installer_or_from_another_host_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = mock_release(dir.path(), "v0.2.0", None, "");
    let json = std::fs::read_to_string(dir.path().join("latest.json")).unwrap();
    std::fs::write(
        dir.path().join("latest.json"),
        json.replace("Selachi-Setup-0.2.0.exe", "other.exe"),
    )
    .unwrap();
    let f = LocalFetcher::new(dir.path());
    assert!(matches!(
        check(&f, &cfg, &v("0.1.0")),
        Err(UpdateError::BadRelease(_))
    ));
    // the URL must come from the official repository
    let cfg_ok = mock_release(dir.path(), "v0.2.0", None, "");
    cfg = Config {
        asset_url_prefix: Some(
            "https://github.com/YewdielMartinez/Selachi/releases/download/".into(),
        ),
        ..cfg_ok
    };
    assert!(matches!(
        check(&f, &cfg, &v("0.1.0")),
        Err(UpdateError::BadRelease(m)) if m.contains("oficial")
    ));
}

#[test]
fn a_non_https_asset_url_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = mock_release(dir.path(), "v0.2.0", None, "");
    cfg.asset_url_prefix = None;
    let json = std::fs::read_to_string(dir.path().join("latest.json")).unwrap();
    std::fs::write(
        dir.path().join("latest.json"),
        json.replace("https://example.invalid/dl", "http://example.invalid/dl"),
    )
    .unwrap();
    assert!(matches!(
        check(&LocalFetcher::new(dir.path()), &cfg, &v("0.1.0")),
        Err(UpdateError::Fetch(FetchError::NotHttps(_)))
    ));
}

#[test]
fn staging_verifies_the_hash_and_reuses_a_good_copy() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", None, "");
    let updates = tempfile::tempdir().unwrap();
    let f = LocalFetcher::new(dir.path());
    let rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    let mut got = 0;
    let staged = stage(&f, &cfg, &rel, updates.path(), &mut |n, _| got = n).unwrap();
    assert_eq!(staged.path, updates.path().join("Selachi-Setup-0.2.0.exe"));
    assert_eq!(std::fs::read(&staged.path).unwrap(), INSTALLER_BODY);
    assert_eq!(staged.sha256, sha256_hex(INSTALLER_BODY));
    assert_eq!(got, INSTALLER_BODY.len() as u64);
    // unsigned is allowed for now, and recorded
    assert!(matches!(
        staged.authenticode,
        Authenticode::Unsigned | Authenticode::Invalid(_)
    ));
    // no `.part` left behind
    assert!(!updates.path().join("Selachi-Setup-0.2.0.exe.part").exists());

    // second time: the good copy is reused, the installer is not downloaded again
    let f2 = LocalFetcher::new(dir.path());
    stage(&f2, &cfg, &rel, updates.path(), &mut |_, _| {}).unwrap();
    let seen = f2.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|u| u.ends_with("SHA256SUMS.txt")));
    assert!(!seen.iter().any(|u| u.ends_with(".exe")), "{seen:?}");
}

#[test]
fn a_wrong_hash_is_discarded_and_never_left_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", Some(&"ab".repeat(32)), "");
    let updates = tempfile::tempdir().unwrap();
    let f = LocalFetcher::new(dir.path());
    let rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    let err = stage(&f, &cfg, &rel, updates.path(), &mut |_, _| {}).unwrap_err();
    assert!(matches!(err, UpdateError::HashMismatch { .. }), "{err:?}");
    assert_eq!(std::fs::read_dir(updates.path()).unwrap().count(), 0);
}

#[test]
fn a_tampered_copy_in_the_updates_folder_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", None, "");
    let updates = tempfile::tempdir().unwrap();
    std::fs::write(updates.path().join("Selachi-Setup-0.2.0.exe"), b"evil").unwrap();
    let f = LocalFetcher::new(dir.path());
    let rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    let staged = stage(&f, &cfg, &rel, updates.path(), &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(staged.path).unwrap(), INSTALLER_BODY);
}

#[test]
fn an_installer_missing_from_the_sums_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", None, "");
    std::fs::write(
        dir.path().join("SHA256SUMS.txt"),
        format!("{}  selachi.exe\n", "0".repeat(64)),
    )
    .unwrap();
    let f = LocalFetcher::new(dir.path());
    let rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    let updates = tempfile::tempdir().unwrap();
    assert!(matches!(
        stage(&f, &cfg, &rel, updates.path(), &mut |_, _| {}),
        Err(UpdateError::NotInSums(_))
    ));
}

#[test]
fn require_signature_refuses_an_unsigned_installer() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = mock_release(dir.path(), "v0.2.0", None, "");
    cfg.require_signature = true;
    let f = LocalFetcher::new(dir.path());
    let rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    let updates = tempfile::tempdir().unwrap();
    assert!(matches!(
        stage(&f, &cfg, &rel, updates.path(), &mut |_, _| {}),
        Err(UpdateError::Signature(_))
    ));
    assert!(!updates.path().join("Selachi-Setup-0.2.0.exe").exists());
}

#[test]
fn an_oversized_installer_is_refused_before_downloading() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", None, "");
    let f = LocalFetcher::new(dir.path());
    let mut rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    rel.installer.size = MAX_INSTALLER_BYTES + 1;
    let before = f.seen.lock().unwrap().len();
    let updates = tempfile::tempdir().unwrap();
    assert!(matches!(
        stage(&f, &cfg, &rel, updates.path(), &mut |_, _| {}),
        Err(UpdateError::Fetch(FetchError::TooLarge { .. }))
    ));
    assert_eq!(f.seen.lock().unwrap().len(), before, "nothing was fetched");
}

#[test]
fn an_installer_name_cannot_escape_the_updates_folder() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_release(dir.path(), "v0.2.0", None, "");
    let f = LocalFetcher::new(dir.path());
    let mut rel = check(&f, &cfg, &v("0.1.0")).unwrap().unwrap();
    rel.installer.name = "..\\..\\evil.exe".into();
    let updates = tempfile::tempdir().unwrap();
    assert!(matches!(
        stage(&f, &cfg, &rel, updates.path(), &mut |_, _| {}),
        Err(UpdateError::BadRelease(_))
    ));
}

#[test]
fn sums_parse_the_sha256sum_format() {
    let h = "ab".repeat(32);
    let text = format!(
        "{h}  Selachi-Setup-0.2.0.exe\r\n{}  *selachi.exe\nbroken line\n\n",
        "CD".repeat(32)
    );
    let m = parse_sums(&text);
    assert_eq!(m.len(), 2);
    assert_eq!(m["Selachi-Setup-0.2.0.exe"], h);
    assert_eq!(
        m["selachi.exe"],
        "cd".repeat(32),
        "lower-cased, binary marker dropped"
    );
}

#[test]
fn the_installer_runs_with_the_documented_flags() {
    let cmd = installer_command(Path::new("C:/u/Selachi-Setup-0.2.0.exe"), true);
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args.join(" "),
        "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS"
    );
    let exit: Vec<_> = installer_command(Path::new("x.exe"), false)
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(!exit.contains(&"/RESTARTAPPLICATIONS".to_string()));
    assert!(exit.contains(&"/CLOSEAPPLICATIONS".to_string()));
}

#[test]
fn cleanup_keeps_only_the_wanted_installer() {
    let dir = tempfile::tempdir().unwrap();
    for n in [
        "Selachi-Setup-0.1.0.exe",
        "Selachi-Setup-0.2.0.exe",
        "Selachi-Setup-0.3.0.exe.part",
        "notes.txt",
    ] {
        std::fs::write(dir.path().join(n), b"x").unwrap();
    }
    cleanup(dir.path(), Some(&v("0.2.0")));
    let mut left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["Selachi-Setup-0.2.0.exe", "notes.txt"]);
}

#[test]
fn a_release_url_over_http_is_never_fetched_by_the_real_client() {
    let r = HttpFetcher.get(
        "http://api.github.com/x",
        10,
        std::time::Duration::from_secs(1),
        &mut |_, _| {},
    );
    assert!(matches!(r, Err(FetchError::NotHttps(_))));
}
