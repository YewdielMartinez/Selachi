//! Keyboard contract of the interactive components: Tab reaches them in order, Enter / Space
//! activate, opt-outs and disabled ones are skipped, a press never steals focus, `Select` and
//! the other inputs behave from the keyboard.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, Subscription, TestAppContext,
    VisualTestContext, Window, div, prelude::*, px,
};
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
        div()
            .w(px(400.0))
            .p(px(10.0))
            .child((self.build)(window, cx))
    }
}

fn probe(
    cx: &mut TestAppContext,
    build: impl Fn(&mut Window, &mut Context<Probe>) -> AnyElement + 'static,
) -> (Entity<Probe>, &mut VisualTestContext) {
    testing::open_window(cx, move |_, _| Probe {
        build: Box::new(build),
        _subs: Vec::new(),
    })
}

fn counter() -> Rc<Cell<u32>> {
    Rc::new(Cell::new(0))
}

fn column(children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .children(children)
        .into_any_element()
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
    let (_v, vcx) = probe(cx, move |_, _| {
        Button::tonal("save", &t, "Guardar")
            .on_click(bump(&h))
            .into_any_element()
    });
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
            Button::primary("b-button", &t, "Button")
                .on_click(hit("button"))
                .into_any_element(),
            Button::action("b-action", &t, "Action")
                .on_click(hit("action"))
                .into_any_element(),
            IconButton::new("b-icon", &t, Icon::Plus)
                .on_click(hit("icon"))
                .into_any_element(),
            GhostButton::new("b-ghost", &t, "Ghost")
                .on_click(hit("ghost"))
                .into_any_element(),
            Pill::new("b-pill", &t)
                .label("Pill")
                .on_click(hit("pill"))
                .into_any_element(),
            Toggle::new("b-toggle", &t)
                .on_change(move |_, _, _| l2.borrow_mut().push("toggle"))
                .into_any_element(),
            ListRow::new("b-row", &t)
                .title("Row")
                .on_click(hit("row"))
                .into_any_element(),
            SegmentedControl::new("b-seg", &t, ["A", "B"])
                .on_change(move |ix, _, _| {
                    l3.borrow_mut()
                        .push(if ix == 0 { "seg-a" } else { "seg-b" })
                })
                .into_any_element(),
        ])
    });
    press(
        vcx,
        "tab enter tab enter tab enter tab enter tab enter tab space tab enter tab enter tab enter",
    );
    assert_eq!(
        *log.borrow(),
        [
            "button", "action", "icon", "ghost", "pill", "toggle", "row", "seg-a", "seg-b"
        ]
    );
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
            Button::tonal("c-decor", &t, "Decorativo")
                .focusable(false)
                .on_click(hit("decor"))
                .into_any_element(),
            Button::tonal("c-off", &t, "Apagado")
                .disabled(true)
                .on_click(hit("off"))
                .into_any_element(),
            Pill::new("c-label", &t)
                .label("Solo etiqueta")
                .into_any_element(),
            IconButton::new("c-icon", &t, Icon::Plus)
                .on_click(hit("icon-off"))
                .focusable(false)
                .into_any_element(),
            Button::tonal("c-real", &t, "Real")
                .on_click(hit("real"))
                .into_any_element(),
        ])
    });
    press(vcx, "tab enter");
    assert_eq!(
        *log.borrow(),
        ["real"],
        "the first tab stop is the only real one"
    );
    press(vcx, "tab enter");
    assert_eq!(*log.borrow(), ["real", "real"], "and Tab wraps back to it");
}

