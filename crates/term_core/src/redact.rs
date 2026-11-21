//! Secret redaction (A13): hides tokens and keys before terminal output leaves the machine
//! (copied blocks, prompts sent to an agent). Self-contained: no regex engine, ASCII scanning
//! over bytes so multi-byte text is never split.
//!
//! What is hidden (each hit is replaced by `[REDACTED]` and counted):
//! - PEM private key blocks (`-----BEGIN ... PRIVATE KEY-----` up to the matching `END`).
//! - Well-known token shapes: `sk-...`, `ghp_/gho_/ghu_/ghs_/ghr_...`, `github_pat_...`,
//!   `xoxb-/xoxa-/xoxp-/xoxr-/xoxs-...`, `AKIA.../ASIA...`, `AIza...`, JWTs (`eyJ...x.y.z`).
//! - `Bearer <token>` and `Authorization: Basic <token>`.
//! - `NAME=value` / `NAME = value` when `NAME` contains SECRET, TOKEN, PASSWORD, PASSWD, KEY or
//!   CREDENTIAL (case-insensitive): `.env` lines, `$env:API_KEY = "x"`, `password=hunter2`.
//! - `"name": "value"` (JSON/YAML-ish, value quoted) for the same names.
//! - CLI flags `--password x`, `--token x`, `--secret x`, `--api-key x` (and the `=` form).
//!
//! It is a safety net, not a guarantee: unknown token formats pass through.

/// Replacement text.
pub const REDACTED: &str = "[REDACTED]";

const KEY_WORDS: &[&str] = &["SECRET", "TOKEN", "PASSWORD", "PASSWD", "KEY", "CREDENTIAL"];
const FLAG_NAMES: &[&str] = &["password", "passwd", "token", "secret", "api-key", "apikey", "access-key", "secret-key"];

/// Redacts secrets in `text`; returns the new text and how many secrets were hidden.
pub fn redact(text: &str) -> (String, usize) {
    let mut total = 0;
    let (s, n) = redact_private_keys(text);
    total += n;
    let (s, n) = redact_tokens(&s);
    total += n;
    let (s, n) = redact_bearer(&s);
    total += n;
    let (s, n) = redact_key_values(&s);
    total += n;
    let (s, n) = redact_flags(&s);
    total += n;
    (s, total)
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn starts_with_ci(hay: &[u8], needle: &str) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

fn find_ci(hay: &[u8], from: usize, needle: &str) -> Option<usize> {
    (from..hay.len()).find(|&i| starts_with_ci(&hay[i..], needle))
}

fn redact_private_keys(text: &str) -> (String, usize) {
    const BEGIN: &str = "-----BEGIN ";
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut count) = (0, 0);
    while let Some(start) = find_ci(b, i, BEGIN) {
        // Only `... PRIVATE KEY-----` headers on the same line count.
        let line_end = b[start..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| start + p);
        let header = &text[start..line_end];
        if !(header.to_ascii_uppercase().contains("PRIVATE KEY-----")) {
            out.push_str(&text[i..start + BEGIN.len()]);
            i = start + BEGIN.len();
            continue;
        }
        out.push_str(&text[i..start]);
        out.push_str("[REDACTED PRIVATE KEY]");
        count += 1;
        i = match find_ci(b, line_end, "-----END ") {
            // Skip to the end of the END line's closing dashes.
            Some(e) => find_ci(b, e + 9, "-----").map_or(b.len(), |c| c + 5),
            None => b.len(),
        };
    }
    out.push_str(&text[i..]);
    (out, count)
}

/// Token shapes: `(prefix, min tail length, tail charset)`.
struct Shape {
    prefix: &'static str,
    min_tail: usize,
    tail: fn(u8) -> bool,
}

