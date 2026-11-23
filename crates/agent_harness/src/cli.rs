//! `selachi-agent`: argument parsing and the commands.
//!
//! ```text
//! selachi-agent list   [--json] [--models]
//! selachi-agent run    <agent> [--cwd DIR] [--model M] [--permission read-only|ask|accept-edits]
//!                      [--worktree] [--timeout 15m] [--budget-tokens N] [--json] "<prompt>"   ("-" = stdin)
//! selachi-agent status <run_id> [--json]
//! selachi-agent result <run_id> [--json]
//! selachi-agent cancel <run_id> [--json]
//! selachi-agent diff   <run_id> [--stat]
//! selachi-agent apply  <run_id> [--dry-run] [--yes] [--json]
//! selachi-agent mcp
//! ```
//!
//! Exit codes: 0 ok, 1 the run failed, 2 usage error, 3 refused by a rule, 4 timed out, 5 cancelled,
//! 6 token budget exceeded.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::agents::list_agents;
use crate::apply::{Confirmation, Risk};
use crate::control::CancelOutcome;
use crate::output::RunStatus;
use crate::policy::Permission;
use crate::run::{Harness, RunRequest};

pub const USAGE: &str = "\
selachi-agent: delegate work to another coding agent (Claude Code, Codex, OpenCode, Gemini, Antigravity)

USAGE
  selachi-agent list   [--json] [--models]
  selachi-agent run    <agent> [options] \"<prompt>\"        (use - to read the prompt from stdin)
  selachi-agent status <run_id> [--json]
  selachi-agent result <run_id> [--json]
  selachi-agent cancel <run_id> [--json]
  selachi-agent diff   <run_id> [--stat]
  selachi-agent apply  <run_id> [--dry-run] [--yes] [--json]
  selachi-agent mcp                                        MCP server on stdio (for Claude Code, Antigravity...)

RUN OPTIONS
  --cwd <dir>               project directory (default: the current directory)
  --model <name>            model for the agent
  --permission <mode>       read-only (default) | ask | accept-edits.  Never more than the calling session
                            (SELACHI_PARENT_PERMISSION, default read-only); full is never delegable
  --worktree                run in a new git worktree + branch, apply the result later with `apply`
  --timeout <time>          90, 90s, 15m, 1h (default 15m)
  --budget-tokens <n>       stop the agent after n billable tokens
  --json                    machine-readable result

ENVIRONMENT
  SELACHI_PARENT_PERMISSION   permission of the calling session (read-only | ask | accept-edits | full)
  SELACHI_DELEGATION_DEPTH    delegation depth of the calling session (0 = top level; max 2)
  SELACHI_PARENT_THREAD       chat thread id of the calling session (links the child thread to it)
  SELACHI_PARENT_TAB          terminal tab id of the calling session
  SELACHI_DATA_DIR            data folder (default %APPDATA%\\Selachi)

EXIT CODES
  0 ok   1 run failed   2 usage error   3 refused by a rule   4 timed out   5 cancelled   6 token budget exceeded
";

/// A parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Version,
    List { json: bool, models: bool },
    Run(RunArgs),
    Status { id: String, json: bool },
    Result { id: String, json: bool },
    Cancel { id: String, json: bool },
    Diff { id: String, stat: bool },
    Apply { id: String, dry_run: bool, yes: bool, json: bool },
    /// MCP server on stdio.
    Mcp,
}

/// Arguments of `run`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunArgs {
    pub agent: String,
    /// `-` means "read stdin".
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub permission: Permission,
    pub worktree: bool,
    pub timeout: Option<Duration>,
    pub budget_tokens: Option<u64>,
    pub json: bool,
    /// Hidden: print `SELACHI_RUN_ID=<id>` on stderr as soon as the run exists (used by `mcp`).
    pub announce_run_id: bool,
}

/// `90`, `90s`, `15m`, `1h`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().map_err(|_| format!("`{s}` is not a duration (use 90, 90s, 15m or 1h)"))?;
    if n == 0 {
        return Err("the timeout must be greater than zero".into());
    }
    Ok(Duration::from_secs(n.saturating_mul(mult)))
}

