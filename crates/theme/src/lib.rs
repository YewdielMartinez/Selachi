//! Selachi design tokens (PLAN section 11): colors, typography, spacing,
//! radii, motion and embedded fonts.

pub mod fonts;
pub mod motion;

pub use gpui::Rgba;

/// Builds an opaque [`Rgba`] from a `0xRRGGBB` value.
pub const fn rgb(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xFF) as f32 / 255.0,
        g: ((hex >> 8) & 0xFF) as f32 / 255.0,
        b: (hex & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
    Oled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub bg: Rgba,
    pub surface_0: Rgba,
    pub surface_1: Rgba,
    pub surface_2: Rgba,
    pub surface_3: Rgba,
    pub surface_4: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub outline: Rgba,
    pub outline_soft: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub error: Rgba,
}

/// Terminal palette; all values are `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalColors {
    pub ansi: [u32; 16],
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub selection: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Typography {
    pub ui_family: &'static str,
    pub mono_family: &'static str,
    pub mono_size: f32,
    pub mono_line_height: f32,
    pub ui_size_sm: f32,
    pub ui_size_md: f32,
    pub ui_size_lg: f32,
}

impl Typography {
    pub const fn new() -> Self {
        Self {
            ui_family: fonts::UI_FAMILY,
            mono_family: fonts::MONO_FAMILY,
            mono_size: 13.0,
            mono_line_height: 1.35,
            ui_size_sm: 12.0,
            ui_size_md: 13.0,
            ui_size_lg: 15.0,
        }
    }
}

impl Default for Typography {
    fn default() -> Self {
        Self::new()
    }
}

/// 4 px base spacing scale.
pub struct Spacing;

impl Spacing {
    pub const S1: f32 = 4.0;
    pub const S2: f32 = 8.0;
    pub const S3: f32 = 12.0;
    pub const S4: f32 = 16.0;
    pub const S6: f32 = 24.0;
    pub const S8: f32 = 32.0;
    pub const S10: f32 = 40.0;
    pub const S12: f32 = 48.0;
    pub const S16: f32 = 64.0;
}

/// Corner radii. `R4XL` (also exported as `XXXXL`) is the floating-island radius.
pub struct Radius;

impl Radius {
    pub const XS: f32 = 8.0;
    pub const SM: f32 = 12.0;
    pub const MD: f32 = 16.0;
    pub const LG: f32 = 24.0;
    pub const XL: f32 = 32.0;
    pub const R4XL: f32 = 64.0;
    pub const XXXXL: f32 = Self::R4XL;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub mode: Mode,
    pub colors: Colors,
    pub terminal: TerminalColors,
    pub typography: Typography,
}

const ACCENT: u32 = 0x00AAFF;
const ON_ACCENT: u32 = 0x001E2E;

/// Dark ANSI palette, shared by the dark and OLED themes. Restrained,
/// slightly desaturated hues tuned for the cool neutral surfaces; blue is
/// derived from the #00AAFF brand accent.
const ANSI_DARK: [u32; 16] = [
    0x2B2C31, // 0 black
    0xE8707B, // 1 red
    0x7FCB8F, // 2 green
    0xE0B96A, // 3 yellow
    0x3BA4F5, // 4 blue
    0xBC8CE8, // 5 magenta
    0x4FC9D6, // 6 cyan
    0xC5C6CE, // 7 white
    0x6F727C, // 8 bright black
    0xFF9AA2, // 9 bright red
    0xA3E4AE, // 10 bright green
    0xF2D58E, // 11 bright yellow
    0x5CC0FF, // 12 bright blue
    0xD7B3F5, // 13 bright magenta
    0x7FDDE8, // 14 bright cyan
    0xECEEF2, // 15 bright white
];

/// Light ANSI palette: darker, more saturated so every hue stays readable
/// on #FBF9FC.
const ANSI_LIGHT: [u32; 16] = [
    0x1B1B1E, // 0 black
    0xBA1A1A, // 1 red
    0x1A7F37, // 2 green
    0x8A5F00, // 3 yellow
    0x0067B3, // 4 blue
    0x8A3FC0, // 5 magenta
    0x00707F, // 6 cyan
    0xC5C6CE, // 7 white
    0x5B5E66, // 8 bright black
    0xD02B2B, // 9 bright red
    0x15803D, // 10 bright green
    0x946400, // 11 bright yellow
    0x006FC6, // 12 bright blue
    0x9B4FD0, // 13 bright magenta
    0x007F90, // 14 bright cyan
    0x75777E, // 15 bright white
];

