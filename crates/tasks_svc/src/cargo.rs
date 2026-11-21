//! Cargo: the root package/workspace plus `cargo run -p <member>` for binaries.

use crate::model::TaskKind;
use crate::scan::{expand_dirs, Scan};
use std::path::Path;

const MAX_MEMBERS: usize = 64;

pub(crate) fn detect(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let manifest = root.join("Cargo.toml");
    let Some(text) = s.read(&manifest) else {
        return;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return;
    };
    let workspace = doc.get("workspace").and_then(toml::Value::as_table);

    s.push(
        TaskKind::Cargo,
        "build",
        "cargo build",
        &root,
        &manifest,
        None,
    );
    s.push(
        TaskKind::Cargo,
        "test",
        "cargo test",
        &root,
        &manifest,
        None,
    );
    s.push(
        TaskKind::Cargo,
        "clippy",
        "cargo clippy --all-targets -- -D warnings",
        &root,
        &manifest,
        Some("Lints como errores".into()),
    );
    if doc.contains_key("package") && has_bin(s, &root, &doc) {
        s.push(TaskKind::Cargo, "run", "cargo run", &root, &manifest, None);
    }

    let Some(ws) = workspace else {
        return;
    };
    let patterns: Vec<String> = ws
        .get("members")
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let excluded: Vec<String> = ws
        .get("exclude")
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(toml::Value::as_str)
                .map(|p| p.trim_end_matches('/').replace('\\', "/"))
                .collect()
        })
        .unwrap_or_default();
    for dir in expand_dirs(s, &patterns, MAX_MEMBERS) {
        if excluded.contains(&s.rel(&dir)) {
            continue;
        }
        let member_manifest = dir.join("Cargo.toml");
        let Some(text) = s.read(&member_manifest) else {
            continue;
        };
        let Ok(member) = text.parse::<toml::Table>() else {
            continue;
        };
        let Some(name) = member
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        if has_bin(s, &dir, &member) {
            s.push(
                TaskKind::Cargo,
                format!("run: {name}"),
                format!("cargo run -p {name}"),
                &root,
                &member_manifest,
                None,
            );
        }
    }
}

/// `src/main.rs`, `src/bin/*` or a `[[bin]]` table.
fn has_bin(s: &mut Scan<'_>, dir: &Path, manifest: &toml::Table) -> bool {
    if manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return true;
    }
    s.exists(&dir.join("src").join("main.rs")) || s.exists(&dir.join("src").join("bin"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DiscoverOptions;

    fn run(dir: &Path) -> Vec<crate::TaskDef> {
        let opts = DiscoverOptions::default();
        let mut s = Scan::new(dir, &opts);
        detect(&mut s);
        s.finish().0
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn commands(tasks: &[crate::TaskDef]) -> Vec<&str> {
        tasks.iter().map(|t| t.command.as_str()).collect()
    }

    #[test]
    fn single_binary_package() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        write(d.path(), "src/main.rs", "fn main(){}");
        assert_eq!(
            commands(&run(d.path())),
            [
                "cargo build",
                "cargo test",
                "cargo clippy --all-targets -- -D warnings",
                "cargo run"
            ]
        );
    }

    #[test]
    fn library_has_no_run() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        write(d.path(), "src/lib.rs", "");
        assert!(!commands(&run(d.path())).contains(&"cargo run"));
    }

    #[test]
    fn workspace_members_with_bins() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "Cargo.toml",
            "[workspace]\nmembers=[\"crates/*\"]\nexclude=[\"crates/old\"]\n",
        );
        write(
            d.path(),
            "crates/app/Cargo.toml",
            "[package]\nname=\"app\"\n",
        );
        write(d.path(), "crates/app/src/main.rs", "");
        write(
            d.path(),
            "crates/lib/Cargo.toml",
            "[package]\nname=\"lib\"\n",
        );
        write(d.path(), "crates/lib/src/lib.rs", "");
        write(
            d.path(),
            "crates/tool/Cargo.toml",
            "[package]\nname=\"tool-x\"\n[[bin]]\nname=\"t\"\npath=\"t.rs\"\n",
        );
        write(
            d.path(),
            "crates/old/Cargo.toml",
            "[package]\nname=\"old\"\n",
        );
        write(d.path(), "crates/old/src/main.rs", "");
        let t = run(d.path());
        let c = commands(&t);
        assert!(c.contains(&"cargo run -p app"));
        assert!(c.contains(&"cargo run -p tool-x"));
        assert!(!c.contains(&"cargo run -p lib"));
        assert!(!c.contains(&"cargo run -p old"));
        assert!(!c.contains(&"cargo run")); // virtual workspace
        let app = t.iter().find(|t| t.command == "cargo run -p app").unwrap();
        assert_eq!(app.cwd, d.path());
        assert_eq!(app.label, "run: app");
    }

    #[test]
    fn invalid_manifest_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "Cargo.toml", "[package\nname=");
        assert!(run(d.path()).is_empty());
    }
}
