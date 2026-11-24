//! `FocusCard` and `RadioRow`: custom-content clickable blocks that are real keyboard citizens.
//! They replace the raw `.tab_index(0).focus(|s| ...)` rows of the panels: same tab stop, but
//! with the standard accent focus ring (keyboard focus only), `Enter` / `Space` running the click
//! and no focus steal on a mouse press.
//!
//! * [`FocusCard`]: one focusable, clickable card (a recent project, a preset, an agent to pick).
//! * [`RadioRow`]: a card that is one option of a mutually exclusive group. The group has a single
//!   tab stop (the selected row, or the first while nothing is selected); `Up` / `Left` and
//!   `Down` / `Right` move the focus between the rows (wrapping), `Space` / `Enter` select.
//!   The key context carries the role (`radio`) and its state (`aria-checked`).

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FocusHandle, IntoElement, KeyContext,
    ParentElement, RenderOnce, SharedString, StyleRefinement, Styled, Window, actions, div,
    prelude::*, px,
};
use theme::{Radius, Rgba, Theme};

use crate::focus;
use crate::surface::CardTone;

type ClickFn = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type IndexFn = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// Key context of a [`RadioRow`].
pub const RADIO_CONTEXT: &str = "SelachiRadioRow";
/// The role a [`RadioRow`] declares in its key context.
pub const RADIO_ROLE: &str = "radio";

actions!(
    ui_kit_radio,
    [
        /// `Up` / `Left`: focus the previous row of the group.
        RadioPrev,
        /// `Down` / `Right`: focus the next row of the group.
        RadioNext,
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    let c = Some(RADIO_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", RadioPrev, c),
        KeyBinding::new("left", RadioPrev, c),
        KeyBinding::new("down", RadioNext, c),
        KeyBinding::new("right", RadioNext, c),
    ]);
}

/// Tone a card rests on, the tone it hovers to and the tone it takes while active / selected.
fn tones(theme: &Theme, tone: CardTone) -> (Rgba, Rgba, Rgba) {
    let c = theme.colors;
    match tone {
        CardTone::Base => (c.surface_0, c.surface_2, c.surface_3),
        CardTone::Raised => (c.surface_1, c.surface_2, c.surface_3),
        CardTone::High => (c.surface_2, c.surface_3, c.surface_4),
    }
}

// ---------------------------------------------------------------------------------------------
// FocusCard
// ---------------------------------------------------------------------------------------------

/// A focusable, clickable card wrapper. It is `Styled` (size, padding and layout are set with the
/// usual gpui methods) and takes children.
///
/// ```ignore
/// FocusCard::new("proj-0", &theme)
///     .active(selected == 0)
///     .on_click(|_, _, cx| { /* open it */ })
///     .p_3()
///     .child("content")
/// ```
#[derive(IntoElement)]
pub struct FocusCard {
    id: SharedString,
    theme: Theme,
    tone: CardTone,
    radius: f32,
    active: bool,
    focusable: bool,
    on_click: Option<ClickFn>,
    base: Div,
}

impl FocusCard {
    /// `surface_1` card, radius 12, padding 12.
    pub fn new(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            tone: CardTone::Raised,
            radius: Radius::SM,
            active: false,
            focusable: true,
            on_click: None,
            base: div().p(px(12.0)),
        }
    }

    pub fn tone(mut self, tone: CardTone) -> Self {
        self.tone = tone;
        self
    }

    /// Corner radius in px (default 12); the focus ring follows it.
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// The selected card of a set: the active tone instead of the resting one.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Keyboard focus (tab stop + ring); on by default. A card without `on_click` is never focusable.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    /// Makes the card clickable (pointer cursor, hover tone, `Enter` / `Space` when focused).
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl Styled for FocusCard {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for FocusCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for FocusCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        crate::ensure_init(cx);
        let (rest, hover, active) = tones(&self.theme, self.tone);
        let id = ElementId::from(self.id.clone());
        let clickable = self.on_click.is_some();
        let focus = (clickable && self.focusable).then(|| focus::handle(window, cx, &id));
        let is_active = self.active;
        let mut el = self
            .base
            .id(id.clone())
            .occlude()
            .rounded(px(self.radius))
            .bg(if is_active { active } else { rest })
            .when(clickable, |d| d.cursor_pointer())
            .when(clickable && !is_active, |d| d.hover(move |s| s.bg(hover)));
        if let Some(f) = self.on_click {
            el = el.on_click(move |e, w, cx| f(e, w, cx));
        }
        focus::wire(el, focus, &id, self.radius, self.theme.colors.accent)
    }
}

// ---------------------------------------------------------------------------------------------
// RadioRow
// ---------------------------------------------------------------------------------------------

