//! Behavioural tests of the text field through a real (test) window: typing goes through the
//! input handler exactly like the keyboard layout / IME would, keys through the keymap.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    ClipboardItem, Context, Entity, EntityInputHandler, IntoElement, Render, Subscription, TestAppContext,
    VisualTestContext, Window, div, px,
};
use theme::Theme;

use super::*;
use crate::testing::{self, press, type_text};

type Events = Rc<RefCell<Vec<TextFieldEvent>>>;
type Builder = Box<dyn FnOnce(&mut Context<TextField>) -> TextField>;

struct Host {
    fields: Vec<Entity<TextField>>,
    _subs: Vec<Subscription>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(300.0)).p(px(10.0)).flex().flex_col().gap(px(8.0)).children(self.fields.clone())
    }
}

/// A window with one field per builder, the first one focused; events of the first are recorded.
fn host(
    cx: &mut TestAppContext,
    builders: Vec<Builder>,
) -> (Vec<Entity<TextField>>, &mut VisualTestContext, Events) {
    let events: Events = Rc::default();
    let ev = events.clone();
    let (view, vcx) = testing::open_window(cx, move |window, cx| {
        let fields: Vec<_> = builders.into_iter().map(|b| cx.new(b)).collect();
        let subs = fields
            .first()
            .map(|f| cx.subscribe(f, move |_: &mut Host, _, e: &TextFieldEvent, _| ev.borrow_mut().push(*e)))
            .into_iter()
            .collect();
        if let Some(f) = fields.first() {
            f.read(cx).focus(window);
        }
        Host { fields, _subs: subs }
    });
    let fields = view.read_with(vcx, |h, _| h.fields.clone());
    (fields, vcx, events)
}

fn single(cx: &mut TestAppContext) -> (Entity<TextField>, &mut VisualTestContext, Events) {
    let (mut f, vcx, ev) = host(cx, vec![Box::new(|cx| TextField::new(Theme::dark(), cx))]);
    (f.remove(0), vcx, ev)
}

fn area(cx: &mut TestAppContext, mode: EnterMode) -> (Entity<TextField>, &mut VisualTestContext, Events) {
    let (mut f, vcx, ev) = host(cx, vec![Box::new(move |cx| TextField::area(Theme::dark(), cx).enter_mode(mode))]);
    (f.remove(0), vcx, ev)
}

fn text(f: &Entity<TextField>, vcx: &VisualTestContext) -> String {
    f.read_with(vcx, |f, _| f.text().to_string())
}

fn clipboard(vcx: &mut VisualTestContext) -> Option<String> {
    vcx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()))
}

#[gpui::test]
fn typing_goes_through_the_input_handler_and_emits_changed(cx: &mut TestAppContext) {
    let (f, vcx, ev) = single(cx);
    type_text(vcx, "hola");
    assert_eq!(text(&f, vcx), "hola");
    assert_eq!(ev.borrow().iter().filter(|e| **e == TextFieldEvent::Changed).count(), 4, "one event per char");
    press(vcx, "backspace");
    assert_eq!(text(&f, vcx), "hol");
    // Accented chars are one backspace each.
    type_text(vcx, "ñ");
    press(vcx, "backspace");
    assert_eq!(text(&f, vcx), "hol");
}

#[gpui::test]
fn selection_by_keyboard_is_replaced_by_typing(cx: &mut TestAppContext) {
    let (f, vcx, _) = single(cx);
    type_text(vcx, "hello world");
    press(vcx, "shift-left shift-left shift-left shift-left shift-left");
    assert_eq!(f.read_with(vcx, |f, _| f.selection()), 6..11);
    type_text(vcx, "there");
    assert_eq!(text(&f, vcx), "hello there");
    press(vcx, "ctrl-a");
    assert_eq!(f.read_with(vcx, |f, _| f.selection()), 0..11);
    type_text(vcx, "z");
    assert_eq!(text(&f, vcx), "z");
    // Home / End and shift-home.
    type_text(vcx, "yx");
    press(vcx, "home");
    assert_eq!(f.read_with(vcx, |f, _| f.caret()), 0);
    press(vcx, "shift-end");
    assert_eq!(f.read_with(vcx, |f, _| f.selection()), 0..3);
}

#[gpui::test]
fn word_motion_and_word_delete(cx: &mut TestAppContext) {
    let (f, vcx, _) = single(cx);
    type_text(vcx, "uno dos tres");
    press(vcx, "ctrl-left");
    assert_eq!(f.read_with(vcx, |f, _| f.caret()), 8);
    press(vcx, "ctrl-shift-left");
    assert_eq!(f.read_with(vcx, |f, _| f.selection()), 4..8);
    press(vcx, "right ctrl-backspace");
    assert_eq!(text(&f, vcx), "uno tres");
    press(vcx, "ctrl-home ctrl-delete");
    assert_eq!(text(&f, vcx), "tres");
}

