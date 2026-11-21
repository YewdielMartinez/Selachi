//! Secret redaction (A13): hide tokens, keys and passwords before text leaves
//! the machine (copying a block, sending a failed command to an agent...).
//!
//! The detectors extend the heuristics behind [`crate::looks_sensitive`]: that one
//! answers "should this command be kept out of the history?" and errs on the side
//! of flagging; this one *rewrites* text, so it is stricter about false positives
//! (git SHAs, UUIDs, base64 blobs and plain code like `token: String` are left
//! alone). Every hit is replaced by `«oculto:<tipo>»`.
//!
//! Implementation: every rule reports byte spans; spans from all rules are merged
//! (overlaps collapse into one) and the text is rebuilt once. Redacting an
//! already-redacted text is a no-op.

use regex::{Captures, Regex};
use std::sync::LazyLock;

/// The result of [`redact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    /// The text with every detected secret replaced by `«oculto:<tipo>»`.
    pub text: String,
    /// How many spans were replaced.
    pub count: usize,
    /// Distinct kinds found, in order of first appearance (`"github"`, `"jwt"`...).
    pub kinds: Vec<&'static str>,
}

impl Redacted {
    /// Nothing was hidden.
    pub fn is_clean(&self) -> bool {
        self.count == 0
    }

    fn unchanged(text: &str) -> Redacted {
        Redacted {
            text: text.to_string(),
            count: 0,
            kinds: Vec::new(),
        }
    }
}

/// Replace secrets in `text` by `«oculto:<tipo>»` placeholders.
///
/// Detects: `sk-ant-…`, `sk-…` (OpenAI), `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`/`github_pat_`,
/// `xox[bpasr]-` (Slack), `AKIA…`/`ASIA…` (AWS), `AIza…` (Google), `npm_…`, `hf_…`,
/// Stripe `sk_live_`/`sk_test_`, JWT (`eyJ….….…`), `Bearer …`/`Authorization:` values,
/// PEM private key blocks, credentials in URLs, `Password=…` in connection strings,
/// `password|token|secret|api_key = value` (also `--password value` flags and JSON/YAML
/// `"token": "…"`), and `.env` lines whose key contains SECRET/TOKEN/PASSWORD/KEY/PWD.
pub fn redact(text: &str) -> Redacted {
    if text.len() < 6 {
        return Redacted::unchanged(text);
    }
    let lc = text.to_ascii_lowercase();
    let has_keyword = KEYWORDS.iter().any(|k| lc.contains(k));

    let mut hits: Vec<Hit> = Vec::new();
    for rule in rules() {
        if rule.needs_keyword && !has_keyword {
            continue;
        }
        for caps in rule.re.captures_iter(text) {
            if let Some((start, end)) = (rule.extract)(&caps, text) {
                if start < end {
                    hits.push(Hit {
                        start,
                        end,
                        kind: rule.kind,
                    });
                }
            }
        }
    }
    if hits.is_empty() {
        return Redacted::unchanged(text);
    }

    // Earliest first; at the same start the longest wins (the sort is stable, so
    // a more specific rule registered first names the span).
    hits.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut merged: Vec<Hit> = Vec::with_capacity(hits.len());
    for h in hits {
        match merged.last_mut() {
            Some(cur) if h.start < cur.end => cur.end = cur.end.max(h.end),
            _ => merged.push(h),
        }
    }

    let mut out = String::with_capacity(text.len());
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut pos = 0;
    for h in &merged {
        out.push_str(&text[pos..h.start]);
        out.push_str("«oculto:");
        out.push_str(h.kind);
        out.push('»');
        pos = h.end;
        if !kinds.contains(&h.kind) {
            kinds.push(h.kind);
        }
    }
    out.push_str(&text[pos..]);
    Redacted {
        text: out,
        count: merged.len(),
        kinds,
    }
}

#[derive(Clone, Copy)]
struct Hit {
    start: usize,
    end: usize,
    kind: &'static str,
}

