//! Pure viewer state: which body a file kind gets, go-to-line parsing, the find cursor, the plugin
//! hint condition and the XL reading width. No gpui in here so all of it is unit-tested.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use viewer_svc::{Document, FileKind, Format};

use crate::spans::line_contains;

// ---------------------------------------------------------------------------------------------
// Kind routing
// ---------------------------------------------------------------------------------------------

/// What the viewer body shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// Source text with line numbers and syntax colors.
    Code,
    /// Unified or side-by-side diff against a git revision.
    Diff,
    /// Rendered markdown.
    Markdown,
    /// Collapsible JSON / TOML / YAML tree.
    Tree(Format),
    /// Raster or SVG image.
    Image,
    /// Hex dump (binaries and files over the size cap).
    Hex,
}

/// The body a file opens in.
pub fn default_body(kind: FileKind) -> Body {
    match kind {
        FileKind::Text { .. } => Body::Code,
        FileKind::Markdown => Body::Markdown,
        FileKind::Json => Body::Tree(Format::Json),
        FileKind::Toml => Body::Tree(Format::Toml),
        FileKind::Yaml => Body::Tree(Format::Yaml),
        FileKind::Image { .. } | FileKind::Svg => Body::Image,
        FileKind::Binary | FileKind::TooLarge => Body::Hex,
    }
}

/// The "source" counterpart of a rendered body (markdown <-> text, tree <-> text, svg <-> text).
pub fn source_body(kind: FileKind) -> Option<Body> {
    match kind {
        FileKind::Markdown | FileKind::Json | FileKind::Toml | FileKind::Yaml | FileKind::Svg => Some(Body::Code),
        _ => None,
    }
}

/// Files that can be diffed against git (anything shown as text).
pub fn supports_diff(kind: FileKind) -> bool {
    kind.is_textual()
}

/// Whether the body has line rows a minimap can summarise.
pub fn has_minimap(body: Body) -> bool {
    matches!(body, Body::Code | Body::Diff)
}

/// Label of the rendered/source toggle ("Vista" / "Fuente").
pub fn rendered_label(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Markdown => "Vista",
        FileKind::Svg => "Imagen",
        _ => "Árbol",
    }
}

// ---------------------------------------------------------------------------------------------
// Responsive
// ---------------------------------------------------------------------------------------------

/// Window width (logical px) from which the shell is in the XL class (DESIGN.md "Responsive").
pub const XL_FROM_PX: f32 = 2560.0;
/// Maximum width of a reading panel at XL.
pub const READING_MAX_PX: f32 = 860.0;

/// Width cap for the body: markdown and trees are reading panels; at XL they are limited to 860 px
/// and centered so lines stay legible. Markdown is always capped (a 4K pane of prose is unreadable
/// at any size); code, diff, images and hex use the whole pane. `width` is the pane's zone.
pub fn reading_cap(width: f32, body: Body) -> Option<f32> {
    match body {
        Body::Markdown => Some(READING_MAX_PX),
        Body::Tree(_) if width >= XL_FROM_PX => Some(READING_MAX_PX),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Go to line
// ---------------------------------------------------------------------------------------------

/// Parsed `Ctrl+G` input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Goto {
    /// 0-based line.
    pub line: usize,
    /// 0-based column, when given as `line:col`.
    pub col: Option<usize>,
}

/// Parses `42`, `42:7` or `:42` (1-based, as editors show them) against a file of `total` lines.
/// Lines past the end clamp to the last line, `0` is the first line. `None` for empty or
/// non-numeric input and for an empty file.
pub fn parse_goto(input: &str, total: usize) -> Option<Goto> {
    if total == 0 {
        return None;
    }
    let s = input.trim().trim_start_matches(':');
    let (line, col) = match s.split_once(':') {
        Some((l, c)) => (l.trim(), Some(c.trim())),
        None => (s, None),
    };
    let line: usize = line.parse().ok()?;
    let col = match col {
        Some(c) if !c.is_empty() => Some(c.parse::<usize>().ok()?.saturating_sub(1)),
        _ => None,
    };
    Some(Goto { line: line.saturating_sub(1).min(total - 1), col })
}

// ---------------------------------------------------------------------------------------------
// Find
// ---------------------------------------------------------------------------------------------

/// Matches kept at most (a one-letter query on a 50 MB file would match every line).
pub const MAX_MATCHES: usize = 20_000;

/// The find bar's state: the query, the matching lines (one entry per line) and a cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindState {
    pub query: String,
    /// 0-based lines that contain the query, ascending.
    pub matches: Vec<usize>,
    /// Index into `matches` of the current hit.
    pub current: Option<usize>,
    /// There were more than [`MAX_MATCHES`].
    pub capped: bool,
}

