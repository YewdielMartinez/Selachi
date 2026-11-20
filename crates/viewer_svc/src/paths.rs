//! Finding file paths (with optional line/column) in terminal output, for Ctrl+click.
//!
//! [`find_paths`] is purely lexical and fast (it runs on every visible line);
//! whether a candidate exists is answered lazily by [`resolve`] when the user hovers
//! or clicks.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use crate::detect::is_known_extension;

/// A path-looking span of a terminal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMatch {
    /// Byte range in the line to underline: the path and its `:line:col` / `(line,col)`
    /// suffix, without surrounding quotes or punctuation.
    pub range: Range<usize>,
    /// The path as typed (no quotes, no location suffix).
    pub raw: String,
    /// `raw` made absolute against `cwd` (`~` expanded, `.`/`..` folded). Not checked
    /// for existence, see [`resolve`].
    pub path: PathBuf,
    /// 1-based line, if the text carried one.
    pub line: Option<u32>,
    /// 1-based column, if the text carried one.
    pub col: Option<u32>,
}

/// A [`PathMatch`] that exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Check that `m` exists. Cheap (one `stat`), meant to be called on hover/click.
pub fn resolve(m: &PathMatch) -> Option<Resolved> {
    let meta = std::fs::metadata(&m.path).ok()?;
    Some(Resolved {
        path: m.path.clone(),
        is_dir: meta.is_dir(),
    })
}

impl PathMatch {
    /// See [`resolve`].
    pub fn resolve(&self) -> Option<Resolved> {
        resolve(self)
    }
}

/// Find every path-like token in `line`. Relative paths are resolved against `cwd`.
///
/// Recognised: `src/app.ts:42:7`, `C:\x\y.rs(12,5)` (MSVC), `./a/b`, `../a`, `~\x`,
/// `/usr/bin/env`, `\\server\share\f.txt`, bare `Cargo.toml` (known extensions),
/// `file.rs#L12`, and quoted paths (which may contain spaces), including Python's
/// `File "x.py", line 12`. URLs are skipped. Paths with spaces need quotes.
pub fn find_paths(line: &str, cwd: &Path) -> Vec<PathMatch> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let Some(c) = line[i..].chars().next() else {
            break;
        };
        let clen = c.len_utf8();
        if is_quote(c) {
            if let Some(rel) = line[i + clen..].find(c) {
                let close = i + clen + rel;
                let inner = &line[i + clen..close];
                if let Some(mut m) = analyze(inner, i + clen, true, cwd) {
                    // Python traceback: File "x.py", line 12
                    if m.line.is_none() {
                        if let Some((n, extra)) = python_line(&line[close + c.len_utf8()..]) {
                            m.line = Some(n);
                            m.range.end = close + c.len_utf8() + extra;
                        }
                    }
                    i = close + c.len_utf8();
                    out.push(m);
                    continue;
                }
            }
            i += clen;
            continue;
        }
        if c.is_whitespace() || is_terminator(c) {
            i += clen;
            continue;
        }
        let start = i;
        let mut end = i;
        for (off, ch) in line[i..].char_indices() {
            if ch.is_whitespace() || is_terminator(ch) || is_quote(ch) {
                break;
            }
            end = i + off + ch.len_utf8();
        }
        let end = extend_msvc_args(line, start, end);
        if let Some(m) = analyze(&line[start..end], start, false, cwd) {
            out.push(m);
        }
        i = end.max(i + clen);
    }
    out
}

