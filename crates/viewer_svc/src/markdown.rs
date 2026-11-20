//! Markdown to a small block/inline model a UI layer can render directly.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Inline content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdInline {
    Text(String),
    Code(String),
    Emph(Vec<MdInline>),
    Strong(Vec<MdInline>),
    Strike(Vec<MdInline>),
    Link {
        url: String,
        children: Vec<MdInline>,
    },
    Image {
        src: String,
        alt: String,
    },
    SoftBreak,
    HardBreak,
}

/// Column alignment of a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdAlign {
    None,
    Left,
    Center,
    Right,
}

/// A simple table: header cells and body rows of inline content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MdTable {
    pub aligns: Vec<MdAlign>,
    pub header: Vec<Vec<MdInline>>,
    pub rows: Vec<Vec<Vec<MdInline>>>,
}

/// One list entry; `checked` is set for `- [ ]` / `- [x]` task items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdListItem {
    pub checked: Option<bool>,
    pub blocks: Vec<MdBlock>,
}

/// Block-level content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Heading {
        level: u8,
        inlines: Vec<MdInline>,
    },
    Paragraph(Vec<MdInline>),
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    List {
        ordered: bool,
        /// First number of an ordered list (1 for bullet lists).
        start: u64,
        items: Vec<MdListItem>,
    },
    Quote(Vec<MdBlock>),
    Rule,
    Table(MdTable),
    /// A paragraph that is just one image.
    Image {
        src: String,
        alt: String,
    },
    /// Raw HTML block, shown verbatim.
    Html(String),
}

/// Parse CommonMark (+ tables, strikethrough, task lists) into blocks.
pub fn markdown_blocks(text: &str) -> Vec<MdBlock> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events: Vec<Event<'_>> = Parser::new_ext(text, opts).collect();
    let mut p = Builder {
        events,
        pos: 0,
        task: None,
    };
    p.blocks()
}

struct Builder<'a> {
    events: Vec<Event<'a>>,
    pos: usize,
    /// Task marker seen while parsing the current list item.
    task: Option<bool>,
}

