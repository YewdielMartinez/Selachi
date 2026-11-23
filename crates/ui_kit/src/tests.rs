//! Keyboard contract of the interactive components: Tab reaches them in order, Enter / Space
//! activate, opt-outs and disabled ones are skipped, a press never steals focus, `Select` and
//! the other inputs behave from the keyboard.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{AnyElement, Context, Entity, IntoElement, Render, Subscription, TestAppContext, VisualTestContext, Window, div, prelude::*, px};
use theme::Theme;

use crate::testing::{self, press};
use crate::*;

type BuildFn = Box<dyn Fn(&mut Window, &mut Context<Probe>) -> AnyElement>;

/// A root view that builds its content from a closure on every render.
struct Probe {
    build: BuildFn,
    _subs: Vec<Subscription>,
}

impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(400.0)).p(px(10.0)).child((self.build)(window, cx))
    }
}

fn probe(
    cx: &mut TestAppContext,
    build: impl Fn(&mut Window, &mut Context<Probe>) -> AnyElement + 'static,
) -> (Entity<Probe>, &mut VisualTestContext) {
    testing::open_window(cx, move |_, _| Probe { build: Box::new(build), _subs: Vec::new() })
}

fn counter() -> Rc<Cell<u32>> {
    Rc::new(Cell::new(0))
}

fn column(children: Vec<AnyElement>) -> AnyElement {
    div().flex().flex_col().gap(px(8.0)).children(children).into_any_element()
}

fn bump(c: &Rc<Cell<u32>>) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static {
    let c = c.clone();
    move |_, window, _| {
        c.set(c.get() + 1);
        window.refresh();
    }
}

#[gpui::test]
fn tab_then_enter_activates_a_button(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let hits = counter();
    let h = hits.clone();
    let (_v, vcx) = probe(cx, move |_, _| Button::tonal("save", &t, "Guardar").on_click(bump(&h)).into_any_element());
    press(vcx, "tab enter");
    assert_eq!(hits.get(), 1, "Tab focused the button and Enter clicked it");
    press(vcx, "space");
    assert_eq!(hits.get(), 2, "Space too");
}

#[gpui::test]
fn every_clickable_component_is_a_tab_stop_in_order(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let log: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let l = log.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let hit = |name: &'static str| {
            let l = l.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App| l.borrow_mut().push(name)
        };
        let l2 = l.clone();
        let l3 = l.clone();
        column(vec![
            Button::primary("b-button", &t, "Button").on_click(hit("button")).into_any_element(),
            Button::action("b-action", &t, "Action").on_click(hit("action")).into_any_element(),
            IconButton::new("b-icon", &t, Icon::Plus).on_click(hit("icon")).into_any_element(),
            GhostButton::new("b-ghost", &t, "Ghost").on_click(hit("ghost")).into_any_element(),
            Pill::new("b-pill", &t).label("Pill").on_click(hit("pill")).into_any_element(),
            Toggle::new("b-toggle", &t).on_change(move |_, _, _| l2.borrow_mut().push("toggle")).into_any_element(),
            ListRow::new("b-row", &t).title("Row").on_click(hit("row")).into_any_element(),
            SegmentedControl::new("b-seg", &t, ["A", "B"])
                .on_change(move |ix, _, _| l3.borrow_mut().push(if ix == 0 { "seg-a" } else { "seg-b" }))
                .into_any_element(),
        ])
    });
    press(vcx, "tab enter tab enter tab enter tab enter tab enter tab space tab enter tab enter tab enter");
    assert_eq!(*log.borrow(), ["button", "action", "icon", "ghost", "pill", "toggle", "row", "seg-a", "seg-b"]);
    // Shift+Tab walks back.
    log.borrow_mut().clear();
    press(vcx, "shift-tab enter");
    assert_eq!(*log.borrow(), ["seg-a"]);
}

#[gpui::test]
fn opt_out_disabled_and_inert_are_skipped(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let log: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let l = log.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let hit = |name: &'static str| {
            let l = l.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App| l.borrow_mut().push(name)
        };
        column(vec![
            Button::tonal("c-decor", &t, "Decorativo").focusable(false).on_click(hit("decor")).into_any_element(),
            Button::tonal("c-off", &t, "Apagado").disabled(true).on_click(hit("off")).into_any_element(),
            Pill::new("c-label", &t).label("Solo etiqueta").into_any_element(),
            IconButton::new("c-icon", &t, Icon::Plus).on_click(hit("icon-off")).focusable(false).into_any_element(),
            Button::tonal("c-real", &t, "Real").on_click(hit("real")).into_any_element(),
        ])
    });
    press(vcx, "tab enter");
    assert_eq!(*log.borrow(), ["real"], "the first tab stop is the only real one");
    press(vcx, "tab enter");
    assert_eq!(*log.borrow(), ["real", "real"], "and Tab wraps back to it");
}

