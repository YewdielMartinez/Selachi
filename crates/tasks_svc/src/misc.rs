//! Python (pyproject), Docker Compose and `.vscode/tasks.json`.

use crate::model::TaskKind;
use crate::scan::{quote_arg, truncate, Scan};
use serde_json::Value;
use std::path::Path;

// ---- Python ------------------------------------------------------------------

pub(crate) fn detect_python(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let manifest = root.join("pyproject.toml");
    let text = s.read(&manifest);
    let doc: toml::Table = text
        .as_deref()
        .and_then(|t| t.parse().ok())
        .unwrap_or_default();

    // `uv run x` / `poetry run x` / `pdm run x` when that tool manages the project.
    let runner: Option<&str> = if text.is_none() {
        None
    } else if s.exists(&root.join("uv.lock")) {
        Some("uv run")
    } else if doc.get("tool").and_then(|t| t.get("poetry")).is_some() {
        Some("poetry run")
    } else if s.exists(&root.join("pdm.lock")) {
        Some("pdm run")
    } else {
        None
    };
    let with_runner = |cmd: &str| match runner {
        Some(r) => format!("{r} {cmd}"),
        None => cmd.to_string(),
    };

    let mut scripts: Vec<(String, Option<String>)> = Vec::new();
    let table_keys = |t: Option<&toml::Value>| -> Vec<(String, Option<String>)> {
        t.and_then(toml::Value::as_table)
            .map(|t| {
                t.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string)))
                    .collect()
            })
            .unwrap_or_default()
    };
    scripts.extend(table_keys(
        doc.get("project").and_then(|p| p.get("scripts")),
    ));
    scripts.extend(table_keys(
        doc.get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("scripts")),
    ));
    for (name, entry) in scripts {
        let description = entry.map(|e| format!("entry point {e}"));
        s.push(
            TaskKind::Python,
            name.clone(),
            with_runner(&quote_arg(&name)),
            &root,
            &manifest,
            description,
        );
    }
    // poe the poet / pdm scripts.
    let tool = doc.get("tool").and_then(toml::Value::as_table);
    if let Some(poe) = tool
        .and_then(|t| t.get("poe"))
        .and_then(|p| p.get("tasks"))
        .and_then(toml::Value::as_table)
    {
        for (name, v) in poe {
            let desc = v
                .as_table()
                .and_then(|t| t.get("help"))
                .and_then(toml::Value::as_str)
                .or_else(|| v.as_str())
                .map(str::to_string);
            s.push(
                TaskKind::Python,
                name.clone(),
                format!("poe {}", quote_arg(name)),
                &root,
                &manifest,
                desc,
            );
        }
    }
    if let Some(pdm) = tool
        .and_then(|t| t.get("pdm"))
        .and_then(|p| p.get("scripts"))
        .and_then(toml::Value::as_table)
    {
        for name in pdm.keys().filter(|k| !k.starts_with('_')) {
            s.push(
                TaskKind::Python,
                name.clone(),
                format!("pdm run {}", quote_arg(name)),
                &root,
                &manifest,
                None,
            );
        }
    }

    let pytest_configured = tool
        .and_then(|t| t.get("pytest"))
        .and_then(|p| p.get("ini_options"))
        .is_some()
        || s.exists(&root.join("pytest.ini"))
        || s.read(&root.join("setup.cfg"))
            .is_some_and(|c| c.contains("[tool:pytest]"))
        || s.read(&root.join("tox.ini"))
            .is_some_and(|c| c.contains("[pytest]"));
    if pytest_configured {
        let source = if text.is_some() {
            manifest.clone()
        } else {
            root.join("pytest.ini")
        };
        let command = match runner {
            Some(r) => format!("{r} pytest"),
            None => "python -m pytest".to_string(),
        };
        s.push(TaskKind::Python, "pytest", command, &root, &source, None);
    }
}

// ---- Docker Compose ----------------------------------------------------------

const COMPOSE_FILES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

pub(crate) fn detect_docker(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let found: Vec<&str> = COMPOSE_FILES
        .iter()
        .copied()
        .filter(|n| s.exists(&root.join(n)))
        .collect();
    let Some(first) = found.first().copied() else {
        return;
    };
    let path = root.join(first);
    let services = s
        .read(&path)
        .map(|t| compose_services(&t))
        .unwrap_or_default();
    let desc = (!services.is_empty()).then(|| truncate(&services.join(", "), 100));
    for (label, args, d) in [
        ("up", "up", desc.clone()),
        ("up -d", "up -d", desc),
        ("down", "down", None),
        ("logs", "logs -f", None),
    ] {
        s.push(
            TaskKind::Docker,
            format!("compose {label}"),
            format!("docker compose {args}"),
            &root,
            &path,
            d,
        );
    }
}

