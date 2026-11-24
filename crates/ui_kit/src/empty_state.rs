//! `EmptyState`: the muted, centered "nothing here" block (icon, title, body, optional action).
//! It is the shared form of the `ui_git` "Sin cambios" / "Esto no es un repositorio" message.

use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Window, div, prelude::*,
    px,
};
use theme::Theme;

use crate::icon::{Icon, icon};

/// A centered message for an empty list or panel. Only the title is required.
///
/// ```ignore
/// EmptyState::new(&theme, "Sin cambios")
///     .icon(Icon::GitBranch)
///     .body("El arbol de trabajo esta limpio.")
///     .action(Button::new("refresh", &theme, "Actualizar", ButtonKind::Tonal).on_click(|_, _, _| {}))
/// ```
#[derive(IntoElement)]
pub struct EmptyState {
    theme: Theme,
    icon: Option<Icon>,
    icon_size: f32,
    title: SharedString,
    body: Option<SharedString>,
    action: Option<AnyElement>,
    padding_y: f32,
    fill: bool,
}

impl EmptyState {
    pub fn new(theme: &Theme, title: impl Into<SharedString>) -> Self {
        Self {
            theme: *theme,
            icon: None,
            icon_size: 32.0,
            title: title.into(),
            body: None,
            action: None,
            padding_y: 20.0,
            fill: false,
        }
    }

    /// A pixel icon above the title, in `muted` (default 32 px, twice its 16 px grid).
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    /// The explanation under the title (`muted`, small, wraps).
    pub fn body(mut self, body: impl Into<SharedString>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// The one thing the user can do about it (usually a `Button`).
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// Vertical padding in px (default 20).
    pub fn padding_y(mut self, px: f32) -> Self {
        self.padding_y = px;
        self
    }

    /// Fills the available height and centers vertically (a whole empty panel) instead of
    /// hugging its content (an empty section inside a list).
    pub fn fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .w_full()
            .when(self.fill, |d| d.h_full().flex_1().justify_center())
            .when(!self.fill, |d| d.flex_none())
            .py(px(self.padding_y))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.0))
            .font_family(ty.ui_family)
            .when_some(self.icon, |d, i| {
                d.child(div().pb(px(8.0)).child(icon(i, self.icon_size, c.muted)))
            })
            .child(
                div()
                    .px(px(12.0))
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.text)
                    .text_center()
                    .child(self.title),
            )
            .when_some(self.body, |d, body| {
                d.child(
                    div()
                        .px(px(12.0))
                        .max_w(px(320.0))
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .text_center()
                        .child(body),
                )
            })
            .when_some(self.action, |d, action| {
                d.child(div().pt(px(12.0)).flex().justify_center().child(action))
            })
    }
}
