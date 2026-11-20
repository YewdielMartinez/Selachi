//! Syntax highlighting: tree-sitter grammars behind cargo features, reduced to a
//! small stable [`HighlightKind`] enum the UI maps to theme colours.

use std::ops::Range;
use std::sync::atomic::AtomicUsize;

use crate::detect::Lang;

/// Stable, theme-agnostic highlight classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HighlightKind {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Constant,
    Variable,
    Property,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Plain,
}

/// A highlighted byte range of the document text (absolute offsets within the buffer
/// that was highlighted). Sorted, non-overlapping; gaps are [`HighlightKind::Plain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: HighlightKind,
}

/// Whether the grammar for `lang` was compiled in (cargo feature `lang-*`).
pub fn grammar_available(lang: Lang) -> bool {
    imp::available(lang)
}

/// Languages with a compiled-in grammar, in [`Lang::ALL`] order.
pub fn available_languages() -> Vec<Lang> {
    Lang::ALL
        .into_iter()
        .filter(|l| grammar_available(*l))
        .collect()
}

/// Highlight `bytes` as `lang`. Returns no spans when the grammar is missing, the
/// input is not valid UTF-8 (offsets would not map onto decoded text) or `cancel` is
/// raised (non-zero).
pub(crate) fn highlight_bytes(lang: Lang, bytes: &[u8], cancel: Option<&AtomicUsize>) -> Vec<Span> {
    if std::str::from_utf8(bytes).is_err() {
        return Vec::new();
    }
    imp::highlight(lang, bytes, cancel)
}

/// Cut `spans` (offsets relative to a buffer that starts at `base` within the text)
/// into per-line lists of line-relative ranges. `lines` are byte ranges in text coordinates.
pub(crate) fn slice_spans(
    spans: &[Span],
    base: usize,
    lines: &[Range<usize>],
) -> Vec<Vec<(Range<usize>, HighlightKind)>> {
    lines
        .iter()
        .map(|line| {
            let first = spans.partition_point(|s| base + s.end <= line.start);
            let mut out = Vec::new();
            for s in &spans[first..] {
                let (a, b) = (base + s.start, base + s.end);
                if a >= line.end {
                    break;
                }
                let a = a.max(line.start);
                let b = b.min(line.end);
                if a < b && s.kind != HighlightKind::Plain {
                    out.push((a - line.start..b - line.start, s.kind));
                }
            }
            out
        })
        .collect()
}

/// Map a tree-sitter capture name to a [`HighlightKind`] by its first dotted segment.
#[cfg(feature = "highlight")]
pub(crate) fn kind_for_capture(name: &str) -> HighlightKind {
    match name {
        "text.title" | "markup.heading" | "markup.strong" | "markup.italic" | "text.strong"
        | "text.emphasis" => return HighlightKind::Keyword,
        "text.literal" | "markup.raw" | "text.uri" | "markup.link" => return HighlightKind::String,
        "text.reference" | "markup.list" | "markup.quote" => return HighlightKind::Attribute,
        // JSON/TOML object keys.
        "string.special.key" => return HighlightKind::Property,
        _ => {}
    }
    let head = name.split('.').next().unwrap_or(name);
    match head {
        "keyword" | "conditional" | "repeat" | "include" | "storage" | "exception" => {
            HighlightKind::Keyword
        }
        "string" | "character" | "escape" => HighlightKind::String,
        "comment" => HighlightKind::Comment,
        "function" | "method" | "constructor" => HighlightKind::Function,
        "type" | "module" | "namespace" | "class" | "interface" | "enum" | "struct" => {
            HighlightKind::Type
        }
        "number" | "float" | "boolean" => HighlightKind::Number,
        "constant" | "label" => HighlightKind::Constant,
        "variable" | "parameter" | "identifier" | "embedded" | "local" => HighlightKind::Variable,
        "property" | "field" | "member" => HighlightKind::Property,
        "operator" => HighlightKind::Operator,
        "punctuation" | "delimiter" | "bracket" => HighlightKind::Punctuation,
        "tag" => HighlightKind::Tag,
        "attribute" => HighlightKind::Attribute,
        _ => HighlightKind::Plain,
    }
}

#[cfg(not(feature = "highlight"))]
mod imp {
    use super::*;

    pub fn available(_lang: Lang) -> bool {
        false
    }

    pub fn highlight(_lang: Lang, _bytes: &[u8], _cancel: Option<&AtomicUsize>) -> Vec<Span> {
        Vec::new()
    }
}

#[cfg(feature = "highlight")]
mod imp {
    use std::sync::OnceLock;

    use tree_sitter::Language;
    use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

