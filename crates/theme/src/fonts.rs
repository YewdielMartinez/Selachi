//! Fonts embedded into the binary (see `assets/fonts/README.md`).

use std::borrow::Cow;

pub const UI_FAMILY: &str = "DM Sans";
pub const MONO_FAMILY: &str = "Geist Mono";

/// Raw TTF data for every embedded face, ready for `TextSystem::add_fonts`.
pub fn embedded() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/DMSans-Regular.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/DMSans-Medium.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/DMSans-SemiBold.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/GeistMono-Regular.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/GeistMono-Medium.ttf")[..]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_are_truetype() {
        let all = embedded();
        assert_eq!(all.len(), 5);
        for f in all {
            assert!(f.len() > 10_000);
            assert_eq!(&f[..4], &[0x00, 0x01, 0x00, 0x00]);
        }
    }
}