/// Parse the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter().peekable();
    let Some(sub) = it.next() else { return Ok(Command::Help) };
    let sub = sub.as_str();
    if matches!(sub, "-h" | "--help" | "help") {
        return Ok(Command::Help);
    }
    if matches!(sub, "-V" | "--version" | "version") {
        return Ok(Command::Version);
    }

    // flags: `--name value` or `--name=value`; everything else is positional
    let mut positional: Vec<String> = Vec::new();
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let value_flags = ["--cwd", "--model", "--permission", "--timeout", "--budget-tokens"];
    let mut only_positional = false;
    while let Some(a) = it.next() {
        if only_positional || !a.starts_with("--") {
            positional.push(a.clone());
            continue;
        }
        if a == "--" {
            only_positional = true;
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (a.clone(), None),
        };
        if value_flags.contains(&name.as_str()) {
            let v = match inline {
                Some(v) => v,
                None => it.next().cloned().ok_or_else(|| format!("{name} needs a value"))?,
            };
            flags.push((name, Some(v)));
        } else {
            if inline.is_some() {
                return Err(format!("{name} does not take a value"));
            }
            flags.push((name, None));
        }
    }
    let has = |n: &str| flags.iter().any(|(f, _)| f == n);
    let val = |n: &str| flags.iter().rev().find(|(f, _)| f == n).and_then(|(_, v)| v.clone());
    let allowed: &[&str] = match sub {
        "list" => &["--json", "--models"],
        "run" => &["--cwd", "--model", "--permission", "--worktree", "--timeout", "--budget-tokens", "--json", "--announce-run-id"],
        "status" | "result" | "cancel" => &["--json"],
        "diff" => &["--stat"],
        "apply" => &["--dry-run", "--yes", "--json"],
        "mcp" => &[],
        other => return Err(format!("unknown command `{other}`; see `selachi-agent --help`")),
    };
    if let Some((bad, _)) = flags.iter().find(|(f, _)| !allowed.contains(&f.as_str())) {
        return Err(format!("`{sub}` does not take {bad}"));
    }
    let one_id = |positional: &[String]| -> Result<String, String> {
        match positional {
            [id] => Ok(id.clone()),
            [] => Err(format!("`{sub}` needs a run id")),
            _ => Err(format!("`{sub}` takes a single run id")),
        }
    };

    Ok(match sub {
        "list" => {
            if !positional.is_empty() {
                return Err("`list` takes no arguments".into());
            }
            Command::List { json: has("--json"), models: has("--models") }
        }
        "run" => {
            let mut p = positional.into_iter();
            let agent = p.next().ok_or("`run` needs an agent (claude, codex, opencode, gemini, agy) and a prompt")?;
            let prompt = p.collect::<Vec<_>>().join(" ");
            if prompt.trim().is_empty() {
                return Err("`run` needs a prompt (use - to read it from stdin)".into());
            }
            let permission = match val("--permission") {
                Some(v) => Permission::parse(&v).ok_or_else(|| format!("unknown permission `{v}` (read-only, ask, accept-edits)"))?,
                None => Permission::ReadOnly,
            };
            Command::Run(RunArgs {
                agent,
                prompt,
                cwd: val("--cwd").map(PathBuf::from),
                model: val("--model"),
                permission,
                worktree: has("--worktree"),
                timeout: val("--timeout").map(|v| parse_duration(&v)).transpose()?,
                budget_tokens: val("--budget-tokens")
                    .map(|v| v.replace('_', "").parse::<u64>().map_err(|_| format!("--budget-tokens `{v}` is not a number")))
                    .transpose()?,
                json: has("--json"),
                announce_run_id: has("--announce-run-id"),
            })
        }
        "mcp" => {
            if !positional.is_empty() {
                return Err("`mcp` takes no arguments (it speaks MCP on stdin/stdout)".into());
            }
            Command::Mcp
        }
        "status" => Command::Status { id: one_id(&positional)?, json: has("--json") },
        "result" => Command::Result { id: one_id(&positional)?, json: has("--json") },
        "cancel" => Command::Cancel { id: one_id(&positional)?, json: has("--json") },
        "diff" => Command::Diff { id: one_id(&positional)?, stat: has("--stat") },
        "apply" => Command::Apply { id: one_id(&positional)?, dry_run: has("--dry-run"), yes: has("--yes"), json: has("--json") },
        _ => unreachable!("filtered above"),
    })
}

