#![allow(clippy::unwrap_used, clippy::expect_used)]
// These tests need the default grammar set.
#![cfg(all(
    feature = "lang-rust",
    feature = "lang-typescript",
    feature = "lang-javascript",
    feature = "lang-json",
    feature = "lang-toml",
    feature = "lang-python",
    feature = "lang-markdown",
    feature = "lang-css",
    feature = "lang-html",
    feature = "lang-bash",
    feature = "lang-csharp"
))]

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use viewer_svc::{available_languages, Document, HighlightKind, Lang, Limits};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

type LineSpans = Vec<(Range<usize>, HighlightKind)>;

fn open(name: &str) -> Document {
    let d = Document::open_lazy(&fixture(name), Limits::default()).unwrap();
    d.wait_indexed();
    d.highlight_blocking();
    d
}

fn all_lines(d: &Document) -> (Vec<String>, Vec<LineSpans>) {
    let n = d.line_count();
    let lines = d.lines(0..n).into_iter().map(|c| c.into_owned()).collect();
    (lines, d.highlight_lines(0..n))
}

/// Text of every span of `kind`.
fn texts_of(d: &Document, kind: HighlightKind) -> Vec<String> {
    let (lines, spans) = all_lines(d);
    let mut out = Vec::new();
    for (line, sp) in lines.iter().zip(&spans) {
        for (r, k) in sp {
            if *k == kind {
                out.push(line[r.clone()].to_string());
            }
        }
    }
    out
}

fn has(d: &Document, kind: HighlightKind, needle: &str) -> bool {
    texts_of(d, kind).iter().any(|t| t.contains(needle))
}

#[test]
fn default_feature_set_is_available() {
    let langs = available_languages();
    for l in [
        Lang::Rust,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::Json,
        Lang::Toml,
        Lang::Python,
        Lang::Markdown,
        Lang::Css,
        Lang::Html,
        Lang::Bash,
        Lang::CSharp,
    ] {
        assert!(langs.contains(&l), "{l:?} missing");
        assert!(l.has_grammar());
    }
    assert!(!Lang::Yaml.has_grammar());
}

#[test]
fn rust() {
    let d = open("sample.rs");
    assert!(has(&d, HighlightKind::Keyword, "fn"));
    assert!(has(&d, HighlightKind::Keyword, "pub"));
    assert!(has(&d, HighlightKind::Comment, "// print it"));
    assert!(has(&d, HighlightKind::String, "\"origin\""));
    assert!(has(&d, HighlightKind::Number, "42"));
    assert!(has(&d, HighlightKind::Type, "Point"));
    assert!(has(&d, HighlightKind::Function, "new"));
}

#[test]
fn typescript() {
    let d = open("sample.ts");
    assert!(has(&d, HighlightKind::Keyword, "interface"));
    assert!(has(&d, HighlightKind::Keyword, "return"));
    assert!(has(&d, HighlightKind::Comment, "// greeting"));
    assert!(has(&d, HighlightKind::Type, "string"));
    assert!(has(&d, HighlightKind::Type, "User"));
    assert!(has(&d, HighlightKind::String, "hello"));
}

#[test]
fn javascript() {
    let d = open("sample.js");
    assert!(has(&d, HighlightKind::Keyword, "const"));
    assert!(has(&d, HighlightKind::Keyword, "function"));
    assert!(has(&d, HighlightKind::Number, "42"));
    assert!(has(&d, HighlightKind::Comment, "// sum"));
}

#[test]
fn json() {
    let d = open("sample.json");
    assert!(has(&d, HighlightKind::String, "\"selachi\""));
    assert!(has(&d, HighlightKind::Number, "3.14"));
    assert!(has(&d, HighlightKind::Property, "\"name\""));
}

#[test]
fn toml() {
    let d = open("sample.toml");
    assert!(has(&d, HighlightKind::String, "\"selachi\""));
    assert!(has(&d, HighlightKind::Property, "name"));
}

#[test]
fn python() {
    let d = open("sample.py");
    assert!(has(&d, HighlightKind::Keyword, "def"));
    assert!(has(&d, HighlightKind::Keyword, "import"));
    assert!(has(&d, HighlightKind::Comment, "# greeting"));
    assert!(has(&d, HighlightKind::Function, "greet"));
}

#[test]
fn css() {
    let d = open("sample.css");
    assert!(has(&d, HighlightKind::Comment, "/* tight */"));
    assert!(has(&d, HighlightKind::Property, "color"));
}

#[test]
fn html_with_injected_css_and_js() {
    let d = open("sample.html");
    assert!(has(&d, HighlightKind::Tag, "body"));
    assert!(has(&d, HighlightKind::Attribute, "class"));
    // <script>const a = 1;</script> is highlighted as JavaScript
    assert!(has(&d, HighlightKind::Keyword, "const"));
}

#[test]
fn bash() {
    let d = open("sample.sh");
    assert!(has(&d, HighlightKind::Keyword, "if"));
    assert!(has(&d, HighlightKind::Comment, "# greet"));
    assert!(has(&d, HighlightKind::String, "world"));
}

