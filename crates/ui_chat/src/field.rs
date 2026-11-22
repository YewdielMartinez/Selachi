//! The text field of the composer and of inline renames: a pure [`TextBuf`] plus a gpui entity
//! implementing `EntityInputHandler` (IMEs and keyboard layouts work), soft wrapping, a
//! vertical scroll when the text outgrows `max_lines`, and chat keys: `Enter` submits,
//! `Shift+Enter` inserts a newline, `Esc` cancels. When the owner captures navigation (a mention
//! popover is open) `Up` / `Down` / `Tab` become [`Nav`] events instead of moving the caret.
//!
//! Follows the field of `ui_review` (same structure, different keys); ui_kit has no text input.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollWheelEvent, SharedString, Style,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, size,
};
use theme::Theme;

actions!(
    chat_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        DeleteWord,
        Newline,
        Submit,
        Cancel,
        Tab,
    ]
);

/// Key context of the field.
pub const CONTEXT: &str = "ChatField";

/// Registers the bindings of [`ChatField`] (called by `ui_chat::init`).
pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("left", Left, Some(CONTEXT)),
        KeyBinding::new("right", Right, Some(CONTEXT)),
        KeyBinding::new("up", Up, Some(CONTEXT)),
        KeyBinding::new("down", Down, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-x", Cut, Some(CONTEXT)),
        KeyBinding::new("home", Home, Some(CONTEXT)),
        KeyBinding::new("end", End, Some(CONTEXT)),
        KeyBinding::new("ctrl-backspace", DeleteWord, Some(CONTEXT)),
        KeyBinding::new("shift-enter", Newline, Some(CONTEXT)),
        KeyBinding::new("enter", Submit, Some(CONTEXT)),
        KeyBinding::new("ctrl-enter", Submit, Some(CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
        KeyBinding::new("tab", Tab, Some(CONTEXT)),
    ]);
}

// ---------------------------------------------------------------------------------------------
// Pure model
// ---------------------------------------------------------------------------------------------

/// Text plus a selection (byte offsets, always on char boundaries).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBuf {
    pub text: String,
    /// `start..end`; empty = a caret.
    pub sel: Range<usize>,
    /// The caret is at `sel.start` (the selection was extended leftwards).
    pub reversed: bool,
    pub marked: Option<Range<usize>>,
}

impl TextBuf {
    pub fn cursor(&self) -> usize {
        if self.reversed { self.sel.start } else { self.sel.end }
    }