/// Names under the top-level `services:` key (light line parser, no YAML crate).
pub(crate) fn compose_services(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_services = false;
    let mut indent: Option<usize> = None;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let ind = line.len() - line.trim_start().len();
        if ind == 0 {
            in_services = trimmed == "services:" || trimmed.starts_with("services: ");
            indent = None;
            continue;
        }
        if !in_services {
            continue;
        }
        let want = *indent.get_or_insert(ind);
        if ind == want {
            if let Some(name) = trimmed
                .strip_suffix(':')
                .or_else(|| trimmed.split(": ").next())
            {
                let name = name.trim_matches(['"', '\'']);
                if !name.is_empty() && !name.starts_with('-') && !name.contains(' ') {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

// ---- .vscode/tasks.json ------------------------------------------------------

pub(crate) fn detect_vscode(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let path = root.join(".vscode").join("tasks.json");
    let Some(text) = s.read(&path) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<Value>(&strip_jsonc(&text)) else {
        return;
    };
    let top = json.clone();
    let Some(tasks) = json.get("tasks").and_then(Value::as_array) else {
        return;
    };
    let root_str = root.to_string_lossy().into_owned();
    for t in tasks {
        let Some(label) = t.get("label").and_then(Value::as_str) else {
            continue;
        };
        // Windows overrides win (this app is Windows-only).
        let eff = |key: &str| -> Option<&Value> {
            t.get("windows")
                .and_then(|w| w.get(key))
                .or_else(|| t.get(key))
                .or_else(|| top.get("windows").and_then(|w| w.get(key)))
                .or_else(|| top.get(key))
        };
        let ty = eff("type").and_then(Value::as_str).unwrap_or("shell");
        if ty != "shell" {
            continue;
        }
        let Some(command) = eff("command").and_then(Value::as_str) else {
            continue;
        };
        let mut line = command.to_string();
        if let Some(args) = eff("args").and_then(Value::as_array) {
            for a in args {
                let arg = match a {
                    Value::String(x) => x.clone(),
                    Value::Object(o) => o
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    _ => continue,
                };
                line.push(' ');
                line.push_str(&shell_word(&arg));
            }
        }
        let line = line.replace("${workspaceFolder}", &root_str);
        if line.contains("${") {
            continue; // needs VS Code to resolve (input, file, env...)
        }
        let cwd = t
            .get("options")
            .and_then(|o| o.get("cwd"))
            .and_then(Value::as_str)
            .map(|c| c.replace("${workspaceFolder}", &root_str))
            .filter(|c| !c.contains("${"))
            .map(|c| {
                let p = Path::new(&c);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                }
            })
            .unwrap_or_else(|| root.clone());
        let detail = t.get("detail").and_then(Value::as_str).map(str::to_string);
        s.push(TaskKind::Custom, label, line, &cwd, &path, detail);
    }
}

/// Leave plain words alone, wrap anything with spaces or shell syntax.
fn shell_word(arg: &str) -> String {
    let plain = !arg.is_empty()
        && !arg.contains(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '&' | '|' | ';' | '<' | '>' | '(' | ')' | '`'
                )
        });
    if plain || (arg.starts_with('"') && arg.ends_with('"') && arg.len() >= 2) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "''"))
    }
}

