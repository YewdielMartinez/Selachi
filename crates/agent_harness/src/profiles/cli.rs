//! `selachi-agent profiles ...`: list, show, dry-run, init and import-claude.
//!
//! ```text
//! selachi-agent profiles list    [--cwd DIR] [--json]
//! selachi-agent profiles show    <name> [--cwd DIR] [--json]
//! selachi-agent profiles dry-run <name> "<prompt>" [--cwd DIR] [--agent A] [--model M] [--permission P]
//!                                [--worktree] [--timeout T] [--budget-tokens N] [--json]
//! selachi-agent profiles init    [--project] [--cwd DIR] [--only NAME[,NAME]] [--force] [--dry-run]
//! selachi-agent profiles import-claude [--project] [--cwd DIR] [--from DIR] [--force] [--dry-run] [--yes]
//! ```
//!
//! `dry-run` prints the exact final prompt and the effective limits and runs nothing: no thread, no
//! checkpoint, no registry record, no agent.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{build_run_args, harness, warn_config, RunArgs};
use crate::run::RunRequest;

use super::{import, slugify, templates, Library, Listing, Profile};

/// A parsed `profiles` command.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfilesCmd {
    List {
        json: bool,
        cwd: Option<PathBuf>,
    },
    Show {
        name: String,
        json: bool,
        cwd: Option<PathBuf>,
    },
    DryRun {
        run: RunArgs,
        json: bool,
    },
    Init {
        project: bool,
        force: bool,
        dry_run: bool,
        cwd: Option<PathBuf>,
        only: Vec<String>,
    },
    ImportClaude {
        project: bool,
        force: bool,
        dry_run: bool,
        yes: bool,
        cwd: Option<PathBuf>,
        from: Option<PathBuf>,
    },
}

pub type Flags = [(String, Option<String>)];

/// Flags `profiles <action>` accepts.
fn allowed(action: &str) -> &'static [&'static str] {
    match action {
        "list" => &["--cwd", "--json"],
        "show" => &["--cwd", "--json"],
        "dry-run" => &[
            "--cwd",
            "--json",
            "--agent",
            "--model",
            "--permission",
            "--worktree",
            "--timeout",
            "--budget-tokens",
        ],
        "init" => &["--project", "--cwd", "--only", "--force", "--dry-run"],
        "import-claude" => &[
            "--project",
            "--cwd",
            "--from",
            "--force",
            "--dry-run",
            "--yes",
        ],
        _ => &[],
    }
}

