//! `Select`: a dropdown with keyboard navigation.
//!
//! The trigger is a tab stop (focus ring on keyboard focus). With the trigger focused:
//!
//! | Key | Closed | Open |
//! |---|---|---|
//! | `Down` / `Up` / `Enter` / `Space` | opens on the current value | next / previous option (`Enter` / `Space` pick it) |
//! | `Home` / `End` | | first / last option |
//! | `Esc` | (falls through) | closes, focus stays on the trigger |
//! | letters | picks the next option starting with them | moves the highlight |
//! | `Tab` | moves on | closes, then moves on |
//!
//! The trigger has a max width and ellipsizes a long label. The list is a plain scrolling column
//! up to [`VIRTUALIZE_ABOVE`] options and a [`VirtualList`] beyond (so a thousand-entry list of
//! fonts or branches costs the same as twenty). Option ids are the option **values**, never
//! indices, so hover and highlight stay with the option when the list changes.
//!
//! ```ignore
//! let theme_select = cx.new(|cx| {
//!     Select::new("theme", theme, cx)
//!         .options([SelectOption::new("system", "Sistema"), SelectOption::new("dark", "Oscuro")])
//!         .with_value("dark")
//!         .max_w(240.0)
//! });
//! cx.subscribe(&theme_select, |this, select, ev: &SelectEvent, cx| {
//!     let SelectEvent::Changed(_) = ev;
//!     this.set_theme(select.read(cx).value().cloned(), cx);
//! })
//! .detach();
//! ```

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, ElementId, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, Render, ScrollHandle, ScrollStrategy, SharedString, Window, actions,
    canvas, div, prelude::*, px,
};
use theme::{Radius, Theme};

use crate::focus;
use crate::icon::{Icon, icon};
use crate::list::{UniformListScrollHandle, VirtualList};
use crate::popover::{Placement, Popover};

/// More options than this and the list is virtualized.
pub const VIRTUALIZE_ABOVE: usize = 50;
const ROW_H: f32 = 32.0;
const LIST_MAX_H: f32 = 320.0;
/// Type-ahead resets after this much silence.
const TYPEAHEAD_RESET: Duration = Duration::from_millis(800);

actions!(
    ui_kit_select,
    [
        /// Next option (opens the list when closed).
        SelectNext,
        /// Previous option (opens the list when closed).
        SelectPrev,
        /// First option.
        SelectFirst,
        /// Last option.
        SelectLast,
        /// Pick the highlighted option (opens the list when closed).
        SelectConfirm,
        /// `Space`: part of a type-ahead query when one is running, else like `SelectConfirm`.
        SelectSpace,
        /// Close the list.
        SelectCancel,
    ]
);

/// Key context of the trigger.
pub(crate) const CONTEXT: &str = "SelachiSelect";

pub(crate) fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, c),
        KeyBinding::new("up", SelectPrev, c),
        KeyBinding::new("home", SelectFirst, c),
        KeyBinding::new("end", SelectLast, c),
        KeyBinding::new("enter", SelectConfirm, c),
        KeyBinding::new("space", SelectSpace, c),
        KeyBinding::new("escape", SelectCancel, c),
    ]);
}

/// One entry of a [`Select`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    /// Stable identity (also the element id of its row).
    pub value: SharedString,
    pub label: SharedString,
    /// Right-aligned muted text (a shortcut, a path, a count).
    pub detail: Option<SharedString>,
    pub disabled: bool,
}

impl SelectOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: None,
            disabled: false,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// What a [`Select`] tells its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectEvent {
    /// The user picked the option at this index ([`Select::value`] has it).
    Changed(usize),
}

/// The dropdown entity. See the [module docs](crate::select).
pub struct Select {
    id: SharedString,
    theme: Theme,
    options: Vec<SelectOption>,
    selected: Option<usize>,
    placeholder: SharedString,
    open: bool,
    highlighted: Option<usize>,
    disabled: bool,
    width: Option<f32>,
    max_w: f32,
    focus: FocusHandle,
    scroll: ScrollHandle,
    vscroll: UniformListScrollHandle,
    trigger_w: Rc<Cell<f32>>,
    query: String,
    last_key: Option<Instant>,
}

