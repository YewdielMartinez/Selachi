//! How the commands of a delegated agent are approved (PLAN section 20, ola 8D).
//!
//! Three modes, from the most careful to the most permissive:
//!
//! | mode | what happens to a request of the child |
//! |---|---|
//! | `ask` (default) | everything goes to the attention queue of a Selachi window |
//! | `allowlist` | a shell command that matches one of the profile's `allowed_commands` is approved on its own; the rest goes to the queue |
//! | `auto` | nothing is asked: everything is approved on its own |
//!
//! The effective mode of a run is the **minimum** of `[delegation] approvals`, the profile's
//! `approvals` and the request's (`--approvals`, MCP `approvals`): a profile or a run can only lower
//! it. The one exception is the user pressing "No volver a preguntar en esta ejecución" in the
//! attention queue, which switches that single run to `auto` (that is the user deciding, not a
//! configuration).
//!
//! # The hard blocklist
//!
//! Checked **first, in every mode**, before anything is asked or approved: a command in it is denied
//! on the spot, without asking, with a reason that goes into the run's `notes`:
//!
//! * `git push`, `git commit`, `git stash`, `git clean`, `git reset --hard`, `git checkout --` (or
//!   `.` / `-f`);
//! * `rm -rf` / `Remove-Item -Recurse` / `rd /s` outside the project (a path that cannot be resolved,
//!   such as `$env:X`, counts as outside), and `del /s`;
//! * `format`, `diskpart` and friends, `reg add|delete|import|copy|restore`, registry cmdlets on
//!   `HKLM:` / `HKCU:`, `cmdkey`, `vaultcmd`, encoded PowerShell commands;
//! * any argument that names a credential-looking file (`.env`, `.npmrc`, `.netrc`, `.git-credentials`,
//!   `.ssh\*`, `.aws\*`, `id_rsa`, `*.pem`, `auth.json`...).
//!
//! The command line is split into segments at `;`, `|`, `&`, new lines, parentheses and backticks, and
//! wrappers (`cmd /c`, `powershell -Command`, `bash -c`, `env`, `sudo`...) are looked through, so
//! `cd x && git push` and `cmd /c "git push"` are caught. This is a seat belt for an agent that is
//! being helpful, not a sandbox for a hostile one: the Job Object, the permission mode and the
//! worktree are the containment.
//!
//! # The allowlist
//!
//! Patterns are plain globs over the whole command line (`*` any text, `?` one character, case
//! insensitive, runs of spaces count as one): `cargo *`, `git diff *`, `rg *`, `git status`. A trailing
//! ` *` also matches the bare command. A command that contains `;`, `|`, `&`, `>`, `<`, a backtick,
//! `$(` or a line break **never** matches: it goes to the queue.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a run's approvals are handled. Ordered by permissiveness: `Ask < Allowlist < Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Ask,
    Allowlist,
    Auto,
}

impl ApprovalMode {
    pub const ALL: [ApprovalMode; 3] = [
        ApprovalMode::Ask,
        ApprovalMode::Allowlist,
        ApprovalMode::Auto,
    ];

    /// Accepts `ask`, `allowlist` (also `allow-list`, `allow_list`), `auto`; case-insensitive.
    pub fn parse(s: &str) -> Option<ApprovalMode> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '-'], "")
            .as_str()
        {
            "ask" => Some(ApprovalMode::Ask),
            "allowlist" => Some(ApprovalMode::Allowlist),
            "auto" => Some(ApprovalMode::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalMode::Ask => "ask",
            ApprovalMode::Allowlist => "allowlist",
            ApprovalMode::Auto => "auto",
        }
    }
}

impl fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ApprovalMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ApprovalMode::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown approvals mode {s:?} (ask, allowlist or auto)"
            ))
        })
    }
}

/// `min(global, profile, request)`: nobody can raise what another party lowered.
pub fn effective_mode(
    global: ApprovalMode,
    profile: Option<ApprovalMode>,
    request: Option<ApprovalMode>,
) -> ApprovalMode {
    [profile, request]
        .into_iter()
        .flatten()
        .fold(global, ApprovalMode::min)
}

/// What to do with one request of the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// On the hard blocklist: denied without asking; the text says why.
    Block(String),
    /// Approve it now, without asking.
    Auto,
    /// Send it to the attention queue.
    Ask,
}

/// Where the child works, to judge paths.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    /// Directory the agent runs in (relative paths resolve against it).
    pub cwd: &'a Path,
    /// Places `rm -rf` may touch: the project and the run's own worktree.
    pub roots: &'a [&'a Path],
}

