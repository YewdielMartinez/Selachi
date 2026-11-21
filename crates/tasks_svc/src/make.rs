//! Makefile targets and justfile recipes.

use crate::model::TaskKind;
use crate::scan::{truncate, Scan};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const MAKEFILES: &[&str] = &["Makefile", "makefile", "GNUmakefile"];
const JUSTFILES: &[&str] = &["justfile", "Justfile", ".justfile"];

pub(crate) fn detect(s: &mut Scan<'_>) {
    let root = s.root.to_path_buf();
    let entries = s.list_dir(&root);
    let has = |name: &str| entries.iter().any(|(n, d)| !d && n == name);

    if let Some(name) = MAKEFILES.iter().find(|n| has(n)) {
        let path = root.join(name);
        if let Some(text) = s.read(&path) {
            for (target, doc) in make_targets(&text) {
                let command = format!("make {target}");
                s.push(TaskKind::Make, target, command, &root, &path, doc);
            }
        }
    }
    if let Some(name) = JUSTFILES.iter().find(|n| has(n)) {
        let path = root.join(name);
        if let Some(text) = s.read(&path) {
            let recipes = if s.opts.use_just_cli {
                just_cli(&root, &path)
            } else {
                None
            }
            .unwrap_or_else(|| just_recipes(&text));
            for (recipe, doc) in recipes {
                let command = format!("just {recipe}");
                s.push(TaskKind::Just, recipe, command, &root, &path, doc);
            }
        }
    }
}

/// `(target, doc)` of the user-facing targets of a Makefile, in file order.
pub(crate) fn make_targets(text: &str) -> Vec<(String, Option<String>)> {
    let mut phony: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(".PHONY") {
            if let Some(rest) = rest.trim_start().strip_prefix(':') {
                phony.extend(rest.split_whitespace().map(str::to_string));
            }
        }
    }
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut prev_comment: Option<String> = None;
    for line in text.lines() {
        if line.starts_with('#') {
            let c = line.trim_start_matches('#').trim();
            prev_comment = (!c.is_empty()).then(|| c.to_string());
            continue;
        }
        let first = line.chars().next();
        if first.is_none() || first.is_some_and(char::is_whitespace) {
            if first.is_none() {
                prev_comment = None;
            }
            continue;
        }
        let comment = prev_comment.take();
        let Some(colon) = line.find(':') else {
            continue;
        };
        let after = &line[colon + 1..];
        // `A := 1`, `A ::= 1` and `A:=1` are assignments; `A = 1: x` has `=` before ':'.
        if after.starts_with(['=', ':']) || line[..colon].contains(['=', '$', '%', '(']) {
            continue;
        }
        let inline_doc = after
            .split_once("##")
            .map(|(_, d)| d.trim().to_string())
            .filter(|d| !d.is_empty());
        let doc = inline_doc.or(comment);
        for name in line[..colon].split_whitespace() {
            if name.starts_with(['.', '_']) {
                continue;
            }
            let in_phony = phony.iter().any(|p| p == name);
            // `main.o`, `out/app`: file targets, unless declared phony.
            if !in_phony && name.contains(['.', '/', '\\', '*', '$']) {
                continue;
            }
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
            {
                continue;
            }
            if out.iter().any(|(n, _)| n == name) {
                continue;
            }
            out.push((name.to_string(), doc.as_deref().map(|d| truncate(d, 120))));
        }
    }
    out
}

/// Recipes of a justfile read directly: `name args:` at column 0, public only.
pub(crate) fn just_recipes(text: &str) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut doc: Option<String> = None;
    let mut private_attr = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            doc = None;
            private_attr = false;
            continue;
        }
        if let Some(c) = trimmed.strip_prefix('#') {
            doc = Some(c.trim().to_string());
            continue;
        }
        if trimmed.starts_with('[') {
            let attr = trimmed.to_ascii_lowercase();
            if attr.contains("private") {
                private_attr = true;
            }
            continue;
        }
        if trimmed.starts_with([' ', '\t']) {
            continue; // recipe body
        }
        let is_private = std::mem::take(&mut private_attr);
        let doc_here = doc.take();
        // Settings, assignments, imports, aliases.
        let head = trimmed.split_whitespace().next().unwrap_or("");
        if matches!(
            head,
            "set" | "alias" | "export" | "import" | "mod" | "unexport"
        ) {
            // `export FOO := ..` / `set shell := [...]`
            if trimmed.contains(":=") || head != "export" {
                continue;
            }
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        if trimmed[colon + 1..].starts_with('=') {
            continue; // `name := value`
        }
        let signature = trimmed[..colon].trim_start_matches('@');
        let Some(name) = signature.split_whitespace().next() else {
            continue;
        };
        if is_private
            || name.starts_with('_')
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
        {
            continue;
        }
        if out.iter().any(|(n, _)| n == name) {
            continue;
        }
        out.push((
            name.to_string(),
            doc_here
                .filter(|d| !d.is_empty())
                .map(|d| truncate(&d, 120)),
        ));
    }
    out
}

