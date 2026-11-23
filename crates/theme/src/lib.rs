//! Selachi design tokens (PLAN section 11): colors, typography, spacing,
//! radii, motion and embedded fonts.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

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
    /// Action blue (`#005EEB`): fill of the one call-to-action button, active toggles and
    /// strong selection. Never a text color on dark surfaces (see docs/DESIGN.md).
    pub accent_strong: Rgba,
    /// Hover fill of the action button (`#1A72FF`).
    pub accent_strong_hover: Rgba,
    /// Press fill of the action button (`#0052CC`).
    pub accent_strong_press: Rgba,
    /// Top stop of the action button's vertical gradient (`#1A72FF`).
    pub accent_strong_gradient_top: Rgba,
    /// Label/icon color on `accent_strong` (`#FFFFFF`).
    pub on_accent_strong: Rgba,
    pub error: Rgba,
    /// Success / "ok" / done. Legible as text on `bg` and `surface_0..=surface_2` (>= 4.5:1) and as
    /// an indicator (dots, glyphs, >= 3:1). Replaces borrowing `terminal.ansi[2]`.
    pub success: Rgba,
    /// Ink on a `success` fill (>= 4.5:1).
    pub on_success: Rgba,
    /// Warning / "waiting" / dirty. Same legibility guarantees as `success`.
    pub warning: Rgba,
    /// Ink on a `warning` fill (>= 4.5:1).
    pub on_warning: Rgba,
    /// Neutral information (hints, "new", links that are not the accent). Same guarantees.
    pub info: Rgba,
    /// Ink on an `info` fill (>= 4.5:1).
    pub on_info: Rgba,
    /// Translucent overlay painted behind drawers, sheets and dialogs. Straight alpha (not
    /// pre-multiplied); `Rgba` -> `Hsla` keeps the alpha.
    pub scrim: Rgba,
    /// Base tint of the frosted-glass zones (sidebar, right panel, drawers); `ui_kit::glass_tint`
    /// applies the user's translucency to it.
    pub glass: Rgba,
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
    /// Micro labels and section headers (11).
    pub ui_size_xs: f32,
    /// Secondary text, chips, meta (12).
    pub ui_size_sm: f32,
    /// Default UI text (13).
    pub ui_size_md: f32,
    /// Body text of chat / action buttons (14).
    pub ui_size_body: f32,
    /// Panel titles (15).
    pub ui_size_lg: f32,
    /// Large titles, onboarding (20).
    pub ui_size_xl: f32,
    /// Display numbers and hero titles (28).
    pub ui_size_display: f32,
}

