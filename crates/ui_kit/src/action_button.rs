//! `ActionButton`: the vivid call-to-action pill ("+ Nuevo chat", "Commit", "Instalar").
//!
//! Spec: docs/DESIGN.md, "Acento fuerte". Full-radius pill, 36 (`Md`) or 40 (`Lg`) px high,
//! padding 0 18, `Plus` icon 16 px + DM Sans 14 semibold label in `on_accent_strong`, a very
//! subtle vertical gradient (`accent_strong_gradient_top` -> `accent_strong`) and a soft blue
//! shadow `0 6px 16px rgba(0,94,235,0.28)`. Hover = flat `accent_strong_hover`; press =
//! `accent_strong_press`, no shadow and the content nudged 1 px down.
//!
//! gpui has no transform, so the 0.97 press scale is approximated by that sunken look (the
//! shadow vanishes and the label drops 1 px) instead of a real scale.
//!
//! At most one per view; the secondary actions are `Button::tonal` / `ghost`.

use gpui::{
    App, BoxShadow, ClickEvent, Div, ElementId, FontWeight, IntoElement, RenderOnce, SharedString, Stateful,
    Window, div, linear_color_stop, linear_gradient, point, prelude::*, px,
};
use theme::Theme;

use crate::button::{Button, ClickFn};
use crate::focus;
use crate::icon::{Icon, icon};
use crate::tooltip::Tooltip;

/// Height of an [`ActionButton`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActionSize {
    /// 36 px.
    #[default]
    Md,
    /// 40 px.
    Lg,
}

impl ActionSize {
    pub const fn height(self) -> f32 {
        match self {
            ActionSize::Md => 36.0,
            ActionSize::Lg => 40.0,
        }
    }
}

/// The vivid action pill. Build it with [`Button::action`] or [`ActionButton::new`].
///
/// ```ignore
/// Button::action("new-chat", &theme, "Nuevo chat").on_click(|_, _, _| {})
/// Button::action("commit", &theme, "Commit").size(ActionSize::Lg).no_icon()
/// ```
#[derive(IntoElement)]
pub struct ActionButton {
    id: SharedString,
    theme: Theme,
    label: SharedString,
    icon: Option<Icon>,
    size: ActionSize,
    disabled: bool,
    focusable: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickFn>,
}

impl ActionButton {
    /// A `Md` action button with the default `Plus` icon.
    pub fn new(id: impl Into<SharedString>, theme: &Theme, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            label: label.into(),
            icon: Some(Icon::Plus),
            size: ActionSize::Md,
            disabled: false,
            focusable: true,
            tooltip: None,
            on_click: None,
        }
    }

    /// Replaces the leading icon (default [`Icon::Plus`]).
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Label only, no leading icon.
    pub fn no_icon(mut self) -> Self {
        self.icon = None;
        self
    }

    pub fn size(mut self, size: ActionSize) -> Self {
        self.size = size;
        self
    }

    /// `surface_3` fill, `muted` label, no shadow, inert and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Keyboard focus (tab stop + ring); on by default.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl Button {
    /// The vivid call-to-action pill (`accent_strong`). One per view at most.
    pub fn action(
        id: impl Into<SharedString>,
        theme: &Theme,
        label: impl Into<SharedString>,
    ) -> ActionButton {
        ActionButton::new(id, theme, label)
    }
}

/// `0 6px 16px rgba(0, 94, 235, 0.28)`, derived from the `accent_strong` token.
fn action_shadow(strong: gpui::Rgba) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: gpui::Rgba { a: 0.28, ..strong }.into(),
        offset: point(px(0.0), px(6.0)),
        blur_radius: px(16.0),
        spread_radius: px(0.0),
    }]
}

impl RenderOnce for ActionButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let enabled = !self.disabled;
        let id = ElementId::from(self.id.clone());
        let focus = (enabled && self.focusable && self.on_click.is_some()).then(|| focus::handle(window, cx, &id));
        let ring_radius = self.size.height() / 2.0;
        let fg = if enabled { c.on_accent_strong } else { c.muted };
        let (hover, press) = (c.accent_strong_hover, c.accent_strong_press);

        let base: Stateful<Div> = div().id(id.clone());
        let mut el = base
            .occlude()
            .h(px(self.size.height()))
            .flex_none()
            .px(px(18.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .rounded_full()
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_body))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(fg)
            .when(enabled, |d| {
                d.bg(linear_gradient(
                    180.0,
                    linear_color_stop(c.accent_strong_gradient_top, 0.0),
                    linear_color_stop(c.accent_strong, 1.0),
                ))
                .shadow(action_shadow(c.accent_strong))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .active(move |s| s.bg(press).shadow_none().pt(px(2.0)))
            })
            .when(!enabled, |d| d.bg(c.surface_3))
            .when_some(self.icon, |d, i| d.child(icon(i, 16.0, fg)))
            .child(self.label);
        if enabled && let Some(f) = self.on_click {
            el = el.on_click(move |e, w, cx| f(e, w, cx));
        }
        if let Some(t) = self.tooltip {
            el = el.tooltip(Tooltip::text(&self.theme, t));
        }
        focus::wire(el, focus, &id, ring_radius, c.accent)
    }
}
