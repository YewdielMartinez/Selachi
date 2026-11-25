//! Pure detection of coding-agent CLI processes launched outside the Selachi harness.
//!
//! Given a process snapshot (`&[ProcInfo]`) and optional CPU percentages from [`crate::CpuSampler`],
//! [`find_external_agents`] walks the descendant tree of a session's root process and finds child
//! processes matching known coding-agent executables (`agy`, `codex`, `claude`, `gemini`, `opencode`,
//! plus any configured `acp:<cmd>` executables).
//!
//! No syscalls, no timers, no credential file reads: command lines are always passed through
//! `store::redact` before returning.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::runtime::{classify, RuntimeKind};
use crate::snapshot::ProcInfo;

/// Built-in agent CLI executable stems recognized as external subagents.
pub const KNOWN_AGENTS: &[&str] = &["agy", "codex", "claude", "gemini", "opencode"];

/// Selachi harness binaries: descendants behind these were launched *through* the harness,
/// not outside Selachi, so the walk stops before entering them.
const HARNESS_EXES: &[&str] = &["selachi", "selachi-agent", "selachi-mcp"];

/// Agent CLI arguments extracted from a command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedAgentArgs {
    /// Model from `--model` / `-m`.
    pub model: Option<String>,
    /// Reasoning effort from `--effort`.
    pub effort: Option<String>,
    /// Working directory if passed via `--cwd` / `-C`.
    pub cwd: Option<PathBuf>,
}

/// A coding-agent CLI process found descending from a session root outside the Selachi harness.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalAgentProc {
    /// Canonical agent id (`"agy"`, `"codex"`, `"claude"`, `"gemini"`, `"opencode"`, or custom ACP stem).
    pub agent: String,
    /// Process id.
    pub pid: u32,
    /// Parent process id.
    pub ppid: u32,
    /// Command line passed through `store::redact` (empty if unavailable).
    pub cmdline_redacted: String,
    /// Number of secret spans redacted from `cmdline_redacted`.
    pub redacted_secrets: usize,
    /// Model parsed from `--model` / `-m`, if present.
    pub model: Option<String>,
    /// Effort parsed from `--effort`, if present.
    pub effort: Option<String>,
    /// Working directory parsed from `--cwd` / `-C`, if present.
    pub cwd: Option<PathBuf>,
    /// Process creation time, if known.
    pub start_time: Option<SystemTime>,
    /// CPU usage percentage of this agent's subtree (`None` if no pid in the subtree was in `cpu_by_pid`).
    pub cpu_pct: Option<f32>,
    /// Working set (RSS) in bytes of this agent's subtree.
    pub rss_bytes: u64,
}

/// Finds known coding-agent CLI processes that descend from `root_pid` in `procs`.
///
/// * Pure and testable: takes a snapshot and optional `cpu_by_pid` map (from [`crate::CpuSampler`]).
/// * Excludes `root_pid` itself (only strict descendants).
/// * Guards against PID reuse by requiring each child to have started at or after its parent.
/// * Skips subtrees under `selachi` / `selachi-agent` / `selachi-mcp` (those are harness runs).
/// * Aggregates subtree RSS (`working_set`) and CPU across each external agent's non-agent children.
/// * Redacts command lines with `store::redact`.
pub fn find_external_agents(
    root_pid: u32,
    procs: &[ProcInfo],
    cpu_by_pid: &HashMap<u32, f32>,
) -> Vec<ExternalAgentProc> {
    find_external_agents_with(root_pid, procs, cpu_by_pid, &[], &HashSet::new())
}