fn harness(export_env: bool) -> Result<Harness, String> {
    crate::fake::apply_env(Harness::builder().export_child_env(export_env))?.build()
}

/// The harness of `run`: also announces the run id when asked to.
fn run_harness(announce: bool) -> Result<Harness, String> {
    let mut b = Harness::builder().export_child_env(true);
    if announce {
        b = b.on_started(std::sync::Arc::new(|id: &str| {
            use std::io::Write as _;
            let mut e = std::io::stderr();
            let _ = writeln!(e, "{}{id}", crate::mcp::runs::RUN_ID_MARKER);
            let _ = e.flush();
        }));
    }
    crate::fake::apply_env(b)?.build()
}

/// Run the CLI. Returns the process exit code.
pub fn run_cli(args: &[String]) -> i32 {
    let cmd = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("selachi-agent: {e}\n(see `selachi-agent --help`)");
            return 2;
        }
    };
    match cmd {
        Command::Help => {
            print!("{USAGE}");
            0
        }
        Command::Version => {
            println!("selachi-agent {}", env!("CARGO_PKG_VERSION"));
            0
        }
        other => match execute(other) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("selachi-agent: {e}");
                1
            }
        },
    }
}

fn execute(cmd: Command) -> Result<i32, String> {
    match cmd {
        Command::Help | Command::Version => Ok(0),
        Command::List { json, models } => {
            let h = harness(false)?;
            warn_config(&h);
            let list = list_agents(h.config(), h.caller(), h.usage_probe().as_ref(), h.data_dir(), models);
            if json {
                println!("{}", serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?);
            } else {
                print!("{}", list.to_text());
            }
            Ok(0)
        }
        Command::Mcp => Ok(crate::mcp::run_stdio()),
        Command::Run(a) => {
            let h = run_harness(a.announce_run_id)?;
            warn_config(&h);
            let prompt = if a.prompt.trim() == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).map_err(|e| format!("cannot read the prompt from stdin: {e}"))?;
                s
            } else {
                a.prompt.clone()
            };
            let cwd = match a.cwd.clone() {
                Some(c) => c,
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            let mut req = RunRequest::new(a.agent, prompt, cwd);
            req.model = a.model;
            req.permission = a.permission;
            req.worktree = a.worktree;
            req.timeout = a.timeout;
            req.budget_tokens = a.budget_tokens;
            let result = h.run(req);
            // not println!: when the caller (the MCP server) is gone the run is already saved, and a
            // closed pipe must not turn into a panic
            let mut out = std::io::stdout();
            let _ = if a.json { writeln!(out, "{}", result.to_json()) } else { write!(out, "{}", result.to_text()) };
            let _ = out.flush();
            Ok(result.status.exit_code())
        }
        Command::Status { id, json } => {
            let s = harness(false)?.status(&id)?;
            if json {
                println!("{}", s.to_json());
            } else {
                print!("{}", s.to_text());
            }
            Ok(0)
        }
        Command::Result { id, json } => {
            let r = harness(false)?.result(&id)?;
            if json {
                println!("{}", r.to_json());
            } else {
                print!("{}", r.to_text());
            }
            Ok(if r.status == RunStatus::Done { 0 } else { r.status.exit_code() })
        }
        Command::Cancel { id, json } => {
            let out = harness(false)?.cancel(&id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?);
            } else {
                println!(
                    "{}",
                    match &out {
                        CancelOutcome::AlreadyFinished { status } => format!("run {id} had already finished ({})", status.as_str()),
                        CancelOutcome::Cancelled => format!("run {id} cancelled"),
                        CancelOutcome::Killed => format!("run {id} cancelled (its runner had to be killed)"),
                    }
                );
            }
            Ok(0)
        }
        Command::Diff { id, stat } => {
            let h = harness(false)?;
            let diff = h.diff(&id)?;
            if stat {
                println!("{}", crate::output::diff_stat_of(&diff).text);
            } else {
                // what is shown is redacted; `apply` uses the exact patch
                print!("{}", store::redact(&diff).text);
            }
            Ok(0)
        }
        Command::Apply { id, dry_run, yes, json } => apply_command(&harness(false)?, &id, dry_run, yes, json),
    }
}