#[test]
fn csharp() {
    let d = open("sample.cs");
    assert!(has(&d, HighlightKind::Keyword, "public"));
    assert!(has(&d, HighlightKind::Keyword, "namespace"));
    assert!(has(&d, HighlightKind::Comment, "// greet"));
}

#[test]
fn markdown_with_fenced_code_injection() {
    let d = open("sample.md");
    let (lines, spans) = all_lines(&d);
    assert!(!spans.iter().all(Vec::is_empty));
    // the heading is highlighted
    assert!(!spans[0].is_empty(), "{:?}", lines[0]);
    // the rust fence body `fn main() {}` has a keyword
    let fence = lines.iter().position(|l| l == "fn main() {}").unwrap();
    assert!(spans[fence]
        .iter()
        .any(|(r, k)| *k == HighlightKind::Keyword && &lines[fence][r.clone()] == "fn"));
}

#[test]
fn spans_are_line_relative_sorted_and_in_bounds() {
    let d = open("sample.rs");
    let (lines, spans) = all_lines(&d);
    assert_eq!(lines.len(), spans.len());
    for (line, sp) in lines.iter().zip(&spans) {
        let mut prev_end = 0;
        for (r, k) in sp {
            assert!(r.start >= prev_end, "overlap in {line:?}: {sp:?}");
            assert!(r.start < r.end && r.end <= line.len(), "{line:?} {r:?}");
            assert!(line.is_char_boundary(r.start) && line.is_char_boundary(r.end));
            assert_ne!(*k, HighlightKind::Plain);
            prev_end = r.end;
        }
    }
}

#[test]
fn visible_range_matches_whole_file_result() {
    let d = open("sample.rs");
    let n = d.line_count();
    let all = d.highlight_lines(0..n);
    let window = d.highlight_lines(5..12);
    assert_eq!(window.len(), 7);
    assert_eq!(&all[5..12], &window[..]);
    // past the end is clamped
    assert_eq!(d.highlight_lines(n - 2..n + 50).len(), 2);
    assert!(d.highlight_lines(n + 5..n + 9).is_empty());
}

#[test]
fn window_fallback_before_background_parse_finishes() {
    // open_lazy never starts the background parse, so highlight_lines must work on
    // the visible window alone.
    let d = Document::open_lazy(&fixture("sample.rs"), Limits::default()).unwrap();
    assert!(!d.highlight_ready());
    let spans = d.highlight_lines(11..13);
    assert_eq!(spans.len(), 2);
    let lines = d.lines(11..13);
    // line 11 (0-based) is `pub fn new(...)`
    assert!(
        spans[0]
            .iter()
            .any(|(r, k)| *k == HighlightKind::Keyword && &lines[0][r.clone()] == "pub"),
        "{:?} {:?}",
        lines[0],
        spans[0]
    );
}

#[test]
fn background_parse_becomes_ready() {
    let d = Document::open(&fixture("sample.rs"), Limits::default()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !d.highlight_ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "highlight never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(has(&d, HighlightKind::Keyword, "fn"));
}

#[test]
fn big_files_are_plain() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.rs");
    fs::write(&p, "fn main() {}\n".repeat(1000)).unwrap();
    let limits = Limits {
        max_highlight_bytes: 100,
        ..Limits::default()
    };
    let d = Document::open_lazy(&p, limits).unwrap();
    d.wait_indexed();
    d.highlight_blocking();
    assert!(!d.highlight_ready());
    let spans = d.highlight_lines(0..5);
    assert_eq!(spans.len(), 5);
    assert!(spans.iter().all(Vec::is_empty));
}

#[test]
fn plain_text_and_unknown_languages_have_empty_spans() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.txt");
    fs::write(&p, "fn main() {}\n").unwrap();
    let d = Document::open_lazy(&p, Limits::default()).unwrap();
    let spans = d.highlight_lines(0..1);
    assert_eq!(spans, vec![Vec::new()]);
    // YAML has no bundled grammar
    let d = open("sample.yaml");
    assert!(d.highlight_lines(0..3).iter().all(Vec::is_empty));
}

#[test]
fn crlf_does_not_shift_spans() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn a() {}\r\nfn b() {}\r\n").unwrap();
    let d = Document::open_lazy(&p, Limits::default()).unwrap();
    d.wait_indexed();
    d.highlight_blocking();
    let lines = d.lines(0..2);
    let spans = d.highlight_lines(0..2);
    for (l, sp) in lines.iter().zip(&spans) {
        assert!(sp
            .iter()
            .any(|(r, k)| *k == HighlightKind::Keyword && &l[r.clone()] == "fn"));
    }
}

#[test]
fn utf16_file_highlights_on_transcoded_text() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.rs");
    let mut bytes = vec![0xFF, 0xFE];
    for u in "fn héllo() {}\n".encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    fs::write(&p, bytes).unwrap();
    let d = Document::open_lazy(&p, Limits::default()).unwrap();
    d.wait_indexed();
    d.highlight_blocking();
    let l = d.line(0).unwrap().into_owned();
    let sp = &d.highlight_lines(0..1)[0];
    assert!(sp
        .iter()
        .any(|(r, k)| *k == HighlightKind::Keyword && &l[r.clone()] == "fn"));
}