fn alnum_dash(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn alnum_us(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn alnum_hyphen(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn upper_digit(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn jwt_chars(b: u8) -> bool {
    alnum_dash(b) || b == b'.'
}

const SHAPES: &[Shape] = &[
    Shape { prefix: "sk-", min_tail: 16, tail: alnum_dash },
    Shape { prefix: "ghp_", min_tail: 20, tail: alnum_us },
    Shape { prefix: "gho_", min_tail: 20, tail: alnum_us },
    Shape { prefix: "ghu_", min_tail: 20, tail: alnum_us },
    Shape { prefix: "ghs_", min_tail: 20, tail: alnum_us },
    Shape { prefix: "ghr_", min_tail: 20, tail: alnum_us },
    Shape { prefix: "github_pat_", min_tail: 20, tail: alnum_us },
    Shape { prefix: "xoxb-", min_tail: 10, tail: alnum_hyphen },
    Shape { prefix: "xoxa-", min_tail: 10, tail: alnum_hyphen },
    Shape { prefix: "xoxp-", min_tail: 10, tail: alnum_hyphen },
    Shape { prefix: "xoxr-", min_tail: 10, tail: alnum_hyphen },
    Shape { prefix: "xoxs-", min_tail: 10, tail: alnum_hyphen },
    Shape { prefix: "AKIA", min_tail: 16, tail: upper_digit },
    Shape { prefix: "ASIA", min_tail: 16, tail: upper_digit },
    Shape { prefix: "AIza", min_tail: 30, tail: alnum_dash },
    Shape { prefix: "eyJ", min_tail: 20, tail: jwt_chars },
];

fn redact_tokens(text: &str) -> (String, usize) {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last, mut count) = (0, 0, 0);
    while i < b.len() {
        let boundary = i == 0 || !is_ident(b[i - 1]);
        if boundary
            && let Some(end) = match_shape(&b[i..]).map(|n| i + n)
        {
            out.push_str(&text[last..i]);
            out.push_str(REDACTED);
            count += 1;
            i = end;
            last = end;
            continue;
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// Length of a token starting at the beginning of `s`, if it matches a known shape.
fn match_shape(s: &[u8]) -> Option<usize> {
    for shape in SHAPES {
        let p = shape.prefix.as_bytes();
        if !s.starts_with(p) {
            continue;
        }
        let tail = s[p.len()..].iter().take_while(|&&c| (shape.tail)(c)).count();
        if tail < shape.min_tail {
            continue;
        }
        // JWTs need three dot-separated parts.
        if shape.prefix == "eyJ" && s[..p.len() + tail].iter().filter(|&&c| c == b'.').count() < 2 {
            continue;
        }
        return Some(p.len() + tail);
    }
    None
}

fn redact_bearer(text: &str) -> (String, usize) {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last, mut count) = (0, 0, 0);
    while i < b.len() {
        let boundary = i == 0 || !is_ident(b[i - 1]);
        let word = if !boundary {
            None
        } else if starts_with_ci(&b[i..], "bearer ") {
            Some(7)
        } else if starts_with_ci(&b[i..], "basic ") {
            Some(6)
        } else {
            None
        };
        if let Some(w) = word {
            let start = i + w;
            let len = b[start..].iter().take_while(|&&c| !c.is_ascii_whitespace() && c != b'"' && c != b'\'').count();
            // `Basic` is a common English word: require something credential-like.
            let min = if w == 7 { 8 } else { 12 };
            let token = &text[start..start + len];
            if len >= min && token != REDACTED && !token.starts_with(REDACTED) {
                out.push_str(&text[last..start]);
                out.push_str(REDACTED);
                count += 1;
                i = start + len;
                last = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

fn sensitive_name(name: &[u8]) -> bool {
    let upper = name.to_ascii_uppercase();
    let upper = String::from_utf8_lossy(&upper);
    KEY_WORDS.iter().any(|w| upper.contains(w))
}

/// Parses a value starting at `s[0]`: quoted (to the closing quote) or bare (to whitespace and
/// separators). Returns the range `(start, end)` to replace, quotes excluded.
fn value_span(s: &[u8]) -> Option<(usize, usize)> {
    match s.first()? {
        q @ (b'"' | b'\'') => {
            let close = s[1..].iter().position(|c| c == q)?;
            (close > 0).then_some((1, 1 + close))
        },
        _ => {
            let len = s.iter().take_while(|&&c| !c.is_ascii_whitespace() && !matches!(c, b'&' | b',' | b';' | b'"' | b'\'' | b')' | b'}')).count();
            (len > 0).then_some((0, len))
        },
    }
}

fn redact_key_values(text: &str) -> (String, usize) {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last, mut count) = (0, 0, 0);
    while i < b.len() {
        // A name starts at an identifier char that is not preceded by one.
        if !(is_ident(b[i]) && (i == 0 || !(is_ident(b[i - 1]) || b[i - 1] == b'-' || b[i - 1] == b'.'))) {
            i += 1;
            continue;
        }
        // Names may contain `-` and `.` (`api-key`, `app.secret`).
        let name_len = b[i..].iter().take_while(|&&c| is_ident(c) || c == b'-' || c == b'.').count();
        let name_end = i + name_len;
        let quoted_key = i > 0 && matches!(b[i - 1], b'"' | b'\'') && name_end < b.len() && b[name_end] == b[i - 1];
        let mut j = if quoted_key { name_end + 1 } else { name_end };
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        let sep = b.get(j).copied();
        let ok_sep = match sep {
            Some(b'=') => true,
            // `:` only in JSON/YAML shapes: quoted key, or a quoted value (checked below).
            Some(b':') => true,
            _ => false,
        };
        if ok_sep && sensitive_name(&b[i..name_end]) {
            let mut v = j + 1;
            // `==` or `:=` style: skip a second `=`.
            if sep == Some(b'=') && b.get(v) == Some(&b'=') {
                v += 1;
            }
            while v < b.len() && (b[v] == b' ' || b[v] == b'\t') {
                v += 1;
            }
            if let Some((s, e)) = value_span(&b[v..]) {
                let quoted_value = s == 1;
                let allowed = sep == Some(b'=') || quoted_key || quoted_value;
                let value = &text[v + s..v + e];
                if allowed && !value.starts_with(REDACTED) {
                    out.push_str(&text[last..v + s]);
                    out.push_str(REDACTED);
                    count += 1;
                    last = v + e;
                    i = v + e;
                    continue;
                }
            }
        }
        i = name_end.max(i + 1);
    }
    out.push_str(&text[last..]);
    (out, count)
}

fn redact_flags(text: &str) -> (String, usize) {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last, mut count) = (0, 0, 0);
    while i < b.len() {
        let at_flag = b[i] == b'-' && b.get(i + 1) == Some(&b'-') && (i == 0 || b[i - 1].is_ascii_whitespace());
        if at_flag {
            let name_start = i + 2;
            let name_len = b[name_start..].iter().take_while(|&&c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_').count();
            let name = &text[name_start..name_start + name_len];
            if FLAG_NAMES.iter().any(|f| name.eq_ignore_ascii_case(f)) {
                let mut v = name_start + name_len;
                if b.get(v) == Some(&b'=') || b.get(v) == Some(&b' ') {
                    v += 1;
                    if let Some((s, e)) = value_span(&b[v..]) {
                        let value = &text[v + s..v + e];
                        // Not another flag, not already hidden.
                        if !value.starts_with('-') && !value.starts_with(REDACTED) {
                            out.push_str(&text[last..v + s]);
                            out.push_str(REDACTED);
                            count += 1;
                            last = v + e;
                            i = v + e;
                            continue;
                        }
                    }
                }
            }
            i = name_start + name_len.max(1);
            continue;
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        let cases: &[(&str, &str, usize)] = &[
            // Token shapes.
            ("key sk-abcdefghijklmnop1234 end", "key [REDACTED] end", 1),
            ("sk-ant-api03-AbCdEf_ghIJKLmnop-qrstuv", "[REDACTED]", 1),
            ("export T=ghp_abcdefghijklmnopqrstuvwx", "export T=[REDACTED]", 1),
            ("github_pat_11ABCDEFG0abcdefghijklmnop_xyz", "[REDACTED]", 1),
            ("slack xoxb-1234567890-abcdefghij ok", "slack [REDACTED] ok", 1),
            ("AKIAIOSFODNN7EXAMPLE and ASIAIOSFODNN7EXAMPLE", "[REDACTED] and [REDACTED]", 2),
            ("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r", "[REDACTED]", 1),
            // Authorization.
            ("Authorization: Bearer abc123def456ghi", "Authorization: Bearer [REDACTED]", 1),
            ("curl -H 'authorization: bearer ZZZZZZZZZZ'", "curl -H 'authorization: bearer [REDACTED]'", 1),
            // name=value.
            ("password=hunter2 user=bob", "password=[REDACTED] user=bob", 1),
            ("DB_PASSWORD=\"p w d\"", "DB_PASSWORD=\"[REDACTED]\"", 1),
            ("API_KEY=abc\nSECRET_TOKEN = xyz\nHOME=C:\\x", "API_KEY=[REDACTED]\nSECRET_TOKEN = [REDACTED]\nHOME=C:\\x", 2),
            ("$env:GITHUB_TOKEN = 'abc123'", "$env:GITHUB_TOKEN = '[REDACTED]'", 1),
            ("https://x/y?token=abc123&page=2", "https://x/y?token=[REDACTED]&page=2", 1),
            ("client_secret: 'abc'", "client_secret: '[REDACTED]'", 1),
            ("{\"password\": \"s3cret\", \"user\": \"u\"}", "{\"password\": \"[REDACTED]\", \"user\": \"u\"}", 1),
            // Flags.
            ("tool --password hunter2 --verbose", "tool --password [REDACTED] --verbose", 1),
            ("tool --token=abc --other x", "tool --token=[REDACTED] --other x", 1),
            ("tool --api-key \"k k\"", "tool --api-key \"[REDACTED]\"", 1),
        ];
        for (input, want, n) in cases {
            let (got, count) = redact(input);
            assert_eq!((got.as_str(), count), (*want, *n), "input: {input:?}");
        }
    }

    #[test]
    fn private_key_blocks() {
        let text = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact(text), ("before\n[REDACTED PRIVATE KEY]\nafter".to_string(), 1));
        // Unterminated: everything to the end goes.
        let (s, n) = redact("x\n-----BEGIN PRIVATE KEY-----\nMIIE\nmore");
        assert_eq!((s.as_str(), n), ("x\n[REDACTED PRIVATE KEY]", 1));
        // A public key or certificate is not a secret.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIC\n-----END CERTIFICATE-----";
        assert_eq!(redact(cert), (cert.to_string(), 0));
    }

    #[test]
    fn harmless_text_is_untouched() {
        for t in [
            "total 4 files\nok",
            "task-force: sk-short",
            "the keyboard is nice",
            "token: expired",
            "Basic usage: tool [options]",
            "monkey=banana",
            "passwordless login",
            "ñandú 日本語 😀 — texto normal",
            "--verbose --output file",
            "AKIA short",
            "C:\\Users\\me\\sk-notes.txt",
        ] {
            let (s, n) = redact(t);
            // `monkey` contains KEY by design; it is the one expected over-match.
            if t == "monkey=banana" {
                assert_eq!((s.as_str(), n), ("monkey=[REDACTED]", 1));
            } else {
                assert_eq!((s.as_str(), n), (t, 0), "{t:?}");
            }
        }
    }

    #[test]
    fn redaction_is_idempotent_and_utf8_safe() {
        let input = "ñ password=abc 日本 sk-abcdefghijklmnop1234 😀 --token xyz";
        let (once, n) = redact(input);
        assert_eq!(n, 3);
        let (twice, m) = redact(&once);
        assert_eq!((twice.as_str(), m), (once.as_str(), 0));
        assert!(once.contains('ñ') && once.contains("日本") && once.contains('😀'));
    }
}
