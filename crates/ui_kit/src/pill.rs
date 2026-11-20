//! `Pill`: the tab / chip primitive. Active = grey pill, never a colored line (DESIGN.md rule 4).

use gpui::{
    AnyElement, App, ClickEvent, IntoElement, ParentElement, RenderOnce, SharedString, Window,
    div, prelude::*, px,
};
use theme::Theme;

use crate::icon::{Icon, icon};
use crate::tooltip::Tooltip;

/// Which pill metrics to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PillSize {
    /// Titlebar tab: 28 px high, radius 10, padding 12, text 13. Inactive has no background.
    #[default]
    Tab,
    /// Status-bar chip: 20 px high, radius 8, padding 8, text 12, always on `surface_1`.
    Compact,
}

/// A rounded pill with an optional icon, a label and trailing children (a meter, a dot).
///
/// ```ignore
/// Pill::new("tab-0", &theme)
///     .icon(Icon::Terminal)
///     .label("crates")
///     .active(true)
///     .on_click(|_, _, _| {})
/// ```
#[derive(IntoElement)]
pub struct Pill {
    id: SharedString,
    theme: Theme,
    size: PillSize,
    icon: Option<Icon>,
    label: Option<SharedString>,
    active: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    children: Vec<AnyElement>,
}

impl Pill {
    pub fn new(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            size: PillSize::Tab,
            icon: None,
            label: None,
            active: false,
            tooltip: None,
            on_click: None,
            children: Vec::new(),
        }
    }

    /// Compact status-bar chip.
    pub fn compact(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self::new(id, theme).size(PillSize::Compact)
    }

    pub fn size(mut self, size: PillSize) -> Self {
        self.size = size;
        self
    }

    /// Leading 16 px pixel icon.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// `surface_3` background and `text` foreground.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Hover hint (multi-line allowed).
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Makes the pill clickable (pointer cursor and hover tone).
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl ParentElement for Pill {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Pill {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let clickable = self.on_click.is_some();
        let (h, radius, px_x, gap, text_size) = match self.size {
            PillSize::Tab => (28.0, 10.0, 12.0, 8.0, ty.ui_size_md),
            PillSize::Compact => (20.0, 8.0, 8.0, 6.0, ty.ui_size_sm),
        };
        let fg = if self.active { c.text } else { c.muted };
        let rest_bg = match (self.size, self.active) {
            (_, true) => Some(c.surface_3),
            (PillSize::Compact, false) => Some(c.surface_1),
            (PillSize::Tab, false) => None,
        };
        let hover_bg = c.surface_2;

        let mut el = div()
            .id(self.id)
            // Pills often live in the drag area of the titlebar: they must not be part of it.
            .occlude()
            .h(px(h))
            .flex_none()
            .px(px(px_x))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(gap))
            .rounded(px(radius))
            .font_family(ty.ui_family)
            .text_size(px(text_size))
            .text_color(fg)
            .when_some(rest_bg, |d, bg| d.bg(bg))
            .when(clickable && !self.active, |d| d.hover(move |s| s.bg(hover_bg)).cursor_pointer())
            .when(clickable && self.active, |d| d.cursor_pointer())
            .when_some(self.icon, |d, i| d.child(icon(i, 16.0, fg)))
            .when_some(self.label, |d, l| d.child(l))
            .children(self.children);
        if let Some(f) = self.on_click {
            el = el.on_click(move |e, w, cx| f(e, w, cx));
        }
        if let Some(t) = self.tooltip {
            el = el.tooltip(Tooltip::text(&self.theme, t));
        }
        el
    }
}