/// Like [`find_external_agents`], also matching `extra_agents` (e.g. configured `acp:<cmd>` commands
/// or executable names) and skipping any process subtree whose pid is in `exclude_pids` (e.g.
/// harness-managed child threads already tracked by Selachi).
pub fn find_external_agents_with(
    root_pid: u32,
    procs: &[ProcInfo],
    cpu_by_pid: &HashMap<u32, f32>,
    extra_agents: &[&str],
    exclude_pids: &HashSet<u32>,
) -> Vec<ExternalAgentProc> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    if !by_pid.contains_key(&root_pid) {
        return Vec::new();
    }

    let mut children: HashMap<u32, Vec<&ProcInfo>> = HashMap::new();
    for p in procs {
        if p.pid != p.ppid && p.pid > 4 {
            children.entry(p.ppid).or_default().push(p);
        }
    }

    let extras = normalize_extra_agents(extra_agents);

    // 1. Walk strict descendants of `root_pid`, collecting every matched external agent.
    let mut matched_pids: HashSet<u32> = HashSet::new();
    let mut matched: Vec<(&ProcInfo, String)> = Vec::new();
    let mut visited: HashSet<u32> = HashSet::from([root_pid]);
    let mut stack: Vec<u32> = valid_children(root_pid, &by_pid, &children);

    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) || exclude_pids.contains(&pid) {
            continue;
        }
        let Some(info) = by_pid.get(&pid).copied() else {
            continue;
        };
        if is_harness_exe(info) {
            // Subtree belongs to a Selachi harness invocation, not an external launch.
            continue;
        }
        if let Some(agent) = match_agent_exe_inner(info, &extras) {
            matched_pids.insert(pid);
            matched.push((info, agent));
        }
        for child in valid_children(pid, &by_pid, &children) {
            if !visited.contains(&child) {
                stack.push(child);
            }
        }
    }

    // 2. Build results with subtree RSS/CPU (stopping at nested matched agents or excluded pids
    //    so resources are never double-counted) and redacted command lines.
    let mut out: Vec<ExternalAgentProc> = matched
        .into_iter()
        .map(|(info, agent)| {
            let (rss_bytes, cpu_pct) = subtree_resources(
                info.pid,
                &by_pid,
                &children,
                cpu_by_pid,
                &matched_pids,
                exclude_pids,
            );
            let parsed = info
                .cmdline
                .as_deref()
                .map(parse_agent_cli_args)
                .unwrap_or_default();
            let (cmdline_redacted, redacted_secrets) = match info.cmdline.as_deref() {
                Some(raw) if !raw.trim().is_empty() => {
                    let r = store::redact(raw);
                    (r.text, r.count)
                }
                _ => (String::new(), 0),
            };
            // Defensively redact parsed flag values too in case a secret was passed there.
            let model = parsed.model.map(|m| store::redact(&m).text);
            let effort = parsed.effort.map(|e| store::redact(&e).text);
            ExternalAgentProc {
                agent,
                pid: info.pid,
                ppid: info.ppid,
                cmdline_redacted,
                redacted_secrets,
                model,
                effort,
                cwd: parsed.cwd,
                start_time: info.start_time,
                cpu_pct,
                rss_bytes,
            }
        })
        .collect();

    out.sort_by_key(|p| (p.start_time, p.pid));
    out
}

/// Checks whether `proc_info` represents a known coding-agent CLI executable (or a Node/Bun/Deno/Python
/// wrapper launching one), returning its canonical agent id (`"agy"`, `"codex"`, `"claude"`,
/// `"gemini"`, `"opencode"`, or a normalized stem from `extra_agents`).
pub fn match_agent_exe(proc_info: &ProcInfo, extra_agents: &[&str]) -> Option<String> {
    let extras = normalize_extra_agents(extra_agents);
    match_agent_exe_inner(proc_info, &extras)
}

fn match_agent_exe_inner(proc_info: &ProcInfo, extras: &[String]) -> Option<String> {
    // 1. Direct match on image name (`agy.exe`, `claude.exe`, `codex.exe`, ...).
    let stem = exe_stem(&proc_info.name);
    if let Some(agent) = canonical_agent(&stem, extras) {
        return Some(agent);
    }
    // 2. Fallback to `exe_path` basename if `name` differed or was truncated.
    if let Some(path) = proc_info.exe_path.as_deref() {
        let path_stem = exe_stem(last_path_segment(path));
        if let Some(agent) = canonical_agent(&path_stem, extras) {
            return Some(agent);
        }
    }
    // 3. Script-runtime wrappers (node/bun/deno/python running e.g. `...\claude-code\cli.js`).
    //    We inspect ONLY the script path argument (first non-flag arg), never arbitrary prompt text.
    let rt = classify(&proc_info.name, None, None).kind;
    if matches!(
        rt,
        RuntimeKind::Node | RuntimeKind::Bun | RuntimeKind::Deno | RuntimeKind::Python
    ) {
        if let Some(cmd) = proc_info.cmdline.as_deref() {
            return match_runtime_script(cmd, extras);
        }
    }
    None
}

