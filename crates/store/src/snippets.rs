//! Snippet templates (A12): `dotnet ef migrations add {nombre}`.
//!
//! Placeholder syntax:
//!
//! | form | meaning |
//! |---|---|
//! | `{nombre}` | required value |
//! | `{nombre:default}` | optional, `default` when empty |
//! | `{nombre\|a\|b}` | one of the options (the first is the default) |
//! | `{{` / `}}` | a literal `{` / `}` |
//!
//! A `{...}` whose content is not a valid placeholder (`awk '{print $1}'`) is kept
//! literally, so shell snippets with braces mostly just work.
//!
//! [`render`] can quote values for PowerShell so a value can never break out of its
//! argument (see [`Quoting`]).

use crate::error::{Result, StoreError};
use std::collections::HashMap;

/// A placeholder of a template (first appearance wins for default and options).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// Value used when none is given (`{n:20}`; the first option for `{a|x|y}`).
    pub default: Option<String>,
    /// Allowed values for the `{a|x|y}` form; empty = free text.
    pub options: Vec<String>,
}

/// How [`render`] writes values into the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quoting {
    /// Values are inserted verbatim. Use only for text that is not executed.
    Raw,
    /// PowerShell-safe: a value becomes one literal argument. Simple values
    /// (`feature/x`, `20`) stay bare, anything else is wrapped in single quotes
    /// with `'` doubled. A placeholder already inside `'...'` or `"..."` in the
    /// template is escaped for that quote instead. Values with line breaks or
    /// control characters are rejected.
    #[default]
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Lit(String),
    Param(Param),
}

/// The placeholders of `template`, once each, in order of first appearance.
pub fn parse_template(template: &str) -> Vec<Param> {
    let mut out: Vec<Param> = Vec::new();
    for tok in tokenize(template) {
        if let Token::Param(p) = tok {
            if !out.iter().any(|q| q.name == p.name) {
                out.push(p);
            }
        }
    }
    out
}

/// Fill `template` with `values`. Missing values fall back to the default; a
/// missing required value, a value outside the options, or (PowerShell mode) a
/// value with line breaks is an error. Extra values are ignored.
pub fn render(
    template: &str,
    values: &HashMap<String, String>,
    quoting: Quoting,
) -> Result<String> {
    let tokens = tokenize(template);
    let params = parse_template(template);
    let mut resolved: HashMap<&str, String> = HashMap::new();
    for p in &params {
        let given = values.get(&p.name).filter(|v| !v.is_empty());
        let value = match given {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => d.clone(),
                None => {
                    return Err(StoreError::Invalid(format!(
                        "missing value for parameter `{}`",
                        p.name
                    )))
                }
            },
        };
        if !p.options.is_empty() && !p.options.contains(&value) {
            return Err(StoreError::Invalid(format!(
                "`{value}` is not one of the options for `{}`: {}",
                p.name,
                p.options.join(", ")
            )));
        }
        resolved.insert(&p.name, value);
    }

    let mut out = String::with_capacity(template.len() + 16);
    for (i, tok) in tokens.iter().enumerate() {
        match tok {
            Token::Lit(s) => out.push_str(s),
            Token::Param(p) => {
                let value = resolved.get(p.name.as_str()).map_or("", String::as_str);
                match quoting {
                    Quoting::Raw => out.push_str(value),
                    Quoting::PowerShell => {
                        let before = i.checked_sub(1).and_then(|j| lit_last_char(&tokens[j]));
                        let after = tokens.get(i + 1).and_then(lit_first_char);
                        push_powershell(&mut out, &p.name, value, before, after)?;
                    }
                }
            }
        }
    }
    Ok(out)
}

fn lit_last_char(t: &Token) -> Option<char> {
    match t {
        Token::Lit(s) => s.chars().next_back(),
        Token::Param(_) => None,
    }
}

fn lit_first_char(t: &Token) -> Option<char> {
    match t {
        Token::Lit(s) => s.chars().next(),
        Token::Param(_) => None,
    }
}

fn is_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

fn is_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}')
}

