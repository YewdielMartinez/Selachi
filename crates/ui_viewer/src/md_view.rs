//! Renders `MdBlock`s with DM Sans (code blocks in Geist Mono, highlighted). Tonal surfaces only:
//! no divider lines (a rule is three pixel squares, a quote is a tonal card).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement,
    ParentElement as _, SharedString, StrikethroughStyle, Styled as _, StyledText, div, img,
    prelude::*, px, relative,
};
use theme::{Radius, Theme};
use ui_kit::{Gap, Icon, Segments, SelectableText, SelectionGroup, icon};
use viewer_svc::{MdAlign, MdBlock, MdInline, MdListItem, MdTable};

use crate::md_inline::{FlatInline, InlineStyle, flatten, is_openable};
use crate::spans::{LineSpans, overlay};
use crate::syntax::{color_for, is_italic};

/// Everything block rendering needs besides the blocks.
pub struct MdRender<'a> {
    pub theme: Theme,
    /// Directory of the markdown file, to resolve relative image paths.
    pub base_dir: Option<PathBuf>,
    /// Highlight spans of code blocks by their ordinal in document order.
    pub highlights: &'a HashMap<usize, Vec<LineSpans>>,
    next_id: Cell<usize>,
    next_code: Cell<usize>,
    in_list: Cell<bool>,
    /// Selection of the document, when its text is selectable.
    group: Option<SelectionGroup>,
    /// Text of every selectable segment in document order (the group's copy source).
    segs: RefCell<Segments>,
    gap: Cell<Gap>,
}

impl<'a> MdRender<'a> {
    pub fn new(
        theme: Theme,
        base_dir: Option<PathBuf>,
        highlights: &'a HashMap<usize, Vec<LineSpans>>,
    ) -> Self {
        Self {
            theme,
            base_dir,
            highlights,
            next_id: Cell::new(0),
            next_code: Cell::new(0),
            in_list: Cell::new(false),
            group: None,
            segs: RefCell::new(Segments::new()),
            gap: Cell::new(Gap::Block),
        }
    }

    /// Makes every text run selectable through `group` (call [`Self::finish`] after rendering).
    pub fn with_group(mut self, group: SelectionGroup) -> Self {
        self.group = Some(group);
        self
    }

    /// Hands the text of the rendered segments to the group (copy source for `Ctrl+C`).
    pub fn finish(&self) {
        if let Some(g) = &self.group {
            g.set_segments(self.segs.take());
        }
    }

    fn seg(&self, text: &str) -> Option<usize> {
        self.group.as_ref()?;
        let gap = self.gap.replace(Gap::Line);
        Some(self.segs.borrow_mut().push(text.to_owned(), gap))
    }

    fn block_start(&self) {
        if !self.in_list.get() {
            self.gap.set(Gap::Block);
        }
    }

    fn id(&self) -> usize {
        let i = self.next_id.get();
        self.next_id.set(i + 1);
        i
    }
}
/// Ordinals are assigned in document order, depth first; the viewer uses this to know which
/// code blocks to highlight in the background.
pub fn code_blocks(blocks: &[MdBlock]) -> Vec<(usize, String, String)> {
    fn walk(blocks: &[MdBlock], out: &mut Vec<(usize, String, String)>) {
        for b in blocks {
            match b {
                MdBlock::CodeBlock { lang, text } => {
                    out.push((out.len(), lang.clone().unwrap_or_default(), text.clone()))
                }
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

pub fn render_blocks(blocks: &[MdBlock], r: &MdRender<'_>) -> Vec<AnyElement> {
    blocks.iter().map(|b| render_block(b, r)).collect()
}

fn inline_styles(
    flat: &FlatInline,
    theme: &Theme,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let c = theme.colors;
    flat.runs
        .iter()
        .map(|(range, s): &(std::ops::Range<usize>, InlineStyle)| {
            let mut h = HighlightStyle::default();
            if s.bold {
                h.font_weight = Some(FontWeight::SEMIBOLD);
            }
            if s.italic {
                h.font_style = Some(FontStyle::Italic);
            }
            if s.strike {
                h.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(c.muted.into()),
                });
            }
            if s.code {
                h.background_color = Some(c.surface_3.into());
                h.color = Some(c.error.into());
            }
            if s.link {
                h.color = Some(c.accent.into());
                h.underline = Some(gpui::UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(c.accent.into()),
                    wavy: false,
                });
            }
            (range.clone(), h)
        })
        .collect()
}

/// Inline content as one text element (links are clickable).
fn inline_el(inlines: &[MdInline], r: &MdRender<'_>) -> AnyElement {
    let flat = flatten(inlines);
    let styles = inline_styles(&flat, &r.theme);
    if let (Some(group), Some(seg)) = (&r.group, r.seg(&flat.text)) {
        return SelectableText::new(group, seg, SharedString::from(flat.text.clone()))
            .with_highlights(styles)
            .links(flat.links)
            .into_any_element();
    }
    let text = StyledText::new(SharedString::from(flat.text)).with_highlights(styles);
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
        1 => 26.0,
        2 => 21.0,
        3 => 18.0,
        4 => 16.0,
        _ => 15.0,
    }
}

