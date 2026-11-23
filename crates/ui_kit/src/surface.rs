//! Surfaces: `SectionLabel` (micro label) and `Card` (tonal rounded container).

use gpui::{
    AnyElement, App, Div, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    StyleRefinement, Window, div, px,
};
use theme::{Radius, Rgba, Theme};

/// 11 px muted micro label that titles a group of controls ("Apariencia", "Working").
/// Optionally uppercased (DESIGN.md allows forced caps only here).
///
/// ```ignore
/// SectionLabel::new(&theme, "Tema").uppercase(true)
/// ```
#[derive(IntoElement)]
pub struct SectionLabel {
    theme: Theme,
    text: SharedString,
    uppercase: bool,
}

impl SectionLabel {
    pub fn new(theme: &Theme, text: impl Into<SharedString>) -> Self {
        Self { theme: *theme, text: text.into(), uppercase: false }
    }

    pub fn uppercase(mut self, uppercase: bool) -> Self {
        self.uppercase = uppercase;
        self
    }
}

impl RenderOnce for SectionLabel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let ty = self.theme.typography;
        let text: SharedString =
            if self.uppercase { self.text.to_uppercase().into() } else { self.text };
        div()
            .font_family(ty.ui_family)
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(self.theme.colors.muted)
            .child(text)
    }
}

/// Which surface tone a [`Card`] paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CardTone {
    /// `surface_0`: the floating work card (terminal).
    Base,
    /// `surface_1` (default): grouped settings, list panels.
    #[default]
    Raised,
    /// `surface_2`: a card sitting on a `surface_1` panel.
    High,
}

impl CardTone {
    pub fn color(self, theme: &Theme) -> Rgba {
        match self {
            CardTone::Base => theme.colors.surface_0,
            CardTone::Raised => theme.colors.surface_1,
            CardTone::High => theme.colors.surface_2,
        }
    }
}

/// Tonal container with rounded corners and no border. It is `Styled`, so size, padding and
/// flex layout are set with the usual gpui methods.
///
/// ```ignore
/// Card::new(&theme).tone(CardTone::Raised).p_4().child("content")
/// ```
#[derive(IntoElement)]
pub struct Card {
    base: Div,
}

impl Card {
    /// `surface_1` background, radius `MD` (16).
    pub fn new(theme: &Theme) -> Self {
        Self { base: div().bg(CardTone::Raised.color(theme)).rounded(px(Radius::MD)) }
    }

    pub fn tone(mut self, tone: CardTone, theme: &Theme) -> Self {
        self.base = self.base.bg(tone.color(theme));
        self
    }

    /// Corner radius in px (default 16).
    pub fn radius(mut self, radius: f32) -> Self {
        self.base = self.base.rounded(px(radius));
        self
    }
}

impl Styled for Card {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.base
    }
}

// ---------------------------------------------------------------------------------------------
// Glass (frosted sidebar / right panel / drawers)
// ---------------------------------------------------------------------------------------------

/// Tint painted over the blurred backdrop of a glass zone. The base is the theme's `glass` token (dark `#2A2A2E`, OLED
/// `#121214`, light white). `translucency` is `0..=100` (percent); the alpha is
/// `1 - translucency * 0.6`, so 35 % reads clearly see-through but calm and 0 % is opaque.
pub fn glass_tint(theme: &Theme, translucency: u8) -> gpui::Hsla {
    let t = f32::from(translucency.min(100)) / 100.0;
    let alpha = 1.0 - t * 0.6;
    let base: gpui::Hsla = theme.colors.glass.into();
    base.opacity(alpha)
}

/// Hover pill over glass: white at 8 % (black at 6 % in light mode, where white would vanish).
pub fn glass_hover(theme: &Theme) -> gpui::Hsla {
    glass_pill(theme, 0.08, 0.06)
}

/// Selected pill over glass: white at 12 % (black at 9 % in light mode).
pub fn glass_selected(theme: &Theme) -> gpui::Hsla {
    glass_pill(theme, 0.12, 0.09)
}

fn glass_pill(theme: &Theme, dark: f32, light: f32) -> gpui::Hsla {
    match theme.mode {
        theme::Mode::Light => gpui::Hsla { h: 0.0, s: 0.0, l: 0.0, a: light },
        _ => gpui::Hsla { h: 0.0, s: 0.0, l: 1.0, a: dark },
    }
}