type Extract = fn(&Captures<'_>, &str) -> Option<(usize, usize)>;

struct Rule {
    kind: &'static str,
    re: Regex,
    extract: Extract,
    /// Only worth running when the text contains one of [`KEYWORDS`].
    needs_keyword: bool,
}

/// Lowercase substrings that gate the key/value rules (cheap pre-check).
const KEYWORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "api-key",
    "apikey",
    "access_key",
    "access-key",
    "accesskey",
    "private_key",
    "private-key",
    "privatekey",
    "credential",
    "authorization",
    "bearer",
    "key",
];

/// Words a generic key must contain (regex alternation).
const KEY_WORDS: &str =
    "password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credential";

/// Same words, for the post-check that rejects longer words (`tokenizer`).
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "api-key",
    "apikey",
    "access_key",
    "access-key",
    "accesskey",
    "private_key",
    "private-key",
    "privatekey",
    "credential",
];

/// Shared value shape: quoted (quotes kept) or bare.
const VALUE: &str = r#"(?:"(?P<dq>[^"\r\n]+)"|'(?P<sq>[^'\r\n]+)'|(?P<bare>[^\s,;"'&)}\]]+))"#;

fn rules() -> &'static [Rule] {
    static RULES: LazyLock<Vec<Rule>> = LazyLock::new(build_rules);
    &RULES
}

fn build_rules() -> Vec<Rule> {
    fn rule(kind: &'static str, pattern: &str, extract: Extract, needs_keyword: bool) -> Rule {
        // The patterns are literals of this file; a typo must show up in the tests.
        #[allow(clippy::expect_used)]
        let re = Regex::new(pattern).expect("valid redaction regex");
        Rule {
            kind,
            re,
            extract,
            needs_keyword,
        }
    }
    vec![
        rule(
            "clave-privada",
            r"-----BEGIN (?:[A-Z]+ )*PRIVATE KEY(?: BLOCK)?-----[\s\S]*?(?:-----END (?:[A-Z]+ )*PRIVATE KEY(?: BLOCK)?-----|\z)",
            whole,
            false,
        ),
        rule("anthropic", r"\bsk-ant-[A-Za-z0-9_-]{16,}", whole, false),
        rule(
            "openai",
            r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}",
            openai,
            false,
        ),
        rule(
            "github",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})",
            whole,
            false,
        ),
        rule("slack", r"\bxox[bpasr]-[A-Za-z0-9-]{10,}", whole, false),
        rule("aws", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", whole, false),
        rule("google", r"\bAIza[A-Za-z0-9_-]{35}", google, false),
        rule("npm", r"\bnpm_[A-Za-z0-9]{36}\b", whole, false),
        rule("huggingface", r"\bhf_[A-Za-z0-9]{30,}\b", whole, false),
        rule(
            "stripe",
            r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}",
            whole,
            false,
        ),
        rule(
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*",
            whole,
            false,
        ),
        rule(
            "bearer",
            r"(?i)\bbearer\s+([A-Za-z0-9._~+/=-]{16,})",
            token_value,
            true,
        ),
        rule(
            "basic",
            r#"(?i)\bauthorization["']?\s*[:=]\s*["']?basic\s+([A-Za-z0-9+/]{8,}={0,2})"#,
            group1,
            true,
        ),
        rule(
            "authorization",
            r#"(?i)\bauthorization["']?\s*[:=]\s*["']?(?:token\s+|apikey\s+)?([A-Za-z0-9._~+/=-]{16,})"#,
            token_value,
            true,
        ),
        rule(
            "url",
            r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@]+:([^\s/@]+)@",
            url_password,
            false,
        ),
        rule(
            "conexion",
            r#"(?i);\s*(?:password|pwd)\s*=\s*([^;"'\r\n]+)"#,
            conn_password,
            true,
        ),
        rule(
            "env",
            r#"(?m)^[ \t]*(?:export[ \t]+)?(?P<key>[A-Z][A-Z0-9_]*)[ \t]*=[ \t]*(?:"(?P<dq>[^"\r\n]*)"|'(?P<sq>[^'\r\n]*)'|(?P<bare>[^\s#]+))"#,
            env_line,
            true,
        ),
        rule(
            "flag",
            &format!(
                r"(?i)(?:^|\s)--?(?:password|passwd|pwd|token|secret|api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret)(?:\s+|=){VALUE}"
            ),
            flag_value,
            true,
        ),
        rule(
            "secreto",
            &format!(
                r#"(?i)(?P<key>[A-Za-z0-9_.-]*(?:{KEY_WORDS})[A-Za-z0-9_.-]*)["']?[ \t]*(?P<sep>[:=])[ \t]*{VALUE}"#
            ),
            key_value,
            true,
        ),
    ]
}

