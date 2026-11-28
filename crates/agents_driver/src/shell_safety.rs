//! Shell-command safety shared by the driver and the harness.
//!
//! * [`credential_path`] / [`credential_in_token`]: does a command-line token name a credentials file
//!   (`.env`, `.ssh\*`, `id_rsa`, `*.pem`, `.git\config`...)? The harness hard blocklist
//!   (`agent_harness::approval_policy`) and the read-only classifier below use the same rules.
//! * [`is_read_only_command`] / [`read_only_verdict`]: a strict, pure classifier that decides whether
//!   one shell command line only **inspects** the project (listing, reading, searching, git history),
//!   so a read-only agent may run it. Everything it does not understand is refused.
//!
//! # What the classifier accepts
//!
//! One or more `;`-separated pipelines. Every pipeline is `producer [| consumer]...`:
//!
//! * producers: `Get-ChildItem` (`gci`, `ls`, `dir`), `Get-Content` (`gc`, `cat`, `type`), `Select-String`
//!   (`sls`), `rg`, `grep`, `findstr`, `Test-Path`, `Get-Item` (`gi`), `Resolve-Path` (`rvpa`), `where`,
//!   `pwd` (`Get-Location`, `gl`), `Write-Output` / `Write-Host` / `echo` with literals, `head`, `tail`, `wc`
//!   and `git` (`status`, `log`, `diff`, `show`, `branch --list`, `ls-files`, `blame`, `rev-parse`);
//! * consumers (after a `|`): `Select-Object`, `Sort-Object`, `Measure-Object`, `Format-Table|List|Wide`,
//!   `Out-String`, `Select-String`, `rg`, `grep`, `findstr`, `head`, `tail`, `wc`.
//!
//! Each program has a closed list of options; an option that is not on the list (`rg --pre`, `git -c`,
//! `git log --output`, `Get-Content -Wait`, ...) refuses the whole command. Path arguments must stay inside
//! the project folder (or the read-only cargo registry sources) and never name a credentials file.
//!
//! # What it refuses
//!
//! Redirections, `&`, `&&`, `||`, script blocks and groups (`{ } ( )`), subexpressions and variables (`$`,
//! backtick, `@`, `%VAR%`), `^`, `#`, stop-parsing `--%`, new lines and every control character, non-ASCII
//! punctuation (smart quotes and en dashes are quote and dash characters for PowerShell), doubled or
//! escaped quotes (`"a"";x;""b"`, `\"`; PowerShell, bash and cmd disagree about them), single-quoted `| & < > ^`
//! (cmd.exe does not quote with `'`), programs given by path, interpreters and wrappers, anything that
//! writes, `..` in a path, UNC and device paths, drive-relative paths, alternate data streams and provider
//! paths (`env:`, `HKLM:`), Windows device names, `~`, wildcards that could match a credentials file.
//!
//! Assumptions: the command runs in PowerShell, a POSIX shell or cmd.exe, and a directory junction or
//! symlink inside the project is the user's own business (the check is lexical; it does no file I/O).

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Credential-looking paths
// ---------------------------------------------------------------------------

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

/// File names a wildcard is tried against (see [`wildcard_may_hit_credential`]).
const WILDCARD_PROBES: &[&str] = &[
    ".env",
    ".env.local",
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
    "server.pem",
    "server.key",
    "cert.pfx",
    "cert.p12",
    "key.ppk",
];

/// The credential-looking thing `token` names, if it does.
pub fn credential_path(token: &str) -> Option<String> {
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
    // `.git/config` can hold `https://user:token@host/...` remotes
    if comps.windows(2).any(|w| w[0] == ".git" && w[1] == "config") {
        return Some("`.git/config`".to_string());
    }
    None
}

/// [`credential_path`] on the token and on the pieces a `rev:path`, `--flag=path` or `a,b` hides.
pub fn credential_in_token(token: &str) -> Option<String> {
    if let Some(c) = credential_path(token) {
        return Some(c);
    }
    for sep in [':', '=', ','] {
        for piece in token.split(sep) {
            if piece.len() < token.len() {
                if let Some(c) = credential_path(piece) {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// Plain glob (`*`, `?`), case-insensitive.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
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

/// Could the wildcard component `pattern` match the name of a credentials file?
pub fn wildcard_may_hit_credential(pattern: &str) -> bool {
    WILDCARD_PROBES.iter().any(|n| glob(pattern, n))
}

fn wildcard_may_hit_credential_dir(pattern: &str) -> bool {
    CREDENTIAL_DIRS.iter().any(|n| glob(pattern, n))
}

// ---------------------------------------------------------------------------
// The read-only classifier
// ---------------------------------------------------------------------------

/// Longest command line the classifier looks at.
pub const MAX_COMMAND_CHARS: usize = 4000;

/// Where a read-only command may look.
#[derive(Debug, Clone, Default)]
pub struct ReadScope {
    /// The project folder; relative paths resolve against it.
    pub cwd: PathBuf,
    /// Further read-only roots (none by default).
    pub roots: Vec<PathBuf>,
    /// `%USERPROFILE%\.cargo\registry\src`: `$env:USERPROFILE\.cargo\registry\src\...` (also `$HOME`,
    /// `~`, `$env:CARGO_HOME\registry\src`) is read as this folder.
    pub cargo_src: Option<PathBuf>,
}

impl ReadScope {
    /// The project folder plus the cargo registry sources found through `CARGO_HOME` /
    /// `USERPROFILE` / `HOME` (the only place outside the project a read-only agent may look at).
    pub fn for_cwd(cwd: impl Into<PathBuf>) -> ReadScope {
        let home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(|h| PathBuf::from(h).join(".cargo"))
            });
        ReadScope {
            cwd: cwd.into(),
            roots: Vec::new(),
            cargo_src: home.map(|h| h.join("registry").join("src")),
        }
    }
}

/// Why a command is not read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// Spanish sentence, ready to show.
    pub reason: String,
    /// The command names a credentials file (a hard block, not just "not a read command").
    pub credential: bool,
}

impl Denial {
    fn new(reason: impl Into<String>) -> Denial {
        Denial {
            reason: reason.into(),
            credential: false,
        }
    }
}

type Verdict<T> = Result<T, Denial>;

fn deny<T>(reason: impl Into<String>) -> Verdict<T> {
    Err(Denial::new(reason))
}

/// `true` when `cmd` only inspects the project (see the module docs).
pub fn is_read_only_command(cmd: &str, scope: &ReadScope) -> bool {
    read_only_verdict(cmd, scope).is_ok()
}

/// [`is_read_only_command`] with the reason when it is not.
pub fn read_only_verdict(cmd: &str, scope: &ReadScope) -> Result<(), Denial> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return deny("el comando está vacío");
    }
    if cmd.ends_with('…') || cmd.ends_with("...") {
        return deny("el comando llegó cortado; no se puede comprobar completo");
    }
    if cmd.chars().count() > MAX_COMMAND_CHARS {
        return deny("el comando es demasiado largo para comprobarlo");
    }
    let text = substitute_registry(cmd, scope);
    let segments = lex(&text)?;
    let mut any = false;
    for pipeline in split_pipelines(segments) {
        any = true;
        check_pipeline(&pipeline, scope)?;
    }
    if !any {
        return deny("el comando está vacío");
    }
    Ok(())
}

const REG_MARK: &str = "SELACHIREGISTRYSRC";