    use super::*;

    /// Capture names handed to tree-sitter; the index is what comes back in events.
    const NAMES: &[&str] = &[
        "attribute",
        "boolean",
        "character",
        "comment",
        "conditional",
        "constant",
        "constant.builtin",
        "constructor",
        "delimiter",
        "embedded",
        "escape",
        "exception",
        "field",
        "float",
        "function",
        "function.builtin",
        "function.call",
        "function.macro",
        "function.method",
        "include",
        "keyword",
        "keyword.control",
        "keyword.function",
        "keyword.operator",
        "keyword.return",
        "label",
        "markup.heading",
        "markup.italic",
        "markup.link",
        "markup.list",
        "markup.quote",
        "markup.raw",
        "markup.strong",
        "method",
        "module",
        "namespace",
        "number",
        "operator",
        "parameter",
        "property",
        "property.builtin",
        "punctuation",
        "punctuation.bracket",
        "punctuation.delimiter",
        "punctuation.special",
        "repeat",
        "storage",
        "string",
        "string.escape",
        "string.regexp",
        "string.special",
        "string.special.key",
        "string.special.symbol",
        "string.special.uri",
        "tag",
        "tag.builtin",
        "text.emphasis",
        "text.literal",
        "text.reference",
        "text.strong",
        "text.title",
        "text.uri",
        "type",
        "type.builtin",
        "variable",
        "variable.builtin",
        "variable.member",
        "variable.parameter",
    ];

    /// One slot per [`Lang`] plus the markdown inline grammar.
    const SLOTS: usize = Lang::ALL.len() + 1;
    const MD_INLINE_SLOT: usize = Lang::ALL.len();

