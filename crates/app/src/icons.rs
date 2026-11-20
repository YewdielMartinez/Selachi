//! Pixel-grid icon helper. Icons are white-on-transparent svgs tinted via `text_color`.

use gpui::{Hsla, Svg, prelude::*, px, svg};

/// An embedded `assets/icons/<name>.svg` drawn at `size` logical px in `color`.
pub fn icon(name: &str, size: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .flex_none()
        .size(px(size))
        .text_color(color)
}
