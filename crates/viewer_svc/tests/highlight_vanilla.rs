#![allow(clippy::unwrap_used, clippy::expect_used)]
// The vanilla grammar set (the `default` features): JSON, TOML, CSS, HTML, Markdown.
// Every other language is a plugin, see tests/grammars*.rs.
#![cfg(all(
    feature = "lang-json",
    feature = "lang-toml",
    feature = "lang-css",
    feature = "lang-html",
    feature = "lang-markdown"
))]

use std::ops::Range;
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
use viewer_svc::available_languages;
use viewer_svc::{Document, HighlightKind, Lang, Limits};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

type LineSpans = Vec<(Range<usize>, HighlightKind)>;

fn open(name: &str) -> (Vec<String>, Vec<LineSpans>) {
    let d = Document::open_lazy(&fixture(name), Limits::default()).unwrap();
    d.wait_indexed();
    d.highlight_blocking();
    let n = d.line_count();
    (
        d.lines(0..n).into_iter().map(|c| c.into_owned()).collect(),
        d.highlight_lines(0..n),
    )
}

fn has(name: &str, kind: HighlightKind, needle: &str) -> bool {
    let (lines, spans) = open(name);
    lines.iter().zip(&spans).any(|(l, sp)| {
        sp.iter()
            .any(|(r, k)| *k == kind && l[r.clone()].contains(needle))
    })
}

#[test]
fn vanilla_set_is_exactly_five_languages() {
    #[cfg(not(any(
        feature = "lang-rust",
        feature = "lang-typescript",
        feature = "lang-javascript",
        feature = "lang-python",
        feature = "lang-bash",
        feature = "lang-csharp"
    )))]
    assert_eq!(
        available_languages(),
        vec![
            Lang::Json,
            Lang::Toml,
            Lang::Markdown,
            Lang::Css,
            Lang::Html
        ]
    );
    for l in [
        Lang::Json,
        Lang::Toml,
        Lang::Markdown,
        Lang::Css,
        Lang::Html,
    ] {
        assert!(l.has_grammar(), "{l:?}");
    }
}

#[cfg(not(any(
    feature = "lang-rust",
    feature = "lang-typescript",
    feature = "lang-javascript",
    feature = "lang-python",
    feature = "lang-bash",
    feature = "lang-csharp"
)))]
#[test]
fn other_languages_need_a_plugin() {
    for l in [
        Lang::Rust,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::Python,
        Lang::Bash,
        Lang::CSharp,
        Lang::Yaml,
        Lang::PowerShell,
    ] {
        assert!(!l.has_grammar(), "{l:?}");
    }
    // no registry, no plugin: plain text, no panic
    let (_, spans) = open("sample.rs");
    assert!(spans.iter().all(Vec::is_empty));
    let (_, spans) = open("sample.cs");
    assert!(spans.iter().all(Vec::is_empty));
}

#[test]
fn json() {
    assert!(has("sample.json", HighlightKind::String, "\"selachi\""));
    assert!(has("sample.json", HighlightKind::Number, "3.14"));
    assert!(has("sample.json", HighlightKind::Property, "\"name\""));
}

#[test]
fn toml() {
    assert!(has("sample.toml", HighlightKind::String, "\"selachi\""));
    assert!(has("sample.toml", HighlightKind::Property, "name"));
}

#[test]
fn css() {
    assert!(has("sample.css", HighlightKind::Comment, "/* tight */"));
    assert!(has("sample.css", HighlightKind::Property, "color"));
}

#[test]
fn html() {
    assert!(has("sample.html", HighlightKind::Tag, "body"));
    assert!(has("sample.html", HighlightKind::Attribute, "class"));
}

#[test]
fn markdown() {
    let (lines, spans) = open("sample.md");
    assert!(!spans[0].is_empty(), "{:?}", lines[0]);
    assert!(spans.iter().any(|s| !s.is_empty()));
}