// ---- extractors ------------------------------------------------------------

fn whole(caps: &Captures<'_>, _: &str) -> Option<(usize, usize)> {
    caps.get(0).map(|m| (m.start(), m.end()))
}

fn group1(caps: &Captures<'_>, _: &str) -> Option<(usize, usize)> {
    caps.get(1).map(|m| (m.start(), m.end()))
}

/// `sk-...`: real keys always mix in digits; kebab-case prose does not.
fn openai(caps: &Captures<'_>, _: &str) -> Option<(usize, usize)> {
    let m = caps.get(0)?;
    m.as_str()
        .bytes()
        .any(|b| b.is_ascii_digit())
        .then_some((m.start(), m.end()))
}

/// Keys are not followed by more token characters (so base64 noise is ignored).
fn google(caps: &Captures<'_>, text: &str) -> Option<(usize, usize)> {
    let m = caps.get(0)?;
    let next = text[m.end()..].chars().next();
    (!next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .then_some((m.start(), m.end()))
}

/// Group 1 is a token: skip pure-letter words (prose such as "bearer authentication").
fn token_value(caps: &Captures<'_>, _: &str) -> Option<(usize, usize)> {
    let m = caps.get(1)?;
    (!m.as_str().bytes().all(|b| b.is_ascii_alphabetic())).then_some((m.start(), m.end()))
}

fn url_password(caps: &Captures<'_>, _: &str) -> Option<(usize, usize)> {
    let m = caps.get(1)?;
    (!is_placeholder(m.as_str())).then_some((m.start(), m.end()))
}

fn conn_password(caps: &Captures<'_>, _: &str) -> Option<(usize, usize)> {
    let m = caps.get(1)?;
    let value = m.as_str().trim_end();
    if value.is_empty() || is_placeholder(value) {
        return None;
    }
    Some((m.start(), m.start() + value.len()))
}

fn value_span(caps: &Captures<'_>) -> Option<(usize, usize, bool)> {
    if let Some(m) = caps.name("dq").or_else(|| caps.name("sq")) {
        Some((m.start(), m.end(), true))
    } else {
        caps.name("bare").map(|m| (m.start(), m.end(), false))
    }
}

fn flag_value(caps: &Captures<'_>, text: &str) -> Option<(usize, usize)> {
    let (s, e, quoted) = value_span(caps)?;
    let value = &text[s..e];
    if is_placeholder(value) || (!quoted && value.starts_with('-')) {
        return None;
    }
    Some((s, e))
}

/// Generic `key = value` / `key: value`.
fn key_value(caps: &Captures<'_>, text: &str) -> Option<(usize, usize)> {
    let key = caps.name("key")?.as_str();
    if !key_has_secret_word(key) {
        return None;
    }
    let (s, e, quoted) = value_span(caps)?;
    let value = &text[s..e];
    if is_placeholder(value) || is_type_or_keyword(value) {
        return None;
    }
    if quoted {
        // Quoted literals: anything that is not a reference or a type name.
        return (value.chars().count() >= 3).then_some((s, e));
    }
    if value.contains(['<', '>', '(']) || value.starts_with("//") {
        return None;
    }
    if is_urlish_without_credentials(value) || looks_like_path(value) {
        return None;
    }
    let colon = caps.name("sep")?.as_str() == ":";
    let has_digit = value.bytes().any(|b| b.is_ascii_digit());
    let shouty = !key.bytes().any(|b| b.is_ascii_lowercase());
    if is_identifier_path(value) {
        // `token = next_token` is code; `PASSWORD=letmein` is an env assignment.
        return (shouty && !colon && value.len() >= 4).then_some((s, e));
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return (value.len() >= 6).then_some((s, e));
    }
    // `key: word` (YAML/prose) needs a digit; `key=word` any mixed value.
    let long_enough = if colon {
        value.len() >= 6
    } else {
        value.len() >= 4
    };
    let plausible = !colon || has_digit;
    (long_enough && plausible).then_some((s, e))
}

/// `.env` line: the key (uppercase) must name a secret.
fn env_line(caps: &Captures<'_>, text: &str) -> Option<(usize, usize)> {
    let key = caps.name("key")?.as_str();
    if !env_key_is_secret(key) {
        return None;
    }
    let (s, e, quoted) = value_span(caps)?;
    let value = &text[s..e];
    if value.is_empty() || is_placeholder(value) || is_type_or_keyword(value) {
        return None;
    }
    if !quoted && (looks_like_path(value) || is_urlish_without_credentials(value)) {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) && value.len() < 6 {
        return None;
    }
    Some((s, e))
}

// ---- helpers ---------------------------------------------------------------

/// A key names a secret: a keyword that is not the prefix of a longer word
/// (`tokenizer`, `tokens`, `passwordless` are not secrets).
fn key_has_secret_word(key: &str) -> bool {
    let lc = key.to_ascii_lowercase();
    let bytes = key.as_bytes();
    for w in SECRET_WORDS {
        let mut from = 0;
        while let Some(i) = lc[from..].find(w) {
            let end = from + i + w.len();
            match bytes.get(end) {
                None => return true,
                Some(b) if !b.is_ascii_lowercase() => return true,
                _ => {}
            }
            from = end;
        }
    }
    false
}

/// `.env` key naming a secret: a `_`-separated segment is (or ends with) one of the words.
fn env_key_is_secret(key: &str) -> bool {
    key.split('_').any(|seg| {
        matches!(
            seg,
            "SECRET" | "TOKEN" | "PASSWORD" | "PASSWD" | "PWD" | "KEY" | "APIKEY"
        ) || seg.ends_with("SECRET")
            || seg.ends_with("TOKEN")
            || seg.ends_with("PASSWORD")
            || seg.ends_with("APIKEY")
    })
}

/// References, templates and masks: not literal secrets.
fn is_placeholder(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() {
        return true;
    }
    if v.starts_with(['$', '%', '{', '<', '[', '(', '@', '«', '`', '*', '#'])
        || v.starts_with("...")
    {
        return true;
    }
    let lc = v.to_ascii_lowercase();
    const REFS: &[&str] = &[
        "process.env",
        "os.environ",
        "getenv",
        "environ[",
        "env[",
        "environment.get",
        "config.",
        "settings.",
        "secrets.",
        "your_",
        "your-",
        "xxxx",
        "****",
    ];
    if REFS.iter().any(|r| lc.contains(r)) {
        return true;
    }
    // Masks like `••••` or `----`.
    let mut chars = v.chars();
    let first = chars.next();
    v.chars().count() >= 4
        && first.is_some_and(|c| !c.is_alphanumeric())
        && chars.all(|c| Some(c) == first)
}

/// Words that are types or literals, not values (`password: String`, `token = null`).
fn is_type_or_keyword(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "null"
            | "none"
            | "nil"
            | "undefined"
            | "true"
            | "false"
            | "string"
            | "str"
            | "bool"
            | "boolean"
            | "int"
            | "integer"
            | "number"
            | "object"
            | "any"
            | "required"
            | "optional"
            | "secret"
            | "password"
            | "token"
            | "empty"
            | "redacted"
            | "hidden"
    )
}

/// `foo`, `self.token`, `a.b_c` (identifiers without digits or symbols).
fn is_identifier_path(v: &str) -> bool {
    v.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && v.bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'.')
}

fn looks_like_path(v: &str) -> bool {
    let b = v.as_bytes();
    v.starts_with(['/', '~', '\\'])
        || v.starts_with("./")
        || v.starts_with("../")
        || (b.len() > 2 && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
}

/// `https://example.com/x`: a URL value is not a secret by itself (credentials
/// inside URLs are handled by the `url` rule).
fn is_urlish_without_credentials(v: &str) -> bool {
    v.contains("://") && !v.contains('@')
}

#[cfg(test)]
mod tests;