impl EventEmitter<SelectEvent> for Select {}

impl Focusable for Select {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The next enabled index from `from` in direction `dir` (+1 / -1), clamped at the ends (no wrap).
pub(crate) fn step(disabled: &[bool], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = disabled.len();
    if n == 0 {
        return None;
    }
    let mut i = match from {
        Some(f) => f as i64 + i64::from(dir),
        None if dir > 0 => 0,
        None => n as i64 - 1,
    };
    while (0..n as i64).contains(&i) {
        if !disabled[i as usize] {
            return Some(i as usize);
        }
        i += i64::from(dir);
    }
    from.filter(|f| !disabled.get(*f).copied().unwrap_or(true))
}

/// The option a type-ahead `query` points at: one repeated letter cycles through the options that
/// start with it (from the one after `current`); a longer query matches by prefix from `current`.
pub(crate) fn typeahead_match(
    labels: &[&str],
    disabled: &[bool],
    current: Option<usize>,
    query: &str,
) -> Option<usize> {
    let q = query.to_lowercase();
    let chars: Vec<char> = q.chars().collect();
    let first = *chars.first()?;
    let n = labels.len();
    if n == 0 {
        return None;
    }
    let repeated = chars.iter().all(|c| *c == first);
    let (needle, from) = if chars.len() == 1 || repeated {
        (first.to_string(), current.map_or(0, |c| c + 1))
    } else {
        (q, current.unwrap_or(0))
    };
    (0..n)
        .map(|k| (from + k) % n)
        .find(|&i| !disabled[i] && labels[i].to_lowercase().starts_with(&needle))
}

impl Select {
    pub fn new(id: impl Into<SharedString>, theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            id: id.into(),
            theme,
            options: Vec::new(),
            selected: None,
            placeholder: SharedString::default(),
            open: false,
            highlighted: None,
            disabled: false,
            width: None,
            max_w: 320.0,
            focus: cx.focus_handle().tab_stop(true),
            scroll: ScrollHandle::new(),
            vscroll: UniformListScrollHandle::new(),
            trigger_w: Rc::new(Cell::new(0.0)),
            query: String::new(),
            last_key: None,
        }
    }

    // ----- builders ------------------------------------------------------------------------

    pub fn options(mut self, options: impl IntoIterator<Item = SelectOption>) -> Self {
        self.options = options.into_iter().collect();
        self
    }

    /// Initial selection by value (no event).
    pub fn with_value(mut self, value: &str) -> Self {
        self.selected = self.options.iter().position(|o| o.value == value);
        self
    }

    /// Text shown while nothing is selected.
    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Fixed trigger width in px (default: fit the label between 120 px and [`Select::max_w`]).
    pub fn width(mut self, px: f32) -> Self {
        self.width = Some(px);
        self
    }

    /// Widest the trigger may grow (default 320); a longer label is cut with an ellipsis.
    pub fn max_w(mut self, px: f32) -> Self {
        self.max_w = px;
        self
    }

    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    // ----- state ---------------------------------------------------------------------------

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// The value of the selected option (`None` while nothing is selected).
    pub fn value(&self) -> Option<&SharedString> {
        self.selected
            .and_then(|i| self.options.get(i))
            .map(|o| &o.value)
    }