#[gpui::test]
fn a_press_does_not_steal_focus_but_still_clicks(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let hits = counter();
    let h = hits.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        Button::tonal("press-me", &t, "Pulsar")
            .on_click(bump(&h))
            .into_any_element()
    });
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
        Toggle::new("tg", &t)
            .on(o.get())
            .on_change(move |v, w, _| {
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
        Slider::new("sl", &t)
            .range(0.0, 100.0)
            .step(5.0)
            .value(vv.get())
            .on_change(move |x, w, _| {
                v2.set(x);
                w.refresh();
            })
            .into_any_element()
    });
    press(vcx, "tab right right");
    assert_eq!(v.get(), 60.0);
    press(vcx, "left pagedown");
    assert_eq!(
        v.get(),
        5.0,
        "left = one step (55), page down = a tenth of the range more (clamped to 55 - 50)"
    );
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
            Checkbox::new("ck", &t)
                .label("Acepto")
                .checked(c.get())
                .on_change(move |v, w, _| {
                    c2.set(v);
                    w.refresh();
                })
                .into_any_element(),
            RadioGroup::new("rg", &t, ["Uno", "Dos", "Tres"])
                .selected(Some(p.get()))
                .on_change(move |ix, w, _| {
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
        div()
            .w(px(400.0))
            .h(px(500.0))
            .p(px(10.0))
            .child(self.select.clone())
    }
}

fn select_host<'a>(
    cx: &'a mut TestAppContext,
    n: usize,
    initial: &'static str,
) -> (
    Entity<Select>,
    &'a mut VisualTestContext,
    Rc<RefCell<Vec<usize>>>,
) {
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
    assert!(
        s.read_with(vcx, |s, _| s.is_open()),
        "Down opens on the current value"
    );
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(0));
    press(vcx, "down");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(1));
    press(vcx, "down");
    assert_eq!(
        s.read_with(vcx, |s, _| s.highlighted_index()),
        Some(3),
        "index 2 is disabled"
    );
    press(vcx, "up up up");
    assert_eq!(
        s.read_with(vcx, |s, _| s.highlighted_index()),
        Some(0),
        "clamped at the first"
    );
    press(vcx, "end");
    assert_eq!(s.read_with(vcx, |s, _| s.highlighted_index()), Some(5));
    press(vcx, "enter");
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    assert_eq!(
        s.read_with(vcx, |s, _| s.value().cloned()),
        Some("v5".into())
    );
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
    assert_eq!(
        s.read_with(vcx, |s, _| s.value().cloned()),
        Some("v1".into())
    );
    assert!(ev.borrow().is_empty());
}

