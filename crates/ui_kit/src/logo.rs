//! The shark logo, one PNG per theme mode (DESIGN.md "Logo por modo").

use gpui::{App, ImageSource, IntoElement, RenderOnce, Resource, Window, img, prelude::*, px};
use theme::Mode;

pub(crate) fn logo_asset(mode: Mode) -> &'static str {
    match mode {
        Mode::Light => "logo-light-mode.png",
        Mode::Dark | Mode::Oled => "logo-dark-mode.png",
    }
}

/// The multi-tone shark logo. Light art on dark/OLED, dark art on light: the file changes,
/// there is never a chip behind it.
///
/// ```ignore
/// Logo::new(theme.mode).size(40.0)
/// ```
#[derive(IntoElement)]
pub struct Logo {
    mode: Mode,
    size: f32,
}

impl Logo {
    pub fn new(mode: Mode) -> Self {
        Self { mode, size: 40.0 }
    }

    /// Square edge in logical px (default 40).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Logo {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        img(ImageSource::Resource(Resource::Embedded(logo_asset(self.mode).into())))
            .w(px(self.size))
            .h(px(self.size))
            .flex_none()
    }
}
