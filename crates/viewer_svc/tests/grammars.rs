#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "plugins")]
//! Grammar registry, index and installer. Tests using a real compiled plugin are
//! `#[ignore]`d: run `packaging/grammars/build.ps1` once, then
//! `cargo test -p viewer_svc --test grammars -- --ignored`.

mod common;

use std::fs;

use common::{copy_dist, dist_plugins, scratch, zip_dir, Fake};
use viewer_svc::grammars::{
    sha256_hex, verify_archive, GrammarError, GrammarIndex, GrammarRegistry, GrammarSource,
    GrammarState, IndexEntry, InstallOutcome, Manifest, UninstallOutcome,
};
use viewer_svc::Lang;

fn entry_for(lang: &str, zip: &[u8]) -> IndexEntry {
    IndexEntry {
        lang: lang.to_owned(),
        version: "1.0.0".into(),
        url: "https://example.invalid/x.zip".into(),
        sha256: sha256_hex(zip),
        size: zip.len() as u64,
        display_name: None,
        file_extensions: vec![],
    }
}

// ---- discovery and detection ------------------------------------------------------------

#[test]
fn discovers_plugins_and_lists_builtins_first() {
    let root = scratch("discover");
    Fake::new("zeta").write(&root);
    Fake {
        extensions: &["go"],
        ..Fake::new("go")
    }
    .write(&root);
    // not plugins: a folder without a manifest and a stray file
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("note.txt"), "x").unwrap();

    let reg = GrammarRegistry::new(&root);
    let list = reg.available();
    let ids: Vec<&str> = list.iter().map(|g| g.id.as_str()).collect();
    assert!(ids.ends_with(&["go", "zeta"]), "{ids:?}");
    let builtin_count = list
        .iter()
        .filter(|g| g.source == GrammarSource::Builtin)
        .count();
    assert_eq!(builtin_count, list.len() - 2);
    assert!(list
        .iter()
        .take(builtin_count)
        .all(|g| g.source == GrammarSource::Builtin));
    let go = list.iter().find(|g| g.id == "go").unwrap();
    assert_eq!(go.state, GrammarState::Installed);
    assert_eq!(go.version.as_deref(), Some("1.0.0"));
    assert!(reg.problems().is_empty());
}

#[test]
fn broken_manifest_is_reported_not_fatal() {
    let root = scratch("broken_manifest");
    let dir = Fake::new("ok").write(&root);
    assert!(dir.exists());
    fs::create_dir(root.join("bad")).unwrap();
    fs::write(root.join("bad").join("manifest.toml"), "name = [").unwrap();
    // folder name and manifest name disagree
    let other = Fake::new("liar").write(&root);
    fs::rename(&other, root.join("renamed")).unwrap();

    let reg = GrammarRegistry::new(&root);
    assert!(reg.has_plugin("ok"));
    assert_eq!(reg.problems().len(), 2);
}

#[test]
fn extensions_come_from_manifests() {
    let root = scratch("extensions");
    Fake {
        extensions: &["cs", ".CSX"],
        ..Fake::new("csharp")
    }
    .write(&root);
    Fake {
        extensions: &["go"],
        ..Fake::new("go")
    }
    .write(&root);
    let reg = GrammarRegistry::new(&root);
    // `.cs` is a known Lang with an installed plugin of the same id...
    assert_eq!(reg.id_for_extension("cs").as_deref(), Some("csharp"));
    assert_eq!(reg.id_for_extension(".CSX").as_deref(), Some("csharp"));
    // ...while `.go` is only known through the manifest.
    assert_eq!(reg.id_for_extension("go").as_deref(), Some("go"));
    assert_eq!(reg.id_for_extension("nope"), None);
    // built-in languages win and need no plugin
    assert_eq!(reg.id_for_extension("json").as_deref(), Some("json"));
    assert!(Lang::from_extension("go").is_none());
}

#[test]
#[cfg(not(feature = "lang-csharp"))]
fn builtin_resolves_without_loading_anything() {
    let reg = GrammarRegistry::new(scratch("builtin"));
    let g = reg.language_for(Lang::Json).unwrap();
    assert_eq!(g.source, GrammarSource::Builtin);
    assert!(g.language().is_none());
    assert_eq!(
        reg.language_for(Lang::CSharp).unwrap_err(),
        GrammarError::NotInstalled("csharp".into())
    );
}