fn render_block(block: &MdBlock, r: &MdRender<'_>) -> AnyElement {
    let theme = r.theme;
    let c = theme.colors;
    r.block_start();
    match block {
        MdBlock::Heading { level, inlines } => div()
            .mt(px(if *level <= 2 { 22.0 } else { 14.0 }))
            .mb(px(6.0))
            .text_size(px(heading_size(*level)))
            .font_weight(if *level <= 3 {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .line_height(relative(1.3))
            .text_color(c.text)
            .child(inline_el(inlines, r))
            .into_any_element(),
        MdBlock::Paragraph(inlines) => div()
            .my(px(5.0))
            .text_size(px(r.theme.typography.ui_size_lg))
            .line_height(relative(1.6))
            .text_color(c.text)
            .child(inline_el(inlines, r))
            .into_any_element(),
        MdBlock::CodeBlock { lang, text } => code_block(lang.as_deref(), text, r),
        MdBlock::List {
            ordered,
            start,
            items,
        } => {
            let rows: Vec<AnyElement> = items
                .iter()
                .enumerate()
                .map(|(i, item)| list_item(*ordered, *start + i as u64, item, r))
                .collect();
            div()
                .my(px(5.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .children(rows)
                .into_any_element()
        }
        MdBlock::Quote(inner) => div()
            .my(px(8.0))
            .px(px(16.0))
            .py(px(6.0))
            .rounded(px(Radius::SM))
            .bg(c.surface_1)
            .text_color(c.muted)
            .flex()
            .flex_col()
            .children(render_blocks(inner, r))
            .into_any_element(),
        MdBlock::Rule => div()
            .my(px(18.0))
            .flex()
            .flex_row()
            .gap(px(6.0))
            .children((0..3).map(|i| {
                div()
                    .size(px(4.0))
                    .bg(if i == 1 { c.outline } else { c.surface_4 })
            }))
            .into_any_element(),
        MdBlock::Table(t) => table(t, r),
        MdBlock::Image { src, alt } => image_block(src, alt, r),
        MdBlock::Html(h) => div()
            .my(px(6.0))
            .font_family(theme.typography.mono_family)
            .text_size(px(r.theme.typography.ui_size_sm))
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
    let spans = r.highlights.get(&ordinal);
    let lines: Vec<AnyElement> = text
        .trim_end_matches('\n')
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let base: LineSpans = spans.and_then(|s| s.get(i)).cloned().unwrap_or_default();
            let prepared = crate::spans::prepare_line(line, &base, crate::spans::MAX_LINE_BYTES);
            let styled = overlay::<_, ()>(&prepared.spans, &[], prepared.text.len())
                .into_iter()
                .filter_map(|(range, kind, _)| {
                    let kind = kind?;
                    let h = HighlightStyle {
                        color: color_for(kind, &theme).map(Into::into),
                        font_style: is_italic(kind).then_some(FontStyle::Italic),
                        ..HighlightStyle::default()
                    };
                    Some((range, h))
                })
                .collect::<Vec<_>>();
            div()
                .h(px(19.0))
                .whitespace_nowrap()
                .child(match (&r.group, r.seg(&prepared.text)) {
                    (Some(group), Some(seg)) => {
                        SelectableText::new(group, seg, SharedString::from(prepared.text))
                            .with_highlights(styled)
                            .into_any_element()
                    }
                    _ => StyledText::new(SharedString::from(prepared.text))
                        .with_highlights(styled)
                        .into_any_element(),
                })
                .into_any_element()
        })
        .collect();
    div()
        .my(px(8.0))
        .p(px(14.0))
        .rounded(px(Radius::SM))
        .bg(c.surface_1)
        .flex()
        .flex_col()
        .gap(px(2.0))
        .when_some(lang.filter(|l| !l.is_empty()), |d, l| {
            d.child(
                div()
                    .mb(px(4.0))
                    .text_size(px(r.theme.typography.ui_size_xs))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.outline)
                    .child(SharedString::from(l.to_owned())),
            )
        })
        .child(
            div()
                .overflow_hidden()
                .font_family(theme.typography.mono_family)
                .text_size(px(r.theme.typography.mono_size))
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
            .rounded(px(Radius::CHIP))
            .bg(c.surface_3)
            .flex()
            .items_center()
            .justify_center()
            .when(done, |d| d.child(icon(Icon::Check, 12.0, c.text)))
            .into_any_element(),
        None if ordered => div()
            .flex_none()
            .w(px(22.0))
            .text_size(px(r.theme.typography.ui_size_lg))
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
    let was = r.in_list.replace(true);
    let body = render_blocks(&item.blocks, r);
    r.in_list.set(was);
    div()
        .flex()
        .flex_row()
        .gap(px(8.0))
        .pl(px(6.0))
        .child(marker)
        .child(div().flex_1().min_w_0().flex().flex_col().children(body))
        .into_any_element()
}

fn table(t: &MdTable, r: &MdRender<'_>) -> AnyElement {
    let c = r.theme.colors;
    let cell = |inlines: &[MdInline],
                align: MdAlign,
                header: bool,
                row: usize,
                col: usize|
     -> AnyElement {
        if col > 0 {
            r.gap.set(Gap::Tab);
        } else if row > 0 {
            r.gap.set(Gap::Line);
        }
        let d = div()
            .flex_1()
            .min_w_0()
            .px(px(12.0))
            .py(px(8.0))
            .flex()
            .text_size(px(r.theme.typography.ui_size_body))
            .when(header, |d| d.font_weight(FontWeight::MEDIUM))
            .text_color(if header { c.text } else { c.muted });
        let d = match align {
            MdAlign::Right => d.justify_end(),
            MdAlign::Center => d.justify_center(),
            _ => d,
        };
        d.child(inline_el(inlines, r)).into_any_element()
    };
    let align = |i: usize| t.aligns.get(i).copied().unwrap_or(MdAlign::None);
    let header = div().flex().flex_row().bg(c.surface_2).children(
        t.header
            .iter()
            .enumerate()
            .map(|(i, h)| cell(h, align(i), true, 0, i)),
    );
    let rows = t.rows.iter().enumerate().map(|(ri, row)| {
        div().flex().flex_row().children(
            row.iter()
                .enumerate()
                .map(|(i, h)| cell(h, align(i), false, ri + 1, i)),
        )
    });
    div()
        .my(px(8.0))
        .rounded(px(Radius::SM))
        .bg(c.surface_1)
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(header)
        .children(rows)
        .into_any_element()
}

fn image_block(src: &str, alt: &str, r: &MdRender<'_>) -> AnyElement {
    let c = r.theme.colors;
    let local = if src.contains("://") {
        None
    } else {
        let p = PathBuf::from(src);
        let resolved = if p.is_absolute() {
            Some(p)
        } else {
            r.base_dir.as_ref().map(|d| d.join(&p))
        };
        resolved.filter(|p| p.is_file())
    };
    match local {
        Some(path) => div()
            .my(px(8.0))
            .child(
                img(path)
                    .max_w_full()
                    .max_h(px(420.0))
                    .rounded(px(Radius::CONTROL)),
            )
            .into_any_element(),
        None => div()
            .my(px(6.0))
            .text_size(px(r.theme.typography.ui_size_md))
            .text_color(c.outline)
            .child(SharedString::from(format!("[imagen: {alt}]")))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewer_svc::markdown_blocks;

    #[test]
    fn code_blocks_are_numbered_in_document_order_including_nested_ones() {
        let md = "```rust\nfn a() {}\n```\n\ntext\n\n- item\n\n  ```json\n  {}\n  ```\n\n> ```toml\n> a = 1\n> ```\n";
        let blocks = markdown_blocks(md);
        let found = code_blocks(&blocks);
        let langs: Vec<_> = found.iter().map(|(i, l, _)| (*i, l.as_str())).collect();
        assert_eq!(langs, vec![(0, "rust"), (1, "json"), (2, "toml")]);
    }

    #[test]
    fn heading_sizes_shrink() {
        assert!(heading_size(1) > heading_size(2));
        assert!(heading_size(2) > heading_size(3));
        assert_eq!(heading_size(6), heading_size(5));
    }
}