#[gpui::test]
fn select_type_ahead_jumps_to_a_match(cx: &mut TestAppContext) {
    let events: Rc<RefCell<Vec<usize>>> = Rc::default();
    let ev = events.clone();
    let (view, vcx) = testing::open_window(cx, move |window, cx| {
        let select = cx.new(|cx| {
            Select::new("ta", Theme::dark(), cx).options(
                ["Alpha", "Beta", "Bravo", "Charlie"]
                    .map(|n| SelectOption::new(n.to_lowercase(), n)),
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
    assert_eq!(
        s.read_with(vcx, |s, _| s.value().cloned()),
        Some("charlie".into())
    );
    press(vcx, "b");
    assert_eq!(
        s.read_with(vcx, |s, _| s.value().cloned()),
        Some("beta".into())
    );
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
    assert!(
        s.read_with(vcx, |s, _| s.is_open()),
        "a press on the trigger opens it"
    );
    testing::click_id(vcx, "sel-v3");
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    assert_eq!(
        s.read_with(vcx, |s, _| s.value().cloned()),
        Some("v3".into())
    );
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
        s.set_options(
            [
                SelectOption::new("v9", "Nueva"),
                SelectOption::new("v3", "Tres"),
            ],
            cx,
        );
    });
    assert_eq!(
        s.read_with(vcx, |s, _| s.selected_index()),
        Some(1),
        "v3 moved to index 1"
    );
    s.update(vcx, |s, cx| {
        s.set_options([SelectOption::new("x", "X")], cx)
    });
    assert_eq!(s.read_with(vcx, |s, _| s.selected_index()), None);
}

// ----- v3 additions -------------------------------------------------------------------------

/// Focus handles captured while rendering (a handle can only be created during a frame).
type Handles = Rc<RefCell<std::collections::HashMap<String, gpui::FocusHandle>>>;

fn record(handles: &Handles, window: &mut Window, cx: &mut gpui::App, id: gpui::ElementId) {
    let h = focus::handle(window, cx, &id);
    handles.borrow_mut().insert(id.to_string(), h);
}

fn handle_of(handles: &Handles, id: &gpui::ElementId) -> gpui::FocusHandle {
    handles.borrow()[&id.to_string()].clone()
}

fn selected_cell() -> Rc<Cell<Option<usize>>> {
    Rc::new(Cell::new(None))
}

/// Three radio rows (optionally preselected) followed by a plain button, to tell when Tab leaves.
/// Focus is observed through behavior: `Space` selects the focused row.
fn radio_probe(
    cx: &mut TestAppContext,
    sel: Rc<Cell<Option<usize>>>,
    on_arrow: bool,
) -> (&mut VisualTestContext, Rc<Cell<u32>>) {
    let t = Theme::dark();
    let after_hits = counter();
    let ah = after_hits.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let mut all: Vec<AnyElement> = (0..3)
            .map(|ix| {
                let s = sel.clone();
                RadioRow::new("rg3", ix, 3, &t)
                    .selected(sel.get())
                    .select_on_arrow(on_arrow)
                    .on_select(move |i, w, _| {
                        s.set(Some(i));
                        w.refresh();
                    })
                    .child(format!("fila {ix}"))
                    .into_any_element()
            })
            .collect();
        all.push(
            Button::tonal("after", &t, "Despues")
                .on_click(bump(&ah))
                .into_any_element(),
        );
        column(all)
    });
    (vcx, after_hits)
}

#[gpui::test]
fn radio_rows_have_one_tab_stop_arrows_move_focus_and_space_selects(cx: &mut TestAppContext) {
    let sel = selected_cell();
    let (vcx, after) = radio_probe(cx, sel.clone(), false);
    // Nothing selected: the first row is the only stop.
    press(vcx, "tab space");
    assert_eq!(sel.get(), Some(0), "Tab landed on the first row");
    sel.set(None);
    press(vcx, "down");
    assert_eq!(sel.get(), None, "an arrow only moves the focus");
    press(vcx, "space");
    assert_eq!(sel.get(), Some(1), "Down moved to the second row");
    press(vcx, "down space");
    assert_eq!(sel.get(), Some(2));
    press(vcx, "down space");
    assert_eq!(sel.get(), Some(0), "Down wraps around");
    press(vcx, "up space");
    assert_eq!(sel.get(), Some(2), "Up wraps backwards");
    press(vcx, "up enter");
    assert_eq!(sel.get(), Some(1), "Enter selects");
    // One group = one tab stop: Tab leaves for the button after it.
    press(vcx, "tab enter");
    assert_eq!(after.get(), 1, "Tab left the group");
    assert_eq!(sel.get(), Some(1));
    // Shift+Tab comes back to the selected row, not to the first one.
    press(vcx, "shift-tab down space");
    assert_eq!(sel.get(), Some(2), "back on the selected row (1), Down = 2");
}

#[gpui::test]
fn radio_tab_stop_is_the_selected_row(cx: &mut TestAppContext) {
    let sel = Rc::new(Cell::new(Some(1)));
    let (vcx, _) = radio_probe(cx, sel.clone(), false);
    press(vcx, "tab down space");
    assert_eq!(
        sel.get(),
        Some(2),
        "Tab lands on the selected row (a first-row stop would give 1)"
    );
}

#[gpui::test]
fn radio_arrows_select_when_asked_and_click_selects(cx: &mut TestAppContext) {
    let sel = Rc::new(Cell::new(Some(0)));
    let (vcx, _) = radio_probe(cx, sel.clone(), true);
    press(vcx, "tab down");
    assert_eq!(sel.get(), Some(1), "the arrow selected the row it moved to");
    let id = RadioRow::row_id("rg3", 2).to_string();
    testing::click_id(vcx, &id);
    assert_eq!(sel.get(), Some(2), "a click selects");
}

#[gpui::test]
fn focus_card_is_a_tab_stop_that_clicks_on_enter_and_space(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let hits = counter();
    let h = hits.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        column(vec![
            FocusCard::new("fc-inert", &t)
                .child("sin accion")
                .into_any_element(),
            FocusCard::new("fc-off", &t)
                .focusable(false)
                .on_click(|_, _, _| {})
                .child("fuera del orden")
                .into_any_element(),
            FocusCard::new("fc", &t)
                .on_click(bump(&h))
                .p_4()
                .child("tarjeta")
                .into_any_element(),
        ])
    });
    press(vcx, "tab enter");
    assert_eq!(hits.get(), 1, "the card is the first (only) stop");
    press(vcx, "space");
    assert_eq!(hits.get(), 2);
    testing::click_id(vcx, "fc");
    assert_eq!(hits.get(), 3);
}