fn warn_config(h: &Harness) {
    if let Some(w) = h.config_warning() {
        eprintln!("selachi-agent: warning: {w}");
    }
}

fn ask(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}

fn apply_command(h: &Harness, id: &str, dry_run: bool, yes: bool, json: bool) -> Result<i32, String> {
    let mut plan = h.apply_plan(id)?;
    if dry_run {
        plan.dry_run = true;
        if json {
            println!("{}", plan.to_json());
        } else {
            print!("{}", plan.to_text());
            println!("(dry run: nothing was changed)");
        }
        return Ok(0);
    }
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let confirmation = if yes {
        if plan.risk == Risk::Full {
            return Err(format!(
                "--yes is not allowed for a full-risk apply ({}); run it on a terminal and confirm there",
                plan.risk_reasons.join("; ")
            ));
        }
        Confirmation::Flag
    } else if tty {
        print!("{}", plan.to_text());
        if !plan.can_apply {
            return Err("this plan cannot be applied (see above)".into());
        }
        let (question, expected) = if plan.risk == Risk::Full {
            ("This is a FULL-risk apply. Type `apply` to continue: ", "apply")
        } else {
            ("Apply these changes to the project? [y/N] ", "y")
        };
        let answer = ask(question).unwrap_or_default();
        if !answer.eq_ignore_ascii_case(expected) && !(expected == "y" && answer.eq_ignore_ascii_case("yes")) {
            println!("not applied");
            return Ok(1);
        }
        Confirmation::Interactive
    } else {
        return Err(
            "apply needs confirmation: run it on a terminal, pass --yes (normal-risk changes only), or use --dry-run to see the plan"
                .into(),
        );
    };
    let report = h.apply(id, confirmation)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    } else {
        println!("applied {} file(s) to {}", report.files.len(), report.target);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Command, String> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn run_command_line() {
        let args: Vec<String> = [
            "run", "codex", "--cwd", "C:/p", "--permission=accept-edits", "--worktree", "--timeout", "5m", "--budget-tokens", "50_000",
            "--json", "revisa", "los", "tests",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let Command::Run(a) = parse(&args).unwrap() else { panic!("not run") };
        assert_eq!(a.agent, "codex");
        assert_eq!(a.prompt, "revisa los tests");
        assert_eq!(a.cwd, Some(PathBuf::from("C:/p")));
        assert_eq!(a.permission, Permission::AcceptEdits);
        assert!(a.worktree && a.json);
        assert_eq!(a.timeout, Some(Duration::from_secs(300)));
        assert_eq!(a.budget_tokens, Some(50_000));
    }

    #[test]
    fn defaults_and_errors() {
        let Command::Run(a) = p("run claude hola").unwrap() else { panic!() };
        assert_eq!((a.permission, a.worktree, a.timeout, a.model), (Permission::ReadOnly, false, None, None));
        assert!(p("run codex").is_err(), "a prompt is required");
        assert!(p("run").is_err());
        assert!(p("run codex --permission root x").is_err());
        assert!(p("run codex --bogus x").unwrap_err().contains("--bogus"));
        assert!(p("status").is_err());
        assert!(p("status 1 2").is_err());
        assert!(p("frobnicate").is_err());
        assert!(p("list extra").is_err());
        assert_eq!(p("").unwrap(), Command::Help);
        assert_eq!(p("--help").unwrap(), Command::Help);
        assert_eq!(p("list --json --models").unwrap(), Command::List { json: true, models: true });
        assert_eq!(p("apply 7 --dry-run").unwrap(), Command::Apply { id: "7".into(), dry_run: true, yes: false, json: false });
        assert_eq!(p("diff 3 --stat").unwrap(), Command::Diff { id: "3".into(), stat: true });
    }

    #[test]
    fn double_dash_ends_flags() {
        let args: Vec<String> = ["run", "codex", "--", "--json", "is", "text"].iter().map(|s| s.to_string()).collect();
        let Command::Run(a) = parse(&args).unwrap() else { panic!() };
        assert_eq!(a.prompt, "--json is text");
        assert!(!a.json);
    }
}
