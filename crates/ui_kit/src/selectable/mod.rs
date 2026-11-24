//! Selectable text: real text selection for gpui 0.2.2, which has none in `StyledText`.
//!
//! * [`SelectionGroup`] is the selection **model** shared by every text run of one message /
//!   panel. A position is `(segment, byte offset)`; a segment is one run of text (a paragraph, a
//!   code line, a table cell). Positions are in *model* coordinates, so a selection survives
//!   scrolling and virtualization (the owner supplies the text of off-screen segments through a
//!   [`SegmentSource`]).
//! * [`SelectableText`] is the element: it shapes and paints its text like `StyledText` (runs or
//!   highlights, wrapping, links) and registers its glyph layout with the group each frame, which
//!   makes hit-testing (`closest index` per glyph) and the selection quads possible.
//! * [`SelectionGroup::container`] is the keyboard-focusable wrapper that owns the mouse
//!   handling: drag, double click (word), triple click (paragraph / line), `Shift`+click (extend),
//!   `Ctrl+C` ([`Copy`]) and `Ctrl+A` ([`SelectAll`]).
//!
//! ```ignore
//! let group = SelectionGroup::new(cx);            // keep it in your view state
//! let mut segs = Segments::new();
//! let a = segs.push("Hello wide world", Gap::Block);
//! let b = segs.push("second paragraph", Gap::Block);
//! group.set_segments(segs);
//! let body = group
//!     .container("msg-1", &theme)
//!     .child(SelectableText::new(&group, a, "Hello wide world"))
//!     .child(SelectableText::new(&group, b, "second paragraph"));
//! ```
//!
//! Cost: nothing runs unless the pointer interacts. A press schedules a repaint, a drag repaints
//! per move, and idle windows keep two cheap mouse listeners per container (a bool check while no
//! drag is going). The selection quads are painted only for segments inside the selection.
//!
//! Links: a plain click (press and release on the same link without dragging) opens it through
//! [`SelectionGroup::on_link`] (default: web and mail links via `cx.open_url`); dragging across
//! a link selects it instead.

mod group;
mod text;

#[cfg(test)]
mod tests;

pub use group::{Copy, Gap, SegmentSource, Segments, SelPos, SelectAll, SelectionGroup};
pub use text::SelectableText;

pub(crate) use group::bind_keys;

/// Key context of [`SelectionGroup::container`].
pub const CONTEXT: &str = "SelachiSelectable";

/// Largest char boundary `<= off`, and never past the end.
pub fn clamp_boundary(text: &str, off: usize) -> usize {
    let mut i = off.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class_of(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// The run of same-class characters around `off` (a word, a blank run or a punctuation run): what a
/// double click selects. At the end of the text it looks at the last character.
pub fn word_range(text: &str, off: usize) -> std::ops::Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let off = clamp_boundary(text, off);
    let at = if off >= text.len() {
        text[..off].char_indices().next_back().map(|(i, _)| i)
    } else {
        Some(off)
    };
    let Some(at) = at else { return 0..0 };
    let class = class_of(text[at..].chars().next().unwrap_or(' '));
    let mut start = at;
    for (i, c) in text[..at].char_indices().rev() {
        if class_of(c) != class {
            break;
        }
        start = i;
    }
    let mut end = at;
    for (i, c) in text[at..].char_indices() {
        if class_of(c) != class {
            break;
        }
        end = at + i + c.len_utf8();
    }
    start..end
}

/// The hard line (between `\n`) containing `off`: what a triple click selects. A soft-wrapped
/// paragraph is one hard line, so it is selected whole.
pub fn line_range(text: &str, off: usize) -> std::ops::Range<usize> {
    let off = clamp_boundary(text, off);
    let start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    let end = text[off..].find('\n').map_or(text.len(), |i| off + i);
    start..end
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn words_blanks_and_punctuation() {
        let t = "let foo_bar = baz(1);";
        assert_eq!(&t[word_range(t, 5)], "foo_bar");
        assert_eq!(&t[word_range(t, 3)], " ");
        assert_eq!(&t[word_range(t, 17)], "(");
        assert_eq!(&t[word_range(t, t.len())], ");");
        assert_eq!(word_range("", 0), 0..0);
    }

    #[test]
    fn words_respect_multibyte_boundaries() {
        let t = "añadí código";
        assert_eq!(&t[word_range(t, 2)], "añadí");
        assert_eq!(&t[word_range(t, 9)], "código");
    }

    #[test]
    fn a_line_is_between_newlines() {
        let t = "one\ntwo words\nthree";
        assert_eq!(&t[line_range(t, 6)], "two words");
        assert_eq!(&t[line_range(t, 0)], "one");
        assert_eq!(&t[line_range(t, t.len())], "three");
    }
}