#[gpui::test]
fn a_press_does_not_steal_focus_but_still_clicks(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let hits = counter();
    let h = hits.clone();
    let (_v, vcx) = probe(cx, move |_, _| Button::tonal("press-me", &t, "Pulsar").on_click(bump(&h)).into_any_element());
    testing::click_id(vcx, "press-me");
    assert_eq!(hits.get(), 1);
    // Nothing is focused after a mouse click (a toolbar must not take focus from the terminal):
    // Tab therefore reaches the button for the first time.
    press(vcx, "tab enter");
    assert_eq!(hits.get(), 2);
}

#[gpui::test]
fn toggle_flips_with_space_and_reports_the_new_value(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let on = Rc::new(Cell::new(false));
    let o = on.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let o2 = o.clone();
        Toggle::new("tg", &t).on(o.get()).on_change(move |v, w, _| {
            o2.set(v);
            w.refresh();
        })
        .into_any_element()
    });
    press(vcx, "tab space");
    assert!(on.get());
    press(vcx, "space");
    assert!(!on.get());
}

#[gpui::test]
fn slider_moves_with_the_arrow_keys(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let v = Rc::new(Cell::new(50.0f32));
    let vv = v.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let v2 = vv.clone();
        Slider::new("sl", &t).range(0.0, 100.0).step(5.0).value(vv.get()).on_change(move |x, w, _| {
            v2.set(x);
            w.refresh();
        })
        .into_any_element()
    });
    press(vcx, "tab right right");
    assert_eq!(v.get(), 60.0);
    press(vcx, "left pagedown");
    assert_eq!(v.get(), 5.0, "left = one step (55), page down = a tenth of the range more (clamped to 55 - 50)");
    press(vcx, "end");
    assert_eq!(v.get(), 100.0);
    press(vcx, "home");
    assert_eq!(v.get(), 0.0);
}

#[gpui::test]
fn checkbox_and_radio_group_from_the_keyboard(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let checked = Rc::new(Cell::new(false));
    let picked = Rc::new(Cell::new(0usize));
    let (c, p) = (checked.clone(), picked.clone());
    let (_v, vcx) = probe(cx, move |_, _| {
        let (c2, p2) = (c.clone(), p.clone());
        column(vec![
            Checkbox::new("ck", &t).label("Acepto").checked(c.get()).on_change(move |v, w, _| {
                c2.set(v);
                w.refresh();
            })
            .into_any_element(),
            RadioGroup::new("rg", &t, ["Uno", "Dos", "Tres"]).selected(Some(p.get())).on_change(move |ix, w, _| {
                p2.set(ix);
                w.refresh();
            })
            .into_any_element(),
        ])
    });
    press(vcx, "tab space");
    assert!(checked.get());
    press(vcx, "space");
    assert!(!checked.get());
    press(vcx, "tab tab enter");
    assert_eq!(picked.get(), 1, "second radio");
    press(vcx, "tab space");
    assert_eq!(picked.get(), 2);
}

// ----- Select ------------------------------------------------------------------------------

struct SelectHost {
    select: Entity<Select>,
    _sub: Subscription,
}

impl Render for SelectHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(400.0)).h(px(500.0)).p(px(10.0)).child(self.select.clone())
    }
}

fn select_host<'a>(
    cx: &'a mut TestAppContext,
    n: usize,
    initial: &'static str,
) -> (Entity<Select>, &'a mut VisualTestContext, Rc<RefCell<Vec<usize>>>) {
    let events: Rc<RefCell<Vec<usize>>> = Rc::default();
    let ev = events.clone();
    let (view, vcx) = testing::open_window(cx, move |window, cx| {
        let select = cx.new(|cx| {
            Select::new("sel", Theme::dark(), cx)
                .options((0..n).map(|i| {
                    let o = SelectOption::new(format!("v{i}"), format!("Opción {i}"));
                    if i == 2 { o.disabled(true) } else { o }
                }))
                .with_value(initial)
                .max_w(200.0)
        });
        select.read(cx).focus(window);
        let sub = cx.subscribe(&select, move |_: &mut SelectHost, _, e: &SelectEvent, _| {
            let SelectEvent::Changed(i) = e;
            ev.borrow_mut().push(*i);
        });
        SelectHost { select, _sub: sub }
    });
    let select = view.read_with(vcx, |h, _| h.select.clone());
    (select, vcx, events)
}

