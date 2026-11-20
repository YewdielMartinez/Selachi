//! Hover hint: the only floating surface besides popovers (`surface_3`, 1 px `outline_soft`).

use gpui::{
    AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px,
};
use theme::Theme;

/// A small hint bubble. Multi-line text is supported (`\n`), which is how the usage pill lists
/// its breakdown.
///
/// Use [`Tooltip::text`] as the argument of gpui's `.tooltip(..)`:
///
/// ```ignore
/// div().id("x").tooltip(Tooltip::text(&theme, "Rama main · limpia"))
/// ```
pub struct Tooltip {
    text: SharedString,
    theme: Theme,
}

impl Tooltip {
    /// Builder closure for `.tooltip(..)`.
    pub fn text(
        theme: &Theme,
        text: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text: SharedString = text.into();
        let theme = *theme;
        move |_window, cx| cx.new(|_| Tooltip { text: text.clone(), theme }).into()
    }

    /// The bubble as a plain element, for galleries and docs (not hover-driven).
    pub fn bubble(theme: &Theme, text: impl Into<SharedString>) -> impl IntoElement {
        Tooltip { text: text.into(), theme: *theme }.element()
    }

    fn element(&self) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(c.surface_3)
            .border_1()
            .border_color(c.outline_soft)
            .text_color(c.text)
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_sm))
            .child(self.text.clone())
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.element()
    }
}
