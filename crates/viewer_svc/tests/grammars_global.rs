#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "plugins")]
//! Document highlighting through the process-wide plugin registry. Every test installs
//! its own registry, so they run one at a time (`SERIAL`). Tests with real compiled
//! plugins are `#[ignore]`d (run `packaging/grammars/build.ps1`, then `-- --ignored`).

mod common;

use std::fs;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};

use common::{copy_dist, fixture, scratch, Fake};
use viewer_svc::grammars::{set_plugin_registry, GrammarRegistry};
#[allow(unused_imports)]
use viewer_svc::Lang;
use viewer_svc::{Document, HighlightKind, Limits};

static SERIAL: Mutex<()> = Mutex::new(());

/// Serialises the test and clears the global registry when it ends.
struct Global(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Global {
    fn with(root: &std::path::Path) -> (Global, Arc<GrammarRegistry>) {
        let g = Global(SERIAL.lock().unwrap_or_else(|e| e.into_inner()));
        let reg = Arc::new(GrammarRegistry::new(root));
        set_plugin_registry(Some(Arc::clone(&reg)));
        (g, reg)
    }
}

impl Drop for Global {
    fn drop(&mut self) {
        set_plugin_registry(None);
    }
}

fn open(path: &std::path::Path) -> Document {
    let d = Document::open_lazy(path, Limits::default()).unwrap();
    d.wait_indexed();
    d.highlight_blocking();
    d
}

type LineSpans = Vec<(Range<usize>, HighlightKind)>;

fn spans_of(d: &Document) -> (Vec<String>, Vec<LineSpans>) {
    let n = d.line_count();
    (
        d.lines(0..n).into_iter().map(|c| c.into_owned()).collect(),
        d.highlight_lines(0..n),
    )
}

fn has(d: &Document, kind: HighlightKind, needle: &str) -> bool {
    let (lines, spans) = spans_of(d);
    lines.iter().zip(&spans).any(|(l, sp)| {
        sp.iter()
            .any(|(r, k)| *k == kind && l[r.clone()].contains(needle))
    })
}

#[test]
#[cfg(not(feature = "lang-csharp"))]
fn without_a_registry_plugin_languages_are_plain() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    set_plugin_registry(None);
    let d = open(&fixture("sample.cs"));
    assert_eq!(d.lang(), Some(Lang::CSharp));
    assert_eq!(d.grammar_id(), None);
    assert!(!Lang::CSharp.has_grammar());
    assert!(d.highlight_lines(0..3).iter().all(Vec::is_empty));
    assert_eq!(d.highlight_warning(), None);
}

#[test]
#[cfg(not(feature = "lang-csharp"))]
fn failed_plugin_falls_back_to_plain_text_with_a_warning() {
    let root = scratch("g_fallback");
    // Valid manifest + hashes, but the "DLL" is garbage: the load fails, nothing panics.
    Fake {
        extensions: &["cs"],
        ..Fake::new("csharp")
    }
    .write(&root);
    let (_g, reg) = Global::with(&root);
    assert!(Lang::CSharp.has_grammar(), "installed plugins count");
    let d = open(&fixture("sample.cs"));
    assert!(d.highlight_lines(0..5).iter().all(Vec::is_empty));
    let warn = d.highlight_warning().expect("warning for a failed plugin");
    assert!(warn.contains("could not load"), "{warn}");
    // after the failure the grammar is no longer offered
    assert!(!Lang::CSharp.has_grammar());
    assert_eq!(d.grammar_id(), None);
    assert!(reg.failure("csharp").is_some());
    // the document itself stays fully usable
    assert!(d.line_count() > 3);
}

#[test]
#[cfg(not(feature = "lang-csharp"))]
fn tampered_plugin_is_never_loaded() {
    let root = scratch("g_tamper");
    let dir = Fake {
        extensions: &["cs"],
        ..Fake::new("csharp")
    }
    .write(&root);
    fs::write(dir.join("grammar.dll"), b"swapped after install").unwrap();
    let (_g, _reg) = Global::with(&root);
    let d = open(&fixture("sample.cs"));
    assert!(d.highlight_lines(0..5).iter().all(Vec::is_empty));
    let warn = d.highlight_warning().unwrap();
    assert!(warn.contains("sha256 mismatch"), "{warn}");
}