/// `file.cpp(12, 5)`: the token stops at the space inside the parentheses, so pull in
/// the rest (` 5)`) when the token ends with `(<digits>,`.
fn extend_msvc_args(line: &str, start: usize, end: usize) -> usize {
    let tok = &line[start..end];
    let Some(args) = tok.strip_suffix(',').and_then(|t| t.rsplit_once('(')) else {
        return end;
    };
    if args.1.is_empty() || !args.1.bytes().all(|b| b.is_ascii_digit()) {
        return end;
    }
    let rest = &line[end..];
    let Some(tail) = rest.strip_prefix(' ') else {
        return end;
    };
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && tail[digits..].starts_with(')') {
        end + 1 + digits + 1
    } else {
        end
    }
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

/// Characters that end an unquoted token (invalid in Windows paths, or shell syntax).
fn is_terminator(c: char) -> bool {
    matches!(c, '<' | '>' | '|' | '*' | '?' | ';' | '\u{0}'..='\u{1f}')
}

/// `, line 12` right after a closing quote.
fn python_line(rest: &str) -> Option<(u32, usize)> {
    let tail = rest.strip_prefix(", line ")?;
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let n = tail[..digits].parse().ok()?;
    Some((n, ", line ".len() + digits))
}

fn analyze(tok: &str, offset: usize, quoted: bool, cwd: &Path) -> Option<PathMatch> {
    // 1. leading wrappers and `--flag=` prefixes
    let mut lead = 0;
    if !quoted {
        lead = tok.len() - tok.trim_start_matches(['(', '[', '{', '=']).len();
        let rest = &tok[lead..];
        if rest.starts_with('-') {
            if let Some(eq) = rest.find('=') {
                lead += eq + 1;
            }
        } else if let Some(eq) = rest.find('=') {
            // `KEY=value/path`: only when the key has no separators.
            if !rest[..eq].contains(['/', '\\', ':']) && eq > 0 {
                lead += eq + 1;
            }
        }
    }
    let body = &tok[lead..];
    if body.is_empty() || body.contains("://") {
        return None;
    }

    // 2. trailing punctuation, then the location suffix
    let trimmed = body.trim_end_matches(['.', ',', ';', ':', '!']);
    let (stem, line, col, consumed) = if let Some(r) = split_msvc(trimmed) {
        r
    } else {
        let t = trimmed
            .trim_end_matches([')', ']', '}', '\'', '"'])
            .trim_end_matches(['.', ',', ';', ':', '!']);
        split_colon(t)
    };
    let has_loc = line.is_some();
    if !is_path_like(stem, has_loc, quoted) {
        return None;
    }
    let start = offset + lead;
    Some(PathMatch {
        range: start..start + consumed,
        raw: stem.to_string(),
        path: absolutize(stem, cwd),
        line,
        col,
    })
}

/// `name(12)` / `name(12,5)` / `name(12, 5)` at the end. Returns
/// `(stem, line, col, bytes_consumed)`.
fn split_msvc(s: &str) -> Option<(&str, Option<u32>, Option<u32>, usize)> {
    let inner = s.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let args = &inner[open + 1..];
    let (l, c) = match args.split_once(',') {
        Some((l, c)) => (l.trim(), Some(c.trim())),
        None => (args.trim(), None),
    };
    let line: u32 = parse_num(l)?;
    let col = match c {
        Some(c) => Some(parse_num(c)?),
        None => None,
    };
    let stem = &s[..open];
    if stem.is_empty() {
        return None;
    }
    Some((stem, Some(line), col, s.len()))
}

/// `name:12`, `name:12:5`, `name#L12` at the end.
fn split_colon(s: &str) -> (&str, Option<u32>, Option<u32>, usize) {
    let none = (s, None, None, s.len());
    // GitHub style anchor.
    if let Some(p) = s.rfind("#L") {
        let tail = &s[p + 2..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        // allow `#L12-L20`
        if digits > 0 && (digits == tail.len() || tail[digits..].starts_with("-L")) && p > 0 {
            if let Some(n) = parse_num(&tail[..digits]) {
                return (&s[..p], Some(n), None, p + 2 + digits);
            }
        }
    }
    let Some((head, last)) = s.rsplit_once(':') else {
        return none;
    };
    let Some(n_last) = parse_num(last) else {
        return none;
    };
    // name:line:col ?
    if let Some((head2, mid)) = head.rsplit_once(':') {
        if let Some(n_mid) = parse_num(mid) {
            if head2.len() >= 2 {
                return (head2, Some(n_mid), Some(n_last), s.len());
            }
        }
    }
    if head.len() >= 2 {
        (head, Some(n_last), None, s.len())
    } else {
        none
    }
}

fn parse_num(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

fn last_segment(s: &str) -> &str {
    s.rsplit(is_sep).next().unwrap_or(s)
}

fn has_known_ext(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) => !stem.is_empty() && !ext.is_empty() && is_known_extension(ext),
        None => false,
    }
}

fn is_path_like(s: &str, has_loc: bool, quoted: bool) -> bool {
    if s.len() < 2 || s.chars().any(|c| c.is_control()) {
        return false;
    }
    if !quoted && s.chars().any(char::is_whitespace) {
        return false;
    }
    let b = s.as_bytes();

    // Drive letter: C:\x or C:/x
    if b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return true;
    }
    // UNC: \\server\share
    if let Some(rest) = s.strip_prefix("\\\\") {
        return rest.contains('\\') && !rest.starts_with('\\');
    }
    // Home
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return !rest.is_empty();
    }
    // Explicit relative
    for p in ["./", ".\\", "../", "..\\"] {
        if let Some(rest) = s.strip_prefix(p) {
            return !rest.is_empty();
        }
    }
    // Rooted (/usr/bin, \foo\bar): at least two segments or a known extension, and not
    // `//` comments or a lone `/c` command switch.
    if s.starts_with('/') || s.starts_with('\\') {
        let rest = &s[1..];
        if rest.starts_with('/') || rest.starts_with('*') || rest.is_empty() {
            return false;
        }
        return (rest.contains(is_sep) && !rest.ends_with(is_sep))
            || has_known_ext(last_segment(s))
            || has_loc;
    }
    if s.contains(is_sep) {
        let last = last_segment(s);
        // `dir/` with a trailing separator, or any file with a known extension.
        if last.is_empty() {
            return s.len() > 2 && s.trim_end_matches(is_sep).contains(['.', '_', '-']);
        }
        if s.starts_with('.') && s.len() > 2 {
            return true; // .github/workflows
        }
        return has_known_ext(last)
            || (has_loc && last.contains('.') && last.chars().any(char::is_alphabetic));
    }
    // Bare file name.
    has_known_ext(s)
        || (has_loc && s.contains('.') && !s.starts_with('.') && s.chars().any(char::is_alphabetic))
}

fn absolutize(raw: &str, cwd: &Path) -> PathBuf {
    let expanded: PathBuf = if raw == "~" {
        home_dir().unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        match home_dir() {
            Some(h) => h.join(rest),
            None => PathBuf::from(raw),
        }
    } else {
        PathBuf::from(raw)
    };
    normalize(&cwd.join(expanded))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Fold `.` and `..` lexically (no filesystem access).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if can_pop {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