#[gpui::test]
fn clipboard_copy_cut_paste(cx: &mut TestAppContext) {
    let (f, vcx, ev) = single(cx);
    type_text(vcx, "copiame");
    press(vcx, "ctrl-a ctrl-c");
    assert_eq!(clipboard(vcx).as_deref(), Some("copiame"));
    press(vcx, "ctrl-x");
    assert_eq!(text(&f, vcx), "");
    press(vcx, "ctrl-v ctrl-v");
    assert_eq!(text(&f, vcx), "copiamecopiame");
    // Pasting multi-line text into a single line flattens it.
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("a\r\nb\nc".into())));
    press(vcx, "ctrl-a ctrl-v");
    assert_eq!(text(&f, vcx), "a b c");
    assert!(ev.borrow().contains(&TextFieldEvent::Changed));
}

#[gpui::test]
fn ime_composition_is_replaced_by_the_commit(cx: &mut TestAppContext) {
    let (f, vcx, _) = single(cx);
    type_text(vcx, "ab");
    // The IME opens a composition at the caret...
    f.update_in(vcx, |f, window, cx| f.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx));
    assert_eq!(text(&f, vcx), "abni");
    assert!(f.read_with(vcx, |f, _| f.is_composing()));
    // ...updates it...
    f.update_in(vcx, |f, window, cx| f.replace_and_mark_text_in_range(None, "nih", Some(3..3), window, cx));
    assert_eq!(text(&f, vcx), "abnih");
    // ...and commits the final text over the composition.
    f.update_in(vcx, |f, window, cx| f.replace_text_in_range(None, "你好", window, cx));
    assert_eq!(text(&f, vcx), "ab你好");
    assert!(!f.read_with(vcx, |f, _| f.is_composing()));
    assert_eq!(f.read_with(vcx, |f, _| f.caret()), "ab你好".len());
    // UTF-16 ranges of the handler map back to bytes.
    let sel = f.update_in(vcx, |f, window, cx| f.selected_text_range(false, window, cx));
    assert_eq!(sel.map(|s| s.range), Some(4..4));
}

#[gpui::test]
fn submit_cancel_blur_and_reverse(cx: &mut TestAppContext) {
    let (_f, vcx, ev) = single(cx);
    press(vcx, "enter");
    press(vcx, "ctrl-enter");
    press(vcx, "shift-enter");
    press(vcx, "escape");
    assert_eq!(
        *ev.borrow(),
        vec![TextFieldEvent::Submit, TextFieldEvent::Submit, TextFieldEvent::SubmitReverse, TextFieldEvent::Cancel]
    );
    ev.borrow_mut().clear();
    vcx.update(|window, _| window.blur());
    vcx.run_until_parked();
    assert_eq!(*ev.borrow(), vec![TextFieldEvent::Blur]);
}

#[gpui::test]
fn read_only_selects_and_copies_but_does_not_edit(cx: &mut TestAppContext) {
    let (mut fields, vcx, ev) =
        host(cx, vec![Box::new(|cx| TextField::new(Theme::dark(), cx).with_text("fijo").read_only(true))]);
    let f = fields.remove(0);
    type_text(vcx, "x");
    press(vcx, "backspace ctrl-v");
    assert_eq!(text(&f, vcx), "fijo");
    press(vcx, "ctrl-a ctrl-c");
    assert_eq!(clipboard(vcx).as_deref(), Some("fijo"));
    press(vcx, "ctrl-x");
    assert_eq!(text(&f, vcx), "fijo", "cut is an edit");
    assert!(ev.borrow().is_empty(), "no Changed from a read-only field");
    f.update(vcx, |f, cx| f.set_read_only(false, cx));
    type_text(vcx, "!");
    assert_eq!(text(&f, vcx), "!");
}

#[gpui::test]
fn password_never_reaches_the_clipboard_and_keeps_offsets(cx: &mut TestAppContext) {
    let (mut fields, vcx, _) = host(cx, vec![Box::new(|cx| TextField::new(Theme::dark(), cx).password(true))]);
    let f = fields.remove(0);
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("antes".into())));
    type_text(vcx, "sécret");
    press(vcx, "ctrl-a ctrl-c");
    assert_eq!(clipboard(vcx).as_deref(), Some("antes"), "copy is disabled");
    press(vcx, "ctrl-x");
    assert_eq!(text(&f, vcx), "sécret", "cut is disabled");
    // Display offsets (3-byte bullets) map to buffer offsets (the é is 2 bytes).
    f.update(vcx, |f, _| {
        assert_eq!(f.offset_to_display(0), 0);
        assert_eq!(f.offset_to_display(1), 3);
        assert_eq!(f.offset_to_display(3), 6, "after the 2-byte é: two chars");
        assert_eq!(f.display_to_offset(6), 3);
        assert_eq!(f.display_to_offset(3), 1);
        assert_eq!(f.display_to_offset(999), "sécret".len());
    });
    press(vcx, "backspace");
    assert_eq!(text(&f, vcx), "");
}