/// JSON with comments and trailing commas (VS Code's JSONC) -> strict JSON.
pub(crate) fn strip_jsonc(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                if b[i] == '\\' && i + 1 < b.len() {
                    out.push(b[i + 1]);
                    i += 2;
                    continue;
                }
                if b[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            // Trailing comma: next significant char closes the container.
            let mut j = i + 1;
            while j < b.len() && b[j].is_whitespace() {
                j += 1;
            }
            if !matches!(b.get(j), Some('}') | Some(']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DiscoverOptions;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn run(dir: &Path, f: fn(&mut Scan<'_>)) -> Vec<crate::TaskDef> {
        let opts = DiscoverOptions::default();
        let mut s = Scan::new(dir, &opts);
        f(&mut s);
        s.finish().0
    }

    fn commands(tasks: &[crate::TaskDef]) -> Vec<&str> {
        tasks.iter().map(|t| t.command.as_str()).collect()
    }

    #[test]
    fn python_scripts_and_pytest() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "pyproject.toml",
            "[project]\nname=\"x\"\n[project.scripts]\nserve=\"x.main:run\"\n[tool.pytest.ini_options]\ntestpaths=[\"tests\"]\n",
        );
        let t = run(d.path(), detect_python);
        assert_eq!(commands(&t), ["serve", "python -m pytest"]);
        assert_eq!(t[0].description.as_deref(), Some("entry point x.main:run"));
    }

    #[test]
    fn python_runner_follows_the_project_tool() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "pyproject.toml",
            "[tool.poetry.scripts]\nserve=\"x:run\"\n[tool.pytest.ini_options]\n",
        );
        assert_eq!(
            commands(&run(d.path(), detect_python)),
            ["poetry run serve", "poetry run pytest"]
        );
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "pyproject.toml",
            "[project.scripts]\ncli=\"x:main\"\n",
        );
        write(d.path(), "uv.lock", "");
        assert_eq!(commands(&run(d.path(), detect_python)), ["uv run cli"]);
    }

    #[test]
    fn python_pytest_ini_without_pyproject_and_poe() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "pytest.ini", "[pytest]\n");
        assert_eq!(
            commands(&run(d.path(), detect_python)),
            ["python -m pytest"]
        );
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "pyproject.toml",
            "[tool.poe.tasks]\nlint=\"ruff check .\"\nfmt={cmd=\"ruff format\",help=\"Format\"}\n",
        );
        let t = run(d.path(), detect_python);
        assert_eq!(commands(&t), ["poe fmt", "poe lint"]);
        assert_eq!(t[0].description.as_deref(), Some("Format"));
    }

    #[test]
    fn python_nothing_without_config() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "pyproject.toml", "[project]\nname=\"x\"\n");
        assert!(run(d.path(), detect_python).is_empty());
    }

    #[test]
    fn compose_tasks_and_services() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "docker-compose.yml",
            "version: '3'\nservices:\n  db:\n    image: postgres\n  web:\n    build: .\n    ports:\n      - 80:80\nvolumes:\n  data:\n",
        );
        let t = run(d.path(), detect_docker);
        assert_eq!(
            commands(&t),
            [
                "docker compose up",
                "docker compose up -d",
                "docker compose down",
                "docker compose logs -f"
            ]
        );
        assert_eq!(t[0].description.as_deref(), Some("db, web"));
        assert_eq!(t[0].kind, TaskKind::Docker);
    }

    #[test]
    fn compose_services_parser() {
        assert_eq!(
            compose_services("services:\n    a:\n        image: x\n    b: {}\nx-ext:\n  c:\n"),
            ["a", "b"]
        );
        assert!(compose_services("version: 3\n").is_empty());
    }

    #[test]
    fn jsonc_stripping() {
        let src = "{\n // c\n \"a\": \"http://x // not comment\", /* b */ \"b\": [1, 2,],\n}\n";
        let v: Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
        assert_eq!(v["a"], "http://x // not comment");
        assert_eq!(v["b"][1], 2);
    }

    #[test]
    fn vscode_shell_tasks_only() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            ".vscode/tasks.json",
            r#"{
  // tasks
  "version": "2.0.0",
  "tasks": [
    { "label": "build api", "type": "shell", "command": "dotnet", "args": ["build", "src/My Api"], "detail": "Compila", },
    { "label": "watch", "command": "npm run dev", "options": { "cwd": "${workspaceFolder}/web" } },
    { "label": "tsc", "type": "typescript", "tsconfig": "tsconfig.json" },
    { "label": "proc", "type": "process", "command": "node" },
    { "label": "needs input", "type": "shell", "command": "echo ${input:name}" },
    { "label": "win", "type": "shell", "command": "ls", "windows": { "command": "dir" } },
    { "type": "shell", "command": "no label" },
  ]
}"#,
        );
        let t = run(d.path(), detect_vscode);
        let labels: Vec<&str> = t.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["build api", "watch", "win"]);
        assert_eq!(t[0].command, "dotnet build 'src/My Api'");
        assert_eq!(t[0].description.as_deref(), Some("Compila"));
        assert_eq!(t[0].kind, TaskKind::Custom);
        assert_eq!(t[1].cwd, d.path().join("web"));
        assert_eq!(t[2].command, "dir");
    }
}