// ---- verification: nothing is loaded when a check fails -------------------------------------

#[test]
fn hash_mismatch_is_rejected_before_loading() {
    let root = scratch("hash_mismatch");
    let dir = Fake::new("evil").write(&root);
    // Tamper with the DLL after the manifest was written. The bytes are garbage: if the
    // loader had tried to map it we would see a Load error, not a hash error.
    fs::write(dir.join("grammar.dll"), b"MZ tampered").unwrap();
    let reg = GrammarRegistry::new(&root);
    let err = reg.language_for_id("evil").unwrap_err();
    assert!(
        matches!(&err, GrammarError::HashMismatch { file, .. } if file == "grammar.dll"),
        "{err:?}"
    );
    // verify() reports the same without loading
    assert!(matches!(
        reg.verify("evil"),
        Err(GrammarError::HashMismatch { .. })
    ));
    // the failure is cached and visible to the UI
    assert_eq!(reg.failure("evil"), Some(err.clone()));
    assert_eq!(reg.language_for_id("evil").unwrap_err(), err);
    let info = reg
        .available()
        .into_iter()
        .find(|g| g.id == "evil")
        .unwrap();
    assert_eq!(info.state, GrammarState::Failed(err));
    assert!(!reg.has_plugin("evil"));
}

#[test]
fn tampered_query_is_rejected_too() {
    let root = scratch("query_tamper");
    let dir = Fake::new("q").write(&root);
    fs::write(dir.join("highlights.scm"), "(comment) @keyword").unwrap();
    let reg = GrammarRegistry::new(&root);
    assert!(matches!(
        reg.language_for_id("q"),
        Err(GrammarError::HashMismatch { file, .. }) if file == "highlights.scm"
    ));
}

#[test]
fn missing_file_is_reported() {
    let root = scratch("missing_file");
    let dir = Fake::new("m").write(&root);
    fs::remove_file(dir.join("highlights.scm")).unwrap();
    let reg = GrammarRegistry::new(&root);
    assert_eq!(
        reg.language_for_id("m").unwrap_err(),
        GrammarError::MissingFile {
            file: "highlights.scm".into()
        }
    );
}

#[test]
fn abi_out_of_range_is_rejected_before_loading() {
    let root = scratch("abi");
    // garbage DLL + valid hashes: only the ABI check can fail (a load attempt would be a
    // Load error)
    for (id, abi) in [("old", 1u32), ("future", 99u32)] {
        Fake {
            abi,
            ..Fake::new(id)
        }
        .write(&root);
    }
    let reg = GrammarRegistry::new(&root);
    for id in ["old", "future"] {
        let err = reg.language_for_id(id).unwrap_err();
        assert!(
            matches!(err, GrammarError::AbiIncompatible { .. }),
            "{err:?}"
        );
    }
    match reg.language_for_id("future").unwrap_err() {
        GrammarError::AbiIncompatible { found, min, max } => {
            assert_eq!(found, 99);
            assert_eq!(min, tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION);
            assert_eq!(max, tree_sitter::LANGUAGE_VERSION);
        }
        e => panic!("{e:?}"),
    }
}

#[test]
fn not_a_dll_is_a_load_error_not_a_panic() {
    let root = scratch("not_a_dll");
    Fake::new("junk").write(&root);
    let reg = GrammarRegistry::new(&root);
    let err = reg.language_for_id("junk").unwrap_err();
    assert!(matches!(err, GrammarError::Load { .. }), "{err:?}");
}

#[cfg(windows)]
#[test]
fn missing_symbol_is_a_typed_error() {
    // A real, harmless DLL that obviously has no tree-sitter entry point.
    let sys = std::path::PathBuf::from(
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()),
    )
    .join("System32")
    .join("version.dll");
    let bytes = fs::read(&sys).unwrap();
    let root = scratch("missing_symbol");
    Fake {
        dll: &bytes,
        symbol: "tree_sitter_does_not_exist",
        ..Fake::new("nosym")
    }
    .write(&root);
    let reg = GrammarRegistry::new(&root);
    let err = reg.language_for_id("nosym").unwrap_err();
    assert!(
        matches!(&err, GrammarError::MissingSymbol { symbol, .. } if symbol == "tree_sitter_does_not_exist"),
        "{err:?}"
    );
}