impl FindState {
    /// New query; the search results arrive later through [`FindState::set_matches`].
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.matches.clear();
        self.current = None;
        self.capped = false;
    }

    /// Installs the results of a finished search. The cursor lands on the first match at or after
    /// `from_line` (wrapping to the first match), so searching while scrolled does not jump to the top.
    pub fn set_matches(&mut self, matches: Vec<usize>, capped: bool, from_line: usize) {
        self.current = if matches.is_empty() {
            None
        } else {
            Some(matches.iter().position(|&l| l >= from_line).unwrap_or(0))
        };
        self.matches = matches;
        self.capped = capped;
    }

    pub fn current_line(&self) -> Option<usize> {
        self.current.and_then(|i| self.matches.get(i)).copied()
    }

    /// Moves to the next match, wrapping around. Returns the new line.
    pub fn next_match(&mut self) -> Option<usize> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        self.current = Some(self.current.map_or(0, |i| (i + 1) % n));
        self.current_line()
    }

    /// Moves to the previous match, wrapping around.
    pub fn prev_match(&mut self) -> Option<usize> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        self.current = Some(self.current.map_or(n - 1, |i| (i + n - 1) % n));
        self.current_line()
    }

    /// `"3 / 12"`, `"sin resultados"` or empty when there is no query.
    pub fn status(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        match self.current {
            Some(i) => format!("{} / {}{}", i + 1, self.matches.len(), if self.capped { "+" } else { "" }),
            None => "sin resultados".to_owned(),
        }
    }
}

/// Lines of `doc` containing `query` (ASCII case-insensitive), in ascending order, at most
/// [`MAX_MATCHES`]; the flag says the cap was hit. Checks `cancel` between chunks so a newer query
/// can stop an older scan. Meant for a background thread: it reads the file in 4096-line chunks.
pub fn search_document(doc: &Document, query: &str, cancel: &AtomicBool) -> (Vec<usize>, bool) {
    const CHUNK: usize = 4096;
    let mut out = Vec::new();
    if query.is_empty() {
        return (out, false);
    }
    doc.wait_indexed();
    let total = doc.line_count();
    let mut start = 0;
    while start < total {
        if cancel.load(Ordering::Relaxed) {
            return (Vec::new(), false);
        }
        let end = (start + CHUNK).min(total);
        for (i, line) in doc.lines(start..end).iter().enumerate() {
            if line_contains(line, query) {
                if out.len() >= MAX_MATCHES {
                    return (out, true);
                }
                out.push(start + i);
            }
        }
        start = end;
    }
    (out, false)
}

/// A cancel flag shared with a running search.
pub fn new_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

// ---------------------------------------------------------------------------------------------
// Plugin hint
// ---------------------------------------------------------------------------------------------

/// The "Instala el plugin de <lenguaje> para ver colores" banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginHint {
    /// Plugin id to hand to `ViewerEvent::InstallGrammar` (`csharp`, `rust`...).
    pub id: String,
    /// Language display name.
    pub language: String,
    /// Banner text.
    pub message: String,
    /// The plugin is installed but failed to load (the text carries the reason).
    pub failed: bool,
}