#[gpui::test]
fn select_opens_navigates_skips_disabled_and_confirms(cx: &mut TestAppContext) {
    let (s, vcx, ev) = select_host(cx, 6, "v0");
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    press(vcx, "down");
    assert!(s.read_with(vcx, |s, _| s.is_open()), "Down opens on the current value");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(0));
    press(vcx, "down");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(1));
    press(vcx, "down");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(3), "index 2 is disabled");
    press(vcx, "up up up");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(0), "clamped at the first");
    press(vcx, "end");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(5));
    press(vcx, "enter");
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    assert_eq!(s.read_with(vcx, |s, _| s.value().cloned()), Some("v5".into()));
    assert_eq!(*ev.borrow(), vec![5]);
    // Re-picking the same value is not a change.
    press(vcx, "enter enter");
    assert_eq!(*ev.borrow(), vec![5]);
}

#[gpui::test]
fn select_escape_closes_and_keeps_the_value(cx: &mut TestAppContext) {
    let (s, vcx, ev) = select_host(cx, 4, "v1");
    press(vcx, "space");
    assert!(s.read_with(vcx, |s, _| s.is_open()));
    press(vcx, "down escape");
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    assert_eq!(s.read_with(vcx, |s, _| s.value().cloned()), Some("v1".into()));
    assert!(ev.borrow().is_empty());
}

#[gpui::test]
fn select_type_ahead_jumps_to_a_match(cx: &mut TestAppContext) {
    let events: Rc<RefCell<Vec<usize>>> = Rc::default();
    let ev = events.clone();
    let (view, vcx) = testing::open_window(cx, move |window, cx| {
        let select = cx.new(|cx| {
            Select::new("ta", Theme::dark(), cx).options(
                ["Alpha", "Beta", "Bravo", "Charlie"].map(|n| SelectOption::new(n.to_lowercase(), n)),
            )
        });
        select.read(cx).focus(window);
        let sub = cx.subscribe(&select, move |_: &mut SelectHost, _, e: &SelectEvent, _| {
            let SelectEvent::Changed(i) = e;
            ev.borrow_mut().push(*i);
        });
        SelectHost { select, _sub: sub }
    });
    let s = view.read_with(vcx, |h, _| h.select.clone());
    // Closed: typing picks directly, like a native select.
    press(vcx, "c");
    assert_eq!(s.read_with(vcx, |s, _| s.value().cloned()), Some("charlie".into()));
    press(vcx, "b");
    assert_eq!(s.read_with(vcx, |s, _| s.value().cloned()), Some("beta".into()));
    assert_eq!(*events.borrow(), vec![3, 1]);
}

#[gpui::test]
fn select_with_many_options_is_virtualized_and_reaches_the_end(cx: &mut TestAppContext) {
    let (s, vcx, ev) = select_host(cx, 500, "v0");
    press(vcx, "down end");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(499));
    press(vcx, "enter");
    assert_eq!(*ev.borrow(), vec![499]);
    assert!(s.read_with(vcx, |s, _| s.options_len()) > select::VIRTUALIZE_ABOVE);
}

#[gpui::test]
fn select_mouse_opens_and_picks_by_value_id(cx: &mut TestAppContext) {
    let (s, vcx, ev) = select_host(cx, 5, "v0");
    testing::click_id(vcx, "sel-trigger");
    assert!(s.read_with(vcx, |s, _| s.is_open()), "a press on the trigger opens it");
    testing::click_id(vcx, "sel-v3");
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    assert_eq!(s.read_with(vcx, |s, _| s.value().cloned()), Some("v3".into()));
    assert_eq!(*ev.borrow(), vec![3]);
    // A disabled option (index 2) ignores the click.
    testing::click_id(vcx, "sel-trigger");
    testing::click_id(vcx, "sel-v2");
    assert!(s.read_with(vcx, |s, _| s.is_open()), "still open");
    assert_eq!(*ev.borrow(), vec![3]);
}

#[gpui::test]
fn select_set_options_keeps_the_selection_by_value(cx: &mut TestAppContext) {
    let (s, vcx, _) = select_host(cx, 4, "v3");
    s.update(vcx, |s, cx| {
        s.set_options([SelectOption::new("v9", "Nueva"), SelectOption::new("v3", "Tres")], cx);
    });
    assert_eq!(s.read_with(vcx, |s, _| s.selected_index()), Some(1), "v3 moved to index 1");
    s.update(vcx, |s, cx| s.set_options([SelectOption::new("x", "X")], cx));
    assert_eq!(s.read_with(vcx, |s, _| s.selected_index()), None);
}