/// Decide a request: blocklist first, then the mode.
pub fn gate(
    mode: ApprovalMode,
    allowed: &[String],
    tool: &str,
    detail: &str,
    scope: Scope<'_>,
) -> Gate {
    if let Some(reason) = blocked(tool, detail, scope) {
        return Gate::Block(reason);
    }
    match mode {
        ApprovalMode::Auto => Gate::Auto,
        ApprovalMode::Allowlist => match shell_command(tool, detail) {
            Some(cmd) if command_allowed(&cmd, allowed) => Gate::Auto,
            _ => Gate::Ask,
        },
        ApprovalMode::Ask => Gate::Ask,
    }
}

// ---------------------------------------------------------------------------
// Which text is the command
// ---------------------------------------------------------------------------

/// Titles / tool names of the agents' shell tools (lowercase). Claude: `Bash`, `PowerShell`; Codex:
/// `Command`; ACP agents use their own titles.
const SHELL_TOOLS: &[&str] = &[
    "bash",
    "powershell",
    "shell",
    "command",
    "execute",
    "exec",
    "exec_command",
    "run_command",
    "run_shell_command",
    "local_shell",
    "terminal",
];

fn shell_title(title: &str) -> bool {
    let t = title.trim().to_ascii_lowercase();
    SHELL_TOOLS.contains(&t.as_str())
        || t.contains("shell command")
        || t.contains("run command")
        || t.contains("execute command")
        || t.contains("terminal command")
}

/// The shell command a request is about, when it is one. Agents word it differently: Claude sends
/// tool `Bash` and the command as detail, Codex `Command` and the command, ACP agents a title and
/// `"<title>: <command>"`.
pub fn shell_command(tool: &str, detail: &str) -> Option<String> {
    let tool_t = tool.trim();
    let mut d = detail.trim();
    if d.is_empty() {
        return None;
    }
    // `title: command`
    if let Some(rest) = d.strip_prefix(tool_t).and_then(|r| r.strip_prefix(": ")) {
        if shell_title(tool_t) || rest.trim() == tool_t {
            d = rest.trim();
            return (!d.is_empty()).then(|| d.to_string());
        }
    }
    // `Bash: command` inside the detail
    if let Some((head, rest)) = d.split_once(": ") {
        if SHELL_TOOLS.contains(&head.trim().to_ascii_lowercase().as_str()) {
            return Some(rest.trim().to_string()).filter(|r| !r.is_empty());
        }
    }
    shell_title(tool_t).then(|| d.to_string())
}

// ---------------------------------------------------------------------------
// The allowlist
// ---------------------------------------------------------------------------

fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does `cmd` contain something that chains, redirects or substitutes? Such a command never matches
/// the allowlist.
pub fn has_shell_meta(cmd: &str) -> bool {
    cmd.contains(['\n', '\r', ';', '|', '&', '>', '<', '`']) || cmd.contains("$(")
}

/// Plain glob: `*` any run (also empty), `?` one character; case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // iterative matcher with backtracking to the last `*`
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Is `cmd` approved by one of `patterns`?
pub fn command_allowed(cmd: &str, patterns: &[String]) -> bool {
    // meta characters are looked for in the original text: collapsing spaces would hide a line break
    if has_shell_meta(cmd) {
        return false;
    }
    let cmd = collapse_spaces(cmd);
    if cmd.is_empty() || cmd.ends_with('…') {
        // a command that was cut short is not the command that will run
        return false;
    }
    patterns.iter().any(|p| {
        let p = collapse_spaces(p);
        if p.is_empty() {
            return false;
        }
        if glob_match(&p, &cmd) {
            return true;
        }
        // `cargo *` also allows plain `cargo`
        p.strip_suffix(" *")
            .is_some_and(|base| !base.contains(['*', '?']) && base.eq_ignore_ascii_case(&cmd))
    })
}

/// Why an `allowed_commands` entry cannot be used, if it cannot.
pub fn pattern_problem(p: &str) -> Option<String> {
    let t = p.trim();
    if t.is_empty() || t.chars().count() > 200 || t.chars().any(|c| c.is_control()) {
        return Some("must be a command pattern (1-200 characters, no line breaks)".into());
    }
    if has_shell_meta(t) {
        return Some(
            "must not contain `;`, `|`, `&`, `>`, `<`, a backtick or `$(`: a command that chains or redirects never matches the allowlist"
                .into(),
        );
    }
    if t.trim_matches(['*', '?', ' ']).is_empty() {
        return Some("matches every command; name the program (e.g. `cargo *`)".into());
    }
    None
}

// ---------------------------------------------------------------------------
// The hard blocklist
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Tok {
    text: String,
    quoted: bool,
}