impl<'a> Builder<'a> {
    fn peek(&self) -> Option<&Event<'a>> {
        self.events.get(self.pos)
    }

    fn next(&mut self) -> Option<Event<'a>> {
        let e = self.events.get(self.pos).cloned();
        if e.is_some() {
            self.pos += 1;
        }
        e
    }

    /// Consume everything up to and including the `End` matching an already consumed `Start`.
    fn skip_to_end(&mut self) {
        let mut depth = 1usize;
        while let Some(e) = self.next() {
            match e {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn is_inline_start(e: &Event<'_>) -> bool {
        match e {
            Event::Text(_)
            | Event::Code(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::InlineMath(_) => true,
            Event::Start(t) => matches!(
                t,
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            ),
            _ => false,
        }
    }

    /// Blocks until the enclosing `End` (consumed) or end of input.
    fn blocks(&mut self) -> Vec<MdBlock> {
        let mut out = Vec::new();
        while let Some(e) = self.peek() {
            if matches!(e, Event::End(_)) {
                self.pos += 1;
                break;
            }
            if Self::is_inline_start(e) {
                // Tight list items carry inlines without a paragraph.
                let inl = self.inline_run();
                if !inl.is_empty() {
                    out.push(MdBlock::Paragraph(inl));
                }
                continue;
            }
            let Some(e) = self.next() else { break };
            match e {
                Event::Start(Tag::Paragraph) => {
                    let inl = self.inlines();
                    out.push(match single_image(&inl) {
                        Some((src, alt)) => MdBlock::Image { src, alt },
                        None => MdBlock::Paragraph(inl),
                    });
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    let inlines = self.inlines();
                    out.push(MdBlock::Heading {
                        level: level as u8,
                        inlines,
                    });
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    let inner = self.blocks();
                    out.push(MdBlock::Quote(inner));
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        CodeBlockKind::Indented => None,
                    };
                    let mut text = String::new();
                    while let Some(e) = self.next() {
                        match e {
                            Event::Text(t) | Event::Code(t) => text.push_str(&t),
                            Event::End(_) => break,
                            _ => {}
                        }
                    }
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    out.push(MdBlock::CodeBlock { lang, text });
                }
                Event::Start(Tag::List(start)) => {
                    let mut items = Vec::new();
                    while let Some(e) = self.peek() {
                        match e {
                            Event::Start(Tag::Item) => {
                                self.pos += 1;
                                self.task = None;
                                let blocks = self.blocks();
                                items.push(MdListItem {
                                    checked: self.task.take(),
                                    blocks,
                                });
                            }
                            Event::End(_) => {
                                self.pos += 1;
                                break;
                            }
                            _ => {
                                self.pos += 1;
                            }
                        }
                    }
                    out.push(MdBlock::List {
                        ordered: start.is_some(),
                        start: start.unwrap_or(1),
                        items,
                    });
                }
                Event::Start(Tag::Table(aligns)) => out.push(MdBlock::Table(self.table(aligns))),
                Event::Start(Tag::HtmlBlock) => {
                    let mut html = String::new();
                    while let Some(e) = self.next() {
                        match e {
                            Event::Html(t) | Event::Text(t) => html.push_str(&t),
                            Event::End(_) => break,
                            _ => {}
                        }
                    }
                    out.push(MdBlock::Html(html.trim_end().to_string()));
                }
                Event::Rule => out.push(MdBlock::Rule),
                Event::TaskListMarker(done) => self.task = Some(done),
                Event::Html(t) => out.push(MdBlock::Html(t.trim_end().to_string())),
                // Footnotes, definition lists, metadata blocks...: not rendered.
                Event::Start(_) => self.skip_to_end(),
                _ => {}
            }
        }
        out
    }

    /// Consecutive inline events (no enclosing tag) as one list.
    fn inline_run(&mut self) -> Vec<MdInline> {
        let mut out = Vec::new();
        while let Some(e) = self.peek() {
            if !Self::is_inline_start(e) {
                break;
            }
            if let Some(i) = self.inline_item() {
                push_inline(&mut out, i);
            }
        }
        out
    }

    /// Inline events until the enclosing `End` (consumed).
    fn inlines(&mut self) -> Vec<MdInline> {
        let mut out = Vec::new();
        while let Some(e) = self.peek() {
            match e {
                Event::End(_) => {
                    self.pos += 1;
                    break;
                }
                Event::TaskListMarker(done) => {
                    self.task = Some(*done);
                    self.pos += 1;
                }
                _ => {
                    if let Some(i) = self.inline_item() {
                        push_inline(&mut out, i);
                    }
                }
            }
        }
        out
    }

    /// Consume one inline event (and its subtree); `None` for ignorable events.
    fn inline_item(&mut self) -> Option<MdInline> {
        let e = self.next()?;
        Some(match e {
            Event::Text(t) => MdInline::Text(t.to_string()),
            Event::Code(t) => MdInline::Code(t.to_string()),
            Event::InlineHtml(t) | Event::Html(t) => MdInline::Text(t.to_string()),
            Event::InlineMath(t) | Event::DisplayMath(t) => MdInline::Code(t.to_string()),
            Event::FootnoteReference(t) => MdInline::Text(format!("[^{t}]")),
            Event::SoftBreak => MdInline::SoftBreak,
            Event::HardBreak => MdInline::HardBreak,
            Event::Start(Tag::Emphasis) => MdInline::Emph(self.inlines()),
            Event::Start(Tag::Strong) => MdInline::Strong(self.inlines()),
            Event::Start(Tag::Strikethrough) => MdInline::Strike(self.inlines()),
            Event::Start(Tag::Link { dest_url, .. }) => MdInline::Link {
                url: dest_url.to_string(),
                children: self.inlines(),
            },
            Event::Start(Tag::Image { dest_url, .. }) => {
                let children = self.inlines();
                MdInline::Image {
                    src: dest_url.to_string(),
                    alt: plain_text(&children),
                }
            }
            Event::Start(_) => {
                self.skip_to_end();
                return None;
            }
            _ => return None,
        })
    }

    fn table(&mut self, aligns: Vec<Alignment>) -> MdTable {
        let mut t = MdTable {
            aligns: aligns
                .into_iter()
                .map(|a| match a {
                    Alignment::None => MdAlign::None,
                    Alignment::Left => MdAlign::Left,
                    Alignment::Center => MdAlign::Center,
                    Alignment::Right => MdAlign::Right,
                })
                .collect(),
            ..MdTable::default()
        };
        while let Some(e) = self.next() {
            match e {
                Event::Start(Tag::TableHead) => {
                    // The head holds its cells directly.
                    t.header = self.row_cells();
                }
                Event::Start(Tag::TableRow) => {
                    let cells = self.row_cells();
                    t.rows.push(cells);
                }
                Event::End(TagEnd::Table) | Event::End(_) => break,
                _ => {}
            }
        }
        t
    }

    /// Cells until the end of the current row/head (consumed).
    fn row_cells(&mut self) -> Vec<Vec<MdInline>> {
        let mut cells = Vec::new();
        while let Some(e) = self.next() {
            match e {
                Event::Start(Tag::TableCell) => cells.push(self.inlines()),
                Event::End(_) => break,
                _ => {}
            }
        }
        cells
    }
}

/// Append, merging adjacent text so `a` + `b` becomes one `Text("ab")`.
fn push_inline(out: &mut Vec<MdInline>, i: MdInline) {
    if let (MdInline::Text(new), Some(MdInline::Text(prev))) = (&i, out.last_mut()) {
        prev.push_str(new);
        return;
    }
    out.push(i);
}

fn single_image(inl: &[MdInline]) -> Option<(String, String)> {
    match inl {
        [MdInline::Image { src, alt }] => Some((src.clone(), alt.clone())),
        _ => None,
    }
}

/// Plain-text rendering of inlines (used for image alt text and previews).
pub fn plain_text(inlines: &[MdInline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            MdInline::Text(t) | MdInline::Code(t) => s.push_str(t),
            MdInline::Emph(c) | MdInline::Strong(c) | MdInline::Strike(c) => {
                s.push_str(&plain_text(c));
            }
            MdInline::Link { children, .. } => s.push_str(&plain_text(children)),
            MdInline::Image { alt, .. } => s.push_str(alt),
            MdInline::SoftBreak | MdInline::HardBreak => s.push(' '),
        }
    }
    s
}
