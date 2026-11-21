//! Flattens markdown inline content into one string plus styled ranges (what gpui's `StyledText`
//! wants) and the clickable link ranges. Pure.

use std::ops::Range;

use viewer_svc::MdInline;

/// Style flags of a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

impl InlineStyle {
    pub fn is_plain(&self) -> bool {
        *self == InlineStyle::default()
    }
}

/// Flattened inline content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlatInline {
    pub text: String,
    /// Sorted, non-overlapping, only for runs with a non-default style.
    pub runs: Vec<(Range<usize>, InlineStyle)>,
    /// `(range, url)` of every link, in text order.
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
        // Adjacent pieces of one link (bold inside a link) become one clickable range.
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
            MdInline::Link { url, children } => {
                walk(children, InlineStyle { link: true, ..style }, Some(url), out);
            }
            MdInline::Image { alt, .. } => push(out, &format!("[{alt}]"), style, link),
            MdInline::SoftBreak => push(out, " ", style, link),
            MdInline::HardBreak => push(out, "\n", style, link),
        }
    }
}

/// Links worth opening from a click: web and mail only (relative links point at files the viewer
/// cannot resolve from here).
pub fn is_openable(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use MdInline::{Code, Emph, HardBreak, Link, SoftBreak, Strong, Text};

    #[test]
    fn plain_text_has_no_runs() {
        let f = flatten(&[Text("hello ".into()), Text("world".into())]);
        assert_eq!(f.text, "hello world");
        assert!(f.runs.is_empty());
        assert!(f.links.is_empty());
    }

    #[test]
    fn styles_nest() {
        let f = flatten(&[Text("a ".into()), Strong(vec![Text("b ".into()), Emph(vec![Text("c".into())])]), Code("d".into())]);
        assert_eq!(f.text, "a b cd");
        assert_eq!(f.runs.len(), 3);
        assert_eq!(f.runs[0], (2..4, InlineStyle { bold: true, ..Default::default() }));
        assert_eq!(f.runs[1], (4..5, InlineStyle { bold: true, italic: true, ..Default::default() }));
        assert_eq!(f.runs[2], (5..6, InlineStyle { code: true, ..Default::default() }));
        assert!(f.runs.windows(2).all(|w| w[0].0.end <= w[1].0.start));
    }

    #[test]
    fn links_collect_ranges() {
        let f = flatten(&[
            Text("see ".into()),
            Link { url: "https://x.dev".into(), children: vec![Text("the ".into()), Strong(vec![Text("docs".into())])] },
            Text(".".into()),
        ]);
        assert_eq!(f.text, "see the docs.");
        assert_eq!(f.links, vec![(4..12, "https://x.dev".to_string())]);
        assert!(f.runs.iter().all(|(_, s)| s.link));
    }

    #[test]
    fn breaks_and_images() {
        let f = flatten(&[Text("a".into()), SoftBreak, Text("b".into()), HardBreak, MdInline::Image { src: "x.png".into(), alt: "logo".into() }]);
        assert_eq!(f.text, "a b\n[logo]");
    }

    #[test]
    fn openable_links() {
        assert!(is_openable("https://selachi.dev"));
        assert!(is_openable("HTTP://x"));
        assert!(is_openable("mailto:a@b.c"));
        assert!(!is_openable("docs/PLAN.md"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("file:///c:/x"));
    }
}