    static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; SLOTS] =
        [const { OnceLock::new() }; SLOTS];
    static KINDS: OnceLock<Vec<HighlightKind>> = OnceLock::new();

    fn slot(lang: Lang) -> usize {
        Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0)
    }

    fn kinds() -> &'static [HighlightKind] {
        KINDS.get_or_init(|| NAMES.iter().map(|n| kind_for_capture(n)).collect())
    }

    pub fn available(lang: Lang) -> bool {
        match lang {
            Lang::Rust => cfg!(feature = "lang-rust"),
            Lang::TypeScript | Lang::Tsx => cfg!(feature = "lang-typescript"),
            Lang::JavaScript => cfg!(feature = "lang-javascript"),
            Lang::Json => cfg!(feature = "lang-json"),
            Lang::Toml => cfg!(feature = "lang-toml"),
            Lang::Python => cfg!(feature = "lang-python"),
            Lang::Markdown => cfg!(feature = "lang-markdown"),
            Lang::Css => cfg!(feature = "lang-css"),
            Lang::Html => cfg!(feature = "lang-html"),
            Lang::Bash => cfg!(feature = "lang-bash"),
            Lang::CSharp => cfg!(feature = "lang-csharp"),
            // YAML and PowerShell have no bundled grammar (candidates for runtime plugins).
            Lang::Yaml | Lang::PowerShell => false,
        }
    }

    fn make(
        language: Language,
        name: &str,
        highlights: &str,
        injections: &str,
        locals: &str,
    ) -> Option<HighlightConfiguration> {
        let mut cfg =
            HighlightConfiguration::new(language, name, highlights, injections, locals).ok()?;
        cfg.configure(NAMES);
        Some(cfg)
    }

    fn build(lang: Lang) -> Option<HighlightConfiguration> {
        match lang {
            #[cfg(feature = "lang-rust")]
            Lang::Rust => make(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "lang-typescript")]
            Lang::TypeScript => {
                let combined = format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                );
                make(
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                    "typescript",
                    &combined,
                    "",
                    tree_sitter_typescript::LOCALS_QUERY,
                )
                .or_else(|| {
                    make(
                        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                        "typescript",
                        tree_sitter_typescript::HIGHLIGHTS_QUERY,
                        "",
                        "",
                    )
                })
            }
            #[cfg(feature = "lang-typescript")]
            Lang::Tsx => {
                let combined = format!(
                    "{}\n{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                );
                make(
                    tree_sitter_typescript::LANGUAGE_TSX.into(),
                    "tsx",
                    &combined,
                    "",
                    tree_sitter_typescript::LOCALS_QUERY,
                )
                .or_else(|| {
                    make(
                        tree_sitter_typescript::LANGUAGE_TSX.into(),
                        "tsx",
                        tree_sitter_typescript::HIGHLIGHTS_QUERY,
                        "",
                        "",
                    )
                })
            }
            #[cfg(feature = "lang-javascript")]
            Lang::JavaScript => {
                let combined = format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                );
                make(
                    tree_sitter_javascript::LANGUAGE.into(),
                    "javascript",
                    &combined,
                    "",
                    tree_sitter_javascript::LOCALS_QUERY,
                )
                .or_else(|| {
                    make(
                        tree_sitter_javascript::LANGUAGE.into(),
                        "javascript",
                        tree_sitter_javascript::HIGHLIGHT_QUERY,
                        "",
                        "",
                    )
                })
            }
            #[cfg(feature = "lang-json")]
            // The upstream query lists the object-key pattern first, so the generic
            // `(string)` pattern overrides it; restate the key pattern last.
            Lang::Json => make(
                tree_sitter_json::LANGUAGE.into(),
                "json",
                &format!(
                    "{}
(pair key: (_) @string.special.key)
",
                    tree_sitter_json::HIGHLIGHTS_QUERY
                ),
                "",
                "",
            ),
            #[cfg(feature = "lang-toml")]
            Lang::Toml => make(
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "lang-python")]
            Lang::Python => make(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "lang-markdown")]
            Lang::Markdown => make(
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
                tree_sitter_md::INJECTION_QUERY_BLOCK,
                "",
            ),
            #[cfg(feature = "lang-css")]
            Lang::Css => make(
                tree_sitter_css::LANGUAGE.into(),
                "css",
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "lang-html")]
            Lang::Html => make(
                tree_sitter_html::LANGUAGE.into(),
                "html",
                tree_sitter_html::HIGHLIGHTS_QUERY,
                tree_sitter_html::INJECTIONS_QUERY,
                "",
            ),
            #[cfg(feature = "lang-bash")]
            Lang::Bash => make(
                tree_sitter_bash::LANGUAGE.into(),
                "bash",
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "lang-csharp")]
            Lang::CSharp => make(
                tree_sitter_c_sharp::LANGUAGE.into(),
                "c-sharp",
                tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            _ => None,
        }
    }

    fn config(lang: Lang) -> Option<&'static HighlightConfiguration> {
        if !available(lang) {
            return None;
        }
        CONFIGS[slot(lang)].get_or_init(|| build(lang)).as_ref()
    }

    #[cfg(feature = "lang-markdown")]
    fn md_inline() -> Option<&'static HighlightConfiguration> {
        CONFIGS[MD_INLINE_SLOT]
            .get_or_init(|| {
                make(
                    tree_sitter_md::INLINE_LANGUAGE.into(),
                    "markdown_inline",
                    tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                    tree_sitter_md::INJECTION_QUERY_INLINE,
                    "",
                )
            })
            .as_ref()
    }

    #[cfg(not(feature = "lang-markdown"))]
    fn md_inline() -> Option<&'static HighlightConfiguration> {
        let _ = MD_INLINE_SLOT;
        None
    }

    fn injection(name: &str) -> Option<&'static HighlightConfiguration> {
        if name == "markdown_inline" {
            return md_inline();
        }
        Lang::from_name(name).and_then(config)
    }

    pub fn highlight(lang: Lang, bytes: &[u8], cancel: Option<&AtomicUsize>) -> Vec<Span> {
        let Some(cfg) = config(lang) else {
            return Vec::new();
        };
        let mut highlighter = Highlighter::new();
        // The closure (not the fn item) lets the returned config borrow shorten to the call.
        #[allow(clippy::redundant_closure)]
        let events = highlighter.highlight(cfg, bytes, cancel, |n| injection(n));
        let Ok(events) = events else {
            return Vec::new();
        };
        let kinds = kinds();
        let mut stack: Vec<HighlightKind> = Vec::new();
        let mut out: Vec<Span> = Vec::new();
        for ev in events {
            match ev {
                Ok(HighlightEvent::HighlightStart(Highlight(i))) => {
                    stack.push(kinds.get(i).copied().unwrap_or(HighlightKind::Plain));
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    let mut kind = stack.last().copied().unwrap_or(HighlightKind::Plain);
                    if start >= end || kind == HighlightKind::Plain {
                        continue;
                    }
                    // Several grammars tag numeric literals as plain constants.
                    if kind == HighlightKind::Constant
                        && bytes.get(start).is_some_and(u8::is_ascii_digit)
                    {
                        kind = HighlightKind::Number;
                    }
                    match out.last_mut() {
                        Some(prev) if prev.kind == kind && prev.end == start => prev.end = end,
                        _ => out.push(Span { start, end, kind }),
                    }
                }
                // Cancelled or parse failure: give up cleanly with whatever is plain.
                Err(_) => return Vec::new(),
            }
        }
        out
    }
}
