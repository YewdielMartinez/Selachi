//! Color types and resolution of alacritty colors into concrete RGB values.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// An 8-bit-per-channel RGB color.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Rgb8 {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb8 {
    /// Creates a color from its channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

impl From<Rgb> for Rgb8 {
    fn from(c: Rgb) -> Self {
        Rgb8 { r: c.r, g: c.g, b: c.b }
    }
}

impl From<Rgb8> for Rgb {
    fn from(c: Rgb8) -> Self {
        Rgb { r: c.r, g: c.g, b: c.b }
    }
}

/// The color palette used to resolve terminal colors.
#[derive(Clone, Debug)]
pub struct Palette {
    /// The 16 ANSI colors (0-7 normal, 8-15 bright).
    pub ansi: [Rgb8; 16],
    /// Default foreground.
    pub foreground: Rgb8,
    /// Default background.
    pub background: Rgb8,
    /// Cursor color.
    pub cursor: Rgb8,
}

impl Default for Palette {
    /// A neutral dark palette (xterm-like), mainly useful for tests and fallbacks.
    fn default() -> Self {
        let c = Rgb8::new;
        Palette {
            ansi: [
                c(0x00, 0x00, 0x00),
                c(0xcd, 0x31, 0x31),
                c(0x0d, 0xbc, 0x79),
                c(0xe5, 0xe5, 0x10),
                c(0x24, 0x72, 0xc8),
                c(0xbc, 0x3f, 0xbc),
                c(0x11, 0xa8, 0xcd),
                c(0xe5, 0xe5, 0xe5),
                c(0x66, 0x66, 0x66),
                c(0xf1, 0x4c, 0x4c),
                c(0x23, 0xd1, 0x8b),
                c(0xf5, 0xf5, 0x43),
                c(0x3b, 0x8e, 0xea),
                c(0xd6, 0x70, 0xd6),
                c(0x29, 0xb8, 0xdb),
                c(0xff, 0xff, 0xff),
            ],
            foreground: c(0xe5, 0xe5, 0xe5),
            background: c(0x10, 0x10, 0x10),
            cursor: c(0xe5, 0xe5, 0xe5),
        }
    }
}

/// Levels of one channel of the 6x6x6 color cube (indices 16-231).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Resolves an xterm 256-color index using `palette` for the first 16 entries.
///
/// 0-15 come from the palette, 16-231 are the 6x6x6 cube, 232-255 the grayscale ramp.
pub fn indexed_color(palette: &Palette, idx: u8) -> Rgb8 {
    match idx {
        0..=15 => palette.ansi[idx as usize],
        16..=231 => {
            let i = (idx - 16) as usize;
            Rgb8::new(CUBE_LEVELS[i / 36], CUBE_LEVELS[(i / 6) % 6], CUBE_LEVELS[i % 6])
        },
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            Rgb8::new(v, v, v)
        },
    }
}

/// Dims a color (SGR 2 / "Dim*" named colors) by scaling it to roughly two thirds.
pub(crate) fn dim(c: Rgb8) -> Rgb8 {
    let f = |v: u8| ((v as u16 * 2) / 3) as u8;
    Rgb8::new(f(c.r), f(c.g), f(c.b))
}

fn named_color(palette: &Palette, n: NamedColor) -> Rgb8 {
    use NamedColor::*;
    match n {
        Black | Red | Green | Yellow | Blue | Magenta | Cyan | White | BrightBlack | BrightRed
        | BrightGreen | BrightYellow | BrightBlue | BrightMagenta | BrightCyan | BrightWhite => {
            palette.ansi[(n as usize).min(15)]
        },
        Foreground | BrightForeground => palette.foreground,
        DimForeground => dim(palette.foreground),
        Background => palette.background,
        Cursor => palette.cursor,
        DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan | DimWhite => {
            dim(palette.ansi[(n as usize - DimBlack as usize).min(7)])
        },
    }
}

/// Color for an alacritty color-table index as used by OSC 4/10/11/12 requests.
///
/// 0-255 indexed, 256 foreground, 257 background, 258 cursor.
pub(crate) fn color_by_table_index(palette: &Palette, idx: usize) -> Option<Rgb8> {
    match idx {
        0..=255 => Some(indexed_color(palette, idx as u8)),
        256 => Some(palette.foreground),
        257 => Some(palette.background),
        258 => Some(palette.cursor),
        _ => None,
    }
}

/// Resolves an alacritty [`Color`], honoring runtime overrides (OSC 4/10/11).
pub(crate) fn resolve_color(color: Color, palette: &Palette, overrides: &Colors) -> Rgb8 {
    match color {
        Color::Spec(rgb) => rgb.into(),
        Color::Indexed(i) => match overrides[i as usize] {
            Some(rgb) => rgb.into(),
            None => indexed_color(palette, i),
        },
        Color::Named(n) => match overrides[n as usize] {
            Some(rgb) => rgb.into(),
            None => named_color(palette, n),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_ranges() {
        let p = Palette::default();
        assert_eq!(indexed_color(&p, 0), p.ansi[0]);
        assert_eq!(indexed_color(&p, 15), p.ansi[15]);
        assert_eq!(indexed_color(&p, 16), Rgb8::new(0, 0, 0));
        assert_eq!(indexed_color(&p, 21), Rgb8::new(0, 0, 255));
        assert_eq!(indexed_color(&p, 196), Rgb8::new(255, 0, 0));
        assert_eq!(indexed_color(&p, 231), Rgb8::new(255, 255, 255));
        assert_eq!(indexed_color(&p, 232), Rgb8::new(8, 8, 8));
        assert_eq!(indexed_color(&p, 255), Rgb8::new(238, 238, 238));
    }

    #[test]
    fn named_and_overrides() {
        let p = Palette::default();
        let mut colors = Colors::default();
        assert_eq!(resolve_color(Color::Named(NamedColor::Foreground), &p, &colors), p.foreground);
        assert_eq!(resolve_color(Color::Named(NamedColor::BrightRed), &p, &colors), p.ansi[9]);
        assert_eq!(resolve_color(Color::Named(NamedColor::DimRed), &p, &colors), dim(p.ansi[1]));
        colors[NamedColor::Background as usize] = Some(Rgb { r: 1, g: 2, b: 3 });
        colors[4usize] = Some(Rgb { r: 9, g: 9, b: 9 });
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Background), &p, &colors),
            Rgb8::new(1, 2, 3)
        );
        assert_eq!(resolve_color(Color::Indexed(4), &p, &colors), Rgb8::new(9, 9, 9));
        assert_eq!(
            resolve_color(Color::Spec(Rgb { r: 7, g: 8, b: 9 }), &p, &colors),
            Rgb8::new(7, 8, 9)
        );
    }
}