/// `$env:USERPROFILE\.cargo\registry\src` & co. -> a marker word the path check resolves.
fn substitute_registry(cmd: &str, scope: &ReadScope) -> String {
    if scope.cargo_src.is_none() {
        return cmd.to_string();
    }
    let lower = cmd.to_ascii_lowercase();
    let b = lower.as_bytes();
    let is_sep = |c: u8| c == b'\\' || c == b'/';
    let mut out = String::with_capacity(cmd.len());
    let mut i = 0usize;
    while i < b.len() {
        let rest = &lower[i..];
        let prev_ok =
            i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'"' | b'\'' | b',' | b'=' | b':');
        let mut matched: Option<usize> = None;
        // (prefix, segments that must follow, each preceded by a separator)
        let tails: [(&str, &[&str]); 4] = [
            ("$env:userprofile", &[".cargo", "registry", "src"]),
            ("$home", &[".cargo", "registry", "src"]),
            ("$env:cargo_home", &["registry", "src"]),
            ("~", &[".cargo", "registry", "src"]),
        ];
        if prev_ok {
            for (prefix, segs) in tails {
                if !rest.starts_with(prefix) {
                    continue;
                }
                let mut j = i + prefix.len();
                let mut ok = true;
                for seg in segs {
                    if j < b.len() && is_sep(b[j]) && lower[j + 1..].starts_with(seg) {
                        j += 1 + seg.len();
                    } else {
                        ok = false;
                        break;
                    }
                }
                // the last segment must end there (`src`, not `srcx`)
                if ok && (j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_')) {
                    matched = Some(j);
                    break;
                }
            }
        }
        match matched {
            Some(j) => {
                out.push_str(REG_MARK);
                i = j;
            }
            None => {
                // copy one character (not byte) of the original
                let ch = cmd[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

// ---- lexer ------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Tok {
    text: String,
    /// Any part of it was quoted.
    quoted: bool,
    /// It starts inside quotes: for a PowerShell cmdlet that makes it a value, never a parameter.
    lead_quoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    Semi,
    Pipe,
    End,
}

#[derive(Debug, Clone)]
struct Seg {
    toks: Vec<Tok>,
    sep: Sep,
}

fn has_percent_var(s: &str) -> bool {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '%' {
            let mut j = i + 1;
            while j < b.len()
                && (b[j].is_alphanumeric() || matches!(b[j], '_' | '~' | ':' | '(' | ')'))
            {
                j += 1;
            }
            if j < b.len() && b[j] == '%' && j > i + 1 {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn lex(text: &str) -> Verdict<Vec<Seg>> {
    if has_percent_var(text) {
        return deny("usa una variable de entorno (%NOMBRE%), que no se puede resolver");
    }
    let chars: Vec<char> = text.chars().collect();
    let mut segs: Vec<Seg> = Vec::new();
    let mut toks: Vec<Tok> = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut quoted = false;
    let mut lead_quoted = false;
    let mut quote: Option<char> = None;
    let mut prev: char = ' ';
    let mut i = 0usize;
    macro_rules! flush {
        () => {
            if in_tok {
                toks.push(Tok {
                    text: std::mem::take(&mut cur),
                    quoted,
                    lead_quoted,
                });
            }
            in_tok = false;
            quoted = false;
            lead_quoted = false;
        };
    }
    while i < chars.len() {
        let c = chars[i];
        // characters that are never fine
        if (c as u32) < 0x20 && c != '\t' || c == '\u{7f}' {
            return deny("contiene saltos de línea o caracteres de control");
        }
        if !c.is_ascii() && !c.is_alphanumeric() {
            return deny(
                "contiene un carácter especial no ASCII (comillas o guiones tipográficos)",
            );
        }
        match quote {
            Some(q) => {
                if c == q {
                    if prev == '\\' {
                        return deny("contiene una comilla escapada con barra invertida");
                    }
                    if chars.get(i + 1) == Some(&q) {
                        return deny("contiene comillas dobladas, que cada shell lee distinto");
                    }
                    quote = None;
                } else {
                    if c == '`' || (q == '"' && c == '$') {
                        return deny("expande variables o escapes (`$` o acento grave)");
                    }
                    if q == '\'' && matches!(c, '|' | '&' | '<' | '>' | '^') {
                        return deny(
                            "usa | & < > ^ entre comillas simples (cmd no las respeta); usa comillas dobles",
                        );
                    }
                    cur.push(c);
                }
            }
            None => match c {
                ' ' | '\t' => {
                    flush!();
                }
                '"' | '\'' => {
                    if prev == '\\' {
                        return deny("contiene una comilla escapada con barra invertida");
                    }
                    if !in_tok {
                        lead_quoted = true;
                    }
                    if !in_tok {
                        lead_quoted = true;
                    }
                    quote = Some(c);
                    quoted = true;
                    in_tok = true;
                }
                ';' => {
                    flush!();
                    segs.push(Seg {
                        toks: std::mem::take(&mut toks),
                        sep: Sep::Semi,
                    });
                }
                '|' => {
                    if chars.get(i + 1) == Some(&'|') {
                        return deny("encadena comandos con ||");
                    }
                    flush!();
                    segs.push(Seg {
                        toks: std::mem::take(&mut toks),
                        sep: Sep::Pipe,
                    });
                }
                ',' => {
                    flush!();
                }
                '&' | '<' | '>' => return deny("redirige o encadena con & < >"),
                '(' | ')' | '{' | '}' => {
                    return deny("usa bloques, grupos o subexpresiones ( ) { }")
                }
                '`' | '$' | '@' | '^' | '%' | '#' | '[' | ']' => {
                    return deny(
                        "usa variables, escapes o sintaxis de PowerShell ($ ` @ ^ % # [ ])",
                    )
                }
                _ => {
                    cur.push(c);
                    in_tok = true;
                }
            },
        }
        prev = c;
        i += 1;
    }
    if quote.is_some() {
        return deny("tiene comillas sin cerrar");
    }
    if in_tok {
        toks.push(Tok {
            text: cur,
            quoted,
            lead_quoted,
        });
    }
    segs.push(Seg {
        toks,
        sep: Sep::End,
    });
    if segs
        .iter()
        .flat_map(|s| &s.toks)
        .any(|t| !t.quoted && t.text == "--%")
    {
        return deny("usa --% (deja de analizar el comando)");
    }
    if segs
        .iter()
        .flat_map(|s| &s.toks)
        .any(|t| t.text.contains("::"))
    {
        return deny("accede a miembros estáticos con ::");
    }
    Ok(segs)
}

/// Group the segments in `;`-separated pipelines.
fn split_pipelines(segs: Vec<Seg>) -> Vec<Vec<Vec<Tok>>> {
    let mut out: Vec<Vec<Vec<Tok>>> = Vec::new();
    let mut cur: Vec<Vec<Tok>> = Vec::new();
    for s in segs {
        let end = s.sep != Sep::Pipe;
        cur.push(s.toks);
        if end {
            // a pipeline made only of empty segments (`;;`, trailing `;`) is nothing
            if cur.iter().any(|t| !t.is_empty()) {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.iter().any(|t| !t.is_empty()) {
        out.push(cur);
    }
    out
}

// ---- paths -------------------------------------------------------------------

const DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "conin", "conout", "clock", "com1", "com2", "com3", "com4", "com5",
    "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8",
    "lpt9",
];

fn norm_comps(s: &str) -> Vec<String> {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    s.replace('/', "\\")
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(|c| c.to_lowercase())
        .collect()
}

fn is_wild(c: &str) -> bool {
    c.contains(['*', '?'])
}

/// Is the path argument `raw` inside the project (or another allowed root) and not a credentials file?
fn check_path(raw: &str, scope: &ReadScope, reads_content: bool) -> Verdict<()> {
    // not trimmed: a trailing space is part of a Windows name and is refused below
    let t = raw;
    if t.is_empty() {
        return Ok(());
    }
    if let Some(c) = credential_in_token(t) {
        return Err(Denial {
            reason: format!("toca {c}, que parece un archivo de credenciales"),
            credential: true,
        });
    }
    let (mut comps, from_registry) = if let Some(rest) = t.strip_prefix(REG_MARK) {
        let Some(src) = &scope.cargo_src else {
            return deny("la ruta usa una variable que no se puede resolver");
        };
        if !rest.is_empty() && !rest.starts_with(['\\', '/']) {
            return deny("la ruta no es válida");
        }
        let mut v = norm_comps(&src.to_string_lossy());
        v.extend(norm_comps(rest));
        (v, true)
    } else {
        (Vec::new(), false)
    };
    if !from_registry {
        if t.starts_with('~') {
            return deny("la ruta usa ~ (carpeta personal), fuera del proyecto");
        }
        if t.starts_with("\\\\") || t.starts_with("//") {
            return deny("es una ruta de red o de dispositivo (\\\\ o //)");
        }
        // a colon is only the drive letter's
        let b = t.as_bytes();
        let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
        let colons = t.matches(':').count();
        if colons > usize::from(drive) {
            return deny("la ruta usa : (flujos alternativos o proveedores como env: o HKLM:)");
        }
        let norm = t.replace('/', "\\");
        if drive {
            if b.len() == 2 || b[2] != b'\\' && b[2] != b'/' {
                return deny("la ruta es relativa a una unidad (C:algo)");
            }
            comps = norm_comps(&norm);
        } else if norm.starts_with('\\') {
            return deny("la ruta empieza por \\ sin unidad, fuera del proyecto");
        } else {
            comps = norm_comps(&scope.cwd.to_string_lossy());
            comps.extend(norm_comps(&norm));
        }
    }
    // component rules, on the part that came from the argument
    for c in norm_comps(t.strip_prefix(REG_MARK).unwrap_or(t)) {
        if c == ".." {
            return deny("la ruta usa `..`");
        }
        if c != "." && (c.ends_with('.') || c.ends_with(' ')) {
            return deny("un componente de la ruta acaba en punto o espacio");
        }
        let stem = c.split('.').next().unwrap_or("");
        if DEVICE_NAMES.contains(&stem) {
            return deny("la ruta nombra un dispositivo de Windows (CON, NUL, COM1...)");
        }
    }
    let comps: Vec<String> = comps.into_iter().filter(|c| c != ".").collect();
    let mut roots: Vec<Vec<String>> = vec![norm_comps(&scope.cwd.to_string_lossy())];
    roots.extend(scope.roots.iter().map(|r| norm_comps(&r.to_string_lossy())));
    if let Some(src) = &scope.cargo_src {
        if from_registry {
            roots.push(norm_comps(&src.to_string_lossy()));
        }
    }
    if !roots
        .iter()
        .any(|r| !r.is_empty() && comps.len() >= r.len() && comps[..r.len()] == r[..])
    {
        return deny("la ruta queda fuera del proyecto");
    }
    if reads_content {
        let parts: Vec<&str> = t
            .split(['\\', '/'])
            .filter(|c| !c.is_empty() && is_wild(c))
            .collect();
        let last = t.rsplit(['\\', '/']).find(|c| !c.is_empty()).unwrap_or("");
        for w in &parts {
            let hit = if *w == last {
                wildcard_may_hit_credential(w) || wildcard_may_hit_credential_dir(w)
            } else {
                w.starts_with('.') && wildcard_may_hit_credential_dir(w)
            };
            if hit {
                return Err(Denial {
                    reason: format!(
                        "el comodín `{w}` podría coincidir con un archivo de credenciales"
                    ),
                    credential: true,
                });
            }
        }
    }
    Ok(())
}

// ---- argument grammar ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum V {
    /// A path (checked against the scope).
    Path,
    /// A word that is not a path but may name a file (credential check only).
    Text,
    /// A regular expression or search text: not looked at.
    Pattern,
    /// A number.
    Num,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pos {
    /// No positional arguments.
    None,
    /// Paths.
    Paths,
    /// First a pattern (unless the options gave it), then paths.
    PatternThenPaths,
    /// Property names (`Name`, `Length`, `*`).
    Props,
    /// Any literal words.
    Literal,
    /// Simple program names (`where git`).
    Names,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// PowerShell: `-Name`, `-Name:value`, case-insensitive, exact names.
    Ps,
    /// POSIX: `-a`, `-abc` clusters, `--long`, `--long=value`, `--`.
    Posix,
    /// `findstr`: `/i`, `/c:text`.
    Slash,
}

struct Spec {
    style: Style,
    flags: &'static [&'static str],
    values: &'static [(&'static str, V)],
    pos: Pos,
    /// Prints file contents (wildcards are checked against credential names).
    reads: bool,
    /// Accepts `-5` as a count (`head -5`, `git log -5`).
    num_flag: bool,
}

const PS_LIST: Spec = Spec {
    style: Style::Ps,
    flags: &[
        "-recurse",
        "-rec",
        "-force",
        "-name",
        "-file",
        "-directory",
        "-hidden",
        "-readonly",
        "-system",
    ],
    values: &[
        ("-path", V::Path),
        ("-literalpath", V::Path),
        ("-filter", V::Text),
        ("-include", V::Text),
        ("-exclude", V::Text),
        ("-depth", V::Num),
    ],
    pos: Pos::Paths,
    reads: false,
    num_flag: false,
};
const PS_CONTENT: Spec = Spec {
    style: Style::Ps,
    flags: &["-raw"],
    values: &[
        ("-path", V::Path),
        ("-literalpath", V::Path),
        ("-totalcount", V::Num),
        ("-head", V::Num),
        ("-first", V::Num),
        ("-tail", V::Num),
        ("-last", V::Num),
        ("-readcount", V::Num),
        ("-encoding", V::Text),
    ],
    pos: Pos::Paths,
    reads: true,
    num_flag: false,
};
const PS_SELSTR: Spec = Spec {
    style: Style::Ps,
    flags: &[
        "-simplematch",
        "-casesensitive",
        "-list",
        "-notmatch",
        "-quiet",
        "-allmatches",
    ],
    values: &[
        ("-pattern", V::Pattern),
        ("-path", V::Path),
        ("-literalpath", V::Path),
        ("-context", V::Num),
        ("-encoding", V::Text),
        ("-include", V::Text),
        ("-exclude", V::Text),
    ],
    pos: Pos::PatternThenPaths,
    reads: true,
    num_flag: false,
};
const PS_TESTPATH: Spec = Spec {
    style: Style::Ps,
    flags: &[],
    values: &[
        ("-path", V::Path),
        ("-literalpath", V::Path),
        ("-pathtype", V::Text),
    ],
    pos: Pos::Paths,
    reads: false,
    num_flag: false,
};
const PS_GETITEM: Spec = Spec {
    style: Style::Ps,
    flags: &["-force"],
    values: &[("-path", V::Path), ("-literalpath", V::Path)],
    pos: Pos::Paths,
    reads: false,
    num_flag: false,
};
const PS_RESOLVE: Spec = Spec {
    style: Style::Ps,
    flags: &["-relative"],
    values: &[("-path", V::Path), ("-literalpath", V::Path)],
    pos: Pos::Paths,
    reads: false,
    num_flag: false,
};
const PS_NONE: Spec = Spec {
    style: Style::Ps,
    flags: &[],
    values: &[],
    pos: Pos::None,
    reads: false,
    num_flag: false,
};
const PS_WRITE: Spec = Spec {
    style: Style::Ps,
    flags: &["-nonewline"],
    values: &[("-foregroundcolor", V::Text), ("-backgroundcolor", V::Text)],
    pos: Pos::Literal,
    reads: false,
    num_flag: false,
};
const PS_SELECT: Spec = Spec {
    style: Style::Ps,
    flags: &["-unique"],
    values: &[
        ("-first", V::Num),
        ("-last", V::Num),
        ("-skip", V::Num),
        ("-index", V::Num),
        ("-expandproperty", V::Text),
        ("-property", V::Text),
    ],
    pos: Pos::Props,
    reads: false,
    num_flag: false,
};
const PS_SORT: Spec = Spec {
    style: Style::Ps,
    flags: &["-descending", "-unique", "-casesensitive"],
    values: &[
        ("-property", V::Text),
        ("-top", V::Num),
        ("-bottom", V::Num),
    ],
    pos: Pos::Props,
    reads: false,
    num_flag: false,
};
const PS_MEASURE: Spec = Spec {
    style: Style::Ps,
    flags: &[
        "-line",
        "-word",
        "-character",
        "-sum",
        "-average",
        "-maximum",
        "-minimum",
        "-ignorewhitespace",
    ],
    values: &[("-property", V::Text)],
    pos: Pos::Props,
    reads: false,
    num_flag: false,
};
const PS_FORMAT: Spec = Spec {
    style: Style::Ps,
    flags: &["-autosize", "-wrap", "-hidetableheaders"],
    values: &[("-property", V::Text)],
    pos: Pos::Props,
    reads: false,
    num_flag: false,
};
const PS_OUTSTRING: Spec = Spec {
    style: Style::Ps,
    flags: &["-stream"],
    values: &[("-width", V::Num)],
    pos: Pos::None,
    reads: false,
    num_flag: false,
};
const PX_HEAD: Spec = Spec {
    style: Style::Posix,
    flags: &[],
    values: &[
        ("-n", V::Num),
        ("-c", V::Num),
        ("--lines", V::Num),
        ("--bytes", V::Num),
    ],
    pos: Pos::Paths,
    reads: true,
    num_flag: true,
};
const PX_WC: Spec = Spec {
    style: Style::Posix,
    flags: &[
        "-l", "-w", "-c", "-m", "--lines", "--words", "--bytes", "--chars",
    ],
    values: &[],
    pos: Pos::Paths,
    reads: true,
    num_flag: false,
};
const RG: Spec = Spec {
    style: Style::Posix,
    flags: &[
        "-n",
        "--line-number",
        "-N",
        "--no-line-number",
        "-i",
        "--ignore-case",
        "-S",
        "--smart-case",
        "-s",
        "--case-sensitive",
        "-w",
        "--word-regexp",
        "-F",
        "--fixed-strings",
        "-l",
        "--files-with-matches",
        "--files-without-match",
        "-c",
        "--count",
        "--count-matches",
        "-v",
        "--invert-match",
        "-o",
        "--only-matching",
        "-H",
        "--with-filename",
        "-I",
        "--no-filename",
        "--no-heading",
        "--heading",
        "-U",
        "--multiline",
        "--multiline-dotall",
        "-q",
        "--quiet",
        "--json",
        "--stats",
        "--files",
        "--column",
        "--no-messages",
        "--vimgrep",
        "--trim",
        "-x",
        "--line-regexp",
        "-a",
        "--text",
        "-P",
    ],
    values: &[
        ("-A", V::Num),
        ("--after-context", V::Num),
        ("-B", V::Num),
        ("--before-context", V::Num),
        ("-C", V::Num),
        ("--context", V::Num),
        ("-m", V::Num),
        ("--max-count", V::Num),
        ("-M", V::Num),
        ("--max-columns", V::Num),
        ("-d", V::Num),
        ("--max-depth", V::Num),
        ("-e", V::Pattern),
        ("--regexp", V::Pattern),
        ("-g", V::Text),
        ("--glob", V::Text),
        ("--iglob", V::Text),
        ("-t", V::Text),
        ("--type", V::Text),
        ("-T", V::Text),
        ("--type-not", V::Text),
        ("--color", V::Text),
    ],
    pos: Pos::PatternThenPaths,
    reads: true,
    num_flag: false,
};
const GREP: Spec = Spec {
    style: Style::Posix,
    flags: &[
        "-n",
        "--line-number",
        "-i",
        "--ignore-case",
        "-r",
        "--recursive",
        "-l",
        "--files-with-matches",
        "-L",
        "-c",
        "--count",
        "-v",
        "--invert-match",
        "-w",
        "--word-regexp",
        "-F",
        "--fixed-strings",
        "-E",
        "--extended-regexp",
        "-H",
        "--with-filename",
        "-h",
        "--no-filename",
        "-o",
        "--only-matching",
        "-I",
        "-q",
        "--quiet",
        "-s",
        "--no-messages",
        "-x",
        "--line-regexp",
        "-a",
        "--text",
    ],
    values: &[
        ("-A", V::Num),
        ("--after-context", V::Num),
        ("-B", V::Num),
        ("--before-context", V::Num),
        ("-C", V::Num),
        ("--context", V::Num),
        ("-m", V::Num),
        ("--max-count", V::Num),
        ("-e", V::Pattern),
        ("--regexp", V::Pattern),
        ("--include", V::Text),
        ("--exclude", V::Text),
        ("--exclude-dir", V::Text),
        ("--color", V::Text),
    ],
    pos: Pos::PatternThenPaths,
    reads: true,
    num_flag: false,
};
const FINDSTR: Spec = Spec {
    style: Style::Slash,
    flags: &[
        "/i", "/n", "/s", "/r", "/l", "/v", "/m", "/b", "/e", "/x", "/p", "/o",
    ],
    values: &[("/c", V::Pattern)],
    pos: Pos::PatternThenPaths,
    reads: true,
    num_flag: false,
};
const WHERE: Spec = Spec {
    style: Style::Posix,
    flags: &[],
    values: &[],
    pos: Pos::Names,
    reads: false,
    num_flag: false,
};

/// What an argument list contained.
#[derive(Debug, Default)]
struct Parsed {
    /// `-Filter` / `-Include` values (for the file-lister + `Select-String` rule).
    filters: Vec<String>,
    /// Number of path arguments (explicit files to read).
    paths: usize,
    /// Options seen (lowercase, as written in the spec).
    seen: Vec<String>,
}

fn num_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())
}

fn check_value(kind: V, val: &str, name: &str, scope: &ReadScope, reads: bool) -> Verdict<()> {
    match kind {
        V::Num => {
            if num_ok(val) {
                Ok(())
            } else {
                deny(format!("`{name}` espera un número"))
            }
        }
        V::Path => check_path(val, scope, reads),
        V::Text => match credential_in_token(val) {
            Some(c) => Err(cred_denial(c)),
            None => Ok(()),
        },
        V::Pattern => Ok(()),
    }
}

fn prop_ok(s: &str) -> bool {
    let mut ch = s.chars();
    match ch.next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '*' => {}
        _ => return false,
    }
    s.len() <= 64
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '*'))
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn lookup<'a>(spec: &'a Spec, name: &str) -> Option<Result<(), (&'a str, V)>> {
    if spec.flags.contains(&name) {
        return Some(Ok(()));
    }
    spec.values
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, k)| Err((*n, *k)))
}

