//! Pure helpers that turn one source line plus its highlight spans into what gets painted:
//! tab expansion, truncation of absurdly long lines, merging syntax colors with overlays (find
//! matches, diff word ranges), and case-insensitive search inside a line.

use std::ops::Range;

use viewer_svc::HighlightKind;

/// Highlight spans of one line: byte ranges relative to the line start.
pub type LineSpans = Vec<(Range<usize>, HighlightKind)>;

/// Display columns per tab stop.
pub const TAB_WIDTH: usize = 4;
/// Lines longer than this are cut (a minified 10 MB bundle is one line): shaping it would stall a frame.
pub const MAX_LINE_BYTES: usize = 1_200;

/// A line ready to paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedLine {
    pub text: String,
    pub spans: LineSpans,
    /// The text was cut at [`MAX_LINE_BYTES`] and ends in an ellipsis.
    pub truncated: bool,
}

/// Largest char boundary `<= i`.
pub fn snap_down(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i`.
pub fn snap_up(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Expands tabs to [`TAB_WIDTH`] stops and cuts the line at `max_bytes`, remapping `spans` so they
/// still point at the same characters. Spans always come back sorted, non-empty and on char
/// boundaries of the returned text.
pub fn prepare_line(
    line: &str,
    spans: &[(Range<usize>, HighlightKind)],
    max_bytes: usize,
) -> PreparedLine {
    if !line.contains('\t') && line.len() <= max_bytes {
        let spans = spans
            .iter()
            .filter_map(|(r, k)| {
                let (s, e) = (snap_down(line, r.start), snap_up(line, r.end));
                (s < e).then_some((s..e, *k))
            })
            .collect();
        return PreparedLine {
            text: line.to_owned(),
            spans,
            truncated: false,
        };
    }

    let mut out = String::with_capacity(line.len().min(max_bytes) + 4);
    // new offset of every old byte (continuation bytes map to their char's start).
    let mut map: Vec<usize> = Vec::with_capacity(line.len() + 1);
    let mut col = 0usize;
    let mut truncated = false;
    for ch in line.chars() {
        if out.len() >= max_bytes {
            truncated = true;
            break;
        }
        let at = out.len();
        map.extend(std::iter::repeat_n(at, ch.len_utf8()));
        if ch == '\t' {
            let n = TAB_WIDTH - col % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    let body_len = out.len();
    if !truncated {
        map.push(body_len);
    }
    let at = |old: usize| map.get(old).copied().unwrap_or(body_len);
    let mut new_spans: LineSpans = spans
        .iter()
        .filter_map(|(r, k)| {
            let (s, e) = (at(r.start), at(r.end));
            (s < e).then_some((s..e, *k))
        })
        .collect();
    if truncated {
        out.push('\u{2026}');
    }
    new_spans.retain(|(r, _)| r.end <= out.len());
    PreparedLine {
        text: out,
        spans: new_spans,
        truncated,
    }
}

/// Segments of `0..len` styled by `base` and/or `over`; both inputs must be sorted and
/// non-overlapping. Segments styled by neither are omitted. The output is sorted and
/// non-overlapping, which is what gpui's `StyledText::with_highlights` requires.
pub fn overlay<A: Copy, B: Copy>(
    base: &[(Range<usize>, A)],
    over: &[(Range<usize>, B)],
    len: usize,
) -> Vec<(Range<usize>, Option<A>, Option<B>)> {
    let mut cuts: Vec<usize> = Vec::with_capacity((base.len() + over.len()) * 2 + 2);
    cuts.push(0);
    cuts.push(len);
    for (r, _) in base {
        cuts.push(r.start.min(len));
        cuts.push(r.end.min(len));
    }
    for (r, _) in over {
        cuts.push(r.start.min(len));
        cuts.push(r.end.min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let (mut bi, mut oi) = (0usize, 0usize);
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        while base.get(bi).is_some_and(|(r, _)| r.end <= s) {
            bi += 1;
        }
        while over.get(oi).is_some_and(|(r, _)| r.end <= s) {
            oi += 1;
        }
        let a = base
            .get(bi)
            .filter(|(r, _)| r.start <= s && s < r.end)
            .map(|(_, a)| *a);
        let b = over
            .get(oi)
            .filter(|(r, _)| r.start <= s && s < r.end)
            .map(|(_, b)| *b);
        if a.is_some() || b.is_some() {
            out.push((s..e, a, b));
        }
    }
    out
}

/// Byte ranges of every occurrence of `needle` in `line`, ignoring ASCII case. Non-ASCII characters
/// must match exactly (so byte offsets are never ambiguous). Empty needles match nothing.
pub fn find_ranges(line: &str, needle: &str) -> Vec<Range<usize>> {
    let (h, n) = (line.as_bytes(), needle.as_bytes());
    let mut out = Vec::new();
    if n.is_empty() || h.len() < n.len() {
        return out;
    }
    let first = n[0].to_ascii_lowercase();
    let mut i = 0;
    while i + n.len() <= h.len() {
        if h[i].to_ascii_lowercase() == first
            && h[i..i + n.len()].eq_ignore_ascii_case(n)
            && line.is_char_boundary(i)
            && line.is_char_boundary(i + n.len())
        {
            out.push(i..i + n.len());
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

/// True when `line` contains `needle` ignoring ASCII case.
pub fn line_contains(line: &str, needle: &str) -> bool {
    let (h, n) = (line.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return false;
    }
    let first = n[0].to_ascii_lowercase();
    h.windows(n.len())
        .any(|w| w[0].to_ascii_lowercase() == first && w.eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use HighlightKind::{Comment, Keyword, String as Str};

    #[test]
    fn plain_line_passes_through() {
        let p = prepare_line("let x = 1;", &[(0..3, Keyword)], 100);
        assert_eq!(p.text, "let x = 1;");
        assert_eq!(p.spans, vec![(0..3, Keyword)]);
        assert!(!p.truncated);
    }

    #[test]
    fn tabs_expand_and_spans_follow() {
        // "\tfn" -> 4 spaces + "fn"; the keyword span 1..3 becomes 4..6.
        let p = prepare_line("\tfn", &[(1..3, Keyword)], 100);
        assert_eq!(p.text, "    fn");
        assert_eq!(p.spans, vec![(4..6, Keyword)]);
        // A tab in the middle goes to the next stop: "a\tb" -> "a   b".
        let p = prepare_line("a\tb", &[(2..3, Comment)], 100);
        assert_eq!(p.text, "a   b");
        assert_eq!(p.spans, vec![(4..5, Comment)]);
    }

    #[test]
    fn long_lines_are_cut_with_an_ellipsis() {
        let line = "x".repeat(5_000);
        let p = prepare_line(&line, &[(0..10, Keyword), (4_000..4_010, Str)], 100);
        assert!(p.truncated);
        assert!(p.text.ends_with('\u{2026}'));
        assert_eq!(p.text.len(), 100 + '\u{2026}'.len_utf8());
        assert_eq!(p.spans, vec![(0..10, Keyword)]);
    }

    #[test]
    fn multibyte_spans_stay_on_boundaries() {
        let line = "\u{e9}\ta\u{f1}b";
        let p = prepare_line(line, &[(0..2, Keyword), (3..7, Str)], 100);
        for (r, _) in &p.spans {
            assert!(
                p.text.is_char_boundary(r.start) && p.text.is_char_boundary(r.end),
                "{r:?} in {:?}",
                p.text
            );
        }
        // Misaligned span (lossy decoding) is snapped instead of panicking.
        let p = prepare_line("\u{e9}", &[(1..2, Keyword)], 100);
        assert!(
            p.spans
                .iter()
                .all(|(r, _)| p.text.is_char_boundary(r.start) && p.text.is_char_boundary(r.end))
        );
    }

    #[test]
    fn overlay_merges_both_layers() {
        let base = [(0..4, 'k'), (6..10, 's')];
        let over = [(2..8, 'm')];
        let out = overlay(&base, &over, 12);
        assert_eq!(
            out,
            vec![
                (0..2, Some('k'), None),
                (2..4, Some('k'), Some('m')),
                (4..6, None, Some('m')),
                (6..8, Some('s'), Some('m')),
                (8..10, Some('s'), None),
            ]
        );
        // Sorted and non-overlapping.
        assert!(out.windows(2).all(|w| w[0].0.end <= w[1].0.start));
    }

    #[test]
    fn overlay_with_no_layers_is_empty() {
        assert!(overlay::<u8, u8>(&[], &[], 5).is_empty());
        let out = overlay::<u8, u8>(&[], &[(1..3, 7)], 5);
        assert_eq!(out, vec![(1..3, None, Some(7))]);
    }

    #[test]
    fn find_is_ascii_case_insensitive() {
        assert_eq!(find_ranges("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
        assert!(find_ranges("abc", "").is_empty());
        assert!(find_ranges("ab", "abc").is_empty());
        // Non-overlapping, left to right.
        assert_eq!(find_ranges("aaaa", "aa"), vec![0..2, 2..4]);
        // Multibyte text: the match ranges are valid boundaries.
        let line = "\u{f1}and\u{fa} \u{f1}";
        for r in find_ranges(line, "\u{f1}") {
            assert!(line.is_char_boundary(r.start) && line.is_char_boundary(r.end));
        }
        assert!(line_contains("Hola Mundo", "mundo"));
        assert!(!line_contains("Hola", "mundo"));
    }
}