fn push_powershell(
    out: &mut String,
    name: &str,
    value: &str,
    before: Option<char>,
    after: Option<char>,
) -> Result<()> {
    if value.chars().any(|c| c.is_control()) {
        return Err(StoreError::Invalid(format!(
            "parameter `{name}` contains a line break or control character"
        )));
    }
    match (before, after) {
        (Some(b), Some(a)) if is_single_quote(b) && is_single_quote(a) => {
            for c in value.chars() {
                out.push(c);
                if is_single_quote(c) {
                    out.push(c);
                }
            }
        }
        (Some(b), Some(a)) if is_double_quote(b) && is_double_quote(a) => {
            for c in value.chars() {
                if is_double_quote(c) || c == '`' || c == '$' {
                    out.push('`');
                }
                out.push(c);
            }
        }
        _ => {
            if !value.is_empty() && value.chars().all(is_bare_safe) {
                out.push_str(value);
            } else {
                out.push('\'');
                for c in value.chars() {
                    out.push(c);
                    if is_single_quote(c) {
                        out.push(c);
                    }
                }
                out.push('\'');
            }
        }
    }
    Ok(())
}

/// Characters PowerShell passes through as part of a bare word.
fn is_bare_safe(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '\\' | ':' | '+' | '=' | '-')
}

// ---- parsing ---------------------------------------------------------------

fn tokenize(template: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut lit = String::new();
    let mut chars = template.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' if matches!(chars.peek(), Some((_, '{'))) => {
                chars.next();
                lit.push('{');
            }
            '}' if matches!(chars.peek(), Some((_, '}'))) => {
                chars.next();
                lit.push('}');
            }
            '{' => {
                let rest = &template[i + 1..];
                match rest
                    .find('}')
                    .and_then(|end| parse_param(&rest[..end]).map(|p| (end, p)))
                {
                    Some((end, param)) => {
                        if !lit.is_empty() {
                            tokens.push(Token::Lit(std::mem::take(&mut lit)));
                        }
                        tokens.push(Token::Param(param));
                        // Skip the placeholder body and its closing brace.
                        let stop = i + 1 + end + 1;
                        while chars.peek().is_some_and(|(j, _)| *j < stop) {
                            chars.next();
                        }
                    }
                    None => lit.push('{'),
                }
            }
            _ => lit.push(c),
        }
    }
    if !lit.is_empty() {
        tokens.push(Token::Lit(lit));
    }
    tokens
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-')
}