#[test]
fn manifest_rules_block_path_traversal() {
    let origin = std::path::Path::new("m.toml");
    let good = |files: &str| {
        format!(
            "name = \"x\"\nversion = \"1\"\ndisplay_name = \"X\"\ntree_sitter_abi = 14\nsymbol = \"tree_sitter_x\"\n[files]\n\"grammar.dll\" = \"{h}\"\n\"highlights.scm\" = \"{h}\"\n{files}",
            h = "0".repeat(64)
        )
    };
    assert!(Manifest::parse(&good(""), origin).is_ok());
    for evil in [
        format!("\"../x.dll\" = \"{}\"", "0".repeat(64)),
        format!("\"sub/x.scm\" = \"{}\"", "0".repeat(64)),
        format!("\"c:evil\" = \"{}\"", "0".repeat(64)),
        "\"ok.txt\" = \"nothex\"".to_owned(),
    ] {
        assert!(Manifest::parse(&good(&evil), origin).is_err(), "{evil}");
    }
    // bad ids / symbols
    assert!(Manifest::parse(&good("").replace("name = \"x\"", "name = \"../x\""), origin).is_err());
    assert!(Manifest::parse(&good("").replace("tree_sitter_x", "x; evil"), origin).is_err());
    // missing mandatory files
    assert!(Manifest::parse(
        &good("").replace("\"highlights.scm\"", "\"other.scm\""),
        origin
    )
    .is_err());
}

// ---- index and archives ---------------------------------------------------------------------

#[test]
fn index_parses_and_validates() {
    let text = format!(
        r#"
[[grammar]]
lang = "csharp"
version = "0.23.5"
url = "https://example.com/csharp-0.23.5.zip"
sha256 = "{}"
size = 123
display_name = "C#"
file_extensions = ["cs", ".CSX"]

[[grammar]]
lang = "go"
version = "1.0"
url = "https://example.com/go.zip"
sha256 = "{}"
"#,
        "AB".repeat(32),
        "cd".repeat(32)
    );
    let idx = GrammarIndex::parse(&text).unwrap();
    assert_eq!(idx.grammars.len(), 2);
    assert_eq!(idx.entry("csharp").unwrap().size, 123);
    assert_eq!(idx.entry("csharp").unwrap().sha256, "ab".repeat(32));
    assert_eq!(idx.entry_for_extension(".csx").unwrap().lang, "csharp");
    assert_eq!(idx.entry("go").unwrap().size, 0);
    assert!(idx.entry("nope").is_none());
    // round trip
    assert_eq!(GrammarIndex::parse(&idx.to_toml()).unwrap(), idx);

    for bad in [
        text.replace("https://example.com/go.zip", "http://example.com/go.zip"),
        text.replace("lang = \"go\"", "lang = \"../go\""),
        text.replace(&"cd".repeat(32), "short"),
        "[[grammar]]\nlang = 1".to_owned(),
    ] {
        assert!(GrammarIndex::parse(&bad).is_err(), "{bad}");
    }
    assert!(GrammarIndex::parse("").unwrap().grammars.is_empty());
}

#[test]
fn verify_archive_checks_sha256() {
    let bytes = b"abc";
    let sha = sha256_hex(bytes);
    assert_eq!(
        sha,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(verify_archive(bytes, &sha).is_ok());
    assert!(verify_archive(bytes, &sha.to_uppercase()).is_ok());
    let err = verify_archive(b"tampered", &sha).unwrap_err();
    assert!(matches!(err, GrammarError::ArchiveHash { .. }), "{err:?}");
}

#[test]
fn install_from_archive_installs_verified_plugin() {
    let src = scratch("arch_src");
    let plugin = Fake {
        extensions: &["go"],
        ..Fake::new("go")
    }
    .write(&src);
    let zip = zip_dir(&plugin);

    let root = scratch("arch_dst");
    let reg = GrammarRegistry::new(&root);
    assert_eq!(reg.id_for_extension("go"), None);
    let out = reg
        .install_from_archive(&zip, &entry_for("go", &zip))
        .unwrap();
    assert_eq!(
        out,
        InstallOutcome::Installed {
            id: "go".into(),
            version: "1.0.0".into()
        }
    );
    assert!(root.join("go").join("manifest.toml").exists());
    assert_eq!(reg.id_for_extension("go").as_deref(), Some("go"));
    assert!(reg.verify("go").is_ok());
    // survives a restart
    assert!(GrammarRegistry::new(&root).has_plugin("go"));
    // no staging leftovers
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["go".to_owned()]);
}

