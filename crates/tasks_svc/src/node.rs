//! package.json scripts (npm / pnpm / yarn / bun) and workspace packages.

use crate::model::TaskKind;
use crate::scan::{expand_dirs, truncate, Scan};
use serde_json::Value;
use std::path::Path;

/// Workspace packages inspected at most.
const MAX_PACKAGES: usize = 64;

pub(crate) fn detect(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let manifest = root.join("package.json");
    let Some(text) = s.read(&manifest) else {
        return;
    };
    let kind = package_manager(s, &root);
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    push_scripts(s, kind, &root, &manifest, &json, None);

    // Workspaces: package.json `workspaces` (array or { packages }) and pnpm-workspace.yaml.
    let mut patterns = workspace_patterns(&json);
    if let Some(yaml) = s.read(&root.join("pnpm-workspace.yaml")) {
        patterns.extend(pnpm_workspace_patterns(&yaml));
    }
    if patterns.is_empty() {
        return;
    }
    for dir in expand_dirs(s, &patterns, MAX_PACKAGES) {
        let pkg_manifest = dir.join("package.json");
        let Some(text) = s.read(&pkg_manifest) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let name = pkg
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        push_scripts(s, kind, &dir, &pkg_manifest, &pkg, Some(&name));
    }
}

fn package_manager(s: &mut Scan<'_>, root: &Path) -> TaskKind {
    if s.exists(&root.join("pnpm-lock.yaml")) {
        TaskKind::Pnpm
    } else if s.exists(&root.join("yarn.lock")) {
        TaskKind::Yarn
    } else if s.exists(&root.join("bun.lockb")) || s.exists(&root.join("bun.lock")) {
        TaskKind::Bun
    } else {
        TaskKind::Npm
    }
}

fn push_scripts(
    s: &mut Scan<'_>,
    kind: TaskKind,
    dir: &Path,
    manifest: &Path,
    json: &Value,
    package: Option<&str>,
) {
    let Some(scripts) = json.get("scripts").and_then(Value::as_object) else {
        return;
    };
    let runner = kind.as_str();
    for (name, body) in scripts {
        // `prebuild` / `postinstall` hooks run implicitly; not tasks of their own.
        if (name.starts_with("pre") || name.starts_with("post"))
            && scripts.contains_key(name.trim_start_matches("pre").trim_start_matches("post"))
        {
            continue;
        }
        let label = match package {
            Some(p) => format!("{p}: {name}"),
            None => name.clone(),
        };
        let command = if name.contains(|c: char| c.is_whitespace()) {
            format!("{runner} run \"{name}\"")
        } else {
            format!("{runner} run {name}")
        };
        let description = body.as_str().map(|b| truncate(b, 120));
        s.push(kind, label, command, dir, manifest, description);
    }
}