/// Splits a command line into segments of tokens (quotes respected, backslashes are path
/// separators, not escapes).
fn segments(cmd: &str) -> Vec<Vec<Tok>> {
    let mut out: Vec<Vec<Tok>> = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    let mut tok = String::new();
    let mut quoted = false;
    let mut in_tok = false;
    let mut quote: Option<char> = None;
    let flush = |tok: &mut String, quoted: &mut bool, in_tok: &mut bool, cur: &mut Vec<Tok>| {
        if *in_tok {
            cur.push(Tok {
                text: std::mem::take(tok),
                quoted: *quoted,
            });
        }
        *quoted = false;
        *in_tok = false;
    };
    for c in cmd.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                tok.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                quoted = true;
                in_tok = true;
            }
            c if c.is_whitespace() && c != '\n' && c != '\r' => {
                flush(&mut tok, &mut quoted, &mut in_tok, &mut cur)
            }
            ';' | '|' | '&' | '\n' | '\r' | '(' | ')' | '`' | '{' | '}' => {
                flush(&mut tok, &mut quoted, &mut in_tok, &mut cur);
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => {
                tok.push(c);
                in_tok = true;
            }
        }
    }
    flush(&mut tok, &mut quoted, &mut in_tok, &mut cur);
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `C:\Program Files\Git\bin\git.exe` -> `git`.
fn program_stem(token: &str) -> String {
    let low = token.trim().to_lowercase();
    let name = low.rsplit(['/', '\\']).next().unwrap_or(&low);
    for ext in [".exe", ".cmd", ".bat", ".com", ".ps1"] {
        if let Some(s) = name.strip_suffix(ext) {
            return s.to_string();
        }
    }
    name.to_string()
}

const WRAPPERS: &[&str] = &[
    "cmd",
    "powershell",
    "pwsh",
    "bash",
    "sh",
    "zsh",
    "wsl",
    "env",
    "sudo",
    "call",
    "start",
    "time",
    "nohup",
    "command",
    "exec",
    "iex",
    "invoke-expression",
    "invoke-command",
    "icm",
];

const WRAPPER_FLAGS: &[&str] = &[
    "/c",
    "/k",
    "/s",
    "/q",
    "-c",
    "-lc",
    "-ic",
    "-command",
    "-noprofile",
    "-noninteractive",
    "-nologo",
    "-executionpolicy",
    "-ep",
    "bypass",
    "unrestricted",
    "remotesigned",
    "--",
    "-e",
    "-u",
];