fn match_runtime_script(cmdline: &str, extras: &[String]) -> Option<String> {
    let args = split_cmdline(cmdline);
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "-e" || arg == "--eval" || arg == "-c" || arg == "-m" {
            // `-m <module>` in python may name the agent module; `-e`/`-c` is inline code.
            if arg == "-m" {
                if let Some(mod_name) = iter.next() {
                    let s = exe_stem(mod_name);
                    if let Some(agent) = canonical_agent(&s, extras) {
                        return Some(agent);
                    }
                }
            }
            return None;
        }
        if arg.starts_with('-') {
            // Runtime flags that take a separate value (e.g. `node --require x script.js`).
            if matches!(
                arg.as_str(),
                "-r" | "--require"
                    | "--import"
                    | "--loader"
                    | "--conditions"
                    | "--env-file"
                    | "--max-old-space-size"
            ) {
                let _ = iter.next();
            }
            continue;
        }
        // First non-flag token is the script path. Check its path components and file stem.
        for seg in arg.split(['\\', '/']).rev() {
            let s = script_stem(seg);
            if let Some(agent) = canonical_agent(&s, extras) {
                return Some(agent);
            }
        }
        break;
    }
    None
}

fn canonical_agent(stem: &str, extras: &[String]) -> Option<String> {
    let s = stem.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    match s.as_str() {
        "agy" | "antigravity" => return Some("agy".into()),
        "codex" => return Some("codex".into()),
        "claude" | "claude-code" => return Some("claude".into()),
        "gemini" | "gemini-cli" => return Some("gemini".into()),
        "opencode" | "open-code" => return Some("opencode".into()),
        _ => {}
    }
    extras.iter().find(|e| **e == s).cloned()
}

fn normalize_extra_agents(extra_agents: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in extra_agents {
        let trimmed = raw.trim();
        let after_acp = trimmed
            .strip_prefix("acp:")
            .or_else(|| trimmed.strip_prefix("ACP:"))
            .unwrap_or(trimmed)
            .trim();
        let tokens = split_cmdline(after_acp);
        let Some(first) = tokens.first() else {
            continue;
        };
        let stem = exe_stem(last_path_segment(first));
        if !stem.is_empty() && !out.contains(&stem) {
            out.push(stem);
        }
    }
    out
}

fn is_harness_exe(proc_info: &ProcInfo) -> bool {
    let stem = exe_stem(&proc_info.name);
    if HARNESS_EXES.contains(&stem.as_str()) {
        return true;
    }
    proc_info
        .exe_path
        .as_deref()
        .is_some_and(|p| HARNESS_EXES.contains(&exe_stem(last_path_segment(p)).as_str()))
}

fn valid_children(
    parent_pid: u32,
    by_pid: &HashMap<u32, &ProcInfo>,
    children: &HashMap<u32, Vec<&ProcInfo>>,
) -> Vec<u32> {
    let parent_start = by_pid.get(&parent_pid).and_then(|p| p.start_time);
    let Some(kids) = children.get(&parent_pid) else {
        return Vec::new();
    };
    kids.iter()
        .filter(|k| {
            // PID-reuse guard: a real child cannot have started strictly before its parent.
            // If both timestamps are known, require child >= parent; if either is unknown, reject
            // just like `actions::collect_tree`.
            matches!((parent_start, k.start_time), (Some(ps), Some(ks)) if ks >= ps)
        })
        .map(|k| k.pid)
        .collect()
}

