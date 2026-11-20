//! Runtime classification of a process from its image name / command line,
//! used by the UI to pick an icon and a short label ("node · vite").

/// Kind of runtime / tool a process belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    /// node, npm, npx, pnpm, yarn.
    Node,
    /// dotnet (run / watch / test), VBCSCompiler, MSBuild nodes.
    Dotnet,
    /// python, py, pip, uv, poetry...
    Python,
    /// cargo, rustc, rust-analyzer.
    Rust,
    /// java / javaw (gradle, maven...).
    Java,
    /// docker CLI / daemon / Docker Desktop.
    Docker,
    /// bun / bunx.
    Bun,
    /// deno.
    Deno,
    /// go, gopls, dlv, `go run` binaries.
    Go,
    /// cmd, powershell, pwsh, bash, wsl...
    Shell,
    /// Anything else.
    Other,
}

/// Classification result: the kind plus an optional framework / subcommand tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Runtime {
    /// Runtime family.
    pub kind: RuntimeKind,
    /// Short tag such as `vite`, `next`, `watch`, `uvicorn`, `gradle`.
    pub tag: Option<&'static str>,
}

impl Runtime {
    const fn new(kind: RuntimeKind, tag: Option<&'static str>) -> Self {
        Self { kind, tag }
    }
}

