//! `HighlightKind` -> theme colors. The mapping uses the theme's ANSI palette so a file looks like
//! the terminal around it, and never the brand accent (DESIGN.md: the accent is a signal, not syntax).

use theme::{Mode, Rgba, Theme, rgb};
use viewer_svc::HighlightKind;

/// Color of a highlight class on `theme`. `None` = the default text color.
pub fn color_for(kind: HighlightKind, theme: &Theme) -> Option<Rgba> {
    let a = |i: usize| rgb(theme.terminal.ansi[i]);
    let light = theme.mode == Mode::Light;
    // The "bright" ANSI variants read better on dark surfaces; on light the base hues are the dark ones.
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

/// Comments read as asides: they are the only class painted in italics.
pub fn is_italic(kind: HighlightKind) -> bool {
    kind == HighlightKind::Comment
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [HighlightKind; 14] = [
        HighlightKind::Keyword,
        HighlightKind::String,
        HighlightKind::Comment,
        HighlightKind::Function,
        HighlightKind::Type,
        HighlightKind::Number,
        HighlightKind::Constant,
        HighlightKind::Variable,
        HighlightKind::Property,
        HighlightKind::Operator,
        HighlightKind::Punctuation,
        HighlightKind::Tag,
        HighlightKind::Attribute,
        HighlightKind::Plain,
    ];

    fn lin(c: f32) -> f64 {
        let c = f64::from(c);
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn luminance(c: Rgba) -> f64 {
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn every_colored_class_is_legible_on_the_code_surface() {
        for theme in [Theme::dark(), Theme::light(), Theme::oled()] {
            for kind in ALL {
                if let Some(c) = color_for(kind, &theme) {
                    let ratio = contrast(c, theme.colors.surface_0);
                    // Punctuation is deliberately quiet, everything else must be easy to read.
                    let min = if kind == HighlightKind::Punctuation { 3.0 } else { 4.0 };
                    assert!(ratio >= min, "{:?} {:?}: {ratio:.2}", theme.mode, kind);
                }
            }
        }
    }

    #[test]
    fn plain_text_uses_the_default_color() {
        let t = Theme::dark();
        assert!(color_for(HighlightKind::Plain, &t).is_none());
        assert!(color_for(HighlightKind::Variable, &t).is_none());
        assert!(color_for(HighlightKind::Keyword, &t).is_some());
    }

    #[test]
    fn never_the_brand_accent() {
        for theme in [Theme::dark(), Theme::light()] {
            for kind in ALL {
                assert_ne!(color_for(kind, &theme), Some(theme.colors.accent));
            }
        }
    }
}