/// Parse what follows `profiles` (`positional` = the action and its arguments).
pub fn parse(mut positional: Vec<String>, flags: &Flags) -> Result<ProfilesCmd, String> {
    if positional.is_empty() {
        return Err(
            "`profiles` needs an action: list, show <name>, dry-run <name> \"<prompt>\", init or import-claude"
                .into(),
        );
    }
    let action = positional.remove(0);
    let ok = allowed(&action);
    if ok.is_empty() {
        return Err(format!(
            "unknown profiles action `{action}` (list, show, dry-run, init, import-claude)"
        ));
    }
    if let Some((bad, _)) = flags.iter().find(|(f, _)| !ok.contains(&f.as_str())) {
        return Err(format!("`profiles {action}` does not take {bad}"));
    }
    let has = |n: &str| flags.iter().any(|(f, _)| f == n);
    let val = |n: &str| {
        flags
            .iter()
            .rev()
            .find(|(f, _)| f == n)
            .and_then(|(_, v)| v.clone())
    };
    let cwd = val("--cwd").map(PathBuf::from);
    Ok(match action.as_str() {
        "list" => {
            if !positional.is_empty() {
                return Err("`profiles list` takes no arguments".into());
            }
            ProfilesCmd::List {
                json: has("--json"),
                cwd,
            }
        }
        "show" => match positional.as_slice() {
            [name] => ProfilesCmd::Show {
                name: name.clone(),
                json: has("--json"),
                cwd,
            },
            [] => return Err("`profiles show` needs a profile name".into()),
            _ => {
                return Err("`profiles show` takes a single name (quote names with spaces)".into())
            }
        },
        "dry-run" => {
            if positional.len() < 2 {
                return Err(
                    "`profiles dry-run` needs a profile name and a prompt: dry-run <name> \"<prompt>\"".into(),
                );
            }
            let name = positional.remove(0);
            let prompt = positional.join(" ");
            ProfilesCmd::DryRun {
                run: build_run_args(flags, val("--agent"), Some(name), prompt)?,
                json: has("--json"),
            }
        }
        "init" => {
            if !positional.is_empty() {
                return Err(
                    "`profiles init` takes no arguments (use --only NAME to pick starters)".into(),
                );
            }
            ProfilesCmd::Init {
                project: has("--project"),
                force: has("--force"),
                dry_run: has("--dry-run"),
                cwd,
                only: val("--only")
                    .map(|v| {
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        }
        "import-claude" => {
            if !positional.is_empty() {
                return Err("`profiles import-claude` takes no arguments (use --from DIR)".into());
            }
            ProfilesCmd::ImportClaude {
                project: has("--project"),
                force: has("--force"),
                dry_run: has("--dry-run"),
                yes: has("--yes"),
                cwd,
                from: val("--from").map(PathBuf::from),
            }
        }
        _ => unreachable!("filtered above"),
    })
}

fn project_root(cwd: &Option<PathBuf>) -> Result<PathBuf, String> {
    let dir = match cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let canon = std::fs::canonicalize(&dir)
        .map_err(|e| format!("--cwd {} is not an existing directory: {e}", dir.display()))?;
    Ok(PathBuf::from(store::paths::clean_display(
        &canon.to_string_lossy(),
    )))
}

fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n {
        one
    } else {
        format!(
            "{}...",
            one.chars().take(n.saturating_sub(3)).collect::<String>()
        )
    }
}

/// The text of `profiles list`.
pub fn list_text(l: &Listing, lib: &Library) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    if l.profiles.is_empty() {
        let _ = writeln!(s, "no profiles found.");
        let _ = writeln!(s, "  global:  {}", lib.global_dir.display());
        if let Some(p) = &lib.project_dir {
            let _ = writeln!(s, "  project: {}", p.display());
        }
        let _ = writeln!(s, "`selachi-agent profiles init` writes starter profiles; `profiles import-claude` converts Claude Code subagents.");
    } else {
        let w = l
            .profiles
            .iter()
            .map(|p| p.name.chars().count())
            .max()
            .unwrap_or(4)
            .clamp(4, 28);
        let _ = writeln!(
            s,
            "{:<w$}  {:<9} {:<13} {:<5} {:<8} DESCRIPTION",
            "NAME", "AGENT", "PERMISSION", "TREE", "SCOPE"
        );
        for p in &l.profiles {
            let _ = writeln!(
                s,
                "{:<w$}  {:<9} {:<13} {:<5} {:<8} {}",
                short(&p.name, w),
                short(&p.agent, 9),
                p.permission.map(|x| x.as_str()).unwrap_or("read-only"),
                if p.worktree { "yes" } else { "-" },
                if p.shadows_global {
                    "project*"
                } else {
                    p.scope.as_str()
                },
                short(&p.description, 60)
            );
        }
        if l.profiles.iter().any(|p| p.shadows_global) {
            let _ = writeln!(
                s,
                "(* a project profile that replaces a global one of the same name)"
            );
        }
    }
    for e in &l.errors {
        let _ = writeln!(s, "error: {e}");
    }
    s
}

/// The text of `profiles show`.
pub fn show_text(p: &Profile) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{}  ({}: {})",
        p.name,
        p.scope.as_str(),
        store::paths::clean_display(&p.path.to_string_lossy())
    );
    if !p.description.is_empty() {
        let _ = writeln!(s, "{}", p.description);
    }
    let _ = writeln!(
        s,
        "agent: {}   model: {}   effort: {}",
        p.agent,
        p.model.as_deref().unwrap_or("(default)"),
        p.effort.as_deref().unwrap_or("(default)")
    );
    let _ = writeln!(
        s,
        "permission: {}   worktree: {}   timeout: {}   budget_tokens: {}",
        p.permission
            .map(|x| x.as_str())
            .unwrap_or("read-only (not set)"),
        p.worktree,
        p.timeout
            .map(|d| format!("{} s", d.as_secs()))
            .unwrap_or_else(|| "(config default)".into()),
        p.budget_tokens
            .map(|b| b.to_string())
            .unwrap_or_else(|| "-".into())
    );
    if !p.allowed_tools.is_empty() || !p.denied_tools.is_empty() {
        let _ = writeln!(
            s,
            "allowed_tools: [{}]   denied_tools: [{}]   (enforced only for Claude)",
            p.allowed_tools.join(", "),
            p.denied_tools.join(", ")
        );
    }
    if !p.context_files.is_empty() {
        let _ = writeln!(s, "context_files: {}", p.context_files.join(", "));
    }
    let _ = writeln!(
        s,
        "output_format: {}{}",
        p.output_format.as_str(),
        p.output_schema
            .as_ref()
            .map(|x| format!(" (schema: {x})"))
            .unwrap_or_default()
    );
    if p.shadows_global {
        let _ = writeln!(s, "(replaces a global profile of the same name)");
    }
    if !p.instructions.is_empty() {
        let _ = writeln!(s, "\ninstructions:\n{}", p.instructions);
    }
    s
}