impl Typography {
    pub const fn new() -> Self {
        Self {
            ui_family: fonts::UI_FAMILY,
            mono_family: fonts::MONO_FAMILY,
            mono_size: 13.0,
            mono_line_height: 1.35,
            ui_size_xs: 11.0,
            ui_size_sm: 12.0,
            ui_size_md: 13.0,
            ui_size_body: 14.0,
            ui_size_lg: 15.0,
            ui_size_xl: 20.0,
            ui_size_display: 28.0,
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
///
/// Component radii: [`Radius::PILL`] (active tab, rail button, list row), [`Radius::CONTROL`]
/// (inputs, status chips, segmented items) and [`Radius::CHIP`] (small tags, kbd hints).
pub struct Radius;

impl Radius {
    /// Active-state pill (docs/DESIGN.md rule 4): rail buttons, tabs, list rows.
    pub const PILL: f32 = 10.0;
    /// Inputs, selects, compact chips.
    pub const CONTROL: f32 = 8.0;
    /// Small tags and badges.
    pub const CHIP: f32 = 6.0;
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
const ACCENT_STRONG: u32 = 0x005EEB;
const ACCENT_STRONG_HOVER: u32 = 0x1A72FF;
const ACCENT_STRONG_PRESS: u32 = 0x0052CC;
const ACCENT_STRONG_GRADIENT_TOP: u32 = 0x1A72FF;
const ON_ACCENT_STRONG: u32 = 0xFFFFFF;

/// Window-close button on hover (docs/DESIGN.md titlebar), `0xRRGGBB`.
pub const CLOSE_HOVER: u32 = 0xC42B1C;
/// Window-close button while pressed.
pub const CLOSE_PRESS: u32 = 0x9E2216;
/// Glyph on the close button in both states.
pub const ON_CLOSE: u32 = 0xFFFFFF;

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
    0x44474E, // 7 white (PSReadLine paints plain arguments with it: must read on #FFFFFF)
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
                accent_strong: rgb(ACCENT_STRONG),
                accent_strong_hover: rgb(ACCENT_STRONG_HOVER),
                accent_strong_press: rgb(ACCENT_STRONG_PRESS),
                accent_strong_gradient_top: rgb(ACCENT_STRONG_GRADIENT_TOP),
                on_accent_strong: rgb(ON_ACCENT_STRONG),
                error: rgb(0xFFB4AB),
                success: rgb(0x7FCB8F),
                on_success: rgb(0x0B2A14),
                warning: rgb(0xE0B96A),
                on_warning: rgb(0x2B2000),
                info: rgb(0x5CC0FF),
                on_info: rgb(0x00263A),
                scrim: Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.45 },
                glass: rgb(0x2A2A2E),
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
                accent_strong: rgb(ACCENT_STRONG),
                accent_strong_hover: rgb(ACCENT_STRONG_HOVER),
                accent_strong_press: rgb(ACCENT_STRONG_PRESS),
                accent_strong_gradient_top: rgb(ACCENT_STRONG_GRADIENT_TOP),
                on_accent_strong: rgb(ON_ACCENT_STRONG),
                error: rgb(0xBA1A1A),
                success: rgb(0x17742F),
                on_success: rgb(0xFFFFFF),
                warning: rgb(0x8A5F00),
                on_warning: rgb(0xFFFFFF),
                info: rgb(0x0067B3),
                on_info: rgb(0xFFFFFF),
                scrim: Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.32 },
                glass: rgb(0xFFFFFF),
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
                accent_strong: rgb(ACCENT_STRONG),
                accent_strong_hover: rgb(ACCENT_STRONG_HOVER),
                accent_strong_press: rgb(ACCENT_STRONG_PRESS),
                accent_strong_gradient_top: rgb(ACCENT_STRONG_GRADIENT_TOP),
                on_accent_strong: rgb(ON_ACCENT_STRONG),
                error: rgb(0xFFB4AB),
                success: rgb(0x7FCB8F),
                on_success: rgb(0x0B2A14),
                warning: rgb(0xE0B96A),
                on_warning: rgb(0x2B2000),
                info: rgb(0x5CC0FF),
                on_info: rgb(0x00263A),
                scrim: Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.6 },
                glass: rgb(0x121214),
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

/// Parses `#RRGGBB` (the `#` is optional) into `0xRRGGBB`.
pub fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    (h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

fn linear(c: u32) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn luminance(hex: u32) -> f64 {
    0.2126 * linear((hex >> 16) & 0xFF) + 0.7152 * linear((hex >> 8) & 0xFF) + 0.0722 * linear(hex & 0xFF)
}

/// WCAG contrast ratio of two `0xRRGGBB` colours (1.0 ..= 21.0).
pub fn contrast_ratio(a: u32, b: u32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `hex` blended over `base` at `alpha` (0.0 ..= 1.0), per channel.
fn blend(hex: u32, base: u32, alpha: f64) -> u32 {
    let ch = |shift: u32| {
        let (a, b) = (f64::from((hex >> shift) & 0xFF), f64::from((base >> shift) & 0xFF));
        ((a * alpha + b * (1.0 - alpha)).round() as u32).min(255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

fn hex_of(c: Rgba) -> u32 {
    let q = |v: f32| (v * 255.0).round() as u32;
    (q(c.r) << 16) | (q(c.g) << 8) | q(c.b)
}

/// Text on the accent fill: the brand's dark ink, else white, whichever reads (WCAG >= 4.5).
fn on_accent_for(accent: u32) -> u32 {
    const LIGHT: u32 = 0xFFFFFF;
    if contrast_ratio(accent, ON_ACCENT) >= 4.5 {
        ON_ACCENT
    } else if contrast_ratio(accent, LIGHT) >= 4.5 {
        LIGHT
    } else if contrast_ratio(accent, ON_ACCENT) >= contrast_ratio(accent, LIGHT) {
        ON_ACCENT
    } else {
        LIGHT
    }
}

/// Saturation below which an accent counts as gray/white (`#ECEEF2`).
const GRAY_SATURATION: f64 = 0.10;
/// Saturation of the derived action colour.
const STRONG_SATURATION: f64 = 0.90;
/// WCAG contrast the label on the action colour must reach.
const LABEL_CONTRAST: f64 = 4.5;
/// Same for the hover / press tones, which are only shown for a moment.
const HOVER_CONTRAST: f64 = 3.0;

/// `0xRRGGBB` to `(hue 0..360, saturation 0..1, lightness 0..1)`.
fn to_hsl(hex: u32) -> (f64, f64, f64) {
    let c = |shift: u32| f64::from((hex >> shift) & 0xFF) / 255.0;
    let (r, g, b) = (c(16), c(8), c(0));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-9 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if (max - r).abs() < 1e-9 {
        ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < 1e-9 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// Inverse of [`to_hsl`].
fn from_hsl(h: f64, s: f64, l: f64) -> u32 {
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let q = |v: f64| (((v + m) * 255.0).round() as u32).min(255);
    (q(r) << 16) | (q(g) << 8) | q(b)
}

/// The action-button colours derived from an accent: `(rest, hover = gradient top, press, label)`.
/// Same hue, high saturation, lightness lowered until a white label reads (>= 4.5); hover +8 %,
/// press -6 % lightness. A gray / white accent gives the inverted-text button of the mode.
fn strong_for(accent: u32, mode: Mode, text: u32, bg: u32) -> (u32, u32, u32, u32) {
    let (h, _, _) = to_hsl(accent);
    // HSV saturation: a near-white tone like #ECEEF2 is 2 % there (HSL would say 17 %).
    let s = {
        let c = |shift: u32| f64::from((accent >> shift) & 0xFF) / 255.0;
        let max = c(16).max(c(8)).max(c(0));
        if max < 1e-9 { 0.0 } else { (max - c(16).min(c(8)).min(c(0))) / max }
    };
    let shift = |hex: u32, dl: f64| {
        let (h, s, l) = to_hsl(hex);
        from_hsl(h, s, l + dl)
    };
    if s < GRAY_SATURATION {
        // Ink button on light, light button on dark, with the other tone as its label.
        let rest = text;
        let label = if mode == Mode::Light { 0xFFFFFF } else { bg };
        return (rest, shift(rest, 0.08), shift(rest, -0.06), label);
    }
    let mut l = 0.60;
    let mut rest = from_hsl(h, STRONG_SATURATION, l);
    // The rest tone reads (>= 4.5) and so does its +8 % hover (>= 3, it is only a moment).
    while (contrast_ratio(rest, 0xFFFFFF) < LABEL_CONTRAST || contrast_ratio(shift(rest, 0.08), 0xFFFFFF) < HOVER_CONTRAST)
        && l > 0.05
    {
        l -= 0.005;
        rest = from_hsl(h, STRONG_SATURATION, l);
    }
    (rest, shift(rest, 0.08), shift(rest, -0.06), 0xFFFFFF)
}

impl Theme {
    /// This theme with the user's accent (`#RRGGBB` of `[appearance] accent`): `colors.accent`,
    /// the legible `on_accent`, the terminal cursor and the selection (accent at 25 % over the
    /// background, the formula the base themes use). The action colours (`accent_strong` and its
    /// hover / press / gradient / label) are derived from it (docs/DESIGN.md "Acento
    /// personalizado"); the default accent keeps the design values exactly. The cursor keeps the mode's own colour when the accent would not be
    /// visible on the terminal background (contrast < 3, such as `#ECEEF2` on white).
    pub fn with_accent(mut self, hex: u32) -> Self {
        let hex = hex & 0xFF_FFFF;
        self.colors.accent = rgb(hex);
        self.colors.on_accent = rgb(on_accent_for(hex));
        if contrast_ratio(hex, self.terminal.background) >= 3.0 {
            self.terminal.cursor = hex;
        }
        self.terminal.selection = blend(hex, hex_of(self.colors.bg), 0.25);
        if hex != ACCENT {
            let (rest, hover, press, label) = strong_for(hex, self.mode, hex_of(self.colors.text), hex_of(self.colors.bg));
            self.colors.accent_strong = rgb(rest);
            self.colors.accent_strong_hover = rgb(hover);
            self.colors.accent_strong_gradient_top = rgb(hover);
            self.colors.accent_strong_press = rgb(press);
            self.colors.on_accent_strong = rgb(label);
        }
        self
    }

    /// [`Theme::with_accent`] from the config's string; an unparsable value leaves the theme as is.
    pub fn with_accent_str(self, accent: &str) -> Self {
        match parse_hex(accent) {
            Some(hex) => self.with_accent(hex),
            None => self,
        }
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
    fn parse_hex_reads_rrggbb() {
        assert_eq!(parse_hex("#00AAFF"), Some(0x00AAFF));
        assert_eq!(parse_hex(" 7fcb8f "), Some(0x7FCB8F));
        assert_eq!(parse_hex("#ABC"), None);
        assert_eq!(parse_hex("blue"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn the_default_accent_changes_nothing() {
        for t in [Theme::dark(), Theme::oled()] {
            assert_eq!(t.with_accent(ACCENT), t);
        }
        // Light keeps its darker cursor (the brand accent is too pale on white to be a cursor).
        let l = Theme::light().with_accent(ACCENT);
        assert_eq!(l.terminal.cursor, 0x0078D4);
        assert_eq!(l.terminal.selection, Theme::light().terminal.selection);
        assert_eq!(l.colors.on_accent, Theme::light().colors.on_accent);
    }

    #[test]
    fn a_custom_accent_reaches_accent_cursor_selection_and_ink() {
        for base in all() {
            for hex in [0x7FCB8F, 0xE0B96A, 0xBC8CE8, 0xE8707B, 0x4FC9D6, 0xECEEF2, 0x112233] {
                let t = base.with_accent(hex);
                assert_eq!(hex_of(t.colors.accent), hex);
                // Whichever ink reads better is chosen: never worse than the other candidate.
                let ink = contrast_ratio(hex, hex_of(t.colors.on_accent));
                assert!(ink >= contrast_ratio(hex, 0xFFFFFF).min(contrast_ratio(hex, ON_ACCENT)), "{hex:06X} ink {ink}");
                assert_eq!(t.colors.error, base.colors.error);
            }
        }
        let g = Theme::dark().with_accent(0x7FCB8F);
        assert_eq!(g.terminal.cursor, 0x7FCB8F);
        assert_eq!(g.terminal.selection, blend(0x7FCB8F, 0x131316, 0.25));
        // The pale accent has no visible cursor on white: the mode's own stays.
        let pale = Theme::light().with_accent(0xECEEF2);
        assert_eq!(pale.terminal.cursor, 0x0078D4);
        assert!(contrast_ratio(hex_of(pale.colors.accent), hex_of(pale.colors.on_accent)) >= 4.5);
    }

    #[test]
    fn selection_is_the_accent_at_25_percent_over_the_background() {
        // 0x7FCB8F over #131316: r = 0.25*127 + 0.75*19 = 46 ...
        let t = Theme::dark().with_accent(0x7FCB8F);
        assert_eq!(t.terminal.selection, blend(0x7FCB8F, 0x131316, 0.25));
        assert_eq!((t.terminal.selection >> 16) & 0xFF, 46);
    }

    /// The swatches of Ajustes > Apariencia (`ui_settings::ACCENTS`).
    const SWATCHES: [u32; 8] = [0x00AAFF, 0x4FC9D6, 0x7FCB8F, 0xE0B96A, 0xFF9AA2, 0xE8707B, 0xBC8CE8, 0xECEEF2];

    #[test]
    fn the_action_button_reads_on_every_swatch_and_mode() {
        for base in all() {
            for hex in SWATCHES {
                let t = base.with_accent(hex);
                let c = t.colors;
                let label = contrast_ratio(hex_of(c.accent_strong), hex_of(c.on_accent_strong));
                assert!(label >= 4.5, "{:?} {hex:06X}: label contrast {label}", base.mode);
                // Hover and press are other tones of the same button, still readable.
                for state in [c.accent_strong_hover, c.accent_strong_press] {
                    let k = contrast_ratio(hex_of(state), hex_of(c.on_accent_strong));
                    assert!(k >= 3.0, "{:?} {hex:06X}: state contrast {k}", base.mode);
                }
                assert_ne!(c.accent_strong_hover, c.accent_strong_press);
                assert_eq!(c.accent_strong_hover, c.accent_strong_gradient_top);
            }
        }
    }

    #[test]
    fn the_default_accent_keeps_the_design_blues_exactly() {
        for base in all() {
            let c = base.with_accent(ACCENT).colors;
            assert_eq!(hex_of(c.accent_strong), 0x005EEB);
            assert_eq!(hex_of(c.accent_strong_hover), 0x1A72FF);
            assert_eq!(hex_of(c.accent_strong_gradient_top), 0x1A72FF);
            assert_eq!(hex_of(c.accent_strong_press), 0x0052CC);
            assert_eq!(hex_of(c.on_accent_strong), 0xFFFFFF);
        }
    }

    #[test]
    fn a_colored_accent_gives_a_saturated_action_color_of_its_hue() {
        let t = Theme::dark().with_accent(0x7FCB8F);
        let (h_acc, _, _) = to_hsl(0x7FCB8F);
        let (h, s, l) = to_hsl(hex_of(t.colors.accent_strong));
        assert!((h - h_acc).abs() < 3.0, "hue {h} vs {h_acc}");
        assert!(s > 0.8 && l < 0.45, "s {s} l {l}");
        assert_ne!(t.colors.accent_strong, Theme::dark().colors.accent_strong);
    }

    #[test]
    fn gray_accents_invert_the_text_tone() {
        let light = Theme::light().with_accent(0xECEEF2).colors;
        assert_eq!(hex_of(light.accent_strong), 0x1B1B1E, "ink button on light");
        assert_eq!(hex_of(light.on_accent_strong), 0xFFFFFF);
        let dark = Theme::dark().with_accent(0xECEEF2).colors;
        assert_eq!(hex_of(dark.accent_strong), 0xE4E2E5, "light button on dark");
        assert_eq!(hex_of(dark.on_accent_strong), 0x131316);
    }

    #[test]
    fn hsl_round_trips() {
        for hex in [0x7FCB8F, 0xE0B96A, 0x00AAFF, 0x336699, 0xFF0000, 0x808080] {
            let (h, s, l) = to_hsl(hex);
            let back = from_hsl(h, s, l);
            for shift in [16u32, 8, 0] {
                let d = ((hex >> shift) & 0xFF) as i32 - ((back >> shift) & 0xFF) as i32;
                assert!(d.abs() <= 1, "{hex:06X} -> {back:06X}");
            }
        }
    }

    #[test]
    fn bad_accent_strings_leave_the_theme_alone() {
        let t = Theme::dark();
        assert_eq!(t.with_accent_str("blue"), t);
        assert_eq!(t.with_accent_str("#7FCB8F"), t.with_accent(0x7FCB8F));
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
    fn accent_strong_tokens() {
        for t in all() {
            let c = t.colors;
            assert_eq!(hex_of(c.accent), 0x00AAFF, "{:?} accent unchanged", t.mode);
            assert_eq!(hex_of(c.accent_strong), 0x005EEB, "{:?}", t.mode);
            assert_eq!(hex_of(c.accent_strong_hover), 0x1A72FF, "{:?}", t.mode);
            assert_eq!(hex_of(c.accent_strong_press), 0x0052CC, "{:?}", t.mode);
            assert_eq!(hex_of(c.accent_strong_gradient_top), 0x1A72FF, "{:?}", t.mode);
            assert_eq!(hex_of(c.on_accent_strong), 0xFFFFFF, "{:?}", t.mode);
        }
    }

    #[test]
    fn on_accent_strong_contrast() {
        // White label on every state of the action button (gradient top, rest, hover, press).
        for t in all() {
            let c = t.colors;
            let fg = hex_of(c.on_accent_strong);
            for (name, bg) in [
                ("rest", c.accent_strong),
                ("hover", c.accent_strong_hover),
                ("press", c.accent_strong_press),
                ("gradient_top", c.accent_strong_gradient_top),
            ] {
                let r = contrast(fg, hex_of(bg));
                let floor = if name == "rest" || name == "press" { 4.5 } else { 3.9 };
                assert!(r >= floor, "{:?} on_accent_strong over {name}: {r:.2}", t.mode);
            }
            // The strong blue is a fill only: as text on a dark surface it must stay
            // below AA, which is why `accent` is the text/link color.
            if t.mode != Mode::Light {
                assert!(contrast(hex_of(c.accent_strong), hex_of(c.bg)) < 4.5, "{:?}", t.mode);
            }
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

    /// Alpha-composites `fg` (straight alpha) over an opaque `base`.
    fn over(fg: Rgba, base: u32) -> u32 {
        blend(hex_of(fg), base, f64::from(fg.a))
    }

    #[test]
    fn semantic_roles_read_as_text_on_every_surface() {
        for t in all() {
            let c = t.colors;
            for (name, role) in [("success", c.success), ("warning", c.warning), ("info", c.info)] {
                for (sname, surface) in
                    [("bg", c.bg), ("surface_0", c.surface_0), ("surface_1", c.surface_1), ("surface_2", c.surface_2)]
                {
                    let r = contrast(hex_of(role), hex_of(surface));
                    assert!(r >= 4.5, "{:?} {name} on {sname}: {r:.2}", t.mode);
                }
                // Also fine as an indicator (>= 3:1) on the highest tone in use.
                let r = contrast(hex_of(role), hex_of(c.surface_4));
                assert!(r >= 3.0, "{:?} {name} indicator on surface_4: {r:.2}", t.mode);
            }
            assert!(contrast(hex_of(c.error), hex_of(c.surface_2)) >= 4.5, "{:?} error on surface_2", t.mode);
        }
    }

    #[test]
    fn semantic_fills_carry_a_legible_ink() {
        for t in all() {
            let c = t.colors;
            for (name, fill, ink) in [
                ("success", c.success, c.on_success),
                ("warning", c.warning, c.on_warning),
                ("info", c.info, c.on_info),
            ] {
                let r = contrast(hex_of(fill), hex_of(ink));
                assert!(r >= 4.5, "{:?} on_{name}: {r:.2}", t.mode);
            }
        }
    }

    #[test]
    fn semantic_roles_are_distinct_from_each_other_and_from_error() {
        for t in all() {
            let c = t.colors;
            let all = [c.success, c.warning, c.info, c.error, c.accent];
            for i in 0..all.len() {
                for j in (i + 1)..all.len() {
                    assert_ne!(hex_of(all[i]), hex_of(all[j]), "{:?} {i} == {j}", t.mode);
                }
            }
        }
    }

    #[test]
    fn dark_roles_keep_the_literals_they_replace() {
        // ui_chat / ui_onboarding / unified_sidebar borrowed ansi[2] and ansi[3]: no visual change.
        let d = Theme::dark();
        assert_eq!(hex_of(d.colors.success), d.terminal.ansi[2]);
        assert_eq!(hex_of(d.colors.warning), d.terminal.ansi[3]);
        assert!((d.colors.scrim.a - 0.45).abs() < 1e-6, "thread_list scrim was black at 0.45");
        assert_eq!(hex_of(d.colors.scrim), 0x000000);
    }

    #[test]
    fn scrim_dims_the_background_in_every_mode() {
        for t in all() {
            let s = t.colors.scrim;
            assert!(s.a > 0.2 && s.a < 0.8, "{:?} alpha {}", t.mode, s.a);
            let dimmed = over(s, hex_of(t.colors.bg));
            assert!(luminance(dimmed) < luminance(hex_of(t.colors.bg)) || hex_of(t.colors.bg) == 0, "{:?}", t.mode);
            // What sits on top of the scrim (a surface_2 popover) stays distinguishable from it.
            // (OLED is black on black: the popover's own tone is what separates it there.)
            if t.mode != Mode::Oled {
                assert!(contrast(hex_of(t.colors.surface_2), dimmed) >= 1.2, "{:?} popover vs dimmed bg", t.mode);
            }
        }
    }

    #[test]
    fn glass_base_keeps_the_values_surface_rs_hardcoded() {
        assert_eq!(hex_of(Theme::dark().colors.glass), 0x2A2A2E);
        assert_eq!(hex_of(Theme::oled().colors.glass), 0x121214);
        assert_eq!(hex_of(Theme::light().colors.glass), 0xFFFFFF);
    }

    #[test]
    fn custom_accent_leaves_the_semantic_roles_alone() {
        for base in all() {
            let t = base.with_accent(0x7FCB8F);
            assert_eq!(t.colors.success, base.colors.success);
            assert_eq!(t.colors.warning, base.colors.warning);
            assert_eq!(t.colors.info, base.colors.info);
            assert_eq!(t.colors.scrim, base.colors.scrim);
        }
    }

    #[test]
    fn close_button_tokens_read_with_a_white_glyph() {
        assert_eq!(CLOSE_HOVER, 0xC42B1C);
        assert_eq!(CLOSE_PRESS, 0x9E2216);
        for bg in [CLOSE_HOVER, CLOSE_PRESS] {
            assert!(contrast(ON_CLOSE, bg) >= 4.5, "{bg:06X}");
        }
    }

    #[test]
    fn component_radii_follow_design_md() {
        assert_eq!((Radius::PILL, Radius::CONTROL, Radius::CHIP), (10.0, 8.0, 6.0));
        const { assert!(Radius::CHIP < Radius::CONTROL && Radius::CONTROL < Radius::PILL && Radius::PILL < Radius::SM) };
    }

    #[test]
    fn type_scale_is_strictly_ascending_and_keeps_old_steps() {
        let ty = Typography::new();
        let steps =
            [ty.ui_size_xs, ty.ui_size_sm, ty.ui_size_md, ty.ui_size_body, ty.ui_size_lg, ty.ui_size_xl, ty.ui_size_display];
        assert!(steps.windows(2).all(|w| w[0] < w[1]), "{steps:?}");
        assert_eq!((ty.ui_size_sm, ty.ui_size_md, ty.ui_size_lg), (12.0, 13.0, 15.0));
        assert_eq!(ty.ui_size_xs, 11.0);
    }
}