    pub fn options_len(&self) -> usize {
        self.options.len()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn highlighted_index(&self) -> Option<usize> {
        self.highlighted
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    /// Replaces the options, keeping the selection when its value is still there. No event.
    pub fn set_options(
        &mut self,
        options: impl IntoIterator<Item = SelectOption>,
        cx: &mut Context<Self>,
    ) {
        let keep = self.value().cloned();
        self.options = options.into_iter().collect();
        self.selected = keep.and_then(|v| self.options.iter().position(|o| o.value == v));
        self.highlighted = self.highlighted.filter(|i| *i < self.options.len());
        cx.notify();
    }

    /// Selects by value from outside (hot reload). No event.
    pub fn select_value(&mut self, value: &str, cx: &mut Context<Self>) {
        let ix = self.options.iter().position(|o| o.value == value);
        if ix != self.selected {
            self.selected = ix;
            cx.notify();
        }
    }

    pub fn set_disabled(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.disabled != on {
            self.disabled = on;
            if on {
                self.open = false;
            }
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn open(&mut self, cx: &mut Context<Self>) {
        if self.open || self.disabled || self.options.is_empty() {
            return;
        }
        self.open = true;
        let disabled = self.disabled_flags();
        self.highlighted = self
            .selected
            .filter(|i| !disabled[*i])
            .or_else(|| step(&disabled, None, 1));
        self.reveal();
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.query.clear();
            cx.notify();
        }
    }

    fn disabled_flags(&self) -> Vec<bool> {
        self.options.iter().map(|o| o.disabled).collect()
    }

    fn reveal(&self) {
        if let Some(i) = self.highlighted {
            if self.options.len() > VIRTUALIZE_ABOVE {
                self.vscroll.scroll_to_item(i, ScrollStrategy::Center);
            } else {
                self.scroll.scroll_to_item(i);
            }
        }
    }

    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.options.get(ix).is_none_or(|o| o.disabled) {
            return;
        }
        let changed = self.selected != Some(ix);
        self.selected = Some(ix);
        self.highlighted = Some(ix);
        self.open = false;
        self.query.clear();
        if changed {
            cx.emit(SelectEvent::Changed(ix));
        }
        cx.notify();
    }

    fn highlight(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        if self.highlighted != ix {
            self.highlighted = ix;
            cx.notify();
        }
    }

    fn mv(&mut self, dir: i32, cx: &mut Context<Self>) {
        if !self.open {
            self.open(cx);
            return;
        }
        let next = step(&self.disabled_flags(), self.highlighted, dir);
        self.highlight(next, cx);
        self.reveal();
    }

    fn edge(&mut self, last: bool, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        let flags = self.disabled_flags();
        let next = if last {
            step(&flags, None, -1)
        } else {
            step(&flags, None, 1)
        };
        self.highlight(next, cx);
        self.reveal();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            self.open(cx);
        } else if let Some(i) = self.highlighted {
            self.pick(i, cx);
        }
    }

    fn type_ahead(&mut self, ch: &str, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self
            .last_key
            .is_none_or(|t| now.duration_since(t) > TYPEAHEAD_RESET)
        {
            self.query.clear();
        }
        self.last_key = Some(now);
        self.query.push_str(ch);
        let labels: Vec<&str> = self.options.iter().map(|o| o.label.as_ref()).collect();
        let current = if self.open {
            self.highlighted
        } else {
            self.selected
        };
        let Some(hit) = typeahead_match(&labels, &self.disabled_flags(), current, &self.query)
        else {
            return;
        };
        if self.open {
            self.highlight(Some(hit), cx);
            self.reveal();
        } else {
            self.pick(hit, cx);
        }
    }

    fn typeahead_active(&self) -> bool {
        !self.query.is_empty()
            && self
                .last_key
                .is_some_and(|t| t.elapsed() <= TYPEAHEAD_RESET)
    }
}

/// One row of the list. Plain data so a virtualized list can build it from a closure.
fn option_row(
    theme: &Theme,
    base: &ElementId,
    this: &gpui::Entity<Select>,
    ix: usize,
    opt: &SelectOption,
    highlighted: bool,
    selected: bool,
) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    let enabled = !opt.disabled;
    let entity = this.clone();
    let hover_entity = this.clone();
    let row_id = ElementId::NamedChild(Box::new(base.clone()), opt.value.clone());
    div()
        .id(row_id.clone())
        .debug_selector(move || row_id.to_string())
        .w_full()
        .h(px(ROW_H))
        .flex_none()
        .px(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .rounded(px(Radius::CONTROL))
        .font_family(ty.ui_family)
        .text_size(px(ty.ui_size_md))
        .text_color(if enabled { c.text } else { c.muted })
        .when(highlighted && enabled, |d| d.bg(c.surface_3))
        .when(!enabled, |d| d.opacity(0.5))
        .when(enabled, |d| {
            d.cursor_pointer()
                .on_hover(move |hovered, _, cx| {
                    if *hovered {
                        hover_entity.update(cx, |s, cx| s.highlight(Some(ix), cx));
                    }
                })
                .on_click(move |_, _, cx| entity.update(cx, |s, cx| s.pick(ix, cx)))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(opt.label.clone()),
        )
        .when_some(opt.detail.clone(), |d, detail| {
            d.child(
                div()
                    .flex_none()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .child(detail),
            )
        })
        .child(
            div()
                .size(px(16.0))
                .flex_none()
                .when(selected, |d| d.child(icon(Icon::Check, 16.0, c.accent))),
        )
        .into_any_element()
}

impl Render for Select {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ensure_init(cx);
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let theme = self.theme;
        let this = cx.entity();
        // Domain id: the caller's, not the entity's.
        let base = ElementId::from(self.id.clone());

        let (label, is_placeholder) = match self.selected.and_then(|i| self.options.get(i)) {
            Some(o) => (o.label.clone(), false),
            None => (self.placeholder.clone(), true),
        };
        let track = self.trigger_w.clone();
        let handle = self.focus.clone();
        let trigger_id = ElementId::NamedChild(Box::new(base.clone()), "trigger".into());
        let trigger = div()
            .id(trigger_id.clone())
            .key_context(CONTEXT)
            .relative()
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.mv(1, cx)))
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.mv(-1, cx)))
            .on_action(cx.listener(|s, _: &SelectFirst, _, cx| s.edge(false, cx)))
            .on_action(cx.listener(|s, _: &SelectLast, _, cx| s.edge(true, cx)))
            .on_action(cx.listener(|s, _: &SelectConfirm, _, cx| s.confirm(cx)))
            .on_action(cx.listener(|s, _: &SelectSpace, _, cx| {
                if s.typeahead_active() {
                    s.type_ahead(" ", cx);
                } else {
                    s.confirm(cx);
                }
            }))
            .on_action(cx.listener(|s, _: &SelectCancel, _, cx| {
                if s.open {
                    s.close(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|s, _: &focus::FocusNext, window, cx| {
                s.close(cx);
                window.focus_next();
            }))
            .on_action(cx.listener(|s, _: &focus::FocusPrev, window, cx| {
                s.close(cx);
                window.focus_prev();
            }))
            .on_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| {
                let m = ev.keystroke.modifiers;
                if m.control || m.alt || m.platform || m.function {
                    return;
                }
                if let Some(ch) = ev
                    .keystroke
                    .key_char
                    .as_deref()
                    .filter(|c| c.chars().all(|ch| !ch.is_control()))
                {
                    s.type_ahead(ch, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|s, _, window, cx| {
                    window.focus(&s.focus);
                    if s.open {
                        s.close(cx);
                    } else {
                        s.open(cx);
                    }
                }),
            )
            .when(self.disabled, |d| d.opacity(0.4))
            .when(!self.disabled, |d| d.cursor_pointer())
            .h(px(32.0))
            .when_some(self.width, |d, w| d.w(px(w)))
            .when(self.width.is_none(), |d| {
                d.min_w(px(120.0)).max_w(px(self.max_w))
            })
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::PILL))
            .bg(if self.open { c.surface_3 } else { c.surface_2 })
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .text_color(if is_placeholder { c.outline } else { c.text })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(icon(Icon::ChevronDown, 16.0, c.muted))
            .child(
                canvas(
                    move |b, _, _| track.set(f32::from(b.size.width)),
                    |_, (), _, _| {},
                )
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size_full(),
            )
            .child(focus::ring_element(
                handle,
                trigger_id.to_string(),
                Radius::PILL,
                c.accent,
            ));
        let trigger = trigger.debug_selector({
            let id = format!(
                "{}",
                ElementId::NamedChild(Box::new(base.clone()), "trigger".into())
            );
            move || id
        });

        let pop_id = SharedString::from(format!("{}-pop", self.id));
        let popover = Popover::new(pop_id, &theme)
            .placement(Placement::BottomStart)
            .open(self.open && !self.options.is_empty())
            .padding(4.0)
            .width(
                self.trigger_w
                    .get()
                    .clamp(160.0, 480.0)
                    .max(self.width.unwrap_or(0.0).min(480.0)),
            )
            .on_dismiss({
                let this = this.clone();
                move |_, cx| this.update(cx, |s, cx| s.close(cx))
            })
            .trigger(trigger);

        let popover = if self.open {
            let n = self.options.len();
            let list_h = (n as f32 * ROW_H).min(LIST_MAX_H);
            let sel = self.selected;
            let hi = self.highlighted;
            let list: AnyElement = if n > VIRTUALIZE_ABOVE {
                let options: Rc<[SelectOption]> = self.options.clone().into();
                VirtualList::new(
                    ElementId::NamedChild(Box::new(base.clone()), "vlist".into()),
                    n,
                    ROW_H,
                    move |range, _, _| {
                        range
                            .map(|ix| {
                                option_row(
                                    &theme,
                                    &base,
                                    &this,
                                    ix,
                                    &options[ix],
                                    hi == Some(ix),
                                    sel == Some(ix),
                                )
                            })
                            .collect()
                    },
                )
                .track_scroll(self.vscroll.clone())
                .h(px(list_h))
                .into_any_element()
            } else {
                div()
                    .id(ElementId::NamedChild(Box::new(base.clone()), "list".into()))
                    .max_h(px(list_h))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .children(self.options.iter().enumerate().map(|(ix, o)| {
                        option_row(&theme, &base, &this, ix, o, hi == Some(ix), sel == Some(ix))
                    }))
                    .into_any_element()
            };
            popover.child(list)
        } else {
            popover
        };
        // Same footprint as the trigger inside a row: let the popover root size to it.
        div().flex_none().child(popover)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_skips_disabled_and_clamps() {
        let d = [false, true, false, false, true];
        assert_eq!(step(&d, None, 1), Some(0));
        assert_eq!(
            step(&d, None, -1),
            Some(3),
            "from the end, skipping the disabled last one"
        );
        assert_eq!(step(&d, Some(0), 1), Some(2), "skips index 1");
        assert_eq!(
            step(&d, Some(3), 1),
            Some(3),
            "no wrap, stays on the last enabled"
        );
        assert_eq!(step(&d, Some(2), -1), Some(0));
        assert_eq!(step(&d, Some(0), -1), Some(0));
        assert_eq!(step(&[], None, 1), None);
        assert_eq!(step(&[true, true], None, 1), None, "nothing enabled");
    }

    #[test]
    fn typeahead_cycles_a_letter_and_extends_a_prefix() {
        let labels = ["Alpha", "Beta", "Bravo", "Brown", "Charlie"];
        let off = [false; 5];
        // One letter: the next match after the current one, wrapping.
        assert_eq!(typeahead_match(&labels, &off, None, "b"), Some(1));
        assert_eq!(typeahead_match(&labels, &off, Some(1), "b"), Some(2));
        assert_eq!(
            typeahead_match(&labels, &off, Some(3), "b"),
            Some(1),
            "wraps"
        );
        // A repeated letter keeps cycling.
        assert_eq!(typeahead_match(&labels, &off, Some(1), "bb"), Some(2));
        // A longer prefix narrows from the current option.
        assert_eq!(typeahead_match(&labels, &off, Some(1), "br"), Some(2));
        assert_eq!(typeahead_match(&labels, &off, Some(2), "bro"), Some(3));
        assert_eq!(typeahead_match(&labels, &off, None, "zz"), None);
        assert_eq!(typeahead_match(&labels, &off, None, ""), None);
        // Case-insensitive, and disabled options are never hit.
        assert_eq!(
            typeahead_match(&labels, &[false, true, false, false, false], None, "BE"),
            None
        );
        assert_eq!(typeahead_match(&labels, &off, None, "CH"), Some(4));
    }
}