fn subtree_resources(
    root: u32,
    by_pid: &HashMap<u32, &ProcInfo>,
    children: &HashMap<u32, Vec<&ProcInfo>>,
    cpu_by_pid: &HashMap<u32, f32>,
    matched_pids: &HashSet<u32>,
    exclude_pids: &HashSet<u32>,
) -> (u64, Option<f32>) {
    let mut rss = 0u64;
    let mut cpu_sum = 0.0f32;
    let mut any_cpu = false;
    let mut seen = HashSet::new();
    let mut stack = vec![root];

    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if pid != root && (matched_pids.contains(&pid) || exclude_pids.contains(&pid)) {
            continue;
        }
        let Some(info) = by_pid.get(&pid).copied() else {
            continue;
        };
        if pid != root && is_harness_exe(info) {
            continue;
        }
        rss = rss.saturating_add(info.working_set);
        if let Some(&c) = cpu_by_pid.get(&pid) {
            if c.is_finite() {
                cpu_sum += c;
                any_cpu = true;
            }
        }
        for child in valid_children(pid, by_pid, children) {
            if !seen.contains(&child) {
                stack.push(child);
            }
        }
    }
    (rss, any_cpu.then_some(cpu_sum))
}

/// Extracts `--model` / `-m`, `--effort`, and `--cwd` / `-C` from an agent command line.
///
/// Respects single and double quotes, ignores `argv[0]` (the executable), and stops parsing flags
/// after `--`.
pub fn parse_agent_cli_args(cmdline: &str) -> ParsedAgentArgs {
    let tokens = split_cmdline(cmdline);
    let mut out = ParsedAgentArgs::default();
    let mut i = 1; // skip executable token
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        if tok == "--" {
            break;
        }
        if let Some(val) = tok
            .strip_prefix("--model=")
            .or_else(|| tok.strip_prefix("-m="))
        {
            if !val.is_empty() {
                out.model = Some(val.to_string());
            }
        } else if tok == "--model" || tok == "-m" {
            if let Some(next) = tokens.get(i + 1) {
                if !next.is_empty() && !next.starts_with('-') {
                    out.model = Some(next.clone());
                    i += 1;
                }
            }
        } else if let Some(val) = tok.strip_prefix("--effort=") {
            if !val.is_empty() {
                out.effort = Some(val.to_string());
            }
        } else if tok == "--effort" {
            if let Some(next) = tokens.get(i + 1) {
                if !next.is_empty() && !next.starts_with('-') {
                    out.effort = Some(next.clone());
                    i += 1;
                }
            }
        } else if let Some(val) = tok
            .strip_prefix("--cwd=")
            .or_else(|| tok.strip_prefix("-C="))
        {
            if !val.is_empty() {
                out.cwd = Some(PathBuf::from(val));
            }
        } else if tok == "--cwd" || tok == "-C" {
            if let Some(next) = tokens.get(i + 1) {
                if !next.is_empty() && !next.starts_with('-') {
                    out.cwd = Some(PathBuf::from(next));
                    i += 1;
                }
            }
        }
        i += 1;
    }
    out
}

/// Splits a command line into arguments, honoring double and single quotes.
fn split_cmdline(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = cmd.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' && q == '"' && chars.peek() == Some(&'"') {
                    let _ = chars.next();
                    cur.push('"');
                } else if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    any = true;
                }
                c if c.is_whitespace() => {
                    if any {
                        out.push(std::mem::take(&mut cur));
                        any = false;
                    }
                }
                c => {
                    cur.push(c);
                    any = true;
                }
            },
        }
    }
    if any {
        out.push(cur);
    }
    out
}

