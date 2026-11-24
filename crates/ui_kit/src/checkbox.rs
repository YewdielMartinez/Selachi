//! `Checkbox` and `RadioGroup`: stateless like the other controls (the caller owns the value and
//! re-renders from `on_change`). Both are tab stops; `Space` / `Enter` activate the focused one.
//! No outlines: the box is a `surface_3` tone, checked = `accent_strong` (an active state).

use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px,
};
use theme::{Radius, Theme};

use crate::focus;
use crate::icon::{Icon, icon};

type BoolFn = Box<dyn Fn(bool, &mut Window, &mut App) + 'static>;
type IndexFn = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// A labelled check box. The whole row (box + label) is the hit area and the focus target.
///
/// ```ignore
/// Checkbox::new("telemetry", &theme).label("Enviar diagnósticos").checked(true)
///     .on_change(|checked, _, cx| { /* store */ })
/// ```
#[derive(IntoElement)]
pub struct Checkbox {
    id: SharedString,
    theme: Theme,
    label: Option<SharedString>,
    checked: bool,
    disabled: bool,
    focusable: bool,
    on_change: Option<BoolFn>,
}

impl Checkbox {
    pub fn new(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            label: None,
            checked: false,
            disabled: false,
            focusable: true,
            on_change: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Dimmed, inert and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Keyboard focus (tab stop + ring); on by default.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    /// Called with the *new* value when toggled.
    pub fn on_change(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let checked = self.checked;
        let id = ElementId::from(self.id.clone());
        let interactive = !self.disabled && self.on_change.is_some();
        let focus = (interactive && self.focusable).then(|| focus::handle(window, cx, &id));
        let hover = c.surface_2;
        let mark = div()
            .size(px(20.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(Radius::CHIP))
            .bg(if checked {
                c.accent_strong
            } else {
                c.surface_3
            })
            .when(checked, |d| {
                d.child(icon(Icon::Check, 16.0, c.on_accent_strong))
            });
        let mut el = div()
            .id(id.clone())
            .occlude()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .p(px(2.0))
            .rounded(px(Radius::CONTROL))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .text_color(c.text)
            .when(self.disabled, |d| d.opacity(0.4))
            .when(interactive, |d| {
                d.cursor_pointer().hover(move |s| s.bg(hover))
            })
            .child(mark)
            .when_some(self.label, |d, l| d.child(div().min_w_0().child(l)));
        if let (true, Some(f)) = (interactive, self.on_change) {
            el = el.on_click(move |_: &ClickEvent, w, cx| f(!checked, w, cx));
        }
        // Size to the box + label even inside a stretching column (the ring hugs the row).
        el.style().align_self = Some(gpui::AlignSelf::FlexStart);
        focus::wire(el, focus, &id, Radius::CONTROL, c.accent)
    }
}

/// A group of mutually exclusive options drawn as round radios.
///
/// ```ignore
/// RadioGroup::new("density", &theme, ["Compacta", "Cómoda"]).selected(1)
///     .on_change(|ix, _, cx| { /* store ix */ })
/// ```
#[derive(IntoElement)]
pub struct RadioGroup {
    id: SharedString,
    theme: Theme,
    options: Vec<SharedString>,
    selected: Option<usize>,
    horizontal: bool,
    disabled: bool,
    focusable: bool,
    on_change: Option<IndexFn>,
}

impl RadioGroup {
    pub fn new<S: Into<SharedString>>(
        id: impl Into<SharedString>,
        theme: &Theme,
        options: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            options: options.into_iter().map(Into::into).collect(),
            selected: None,
            horizontal: false,
            disabled: false,
            focusable: true,
            on_change: None,
        }
    }

    pub fn selected(mut self, ix: Option<usize>) -> Self {
        self.selected = ix;
        self
    }

    /// Lay the options out in a row (default: a column).
    pub fn horizontal(mut self, horizontal: bool) -> Self {
        self.horizontal = horizontal;
        self
    }

    /// Dimmed, inert and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Keyboard focus: every radio is a tab stop; on by default.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    pub fn on_change(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for RadioGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let interactive = !self.disabled && self.on_change.is_some();
        let focusable = interactive && self.focusable;
        let (id, selected, on_change) = (self.id, self.selected, self.on_change);
        let items: Vec<_> = self
            .options
            .into_iter()
            .enumerate()
            .map(|(ix, label)| {
                let on = selected == Some(ix);
                let on_change = on_change.clone();
                let row_id = ElementId::NamedInteger(id.clone(), ix as u64);
                let focus = focusable.then(|| focus::handle(window, cx, &row_id));
                let hover = c.surface_2;
                let dot = div()
                    .size(px(20.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(if on { c.accent_strong } else { c.surface_3 })
                    .when(on, |d| {
                        d.child(div().size(px(8.0)).rounded_full().bg(c.on_accent_strong))
                    });
                let el = div()
                    .id(row_id.clone())
                    .occlude()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .p(px(2.0))
                    .rounded(px(Radius::CONTROL))
                    .font_family(ty.ui_family)
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.text)
                    .when(self.disabled, |d| d.opacity(0.4))
                    .when(interactive, |d| {
                        d.cursor_pointer().hover(move |s| s.bg(hover))
                    })
                    .child(dot)
                    .child(div().min_w_0().child(label))
                    .when(interactive, |d| {
                        d.on_click(move |_: &ClickEvent, w, cx| {
                            if let Some(f) = &on_change {
                                f(ix, w, cx);
                            }
                        })
                    });
                focus::wire(el, focus, &row_id, Radius::CONTROL, c.accent)
            })
            .collect();
        div()
            .flex()
            .when(self.horizontal, |d| {
                d.flex_row().items_center().gap(px(16.0))
            })
            .when(!self.horizontal, |d| d.flex_col().gap(px(4.0)))
            .children(items)
    }
}