fn parse_args(spec: &Spec, args: &[Tok], scope: &ReadScope) -> Verdict<Parsed> {
    let mut out = Parsed::default();
    let mut i = 0usize;
    let mut end_flags = false;
    let mut pattern_given = false;
    // positionals of `PatternThenPaths`: which one is the pattern is only known at the end
    let mut pending: Vec<String> = Vec::new();
    while i < args.len() {
        let t = &args[i];
        i += 1;
        let text = t.text.as_str();
        if spec.style == Style::Posix && !end_flags && text == "--" {
            end_flags = true;
            continue;
        }
        // A PowerShell cmdlet sees a quoted word as a value; a native program parses the text it
        // gets, quotes already removed.
        let is_flag = !end_flags
            && text.len() > 1
            && match spec.style {
                Style::Ps => text.starts_with('-') && !t.lead_quoted,
                Style::Posix => text.starts_with('-'),
                Style::Slash => text.starts_with('/') && !text.contains('\\'),
            };
        if !is_flag {
            match spec.pos {
                Pos::None => return deny(format!("argumento inesperado `{}`", short(text))),
                Pos::Paths => {
                    check_path(text, scope, spec.reads)?;
                    out.paths += 1;
                }
                Pos::PatternThenPaths => pending.push(text.to_string()),
                Pos::Props => {
                    if !prop_ok(text) {
                        return deny(format!("argumento no permitido `{}`", short(text)));
                    }
                }
                Pos::Literal => {}
                Pos::Names => {
                    if !name_ok(text) {
                        return deny(format!("argumento no permitido `{}`", short(text)));
                    }
                }
            }
            continue;
        }
        // an option
        if spec.num_flag && text[1..].bytes().all(|b| b.is_ascii_digit()) {
            if !num_ok(&text[1..]) {
                return deny("número demasiado grande");
            }
            continue;
        }
        match spec.style {
            Style::Ps | Style::Slash => {
                let (name, val) = match text.split_once(':') {
                    Some((n, v)) => (n.to_lowercase(), Some(v.to_string())),
                    None => (text.to_lowercase(), None),
                };
                match lookup(spec, &name) {
                    Some(Ok(())) => {
                        if val.is_some() {
                            return deny(format!("la opción `{name}` no lleva valor"));
                        }
                        out.seen.push(name);
                    }
                    Some(Err((n, kind))) => {
                        let v = match val {
                            Some(v) => v,
                            // findstr takes its values attached (`/c:text`)
                            None if spec.style == Style::Slash => {
                                return deny(format!("a `{name}` le falta el valor"))
                            }
                            None => match args.get(i) {
                                Some(v) => {
                                    i += 1;
                                    v.text.clone()
                                }
                                None => return deny(format!("a `{name}` le falta el valor")),
                            },
                        };
                        check_value(kind, &v, n, scope, spec.reads)?;
                        if kind == V::Path {
                            out.paths += 1;
                        }
                        if matches!(n, "-filter" | "-include") {
                            out.filters.push(v);
                        }
                        if matches!(n, "-pattern" | "/c") {
                            pattern_given = true;
                        }
                        out.seen.push(n.to_string());
                    }
                    None => return deny(format!("la opción `{}` no está permitida", short(text))),
                }
            }
            Style::Posix => {
                if let Some(long) = text.strip_prefix("--") {
                    let (name, val) = match long.split_once('=') {
                        Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                        None => (format!("--{long}"), None),
                    };
                    match lookup(spec, &name) {
                        Some(Ok(())) => {
                            if val.is_some() {
                                return deny(format!("la opción `{name}` no lleva valor"));
                            }
                            out.seen.push(name);
                        }
                        Some(Err((n, kind))) => {
                            let v = match val {
                                Some(v) => v,
                                None => match args.get(i) {
                                    Some(v) => {
                                        i += 1;
                                        v.text.clone()
                                    }
                                    None => return deny(format!("a `{name}` le falta el valor")),
                                },
                            };
                            check_value(kind, &v, n, scope, spec.reads)?;
                            if n == "--regexp" {
                                pattern_given = true;
                            }
                            out.seen.push(n.to_string());
                        }
                        None => {
                            return deny(format!("la opción `{}` no está permitida", short(text)))
                        }
                    }
                } else {
                    // a cluster of short options: `-ni`, `-A3`, `-nA 3`
                    let mut rest = &text[1..];
                    while let Some(c) = rest.chars().next() {
                        let name = format!("-{c}");
                        rest = &rest[c.len_utf8()..];
                        match lookup(spec, &name) {
                            Some(Ok(())) => out.seen.push(name),
                            Some(Err((n, kind))) => {
                                let v = if rest.is_empty() {
                                    match args.get(i) {
                                        Some(v) => {
                                            i += 1;
                                            v.text.clone()
                                        }
                                        None => {
                                            return deny(format!("a `{name}` le falta el valor"))
                                        }
                                    }
                                } else {
                                    std::mem::take(&mut rest).to_string()
                                };
                                check_value(kind, &v, n, scope, spec.reads)?;
                                if n == "-e" {
                                    pattern_given = true;
                                }
                                out.seen.push(n.to_string());
                            }
                            None => {
                                return deny(format!(
                                    "la opción `-{c}` no está permitida en `{}`",
                                    short(text)
                                ))
                            }
                        }
                    }
                }
            }
        }
    }
    // the first positional is the pattern unless an option gave it (or the command lists files);
    // every other one is a path, wherever the options were written
    let no_pattern = pattern_given || out.seen.iter().any(|s| s == "--files");
    for (n, p) in pending.iter().enumerate() {
        if n == 0 && !no_pattern {
            continue;
        }
        check_path(p, scope, spec.reads)?;
        out.paths += 1;
    }
    Ok(out)
}