/// Decides whether the source view shows the plugin banner.
///
/// * `warning` (`Document::highlight_warning`): the language's plugin is installed but broken, so
///   the file is shown without colors; the banner says why.
/// * a source file whose language is known but has no grammar (`has_grammar == false` and
///   `grammar_id == None`): "Instala el plugin de ...".
/// * a file that is simply too big / Latin-1 to highlight (the language *has* a grammar) gets no banner:
///   installing a plugin would not change anything.
pub fn plugin_hint(
    kind: FileKind,
    ext: Option<&str>,
    grammar_id: Option<&str>,
    warning: Option<&str>,
    has_grammar: bool,
) -> Option<PluginHint> {
    let lang = kind.lang();
    let (id, language) = match lang {
        Some(l) => (l.id().to_owned(), l.name().to_owned()),
        None => {
            let ext = ext?;
            (ext.to_ascii_lowercase(), ext.to_ascii_uppercase())
        }
    };
    if let Some(w) = warning {
        return Some(PluginHint {
            message: format!("El plugin de {language} no se pudo cargar ({w}); se muestra sin colores"),
            id,
            language,
            failed: true,
        });
    }
    if grammar_id.is_none() && lang.is_some() && !has_grammar {
        return Some(PluginHint {
            message: format!("Instala el plugin de {language} para ver colores"),
            id,
            language,
            failed: false,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewer_svc::{Encoding, ImageFormat, Lang};

    fn text(lang: Option<Lang>) -> FileKind {
        FileKind::Text { lang, encoding: Encoding::Utf8 }
    }

    #[test]
    fn kind_routing() {
        assert_eq!(default_body(text(Some(Lang::Rust))), Body::Code);
        assert_eq!(default_body(text(None)), Body::Code);
        assert_eq!(default_body(FileKind::Markdown), Body::Markdown);
        assert_eq!(default_body(FileKind::Json), Body::Tree(Format::Json));
        assert_eq!(default_body(FileKind::Toml), Body::Tree(Format::Toml));
        assert_eq!(default_body(FileKind::Yaml), Body::Tree(Format::Yaml));
        assert_eq!(default_body(FileKind::Image { format: ImageFormat::Png }), Body::Image);
        assert_eq!(default_body(FileKind::Svg), Body::Image);
        assert_eq!(default_body(FileKind::Binary), Body::Hex);
        assert_eq!(default_body(FileKind::TooLarge), Body::Hex);
    }

    #[test]
    fn source_toggle_only_for_rendered_kinds() {
        assert_eq!(source_body(FileKind::Markdown), Some(Body::Code));
        assert_eq!(source_body(FileKind::Json), Some(Body::Code));
        assert_eq!(source_body(FileKind::Svg), Some(Body::Code));
        assert_eq!(source_body(text(None)), None);
        assert_eq!(source_body(FileKind::Binary), None);
        assert_eq!(source_body(FileKind::Image { format: ImageFormat::Png }), None);
    }

    #[test]
    fn diff_only_for_text() {
        assert!(supports_diff(text(None)));
        assert!(supports_diff(FileKind::Markdown));
        assert!(!supports_diff(FileKind::Binary));
        assert!(!supports_diff(FileKind::TooLarge));
        assert!(!supports_diff(FileKind::Image { format: ImageFormat::Png }));
    }

    #[test]
    fn minimap_only_where_there_are_line_rows() {
        assert!(has_minimap(Body::Code));
        assert!(has_minimap(Body::Diff));
        assert!(!has_minimap(Body::Hex));
        assert!(!has_minimap(Body::Markdown));
    }

    #[test]
    fn reading_width_cap() {
        assert_eq!(reading_cap(1280.0, Body::Markdown), Some(860.0));
        assert_eq!(reading_cap(3840.0, Body::Markdown), Some(860.0));
        assert_eq!(reading_cap(1280.0, Body::Tree(Format::Json)), None);
        assert_eq!(reading_cap(2560.0, Body::Tree(Format::Json)), Some(860.0));
        assert_eq!(reading_cap(3840.0, Body::Code), None);
        assert_eq!(reading_cap(3840.0, Body::Hex), None);
    }

    #[test]
    fn goto_parsing() {
        assert_eq!(parse_goto("42", 100), Some(Goto { line: 41, col: None }));
        assert_eq!(parse_goto(" 42:7 ", 100), Some(Goto { line: 41, col: Some(6) }));
        assert_eq!(parse_goto(":5", 100), Some(Goto { line: 4, col: None }));
        assert_eq!(parse_goto("1", 100), Some(Goto { line: 0, col: None }));
        assert_eq!(parse_goto("0", 100), Some(Goto { line: 0, col: None }));
        // clamps to the last line
        assert_eq!(parse_goto("5000", 100), Some(Goto { line: 99, col: None }));
        assert_eq!(parse_goto("42:", 100), Some(Goto { line: 41, col: None }));
        // invalid
        assert_eq!(parse_goto("", 100), None);
        assert_eq!(parse_goto("abc", 100), None);
        assert_eq!(parse_goto("-3", 100), None);
        assert_eq!(parse_goto("4:x", 100), None);
        assert_eq!(parse_goto("1", 0), None);
    }

    #[test]
    fn find_cursor_wraps() {
        let mut f = FindState::default();
        f.set_query("foo");
        assert_eq!(f.status(), "sin resultados");
        assert_eq!(f.next_match(), None);
        f.set_matches(vec![3, 10, 40], false, 0);
        assert_eq!(f.current_line(), Some(3));
        assert_eq!(f.status(), "1 / 3");
        assert_eq!(f.next_match(), Some(10));
        assert_eq!(f.next_match(), Some(40));
        assert_eq!(f.next_match(), Some(3));
        assert_eq!(f.prev_match(), Some(40));
        assert_eq!(f.prev_match(), Some(10));
    }

    #[test]
    fn find_starts_at_the_scroll_position() {
        let mut f = FindState::default();
        f.set_query("x");
        f.set_matches(vec![3, 10, 40], false, 11);
        assert_eq!(f.current_line(), Some(40));
        // Past the last match: wraps to the first.
        f.set_matches(vec![3, 10, 40], false, 100);
        assert_eq!(f.current_line(), Some(3));
        f.set_matches(vec![1], true, 0);
        assert_eq!(f.status(), "1 / 1+");
    }

    #[test]
    fn empty_query_has_no_status() {
        let f = FindState::default();
        assert_eq!(f.status(), "");
    }

    #[test]
    fn search_scans_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let body: String = (0..10_000).map(|i| if i % 1000 == 7 { format!("Needle {i}\n") } else { format!("line {i}\n") }).collect();
        std::fs::write(&path, body).unwrap();
        let doc = Document::open_lazy(&path, viewer_svc::Limits::default()).unwrap();
        let cancel = AtomicBool::new(false);
        let (hits, capped) = search_document(&doc, "needle", &cancel);
        assert!(!capped);
        assert_eq!(hits, (0..10).map(|k| k * 1000 + 7).collect::<Vec<_>>());
        assert_eq!(search_document(&doc, "", &cancel), (vec![], false));
        // A cancelled scan returns nothing.
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(search_document(&doc, "line", &cancel), (vec![], false));
    }

    #[test]
    fn hint_for_a_known_language_without_grammar() {
        let kind = text(Some(Lang::CSharp));
        let h = plugin_hint(kind, Some("cs"), None, None, false).unwrap();
        assert_eq!(h.id, "csharp");
        assert_eq!(h.language, "C#");
        assert!(h.message.contains("Instala el plugin de C#"));
        assert!(!h.failed);
    }

    #[test]
    fn no_hint_when_highlighting_works() {
        let kind = text(Some(Lang::Rust));
        assert_eq!(plugin_hint(kind, Some("rs"), Some("rust"), None, true), None);
    }

    #[test]
    fn no_hint_when_the_file_is_just_too_big_to_highlight() {
        // The language has a grammar, but the document has no grammar id (size cap): nothing to install.
        let kind = text(Some(Lang::Rust));
        assert_eq!(plugin_hint(kind, Some("rs"), None, None, true), None);
    }

    #[test]
    fn warning_wins_and_marks_failure() {
        let kind = text(Some(Lang::CSharp));
        let h = plugin_hint(kind, Some("cs"), None, Some("sha256 mismatch"), true).unwrap();
        assert!(h.failed);
        assert!(h.message.contains("sha256 mismatch"));
    }

    #[test]
    fn unknown_language_gets_no_install_hint_but_a_warning_still_shows() {
        let kind = text(None);
        assert_eq!(plugin_hint(kind, Some("zig"), None, None, false), None);
        let h = plugin_hint(kind, Some("zig"), None, Some("abi"), false).unwrap();
        assert_eq!(h.id, "zig");
        assert!(h.failed);
        assert_eq!(plugin_hint(kind, None, None, None, false), None);
    }
}