fn last_path_segment(path: &str) -> &str {
    path.rsplit(['\\', '/'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}

fn exe_stem(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com", ".ps1"] {
        if let Some(s) = lower.strip_suffix(ext) {
            return s.to_string();
        }
    }
    lower
}

fn script_stem(seg: &str) -> String {
    let lower = seg.trim().to_ascii_lowercase();
    for ext in [".js", ".mjs", ".cjs", ".ts", ".py", ".exe", ".cmd", ".bat"] {
        if let Some(s) = lower.strip_suffix(ext) {
            return s.to_string();
        }
    }
    lower
}

/// Most extra agent names kept from the configuration.
pub const MAX_EXTRA_AGENTS: usize = 32;

/// Executables that must never be a configured "agent": they are shells, runtimes or system
/// processes, so listing one would flag every process of that kind as an external agent.
const NOT_AGENTS: &[&str] = &[
    "node",
    "bun",
    "deno",
    "python",
    "python3",
    "py",
    "pythonw",
    "cmd",
    "powershell",
    "pwsh",
    "bash",
    "sh",
    "wsl",
    "conhost",
    "explorer",
    "svchost",
    "git",
    "cargo",
    "rustc",
    "windowsterminal",
    "selachi",
    "selachi-agent",
    "selachi-mcp",
];

/// What `[delegation] extra_agents = ["aider", "acp:\"C:\\tools\\my-agent.exe\" --acp", ...]` means:
/// the normalized executable stems (`aider`, `my-agent`), deduplicated, without the built-in agents,
/// shells / runtimes ([`NOT_AGENTS`]) or empty entries, at most [`MAX_EXTRA_AGENTS`]. Hand the result
/// (as `&[&str]`) to [`find_external_agents_with`], or use [`find_external_agents_cfg`].
pub fn extra_agent_stems<S: AsRef<str>>(configured: &[S]) -> Vec<String> {
    let raw: Vec<&str> = configured.iter().map(AsRef::as_ref).collect();
    normalize_extra_agents(&raw)
        .into_iter()
        .filter(|s| !KNOWN_AGENTS.contains(&s.as_str()) && !NOT_AGENTS.contains(&s.as_str()))
        .take(MAX_EXTRA_AGENTS)
        .collect()
}

/// [`find_external_agents_with`] taking the configured names as they are in `config.toml`
/// (`[delegation] extra_agents`), sanitized with [`extra_agent_stems`].
pub fn find_external_agents_cfg<S: AsRef<str>>(
    root_pid: u32,
    procs: &[ProcInfo],
    cpu_by_pid: &HashMap<u32, f32>,
    configured_extra: &[S],
    exclude_pids: &HashSet<u32>,
) -> Vec<ExternalAgentProc> {
    let stems = extra_agent_stems(configured_extra);
    let refs: Vec<&str> = stems.iter().map(String::as_str).collect();
    find_external_agents_with(root_pid, procs, cpu_by_pid, &refs, exclude_pids)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn proc_entry(
        pid: u32,
        ppid: u32,
        name: &str,
        cmdline: Option<&str>,
        start_secs: u64,
        rss_mb: u64,
    ) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: name.into(),
            exe_path: None,
            cmdline: cmdline.map(str::to_string),
            start_time: Some(UNIX_EPOCH + Duration::from_secs(start_secs)),
            working_set: rss_mb * 1024 * 1024,
            private_bytes: rss_mb * 1024 * 1024,
            cpu_time: Duration::ZERO,
            threads: 1,
            access_denied: false,
        }
    }

    #[test]
    fn finds_external_agents_in_fake_snapshot_tree_with_metrics_and_redaction() {
        let procs = vec![
            // Root session: Claude Code
            proc_entry(100, 1, "claude.exe", Some("claude -p"), 10, 120),
            // Shell spawned by Claude's Bash tool
            proc_entry(
                101,
                100,
                "bash.exe",
                Some(r#"bash -c "agy -p 'check' --effort medium &""#),
                11,
                10,
            ),
            // External agy #1 with model, effort, cwd, and a secret in the command line
            proc_entry(
                102,
                101,
                "agy.exe",
                Some(
                    r#""C:\Users\u\AppData\Local\agy\bin\agy.exe" -p "review auth" --model gemini-3-pro --effort medium --cwd C:\repo --token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"#,
                ),
                12,
                200,
            ),
            // Non-agent helper spawned by agy #1 (its RSS + CPU roll up into agy #1)
            proc_entry(103, 102, "rg.exe", Some("rg foo"), 13, 30),
            // External codex #2 with -m flag
            proc_entry(
                104,
                101,
                "codex.exe",
                Some("codex -m gpt-5.1-codex --effort=high"),
                14,
                150,
            ),
            // Node-wrapped gemini CLI
            proc_entry(
                105,
                101,
                "node.exe",
                Some(
                    r#"node C:\npm\node_modules\@google\gemini-cli\dist\index.js -m=gemini-2.5-pro"#,
                ),
                15,
                90,
            ),
            // Plain node script whose prompt mentions "claude" must NOT match
            proc_entry(
                106,
                101,
                "node.exe",
                Some(r#"node C:\repo\scripts\check.js "ask claude about gemini""#),
                16,
                40,
            ),
            // Harness-launched subagent via selachi-agent: must be skipped
            proc_entry(
                107,
                101,
                "selachi-agent.exe",
                Some("selachi-agent run opencode"),
                17,
                20,
            ),
            proc_entry(108, 107, "opencode.exe", Some("opencode acp"), 18, 80),
            // Unrelated agent under a different root
            proc_entry(200, 1, "pwsh.exe", Some("pwsh"), 5, 50),
            proc_entry(201, 200, "agy.exe", Some("agy -p other"), 20, 100),
            // Stale PID reuse: claims ppid 101 but started BEFORE 101 -> ignored
            proc_entry(109, 101, "claude.exe", Some("claude"), 5, 60),
        ];

        let mut cpu = HashMap::new();
        cpu.insert(102, 12.5);
        cpu.insert(103, 2.5);
        cpu.insert(104, 4.0);

        let found = find_external_agents(100, &procs, &cpu);
        assert_eq!(
            found.len(),
            3,
            "expected agy, codex, gemini; got {found:#?}"
        );

        // 1. agy (pid 102) + rg helper (pid 103)
        let agy = &found[0];
        assert_eq!(agy.agent, "agy");
        assert_eq!(agy.pid, 102);
        assert_eq!(agy.ppid, 101);
        assert_eq!(agy.model.as_deref(), Some("gemini-3-pro"));
        assert_eq!(agy.effort.as_deref(), Some("medium"));
        assert_eq!(agy.cwd, Some(PathBuf::from(r"C:\repo")));
        assert_eq!(agy.rss_bytes, 230 * 1024 * 1024);
        assert!((agy.cpu_pct.unwrap() - 15.0).abs() < 0.01);
        assert!(
            !agy.cmdline_redacted.contains("sk-ant-api03"),
            "secret leaked: {}",
            agy.cmdline_redacted
        );
        assert!(agy.redacted_secrets >= 1);

        // 2. codex (pid 104)
        let codex = &found[1];
        assert_eq!(codex.agent, "codex");
        assert_eq!(codex.pid, 104);
        assert_eq!(codex.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(codex.effort.as_deref(), Some("high"));
        assert_eq!(codex.rss_bytes, 150 * 1024 * 1024);
        assert!((codex.cpu_pct.unwrap() - 4.0).abs() < 0.01);

        // 3. node-wrapped gemini (pid 105)
        let gemini = &found[2];
        assert_eq!(gemini.agent, "gemini");
        assert_eq!(gemini.pid, 105);
        assert_eq!(gemini.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(gemini.cpu_pct, None);
    }

    #[test]
    fn matches_all_known_and_extra_acp_agents() {
        for (exe, expected) in [
            ("agy.exe", "agy"),
            ("Antigravity.EXE", "agy"),
            ("codex.exe", "codex"),
            ("claude.exe", "claude"),
            ("gemini.cmd", "gemini"),
            ("opencode.exe", "opencode"),
        ] {
            let p = proc_entry(10, 1, exe, None, 10, 1);
            assert_eq!(match_agent_exe(&p, &[]).as_deref(), Some(expected), "{exe}");
        }
        // Custom ACP agent configured as `acp:"C:\tools\my-agent.exe" --acp`
        let custom = proc_entry(11, 1, "my-agent.exe", Some("my-agent --acp"), 10, 1);
        assert_eq!(match_agent_exe(&custom, &[]), None);
        assert_eq!(
            match_agent_exe(&custom, &[r#"acp:"C:\tools\my-agent.exe" --acp"#]).as_deref(),
            Some("my-agent")
        );
    }

    #[test]
    fn parses_model_and_effort_flags_cleanly() {
        let a = parse_agent_cli_args(
            r#"agy -p "ignore --model fake --effort high" --model claude-sonnet-4.5 --effort xhigh"#,
        );
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4.5"));
        assert_eq!(a.effort.as_deref(), Some("xhigh"));

        let b = parse_agent_cli_args(r#"codex -m "gpt 5" -- --effort low"#);
        assert_eq!(b.model.as_deref(), Some("gpt 5"));
        assert_eq!(b.effort, None, "flags after -- are positional");

        let c = parse_agent_cli_args(r#"claude --model --effort medium"#);
        assert_eq!(c.model, None, "must not swallow next flag as model");
        assert_eq!(c.effort.as_deref(), Some("medium"));
    }

    #[test]
    fn nested_external_agents_do_not_double_count_resources_and_exclude_pids_works() {
        let procs = vec![
            proc_entry(10, 1, "claude.exe", Some("claude"), 1, 100),
            proc_entry(20, 10, "agy.exe", Some("agy -p a"), 2, 100),
            proc_entry(21, 20, "codex.exe", Some("codex -p b"), 3, 50),
            proc_entry(22, 10, "opencode.exe", Some("opencode"), 4, 75),
        ];
        let mut cpu = HashMap::new();
        cpu.insert(20, 10.0);
        cpu.insert(21, 5.0);
        let excluded = HashSet::from([22]);
        let found = find_external_agents_with(10, &procs, &cpu, &[], &excluded);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].pid, 20);
        assert_eq!(found[0].rss_bytes, 100 * 1024 * 1024);
        assert!((found[0].cpu_pct.unwrap() - 10.0).abs() < 0.01);
        assert_eq!(found[1].pid, 21);
        assert_eq!(found[1].rss_bytes, 50 * 1024 * 1024);
        assert!((found[1].cpu_pct.unwrap() - 5.0).abs() < 0.01);
    }

    #[test]
    fn missing_root_returns_empty() {
        let procs = vec![proc_entry(10, 1, "agy.exe", Some("agy"), 1, 10)];
        assert!(find_external_agents(999, &procs, &HashMap::new()).is_empty());
        // Root itself is never returned even if it is an agent
        assert!(find_external_agents(10, &procs, &HashMap::new()).is_empty());
    }

    #[test]
    fn configured_extra_agents_are_sanitized_and_used() {
        let cfg = [
            "aider".to_string(),
            " Aider.EXE ".to_string(),
            r#"acp:"C:	ools\my-agent.exe" --acp"#.to_string(),
            "node".to_string(),
            "powershell.exe".to_string(),
            "codex".to_string(),
            "".to_string(),
        ];
        assert_eq!(extra_agent_stems(&cfg), ["aider", "my-agent"]);
        let many: Vec<String> = (0..100).map(|i| format!("agent{i}")).collect();
        assert_eq!(extra_agent_stems(&many).len(), MAX_EXTRA_AGENTS);

        let procs = vec![
            proc_entry(10, 1, "cmd.exe", Some("cmd"), 1, 10),
            proc_entry(20, 10, "aider.exe", Some("aider --model x"), 2, 100),
            proc_entry(21, 10, "node.exe", Some("node server.js"), 3, 50),
        ];
        let none = find_external_agents(10, &procs, &HashMap::new());
        assert!(none.is_empty());
        let found = find_external_agents_cfg(10, &procs, &HashMap::new(), &cfg, &HashSet::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent, "aider");
        assert_eq!(found[0].pid, 20);
    }
}
