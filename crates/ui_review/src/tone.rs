//! The semantic colors of the review: diff adds / removes, note states, attention kinds. They are
//! the theme roles (`success`, `warning`, `error`, `info`), never a slot of the terminal palette.

use gpui::Hsla;
use theme::{Rgba, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Success,
    Warning,
    Error,
    Info,
}

/// The theme color of `tone` (legible as text on every surface, see `theme`'s tests).
pub(crate) fn role(theme: &Theme, tone: Tone) -> Rgba {
    let c = theme.colors;
    match tone {
        Tone::Success => c.success,
        Tone::Warning => c.warning,
        Tone::Error => c.error,
        Tone::Info => c.info,
    }
}

/// `tone` at `alpha` (row backgrounds, badges).
pub(crate) fn tint(theme: &Theme, tone: Tone, alpha: f32) -> Hsla {
    let mut c = role(theme, tone);
    c.a = alpha;
    c.into()
}

/// `tone` as a foreground (text and glyphs over a tint).
pub(crate) fn ink(theme: &Theme, tone: Tone) -> Hsla {
    role(theme, tone).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_are_the_theme_roles_and_tints_keep_the_hue() {
        for theme in [Theme::dark(), Theme::light(), Theme::oled()] {
            for (tone, expect) in [
                (Tone::Success, theme.colors.success),
                (Tone::Warning, theme.colors.warning),
                (Tone::Error, theme.colors.error),
                (Tone::Info, theme.colors.info),
            ] {
                assert_eq!(role(&theme, tone), expect);
                let t = tint(&theme, tone, 0.2);
                assert!((t.a - 0.2).abs() < 1e-6);
                assert_eq!(ink(&theme, tone), Hsla::from(expect));
            }
        }
    }
}