/// `just --list --unsorted`, when `just` is installed. `None` on any problem
/// (the caller then parses the file itself).
fn just_cli(root: &Path, justfile: &Path) -> Option<Vec<(String, Option<String>)>> {
    let exe = find_in_path("just")?;
    let root = root.to_path_buf();
    let justfile = justfile.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new(exe)
            .args([
                "--list",
                "--unsorted",
                "--list-heading",
                "",
                "--list-prefix",
                "",
            ])
            .arg("--justfile")
            .arg(&justfile)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(Duration::from_secs(2)).ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_just_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Lines like `build *args # compile it` (heading and prefix already removed).
pub(crate) fn parse_just_list(text: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (sig, doc) = match line.split_once('#') {
            Some((s, d)) => (s.trim(), Some(d.trim().to_string())),
            None => (line, None),
        };
        // `mod::recipe` stays as is; the first word is the recipe.
        let Some(name) = sig.split_whitespace().next() else {
            continue;
        };
        if name.ends_with(':') {
            continue; // a module heading
        }
        out.push((
            name.to_string(),
            doc.filter(|d| !d.is_empty()).map(|d| truncate(&d, 120)),
        ));
    }
    out
}

/// Look `name` up on `PATH` (with `.exe` on Windows) without spawning anything.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DiscoverOptions;

    fn names(v: Vec<(String, Option<String>)>) -> Vec<String> {
        v.into_iter().map(|(n, _)| n).collect()
    }

    #[test]
    fn make_targets_table() {
        let mk = "\
CC := gcc
VERSION ?= 1.0
SRC = a.c b.c

.PHONY: all test clean

# Build everything
all: main.o util.o
\t$(CC) -o app $^

main.o: main.c
\t$(CC) -c $<

%.o: %.c
\t$(CC) -c $<

test: ## Run the tests
\tgo test ./...

clean:
\trm -f *.o

_internal:
\techo hidden

.DEFAULT_GOAL := all
deploy-prod docs: all
\techo $@

out/app: main.o
\techo file target
";
        let t = make_targets(mk);
        let n: Vec<&str> = t.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(n, ["all", "test", "clean", "deploy-prod", "docs"]);
        assert_eq!(t[0].1.as_deref(), Some("Build everything"));
        assert_eq!(t[1].1.as_deref(), Some("Run the tests"));
        assert_eq!(t[2].1, None);
    }

    #[test]
    fn make_phony_targets_with_dots_are_kept() {
        let t = names(make_targets(".PHONY: build.release\nbuild.release:\n\tx\n"));
        assert_eq!(t, ["build.release"]);
    }

    #[test]
    fn make_ignores_recipe_lines_and_variables() {
        let t = names(make_targets("A = x:y\nlint:\n\techo a: b\n"));
        assert_eq!(t, ["lint"]);
    }

    #[test]
    fn just_recipes_direct_parse() {
        let jf = "\
set shell := [\"pwsh\", \"-c\"]
export FOO := \"bar\"
version := \"1\"

# Build it
build:
    cargo build

[private]
helper:
    echo

_hidden:
    echo

@test +args='':
    cargo test {{args}}

alias b := build
";
        let t = just_recipes(jf);
        let n: Vec<&str> = t.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(n, ["build", "test"]);
        assert_eq!(t[0].1.as_deref(), Some("Build it"));
    }

    #[test]
    fn just_list_output_parse() {
        let out = "build # Build it\ntest *args\ndocker::up # start\n";
        let t = parse_just_list(out);
        let n: Vec<&str> = t.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(n, ["build", "test", "docker::up"]);
        assert_eq!(t[0].1.as_deref(), Some("Build it"));
    }

    #[test]
    fn detect_reads_files_without_just_cli() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Makefile"), "all:\n\techo\n").unwrap();
        std::fs::write(d.path().join("justfile"), "dev:\n    echo\n").unwrap();
        let opts = DiscoverOptions {
            use_just_cli: false,
        };
        let mut s = Scan::new(d.path(), &opts);
        detect(&mut s);
        let (tasks, deps) = s.finish();
        let c: Vec<&str> = tasks.iter().map(|t| t.command.as_str()).collect();
        assert_eq!(c, ["make all", "just dev"]);
        assert!(deps.contains(&d.path().join("Makefile")));
    }
}