fn short(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    t
}

// ---- git ---------------------------------------------------------------------------

/// `(name, takes a value)`; a value may follow as the next word only for [`GIT_SEPARATE`].
const GIT_COMMON: &[&str] = &[
    "--oneline",
    "--stat",
    "--shortstat",
    "--numstat",
    "--summary",
    "--name-only",
    "--name-status",
    "--graph",
    "--decorate",
    "--no-decorate",
    "--all",
    "--no-merges",
    "--merges",
    "--first-parent",
    "--reverse",
    "--follow",
    "-p",
    "--patch",
    "-u",
    "--no-patch",
    "--no-color",
    "--abbrev-commit",
    "--no-abbrev-commit",
    "--full-history",
    "--left-right",
    "--date-order",
    "--topo-order",
    "--author-date-order",
    "-i",
    "--regexp-ignore-case",
    "--invert-grep",
    "--branches",
    "--tags",
    "--remotes",
    "--cached",
    "--staged",
    "--check",
    "-w",
    "--ignore-all-space",
    "-b",
    "--ignore-space-change",
    "--ignore-blank-lines",
    "--minimal",
    "--patience",
    "--histogram",
    "--color-words",
    "-m",
    "--find-renames",
    "--find-copies",
    "-r",
    "--root",
    "--no-renames",
    "--name",
    "--stat-count",
    "--cc",
    "--textconv-never",
];
/// `--name=value` or `--name value`.
const GIT_SEPARATE: &[&str] = &[
    "-n",
    "--max-count",
    "--skip",
    "--since",
    "--until",
    "--after",
    "--before",
    "--author",
    "--committer",
    "-S",
    "-G",
    "-U",
    "--grep",
    "--diff-filter",
];
/// Only `--name=value`.
const GIT_EQ: &[&str] = &[
    "--stat",
    "--format",
    "--pretty",
    "--date",
    "--abbrev",
    "--stat-width",
    "--unified",
    "--color",
    "--decorate",
    "--word-diff",
    "--stat-name-width",
    "--relative-date",
    "--find-renames",
    "--find-copies",
    "--sort",
    "--exit-code",
    "--ignore-submodules",
    "--dirstat",
    "--abbrev-commit",
];
const GIT_STATUS: &[&str] = &[
    "-s",
    "--short",
    "-b",
    "--branch",
    "--porcelain",
    "--long",
    "-u",
    "--untracked-files",
    "--ignored",
    "--no-ahead-behind",
    "--show-stash",
    "-z",
    "--no-renames",
    "--renames",
];
const GIT_BRANCH: &[&str] = &[
    "--list",
    "-l",
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-v",
    "-vv",
    "--verbose",
    "--show-current",
    "--no-color",
    "--color",
    "--sort",
    "--format",
    "-i",
    "--ignore-case",
    "--abbrev",
    "--no-abbrev",
];
const GIT_LSFILES: &[&str] = &[
    "-c",
    "--cached",
    "-o",
    "--others",
    "-m",
    "--modified",
    "-d",
    "--deleted",
    "-s",
    "--stage",
    "-i",
    "--ignored",
    "--exclude-standard",
    "--full-name",
    "-z",
    "--directory",
    "--no-empty-directory",
];
const GIT_BLAME: &[&str] = &[
    "-w",
    "-M",
    "-C",
    "-e",
    "--show-email",
    "-p",
    "--porcelain",
    "--line-porcelain",
    "-l",
    "-t",
    "-f",
    "--show-name",
    "-n",
    "--show-number",
    "-s",
    "--root",
    "--date",
    "--abbrev",
];
const GIT_REVPARSE: &[&str] = &[
    "--show-toplevel",
    "--git-dir",
    "--is-inside-work-tree",
    "--abbrev-ref",
    "--short",
    "--verify",
    "-q",
    "--quiet",
    "--symbolic-full-name",
    "--show-prefix",
    "--show-cdup",
    "--is-bare-repository",
    "--git-common-dir",
    "--absolute-git-dir",
];