fn workspace_patterns(json: &Value) -> Vec<String> {
    let ws = json.get("workspaces");
    let list = match ws {
        Some(Value::Array(a)) => Some(a),
        Some(Value::Object(o)) => o.get("packages").and_then(Value::as_array),
        _ => None,
    };
    list.map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// `packages:` list of pnpm-workspace.yaml (`  - 'apps/*'`).
fn pnpm_workspace_patterns(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_packages = false;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            in_packages = trimmed.starts_with("packages:");
            continue;
        }
        if in_packages {
            if let Some(item) = trimmed.strip_prefix('-') {
                let item = item.trim().trim_matches(['\'', '"']);
                if !item.is_empty() {
                    out.push(item.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DiscoverOptions;
    use crate::scan::Scan;

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

    #[test]
    fn scripts_and_package_manager_from_lockfile() {
        for (lock, cmd, kind) in [
            ("package-lock.json", "npm run dev", TaskKind::Npm),
            ("pnpm-lock.yaml", "pnpm run dev", TaskKind::Pnpm),
            ("yarn.lock", "yarn run dev", TaskKind::Yarn),
            ("bun.lockb", "bun run dev", TaskKind::Bun),
            ("bun.lock", "bun run dev", TaskKind::Bun),
        ] {
            let d = tempfile::tempdir().unwrap();
            write(
                d.path(),
                "package.json",
                r#"{"scripts":{"dev":"vite","build":"vite build"}}"#,
            );
            write(d.path(), lock, "");
            let tasks = run(d.path());
            assert_eq!(tasks.len(), 2, "{lock}");
            let dev = tasks.iter().find(|t| t.label == "dev").unwrap();
            assert_eq!(dev.command, cmd, "{lock}");
            assert_eq!(dev.kind, kind);
            assert_eq!(dev.description.as_deref(), Some("vite"));
            assert_eq!(dev.cwd, d.path());
        }
    }

    #[test]
    fn pnpm_wins_over_others_and_default_is_npm() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"a":"x"}}"#);
        write(d.path(), "yarn.lock", "");
        write(d.path(), "pnpm-lock.yaml", "");
        assert_eq!(run(d.path())[0].kind, TaskKind::Pnpm);
    }

    #[test]
    fn pre_post_hooks_are_hidden_when_their_target_exists() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "package.json",
            r#"{"scripts":{"build":"a","prebuild":"b","postbuild":"c","predeploy":"d"}}"#,
        );
        let labels: Vec<_> = run(d.path()).into_iter().map(|t| t.label).collect();
        assert_eq!(labels, ["build", "predeploy"]);
    }

    #[test]
    fn script_names_with_spaces_are_quoted() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"my task":"x"}}"#);
        assert_eq!(run(d.path())[0].command, "npm run \"my task\"");
    }

    #[test]
    fn workspaces_array_object_and_pnpm_yaml() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "package.json",
            r#"{"name":"root","workspaces":["packages/*"],"scripts":{"lint":"eslint ."}}"#,
        );
        write(
            d.path(),
            "packages/web/package.json",
            r#"{"name":"@acme/web","scripts":{"dev":"vite"}}"#,
        );
        write(
            d.path(),
            "packages/api/package.json",
            r#"{"scripts":{"start":"node ."}}"#,
        );
        write(d.path(), "packages/docs/readme.md", "");
        write(
            d.path(),
            "packages/node_modules/x/package.json",
            r#"{"scripts":{"nope":"x"}}"#,
        );
        let tasks = run(d.path());
        let labels: Vec<_> = tasks.iter().map(|t| t.label.as_str()).collect();
        assert!(labels.contains(&"lint"));
        assert!(labels.contains(&"@acme/web: dev"));
        assert!(labels.contains(&"api: start"));
        assert!(!labels.iter().any(|l| l.contains("nope")));
        let web = tasks.iter().find(|t| t.label == "@acme/web: dev").unwrap();
        assert_eq!(web.cwd, d.path().join("packages/web"));
        assert_eq!(web.command, "npm run dev");

        // { "workspaces": { "packages": [...] } }
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "package.json",
            r#"{"workspaces":{"packages":["apps/*"]}}"#,
        );
        write(
            d.path(),
            "apps/a/package.json",
            r#"{"name":"a","scripts":{"x":"1"}}"#,
        );
        assert_eq!(run(d.path())[0].label, "a: x");

        // pnpm-workspace.yaml
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"name":"r"}"#);
        write(d.path(), "pnpm-lock.yaml", "");
        write(
            d.path(),
            "pnpm-workspace.yaml",
            "packages:\n  - 'apps/*'\n  - \"!apps/legacy\"\n# c\n",
        );
        write(
            d.path(),
            "apps/site/package.json",
            r#"{"name":"site","scripts":{"build":"next build"}}"#,
        );
        let t = run(d.path());
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].command, "pnpm run build");
        assert_eq!(t[0].label, "site: build");
    }

    #[test]
    fn broken_json_and_bom_are_tolerated() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", "{ not json");
        assert!(run(d.path()).is_empty());
        write(
            d.path(),
            "package.json",
            "\u{feff}{\"scripts\":{\"a\":\"b\"}}",
        );
        assert_eq!(run(d.path()).len(), 1);
    }
}