#[test]
fn plugin_extensions_select_languages_unknown_to_the_enum() {
    let root = scratch("g_go");
    Fake {
        extensions: &["go"],
        ..Fake::new("go")
    }
    .write(&root);
    let (_g, _reg) = Global::with(&root);
    let dir = scratch("g_go_files");
    let p = dir.join("main.go");
    fs::write(&p, "package main\nfunc main() {}\n").unwrap();
    let d = Document::open_lazy(&p, Limits::default()).unwrap();
    d.wait_indexed();
    assert_eq!(d.lang(), None);
    assert_eq!(d.grammar_id().as_deref(), Some("go"));
    // garbage DLL: plain text, warning present
    d.highlight_blocking();
    assert!(d.highlight_lines(0..2).iter().all(Vec::is_empty));
    assert!(d.highlight_warning().is_some());
    // a file with an extension nobody claims is untouched
    let q = dir.join("x.unknownext");
    fs::write(&q, "hello\n").unwrap();
    let d = open(&q);
    assert_eq!(d.grammar_id(), None);
    assert_eq!(d.highlight_warning(), None);
}

#[test]
fn vanilla_languages_do_not_need_the_registry() {
    let root = scratch("g_vanilla");
    let (_g, _reg) = Global::with(&root);
    let d = open(&fixture("sample.json"));
    assert_eq!(d.grammar_id().as_deref(), Some("json"));
    assert!(has(&d, HighlightKind::String, "\"selachi\""));
}

#[test]
#[ignore = "needs packaging/grammars/build.ps1 output"]
fn real_plugins_highlight_documents_and_markdown_fences() {
    let root = scratch("g_real");
    copy_dist(
        &root,
        &[
            "csharp",
            "rust",
            "typescript",
            "javascript",
            "python",
            "bash",
            "yaml",
            "powershell",
        ],
    );
    let (_g, reg) = Global::with(&root);

    // C#: the original acceptance case
    let d = open(&fixture("sample.cs"));
    assert_eq!(d.grammar_id().as_deref(), Some("csharp"));
    assert!(d.highlight_ready());
    assert!(has(&d, HighlightKind::Keyword, "public"));
    assert!(has(&d, HighlightKind::Keyword, "namespace"));
    assert!(has(&d, HighlightKind::Comment, "// greet"));

    for (file, kind, needle) in [
        ("sample.rs", HighlightKind::Keyword, "fn"),
        ("sample.rs", HighlightKind::String, "\"origin\""),
        ("sample.ts", HighlightKind::Keyword, "interface"),
        ("sample.js", HighlightKind::Keyword, "const"),
        ("sample.py", HighlightKind::Keyword, "def"),
        ("sample.sh", HighlightKind::Keyword, "if"),
        ("sample.yaml", HighlightKind::Property, "name"),
    ] {
        let d = open(&fixture(file));
        assert!(has(&d, kind, needle), "{file}: {kind:?} {needle}");
    }

    // PowerShell has its own query file
    let dir = scratch("g_real_ps");
    let p = dir.join("a.ps1");
    fs::write(
        &p,
        "# hi\nfunction Get-Thing { param($x) if ($x -eq 1) { return \"one\" } }\n",
    )
    .unwrap();
    let d = open(&p);
    assert_eq!(d.grammar_id().as_deref(), Some("powershell"));
    assert!(has(&d, HighlightKind::Keyword, "function"));
    assert!(has(&d, HighlightKind::Operator, "-eq"));
    assert!(has(&d, HighlightKind::String, "one"));

    // the vanilla markdown grammar injects a *plugin* grammar into its ```rust fence
    let d = open(&fixture("sample.md"));
    let (lines, spans) = spans_of(&d);
    let fence = lines.iter().position(|l| l == "fn main() {}").unwrap();
    assert!(spans[fence]
        .iter()
        .any(|(r, k)| *k == HighlightKind::Keyword && &lines[fence][r.clone()] == "fn"));

    // TSX files resolve to their own plugin id (not installed here): plain
    let dir = scratch("g_real_tsx");
    let p = dir.join("a.tsx");
    fs::write(&p, "const a = <div/>;\n").unwrap();
    let d = open(&p);
    if cfg!(not(feature = "lang-typescript")) {
        assert_eq!(d.grammar_id(), None);
    }
    assert!(reg.has_plugin("rust"));
}