fn git_flag_allowed(sub: &str, name: &str) -> bool {
    match sub {
        "log" | "show" | "diff" => {
            GIT_COMMON.contains(&name) || GIT_SEPARATE.contains(&name) || GIT_EQ.contains(&name)
        }
        "status" => GIT_STATUS.contains(&name),
        "branch" => GIT_BRANCH.contains(&name),
        "ls-files" => GIT_LSFILES.contains(&name),
        "blame" => GIT_BLAME.contains(&name) || name == "-L",
        "rev-parse" => GIT_REVPARSE.contains(&name),
        _ => false,
    }
}

fn git_value_flag(sub: &str, name: &str) -> Option<bool> {
    // Some(true): separate value allowed, Some(false): only `=value`, None: no value
    if matches!(sub, "log" | "show" | "diff") {
        if GIT_SEPARATE.contains(&name) {
            return Some(true);
        }
        if GIT_EQ.contains(&name) {
            return Some(false);
        }
    }
    match (sub, name) {
        ("blame", "-L") => Some(true),
        ("blame", "--date" | "--abbrev") => Some(false),
        ("branch", "--sort" | "--format" | "--color" | "--abbrev") => Some(false),
        ("status", "--porcelain" | "-u" | "--untracked-files" | "--ignored") => Some(false),
        ("rev-parse", "--short" | "--abbrev-ref") => Some(false),
        _ => None,
    }
}

fn cred_denial(c: String) -> Denial {
    Denial {
        reason: format!("toca {c}, que parece un archivo de credenciales"),
        credential: true,
    }
}

fn check_git(args: &[Tok], scope: &ReadScope) -> Verdict<Parsed> {
    let mut i = 0usize;
    // global options: only --no-pager
    while i < args.len() && args[i].text.starts_with('-') {
        if args[i].text == "--no-pager" {
            i += 1;
        } else {
            return deny(format!(
                "la opción global de git `{}` no está permitida",
                short(&args[i].text)
            ));
        }
    }
    let Some(sub_tok) = args.get(i) else {
        return deny("git sin subcomando");
    };
    let sub = sub_tok.text.to_lowercase();
    i += 1;
    if !matches!(
        sub.as_str(),
        "status" | "log" | "diff" | "show" | "branch" | "ls-files" | "blame" | "rev-parse"
    ) {
        return deny(format!(
            "`git {}` no es un comando de consulta permitido",
            short(&sub)
        ));
    }
    let mut out = Parsed::default();
    let mut end_flags = false;
    let mut list_mode = false;
    let mut positionals: Vec<&str> = Vec::new();
    while i < args.len() {
        let text = args[i].text.as_str();
        i += 1;
        if !end_flags && text == "--" {
            end_flags = true;
            continue;
        }
        if end_flags || !text.starts_with('-') || text.len() < 2 {
            positionals.push(text);
            continue;
        }
        // `-5`
        if matches!(sub.as_str(), "log" | "show" | "diff")
            && text[1..].bytes().all(|b| b.is_ascii_digit())
        {
            if !num_ok(&text[1..]) {
                return deny("número demasiado grande");
            }
            continue;
        }
        let (name, attached): (String, Option<String>) = if let Some(long) = text.strip_prefix("--")
        {
            match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (text.to_string(), None),
            }
        } else if git_flag_allowed(&sub, text) {
            (text.to_string(), None)
        } else {
            // `-n5`, `-Sfoo`, `-U3`, `-L10`: a short option with its value attached
            match (text.get(..2), text.get(2..)) {
                (Some(opt), Some(val)) if git_value_flag(&sub, opt).is_some() => {
                    (opt.to_string(), Some(val.to_string()))
                }
                _ => {
                    return deny(format!(
                        "la opción `{}` de `git {sub}` no está permitida",
                        short(text)
                    ))
                }
            }
        };
        if !git_flag_allowed(&sub, &name) {
            return deny(format!(
                "la opción `{}` de `git {sub}` no está permitida",
                short(text)
            ));
        }
        match git_value_flag(&sub, &name) {
            None => {
                if attached.is_some() {
                    return deny(format!("la opción `{name}` no lleva valor"));
                }
            }
            Some(separate) => {
                let v = match attached {
                    Some(v) => Some(v),
                    None if separate => match args.get(i) {
                        Some(v) => {
                            i += 1;
                            Some(v.text.clone())
                        }
                        None => return deny(format!("a `{name}` le falta el valor")),
                    },
                    None => None,
                };
                if let Some(v) = v {
                    if let Some(c) = credential_in_token(&v) {
                        return Err(cred_denial(c));
                    }
                    if v.split(['/', '\\']).any(|c| c == "..") {
                        return deny("el valor de la opción usa `..` en una ruta");
                    }
                    if matches!(name.as_str(), "-n" | "--max-count" | "--skip" | "-U")
                        && !num_ok(&v)
                    {
                        return deny(format!("`{name}` espera un número"));
                    }
                }
            }
        }
        if matches!(name.as_str(), "--list" | "-l") {
            list_mode = true;
        }
        out.seen.push(name);
    }
    if sub == "branch" && !positionals.is_empty() && !list_mode {
        return deny("`git branch <nombre>` crearía una rama");
    }
    for text in positionals {
        // `rev:path` names a file of that revision
        if let Some((left, right)) = text.split_once(':') {
            if left.len() >= 2 {
                if let Some(c) = credential_in_token(left) {
                    return Err(cred_denial(c));
                }
                check_path(right, scope, true)?;
                out.paths += 1;
                continue;
            }
        }
        // a revision (`HEAD~3`, `main..feature`) or a path: both stay inside the project
        check_path(text, scope, true)?;
        out.paths += 1;
    }
    Ok(out)
}

