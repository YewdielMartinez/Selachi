//! Highlighting for fenced code blocks of a markdown file, through
//! `viewer_svc::highlight_text` (no temp files).

use viewer_svc::Lang;

use crate::spans::LineSpans;

/// The language of a code fence info string (`rust`, `rs`, `c#`, `ts`...).
pub fn lang_of_fence(info: &str) -> Option<Lang> {
    let word = info
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()
        .unwrap_or("")
        .trim();
    if word.is_empty() {
        return None;
    }
    let lower = word.to_ascii_lowercase();
    Lang::from_name(&lower)
        .or_else(|| Lang::from_extension(&lower))
        .or(match lower.as_str() {
            "c#" => Some(Lang::CSharp),
            "sh" | "shell" | "zsh" => Some(Lang::Bash),
            "ps1" | "pwsh" => Some(Lang::PowerShell),
            "js" => Some(Lang::JavaScript),
            "ts" => Some(Lang::TypeScript),
            "py" => Some(Lang::Python),
            "yml" => Some(Lang::Yaml),
            _ => None,
        })
}

/// Highlight spans per line of `text`, or empty spans when the language has no grammar here.
pub fn highlight_snippet(info: &str, text: &str) -> Vec<LineSpans> {
    let n = text.lines().count();
    let empty = || vec![Vec::new(); n];
    let Some(lang) = lang_of_fence(info) else {
        return empty();
    };
    if n == 0 || !lang.has_grammar() {
        return empty();
    }
    let mut spans = viewer_svc::highlight_text(lang.id(), text);
    spans.resize(n, Vec::new());
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_languages() {
        assert_eq!(lang_of_fence("rust"), Some(Lang::Rust));
        assert_eq!(lang_of_fence("rs"), Some(Lang::Rust));
        assert_eq!(lang_of_fence("c#"), Some(Lang::CSharp));
        assert_eq!(lang_of_fence("json5 title=x"), Some(Lang::Json));
        assert_eq!(lang_of_fence("toml"), Some(Lang::Toml));
        assert_eq!(lang_of_fence(""), None);
        assert_eq!(lang_of_fence("nonsense"), None);
    }

    #[test]
    fn snippet_returns_one_entry_per_line() {
        let spans = highlight_snippet("json", "{\n  \"a\": 1\n}\n");
        assert_eq!(spans.len(), 3);
        // The json grammar ships in the default build, so something is highlighted.
        assert!(spans.iter().any(|l| !l.is_empty()));
        let none = highlight_snippet("nonsense", "a\nb\n");
        assert_eq!(none.len(), 2);
        assert!(none.iter().all(Vec::is_empty));
        assert!(highlight_snippet("json", "").is_empty());
    }
}