#[test]
fn install_from_archive_rejects_bad_archives() {
    let src = scratch("arch_bad_src");
    let plugin = Fake::new("go").write(&src);
    let zip = zip_dir(&plugin);
    let root = scratch("arch_bad_dst");
    let reg = GrammarRegistry::new(&root);

    // sha does not match the index
    let mut e = entry_for("go", &zip);
    e.sha256 = sha256_hex(b"something else");
    assert!(matches!(
        reg.install_from_archive(&zip, &e),
        Err(GrammarError::ArchiveHash { .. })
    ));
    // flipped byte in the download
    let mut tampered = zip.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xff;
    assert!(matches!(
        reg.install_from_archive(&tampered, &entry_for("go", &zip)),
        Err(GrammarError::ArchiveHash { .. })
    ));
    // wrong size in the index
    let mut e = entry_for("go", &zip);
    e.size += 1;
    assert!(matches!(
        reg.install_from_archive(&zip, &e),
        Err(GrammarError::Archive(_))
    ));
    // the archive holds `go` but the index entry says `rust`
    assert!(matches!(
        reg.install_from_archive(&zip, &entry_for("rust", &zip)),
        Err(GrammarError::Archive(_))
    ));
    // not a zip (sha matches, so the zip layer is what fails)
    let junk = b"definitely not a zip".to_vec();
    assert!(matches!(
        reg.install_from_archive(&junk, &entry_for("go", &junk)),
        Err(GrammarError::Archive(_))
    ));
    // a file inside differs from the manifest hash
    let evil = Fake::new("go").write(&scratch("arch_evil_src"));
    fs::write(evil.join("grammar.dll"), b"swapped").unwrap();
    let ezip = zip_dir(&evil);
    assert!(matches!(
        reg.install_from_archive(&ezip, &entry_for("go", &ezip)),
        Err(GrammarError::HashMismatch { .. })
    ));
    // nothing was installed and nothing left behind
    assert!(!reg.has_plugin("go"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn archive_entries_outside_the_manifest_are_never_extracted() {
    use std::io::Write;
    let src = scratch("arch_slip_src");
    let plugin = Fake::new("go").write(&src);
    // add a zip-slip entry next to the legit files
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for f in fs::read_dir(&plugin).unwrap().flatten() {
            zw.start_file(f.file_name().to_string_lossy(), o).unwrap();
            zw.write_all(&fs::read(f.path()).unwrap()).unwrap();
        }
        zw.start_file("../escaped.txt", o).unwrap();
        zw.write_all(b"pwned").unwrap();
        zw.finish().unwrap();
    }
    let zip = buf.into_inner();
    let root = scratch("arch_slip_dst").join("plugins");
    let reg = GrammarRegistry::new(&root);
    reg.install_from_archive(&zip, &entry_for("go", &zip))
        .unwrap();
    assert!(!root.parent().unwrap().join("escaped.txt").exists());
    assert!(!root.join("escaped.txt").exists());
    assert!(!root.join("go").join("escaped.txt").exists());
}

#[test]
fn install_from_dir_verifies_and_replaces() {
    let src = scratch("dir_src");
    let plugin = Fake {
        extensions: &["go"],
        ..Fake::new("go")
    }
    .write(&src);
    let root = scratch("dir_dst");
    let reg = GrammarRegistry::new(&root);
    assert!(matches!(
        reg.install_from_dir(&plugin).unwrap(),
        InstallOutcome::Installed { .. }
    ));
    assert!(reg.has_plugin("go"));
    // installing again replaces the folder (nothing is loaded)
    assert!(matches!(
        reg.install_from_dir(&plugin).unwrap(),
        InstallOutcome::Installed { .. }
    ));

    // a source whose DLL does not match its manifest is refused and leaves no trace
    fs::write(plugin.join("grammar.dll"), b"changed").unwrap();
    assert!(matches!(
        reg.install_from_dir(&plugin),
        Err(GrammarError::HashMismatch { .. })
    ));
    assert!(reg.verify("go").is_ok());
    // missing source file
    fs::remove_file(plugin.join("highlights.scm")).unwrap();
    assert!(matches!(
        reg.install_from_dir(&plugin),
        Err(GrammarError::MissingFile { .. })
    ));
    // not a plugin folder at all
    assert!(reg.install_from_dir(&src.join("nothing")).is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn uninstall_removes_unloaded_plugins() {
    let root = scratch("uninstall");
    Fake::new("go").write(&root);
    let reg = GrammarRegistry::new(&root);
    assert_eq!(reg.uninstall("go").unwrap(), UninstallOutcome::Removed);
    assert!(!root.join("go").exists());
    assert!(!reg.has_plugin("go"));
    assert_eq!(
        reg.uninstall("go").unwrap_err(),
        GrammarError::NotInstalled("go".into())
    );
    assert!(matches!(
        reg.uninstall("../x"),
        Err(GrammarError::InvalidId(_))
    ));
}

// ---- real compiled plugins (need packaging/grammars/build.ps1) ---------------------------------

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn real_csharp_plugin_discovers_and_loads() {
    let root = scratch("real_csharp");
    copy_dist(&root, &["csharp"]);
    let reg = GrammarRegistry::new(&root);
    let info = reg
        .available()
        .into_iter()
        .find(|g| g.id == "csharp" && g.source == GrammarSource::Plugin)
        .unwrap();
    assert_eq!(info.display_name, "C#");
    assert_eq!(info.state, GrammarState::Installed);
    assert!(info.file_extensions.contains(&"cs".to_owned()));
    assert!(info.size_bytes > 1_000_000);

    assert!(reg.verify("csharp").is_ok());
    let g = reg.load_plugin("csharp").unwrap();
    assert_eq!(g.source, GrammarSource::Plugin);
    let lang = g.language().unwrap();
    assert!((13..=15).contains(&lang.abi_version()));
    // the grammar really parses C#
    let mut parser = tree_sitter_probe::parser(lang);
    let tree = parser.parse("class A { int x; }", None).unwrap();
    assert!(!tree.root_node().has_error());

    // cached: second call is the same loaded plugin
    assert!(reg.language_for_extension("cs").is_ok());
    let info = reg
        .available()
        .into_iter()
        .find(|g| g.id == "csharp" && g.source == GrammarSource::Plugin)
        .unwrap();
    assert_eq!(info.state, GrammarState::Loaded);
}

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn real_dll_with_wrong_symbol_and_bad_query() {
    let root = scratch("real_bad");
    copy_dist(&root, &["csharp", "rust"]);
    // wrong symbol (hashes unchanged: only the manifest changes)
    let mp = root.join("csharp").join("manifest.toml");
    let text = fs::read_to_string(&mp).unwrap();
    fs::write(&mp, text.replace("tree_sitter_c_sharp", "tree_sitter_nope")).unwrap();
    // syntactically broken query with matching hash
    let rdir = root.join("rust");
    let mut m = Manifest::read(&rdir).unwrap();
    let bad = "(this is not a query";
    fs::write(rdir.join("highlights.scm"), bad).unwrap();
    m.files
        .insert("highlights.scm".into(), sha256_hex(bad.as_bytes()));
    fs::write(rdir.join("manifest.toml"), m.to_toml()).unwrap();

    let reg = GrammarRegistry::new(&root);
    assert!(matches!(
        reg.load_plugin("csharp"),
        Err(GrammarError::MissingSymbol { .. })
    ));
    assert!(matches!(
        reg.load_plugin("rust"),
        Err(GrammarError::Query(_))
    ));
}

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn real_dll_with_lying_abi_is_rejected() {
    let root = scratch("real_abi_lie");
    copy_dist(&root, &["csharp"]);
    let mut m = Manifest::read(&root.join("csharp")).unwrap();
    // inside the supported range, but not what the library reports
    m.tree_sitter_abi = if m.tree_sitter_abi == 14 { 15 } else { 14 };
    fs::write(root.join("csharp").join("manifest.toml"), m.to_toml()).unwrap();
    let reg = GrammarRegistry::new(&root);
    assert!(matches!(
        reg.load_plugin("csharp"),
        Err(GrammarError::BadLanguage(_))
    ));
}

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn every_built_plugin_loads_and_compiles_its_queries() {
    let ids: Vec<String> = fs::read_dir(dist_plugins().join("plugins"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(ids.len() >= 9, "{ids:?}");
    let root = scratch("real_all");
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    copy_dist(&root, &refs);
    let reg = GrammarRegistry::new(&root);
    for id in &ids {
        let g = reg.load_plugin(id).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(g.language().is_some(), "{id}");
    }
}

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn real_plugin_installs_from_dist_archive_and_index() {
    let index =
        GrammarIndex::parse(&fs::read_to_string(dist_plugins().join("grammars.toml")).unwrap())
            .unwrap();
    let entry = index.entry("csharp").unwrap();
    let zip = fs::read(dist_plugins().join(format!("csharp-{}.zip", entry.version))).unwrap();
    verify_archive(&zip, &entry.sha256).unwrap();
    assert_eq!(entry.size, zip.len() as u64);
    assert_eq!(index.entry_for_extension("cs").unwrap().lang, "csharp");

    let root = scratch("real_install");
    let reg = GrammarRegistry::new(&root);
    let out = reg.install_from_archive(&zip, entry).unwrap();
    assert!(matches!(out, InstallOutcome::Installed { .. }));
    assert!(reg.load_plugin("csharp").is_ok());
}

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn loaded_plugins_are_not_removed_or_replaced_until_restart() {
    let root = scratch("real_pending");
    copy_dist(&root, &["csharp"]);
    let reg = GrammarRegistry::new(&root);
    reg.load_plugin("csharp").unwrap();

    // update while loaded: staged, still usable
    let newer = scratch("real_pending_src");
    copy_dist(&newer, &["csharp"]);
    let out = reg.install_from_dir(&newer.join("csharp")).unwrap();
    assert!(
        matches!(out, InstallOutcome::PendingRestart { .. }),
        "{out:?}"
    );
    assert!(root.join("csharp.new").exists());
    let info = reg
        .available()
        .into_iter()
        .find(|g| g.id == "csharp" && g.source == GrammarSource::Plugin)
        .unwrap();
    assert!(info.pending_update);
    assert!(reg.load_plugin("csharp").is_ok());

    // uninstall while loaded: refused now, marked for removal
    assert_eq!(
        reg.uninstall("csharp").unwrap(),
        UninstallOutcome::PendingRestart
    );
    assert!(root.join("csharp").join("grammar.dll").exists());
    let info = reg
        .available()
        .into_iter()
        .find(|g| g.id == "csharp" && g.source == GrammarSource::Plugin)
        .unwrap();
    assert!(info.pending_removal && !info.pending_update);
    assert!(
        reg.load_plugin("csharp").is_ok(),
        "still usable this session"
    );
    assert!(!root.join("csharp.new").exists());

    // "restart": a new registry applies the pending removal (the DLL is still mapped by
    // this process, so deletion may legitimately fail; the marker keeps it hidden)
    let reg2 = GrammarRegistry::new(&root);
    assert!(!reg2.has_plugin("csharp") || root.join("csharp").exists());
    let info2 = reg2
        .available()
        .into_iter()
        .find(|g| g.id == "csharp" && g.source == GrammarSource::Plugin);
    if let Some(i) = info2 {
        assert!(i.pending_removal);
    }
}

/// Minimal parser access without a direct tree-sitter dependency in the test crate.
mod tree_sitter_probe {
    pub fn parser(lang: &tree_sitter::Language) -> tree_sitter::Parser {
        let mut p = tree_sitter::Parser::new();
        p.set_language(lang).unwrap();
        p
    }
}
