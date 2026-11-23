//! The pure editing model of [`crate::TextField`]: text plus a selection, all offsets in bytes on
//! char boundaries. Everything here is unit tested without a window.

use std::ops::Range;

/// Text, selection and IME composition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBuf {
    pub text: String,
    /// `start..end`; empty = a caret.
    pub sel: Range<usize>,
    /// The caret is at `sel.start` (the selection was extended leftwards).
    pub reversed: bool,
    /// The IME composition (underlined), if any.
    pub marked: Option<Range<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

impl TextBuf {
    /// A buffer holding `text` with the caret at the end.
    pub fn with_text(text: &str) -> Self {
        let mut b = Self { text: text.to_string(), ..Self::default() };
        b.move_to(text.len());
        b
    }

    pub fn cursor(&self) -> usize {
        if self.reversed { self.sel.start } else { self.sel.end }
    }

    pub fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..offset].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..].chars().next().map_or(self.text.len(), |c| offset + c.len_utf8())
    }

    /// Start of the word before `offset` (skips spaces, then one run of word or punctuation).
    pub fn prev_word(&self, offset: usize) -> usize {
        let mut it = self.text[..offset].char_indices().rev().peekable();
        while it.next_if(|(_, c)| class(*c) == Class::Space).is_some() {}
        let Some(&(mut start, first)) = it.peek() else { return 0 };
        let k = class(first);
        while let Some(&(i, c)) = it.peek() {
            if class(c) != k {
                break;
            }
            start = i;
            it.next();
        }
        start
    }

    /// Start of the next word after `offset` (skips the current run, then the spaces after it).
    pub fn next_word(&self, offset: usize) -> usize {
        let mut it = self.text[offset..].char_indices().peekable();
        if let Some(&(_, c)) = it.peek() {
            let k = class(c);
            if k != Class::Space {
                while it.next_if(|(_, c)| class(*c) == k).is_some() {}
            }
        }
        while it.next_if(|(_, c)| class(*c) == Class::Space).is_some() {}
        it.peek().map_or(self.text.len(), |&(i, _)| offset + i)
    }

    /// The word (or run of spaces / punctuation) around `offset`.
    pub fn word_at(&self, offset: usize) -> Range<usize> {
        let offset = offset.min(self.text.len());
        let probe = self.text[offset..].chars().next().or_else(|| self.text[..offset].chars().next_back());
        let Some(k) = probe.map(class) else { return offset..offset };
        let mut start = offset;
        for (i, c) in self.text[..offset].char_indices().rev() {
            if class(c) != k {
                break;
            }
            start = i;
        }
        let mut end = offset;
        for (i, c) in self.text[offset..].char_indices() {
            if class(c) != k {
                break;
            }
            end = offset + i + c.len_utf8();
        }
        start..end
    }

    pub fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |i| i + 1)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        self.text[offset..].find('\n').map_or(self.text.len(), |i| offset + i)
    }

    pub fn move_to(&mut self, offset: usize) {
        self.sel = offset..offset;
        self.reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        if self.reversed {
            self.sel.start = offset;
        } else {
            self.sel.end = offset;
        }
        if self.sel.end < self.sel.start {
            self.reversed = !self.reversed;
            self.sel = self.sel.end..self.sel.start;
        }
    }

    pub fn select_range(&mut self, r: Range<usize>) {
        self.sel = r;
        self.reversed = false;
    }

    pub fn left(&mut self) {
        if self.sel.is_empty() {
            self.move_to(self.prev_boundary(self.cursor()));
        } else {
            self.move_to(self.sel.start);
        }
    }

    pub fn right(&mut self) {
        if self.sel.is_empty() {
            self.move_to(self.next_boundary(self.sel.end));
        } else {
            self.move_to(self.sel.end);
        }
    }

    pub fn select_all(&mut self) {
        self.move_to(0);
        self.select_to(self.text.len());
    }

    /// Replace `range` (default: the marked text, else the selection) with `new`; the caret ends
    /// after it. Offsets are clamped to the text.
    pub fn replace(&mut self, range: Option<Range<usize>>, new: &str) {
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.sel.clone());
        let end = range.end.min(self.text.len());
        let range = range.start.min(end)..end;
        self.text.replace_range(range.clone(), new);
        let caret = range.start + new.len();
        self.sel = caret..caret;
        self.reversed = false;
        self.marked = None;
    }

    /// Deletes the selection, or the char before the caret. `true` when something was removed.
    pub fn backspace(&mut self) -> bool {
        if self.sel.is_empty() {
            if self.cursor() == 0 {
                return false;
            }
            self.select_to(self.prev_boundary(self.cursor()));
        }
        self.replace(None, "");
        true
    }

    /// Deletes the selection, or the char after the caret.
    pub fn delete(&mut self) -> bool {
        if self.sel.is_empty() {
            if self.cursor() == self.text.len() {
                return false;
            }
            self.select_to(self.next_boundary(self.cursor()));
        }
        self.replace(None, "");
        true
    }

    /// Deletes the selection, or the word before the caret.
    pub fn delete_word(&mut self) -> bool {
        if self.sel.is_empty() {
            let caret = self.cursor();
            if caret == 0 {
                return false;
            }
            self.move_to(caret);
            self.select_to(self.prev_word(caret));
        }
        self.replace(None, "");
        true
    }

    /// Deletes the selection, or the word after the caret.
    pub fn delete_word_forward(&mut self) -> bool {
        if self.sel.is_empty() {
            let caret = self.cursor();
            if caret == self.text.len() {
                return false;
            }
            self.move_to(caret);
            self.select_to(self.next_word(caret));
        }
        self.replace(None, "");
        true
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.sel.clone()]
    }

    pub fn offset_from_utf16(&self, off: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.text.chars() {
            if utf16 >= off {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    pub fn offset_to_utf16(&self, off: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.text.chars() {
            if utf8 >= off {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    pub fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    pub fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }
}

/// `\r\n` and `\r` to `\n`; with `multiline` off every newline becomes a space.
pub fn clean(text: &str, multiline: bool) -> String {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline { t } else { t.replace('\n', " ") }
}

/// `abc...xyz`: as much of the start (60 %) and the end of `text` as fits in `max_w`, measured with
/// `shape` (width of a candidate string). Pure apart from the closure.
pub fn middle_ellipsis(text: &str, max_w: f32, shape: &dyn Fn(&str) -> f32) -> String {
    if shape(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |n: usize| -> String {
        let pre = (n * 3).div_ceil(5);
        let suf = n - pre;
        let mut out: String = chars[..pre].iter().collect();
        out.push('\u{2026}');
        out.extend(chars[chars.len() - suf..].iter());
        out
    };
    // Largest kept count that fits (the width grows with it; binary search).
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if shape(&build(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str, caret: usize) -> TextBuf {
        let mut b = TextBuf { text: text.into(), ..TextBuf::default() };
        b.move_to(caret);
        b
    }

    #[test]
    fn typing_and_deleting_respect_char_boundaries() {
        let mut b = buf("", 0);
        b.replace(None, "Diseño");
        assert_eq!(b.text, "Diseño");
        b.backspace();
        assert_eq!(b.text, "Diseñ");
        b.left();
        b.left();
        b.delete();
        assert_eq!(b.text, "Disñ");
        b.move_to(0);
        b.replace(None, "ñ");
        assert_eq!(b.text, "ñDisñ");
        assert_eq!(b.cursor(), "ñ".len());
    }

    #[test]
    fn deleting_at_the_edges_changes_nothing() {
        let mut b = buf("ab", 0);
        assert!(!b.backspace());
        b.move_to(2);
        assert!(!b.delete());
        assert!(!b.delete_word_forward());
        b.move_to(0);
        assert!(!b.delete_word());
        assert_eq!(b.text, "ab");
    }

    #[test]
    fn selection_extends_both_ways_and_is_replaced() {
        let mut b = buf("hola mundo", 4);
        b.select_to(0);
        assert!(b.reversed);
        assert_eq!(b.selected_text(), "hola");
        b.select_to(b.next_boundary(b.cursor()));
        assert_eq!(b.selected_text(), "ola");
        b.select_all();
        b.replace(None, "x");
        assert_eq!(b.text, "x");
        assert_eq!(b.sel, 1..1);
    }

    #[test]
    fn arrows_collapse_a_selection() {
        let mut b = buf("abcd", 1);
        b.select_to(3);
        b.left();
        assert_eq!(b.sel, 1..1);
        b.select_to(3);
        b.right();
        assert_eq!(b.sel, 3..3);
    }

    #[test]
    fn word_motion_skips_runs_like_a_native_field() {
        let b = buf("uno dos  tres.cuatro", 0);
        assert_eq!(b.next_word(0), 4);
        assert_eq!(b.next_word(4), 9);
        // From inside a word: to the start of the next run (here the dot).
        assert_eq!(b.next_word(10), 13);
        assert_eq!(b.next_word(13), 14);
        assert_eq!(b.next_word(14), 20, "end of text");
        assert_eq!(b.prev_word(20), 14);
        assert_eq!(b.prev_word(14), 13, "the dot is its own run");
        assert_eq!(b.prev_word(9), 4, "skips the spaces first");
        assert_eq!(b.prev_word(3), 0);
        assert_eq!(b.prev_word(0), 0);
    }

    #[test]
    fn delete_word_stops_at_the_word_start() {
        let mut b = buf("uno dos tres", 12);
        b.delete_word();
        assert_eq!(b.text, "uno dos ");
        b.delete_word();
        assert_eq!(b.text, "uno ");
        let mut b = buf("uno dos tres", 0);
        b.delete_word_forward();
        assert_eq!(b.text, "dos tres");
        // With a selection it deletes exactly the selection.
        let mut b = buf("uno dos", 0);
        b.select_to(2);
        b.delete_word();
        assert_eq!(b.text, "o dos");
    }

    #[test]
    fn word_at_selects_a_run() {
        let b = buf("hola, mundo", 0);
        assert_eq!(b.word_at(1), 0..4);
        assert_eq!(b.word_at(4), 4..5, "the comma");
        assert_eq!(b.word_at(5), 5..6, "the space");
        assert_eq!(b.word_at(8), 6..11);
        assert_eq!(b.word_at(11), 6..11, "at the very end: the last word");
        assert_eq!(buf("", 0).word_at(0), 0..0);
    }

    #[test]
    fn lines_for_multiline_text() {
        let b = buf("ab\ncd\nef", 4);
        assert_eq!(b.line_start(4), 3);
        assert_eq!(b.line_end(4), 5);
        assert_eq!(b.line_start(1), 0);
        assert_eq!(b.line_end(7), 8);
    }

    #[test]
    fn replace_clamps_out_of_range_offsets() {
        let mut b = buf("abc", 3);
        b.replace(Some(1..99), "Z");
        assert_eq!(b.text, "aZ");
        b.replace(Some(50..60), "!");
        assert_eq!(b.text, "aZ!");
    }

    #[test]
    fn utf16_offsets_roundtrip() {
        let b = buf("a😀ñ", 0);
        let end = b.text.len();
        assert_eq!(b.offset_to_utf16(end), 4);
        assert_eq!(b.offset_from_utf16(3), "a😀".len());
        assert_eq!(b.range_from_utf16(&(1..3)), 1.."a😀".len());
    }

    #[test]
    fn clean_normalises_newlines() {
        assert_eq!(clean("a\r\nb\rc", true), "a\nb\nc");
        assert_eq!(clean("a\r\nb\rc\nd", false), "a b c d");
    }

    #[test]
    fn middle_ellipsis_fits_and_keeps_both_ends() {
        let w = |t: &str| t.chars().count() as f32 * 10.0;
        let url = "https://github.com/YewdielMartinez/Selachi/releases/latest/download/grammars.toml";
        let out = middle_ellipsis(url, 300.0, &w);
        assert!(w(&out) <= 300.0, "{out}");
        assert!(out.starts_with("https://git"), "{out}");
        assert!(out.ends_with("toml") && out.contains('\u{2026}'), "{out}");
        // Fits: untouched. Zero room: just the ellipsis.
        assert_eq!(middle_ellipsis("corto", 300.0, &w), "corto");
        assert_eq!(middle_ellipsis(url, 5.0, &w), "\u{2026}");
    }
}