#[gpui::test]
fn icon_button_rotates_in_code_without_changing_layout(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let (_v, vcx) = probe(cx, move |_, _| {
        column(vec![
            IconButton::new("up", &t, Icon::ChevronDown)
                .rotate(Rotation::Deg180)
                .on_click(|_, _, _| {})
                .into_any_element(),
            IconButton::new("down", &t, Icon::ChevronDown)
                .on_click(|_, _, _| {})
                .into_any_element(),
            icon_rotated(Icon::ChevronRight, Rotation::Deg270, 16.0, t.colors.text)
                .into_any_element(),
            Icon::ChevronRight
                .rotate(Rotation::Deg90)
                .svg(16.0, t.colors.text)
                .into_any_element(),
        ])
    });
    let (a, b) = (
        testing::bounds_of(vcx, "up").unwrap(),
        testing::bounds_of(vcx, "down").unwrap(),
    );
    assert_eq!(a.size, b.size, "rotation is paint-only");
    assert_eq!(Rotation::Deg90.degrees(), 90.0);
    assert_eq!(Rotation::None.degrees(), 0.0);
    assert_eq!(
        Icon::ChevronDown.rotate(Rotation::Deg180),
        RotatedIcon {
            icon: Icon::ChevronDown,
            rotation: Rotation::Deg180
        }
    );
}

#[gpui::test]
fn full_width_pill_spans_the_row_and_the_default_hugs_its_label(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let (_v, vcx) = probe(cx, move |_, _| {
        column(vec![
            div()
                .flex()
                .flex_row()
                .child(
                    Pill::new("p-full", &t)
                        .full_width(true)
                        .active(true)
                        .icon(Icon::Folder)
                        .label("Proyectos")
                        .on_click(|_, _, _| {}),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_row()
                .child(
                    Pill::new("p-hug", &t)
                        .label("Proyectos")
                        .on_click(|_, _, _| {}),
                )
                .into_any_element(),
        ])
    });
    let full = testing::bounds_of(vcx, "p-full").unwrap();
    let hug = testing::bounds_of(vcx, "p-hug").unwrap();
    // The probe is 400 wide with 10 px of padding.
    assert_eq!(f32::from(full.size.width), 380.0);
    assert!(f32::from(hug.size.width) < 380.0);
}

#[gpui::test]
fn empty_state_renders_its_action(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let hits = counter();
    let h = hits.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        EmptyState::new(&t, "Sin cambios")
            .icon(Icon::GitBranch)
            .body("El arbol de trabajo esta limpio.")
            .action(Button::tonal("es-act", &t, "Actualizar").on_click(bump(&h)))
            .into_any_element()
    });
    testing::click_id(vcx, "es-act");
    assert_eq!(hits.get(), 1);
}

#[gpui::test]
fn empty_state_with_only_a_title_is_a_valid_block(cx: &mut TestAppContext) {
    let t = Theme::light();
    let (_v, vcx) = probe(cx, move |_, _| {
        div()
            .id("es-wrap")
            .debug_selector(|| "es-wrap".to_string())
            .child(EmptyState::new(&t, "Vacio").fill(false))
            .into_any_element()
    });
    let b = testing::bounds_of(vcx, "es-wrap").unwrap();
    assert!(f32::from(b.size.height) > 0.0);
}

/// A host with a `Select` and another focusable control.
struct SelectPair {
    select: Entity<Select>,
    handles: Handles,
}

impl Render for SelectPair {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::dark();
        record(&self.handles, window, cx, gpui::ElementId::from("other"));
        div()
            .w(px(400.0))
            .h(px(500.0))
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(self.select.clone())
            .child(IconButton::new("other", &t, Icon::Plus).on_click(|_, _, _| {}))
    }
}

