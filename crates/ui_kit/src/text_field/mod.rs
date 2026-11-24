//! [`TextField`] (single line) and [`TextArea`] (multi line): the one text input of Selachi.
//!
//! It replaces the seven copies that lived in `ui_settings`, `ui_layout`, `ui_palette`, `ui_git`,
//! `ui_viewer` (single line) and `ui_chat`, `ui_review` (multi line). One `Entity<TextField>`
//! owns the text; it implements `EntityInputHandler` (IME composition, keyboard layouts), mouse
//! and keyboard selection (double click = word, triple = line / all), clipboard, word motion and
//! word delete, soft wrapping with a vertical scroll for the area, a horizontal scroll that
//! keeps the caret in view for the field, and a middle ellipsis (`abc...xyz`) while a long value
//! is not being edited.
//!
//! ```ignore
//! let name = cx.new(|cx| {
//!     TextField::new(theme, cx).placeholder("Nombre del preset").with_text("Mi preset")
//! });
//! cx.subscribe(&name, |this, field, ev: &TextFieldEvent, cx| match ev {
//!     TextFieldEvent::Changed => this.validate(field.read(cx).text(), cx),
//!     TextFieldEvent::Submit => this.save(cx),
//!     TextFieldEvent::Cancel | TextFieldEvent::Blur => this.close(cx),
//!     _ => {}
//! })
//! .detach();
//! // in render: .child(name.clone())
//! ```
//!
//! Keys live in one `actions!` set bound under the single key context `SelachiTextField`
//! ([`init`](crate::init) binds them; components also call it lazily, so forgetting is harmless).
//!
//! | Variant | Enter | Shift+Enter | Ctrl+Enter |
//! |---|---|---|---|
//! | single line | `Submit` | `SubmitReverse` (find previous) | `Submit` |
//! | area, [`EnterMode::Submit`] (chat) | `Submit` | newline | `Submit` |
//! | area, [`EnterMode::Newline`] (notes) | newline | newline | `Submit` |

mod buf;
mod element;

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, ScrollWheelEvent, SharedString, Subscription, UTF16Selection, Window,
    div, prelude::*, px,
};
use theme::{Radius, Theme};

pub use buf::{TextBuf, clean, middle_ellipsis};
pub(crate) use element::{Laid, SingleLayout};

/// The mask drawn for each char of a password field.
pub(crate) const MASK: char = '\u{2022}';

/// Key context of every text field.
pub const CONTEXT: &str = "SelachiTextField";

/// The actions of the text field (all under [`CONTEXT`]).
pub mod act {
    use gpui::actions;

    actions!(
        ui_kit_text,
        [
            Backspace,
            Delete,
            DeleteWord,
            DeleteWordForward,
            Left,
            Right,
            WordLeft,
            WordRight,
            SelectLeft,
            SelectRight,
            SelectWordLeft,
            SelectWordRight,
            Up,
            Down,
            SelectUp,
            SelectDown,
            Home,
            End,
            SelectHome,
            SelectEnd,
            DocStart,
            DocEnd,
            SelectAll,
            Copy,
            Cut,
            Paste,
            Enter,
            ShiftEnter,
            CtrlEnter,
            Cancel,
            TabKey,
            BackTab,
        ]
    );
}

/// Binds the text field keys (called by [`init`](crate::init)).
pub(crate) fn bind_keys(cx: &mut App) {
    use act::*;
    use gpui::KeyBinding;
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("ctrl-backspace", DeleteWord, c),
        KeyBinding::new("ctrl-delete", DeleteWordForward, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("ctrl-left", WordLeft, c),
        KeyBinding::new("ctrl-right", WordRight, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, c),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("ctrl-home", DocStart, c),
        KeyBinding::new("ctrl-end", DocEnd, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-x", Cut, c),
        KeyBinding::new("ctrl-v", Paste, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", ShiftEnter, c),
        KeyBinding::new("ctrl-enter", CtrlEnter, c),
        KeyBinding::new("escape", Cancel, c),
        KeyBinding::new("tab", TabKey, c),
        KeyBinding::new("shift-tab", BackTab, c),
    ]);
}

/// Navigation keys forwarded while [`TextField::set_capture_nav`] is on (a mention / completion
/// popover owned by the parent is open).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Tab,
}