// ---- programs and pipelines ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prog {
    List,
    Content,
    SelStr,
    TestPath,
    GetItem,
    Resolve,
    Pwd,
    Where,
    Write,
    Select,
    Sort,
    Measure,
    Format,
    OutString,
    Head,
    Wc,
    Rg,
    Grep,
    FindStr,
    Git,
}

fn program(name: &str) -> Option<Prog> {
    let l = name.to_ascii_lowercase();
    let l = l.strip_suffix(".exe").unwrap_or(&l);
    Some(match l {
        "get-childitem" | "gci" | "ls" | "dir" => Prog::List,
        "get-content" | "gc" | "cat" | "type" => Prog::Content,
        "select-string" | "sls" => Prog::SelStr,
        "test-path" => Prog::TestPath,
        "get-item" | "gi" => Prog::GetItem,
        "resolve-path" | "rvpa" => Prog::Resolve,
        "pwd" | "get-location" | "gl" => Prog::Pwd,
        "where" => Prog::Where,
        "write-output" | "write-host" | "echo" => Prog::Write,
        "select-object" | "select" => Prog::Select,
        "sort-object" | "sort" => Prog::Sort,
        "measure-object" | "measure" => Prog::Measure,
        "format-table" | "ft" | "format-list" | "fl" | "format-wide" | "fw" => Prog::Format,
        "out-string" => Prog::OutString,
        "head" | "tail" => Prog::Head,
        "wc" => Prog::Wc,
        "rg" => Prog::Rg,
        "grep" => Prog::Grep,
        "findstr" => Prog::FindStr,
        "git" => Prog::Git,
        _ => return None,
    })
}

fn is_consumer(p: Prog) -> bool {
    matches!(
        p,
        Prog::Select
            | Prog::Sort
            | Prog::Measure
            | Prog::Format
            | Prog::OutString
            | Prog::SelStr
            | Prog::Rg
            | Prog::Grep
            | Prog::FindStr
            | Prog::Head
            | Prog::Wc
    )
}