fn ask(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}

fn user_claude_agents_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())
        .ok_or("cannot find your home folder; pass --from <folder with the .md subagents>")?;
    Ok(PathBuf::from(home).join(".claude").join("agents"))
}

/// Write `items` (file, text) into `dir`, creating it. Refuses to overwrite without `force`, before
/// writing anything.
fn write_all(dir: &Path, items: &[(PathBuf, String)], force: bool) -> Result<(), String> {
    let existing: Vec<String> = items
        .iter()
        .filter(|(p, _)| p.exists())
        .map(|(p, _)| p.display().to_string())
        .collect();
    if !existing.is_empty() && !force {
        return Err(format!(
            "refusing to overwrite {} (use --force to replace {})",
            existing.join(", "),
            if existing.len() == 1 { "it" } else { "them" }
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for (path, text) in items {
        std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Execute a `profiles` command. Returns the exit code.
pub fn execute(cmd: ProfilesCmd) -> Result<i32, String> {
    match cmd {
        ProfilesCmd::List { json, cwd } => {
            let h = harness(false)?;
            warn_config(&h);
            let root = project_root(&cwd)?;
            let lib = h.library(Some(&root));
            let l = lib.list();
            if json {
                let v = json!({
                    "profiles": l.profiles.iter().map(|p| p.to_value(false)).collect::<Vec<_>>(),
                    "errors": l.errors.iter().map(|e| json!({"path": store::paths::clean_display(&e.path.to_string_lossy()), "problems": e.problems})).collect::<Vec<_>>(),
                    "dirs": {
                        "global": store::paths::clean_display(&lib.global_dir.to_string_lossy()),
                        "project": lib.project_dir.as_ref().map(|d| store::paths::clean_display(&d.to_string_lossy())),
                    },
                    "enabled": lib.settings.enabled,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", list_text(&l, &lib));
            }
            Ok(0)
        }
        ProfilesCmd::Show { name, json, cwd } => {
            let h = harness(false)?;
            warn_config(&h);
            let root = project_root(&cwd)?;
            let p = h.library(Some(&root)).find(&name)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&p.to_value(true)).map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", show_text(&p));
            }
            Ok(0)
        }
        ProfilesCmd::DryRun { run, json } => {
            let h = harness(false)?;
            warn_config(&h);
            let prompt = if run.prompt.trim() == "-" {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| format!("cannot read the prompt from stdin: {e}"))?;
                s
            } else {
                run.prompt.clone()
            };
            let cwd = match run.cwd.clone() {
                Some(c) => c,
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            let mut req = RunRequest::new(run.agent.clone().unwrap_or_default(), prompt, cwd);
            req.profile = run.profile.clone();
            req.model = run.model.clone();
            req.permission = run.permission;
            req.permission_explicit = run.permission_explicit;
            req.worktree = run.worktree;
            req.timeout = run.timeout;
            req.budget_tokens = run.budget_tokens;
            match h.prepare(&req) {
                Ok(prep) => {
                    if json {
                        let mut v = prep.to_value();
                        v["dry_run"] = Value::Bool(true);
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
                        );
                    } else {
                        print!("{}", prep.to_text());
                        println!("(dry run: nothing was run)");
                    }
                    Ok(0)
                }
                Err(r) => {
                    if json {
                        let v = json!({"dry_run": true, "refused": true, "code": r.code, "reason": r.reason});
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
                        );
                    } else {
                        eprintln!("selachi-agent: refused: {}", r.reason);
                    }
                    Ok(3)
                }
            }
        }
        ProfilesCmd::Init {
            project,
            force,
            dry_run,
            cwd,
            only,
        } => {
            let h = harness(false)?;
            let dir = if project {
                store::paths::project_agent_profiles_dir(&project_root(&cwd)?)
            } else {
                store::paths::agent_profiles_dir_in(h.data_dir())
            };
            let mut chosen = templates::starters();
            if !only.is_empty() {
                let want: Vec<String> = only.iter().map(|o| o.to_lowercase()).collect();
                let known: Vec<String> = chosen.iter().map(|p| p.name.clone()).collect();
                if let Some(bad) = want.iter().find(|w| {
                    !chosen
                        .iter()
                        .any(|p| p.name.to_lowercase() == **w || slugify(&p.name) == **w)
                }) {
                    return Err(format!(
                        "no starter profile named `{bad}` (starters: {})",
                        known.join(", ")
                    ));
                }
                chosen.retain(|p| {
                    want.iter()
                        .any(|w| p.name.to_lowercase() == *w || slugify(&p.name) == *w)
                });
            }
            let items: Vec<(PathBuf, String)> = chosen
                .iter()
                .map(|p| {
                    let (file, text) = templates::render(p);
                    (dir.join(file), text)
                })
                .collect();
            if dry_run {
                for (path, text) in &items {
                    println!("--- {} ---\n{text}", path.display());
                }
                println!("(dry run: nothing was written)");
                return Ok(0);
            }
            write_all(&dir, &items, force)?;
            for (path, _) in &items {
                println!("wrote {}", path.display());
            }
            Ok(0)
        }
        ProfilesCmd::ImportClaude {
            project,
            force,
            dry_run,
            yes,
            cwd,
            from,
        } => {
            let h = harness(false)?;
            let root = if project || cwd.is_some() {
                Some(project_root(&cwd)?)
            } else {
                None
            };
            let source = match (from, &root, project) {
                (Some(f), _, _) => f,
                (None, Some(r), true) => r.join(".claude").join("agents"),
                _ => user_claude_agents_dir()?,
            };
            let target = match (&root, project) {
                (Some(r), true) => store::paths::project_agent_profiles_dir(r),
                _ => store::paths::agent_profiles_dir_in(h.data_dir()),
            };
            let plan = import::plan(&source, &target)?;
            println!(
                "Claude Code subagents in {} -> profiles in {}",
                source.display(),
                target.display()
            );
            if plan.items.is_empty() {
                for (f, why) in &plan.skipped {
                    println!("skipped {}: {why}", f.display());
                }
                println!("nothing to import");
                return Ok(0);
            }
            for (path, text, conv) in &plan.items {
                println!(
                    "\n--- {} (from {}) ---\n{text}",
                    path.display(),
                    conv.source.display()
                );
                if !conv.ignored.is_empty() {
                    println!("(not imported: {})", conv.ignored.join(", "));
                }
            }
            for (f, why) in &plan.skipped {
                println!("skipped {}: {why}", f.display());
            }
            if dry_run {
                println!("\n(dry run: nothing was written)");
                return Ok(0);
            }
            if !plan.conflicts.is_empty() && !force {
                return Err(format!(
                    "refusing to overwrite {} (use --force to replace them, or --dry-run to only preview)",
                    plan.conflicts.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(", ")
                ));
            }
            if !yes {
                let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
                if !tty {
                    return Err("import-claude needs confirmation: run it on a terminal, pass --yes, or use --dry-run to only preview".into());
                }
                let a = ask(&format!("\nWrite {} profile(s)? [y/N] ", plan.items.len()))
                    .unwrap_or_default();
                if !a.eq_ignore_ascii_case("y") && !a.eq_ignore_ascii_case("yes") {
                    println!("not written");
                    return Ok(1);
                }
            }
            let items: Vec<(PathBuf, String)> = plan
                .items
                .iter()
                .map(|(p, t, _)| (p.clone(), t.clone()))
                .collect();
            write_all(&target, &items, force)?;
            for (path, _) in &items {
                println!("wrote {}", path.display());
            }
            Ok(0)
        }
    }
}