/// What a text field tells its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFieldEvent {
    /// The text changed by typing, deleting, pasting or an IME ([`TextField::set_text`] does not emit).
    Changed,
    /// `Enter` (or `Ctrl+Enter`), see the table of [`crate::text_field`].
    Submit,
    /// `Shift+Enter` in a single-line field (e.g. "previous match").
    SubmitReverse,
    /// `Esc`.
    Cancel,
    /// The field lost focus.
    Blur,
    /// `Up` / `Down` / `Tab` while [`TextField::set_capture_nav`] is on.
    Nav(Nav),
}

/// What `Enter` does in a [`TextArea`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EnterMode {
    /// `Enter` submits, `Shift+Enter` inserts a newline (chat composer).
    #[default]
    Submit,
    /// `Enter` inserts a newline, `Ctrl+Enter` submits (review notes).
    Newline,
}

/// The multi-line variant: the same entity built with [`TextField::area`].
pub type TextArea = TextField;

/// A text input entity. See the [module docs](crate::text_field).
pub struct TextField {
    theme: Theme,
    focus: FocusHandle,
    buf: TextBuf,
    placeholder: SharedString,
    multiline: bool,
    enter: EnterMode,
    read_only: bool,
    password: bool,
    error: Option<SharedString>,
    chrome: bool,
    mono: bool,
    font_size: Option<f32>,
    min_lines: usize,
    max_lines: usize,
    capture_nav: bool,
    // Layout of the last frame (single line).
    single: Option<SingleLayout>,
    // Layout of the last frame (area).
    laid: Rc<RefCell<Option<Laid>>>,
    scroll: Rc<Cell<Pixels>>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    _blur: Option<Subscription>,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    /// A single-line field (32 px box, `surface_2`, accent border while focused).
    pub fn new(theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            theme,
            focus: cx.focus_handle().tab_stop(true),
            buf: TextBuf::default(),
            placeholder: SharedString::default(),
            multiline: false,
            enter: EnterMode::Submit,
            read_only: false,
            password: false,
            error: None,
            chrome: true,
            mono: false,
            font_size: None,
            min_lines: 1,
            max_lines: 1,
            capture_nav: false,
            single: None,
            laid: Rc::new(RefCell::new(None)),
            scroll: Rc::new(Cell::new(px(0.0))),
            last_bounds: None,
            selecting: false,
            _blur: None,
        }
    }

    /// A multi-line area: soft wrapped, 1 to 10 lines tall (see [`TextField::lines`]).
    pub fn area(theme: Theme, cx: &mut Context<Self>) -> Self {
        let mut f = Self::new(theme, cx);
        f.multiline = true;
        f.max_lines = 10;
        f
    }

    // ----- builders ------------------------------------------------------------------------

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Initial text, caret at the end (no event).
    pub fn with_text(mut self, text: &str) -> Self {
        self.buf = TextBuf::with_text(&clean(text, self.multiline));
        self
    }

    /// Selectable and copyable, not editable.
    pub fn read_only(mut self, on: bool) -> Self {
        self.read_only = on;
        self
    }

    /// Draws bullets instead of the text and disables copy / cut. Single line only.
    pub fn password(mut self, on: bool) -> Self {
        self.password = on && !self.multiline;
        self
    }

    /// Monospace font (paths, hashes, commands).
    pub fn monospace(mut self, on: bool) -> Self {
        self.mono = on;
        self
    }

    /// Text size in px (default: the theme's `ui_size_md`, 14 for the area).
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = Some(size);
        self
    }

    /// Area height bounds in lines (it grows with its content between them).
    pub fn lines(mut self, min: usize, max: usize) -> Self {
        self.min_lines = min.max(1);
        self.max_lines = max.max(self.min_lines);
        self
    }

    pub fn enter_mode(mut self, mode: EnterMode) -> Self {
        self.enter = mode;
        self
    }

    /// No box, padding or error line: just the text, for hosts that draw their own chrome (the
    /// chat composer, a palette input inside its own bar).
    pub fn bare(mut self) -> Self {
        self.chrome = false;
        self
    }

    // ----- setters / accessors -------------------------------------------------------------

    pub fn text(&self) -> &str {
        &self.buf.text
    }

    pub fn is_empty(&self) -> bool {
        self.buf.text.is_empty()
    }

    /// Only whitespace (or nothing).
    pub fn is_blank(&self) -> bool {
        self.buf.text.trim().is_empty()
    }

    /// Byte offset of the caret.
    pub fn caret(&self) -> usize {
        self.buf.cursor()
    }

    /// The selection in bytes (empty = a caret).
    pub fn selection(&self) -> Range<usize> {
        self.buf.sel.clone()
    }

    /// An IME composition is in progress.
    pub fn is_composing(&self) -> bool {
        self.buf.marked.is_some()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Replaces the text from outside (hot reload); caret at the end. Does not emit `Changed`.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = clean(text, self.multiline);
        if self.buf.text != text {
            self.buf = TextBuf::with_text(&text);
            self.scroll.set(px(0.0));
            cx.notify();
        }
    }

    /// The text, leaving the field empty (no event).
    pub fn take_text(&mut self, cx: &mut Context<Self>) -> String {
        let t = std::mem::take(&mut self.buf.text);
        self.buf = TextBuf::default();
        self.scroll.set(px(0.0));
        cx.notify();
        t
    }

    /// Types `text` at the caret (emits `Changed`).
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let t = clean(text, self.multiline);
        self.edit(cx, |b| {
            b.replace(None, &t);
            true
        });
    }

    /// Replaces `range` with `with`, caret after it (emits `Changed`).
    pub fn replace_range(&mut self, range: Range<usize>, with: &str, cx: &mut Context<Self>) {
        let t = clean(with, self.multiline);
        self.edit(cx, |b| {
            b.replace(Some(range), &t);
            true
        });
    }

    /// Selects everything.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.buf.select_all();
        cx.notify();
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    pub fn set_read_only(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.read_only != on {
            self.read_only = on;
            cx.notify();
        }
    }

    /// Shows `message` under the field and paints the border in `error`; `None` clears it.
    pub fn set_error(&mut self, message: Option<impl Into<SharedString>>, cx: &mut Context<Self>) {
        let message = message.map(Into::into);
        if self.error != message {
            self.error = message;
            cx.notify();
        }
    }

    /// Re-themes the field (on a theme or accent change).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    pub fn set_lines(&mut self, min: usize, max: usize) {
        self.min_lines = min.max(1);
        self.max_lines = max.max(self.min_lines);
    }

    /// While on, `Up` / `Down` / `Tab` are emitted as [`TextFieldEvent::Nav`] instead of acting.
    pub fn set_capture_nav(&mut self, on: bool) {
        self.capture_nav = on;
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    // ----- editing -------------------------------------------------------------------------

    fn mapped(&self) -> bool {
        self.password && !self.buf.text.is_empty()
    }

    /// Buffer byte offset to display byte offset (they differ only for a password).
    pub(crate) fn offset_to_display(&self, off: usize) -> usize {
        if self.mapped() {
            self.buf.text[..off.min(self.buf.text.len())]
                .chars()
                .count()
                * MASK.len_utf8()
        } else {
            off
        }
    }

    /// Display byte offset to buffer byte offset.
    pub(crate) fn display_to_offset(&self, d: usize) -> usize {
        if self.mapped() {
            self.buf
                .text
                .char_indices()
                .nth(d / MASK.len_utf8())
                .map_or(self.buf.text.len(), |(i, _)| i)
        } else {
            d.min(self.buf.text.len())
        }
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TextBuf) -> bool) {
        if self.read_only {
            return;
        }
        if f(&mut self.buf) {
            cx.emit(TextFieldEvent::Changed);
        }
        cx.notify();
    }

    fn motion(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TextBuf)) {
        f(&mut self.buf);
        cx.notify();
    }

    fn index_at(&self, p: Point<Pixels>) -> usize {
        if self.multiline {
            return match (&self.last_bounds, self.laid.borrow().as_ref()) {
                (Some(b), Some(laid)) => laid.index_for_position(gpui::point(
                    p.x - b.left(),
                    p.y - b.top() + self.scroll.get(),
                )),
                _ => self.buf.text.len(),
            };
        }
        match (&self.last_bounds, &self.single) {
            // The ellipsized copy has no 1:1 mapping to the text: a click just focuses at the end.
            (Some(_), Some(s)) if s.ellipsized => self.buf.text.len(),
            (Some(b), Some(s)) if !self.buf.text.is_empty() => {
                self.display_to_offset(s.line.closest_index_for_x(p.x - b.left() + s.scroll_x))
            }
            _ => 0,
        }
    }

    fn vertical(&mut self, dir: i32, select: bool, cx: &mut Context<Self>) {
        let target = {
            let laid = self.laid.borrow();
            let Some(laid) = laid.as_ref() else { return };
            let here = laid.position_for_index(self.buf.cursor());
            let y = here.y + laid.line_h * (dir as f32) + laid.line_h * 0.5;
            if y < px(0.0) {
                0
            } else if y >= laid.total_height() {
                self.buf.text.len()
            } else {
                laid.index_for_position(gpui::point(here.x, y))
            }
        };
        self.motion(cx, |b| {
            if select {
                b.select_to(target)
            } else {
                b.move_to(target)
            }
        });
    }

    fn newline(&mut self, cx: &mut Context<Self>) {
        if self.multiline {
            self.edit(cx, |b| {
                b.replace(None, "\n");
                true
            });
        }
    }

    fn on_enter(&mut self, _: &act::Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && self.enter == EnterMode::Newline {
            self.newline(cx);
        } else {
            cx.emit(TextFieldEvent::Submit);
        }
    }

    fn on_shift_enter(&mut self, _: &act::ShiftEnter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.newline(cx);
        } else {
            cx.emit(TextFieldEvent::SubmitReverse);
        }
    }

    fn on_up(&mut self, select: bool, cx: &mut Context<Self>) {
        if self.capture_nav && !select {
            cx.emit(TextFieldEvent::Nav(Nav::Up));
        } else if self.multiline {
            self.vertical(-1, select, cx);
        } else {
            // A single line has no rows: let a parent list use the key.
            cx.propagate();
        }
    }

    fn on_down(&mut self, select: bool, cx: &mut Context<Self>) {
        if self.capture_nav && !select {
            cx.emit(TextFieldEvent::Nav(Nav::Down));
        } else if self.multiline {
            self.vertical(1, select, cx);
        } else {
            cx.propagate();
        }
    }

    fn on_copy(&mut self, _: &act::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.sel.is_empty() && !self.password {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buf.selected_text().to_string(),
            ));
        }
    }

    fn on_cut(&mut self, _: &act::Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.sel.is_empty() && !self.password && !self.read_only {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buf.selected_text().to_string(),
            ));
            self.edit(cx, |b| {
                b.replace(None, "");
                true
            });
        }
    }

    fn on_paste(&mut self, _: &act::Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.insert(&text, cx);
        }
    }

    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.selecting = true;
        let i = self.index_at(e.position);
        match e.click_count {
            2 => {
                let w = self.buf.word_at(i);
                self.buf.select_range(w);
            }
            n if n >= 3 => {
                if self.multiline {
                    self.buf
                        .select_range(self.buf.line_start(i)..self.buf.line_end(i));
                } else {
                    self.buf.select_all();
                }
            }
            _ if e.modifiers.shift => self.buf.select_to(i),
            _ => self.buf.move_to(i),
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            let i = self.index_at(e.position);
            self.buf.select_to(i);
            cx.notify();
        }
    }

    fn on_scroll(&mut self, e: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            return;
        }
        let (Some(b), Some(laid)) = (self.last_bounds, self.laid.borrow().clone()) else {
            return;
        };
        let overflow = laid.total_height() - b.size.height;
        if overflow <= px(0.0) {
            return;
        }
        let dy = e.delta.pixel_delta(window.line_height()).y;
        let next = (self.scroll.get() - dy).max(px(0.0)).min(overflow);
        if next != self.scroll.get() {
            self.scroll.set(next);
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.buf.range_from_utf16(&range_utf16);
        actual.replace(self.buf.range_to_utf16(&r));
        Some(self.buf.text[r].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buf.range_to_utf16(&self.buf.sel),
            reversed: self.buf.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buf.marked.as_ref().map(|r| self.buf.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buf.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let r = range_utf16.map(|r| self.buf.range_from_utf16(&r));
        let t = clean(new_text, self.multiline);
        self.edit(cx, |b| {
            b.replace(r, &t);
            true
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = clean(new_text, self.multiline);
        self.edit(cx, |buf| {
            let range = range_utf16
                .map(|r| buf.range_from_utf16(&r))
                .or_else(|| buf.marked.clone())
                .unwrap_or_else(|| buf.sel.clone());
            let end = range.end.min(buf.text.len());
            let range = range.start.min(end)..end;
            buf.text.replace_range(range.clone(), &new_text);
            buf.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
            buf.sel = new_selected_utf16
                .map(|r| buf.range_from_utf16(&r))
                .map(|r| r.start + range.start..r.end + range.start)
                .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
            buf.reversed = false;
            true
        });
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let r = self.buf.range_from_utf16(&range_utf16);
        if self.multiline {
            let laid = self.laid.borrow();
            let laid = laid.as_ref()?;
            let s = self.scroll.get();
            let (a, b) = (
                laid.position_for_index(r.start),
                laid.position_for_index(r.end),
            );
            return Some(Bounds::from_corners(
                gpui::point(bounds.left() + a.x, bounds.top() + a.y - s),
                gpui::point(
                    bounds.left() + b.x.max(a.x + px(1.0)),
                    bounds.top() + b.y - s + laid.line_h,
                ),
            ));
        }
        let s = self.single.as_ref()?;
        let (a, b) = (
            self.offset_to_display(r.start),
            self.offset_to_display(r.end),
        );
        Some(Bounds::from_corners(
            gpui::point(
                bounds.left() + s.line.x_for_index(a) - s.scroll_x,
                bounds.top(),
            ),
            gpui::point(
                bounds.left() + s.line.x_for_index(b) - s.scroll_x,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let b = self.last_bounds?;
        if self.multiline {
            let laid = self.laid.borrow();
            let laid = laid.as_ref()?;
            let utf8 = laid
                .index_for_position(gpui::point(
                    p.x - b.left(),
                    p.y - b.top() + self.scroll.get(),
                ))
                .min(self.buf.text.len());
            return Some(self.buf.offset_to_utf16(utf8));
        }
        let s = self.single.as_ref()?;
        let local = b.localize(&p)?;
        let utf8 = self.display_to_offset(s.line.index_for_x(p.x - local.x + s.scroll_x)?);
        Some(self.buf.offset_to_utf16(utf8.min(self.buf.text.len())))
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ensure_init(cx);
        if self._blur.is_none() {
            // Needs the window, which the constructor does not have: subscribe on the first frame.
            self._blur = Some(cx.on_blur(&self.focus, window, |_, _, cx| {
                cx.emit(TextFieldEvent::Blur)
            }));
        }
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let focused = self.focus.is_focused(window);
        let theme = self.theme;
        let multiline = self.multiline;
        let overflowing = !multiline
            && !focused
            && !self.buf.text.is_empty()
            && self.single.as_ref().is_some_and(|s| s.overflow);
        let full =
            (overflowing && !self.password).then(|| SharedString::from(self.buf.text.clone()));
        let id = ElementId::from(("ui-kit-text-field", cx.entity_id().as_u64() as usize));
        let invalid = self.error.is_some();

        let mut field = div()
            .id(id)
            .when_some(full, |d, text| {
                d.tooltip(crate::Tooltip::text(&theme, text))
            })
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|t, _: &act::Backspace, _, cx| t.edit(cx, TextBuf::backspace)))
            .on_action(cx.listener(|t, _: &act::Delete, _, cx| t.edit(cx, TextBuf::delete)))
            .on_action(
                cx.listener(|t, _: &act::DeleteWord, _, cx| t.edit(cx, TextBuf::delete_word)),
            )
            .on_action(cx.listener(|t, _: &act::DeleteWordForward, _, cx| {
                t.edit(cx, TextBuf::delete_word_forward)
            }))
            .on_action(cx.listener(|t, _: &act::Left, _, cx| t.motion(cx, TextBuf::left)))
            .on_action(cx.listener(|t, _: &act::Right, _, cx| t.motion(cx, TextBuf::right)))
            .on_action(cx.listener(|t, _: &act::WordLeft, _, cx| {
                t.motion(cx, |b| b.move_to(b.prev_word(b.cursor())));
            }))
            .on_action(cx.listener(|t, _: &act::WordRight, _, cx| {
                t.motion(cx, |b| b.move_to(b.next_word(b.cursor())));
            }))
            .on_action(cx.listener(|t, _: &act::SelectLeft, _, cx| {
                t.motion(cx, |b| b.select_to(b.prev_boundary(b.cursor())));
            }))
            .on_action(cx.listener(|t, _: &act::SelectRight, _, cx| {
                t.motion(cx, |b| b.select_to(b.next_boundary(b.cursor())));
            }))
            .on_action(cx.listener(|t, _: &act::SelectWordLeft, _, cx| {
                t.motion(cx, |b| b.select_to(b.prev_word(b.cursor())));
            }))
            .on_action(cx.listener(|t, _: &act::SelectWordRight, _, cx| {
                t.motion(cx, |b| b.select_to(b.next_word(b.cursor())));
            }))
            .on_action(cx.listener(|t, _: &act::Up, _, cx| t.on_up(false, cx)))
            .on_action(cx.listener(|t, _: &act::Down, _, cx| t.on_down(false, cx)))
            .on_action(cx.listener(|t, _: &act::SelectUp, _, cx| t.on_up(true, cx)))
            .on_action(cx.listener(|t, _: &act::SelectDown, _, cx| t.on_down(true, cx)))
            .on_action(cx.listener(|t, _: &act::Home, _, cx| {
                let multi = t.multiline;
                t.motion(cx, |b| {
                    b.move_to(if multi { b.line_start(b.cursor()) } else { 0 })
                });
            }))
            .on_action(cx.listener(|t, _: &act::End, _, cx| {
                let multi = t.multiline;
                t.motion(cx, |b| {
                    b.move_to(if multi {
                        b.line_end(b.cursor())
                    } else {
                        b.text.len()
                    })
                });
            }))
            .on_action(cx.listener(|t, _: &act::SelectHome, _, cx| {
                let multi = t.multiline;
                t.motion(cx, |b| {
                    b.select_to(if multi { b.line_start(b.cursor()) } else { 0 })
                });
            }))
            .on_action(cx.listener(|t, _: &act::SelectEnd, _, cx| {
                let multi = t.multiline;
                t.motion(cx, |b| {
                    b.select_to(if multi {
                        b.line_end(b.cursor())
                    } else {
                        b.text.len()
                    })
                });
            }))
            .on_action(cx.listener(|t, _: &act::DocStart, _, cx| t.motion(cx, |b| b.move_to(0))))
            .on_action(
                cx.listener(|t, _: &act::DocEnd, _, cx| t.motion(cx, |b| b.move_to(b.text.len()))),
            )
            .on_action(
                cx.listener(|t, _: &act::SelectAll, _, cx| t.motion(cx, TextBuf::select_all)),
            )
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_shift_enter))
            .on_action(cx.listener(|_, _: &act::CtrlEnter, _, cx| cx.emit(TextFieldEvent::Submit)))
            .on_action(cx.listener(|_, _: &act::Cancel, _, cx| cx.emit(TextFieldEvent::Cancel)))
            .on_action(cx.listener(|t, _: &act::TabKey, window, cx| {
                if t.capture_nav {
                    cx.emit(TextFieldEvent::Nav(Nav::Tab));
                } else {
                    window.focus_next();
                }
            }))
            .on_action(cx.listener(|_, _: &act::BackTab, window, _| window.focus_prev()))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .font_family(if self.mono {
                ty.mono_family
            } else {
                ty.ui_family
            })
            .text_color(c.text);

        field = if multiline {
            field
                .w_full()
                .text_size(px(self.font_size.unwrap_or(ty.ui_size_body)))
                .line_height(px((self.font_size.unwrap_or(ty.ui_size_body) * 1.5).round()))
                .when(self.chrome, |d| {
                    d.px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(Radius::SM))
                        .bg(c.surface_2)
                        .border_2()
                        .border_color(if invalid {
                            c.error
                        } else if focused {
                            c.accent
                        } else {
                            c.surface_2
                        })
                })
                .child(element::AreaElement { input: cx.entity() })
        } else {
            field
                .w_full()
                .h(px(32.0))
                .flex()
                .items_center()
                .text_size(px(self.font_size.unwrap_or(ty.ui_size_md)))
                .line_height(px(18.0))
                .when(self.chrome, |d| {
                    d.px(px(12.0))
                        .rounded(px(Radius::PILL))
                        .bg(c.surface_2)
                        .border_2()
                        .border_color(if invalid {
                            c.error
                        } else if focused {
                            c.accent
                        } else {
                            c.surface_2
                        })
                })
                .child(
                    div()
                        .w_full()
                        .child(element::LineElement { input: cx.entity() }),
                )
        };

        let error = self.chrome.then(|| self.error.clone()).flatten();
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .when(!multiline, |d| d.flex_1().min_w(px(0.0)))
            .when(multiline, |d| d.w_full())
            .child(field)
            .when_some(error, |d, msg| {
                d.child(
                    div()
                        .px(px(4.0))
                        .font_family(ty.ui_family)
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.error)
                        .child(msg),
                )
            })
    }
}

#[cfg(test)]
mod tests;