/// Classifies from the image name (`node.exe`), optional command line and
/// optional full image path. Matching is case-insensitive.
pub fn classify(name: &str, cmdline: Option<&str>, exe_path: Option<&str>) -> Runtime {
    let n = name.to_ascii_lowercase();
    let stem = n.strip_suffix(".exe").or_else(|| n.strip_suffix(".cmd")).unwrap_or(&n);
    let cmd = cmdline.map(str::to_ascii_lowercase).unwrap_or_default();
    let toks: Vec<&str> = tokens(&cmd);
    let has = |t: &str| toks.contains(&t);
    // Token stems: "vite.js" -> "vite", so `node_modules/vite/bin/vite.js` matches.
    let first_of = |cands: &[&'static str]| -> Option<&'static str> {
        cands.iter().copied().find(|c| toks.iter().any(|t| stem_of(t) == *c))
    };

    match stem {
        "node" | "npm" | "npx" | "pnpm" | "yarn" | "nodejs" => Runtime::new(
            RuntimeKind::Node,
            first_of(&[
                "vite", "next", "nuxt", "astro", "remix", "webpack", "storybook", "vitest", "jest",
                "nodemon", "ts-node", "tsx", "turbo", "expo", "ng",
            ]),
        ),
        "dotnet" => Runtime::new(
            RuntimeKind::Dotnet,
            ["watch", "run", "test", "build", "ef"].into_iter().find(|c| has(c)),
        ),
        "vbcscompiler" => Runtime::new(RuntimeKind::Dotnet, Some("compiler")),
        "msbuild" | "msbuildtaskhost" => Runtime::new(RuntimeKind::Dotnet, Some("msbuild")),
        s if s.starts_with("dotnet-") => Runtime::new(RuntimeKind::Dotnet, None),
        "python" | "python3" | "pythonw" | "py" | "pip" | "pip3" | "pipx" | "uv" | "uvx"
        | "poetry" | "pytest" => Runtime::new(
            RuntimeKind::Python,
            first_of(&[
                "uvicorn", "gunicorn", "flask", "manage", "jupyter", "streamlit", "pytest",
                "fastapi", "celery",
            ])
            .map(|t| if t == "manage" { "django" } else { t }),
        ),
        s if s.starts_with("python3.") => Runtime::new(RuntimeKind::Python, None),
        "cargo" | "rustc" | "rustup" | "rust-analyzer" | "rustfmt" | "clippy-driver" => {
            let tag = if stem == "rust-analyzer" {
                Some("analyzer")
            } else if stem == "rustc" {
                Some("rustc")
            } else {
                ["watch", "run", "test", "build", "check", "clippy", "bench"]
                    .into_iter()
                    .find(|c| has(c))
            };
            Runtime::new(RuntimeKind::Rust, tag)
        }
        s if s.starts_with("cargo-") => Runtime::new(
            RuntimeKind::Rust,
            Some(if s == "cargo-watch" { "watch" } else { "cargo" }),
        ),
        "java" | "javaw" => Runtime::new(
            RuntimeKind::Java,
            if cmd.contains("gradle") {
                Some("gradle")
            } else if cmd.contains("maven") || cmd.contains("mvn") {
                Some("maven")
            } else if cmd.contains("spring-boot") || cmd.contains("springframework") {
                Some("spring")
            } else {
                None
            },
        ),
        "docker" | "dockerd" | "docker-compose" | "docker desktop" | "docker-credential-desktop" => {
            Runtime::new(
                RuntimeKind::Docker,
                if stem == "docker-compose" || has("compose") { Some("compose") } else { None },
            )
        }
        s if s.starts_with("com.docker.") => Runtime::new(RuntimeKind::Docker, None),
        "bun" | "bunx" => Runtime::new(RuntimeKind::Bun, ["dev", "run", "test"].into_iter().find(|c| has(c))),
        "deno" => Runtime::new(RuntimeKind::Deno, ["task", "run", "test"].into_iter().find(|c| has(c))),
        "go" => Runtime::new(RuntimeKind::Go, ["run", "test", "build"].into_iter().find(|c| has(c))),
        "gopls" => Runtime::new(RuntimeKind::Go, Some("gopls")),
        "dlv" => Runtime::new(RuntimeKind::Go, Some("delve")),
        "cmd" | "powershell" | "pwsh" | "bash" | "sh" | "zsh" | "fish" | "nu" | "wsl" => {
            Runtime::new(RuntimeKind::Shell, None)
        }
        _ => {
            let in_go_build = exe_path
                .is_some_and(|p| p.to_ascii_lowercase().contains("\\go-build"));
            if in_go_build {
                Runtime::new(RuntimeKind::Go, Some("go-run"))
            } else {
                Runtime::new(RuntimeKind::Other, None)
            }
        }
    }
}

/// Splits a lower-cased command line into path-ish tokens.
pub(crate) fn tokens(cmd: &str) -> Vec<&str> {
    cmd.split(|c: char| c.is_whitespace() || matches!(c, '\\' | '/' | '"' | '\'' | '='))
        .filter(|t| !t.is_empty())
        .collect()
}

/// `vite.js` -> `vite`, `next.cmd` -> `next`.
fn stem_of(tok: &str) -> &str {
    for ext in [".js", ".mjs", ".cjs", ".ts", ".cmd", ".exe", ".py", ".bat"] {
        if let Some(s) = tok.strip_suffix(ext) {
            return s;
        }
    }
    tok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, cmd: &str) -> Runtime {
        classify(name, Some(cmd), None)
    }

    #[test]
    fn node_frameworks() {
        let r = c("node.exe", r#""C:\Program Files\nodejs\node.exe" C:\app\node_modules\vite\bin\vite.js --port 5173"#);
        assert_eq!(r, Runtime { kind: RuntimeKind::Node, tag: Some("vite") });
        let r = c("node.exe", r"node C:\app\node_modules\next\dist\bin\next dev");
        assert_eq!(r.tag, Some("next"));
        assert_eq!(c("node.exe", "node server.js").tag, None);
        assert_eq!(c("NODE.EXE", "node server.js").kind, RuntimeKind::Node);
    }

    #[test]
    fn other_runtimes() {
        assert_eq!(c("dotnet.exe", "dotnet watch run").tag, Some("watch"));
        assert_eq!(c("dotnet.exe", "dotnet run --project x").kind, RuntimeKind::Dotnet);
        assert_eq!(c("python.exe", "python -m uvicorn app:app").tag, Some("uvicorn"));
        assert_eq!(c("python3.12.exe", "").kind, RuntimeKind::Python);
        assert_eq!(c("cargo.exe", "cargo watch -x run").tag, Some("watch"));
        assert_eq!(c("cargo-watch.exe", "").kind, RuntimeKind::Rust);
        assert_eq!(c("java.exe", "java -jar gradle-wrapper.jar").tag, Some("gradle"));
        assert_eq!(c("docker.exe", "docker compose up").tag, Some("compose"));
        assert_eq!(c("bun.exe", "bun dev").kind, RuntimeKind::Bun);
        assert_eq!(c("deno.exe", "deno task dev").tag, Some("task"));
        assert_eq!(c("go.exe", "go run .").tag, Some("run"));
        assert_eq!(c("pwsh.exe", "").kind, RuntimeKind::Shell);
        assert_eq!(c("cmd.exe", "").kind, RuntimeKind::Shell);
        assert_eq!(c("chrome.exe", "").kind, RuntimeKind::Other);
        let r = classify("server.exe", None, Some(r"C:\Temp\go-build123\b001\exe\server.exe"));
        assert_eq!(r, Runtime { kind: RuntimeKind::Go, tag: Some("go-run") });
    }
}