    pub fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..offset].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..].chars().next().map_or(self.text.len(), |c| offset + c.len_utf8())
    }

    pub fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |i| i + 1)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        self.text[offset..].find('\n').map_or(self.text.len(), |i| offset + i)
    }

    pub fn move_to(&mut self, offset: usize) {
        self.sel = offset..offset;
        self.reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        if self.reversed {
            self.sel.start = offset;
        } else {
            self.sel.end = offset;
        }
        if self.sel.end < self.sel.start {
            self.reversed = !self.reversed;
            self.sel = self.sel.end..self.sel.start;
        }
    }

    pub fn left(&mut self) {
        if self.sel.is_empty() {
            self.move_to(self.prev_boundary(self.cursor()));
        } else {
            self.move_to(self.sel.start);
        }
    }

    pub fn right(&mut self) {
        if self.sel.is_empty() {
            self.move_to(self.next_boundary(self.sel.end));
        } else {
            self.move_to(self.sel.end);
        }
    }

    pub fn select_all(&mut self) {
        self.move_to(0);
        self.select_to(self.text.len());
    }

    /// Replace `range` (default: the marked text, else the selection) with `new`.
    pub fn replace(&mut self, range: Option<Range<usize>>, new: &str) {
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.sel.clone());
        self.text.replace_range(range.clone(), new);
        let caret = range.start + new.len();
        self.sel = caret..caret;
        self.reversed = false;
        self.marked = None;
    }

    pub fn backspace(&mut self) {
        if self.sel.is_empty() {
            self.select_to(self.prev_boundary(self.cursor()));
        }
        self.replace(None, "");
    }

    pub fn delete(&mut self) {
        if self.sel.is_empty() {
            self.select_to(self.next_boundary(self.cursor()));
        }
        self.replace(None, "");
    }

    /// Deletes the selection, or the word before the caret.
    pub fn delete_word(&mut self) {
        if self.sel.is_empty() {
            let caret = self.cursor();
            let head = &self.text[..caret];
            let trimmed = head.trim_end();
            let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            self.move_to(caret);
            self.select_to(start);
        }
        self.replace(None, "");
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.sel.clone()]
    }

    pub fn offset_from_utf16(&self, off: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.text.chars() {
            if utf16 >= off {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    pub fn offset_to_utf16(&self, off: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.text.chars() {
            if utf8 >= off {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    pub fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    pub fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

// ---------------------------------------------------------------------------------------------
// Layout of the last frame
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Laid {
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    line_h: Pixels,
    placeholder: bool,
}

impl Laid {
    fn height_of(&self, i: usize) -> Pixels {
        self.lines[i].size(self.line_h).height
    }

    fn total_height(&self) -> Pixels {
        (0..self.lines.len()).map(|i| self.height_of(i)).fold(px(0.0), |a, b| a + b)
    }

    fn position_for_index(&self, offset: usize) -> Point<Pixels> {
        if self.placeholder {
            return point(px(0.0), px(0.0));
        }
        let mut y = px(0.0);
        for (i, line) in self.lines.iter().enumerate() {
            let start = self.starts[i];
            let end = start + line.len();
            if offset <= end || i + 1 == self.lines.len() {
                let local = offset.saturating_sub(start).min(line.len());
                let p = line.position_for_index(local, self.line_h).unwrap_or_default();
                return point(p.x, y + p.y);
            }
            y += self.height_of(i);
        }
        point(px(0.0), px(0.0))
    }

    fn index_for_position(&self, p: Point<Pixels>) -> usize {
        if self.placeholder {
            return 0;
        }
        let mut top = px(0.0);
        for (i, line) in self.lines.iter().enumerate() {
            let h = self.height_of(i);
            if p.y < top + h || i + 1 == self.lines.len() {
                let local = point(p.x.max(px(0.0)), (p.y - top).max(px(0.0)));
                let idx = match line.closest_index_for_position(local, self.line_h) {
                    Ok(i) | Err(i) => i,
                };
                return self.starts[i] + idx.min(line.len());
            }
            top += h;
        }
        0
    }
}

type SharedLaid = Rc<RefCell<Option<Laid>>>;

// ---------------------------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------------------------

/// Navigation keys forwarded while [`ChatField::set_capture_nav`] is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Tab,
}

/// Emitted by the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    /// The text changed by typing, paste or an IME ([`ChatField::set_text`] does not emit).
    Changed,
    /// `Enter` (or `Ctrl+Enter`).
    Submit,
    /// `Esc`.
    Cancel,
    Nav(Nav),
}

pub struct ChatField {
    theme: Theme,
    focus: FocusHandle,
    buf: TextBuf,
    placeholder: SharedString,
    multiline: bool,
    min_lines: usize,
    max_lines: usize,
    laid: SharedLaid,
    scroll: Rc<Cell<Pixels>>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    capture_nav: bool,
    font_size: Option<f32>,
}

impl EventEmitter<FieldEvent> for ChatField {}

impl Focusable for ChatField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChatField {
    pub fn new(theme: Theme, placeholder: impl Into<SharedString>, multiline: bool, cx: &mut Context<Self>) -> Self {
        Self {
            theme,
            focus: cx.focus_handle(),
            buf: TextBuf::default(),
            placeholder: placeholder.into(),
            multiline,
            min_lines: 1,
            max_lines: if multiline { 10 } else { 1 },
            laid: Rc::new(RefCell::new(None)),
            scroll: Rc::new(Cell::new(px(0.0))),
            last_bounds: None,
            selecting: false,
            capture_nav: false,
            font_size: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.buf.text
    }

    pub fn is_empty(&self) -> bool {
        self.buf.text.trim().is_empty()
    }

    /// Byte offset of the caret.
    pub fn caret(&self) -> usize {
        self.buf.cursor()
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>) {
        self.placeholder = text.into();
    }

    pub fn set_lines(&mut self, min: usize, max: usize) {
        self.min_lines = min.max(1);
        self.max_lines = max.max(self.min_lines);
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = Some(size);
    }

    /// While on, `Up` / `Down` / `Tab` are emitted as [`FieldEvent::Nav`] instead of acting.
    pub fn set_capture_nav(&mut self, on: bool) {
        self.capture_nav = on;
    }

    /// Replaces the text (caret at the end) without emitting `Changed`.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = self.clean(text);
        if self.buf.text != text {
            let len = text.len();
            self.buf = TextBuf { text, ..TextBuf::default() };
            self.buf.move_to(len);
        }
        cx.notify();
    }

    /// The text, leaving the field empty (no event).
    pub fn take_text(&mut self, cx: &mut Context<Self>) -> String {
        let t = std::mem::take(&mut self.buf.text);
        self.buf = TextBuf::default();
        self.scroll.set(px(0.0));
        cx.notify();
        t
    }

    /// Replaces `range` with `with` and puts the caret after it (emits `Changed`).
    pub fn replace_range(&mut self, range: Range<usize>, with: &str, cx: &mut Context<Self>) {
        let end = range.end.min(self.buf.text.len());
        let start = range.start.min(end);
        self.buf.replace(Some(start..end), with);
        self.changed(cx);
    }

    /// Types `text` at the caret (emits `Changed`).
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let t = self.clean(text);
        self.buf.replace(None, &t);
        self.changed(cx);
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn clean(&self, text: &str) -> String {
        let t = normalize_newlines(text);
        if self.multiline { t } else { t.replace('\n', " ") }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Changed);
        cx.notify();
    }

    fn index_at(&self, p: Point<Pixels>) -> usize {
        match (&self.last_bounds, self.laid.borrow().as_ref()) {
            (Some(b), Some(laid)) => laid.index_for_position(point(p.x - b.left(), p.y - b.top() + self.scroll.get())),
            _ => self.buf.text.len(),
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
                laid.index_for_position(point(here.x, y))
            }
        };
        if select {
            self.buf.select_to(target);
        } else {
            self.buf.move_to(target);
        }
        cx.notify();
    }

    fn on_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.left();
        cx.notify();
    }
    fn on_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.right();
        cx.notify();
    }
    fn on_up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(FieldEvent::Nav(Nav::Up));
        } else {
            self.vertical(-1, false, cx);
        }
    }
    fn on_down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(FieldEvent::Nav(Nav::Down));
        } else {
            self.vertical(1, false, cx);
        }
    }
    fn on_tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(FieldEvent::Nav(Nav::Tab));
        }
    }
    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.select_to(self.buf.prev_boundary(self.buf.cursor()));
        cx.notify();
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.select_to(self.buf.next_boundary(self.buf.cursor()));
        cx.notify();
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.select_all();
        cx.notify();
    }
    fn on_home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.move_to(self.buf.line_start(self.buf.cursor()));
        cx.notify();
    }
    fn on_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.move_to(self.buf.line_end(self.buf.cursor()));
        cx.notify();
    }
    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.backspace();
        self.changed(cx);
    }
    fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.delete();
        self.changed(cx);
    }
    fn on_delete_word(&mut self, _: &DeleteWord, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.delete_word();
        self.changed(cx);
    }
    fn on_newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.buf.replace(None, "\n");
            self.changed(cx);
        }
    }
    fn on_submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Submit);
    }
    fn on_cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Cancel);
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            let t = self.clean(&text);
            self.buf.replace(None, &t);
            self.changed(cx);
        }
    }
    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.sel.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buf.selected_text().to_string()));
        }
    }
    fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.sel.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buf.selected_text().to_string()));
            self.buf.replace(None, "");
            self.changed(cx);
        }
    }
    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.selecting = true;
        let i = self.index_at(e.position);
        if e.modifiers.shift {
            self.buf.select_to(i);
        } else {
            self.buf.move_to(i);
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
        let (Some(b), Some(laid)) = (self.last_bounds, self.laid.borrow().clone()) else { return };
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

impl EntityInputHandler for ChatField {
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

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.buf.range_to_utf16(&self.buf.sel), reversed: self.buf.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buf.marked.as_ref().map(|r| self.buf.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buf.marked = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let r = range_utf16.map(|r| self.buf.range_from_utf16(&r));
        let t = self.clean(new_text);
        self.buf.replace(r, &t);
        self.changed(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = self.clean(new_text);
        let range = range_utf16
            .map(|r| self.buf.range_from_utf16(&r))
            .or_else(|| self.buf.marked.clone())
            .unwrap_or_else(|| self.buf.sel.clone());
        self.buf.text.replace_range(range.clone(), &new_text);
        self.buf.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.buf.sel = new_selected_utf16
            .map(|r| self.buf.range_from_utf16(&r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.buf.reversed = false;
        self.changed(cx);
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let laid = self.laid.borrow();
        let laid = laid.as_ref()?;
        let r = self.buf.range_from_utf16(&range_utf16);
        let s = self.scroll.get();
        let (a, b) = (laid.position_for_index(r.start), laid.position_for_index(r.end));
        Some(Bounds::from_corners(
            point(bounds.left() + a.x, bounds.top() + a.y - s),
            point(bounds.left() + b.x.max(a.x + px(1.0)), bounds.top() + b.y - s + laid.line_h),
        ))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let b = self.last_bounds?;
        let laid = self.laid.borrow();
        let laid = laid.as_ref()?;
        let utf8 = laid.index_for_position(point(p.x - b.left(), p.y - b.top() + self.scroll.get())).min(self.buf.text.len());
        Some(self.buf.offset_to_utf16(utf8))
    }
}

// ---------------------------------------------------------------------------------------------
// Element
// ---------------------------------------------------------------------------------------------

struct TextElement {
    input: Entity<ChatField>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let c = input.theme.colors;
        let style = window.text_style();
        let empty = input.buf.text.is_empty();
        let shown: SharedString = if empty { input.placeholder.clone() } else { input.buf.text.clone().into() };
        let color = if empty { c.muted } else { c.text };
        let base = TextRun {
            len: shown.len(),
            font: style.font(),
            color: color.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match (&input.buf.marked, empty) {
            (Some(m), false) => [
                TextRun { len: m.start, ..base.clone() },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle { color: Some(base.color), thickness: px(1.0), wavy: false }),
                    ..base.clone()
                },
                TextRun { len: shown.len() - m.end, ..base },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![base],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_h = window.line_height();
        let min_h = line_h * input.min_lines as f32;
        let max_h = line_h * input.max_lines as f32;
        let laid = input.laid.clone();
        let scroll = input.scroll.clone();
        let caret = input.buf.cursor();

        let measure = move |known: gpui::Size<Option<Pixels>>, avail: gpui::Size<AvailableSpace>, window: &mut Window, _: &mut App| {
            let width = known.width.or(match avail.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let shaped = window
                .text_system()
                .shape_text(shown.clone(), font_size, &runs, width, None)
                .map(|l| l.into_vec())
                .unwrap_or_default();
            let mut starts = Vec::with_capacity(shaped.len());
            let mut at = 0usize;
            for l in &shaped {
                starts.push(at);
                at += l.len() + 1;
            }
            let l = Laid { lines: shaped, starts, line_h, placeholder: empty };
            let total = l.total_height();
            let h = total.max(min_h).min(max_h);
            // Keep the caret inside the visible part.
            let mut s = scroll.get();
            let cy = l.position_for_index(caret).y;
            if cy < s {
                s = cy;
            }
            if cy + line_h > s + h {
                s = cy + line_h - h;
            }
            scroll.set(s.max(px(0.0)).min((total - h).max(px(0.0))));
            let w = width.unwrap_or_else(|| l.lines.iter().map(|x| x.width()).fold(px(0.0), |a, b| a.max(b)));
            *laid.borrow_mut() = Some(l);
            size(w, h)
        };
        let mut layout_style = Style::default();
        layout_style.size.width = gpui::relative(1.0).into();
        (window.request_measured_layout(layout_style, measure), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, theme, sel, caret, laid_rc, scroll) = {
            let i = self.input.read(cx);
            (i.focus.clone(), i.theme, i.buf.sel.clone(), i.buf.cursor(), i.laid.clone(), i.scroll.get())
        };
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let c = theme.colors;
        let laid = laid_rc.borrow().clone();
        if let Some(laid) = &laid {
            let top = bounds.top() - scroll;
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if !sel.is_empty() && !laid.placeholder {
                    let mut tint = c.accent;
                    tint.a = 0.3;
                    let width = bounds.size.width;
                    for (i, line) in laid.lines.iter().enumerate() {
                        let (ls, le) = (laid.starts[i], laid.starts[i] + line.len());
                        if sel.end < ls || sel.start > le {
                            continue;
                        }
                        let a = laid.position_for_index(sel.start.max(ls));
                        let b = laid.position_for_index(sel.end.min(le));
                        let row = |x0: Pixels, x1: Pixels, y: Pixels| {
                            fill(Bounds::new(point(bounds.left() + x0, top + y), size((x1 - x0).max(px(2.0)), laid.line_h)), tint)
                        };
                        if a.y == b.y {
                            window.paint_quad(row(a.x, b.x, a.y));
                        } else {
                            window.paint_quad(row(a.x, width, a.y));
                            let mut y = a.y + laid.line_h;
                            while y < b.y {
                                window.paint_quad(row(px(0.0), width, y));
                                y += laid.line_h;
                            }
                            window.paint_quad(row(px(0.0), b.x, b.y));
                        }
                    }
                }
                let mut y = px(0.0);
                for (i, line) in laid.lines.iter().enumerate() {
                    let _ = line.paint(point(bounds.left(), top + y), laid.line_h, TextAlign::Left, Some(bounds), window, cx);
                    y += laid.height_of(i);
                }
                if sel.is_empty() && focus.is_focused(window) {
                    let p = laid.position_for_index(caret);
                    window.paint_quad(fill(Bounds::new(point(bounds.left() + p.x, top + p.y), size(px(1.5), laid.line_h)), c.accent));
                }
            });
        }
        self.input.update(cx, |input, _| input.last_bounds = Some(bounds));
    }
}

impl Render for ChatField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_left))
            .on_action(cx.listener(Self::on_right))
            .on_action(cx.listener(Self::on_up))
            .on_action(cx.listener(Self::on_down))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_delete_word))
            .on_action(cx.listener(Self::on_newline))
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .w_full()
            .font_family(ty.ui_family)
            .text_size(px(self.font_size.unwrap_or(14.0)))
            .text_color(c.text)
            .line_height(px(21.0))
            .child(TextElement { input: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str, caret: usize) -> TextBuf {
        let mut b = TextBuf { text: text.to_string(), ..TextBuf::default() };
        b.move_to(caret);
        b
    }

    #[test]
    fn newline_and_selection_editing() {
        let mut b = buf("hola mundo", 4);
        b.replace(None, "\n");
        assert_eq!(b.text, "hola\n mundo");
        assert_eq!(b.cursor(), 5);
        b.select_all();
        assert_eq!(b.selected_text(), "hola\n mundo");
        b.replace(None, "x");
        assert_eq!(b.text, "x");
    }

    #[test]
    fn backspace_joins_lines_and_respects_chars() {
        let mut b = buf("a\nñ", 4);
        b.backspace();
        assert_eq!(b.text, "a\n");
        b.backspace();
        assert_eq!(b.text, "a");
    }

    #[test]
    fn delete_word_stops_at_whitespace() {
        let mut b = buf("uno dos tres", 12);
        b.delete_word();
        assert_eq!(b.text, "uno dos ");
    }

    #[test]
    fn utf16_round_trip() {
        let b = buf("a😀b", 0);
        let u = b.offset_to_utf16(5);
        assert_eq!(u, 3);
        assert_eq!(b.offset_from_utf16(u), 5);
    }
}
