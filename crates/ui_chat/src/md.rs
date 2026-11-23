//! Markdown for assistant text: parsing with `viewer_svc::markdown_blocks`, code blocks
//! highlighted with `viewer_svc::highlight_text`, rendered with plain gpui elements. Tonal
//! surfaces only (no divider lines). The parse is lazy and cached per row ([`MdBody`]), so a
//! frame never parses twice.

use std::cell::{Cell, OnceCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClipboardItem, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement, ParentElement as _,
    SharedString, StrikethroughStyle, Styled as _, StyledText, TextRun, div, prelude::*, px, relative,
};
use theme::{Mode, Rgba, Theme, rgb};
use ui_kit::{Icon, icon};
use viewer_svc::{HighlightKind, Lang, MdAlign, MdBlock, MdInline, MdListItem, MdTable, highlight_text, markdown_blocks};

/// Code blocks bigger than this are shown without colors (keeps streaming cheap).
const MAX_HIGHLIGHT_BYTES: usize = 24 * 1024;

/// Highlight spans of one code block, per line (line-relative byte ranges).
pub type LineSpans = Vec<(Range<usize>, HighlightKind)>;

/// A fenced code block found while walking the document, with its colors.
#[derive(Clone, Debug, Default)]
pub struct CodeHl {
    pub lines: Vec<LineSpans>,
}

/// Blocks plus the highlight of each code block in document order.
#[derive(Clone, Debug, Default)]
pub struct ParsedMd {
    pub blocks: Vec<MdBlock>,
    pub code: Vec<CodeHl>,
}