#[gpui::test]
fn tab_moves_focus_to_the_next_field(cx: &mut TestAppContext) {
    let (fields, vcx, _) = host(
        cx,
        vec![
            Box::new(|cx| TextField::new(Theme::dark(), cx)),
            Box::new(|cx| TextField::new(Theme::dark(), cx)),
        ],
    );
    let (a, b) = (fields[0].read_with(vcx, |f, _| f.focus.clone()), fields[1].read_with(vcx, |f, _| f.focus.clone()));
    assert!(testing::is_focused(vcx, &a));
    testing::focus_next(vcx);
    assert!(testing::is_focused(vcx, &b), "tab goes to the second field");
    testing::focus_prev(vcx);
    assert!(testing::is_focused(vcx, &a), "shift-tab comes back");
    // While a popover owner captures navigation, Tab is an event instead.
    fields[0].update(vcx, |f, _| f.set_capture_nav(true));
    press(vcx, "tab down up");
    assert!(testing::is_focused(vcx, &a));
}

#[gpui::test]
fn capture_nav_forwards_up_down_tab(cx: &mut TestAppContext) {
    let (f, vcx, ev) = single(cx);
    f.update(vcx, |f, _| f.set_capture_nav(true));
    press(vcx, "up down tab");
    assert_eq!(
        *ev.borrow(),
        vec![TextFieldEvent::Nav(Nav::Up), TextFieldEvent::Nav(Nav::Down), TextFieldEvent::Nav(Nav::Tab)]
    );
}

#[gpui::test]
fn area_enter_submits_and_shift_enter_adds_a_line(cx: &mut TestAppContext) {
    let (f, vcx, ev) = area(cx, EnterMode::Submit);
    type_text(vcx, "hola");
    press(vcx, "shift-enter");
    type_text(vcx, "mundo");
    assert_eq!(text(&f, vcx), "hola\nmundo");
    assert!(!ev.borrow().contains(&TextFieldEvent::Submit), "shift+enter must not submit");
    press(vcx, "enter");
    assert!(ev.borrow().contains(&TextFieldEvent::Submit));
    // Line-wise Home / End.
    press(vcx, "home");
    assert_eq!(f.read_with(vcx, |f, _| f.caret()), 5);
    press(vcx, "end");
    assert_eq!(f.read_with(vcx, |f, _| f.caret()), 10);
}

#[gpui::test]
fn area_in_newline_mode_submits_with_ctrl_enter(cx: &mut TestAppContext) {
    let (f, vcx, ev) = area(cx, EnterMode::Newline);
    type_text(vcx, "nota");
    press(vcx, "enter");
    type_text(vcx, "dos");
    assert_eq!(text(&f, vcx), "nota\ndos");
    assert!(!ev.borrow().contains(&TextFieldEvent::Submit));
    press(vcx, "ctrl-enter");
    assert!(ev.borrow().contains(&TextFieldEvent::Submit));
}

#[gpui::test]
fn area_arrows_cross_lines_and_newlines_are_normalised(cx: &mut TestAppContext) {
    let (f, vcx, _) = area(cx, EnterMode::Submit);
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("uno\r\ndos\rtres".into())));
    press(vcx, "ctrl-v");
    assert_eq!(text(&f, vcx), "uno\ndos\ntres");
    press(vcx, "up");
    let caret = f.read_with(vcx, |f, _| f.caret());
    assert!((4..=7).contains(&caret), "caret moved to the middle line, was {caret}");
    press(vcx, "up");
    assert!(f.read_with(vcx, |f, _| f.caret()) <= 3, "and to the first one");
}

#[gpui::test]
fn programmatic_updates_do_not_emit_and_errors_are_kept(cx: &mut TestAppContext) {
    let (f, vcx, ev) = single(cx);
    f.update(vcx, |f, cx| f.set_text("de afuera", cx));
    assert_eq!(text(&f, vcx), "de afuera");
    assert!(ev.borrow().is_empty());
    f.update(vcx, |f, cx| {
        assert_eq!(f.take_text(cx), "de afuera");
        f.set_error(Some("Nombre obligatorio"), cx);
    });
    assert!(f.read_with(vcx, |f, _| f.error().is_some_and(|e| e == "Nombre obligatorio")));
    f.update(vcx, |f, cx| f.set_error(None::<&str>, cx));
    assert!(f.read_with(vcx, |f, _| f.error().is_none()));
    // Typing still works afterwards.
    type_text(vcx, "ok");
    assert_eq!(text(&f, vcx), "ok");
}