fn is_env_assign(t: &str) -> bool {
    match t.split_once('=') {
        Some((k, _)) => {
            !k.is_empty()
                && !k.starts_with(|c: char| c.is_ascii_digit())
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

const MAX_NESTING: usize = 4;

/// The reason, if any request text hits the hard blocklist.
pub fn blocked(tool: &str, detail: &str, scope: Scope<'_>) -> Option<String> {
    // The command can be in either field; both are scanned as command lines and for credential
    // paths. A path such as `C:\x\git push.txt` does not start a `git` command, so scanning
    // everything costs almost nothing in false positives.
    let mut texts: Vec<&str> = Vec::with_capacity(2);
    if !detail.trim().is_empty() {
        texts.push(detail);
    }
    if !tool.trim().is_empty() {
        texts.push(tool);
    }
    for text in texts {
        if let Some(r) = blocked_line(text, scope, 0) {
            return Some(r);
        }
    }
    None
}

fn blocked_line(line: &str, scope: Scope<'_>, depth: usize) -> Option<String> {
    for seg in segments(line) {
        if let Some(r) = blocked_segment(&seg, scope, depth) {
            return Some(r);
        }
    }
    None
}

fn blocked_segment(toks: &[Tok], scope: Scope<'_>, depth: usize) -> Option<String> {
    // credential-looking paths anywhere in the segment
    for t in toks {
        if let Some(name) = credential_path(&t.text) {
            return Some(format!(
                "it touches {name}, which looks like a credentials file; delegated agents never get those"
            ));
        }
    }
    // look through wrappers
    let mut i = 0;
    while i < toks.len() {
        let low = toks[i].text.to_lowercase();
        let stem = program_stem(&low);
        if is_env_assign(&toks[i].text)
            || WRAPPERS.contains(&stem.as_str())
            || (i > 0 && WRAPPER_FLAGS.contains(&low.as_str()))
        {
            i += 1;
        } else {
            break;
        }
    }
    // `-EncodedCommand` hides the command from every check: refuse it
    if toks.iter().any(|t| {
        matches!(
            t.text.to_lowercase().as_str(),
            "-encodedcommand" | "-enc" | "-ec"
        )
    }) {
        return Some("an encoded PowerShell command cannot be inspected".into());
    }
    let rest = &toks[i..];
    let first = rest.first()?;
    // `bash -c "git push"`: a single quoted argument after a wrapper is a command line of its own
    if i > 0 && rest.len() == 1 && first.quoted && first.text.contains(char::is_whitespace) {
        return (depth < MAX_NESTING)
            .then(|| blocked_line(&first.text, scope, depth + 1))
            .flatten();
    }
    let program = program_stem(&first.text);
    let args: Vec<String> = rest[1..].iter().map(|t| t.text.to_lowercase()).collect();
    blocked_program(&program, &args, scope)
}

fn blocked_program(program: &str, args: &[String], scope: Scope<'_>) -> Option<String> {
    match program {
        "git" => blocked_git(args),
        "rm" | "remove-item" | "ri" | "rd" | "rmdir" => {
            blocked_recursive_delete(program, args, scope)
        }
        "del" | "erase" => args
            .iter()
            .any(|a| a == "/s")
            .then(|| "`del /s` deletes recursively".to_string()),
        "format" | "format-volume" | "diskpart" | "clear-disk" | "initialize-disk"
        | "remove-partition" | "mkfs" => {
            Some(format!("`{program}` can destroy a disk or a volume"))
        }
        "cmdkey" | "vaultcmd" => Some(format!("`{program}` manages stored credentials")),
        "reg" => match args.first().map(String::as_str) {
            Some("add" | "delete" | "import" | "copy" | "restore" | "load" | "unload") => {
                Some(format!("`reg {}` changes the Windows registry", args[0]))
            }
            _ => None,
        },
        "set-itemproperty"
        | "sp"
        | "new-itemproperty"
        | "remove-itemproperty"
        | "new-item"
        | "set-item" => args
            .iter()
            .any(|a| is_registry_path(a))
            .then(|| "it changes the Windows registry".to_string()),
        _ => None,
    }
}

fn is_registry_path(a: &str) -> bool {
    let a = a.trim_start_matches(['"', '\'']);
    a.starts_with("hklm:")
        || a.starts_with("hkcu:")
        || a.starts_with("hkcr:")
        || a.starts_with("hkey_")
        || a.starts_with("registry::")
}

fn blocked_git(args: &[String]) -> Option<String> {
    let mut i = 0;
    // global options before the subcommand
    while i < args.len() && args[i].starts_with('-') {
        // (the arguments are lowercased, so `-C` arrives as `-c`)
        let takes_value = matches!(
            args[i].as_str(),
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
        );
        i += if takes_value { 2 } else { 1 };
    }
    let sub = args.get(i)?.as_str();
    let rest = &args[(i + 1).min(args.len())..];
    let why = match sub {
        "push" => "`git push` publishes work; the user does that",
        "commit" => "`git commit` is up to the user",
        "stash" => "`git stash` hides the user's uncommitted work",
        "clean" => "`git clean` deletes untracked files",
        "reset" if rest.iter().any(|a| a == "--hard") => "`git reset --hard` discards work",
        "checkout"
            if rest
                .iter()
                .any(|a| a == "--" || a == "." || a == "-f" || a == "--force") =>
        {
            "`git checkout --` discards changes"
        }
        _ => return None,
    };
    Some(why.to_string())
}

fn is_flag_cluster_recursive(a: &str) -> bool {
    // unix: -r, -R, -rf, -fr, -rfv ...
    a.len() <= 5
        && a.starts_with('-')
        && !a.starts_with("--")
        && a[1..].chars().all(|c| "rfivdRFIVD".contains(c))
        && a[1..].contains(['r', 'R'])
}

fn blocked_recursive_delete(program: &str, args: &[String], scope: Scope<'_>) -> Option<String> {
    let recursive = args.iter().any(|a| {
        let a = a.split(':').next().unwrap_or(a);
        a == "--recursive"
            || is_flag_cluster_recursive(a)
            // PowerShell: -r, -rec, -recurse
            || (a.starts_with("-r") && "-recurse".starts_with(a))
            // cmd: rd /s
            || (matches!(program, "rd" | "rmdir") && a == "/s")
    });
    if !recursive {
        return None;
    }
    let mut paths: Vec<String> = Vec::new();
    let mut next_is_path = false;
    for a in args {
        if next_is_path {
            paths.extend(a.split(',').map(str::to_string));
            next_is_path = false;
        } else if matches!(a.as_str(), "-path" | "-literalpath" | "-lp") {
            next_is_path = true;
        } else if let Some(v) = a
            .strip_prefix("-path:")
            .or_else(|| a.strip_prefix("-literalpath:"))
        {
            paths.push(v.to_string());
        } else if (a.starts_with('-') && a.len() > 1)
            || (matches!(program, "rd" | "rmdir") && matches!(a.as_str(), "/s" | "/q"))
        {
            // a flag (value flags such as -Exclude only add harmless relative patterns)
        } else {
            paths.extend(a.split(',').map(str::to_string));
        }
    }
    let paths: Vec<&str> = paths
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        return Some(format!(
            "`{program}` deletes recursively and the path it applies to cannot be determined"
        ));
    }
    for p in paths {
        match classify_path(p, scope) {
            PathClass::Inside => {}
            PathClass::Outside => {
                return Some(format!(
                    "`{program}` deletes recursively outside the project ({p})"
                ))
            }
            PathClass::Root => {
                return Some(format!(
                    "`{program}` would delete the project folder itself ({p})"
                ))
            }
            PathClass::Git => {
                return Some(format!("`{program}` would delete git data ({p})"));
            }
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum PathClass {
    Inside,
    Outside,
    /// The project folder itself.
    Root,
    /// `.git` or something inside it.
    Git,
}

fn components(p: &str) -> Vec<String> {
    p.replace('/', "\\")
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(|c| c.to_lowercase())
        .collect()
}

fn strip_verbatim(p: &str) -> &str {
    p.strip_prefix(r"\\?\").unwrap_or(p)
}

/// Lexical resolution (no file system): inside which root does `p` fall?
fn classify_path(p: &str, scope: Scope<'_>) -> PathClass {
    let p = p.trim().trim_matches(['"', '\'']);
    // variables, `~`, drive-relative and rooted-without-drive paths cannot be resolved
    if p.contains(['$', '%', '~']) {
        return PathClass::Outside;
    }
    let norm = p.replace('/', "\\");
    let bytes = norm.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let mut comps: Vec<String> = if norm.starts_with("\\\\") {
        // UNC: never one of our roots
        return PathClass::Outside;
    } else if has_drive {
        if bytes.len() > 2 && bytes[2] != b'\\' {
            return PathClass::Outside; // `c:foo` is relative to a per-drive cwd
        }
        components(&norm)
    } else if norm.starts_with('\\') {
        return PathClass::Outside;
    } else {
        let mut base = components(strip_verbatim(
            &scope.cwd.to_string_lossy().replace('/', "\\"),
        ));
        base.extend(components(&norm));
        base
    };
    // `.` and `..`
    let mut out: Vec<String> = Vec::with_capacity(comps.len());
    for c in comps.drain(..) {
        match c.as_str() {
            "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(c),
        }
    }
    for root in scope.roots {
        let r = components(strip_verbatim(&root.to_string_lossy().replace('/', "\\")));
        if r.is_empty() || out.len() < r.len() || out[..r.len()] != r[..] {
            continue;
        }
        if out.len() == r.len() {
            return PathClass::Root;
        }
        if out[r.len()..].iter().any(|c| c == ".git") {
            return PathClass::Git;
        }
        return PathClass::Inside;
    }
    PathClass::Outside
}

const CREDENTIAL_NAMES: &[&str] = &[
    ".env",
    ".npmrc",
    ".netrc",
    "_netrc",
    ".git-credentials",
    ".pgpass",
    ".credentials.json",
    "credentials.json",
    "auth.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "secrets.json",
    "secrets.yml",
    "secrets.yaml",
    "secrets.toml",
];
const CREDENTIAL_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".azure", ".kube"];
const CREDENTIAL_EXTS: &[&str] = &[".pem", ".pfx", ".p12", ".ppk", ".key"];
const TEMPLATE_SUFFIXES: &[&str] = &[".example", ".sample", ".template", ".dist", ".defaults"];

/// The credential-looking thing `token` names, if it does.
fn credential_path(token: &str) -> Option<String> {
    let mut t = token.trim().trim_matches(['"', '\'']).to_lowercase();
    // `--flag=path`
    if t.starts_with('-') {
        t = t.split_once('=')?.1.trim_matches(['"', '\'']).to_string();
    }
    let t = t.replace('\\', "/");
    let comps: Vec<&str> = t.split('/').filter(|c| !c.is_empty()).collect();
    let last = comps.last()?;
    if let Some(d) = comps.iter().find(|c| CREDENTIAL_DIRS.contains(c)) {
        return Some(format!("`{d}`"));
    }
    if CREDENTIAL_NAMES.contains(last) {
        return Some(format!("`{last}`"));
    }
    if last.starts_with(".env.") && !TEMPLATE_SUFFIXES.iter().any(|s| last.ends_with(s)) {
        return Some(format!("`{last}`"));
    }
    if CREDENTIAL_EXTS.iter().any(|e| last.ends_with(e)) && last.len() > 4 {
        return Some(format!("`{last}`"));
    }
    None
}

/// `(project_root, run_dir)` as roots, for callers that hold both.
pub fn roots_of<'a>(project_root: &'a Path, run_dir: &'a Path) -> Vec<&'a Path> {
    if project_root == run_dir {
        vec![project_root]
    } else {
        vec![project_root, run_dir]
    }
}

/// Owned flavour of [`Scope`] for state that outlives a call.
#[derive(Debug, Clone)]
pub struct OwnedScope {
    pub cwd: PathBuf,
    pub roots: Vec<PathBuf>,
}

impl OwnedScope {
    pub fn new(project_root: &Path, run_dir: &Path) -> OwnedScope {
        OwnedScope {
            cwd: run_dir.to_path_buf(),
            roots: roots_of(project_root, run_dir)
                .into_iter()
                .map(Path::to_path_buf)
                .collect(),
        }
    }

    pub fn gate(&self, mode: ApprovalMode, allowed: &[String], tool: &str, detail: &str) -> Gate {
        let roots: Vec<&Path> = self.roots.iter().map(PathBuf::as_path).collect();
        gate(
            mode,
            allowed,
            tool,
            detail,
            Scope {
                cwd: &self.cwd,
                roots: &roots,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r"C:\Users\me\proj";

    fn check(cmd: &str) -> Option<String> {
        let roots = [Path::new(ROOT)];
        blocked(
            "Bash",
            cmd,
            Scope {
                cwd: Path::new(ROOT),
                roots: &roots,
            },
        )
    }

    fn assert_blocked(cmd: &str) {
        assert!(check(cmd).is_some(), "should be blocked: {cmd}");
    }

    fn assert_free(cmd: &str) {
        assert_eq!(check(cmd), None, "should not be blocked: {cmd}");
    }

    #[test]
    fn modes_parse_order_and_combine() {
        assert!(ApprovalMode::Ask < ApprovalMode::Allowlist);
        assert!(ApprovalMode::Allowlist < ApprovalMode::Auto);
        assert_eq!(
            ApprovalMode::parse("Allow-List"),
            Some(ApprovalMode::Allowlist)
        );
        assert_eq!(ApprovalMode::parse("yolo"), None);
        assert_eq!(ApprovalMode::default(), ApprovalMode::Ask);
        use ApprovalMode::*;
        // the minimum of every party
        assert_eq!(effective_mode(Auto, None, None), Auto);
        assert_eq!(effective_mode(Auto, Some(Allowlist), None), Allowlist);
        assert_eq!(effective_mode(Auto, Some(Allowlist), Some(Ask)), Ask);
        // nobody raises what another party lowered
        assert_eq!(effective_mode(Ask, Some(Auto), Some(Auto)), Ask);
        assert_eq!(effective_mode(Allowlist, Some(Auto), Some(Auto)), Allowlist);
        for m in ApprovalMode::ALL {
            assert_eq!(ApprovalMode::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn the_allowlist_matches_simple_globs() {
        let allow = |cmd: &str, pats: &[&str]| {
            command_allowed(cmd, &pats.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert!(allow("cargo test -p store", &["cargo *"]));
        assert!(allow("cargo", &["cargo *"]));
        assert!(allow("CARGO  fmt --all", &["cargo *"]));
        assert!(allow("git diff HEAD~1", &["git diff *"]));
        assert!(allow("git status", &["git status"]));
        assert!(allow("rg -n foo src", &["rg *"]));
        assert!(allow("Get-ChildItem -Recurse", &["get-childitem *"]));
        assert!(!allow("git status -sb", &["git status"]));
        assert!(!allow("git log", &["git diff *"]));
        assert!(!allow("cargo-evil run", &["cargo *"]));
        assert!(!allow("anything", &[]));
        assert!(!allow("anything", &["", "   "]));
        // `?` is one character
        assert!(allow("ls a", &["ls ?"]));
        assert!(!allow("ls ab", &["ls ?"]));
    }

    #[test]
    fn a_command_that_chains_or_redirects_never_matches() {
        let pats = vec![
            "cargo *".to_string(),
            "git diff *".to_string(),
            "rg *".to_string(),
        ];
        for cmd in [
            "cargo test; rm -rf x",
            "cargo test | tee out.txt",
            "cargo test && echo done",
            "cargo test || true",
            "cargo test > out.txt",
            "cargo test < in.txt",
            "cargo test `whoami`",
            "cargo test $(whoami)",
            "cargo test\nrm x",
            "rg a & calc",
            "rg \"a|b\" src",
        ] {
            assert!(!command_allowed(cmd, &pats), "must not match: {cmd:?}");
        }
        assert!(command_allowed("cargo test --release", &pats));
        // a command cut short by the agent's truncation is not the command that runs
        assert!(!command_allowed("cargo test \u{2026}", &pats));
    }

    #[test]
    fn allowlist_patterns_are_validated() {
        assert_eq!(pattern_problem("cargo *"), None);
        assert!(pattern_problem("").is_some());
        assert!(pattern_problem("*").is_some());
        assert!(pattern_problem("cargo * ; rm x").is_some());
        assert!(pattern_problem("rg a | b").is_some());
        assert!(pattern_problem("a\nb").is_some());
    }

    #[test]
    fn git_commands_that_publish_or_discard_are_blocked() {
        for c in [
            "git push",
            "git push origin main --force",
            "git commit -m x",
            "git commit --amend",
            "git reset --hard HEAD~1",
            "git checkout -- src/lib.rs",
            "git checkout .",
            "git stash",
            "git stash pop",
            "git clean -fd",
            "git -C other push",
            "git -c core.x=1 commit -m a",
            r#""C:\Program Files\Git\bin\git.exe" push"#,
            "GIT PUSH",
            "cd x && git push",
            "cargo test; git commit -am x",
            "cmd /c git push",
            r#"cmd /c "cd /d x && git push""#,
            "bash -c 'git push'",
            r#"powershell -NoProfile -Command "git push""#,
            "env FOO=1 git push",
            "echo hi | git commit -F -",
            "(git push)",
            "$(git push)",
        ] {
            assert_blocked(c);
        }
        for c in [
            "git status",
            "git diff HEAD",
            "git log --oneline -5",
            "git reset HEAD file",
            "git reset --soft HEAD~1",
            "git checkout main",
            "git checkout -b feature",
            "git branch",
            r#"rg "git push" docs"#,
            "echo git push",
            "cargo test",
        ] {
            assert_free(c);
        }
    }

    #[test]
    fn recursive_deletes_are_blocked_outside_the_project() {
        for c in [
            "rm -rf /",
            "rm -rf ~",
            "rm -rf $HOME/x",
            "rm -rf ..",
            r"rm -rf ..\other",
            r"rm -rf C:\Users\me\other",
            r"rm -rf C:\",
            "rm -rf .",
            "rm -rf .git",
            r"rm -rf C:\Users\me\proj",
            r"rm -rf C:\Users\me\proj\.git\hooks",
            r"Remove-Item -Recurse -Force C:\Windows\Temp\x",
            r"Remove-Item -Path C:\data -Recurse",
            r"Remove-Item C:\data -r",
            "remove-item $env:TEMP\\x -Recurse",
            r"ri -Recurse -Force ..\x",
            r"rd /s /q C:\data",
            r"rmdir /s /q ..\x",
            "Get-ChildItem | Remove-Item -Recurse",
            r"del /s /q *.tmp",
            r"erase /s x",
            "rm -rf C:/Users/me/proj/../other",
            r"rm -rf \Users\me",
            "rm -rf c:foo",
            r"rm -rf \\server\share\x",
        ] {
            assert_blocked(c);
        }
        for c in [
            "rm -rf target",
            r"rm -rf target\debug",
            "rm -rf ./build",
            r"rm -rf C:\Users\me\proj\target",
            r"rm -rf C:/Users/me/proj/target/debug",
            r"Remove-Item -Recurse -Force .\dist",
            "Remove-Item -Recurse build, dist",
            "rm file.txt",
            "rm -f file.txt",
            "Remove-Item file.txt",
            r"rd /s /q build",
            "rm -rf *",
            "rm -rf target/*",
            "del file.txt",
        ] {
            assert_free(c);
        }
    }

    #[test]
    fn the_run_worktree_counts_as_inside() {
        let roots = [
            Path::new(r"C:\Users\me\proj"),
            Path::new(r"C:\wt\proj-codex"),
        ];
        let scope = Scope {
            cwd: Path::new(r"C:\wt\proj-codex"),
            roots: &roots,
        };
        assert_eq!(blocked("Bash", "rm -rf target", scope), None);
        assert_eq!(
            blocked("Bash", r"rm -rf C:\wt\proj-codex\target", scope),
            None
        );
        assert!(blocked("Bash", r"rm -rf C:\wt\other", scope).is_some());
        assert!(blocked("Bash", r"rm -rf C:\wt\proj-codex", scope).is_some());
    }

    #[test]
    fn disks_registry_and_stored_credentials_are_blocked() {
        for c in [
            "format D: /q",
            "format.com C:",
            "diskpart /s script.txt",
            "Format-Volume -DriveLetter D",
            "Clear-Disk -Number 1 -RemoveData",
            r"reg add HKCU\Software\X /v a /d 1",
            r"reg delete HKLM\Software\X /f",
            r"REG.EXE ADD HKCU\x",
            r#"Set-ItemProperty -Path "HKLM:\Software\X" -Name a -Value 1"#,
            r"New-ItemProperty HKCU:\Software\X -Name a",
            "cmdkey /list",
            "vaultcmd /listcreds:x",
            "powershell -EncodedCommand ZQBjAGgAbwA=",
            "powershell -enc ZQBjAGgAbwA=",
        ] {
            assert_blocked(c);
        }
        for c in [
            "reg query HKCU\\Software\\X",
            "cargo fmt --all",
            "Format-Table -AutoSize",
            "Get-ItemProperty HKCU:\\Software\\X",
            "cargo test --format json",
        ] {
            assert_free(c);
        }
    }

    #[test]
    fn credential_looking_files_are_blocked() {
        for c in [
            "cat .env",
            "type C:\\Users\\me\\proj\\.env",
            "cat .env.local",
            "Get-Content .\\.env.production",
            "cat ~/.ssh/id_rsa",
            r"type C:\Users\me\.ssh\config",
            "cat ~/.aws/credentials",
            "cat .npmrc",
            "cat .netrc",
            "cat .git-credentials",
            "cat id_ed25519",
            "cat server.pem",
            "cat cert.pfx",
            "cat ~/.claude/.credentials.json",
            "cat ~/.codex/auth.json",
            "cp .env /tmp/x",
            "rg token --glob=.env",
            "rg password .ssh",
        ] {
            assert_blocked(c);
        }
        for c in [
            "cat .env.example",
            "cat .env.sample",
            "cat .env.template",
            "cat src/credentials/mod.rs",
            "cat README.md",
            "rg env src",
            "cat environment.rs",
        ] {
            assert_free(c);
        }
        // non-command tools are checked too
        let roots = [Path::new(ROOT)];
        let scope = Scope {
            cwd: Path::new(ROOT),
            roots: &roots,
        };
        assert!(blocked("write_text_file", r"C:\Users\me\proj\.env", scope).is_some());
        assert_eq!(
            blocked("write_text_file", r"C:\Users\me\proj\src\main.rs", scope),
            None
        );
    }

    #[test]
    fn the_command_is_found_in_every_agents_wording() {
        // Claude
        assert_eq!(
            shell_command("Bash", "cargo test -p store").as_deref(),
            Some("cargo test -p store")
        );
        assert_eq!(
            shell_command("PowerShell", "Get-ChildItem").as_deref(),
            Some("Get-ChildItem")
        );
        // Codex
        assert_eq!(
            shell_command("Command", "git status").as_deref(),
            Some("git status")
        );
        // ACP: title and `title: command`
        assert_eq!(
            shell_command("Run shell command", "Run shell command: rg foo").as_deref(),
            Some("rg foo")
        );
        assert_eq!(
            shell_command("rg foo src", "rg foo src: rg foo src").as_deref(),
            Some("rg foo src")
        );
        // `Bash: command` inside the detail
        assert_eq!(shell_command("Tool", "Bash: ls").as_deref(), Some("ls"));
        // not commands
        assert_eq!(shell_command("Edit", r"C:\x\a.rs"), None);
        assert_eq!(shell_command("write_text_file", r"C:\x\a.rs"), None);
        assert_eq!(shell_command("Bash", "  "), None);
    }

    #[test]
    fn the_gate_checks_the_blocklist_first_in_every_mode() {
        let roots = [Path::new(ROOT)];
        let scope = Scope {
            cwd: Path::new(ROOT),
            roots: &roots,
        };
        let allow = vec!["git *".to_string(), "cargo *".to_string()];
        for mode in ApprovalMode::ALL {
            // even though `git *` is allowed and the mode is auto
            assert!(
                matches!(gate(mode, &allow, "Bash", "git push", scope), Gate::Block(r) if r.contains("git push")),
                "{mode}"
            );
        }
        // ask: everything else goes to the queue
        assert_eq!(
            gate(ApprovalMode::Ask, &allow, "Bash", "cargo test", scope),
            Gate::Ask
        );
        // allowlist: matching commands are approved, the rest asks
        assert_eq!(
            gate(ApprovalMode::Allowlist, &allow, "Bash", "cargo test", scope),
            Gate::Auto
        );
        assert_eq!(
            gate(
                ApprovalMode::Allowlist,
                &allow,
                "Bash",
                "npm install",
                scope
            ),
            Gate::Ask
        );
        assert_eq!(
            gate(
                ApprovalMode::Allowlist,
                &allow,
                "Bash",
                "cargo test && npm i",
                scope
            ),
            Gate::Ask
        );
        assert_eq!(
            gate(ApprovalMode::Allowlist, &allow, "Edit", r"C:\x\a.rs", scope),
            Gate::Ask,
            "an edit is not a command"
        );
        assert_eq!(
            gate(ApprovalMode::Allowlist, &[], "Bash", "cargo test", scope),
            Gate::Ask
        );
        // auto approves everything that is not blocked
        assert_eq!(
            gate(ApprovalMode::Auto, &[], "Bash", "npm install", scope),
            Gate::Auto
        );
        assert_eq!(
            gate(ApprovalMode::Auto, &[], "Edit", r"C:\x\a.rs", scope),
            Gate::Auto
        );
        assert!(matches!(
            gate(ApprovalMode::Auto, &[], "Bash", "cat .env", scope),
            Gate::Block(_)
        ));
    }
}