/// `(ordinal, language, text)` of every code block, depth first in document order.
pub fn code_blocks(blocks: &[MdBlock]) -> Vec<(usize, String, String)> {
    fn walk(blocks: &[MdBlock], out: &mut Vec<(usize, String, String)>) {
        for b in blocks {
            match b {
                MdBlock::CodeBlock { lang, text } => out.push((out.len(), lang.clone().unwrap_or_default(), text.clone())),
                MdBlock::List { items, .. } => items.iter().for_each(|i| walk(&i.blocks, out)),
                MdBlock::Quote(inner) => walk(inner, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

/// Parses `text` and highlights its code blocks.
pub fn parse_markdown(text: &str) -> ParsedMd {
    let blocks = markdown_blocks(text);
    let code = code_blocks(&blocks)
        .into_iter()
        .map(|(_, lang, body)| {
            let lines = match Lang::from_name(lang.split_whitespace().next().unwrap_or("")) {
                Some(l) if body.len() <= MAX_HIGHLIGHT_BYTES => highlight_text(l.id(), &body),
                _ => Vec::new(),
            };
            CodeHl { lines }
        })
        .collect();
    ParsedMd { blocks, code }
}

/// Text of an assistant block with its lazily parsed Markdown. Cheap to clone.
#[derive(Clone)]
pub struct MdBody {
    text: Rc<str>,
    parsed: Rc<OnceCell<ParsedMd>>,
}

impl std::fmt::Debug for MdBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdBody").field("len", &self.text.len()).finish()
    }
}

impl MdBody {
    pub fn new(text: String) -> Self {
        Self { text: text.into(), parsed: Rc::new(OnceCell::new()) }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Parses on first use.
    pub fn parsed(&self) -> &ParsedMd {
        self.parsed.get_or_init(|| parse_markdown(&self.text))
    }
}

// ---------------------------------------------------------------------------------------------
// Inline flattening
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

impl InlineStyle {
    fn is_plain(&self) -> bool {
        *self == InlineStyle::default()
    }
}

/// Inline content as one string plus styled ranges and link ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlatInline {
    pub text: String,
    pub runs: Vec<(Range<usize>, InlineStyle)>,
    pub links: Vec<(Range<usize>, String)>,
}

pub fn flatten(inlines: &[MdInline]) -> FlatInline {
    let mut out = FlatInline::default();
    walk(inlines, InlineStyle::default(), None, &mut out);
    out
}

fn push(out: &mut FlatInline, s: &str, style: InlineStyle, link: Option<&str>) {
    if s.is_empty() {
        return;
    }
    let start = out.text.len();
    out.text.push_str(s);
    let range = start..out.text.len();
    if !style.is_plain() {
        out.runs.push((range.clone(), style));
    }
    if let Some(url) = link {
        match out.links.last_mut() {
            Some((r, u)) if r.end == start && u == url => r.end = range.end,
            _ => out.links.push((range, url.to_owned())),
        }
    }
}

fn walk(inlines: &[MdInline], style: InlineStyle, link: Option<&str>, out: &mut FlatInline) {
    for i in inlines {
        match i {
            MdInline::Text(t) => push(out, t, style, link),
            MdInline::Code(t) => push(out, t, InlineStyle { code: true, ..style }, link),
            MdInline::Emph(c) => walk(c, InlineStyle { italic: true, ..style }, link, out),
            MdInline::Strong(c) => walk(c, InlineStyle { bold: true, ..style }, link, out),
            MdInline::Strike(c) => walk(c, InlineStyle { strike: true, ..style }, link, out),
            MdInline::Link { url, children } => walk(children, InlineStyle { link: true, ..style }, Some(url), out),
            MdInline::Image { alt, .. } => push(out, &format!("[{alt}]"), style, link),
            MdInline::SoftBreak => push(out, " ", style, link),
            MdInline::HardBreak => push(out, "\n", style, link),
        }
    }
}

/// Links worth opening from a click: web and mail only.
pub fn is_openable(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:")
}

// ---------------------------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------------------------

/// Color of a highlight class (the theme's ANSI palette, never the brand accent). `None` = text.
pub fn color_for(kind: HighlightKind, theme: &Theme) -> Option<Rgba> {
    let a = |i: usize| rgb(theme.terminal.ansi[i]);
    let light = theme.mode == Mode::Light;
    let pick = |dark: usize, light_ix: usize| if light { a(light_ix) } else { a(dark) };
    Some(match kind {
        HighlightKind::Keyword => pick(5, 5),
        HighlightKind::String => pick(2, 2),
        HighlightKind::Comment => theme.colors.outline,
        HighlightKind::Function => pick(12, 4),
        HighlightKind::Type => pick(3, 3),
        HighlightKind::Number => pick(1, 1),
        HighlightKind::Constant => pick(9, 1),
        HighlightKind::Property => pick(6, 6),
        HighlightKind::Operator => theme.colors.muted,
        HighlightKind::Punctuation => theme.colors.outline,
        HighlightKind::Tag => pick(1, 1),
        HighlightKind::Attribute => pick(11, 3),
        HighlightKind::Variable | HighlightKind::Plain => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// Everything block rendering needs besides the blocks.
pub struct MdRender<'a> {
    pub theme: Theme,
    pub parsed: &'a ParsedMd,
    /// Base of the element ids (unique per row).
    pub id_base: usize,
    next_id: Cell<usize>,
    next_code: Cell<usize>,
    in_list: Cell<bool>,
}

impl<'a> MdRender<'a> {
    pub fn new(theme: Theme, parsed: &'a ParsedMd, id_base: usize) -> Self {
        Self { theme, parsed, id_base, next_id: Cell::new(0), next_code: Cell::new(0), in_list: Cell::new(false) }
    }

    fn id(&self) -> usize {
        let i = self.next_id.get();
        self.next_id.set(i + 1);
        self.id_base.wrapping_mul(997).wrapping_add(i)
    }
}

/// Renders the whole document.
pub fn render_md(parsed: &ParsedMd, theme: Theme, id_base: usize) -> AnyElement {
    let r = MdRender::new(theme, parsed, id_base);
    div().w_full().min_w_0().flex().flex_col().children(parsed.blocks.iter().map(|b| render_block(b, &r))).into_any_element()
}

fn inline_styles(flat: &FlatInline, theme: &Theme) -> Vec<(Range<usize>, HighlightStyle)> {
    let c = theme.colors;
    flat.runs
        .iter()
        .map(|(range, s)| {
            let mut h = HighlightStyle::default();
            if s.bold {
                h.font_weight = Some(FontWeight::SEMIBOLD);
            }
            if s.italic {
                h.font_style = Some(FontStyle::Italic);
            }
            if s.strike {
                h.strikethrough = Some(StrikethroughStyle { thickness: px(1.0), color: Some(c.muted.into()) });
            }
            if s.code {
                // Neutral, never the ANSI red (it read as an error): text on a tonal chip.
                h.background_color = Some(c.surface_2.into());
                h.color = Some(c.text.into());
            }
            if s.link {
                h.color = Some(c.accent.into());
                h.underline = Some(gpui::UnderlineStyle { thickness: px(1.0), color: Some(c.accent.into()), wavy: false });
            }
            (range.clone(), h)
        })
        .collect()
}

/// Runs for text that contains inline code: `HighlightStyle` cannot switch the font family, so
/// every span gets a full `TextRun` (UI font for prose, mono for code). `weight` / `color` are the
/// block's own, since runs replace the parent's text style.
fn code_runs(flat: &FlatInline, theme: &Theme, weight: FontWeight, color: Rgba) -> Vec<TextRun> {
    let c = theme.colors;
    let run = |range: Range<usize>, s: InlineStyle| {
        let mut f = gpui::font(if s.code { theme.typography.mono_family } else { theme.typography.ui_family });
        f.weight = if s.bold { FontWeight::SEMIBOLD } else { weight };
        if s.italic {
            f.style = FontStyle::Italic;
        }
        let (fg, bg) = match (s.code, s.link) {
            (_, true) => (c.accent, None),
            (true, false) => (c.text, Some(c.surface_2)),
            (false, false) => (color, None),
        };
        TextRun {
            len: range.len(),
            font: f,
            color: fg.into(),
            background_color: bg.map(Into::into),
            underline: s.link.then(|| gpui::UnderlineStyle { thickness: px(1.0), color: Some(c.accent.into()), wavy: false }),
            strikethrough: s.strike.then(|| StrikethroughStyle { thickness: px(1.0), color: Some(c.muted.into()) }),
        }
    };
    let mut out = Vec::new();
    let mut at = 0;
    for (range, style) in &flat.runs {
        if range.start > at {
            out.push(run(at..range.start, InlineStyle::default()));
        }
        out.push(run(range.clone(), *style));
        at = range.end;
    }
    if at < flat.text.len() {
        out.push(run(at..flat.text.len(), InlineStyle::default()));
    }
    out
}

fn inline_el(inlines: &[MdInline], r: &MdRender<'_>, weight: FontWeight, color: Rgba) -> AnyElement {
    let flat = flatten(inlines);
    let has_code = flat.runs.iter().any(|(_, s)| s.code);
    let text = if has_code {
        let runs = code_runs(&flat, &r.theme, weight, color);
        StyledText::new(SharedString::from(flat.text)).with_runs(runs)
    } else {
        let styles = inline_styles(&flat, &r.theme);
        StyledText::new(SharedString::from(flat.text)).with_highlights(styles)
    };
    if flat.links.is_empty() {
        return text.into_any_element();
    }
    let (ranges, urls): (Vec<_>, Vec<_>) = flat.links.into_iter().unzip();
    InteractiveText::new(("md-text", r.id()), text)
        .on_click(ranges, move |ix, _, cx| {
            if let Some(url) = urls.get(ix).filter(|u| is_openable(u)) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

fn heading_size(level: u8) -> f32 {
    match level {
        1 => 22.0,
        2 => 19.0,
        3 => 16.5,
        _ => 15.0,
    }
}

const BODY_SIZE: f32 = 14.5;

fn render_block(block: &MdBlock, r: &MdRender<'_>) -> AnyElement {
    let theme = r.theme;
    let c = theme.colors;
    match block {
        MdBlock::Heading { level, inlines } => div()
            .mt(px(if *level <= 2 { 16.0 } else { 10.0 }))
            .mb(px(4.0))
            .text_size(px(heading_size(*level)))
            .font_weight(if *level <= 3 { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
            .line_height(relative(1.35))
            .text_color(c.text)
            .child(inline_el(inlines, r, if *level <= 3 { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM }, c.text))
            .into_any_element(),
        MdBlock::Paragraph(inlines) => div()
            .my(px(if r.in_list.get() { 1.0 } else { 4.0 }))
            .text_size(px(BODY_SIZE))
            .line_height(relative(1.6))
            .text_color(c.text)
            .child(inline_el(inlines, r, FontWeight::NORMAL, c.text))
            .into_any_element(),
        MdBlock::CodeBlock { lang, text } => code_block(lang.as_deref(), text, r),
        MdBlock::List { ordered, start, items } => {
            let rows: Vec<AnyElement> =
                items.iter().enumerate().map(|(i, item)| list_item(*ordered, *start + i as u64, item, r)).collect();
            div().my(px(4.0)).flex().flex_col().gap(px(1.0)).children(rows).into_any_element()
        }
        MdBlock::Quote(inner) => div()
            .my(px(6.0))
            .px(px(14.0))
            .py(px(4.0))
            .rounded(px(12.0))
            .bg(c.surface_1)
            .text_color(c.muted)
            .flex()
            .flex_col()
            .children(inner.iter().map(|b| render_block(b, r)))
            .into_any_element(),
        MdBlock::Rule => div()
            .my(px(14.0))
            .flex()
            .flex_row()
            .gap(px(6.0))
            .children((0..3).map(|i| div().size(px(4.0)).bg(if i == 1 { c.outline } else { c.surface_4 })))
            .into_any_element(),
        MdBlock::Table(t) => table(t, r),
        MdBlock::Image { alt, .. } => div()
            .my(px(4.0))
            .text_size(px(13.0))
            .text_color(c.outline)
            .child(SharedString::from(format!("[imagen: {alt}]")))
            .into_any_element(),
        MdBlock::Html(h) => div()
            .my(px(4.0))
            .font_family(theme.typography.mono_family)
            .text_size(px(12.0))
            .text_color(c.outline)
            .child(SharedString::from(h.trim_end().to_owned()))
            .into_any_element(),
    }
}

fn code_block(lang: Option<&str>, text: &str, r: &MdRender<'_>) -> AnyElement {
    let theme = r.theme;
    let c = theme.colors;
    let ordinal = r.next_code.get();
    r.next_code.set(ordinal + 1);
    let hl = r.parsed.code.get(ordinal);
    let body = text.trim_end_matches('\n');
    let lines: Vec<AnyElement> = body
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let spans: &[(Range<usize>, HighlightKind)] = hl.and_then(|h| h.lines.get(i)).map_or(&[], Vec::as_slice);
            let styled: Vec<(Range<usize>, HighlightStyle)> = spans
                .iter()
                .filter(|(range, _)| range.end <= line.len() && line.is_char_boundary(range.start) && line.is_char_boundary(range.end))
                .map(|(range, kind)| {
                    (
                        range.clone(),
                        HighlightStyle {
                            color: color_for(*kind, &theme).map(Into::into),
                            font_style: (*kind == HighlightKind::Comment).then_some(FontStyle::Italic),
                            ..HighlightStyle::default()
                        },
                    )
                })
                .collect();
            div()
                .min_h(px(19.0))
                .whitespace_nowrap()
                .child(StyledText::new(SharedString::from(line.to_owned())).with_highlights(styled))
                .into_any_element()
        })
        .collect();
    let copy_text = body.to_string();
    let copy = div()
        .id(("md-copy", r.id()))
        .occlude()
        .flex_none()
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .text_size(px(11.0))
        .text_color(c.muted)
        .hover(move |s| s.bg(c.surface_3).text_color(c.text))
        .tooltip(ui_kit::Tooltip::text(&theme, "Copiar código"))
        // TODO-SVG: `assets/icons/copy.svg` (16x16). Until it exists the button is text only.
        .child("Copiar")
        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone())));
    div()
        .my(px(6.0))
        .rounded(px(12.0))
        .bg(c.surface_1)
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(28.0))
                .px(px(14.0))
                .pr(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c.outline)
                        .child(SharedString::from(lang.filter(|l| !l.is_empty()).unwrap_or("texto").to_owned())),
                )
                .child(copy),
        )
        .child(
            div()
                .px(px(14.0))
                .pb(px(12.0))
                .overflow_hidden()
                .font_family(theme.typography.mono_family)
                .text_size(px(12.5))
                .text_color(c.text)
                .flex()
                .flex_col()
                .children(lines),
        )
        .into_any_element()
}

fn list_item(ordered: bool, number: u64, item: &MdListItem, r: &MdRender<'_>) -> AnyElement {
    let c = r.theme.colors;
    let marker: AnyElement = match item.checked {
        Some(done) => div()
            .mt(px(4.0))
            .size(px(16.0))
            .flex_none()
            .rounded(px(5.0))
            .bg(c.surface_3)
            .flex()
            .items_center()
            .justify_center()
            .when(done, |d| d.child(icon(Icon::Check, 12.0, c.text)))
            .into_any_element(),
        None if ordered => div()
            .flex_none()
            .w(px(22.0))
            .text_size(px(BODY_SIZE))
            .line_height(relative(1.6))
            .text_color(c.muted)
            .child(SharedString::from(format!("{number}.")))
            .into_any_element(),
        None => div()
            .flex_none()
            .w(px(22.0))
            .pt(px(11.0))
            .child(div().size(px(5.0)).rounded_full().bg(c.outline))
            .into_any_element(),
    };
    div()
        .flex()
        .flex_row()
        .gap(px(8.0))
        .pl(px(4.0))
        .child(marker)
        .child({
            let was = r.in_list.replace(true);
            let body = div().flex_1().min_w_0().flex().flex_col().children(item.blocks.iter().map(|b| render_block(b, r)));
            r.in_list.set(was);
            body
        })
        .into_any_element()
}

fn table(t: &MdTable, r: &MdRender<'_>) -> AnyElement {
    let c = r.theme.colors;
    let cell = |inlines: &[MdInline], align: MdAlign, header: bool| -> AnyElement {
        let d = div()
            .flex_1()
            .min_w_0()
            .px(px(12.0))
            .py(px(7.0))
            .flex()
            .text_size(px(13.5))
            .when(header, |d| d.font_weight(FontWeight::MEDIUM))
            .text_color(if header { c.text } else { c.muted });
        let d = match align {
            MdAlign::Right => d.justify_end(),
            MdAlign::Center => d.justify_center(),
            _ => d,
        };
        let (weight, color) = if header { (FontWeight::MEDIUM, c.text) } else { (FontWeight::NORMAL, c.muted) };
        d.child(inline_el(inlines, r, weight, color)).into_any_element()
    };
    let align = |i: usize| t.aligns.get(i).copied().unwrap_or(MdAlign::None);
    let header = div()
        .flex()
        .flex_row()
        .bg(c.surface_2)
        .children(t.header.iter().enumerate().map(|(i, h)| cell(h, align(i), true)));
    let rows = t
        .rows
        .iter()
        .map(|row| div().flex().flex_row().children(row.iter().enumerate().map(|(i, h)| cell(h, align(i), false))));
    div().my(px(6.0)).rounded(px(12.0)).bg(c.surface_1).overflow_hidden().flex().flex_col().child(header).children(rows).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use MdInline::{Code, Emph, Link, SoftBreak, Strong, Text};

    #[test]
    fn code_blocks_are_numbered_in_document_order() {
        let md = "```rust\nfn a() {}\n```\n\ntext\n\n- item\n\n  ```json\n  {}\n  ```\n\n> ```toml\n> a = 1\n> ```\n";
        let found = code_blocks(&markdown_blocks(md));
        let langs: Vec<_> = found.iter().map(|(i, l, _)| (*i, l.as_str())).collect();
        assert_eq!(langs, vec![(0, "rust"), (1, "json"), (2, "toml")]);
        let parsed = parse_markdown(md);
        assert_eq!(parsed.code.len(), 3);
    }

    #[test]
    fn body_parses_once_and_streaming_fences_survive() {
        let b = MdBody::new("Hola\n\n```json\n{\"a\": 1".to_string());
        let n = b.parsed().blocks.len();
        assert_eq!(n, 2, "paragraph + an unclosed fence");
        assert!(std::ptr::eq(b.parsed(), b.parsed()));
        assert_eq!(b.text().len(), 21);
    }

    #[test]
    fn styles_nest_and_links_collect() {
        let f = flatten(&[Text("a ".into()), Strong(vec![Text("b ".into()), Emph(vec![Text("c".into())])]), Code("d".into())]);
        assert_eq!(f.text, "a b cd");
        assert_eq!(f.runs.len(), 3);
        assert!(f.runs.windows(2).all(|w| w[0].0.end <= w[1].0.start));
        let f = flatten(&[Text("ver ".into()), Link { url: "https://x.dev".into(), children: vec![Text("docs".into())] }, SoftBreak]);
        assert_eq!(f.text, "ver docs ");
        assert_eq!(f.links, vec![(4..8, "https://x.dev".to_string())]);
    }

    #[test]
    fn only_web_links_open() {
        assert!(is_openable("https://selachi.dev"));
        assert!(is_openable("mailto:a@b.c"));
        assert!(!is_openable("docs/PLAN.md"));
        assert!(!is_openable("javascript:alert(1)"));
    }

    #[test]
    fn syntax_colors_never_use_the_accent() {
        for theme in [Theme::dark(), Theme::light()] {
            for kind in [HighlightKind::Keyword, HighlightKind::String, HighlightKind::Function, HighlightKind::Type] {
                assert_ne!(color_for(kind, &theme), Some(theme.colors.accent));
            }
            assert!(color_for(HighlightKind::Plain, &theme).is_none());
        }
    }
}