/// `name`, `name:default`, `name|a|b`; `None` when the text is not a placeholder.
fn parse_param(inner: &str) -> Option<Param> {
    let name_end = inner
        .char_indices()
        .find(|(_, c)| !is_name_char(*c))
        .map_or(inner.len(), |(i, _)| i);
    let name = &inner[..name_end];
    if !name.chars().next().is_some_and(is_name_start) {
        return None;
    }
    let rest = &inner[name_end..];
    let (default, options) = if rest.is_empty() {
        (None, Vec::new())
    } else if let Some(d) = rest.strip_prefix(':') {
        (Some(d.to_string()), Vec::new())
    } else {
        let o = rest.strip_prefix('|')?;
        let options: Vec<String> = o.split('|').map(str::to_string).collect();
        if options.iter().any(String::is_empty) {
            return None;
        }
        (options.first().cloned(), options)
    };
    Some(Param {
        name: name.to_string(),
        default,
        options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn p(name: &str, default: Option<&str>, options: &[&str]) -> Param {
        Param {
            name: name.into(),
            default: default.map(Into::into),
            options: options.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn parses_the_three_forms() {
        assert_eq!(
            parse_template("a {x} b {n:20} c {mode|dev|prod}"),
            [
                p("x", None, &[]),
                p("n", Some("20"), &[]),
                p("mode", Some("dev"), &["dev", "prod"])
            ]
        );
    }

    #[test]
    fn repeated_names_are_listed_once() {
        let params = parse_template("{a} {b:1} {a:other} {b}");
        assert_eq!(params, [p("a", None, &[]), p("b", Some("1"), &[])]);
    }

    #[test]
    fn non_placeholders_stay_literal() {
        assert!(parse_template("awk '{print $1}'").is_empty());
        assert!(parse_template("echo {} and { x }").is_empty());
        let out = render(
            "awk '{print $1}' {f}",
            &vals(&[("f", "a.txt")]),
            Quoting::Raw,
        )
        .unwrap();
        assert_eq!(out, "awk '{print $1}' a.txt");
    }

    #[test]
    fn escaped_braces() {
        let out = render("echo {{x}} {y}", &vals(&[("y", "1")]), Quoting::Raw).unwrap();
        assert_eq!(out, "echo {x} 1");
        assert!(parse_template("{{x}}").is_empty());
    }

    #[test]
    fn unicode_names() {
        assert_eq!(parse_template("{opción} {ñandú:1}").len(), 2);
    }

    #[test]
    fn defaults_and_required() {
        let t = "git log --oneline -n {n:20} {rama}";
        assert_eq!(
            render(t, &vals(&[("rama", "main")]), Quoting::PowerShell).unwrap(),
            "git log --oneline -n 20 main"
        );
        assert_eq!(
            render(
                t,
                &vals(&[("rama", "main"), ("n", "5")]),
                Quoting::PowerShell
            )
            .unwrap(),
            "git log --oneline -n 5 main"
        );
        // empty string = not given
        assert_eq!(
            render(
                t,
                &vals(&[("rama", "main"), ("n", "")]),
                Quoting::PowerShell
            )
            .unwrap(),
            "git log --oneline -n 20 main"
        );
        assert!(render(t, &vals(&[]), Quoting::PowerShell).is_err());
    }

    #[test]
    fn options_are_enforced() {
        let t = "dotnet build -c {cfg|Debug|Release}";
        assert_eq!(
            render(t, &vals(&[]), Quoting::Raw).unwrap(),
            "dotnet build -c Debug"
        );
        assert_eq!(
            render(t, &vals(&[("cfg", "Release")]), Quoting::Raw).unwrap(),
            "dotnet build -c Release"
        );
        assert!(render(t, &vals(&[("cfg", "Other")]), Quoting::Raw).is_err());
    }

    #[test]
    fn powershell_quoting_table() {
        let t = "echo {v}";
        for (value, expected) in [
            ("simple", "echo simple"),
            ("feature/x-1", "echo feature/x-1"),
            ("C:\\a\\b.txt", "echo C:\\a\\b.txt"),
            ("two words", "echo 'two words'"),
            ("it's", "echo 'it''s'"),
            ("a;b", "echo 'a;b'"),
            ("a&b|c", "echo 'a&b|c'"),
            ("$env:PATH", "echo '$env:PATH'"),
            ("$(calc)", "echo '$(calc)'"),
            ("a,b", "echo 'a,b'"),
            ("@x", "echo '@x'"),
            ("it\u{2019}s", "echo 'it\u{2019}\u{2019}s'"),
            ("-x", "echo -x"),
        ] {
            assert_eq!(
                render(t, &vals(&[("v", value)]), Quoting::PowerShell).unwrap(),
                expected,
                "value {value:?}"
            );
        }
    }

    #[test]
    fn powershell_rejects_line_breaks() {
        for bad in ["a\nb", "a\rb", "a\u{0}b"] {
            assert!(render("echo {v}", &vals(&[("v", bad)]), Quoting::PowerShell).is_err());
        }
        // Raw mode does not care.
        assert_eq!(
            render("echo {v}", &vals(&[("v", "a\nb")]), Quoting::Raw).unwrap(),
            "echo a\nb"
        );
    }

    #[test]
    fn powershell_respects_surrounding_quotes() {
        assert_eq!(
            render(
                "git commit -m '{m}'",
                &vals(&[("m", "it's $x")]),
                Quoting::PowerShell
            )
            .unwrap(),
            "git commit -m 'it''s $x'"
        );
        assert_eq!(
            render(
                "git commit -m \"{m}\"",
                &vals(&[("m", "say \"hi\" $x `n")]),
                Quoting::PowerShell
            )
            .unwrap(),
            "git commit -m \"say `\"hi`\" `$x ``n\""
        );
        // Only one side quoted: treated as bare.
        assert_eq!(
            render("echo '{m}", &vals(&[("m", "a b")]), Quoting::PowerShell).unwrap(),
            "echo ''a b'"
        );
    }

    #[test]
    fn raw_inserts_verbatim() {
        assert_eq!(
            render("echo {v}", &vals(&[("v", "a b; c")]), Quoting::Raw).unwrap(),
            "echo a b; c"
        );
    }

    #[test]
    fn same_param_twice_uses_one_value() {
        assert_eq!(
            render(
                "mv {f} {f}.bak",
                &vals(&[("f", "a b")]),
                Quoting::PowerShell
            )
            .unwrap(),
            "mv 'a b' 'a b'.bak"
        );
    }
}