impl Theme {
    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            colors: Colors {
                bg: rgb(0x131316),
                surface_0: rgb(0x0D0E10),
                surface_1: rgb(0x1B1B1E),
                surface_2: rgb(0x1F1F22),
                surface_3: rgb(0x292A2C),
                surface_4: rgb(0x343537),
                text: rgb(0xE4E2E5),
                muted: rgb(0xC5C6CE),
                outline: rgb(0x8E9098),
                outline_soft: rgb(0x44474E),
                accent: rgb(ACCENT),
                on_accent: rgb(ON_ACCENT),
                error: rgb(0xFFB4AB),
            },
            terminal: TerminalColors {
                ansi: ANSI_DARK,
                foreground: 0xE4E2E5,
                background: 0x0D0E10, // = surface_0 (floating terminal card)
                cursor: ACCENT,
                selection: 0x0E3950, // accent at 25% over bg
            },
            typography: Typography::new(),
        }
    }

    pub fn light() -> Self {
        Self {
            mode: Mode::Light,
            colors: Colors {
                bg: rgb(0xFBF9FC),
                surface_0: rgb(0xFFFFFF),
                surface_1: rgb(0xF5F3F6),
                surface_2: rgb(0xEFEDF0),
                surface_3: rgb(0xE9E7EA),
                surface_4: rgb(0xE4E2E5),
                text: rgb(0x1B1B1E),
                muted: rgb(0x44474E),
                outline: rgb(0x75777E),
                outline_soft: rgb(0xC5C6CE),
                accent: rgb(ACCENT),
                on_accent: rgb(ON_ACCENT),
                error: rgb(0xBA1A1A),
            },
            terminal: TerminalColors {
                ansi: ANSI_LIGHT,
                foreground: 0x1B1B1E,
                background: 0xFFFFFF, // = surface_0
                cursor: 0x0078D4,
                selection: 0xBCE5FD, // accent at 25% over bg
            },
            typography: Typography::new(),
        }
    }

    pub fn oled() -> Self {
        Self {
            mode: Mode::Oled,
            colors: Colors {
                bg: rgb(0x000000),
                surface_0: rgb(0x050505),
                surface_1: rgb(0x0B0B0B),
                surface_2: rgb(0x121212),
                surface_3: rgb(0x1A1A1A),
                surface_4: rgb(0x222222),
                text: rgb(0xE4E2E5),
                muted: rgb(0xC5C6CE),
                outline: rgb(0x8E9098),
                outline_soft: rgb(0x242424),
                accent: rgb(ACCENT),
                on_accent: rgb(ON_ACCENT),
                error: rgb(0xFFB4AB),
            },
            terminal: TerminalColors {
                ansi: ANSI_DARK,
                foreground: 0xE4E2E5,
                background: 0x050505, // = surface_0
                cursor: ACCENT,
                selection: 0x002B40, // accent at 25% over bg
            },
            typography: Typography::new(),
        }
    }

    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::dark(),
            Mode::Light => Self::light(),
            Mode::Oled => Self::oled(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin(c: u32) -> f64 {
        let c = c as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(hex: u32) -> f64 {
        0.2126 * lin((hex >> 16) & 0xFF) + 0.7152 * lin((hex >> 8) & 0xFF) + 0.0722 * lin(hex & 0xFF)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn hex_of(c: Rgba) -> u32 {
        let q = |v: f32| (v * 255.0).round() as u32;
        (q(c.r) << 16) | (q(c.g) << 8) | q(c.b)
    }

    fn all() -> [Theme; 3] {
        [Theme::dark(), Theme::light(), Theme::oled()]
    }

    #[test]
    fn rgb_helper_roundtrips() {
        assert_eq!(hex_of(rgb(0x00AAFF)), 0x00AAFF);
        assert_eq!(rgb(0x000000).r, 0.0);
        assert_eq!(rgb(0xFFFFFF).b, 1.0);
        assert_eq!(rgb(0x123456).a, 1.0);
    }

    #[test]
    fn terminal_background_matches_card() {
        // The terminal is a floating card painted in surface_0 (DESIGN.md).
        for t in all() {
            assert_eq!(hex_of(t.colors.surface_0), t.terminal.background, "{:?}", t.mode);
        }
        assert_eq!(Theme::dark().terminal.background, 0x0D0E10);
        assert_eq!(Theme::oled().terminal.background, 0x050505);
        assert_eq!(Theme::light().terminal.background, 0xFFFFFF);
    }

    #[test]
    fn plan_table_values() {
        let d = Theme::dark().colors;
        assert_eq!(hex_of(d.surface_0), 0x0D0E10);
        assert_eq!(hex_of(d.text), 0xE4E2E5);
        assert_eq!(hex_of(d.outline_soft), 0x44474E);
        assert_eq!(hex_of(d.accent), 0x00AAFF);
        let l = Theme::light().colors;
        assert_eq!(hex_of(l.surface_0), 0xFFFFFF);
        assert_eq!(hex_of(l.error), 0xBA1A1A);
        assert_eq!(hex_of(Theme::oled().colors.outline_soft), 0x242424);
    }

    #[test]
    fn ui_text_contrast() {
        for t in all() {
            let bg = hex_of(t.colors.bg);
            assert!(contrast(hex_of(t.colors.text), bg) >= 7.0, "{:?} text", t.mode);
            assert!(contrast(hex_of(t.colors.muted), bg) >= 4.5, "{:?} muted", t.mode);
            assert!(contrast(hex_of(t.colors.error), bg) >= 4.5, "{:?} error", t.mode);
            assert!(
                contrast(hex_of(t.colors.on_accent), hex_of(t.colors.accent)) >= 4.5,
                "{:?} on_accent",
                t.mode
            );
        }
    }

    #[test]
    fn ansi_contrast_on_background() {
        for t in all() {
            let bg = t.terminal.background;
            let light = t.mode == Mode::Light;
            for (i, &c) in t.terminal.ansi.iter().enumerate() {
                let ratio = contrast(c, bg);
                let msg = format!("{:?} ansi[{i}] {c:06X} ratio {ratio:.2}", t.mode);
                match i {
                    // Black: only needs to be distinguishable from the bg
                    // (in light mode it is the text color).
                    0 if !light => assert!(ratio >= 1.15, "{msg}"),
                    // White / bright white on a light bg is inherently faint.
                    7 | 15 if light => assert!(ratio >= 1.4, "{msg}"),
                    // Bright black is used for dim output / comments.
                    8 => assert!(ratio >= 3.0, "{msg}"),
                    _ => assert!(ratio >= 4.5, "{msg}"),
                }
            }
            assert!(contrast(t.terminal.foreground, bg) >= 7.0);
            assert!(contrast(t.terminal.cursor, bg) >= 3.0);
        }
    }

    #[test]
    fn selection_keeps_text_readable() {
        for t in all() {
            let sel = t.terminal.selection;
            assert!(contrast(t.terminal.foreground, sel) >= 7.0, "{:?}", t.mode);
            for i in [1, 2, 3, 4, 5, 6] {
                assert!(contrast(t.terminal.ansi[i], sel) >= 3.0, "{:?} ansi[{i}]", t.mode);
            }
        }
    }

    #[test]
    fn ansi_colors_are_distinct() {
        for t in all() {
            let a = t.terminal.ansi;
            for i in 0..16 {
                for j in (i + 1)..16 {
                    assert_ne!(a[i], a[j], "{:?} {i} == {j}", t.mode);
                }
            }
        }
    }

    #[test]
    fn typography_and_scales() {
        let ty = Theme::dark().typography;
        assert_eq!(ty.ui_family, "DM Sans");
        assert_eq!(ty.mono_family, "Geist Mono");
        assert_eq!(ty.mono_size, 13.0);
        assert_eq!(ty.mono_line_height, 1.35);
        assert_eq!(Spacing::S16, 64.0);
        assert_eq!(Radius::XXXXL, Radius::R4XL);
        assert_eq!(Radius::XS, 8.0);
    }
}