fn check_pipeline(pipeline: &[Vec<Tok>], scope: &ReadScope) -> Verdict<()> {
    // filters of the last file lister seen (Select-String over its objects reads the files)
    let mut lister: Option<Vec<String>> = None;
    for (idx, seg) in pipeline.iter().enumerate() {
        let Some(first) = seg.first() else {
            return deny("hay un `|` sin comando");
        };
        if first.quoted {
            return deny("el programa va entre comillas");
        }
        let name = first.text.as_str();
        if name.contains(['\\', '/', ':']) || name.starts_with('.') {
            return deny(format!(
                "`{}` se lanza por ruta; solo se permiten programas de consulta por su nombre",
                short(name)
            ));
        }
        let Some(prog) = program(name) else {
            return deny(format!(
                "`{}` no es un comando de consulta permitido",
                short(name)
            ));
        };
        if idx > 0 && !is_consumer(prog) {
            return deny(format!(
                "`{}` no puede recibir datos por tubería en solo lectura",
                short(name)
            ));
        }
        let args = &seg[1..];
        let parsed = match prog {
            Prog::Git => check_git(args, scope)?,
            Prog::List => parse_args(&PS_LIST, args, scope)?,
            Prog::Content => parse_args(&PS_CONTENT, args, scope)?,
            Prog::SelStr => parse_args(&PS_SELSTR, args, scope)?,
            Prog::TestPath => parse_args(&PS_TESTPATH, args, scope)?,
            Prog::GetItem => parse_args(&PS_GETITEM, args, scope)?,
            Prog::Resolve => parse_args(&PS_RESOLVE, args, scope)?,
            Prog::Pwd => parse_args(&PS_NONE, args, scope)?,
            Prog::Where => parse_args(&WHERE, args, scope)?,
            Prog::Write => parse_args(&PS_WRITE, args, scope)?,
            Prog::Select => parse_args(&PS_SELECT, args, scope)?,
            Prog::Sort => parse_args(&PS_SORT, args, scope)?,
            Prog::Measure => parse_args(&PS_MEASURE, args, scope)?,
            Prog::Format => parse_args(&PS_FORMAT, args, scope)?,
            Prog::OutString => parse_args(&PS_OUTSTRING, args, scope)?,
            Prog::Head => parse_args(&PX_HEAD, args, scope)?,
            Prog::Wc => parse_args(&PX_WC, args, scope)?,
            Prog::Rg => parse_args(&RG, args, scope)?,
            Prog::Grep => parse_args(&GREP, args, scope)?,
            Prog::FindStr => parse_args(&FINDSTR, args, scope)?,
        };
        if matches!(prog, Prog::List | Prog::GetItem) {
            lister = Some(parsed.filters.clone());
        }
        if prog == Prog::SelStr && parsed.paths == 0 {
            if let Some(filters) = &lister {
                let safe = !filters.is_empty()
                    && filters
                        .iter()
                        .all(|f| !wildcard_may_hit_credential(f.trim()));
                if !safe {
                    return Err(Denial {
                        reason: "Select-String sobre los archivos de Get-ChildItem leería también \
                                 archivos de credenciales; limita con -Filter (por ejemplo *.rs)"
                            .into(),
                        credential: true,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> ReadScope {
        ReadScope {
            cwd: PathBuf::from(r"C:\proj"),
            roots: Vec::new(),
            cargo_src: Some(PathBuf::from(r"C:\Users\u\.cargo\registry\src")),
        }
    }

    fn ok(cmd: &str) {
        if let Err(d) = read_only_verdict(cmd, &scope()) {
            panic!("should be read-only: {cmd:?} -> {}", d.reason);
        }
    }

    fn no(cmd: &str) {
        if read_only_verdict(cmd, &scope()).is_ok() {
            panic!("should be refused: {cmd:?}");
        }
    }

    fn no_cred(cmd: &str) {
        match read_only_verdict(cmd, &scope()) {
            Ok(()) => panic!("should be refused: {cmd:?}"),
            Err(d) => assert!(d.credential, "{cmd:?} -> {}", d.reason),
        }
    }

    #[test]
    fn inspection_commands_are_read_only() {
        for c in [
            "Get-ChildItem",
            "GET-CHILDITEM -Recurse",
            "Get-ChildItem -Path crates/app/src/app_shell, crates/store/src, crates/ui_kit/src -Recurse -File | Select-Object -ExpandProperty FullName",
            "ls",
            "dir",
            "gci -Recurse -Filter *.rs",
            "ls crates\\ui_kit\\src",
            "Get-Content crates/app/src/main.rs -TotalCount 50",
            "Get-Content -Path README.md -Tail 20",
            "cat README.md",
            "type README.md",
            "gc Cargo.toml | Select-String version",
            "Select-String -Path crates\\*\\src\\*.rs -Pattern \"fn main\"",
            "Select-String 'fn main' crates/app/src/main.rs",
            "rg -n \"prompt_for_paths|open_directory|pick_folder|rfd\" crates/",
            "rg -n --no-heading -i foo",
            "rg -nA3 foo crates",
            "rg -e foo -e bar crates",
            "rg --files crates/ui_kit",
            "rg -g \"*.rs\" -t rust foo",
            "rg \"Sesión externa\" crates",
            "grep -rn foo crates",
            "grep -rn -e foo crates/app",
            "findstr /s /i \"foo\" *.rs",
            "findstr /c:\"hello world\" Cargo.toml",
            "Test-Path crates/app",
            "Test-Path -Path Cargo.toml -PathType Leaf",
            "Get-Item Cargo.toml",
            "Resolve-Path .",
            "where git",
            "where.exe git",
            "pwd",
            "Get-Location",
            "Write-Output \"hola\"",
            "echo hi",
            "Write-Host \"=== GIT LOG ===\"",
            "git status",
            "git status -s",
            "git status --porcelain=v1 -b",
            "git log --oneline -n 10",
            "git log -5",
            "git log -n5 --stat",
            "git --no-pager log -3",
            "git log --since=\"2 weeks ago\" --author=me -- crates/app",
            "git log --format=\"%h %s\" -10",
            "git diff --stat",
            "git diff HEAD~1 -- crates/app",
            "git diff --cached --name-only",
            "git show HEAD --stat",
            "git show HEAD:Cargo.toml",
            "git branch --list",
            "git branch -a",
            "git branch --show-current",
            "git ls-files",
            "git ls-files -o --exclude-standard",
            "git blame -L 10,20 crates/app/src/main.rs",
            "git rev-parse --show-toplevel",
            "git rev-parse --abbrev-ref HEAD",
            "Get-ChildItem | Sort-Object Length -Descending | Select-Object -First 5",
            "Get-ChildItem -Recurse -Filter *.rs | Select-String foo",
            "Get-ChildItem | Measure-Object",
            "rg foo | Measure-Object -Line",
            "rg foo | head -n 20",
            "rg foo | tail -5",
            "rg foo | wc -l",
            "Get-Content a.txt | Select-String x | Select-Object -First 3",
            "Get-ChildItem | Format-Table Name, Length -AutoSize",
            "Get-ChildItem | Out-String -Width 200",
            "git log -n 10; Write-Output \"x\"",
            "Write-Output a;",
            "Get-ChildItem;;Get-Location",
        ] {
            ok(c);
        }
    }

    #[test]
    fn the_real_run_8_command_with_the_cargo_registry_is_read_only() {
        ok("Get-ChildItem \"$env:USERPROFILE\\.cargo\\registry\\src\\*\\taffy-0.9.0\" | Select-Object -ExpandProperty FullName; Write-Host \"=== GIT LOG ===\"; git log -n 10 --oneline");
        ok("Get-Content \"$env:USERPROFILE\\.cargo\\registry\\src\\x\\taffy-0.9.0\\src\\lib.rs\"");
        ok("Get-Content $HOME/.cargo/registry/src/x/a.rs");
        ok("rg flex \"$env:CARGO_HOME\\registry\\src\"");
        ok("Get-ChildItem ~/.cargo/registry/src");
        // the registry is reachable only through that exact prefix
        no("Get-Content $env:USERPROFILE\\.ssh\\id_rsa");
        no("Get-Content \"$env:USERPROFILE\\.cargo\\registry\\src\\..\\..\\credentials.toml\"");
        no("Get-Content $env:USERPROFILE\\.cargo\\credentials.toml");
        no("Get-ChildItem $env:USERPROFILE");
        no("Get-ChildItem $env:USERPROFILE\\.cargo\\registry\\srcx");
        no("Get-ChildItem ~");
        no("Get-ChildItem ~/.cargo");
        // without a registry in scope the variable is just a variable
        let none = ReadScope {
            cwd: PathBuf::from(r"C:\proj"),
            ..ReadScope::default()
        };
        assert!(!is_read_only_command(
            "Get-ChildItem $env:USERPROFILE\\.cargo\\registry\\src",
            &none
        ));
    }

    #[test]
    fn writes_redirections_and_blocks_are_refused() {
        for c in [
            "Get-ChildItem > out.txt",
            "Get-ChildItem >> out.txt",
            "echo x > a.txt",
            "echo x 1> a.txt",
            "cat a < b",
            "Get-Content a | Out-File b",
            "Get-Content a | Tee-Object b",
            "Get-Content a | Set-Content b",
            "Set-Content a b",
            "Add-Content a b",
            "Remove-Item x",
            "rm x",
            "del x",
            "rd x",
            "Copy-Item a b",
            "Move-Item a b",
            "New-Item x",
            "mkdir x",
            "ni x",
            "sc x y",
            "Rename-Item a b",
            "Set-Location ..",
            "cd ..",
            "Get-ChildItem | ForEach-Object { $_ }",
            "Get-ChildItem | % { $_ }",
            "Get-ChildItem | Where-Object { $_.Length -gt 1 }",
            "Get-ChildItem | where Length -gt 1",
            "Get-ChildItem | foreach Name",
            "Select-Object -Property @{n='x';e={calc}}",
            "Write-Output $(calc)",
            "Write-Output \"$(calc)\"",
            "Write-Output `calc`",
            "echo $env:PATH",
            "echo \"$env:PATH\"",
            "echo $HOME",
            "Get-Content $env:APPDATA\\x",
            "& calc",
            "& 'C:\\x\\y.exe'",
            ".\\evil.exe",
            "./rg foo",
            "C:\\tools\\rg.exe foo",
            "rg.exe\\..\\x",
            "\"rg\" foo",
            "Invoke-Expression 'calc'",
            "iex 'calc'",
            "Invoke-WebRequest http://x",
            "Invoke-RestMethod http://x",
            "Invoke-Command -ScriptBlock x",
            "Invoke-Item .",
            "ii .",
            "Start-Process calc",
            "start calc",
            "powershell -c calc",
            "pwsh -Command calc",
            "powershell -EncodedCommand AAAA",
            "cmd /c dir",
            "cmd.exe /c dir",
            "python -c print(1)",
            "node -e 1",
            "bash -c ls",
            "sh -c ls",
            "wsl ls",
            "find . -exec rm {} ;",
            "find . -delete",
            "find . -name x",
            "sed -i s/a/b/ f",
            "sed -n 1p f",
            "awk 1 f",
            "xargs rm",
            "curl http://x",
            "cargo build",
            "cargo test",
            "npm install",
            "Get-Process",
            "Get-Date",
            "Get-Variable",
            "Get-Service",
            "Get-ItemProperty HKLM:\\x",
            "reg query HKLM",
            "tasklist",
            "whoami",
            "set",
            "env",
            "printenv",
            "Get-ChildItem && Remove-Item x",
            "Get-ChildItem || Remove-Item x",
            "Get-ChildItem & Remove-Item x",
            "git status & calc",
            "git status && git push",
            "echo a & echo b",
            "Get-ChildItem | Invoke-Expression",
            "Get-ChildItem | cmd",
            "echo calc | powershell",
            "rg foo | xargs rm",
            "Get-ChildItem | Set-Content x",
            "Get-ChildItem | Get-Content",
            "Get-ChildItem | Remove-Item",
            "Get-ChildItem |",
            "| Get-ChildItem",
            "Get-ChildItem --% x",
            "Write-Output @(1,2)",
            "Write-Output @{a=1}",
            "[Environment]::GetEnvironmentVariable('X')",
            "Write-Output [int]",
            "Write-Output %USERPROFILE%",
            "echo \"%USERPROFILE%\"",
            "echo 'x' # c",
            "echo a ^ b",
            "echo (1)",
            "echo {1}",
        ] {
            no(c);
        }
    }

    #[test]
    fn options_that_run_or_write_are_refused() {
        for c in [
            "rg --pre cmd foo",
            "rg --pre=cmd foo",
            "rg --hidden x",
            "rg -uu x",
            "rg -u x",
            "rg --no-ignore x",
            "rg -z x",
            "rg --search-zip x",
            "rg -f pats x",
            "rg --follow x",
            "rg -L x",
            "rg --pre-glob '*.x' x",
            "rg --hostname-bin x y",
            "rg --ignore-file x y",
            "rg --unknown x",
            "rg -A x foo",
            "grep -R foo .",
            "grep -f pats x",
            "grep --exclude-from=x foo",
            "findstr /f:list foo",
            "findstr /g:list foo",
            "findstr /d:c:\\ foo *",
            "git -c core.pager=calc log",
            "git -ccore.x=y status",
            "git -C .. status",
            "git --git-dir=..\\x log",
            "git --exec-path=x status",
            "git --paginate log",
            "git log --output=x",
            "git diff --output x",
            "git diff --output=x",
            "git diff --ext-diff",
            "git diff --textconv",
            "git diff --no-index a b",
            "git log -O file",
            "git log -Ofile",
            "git log --exec=x",
            "git log --open-files-in-pager",
            "git push",
            "git commit -m x",
            "git checkout x",
            "git reset --hard",
            "git stash",
            "git clean -fd",
            "git fetch",
            "git pull",
            "git remote",
            "git config --get user.name",
            "git config -l",
            "git tag -l",
            "git branch newbranch",
            "git branch -d x",
            "git branch -D x",
            "git branch -m a b",
            "git branch --set-upstream-to=x",
            "git blame --contents file x",
            "git blame -S revs x",
            "git rev-parse --git-path x",
            "git",
            "git --no-pager",
            "Get-Content a -Wait",
            "Get-Content -Path a -Stream x",
            "Get-ChildItem -Recurse -Attributes x",
            "Get-ChildItem -Path:x -Foo",
            "Get-ChildItem -Rec:true",
            "Select-String -Pattern x -Path a -AllMatches:1",
            "Select-Object -Property",
            "Write-Output hi -Foo",
            "Write-Host hi -Foo",
            "head -f a",
            "tail -f a",
            "tail --follow a",
            "sort -o x y",
        ] {
            no(c);
        }
    }

    #[test]
    fn paths_must_stay_inside_the_project() {
        for c in [
            "Get-Content ..\\secret.txt",
            "Get-Content a\\..\\..\\x",
            "Get-Content \"C:\\proj\\..\\x\"",
            "Get-Content C:\\Windows\\win.ini",
            "Get-Content C:\\proj2\\a.txt",
            "Get-Content \\\\server\\share\\x",
            "Get-Content //server/x",
            "Get-Content \\\\?\\C:\\x",
            "Get-Content \\\\.\\pipe\\x",
            "Get-ChildItem C:\\",
            "Get-ChildItem /",
            "Get-ChildItem \\",
            "Get-ChildItem \\Windows",
            "Get-ChildItem /etc",
            "Get-Content ~\\.ssh\\id_rsa",
            "Get-Content ~/x",
            "Get-ChildItem env:",
            "Get-ChildItem Env:\\",
            "Get-ChildItem HKLM:\\",
            "Get-ChildItem Cert:\\",
            "Get-ChildItem variable:",
            "Get-Content CON",
            "Get-Content NUL",
            "Get-Content con.txt",
            "Get-Content COM1",
            "Get-Content file.txt:secret",
            "Get-Content C:foo",
            "Get-Content C:",
            "ls ...",
            "ls a.",
            "ls \"a \"",
            "Get-Content -Path:..\\x",
            "Get-Content -LiteralPath ..\\x",
            "rg foo ..",
            "rg foo ../other",
            "rg foo C:\\",
            "git log -- ..\\x",
            "git show HEAD:..\\x",
            "git diff ..\\a",
            "Test-Path ..\\x",
            "Resolve-Path ..",
            "Get-Item ..",
            "tail -n 5 ..\\x",
            "wc -l ..\\x",
        ] {
            no(c);
        }
        // a relative or absolute path inside is fine, whatever the case or the separator
        ok("Get-Content C:\\proj\\a.txt");
        ok("Get-Content c:/PROJ/a.txt");
        ok("Get-Content \"C:\\proj\\sub dir\\a.txt\"");
        ok("Get-Content .\\a.txt");
        ok("Get-ChildItem C:\\proj");
        ok("Get-ChildItem .");
    }

    #[test]
    fn credential_files_are_refused_as_credentials() {
        for c in [
            "Get-Content .env",
            "cat .env.local",
            "type id_rsa",
            "rg foo .ssh",
            "Get-Content .aws\\credentials",
            "Get-Content .ssh\\config",
            "git show HEAD:.env",
            "git show HEAD:.ssh/id_rsa",
            "git log -p -- .env",
            "git diff -- secrets.yml",
            "Get-Content .npmrc",
            "Get-Content .netrc",
            "cat secrets.json",
            "Get-Content \".ENV\"",
            "Get-Content .git/config",
            "Get-Content .git\\config",
            "cat server.pem",
            "cat key.ppk",
            "Get-Content -Path:.env",
            "Get-Content -Path .env",
            "Get-Content -LiteralPath .env",
            "type 'C:\\proj\\.env'",
            "rg foo .env",
            "Select-String x -Path .env",
            "Select-String -Pattern x -Path sub\\.env.production",
            "Get-Content a,.env",
            "Get-Content .e*",
            "Get-Content *",
            "Get-Content .*",
            "Get-Content *.pem",
            "Get-Content id_*",
            "Get-Content .s*\\id_rsa",
            "rg foo crates\\*",
            "rg foo .s*",
            "Select-String foo *",
            "Get-ChildItem | Select-String foo",
            "Get-ChildItem -Recurse | Select-String foo",
            "Get-ChildItem -Filter * | Select-String foo",
            "Get-ChildItem -Filter *.* | Select-String foo",
            "Get-ChildItem -Include .env | Select-String foo",
            "Get-ChildItem -Filter .env",
            "Get-Item .env",
            "Test-Path .env",
        ] {
            no_cred(c);
        }
        // names that only look similar are fine
        ok("Get-Content .env.example");
        ok("Get-Content .env.sample");
        ok("Get-Content environment.rs");
        ok("Get-Content crates\\*\\src\\*.rs");
        ok("rg foo crates\\*\\src");
        ok("Get-ChildItem *");
        ok("Get-ChildItem -Recurse");
        ok("rg \"load_dotenv\" src");
        ok("rg \"\\.env\" src");
    }

    #[test]
    fn quoting_and_unicode_tricks_are_refused() {
        for c in [
            // smart quotes and dashes are quotes and dashes for PowerShell
            "Get-Content \u{201c}.env\u{201d}",
            "Get-Content \u{2018}.env\u{2019}",
            "Get-ChildItem \u{2013}Recurse",
            "Get-ChildItem \u{2014}Recurse",
            "Get-ChildItem\u{200b}",
            "Get-ChildItem\u{a0}-Recurse",
            "Get-ChildItem\u{2028}Remove-Item x",
            "Get-Content a\u{85}b",
            "Get-Content \u{202e}txt.exe",
            "Get-ChildItem\u{feff}",
            "echo \u{1f600}",
            // doubled and escaped quotes: shells disagree about where the string ends
            "rg \"a\"\";calc;\"\"b\"",
            "rg 'a'';calc;''b'",
            "rg \"a\\\" ; calc ; \\\"\"",
            "rg a\\\"b",
            "echo \"unterminated",
            "echo 'unterminated",
            // control characters and new lines
            "Get-ChildItem\nRemove-Item x",
            "Get-ChildItem\r\nRemove-Item x",
            "Get-ChildItem\rRemove-Item x",
            "Get-ChildItem\u{0b}Remove-Item x",
            "Get-ChildItem\u{0c}Remove-Item x",
            "Get-ChildItem\0",
            "Get-ChildItem\u{1b}[31m",
            // cmd does not quote with '
            "rg 'a|b'",
            "rg 'a&b'",
            "rg 'a>b' x",
            "rg 'a^b'",
            "rg \"a$b\"",
            "rg \"a`b\"",
            "rg \"%PATH%\"",
            "rg \"%h%\"",
            // empty / cut
            "",
            "   ",
            ";",
            "Get-ChildItem …",
            "Get-ChildItem ...",
        ] {
            no(c);
        }
        // the safe forms of the same things
        ok("rg \"a|b\"");
        ok("rg 'a b'");
        ok("rg \"a'b\"");
        ok("rg 'a\"b'");
        ok("rg \"\" src");
        ok("rg '$x' src");
        ok("Get-ChildItem\tsrc");
        ok("rg ñandú src");
    }

    #[test]
    fn an_over_long_command_is_refused() {
        let long = format!("Write-Output {}", "a".repeat(MAX_COMMAND_CHARS));
        no(&long);
    }

    #[test]
    fn denial_reasons_are_spanish_and_say_what_is_wrong() {
        let d = read_only_verdict("Remove-Item x", &scope()).unwrap_err();
        assert!(d.reason.contains("Remove-Item"), "{}", d.reason);
        let d = read_only_verdict("Get-ChildItem > a", &scope()).unwrap_err();
        assert!(d.reason.contains("redirige"), "{}", d.reason);
        let d = read_only_verdict("Get-Content .env", &scope()).unwrap_err();
        assert!(d.credential && d.reason.contains(".env"), "{}", d.reason);
        let d = read_only_verdict("Get-Content ..\\x", &scope()).unwrap_err();
        assert!(!d.credential);
    }

    #[test]
    fn the_credential_rules_are_shared() {
        assert!(credential_path(".env").is_some());
        assert!(credential_path("C:\\x\\.ssh\\id_rsa").is_some());
        assert!(credential_path("--key-file=.npmrc").is_some());
        assert!(credential_path(".env.example").is_none());
        assert!(credential_path("src/lib.rs").is_none());
        assert!(credential_path(".git/config").is_some());
        assert!(credential_path(".git/HEAD").is_none());
        assert!(credential_in_token("HEAD:.env").is_some());
        assert!(credential_in_token("a,.ssh/x").is_some());
        assert!(credential_in_token("HEAD:src/lib.rs").is_none());
        assert!(wildcard_may_hit_credential("*"));
        assert!(wildcard_may_hit_credential(".e*"));
        assert!(wildcard_may_hit_credential("*.pem"));
        assert!(!wildcard_may_hit_credential("*.rs"));
        assert!(!wildcard_may_hit_credential("main.*"));
    }

    #[test]
    fn option_terminator_and_option_order_do_not_hide_paths() {
        ok("grep -e foo -- src");
        no("grep -e foo -- ../x");
        no("grep foo -- ../x");
        // `-e` after the first word: that word is a path, not the pattern
        no("rg ../../secret -e x");
        no("rg ../../secret -e x src");
        ok("rg src/main.rs -e x");
        no(r"Select-String ..\x -Pattern y");
        // quoted option-looking words are options for native programs, values for cmdlets
        no("rg \"--pre=calc\" foo");
        no("rg --pre=\"calc\" foo");
        no("git \"-c\" x log");
        no("git log \"--output=x\"");
    }
}