#[gpui::test]
fn select_closes_when_focus_leaves_even_without_tab(cx: &mut TestAppContext) {
    let handles: Handles = Rc::default();
    let hs = handles.clone();
    let (view, vcx) = testing::open_window(cx, move |window, cx| {
        let select = cx.new(|cx| {
            Select::new("pair", Theme::dark(), cx)
                .options((0..4).map(|i| SelectOption::new(format!("v{i}"), format!("Op {i}"))))
        });
        select.read(cx).focus(window);
        SelectPair {
            select,
            handles: hs,
        }
    });
    let s = view.read_with(vcx, |v, _| v.select.clone());
    press(vcx, "down");
    assert!(s.read_with(vcx, |s, _| s.is_open()));
    // A host that swallows Tab moves the focus itself: the list must still close.
    let other = handle_of(&handles, &gpui::ElementId::from("other"));
    vcx.update(|window, _| window.focus(&other));
    vcx.run_until_parked();
    assert!(
        !s.read_with(vcx, |s, _| s.is_open()),
        "focus-out closed the list"
    );
    // Re-open, then lose the focus to nothing (a click on the background / window blur).
    let sel_focus = s.read_with(vcx, gpui::Focusable::focus_handle);
    vcx.update(|window, _| window.focus(&sel_focus));
    press(vcx, "down");
    assert!(s.read_with(vcx, |s, _| s.is_open()));
    vcx.update(|window, _| window.blur());
    vcx.run_until_parked();
    assert!(!s.read_with(vcx, |s, _| s.is_open()));
    // Picking with the mouse is not a focus-out.
    testing::click_id(vcx, "pair-trigger");
    assert!(s.read_with(vcx, |s, _| s.is_open()));
    testing::click_id(vcx, "pair-v2");
    assert_eq!(
        s.read_with(vcx, |s, _| s.value().cloned()),
        Some("v2".into())
    );
}

#[gpui::test]
fn popover_escape_dismisses_through_the_interceptor(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let dismissed = counter();
    let d = dismissed.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let d = d.clone();
        Popover::new("pop-esc", &t)
            .open(true)
            .on_dismiss(move |window, _| {
                d.set(d.get() + 1);
                window.refresh();
            })
            .trigger(IconButton::new("pop-trigger", &t, Icon::Layout).on_click(|_, _, _| {}))
            .child(
                Button::tonal("pop-inner", &t, "Dentro")
                    .on_click(|_, _, _| {})
                    .into_any_element(),
            )
            .into_any_element()
    });
    // Default: the interceptor catches Esc wherever the focus is (here: nowhere).
    press(vcx, "escape");
    assert_eq!(dismissed.get(), 1, "interceptor path");
}

#[gpui::test]
fn popover_cancel_action_works_alone_when_the_interceptor_is_off(cx: &mut TestAppContext) {
    let t = Theme::dark();
    let dismissed = counter();
    let d = dismissed.clone();
    let (_v, vcx) = probe(cx, move |_, _| {
        let d = d.clone();
        Popover::new("pop-act", &t)
            .open(true)
            .esc_interceptor(false)
            .on_dismiss(move |window, _| {
                d.set(d.get() + 1);
                window.refresh();
            })
            .trigger(IconButton::new("pop-trigger2", &t, Icon::Layout).on_click(|_, _, _| {}))
            .child(
                Button::tonal("pop-inner2", &t, "Dentro")
                    .on_click(|_, _, _| {})
                    .into_any_element(),
            )
            .into_any_element()
    });
    // No focus inside the panel: nothing handles Esc (the app keeps it).
    press(vcx, "escape");
    assert_eq!(dismissed.get(), 0, "nothing focused, no interceptor");
    // Focus inside the panel: the component's own Cancel action runs.
    press(vcx, "tab");
    assert_eq!(dismissed.get(), 0, "the trigger is outside the panel");
    press(vcx, "tab escape");
    assert_eq!(dismissed.get(), 1, "the panel handled Esc itself");
}