/// One row of a radio group with custom content. Every row of the group gets the same `group`,
/// its own `index` and the group `len`, plus the group's `selected` index:
///
/// ```ignore
/// for (ix, preset) in presets.iter().enumerate() {
///     RadioRow::new("presets", ix, presets.len(), &theme)
///         .selected(self.selected)
///         .on_select(|ix, _, cx| { /* store ix */ })
///         .child(preset.name.clone())
/// }
/// ```
///
/// Arrow keys only move the focus (the selection changes on `Space` / `Enter` / click); turn on
/// [`RadioRow::select_on_arrow`] for the native radio behavior where the arrow also selects.
#[derive(IntoElement)]
pub struct RadioRow {
    group: SharedString,
    index: usize,
    len: usize,
    theme: Theme,
    selected: Option<usize>,
    select_on_arrow: bool,
    focusable: bool,
    on_select: Option<IndexFn>,
    base: Div,
}

impl RadioRow {
    pub fn new(group: impl Into<SharedString>, index: usize, len: usize, theme: &Theme) -> Self {
        Self {
            group: group.into(),
            index,
            len: len.max(index + 1),
            theme: *theme,
            selected: None,
            select_on_arrow: false,
            focusable: true,
            on_select: None,
            base: div()
                .w_full()
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0)),
        }
    }

    /// The selected index of the whole group (`None` while nothing is).
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// Arrow keys also select the row they move to.
    pub fn select_on_arrow(mut self, on: bool) -> Self {
        self.select_on_arrow = on;
        self
    }

    /// Keyboard focus; on by default.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    /// Called with this row's index on click, `Space` / `Enter` (and on arrows with
    /// [`RadioRow::select_on_arrow`]).
    pub fn on_select(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(f));
        self
    }

    /// The element id of the row at `index` of `group` (what `testing::bounds_of` finds).
    pub fn row_id(group: &str, index: usize) -> ElementId {
        ElementId::NamedInteger(SharedString::from(group.to_string()), index as u64)
    }
}

impl Styled for RadioRow {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for RadioRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

/// Moves the focus to `handle`, selecting `to` when the row asked for it.
fn move_focus(
    handle: &FocusHandle,
    to: usize,
    select: bool,
    on_select: &Option<IndexFn>,
    window: &mut Window,
    cx: &mut App,
) {
    window.focus(handle);
    if select && let Some(f) = on_select {
        f(to, window, cx);
    }
}

impl RenderOnce for RadioRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        crate::ensure_init(cx);
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (ix, len) = (self.index, self.len);
        let checked = self.selected == Some(ix);
        let id = ElementId::NamedInteger(self.group.clone(), ix as u64);
        let interactive = self.on_select.is_some();
        let focus = (interactive && self.focusable).then(|| {
            // Roving tab stop: the selected row, or the first one while nothing is selected.
            let stop = checked || (self.selected.is_none_or(|s| s >= len) && ix == 0);
            focus::handle(window, cx, &id).tab_stop(stop)
        });

        let mut ctx = KeyContext::new_with_defaults();
        ctx.add(RADIO_CONTEXT);
        ctx.set("role", RADIO_ROLE);
        ctx.set("aria-checked", if checked { "true" } else { "false" });

        // Handles of the neighbors (wrapping), resolved now: the action handlers run later.
        let neighbors = focus.is_some().then(|| {
            let (prev, next) = ((ix + len - 1) % len, (ix + 1) % len);
            let handle = |to: usize, window: &mut Window, cx: &mut App| {
                focus::handle(
                    window,
                    cx,
                    &ElementId::NamedInteger(self.group.clone(), to as u64),
                )
            };
            (
                (prev, handle(prev, window, cx)),
                (next, handle(next, window, cx)),
            )
        });

        let on_select = self.on_select;
        let select_on_arrow = self.select_on_arrow;
        let mut el = self
            .base
            .id(id.clone())
            .occlude()
            .key_context(ctx)
            .rounded(px(Radius::SM))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .text_color(c.text)
            .bg(if checked { c.surface_3 } else { c.surface_1 })
            .when(interactive, |d| d.cursor_pointer())
            .when(interactive && !checked, |d| {
                d.hover(move |s| s.bg(c.surface_2))
            });
        if let Some(f) = on_select.clone() {
            el = el.on_click(move |_, w, cx| f(ix, w, cx));
        }
        if let Some(((prev_ix, prev_h), (next_ix, next_h))) = neighbors {
            let (sel_prev, sel_next) = (on_select.clone(), on_select);
            el = el
                .on_action(move |_: &RadioPrev, window, cx| {
                    if prev_ix != ix {
                        move_focus(&prev_h, prev_ix, select_on_arrow, &sel_prev, window, cx);
                    }
                })
                .on_action(move |_: &RadioNext, window, cx| {
                    if next_ix != ix {
                        move_focus(&next_h, next_ix, select_on_arrow, &sel_next, window, cx);
                    }
                });
        }
        focus::wire(el, focus, &id, Radius::SM, c.accent)
    }
}
