//! Buttons: `IconButton` (rail, titlebar), `GhostButton` (quiet text action) and `Button`
//! (primary / tonal / ghost, like the white pills in the references).

use gpui::{
    App, ClickEvent, Div, IntoElement, RenderOnce, SharedString, Stateful, Window, div,
    prelude::*, px,
};
use theme::Theme;

use crate::icon::{Icon, icon};
use crate::tooltip::Tooltip;

pub(crate) type ClickFn = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Square icon button. Exactly one in a group is usually `active` (the current view):
/// `surface_3` pill with the icon in `text`. Hover is a lighter pill (`surface_2`) that keeps
/// the muted icon, so a hovered button can never be mistaken for the active one.
///
/// ```ignore
/// IconButton::new("rail-git", &theme, Icon::GitBranch)
///     .active(false)
///     .tooltip("Git")
///     .on_click(|_, _, _| {})
/// ```
#[derive(IntoElement)]
pub struct IconButton {
    id: SharedString,
    theme: Theme,
    icon: Icon,
    size: f32,
    icon_size: f32,
    active: bool,
    emphasize_on_hover: bool,
    glass: bool,
    passthrough: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickFn>,
}

impl IconButton {
    pub fn new(id: impl Into<SharedString>, theme: &Theme, icon: Icon) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            icon,
            size: 36.0,
            icon_size: 16.0,
            active: false,
            emphasize_on_hover: false,
            glass: false,
            passthrough: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Do not occlude the hitboxes behind: a hovered ancestor stays hovered while the pointer is
    /// over this button (use it for affordances that live inside a hover-driven row).
    pub fn passthrough(mut self, on: bool) -> Self {
        self.passthrough = on;
        self
    }

    /// The button sits on a glass surface: its pills are translucent white (8 % hover, 12 %
    /// active) instead of opaque surfaces.
    pub fn glass(mut self, glass: bool) -> Self {
        self.glass = glass;
        self
    }

    /// Edge of the square hit area (default 36, radius 10).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Icon edge (default 16).
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Ghost-style variant used by "+": the icon brightens to `text` on hover and a press
    /// uses `surface_3`.
    pub fn emphasize_on_hover(mut self, on: bool) -> Self {
        self.emphasize_on_hover = on;
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

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let fg = if self.active { c.text } else { c.muted };
        let group = self.id.clone();
        let group_name = self.id.clone();
        let emphasize = self.emphasize_on_hover;
        let text = c.text;
        let glyph = icon(self.icon, self.icon_size, fg);
        let glyph = if emphasize && !self.active {
            glyph.group_hover(group_name, move |s| s.text_color(text))
        } else {
            glyph
        };
        let theme = self.theme;
        let (bg_active, bg_hover, bg_press): (gpui::Hsla, gpui::Hsla, gpui::Hsla) = if self.glass {
            (crate::glass_selected(&theme), crate::glass_hover(&theme), crate::glass_selected(&theme))
        } else {
            (c.surface_3.into(), c.surface_2.into(), c.surface_3.into())
        };
        let mut el = div()
            .id(self.id)
            .group(group)
            .when(!self.passthrough, |d| d.occlude())
            .size(px(self.size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.0))
            .cursor_pointer()
            .when(self.active, |d| d.bg(bg_active))
            .when(!self.active && !emphasize, |d| {
                d.hover(move |s| s.bg(bg_hover)).active(move |s| s.bg(bg_hover))
            })
            .when(!self.active && emphasize, |d| {
                d.hover(move |s| s.bg(bg_hover)).active(move |s| s.bg(bg_press))
            })
            .child(glyph);
        if let Some(f) = self.on_click {
            el = el.on_click(move |e, w, cx| f(e, w, cx));
        }
        if let Some(t) = self.tooltip {
            el = el.tooltip(Tooltip::text(&self.theme, t));
        }
        el
    }
}

/// Quiet text action: no background until hover, 28 px high, radius 10.
///
/// ```ignore
/// GhostButton::new("clear", &theme, "Limpiar").icon(Icon::Close).on_click(|_, _, _| {})
/// ```
#[derive(IntoElement)]
pub struct GhostButton {
    inner: Button,
}

impl GhostButton {
    pub fn new(id: impl Into<SharedString>, theme: &Theme, label: impl Into<SharedString>) -> Self {
        let mut inner = Button::new(id, theme, label, ButtonKind::Ghost);
        inner.height = 28.0;
        inner.radius = Some(10.0);
        Self { inner }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.inner.icon = Some(icon);
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.inner.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.inner.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for GhostButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.inner.render(window, cx)
    }
}

/// Visual weight of a [`Button`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one main action: `text` colored pill with `bg` colored label (white in dark, ink in
    /// light), like the references.
    Primary,
    /// Secondary action: `surface_3` pill, `text` label.
    Tonal,
    /// Tertiary action: no fill, `muted` label, `surface_2` on hover.
    Ghost,
}

/// Pill-shaped button.
///
/// ```ignore
/// Button::primary("save", &theme, "Guardar").on_click(|_, _, _| {})
/// Button::tonal("cancel", &theme, "Cancelar")
/// ```
#[derive(IntoElement)]
pub struct Button {
    id: SharedString,
    theme: Theme,
    label: SharedString,
    kind: ButtonKind,
    icon: Option<Icon>,
    height: f32,
    radius: Option<f32>,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickFn>,
}

impl Button {
    pub fn new(
        id: impl Into<SharedString>,
        theme: &Theme,
        label: impl Into<SharedString>,
        kind: ButtonKind,
    ) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            label: label.into(),
            kind,
            icon: None,
            height: 32.0,
            radius: None,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn primary(id: impl Into<SharedString>, theme: &Theme, label: impl Into<SharedString>) -> Self {
        Self::new(id, theme, label, ButtonKind::Primary)
    }

    pub fn tonal(id: impl Into<SharedString>, theme: &Theme, label: impl Into<SharedString>) -> Self {
        Self::new(id, theme, label, ButtonKind::Tonal)
    }

    pub fn ghost(id: impl Into<SharedString>, theme: &Theme, label: impl Into<SharedString>) -> Self {
        Self::new(id, theme, label, ButtonKind::Ghost)
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Dimmed and inert.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
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

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (bg, fg) = match self.kind {
            ButtonKind::Primary => (Some(c.text), c.bg),
            ButtonKind::Tonal => (Some(c.surface_3), c.text),
            ButtonKind::Ghost => (None, c.muted),
        };
        let kind = self.kind;
        let enabled = !self.disabled;
        let base: Stateful<Div> = div().id(self.id);
        let mut el = base
            .occlude()
            .h(px(self.height))
            .flex_none()
            .px(px(14.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .when(self.radius.is_none(), |d| d.rounded_full())
            .when_some(self.radius, |d, r| d.rounded(px(r)))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(fg)
            .when_some(bg, |d, bg| d.bg(bg))
            .when(self.disabled, |d| d.opacity(0.4))
            .when(enabled, |d| {
                d.cursor_pointer().hover(move |s| match kind {
                    ButtonKind::Primary => s.opacity(0.88),
                    ButtonKind::Tonal => s.bg(c.surface_4),
                    ButtonKind::Ghost => s.bg(c.surface_2).text_color(c.text),
                })
            })
            .when_some(self.icon, |d, i| d.child(icon(i, 16.0, fg)))
            .child(self.label);
        if enabled && let Some(f) = self.on_click {
            el = el.on_click(move |e, w, cx| f(e, w, cx));
        }
        if let Some(t) = self.tooltip {
            el = el.tooltip(Tooltip::text(&self.theme, t));
        }
        el
    }
}
