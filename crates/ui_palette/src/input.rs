//! A minimal single-line text input (ui_kit has none): used for the preset name.
//!
//! The editing model is [`TextBuf`] (pure, tested); [`TextField`] is the gpui entity that
//! paints it and implements `EntityInputHandler` so the IME and the keyboard layout work.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Render, ShapedLine, SharedString, Style, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative, size,
};
use theme::Theme;

actions!(
    palette_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        DeleteWord,
    ]
);

/// Key context of the field (its bindings live in [`bind_keys`]).
pub const CONTEXT: &str = "PaletteInput";

pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("left", Left, Some(CONTEXT)),
        KeyBinding::new("right", Right, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-x", Cut, Some(CONTEXT)),
        KeyBinding::new("home", Home, Some(CONTEXT)),
        KeyBinding::new("end", End, Some(CONTEXT)),
        KeyBinding::new("ctrl-backspace", DeleteWord, Some(CONTEXT)),
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
        if self.reversed {
            self.sel.start
        } else {
            self.sel.end
        }
    }

    pub fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(self.text.len(), |c| offset + c.len_utf8())
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
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.sel.clone());
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

// ---------------------------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------------------------

/// Emitted by the field.
pub enum TextFieldEvent {
    /// The text changed (typing, paste, IME commit, [`TextField::set_text`] does not emit).
    Changed,
}

pub struct TextField {
    theme: Theme,
    focus: FocusHandle,
    buf: TextBuf,
    placeholder: SharedString,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    /// Choice fields: keys pass through to the palette (Left / Right cycle the options).
    readonly: bool,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    pub fn new(theme: Theme, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            theme,
            focus: cx.focus_handle(),
            buf: TextBuf::default(),
            placeholder: placeholder.into(),
            last_layout: None,
            last_bounds: None,
            selecting: false,
            readonly: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.buf.text
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Replaces the text (caret at the end) without emitting `Changed`.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.buf.text != text {
            self.buf = TextBuf { text: text.to_string(), ..TextBuf::default() };
            self.buf.move_to(text.len());
            cx.notify();
        }
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    pub fn set_readonly(&mut self, readonly: bool, cx: &mut Context<Self>) {
        if self.readonly != readonly {
            self.readonly = readonly;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn index_for_x(&self, p: Point<Pixels>) -> usize {
        match (&self.last_bounds, &self.last_layout) {
            (Some(b), Some(line)) if !self.buf.text.is_empty() => {
                line.closest_index_for_x(p.x - b.left())
            }
            _ => 0,
        }
    }

    fn on_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            cx.propagate();
            return;
        }
        self.buf.left();
        cx.notify();
    }
    fn on_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            cx.propagate();
            return;
        }
        self.buf.right();
        cx.notify();
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
        self.buf.move_to(0);
        cx.notify();
    }
    fn on_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.buf.move_to(self.buf.text.len());
        cx.notify();
    }
    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        self.buf.backspace();
        self.changed(cx);
    }
    fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        self.buf.delete();
        self.changed(cx);
    }
    fn on_delete_word(&mut self, _: &DeleteWord, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        if self.buf.sel.is_empty() {
            let caret = self.buf.cursor();
            let head = &self.buf.text[..caret];
            let trimmed = head.trim_end();
            let start = trimmed.rfind(|c: char| c.is_whitespace()).map_or(0, |i| i + 1);
            self.buf.move_to(caret);
            self.buf.select_to(start);
        }
        self.buf.replace(None, "");
        self.changed(cx);
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.buf.replace(None, &text.replace(['\n', '\r'], " "));
            self.changed(cx);
        }
    }
    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.sel.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buf.selected_text().to_string()));
        }
    }
    fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buf.sel.is_empty() && !self.readonly {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buf.selected_text().to_string()));
            self.buf.replace(None, "");
            self.changed(cx);
        }
    }
    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.selecting = true;
        let i = self.index_for_x(e.position);
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
            self.buf.select_to(self.index_for_x(e.position));
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
        if self.readonly {
            return;
        }
        let r = range_utf16.map(|r| self.buf.range_from_utf16(&r));
        self.buf.replace(r, &new_text.replace(['\n', '\r'], " "));
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
        if self.readonly {
            return;
        }
        let range = range_utf16
            .map(|r| self.buf.range_from_utf16(&r))
            .or_else(|| self.buf.marked.clone())
            .unwrap_or_else(|| self.buf.sel.clone());
        self.buf.text.replace_range(range.clone(), new_text);
        self.buf.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.buf.sel = new_selected_utf16
            .map(|r| self.buf.range_from_utf16(&r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.buf.reversed = false;
        self.changed(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let r = self.buf.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(r.start), bounds.top()),
            point(bounds.left() + line.x_for_index(r.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&p)?;
        let line = self.last_layout.as_ref()?;
        let utf8 = line.index_for_x(p.x - local.x)?;
        Some(self.buf.offset_to_utf16(utf8.min(self.buf.text.len())))
    }
}

/// The element that shapes and paints the line, registers the input handler and the caret.
struct TextElement {
    input: Entity<TextField>,
}

struct Prepaint {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let c = input.theme.colors;
        let style = window.text_style();
        let empty = input.buf.text.is_empty();
        let shown: SharedString = if empty {
            input.placeholder.clone()
        } else {
            input.buf.text.clone().into()
        };
        let color = if empty { c.muted } else { c.text };
        let run = TextRun {
            len: shown.len(),
            font: style.font(),
            color: color.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match (&input.buf.marked, empty) {
            (Some(m), false) => [
                TextRun {
                    len: m.start,
                    ..run.clone()
                },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: shown.len() - m.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(shown, font_size, &runs, None);
        let (sel, cursor) = (input.buf.sel.clone(), input.buf.cursor());
        let (selection, cursor) = if sel.is_empty() {
            let x = if empty {
                px(0.0)
            } else {
                line.x_for_index(cursor)
            };
            let quad = fill(
                Bounds::new(
                    point(bounds.left() + x, bounds.top()),
                    size(px(1.5), bounds.size.height),
                ),
                c.accent,
            );
            (None, Some(quad))
        } else {
            let mut sel_color = c.accent;
            sel_color.a = 0.3;
            let quad = fill(
                Bounds::from_corners(
                    point(bounds.left() + line.x_for_index(sel.start), bounds.top()),
                    point(bounds.left() + line.x_for_index(sel.end), bounds.bottom()),
                ),
                sel_color,
            );
            (Some(quad), None)
        };
        Prepaint {
            line,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(sel) = pre.selection.take() {
            window.paint_quad(sel);
        }
        let _ = pre
            .line
            .paint(bounds.origin, window.line_height(), window, cx);
        if focus.is_focused(window)
            && let Some(cursor) = pre.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let line = pre.line.clone();
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextField {
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
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_delete_word))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .h(px(28.0))
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .items_center()
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_lg))
            .text_color(c.text)
            .line_height(px(22.0))
            .child(div().w_full().child(TextElement { input: cx.entity() }))
    }
}

