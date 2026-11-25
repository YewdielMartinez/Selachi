//! Selection behaviour in a test window: events are dispatched straight into the window
//! (never the real desktop). The test platform lays text out with fixed-width glyphs.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Context, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    Render, TestAppContext, VisualTestContext, Window, div, point, prelude::*, px,
};
use theme::Theme;

use super::*;
use crate::testing;

type Block = (&'static str, Gap, Vec<(Range<usize>, &'static str)>);

struct Demo {
    group: SelectionGroup,
    theme: Theme,
    width: f32,
    blocks: Vec<Block>,
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut segs = Segments::new();
        let mut els = Vec::new();
        for (text, gap, links) in &self.blocks {
            let ix = segs.push(*text, *gap);
            els.push(
                div()
                    .child(SelectableText::new(&self.group, ix, *text).links(links.clone()))
                    .into_any_element(),
            );
        }
        self.group.set_segments(segs);
        div().p(px(10.0)).child(
            self.group
                .container("demo", &self.theme)
                .w(px(self.width))
                .text_size(px(14.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .children(els),
        )
    }
}

fn open(
    cx: &mut TestAppContext,
    width: f32,
    blocks: Vec<Block>,
) -> (SelectionGroup, &mut VisualTestContext) {
    testing::init(cx);
    let group = cx.update(SelectionGroup::new);
    let g = group.clone();
    let (_view, vcx) = testing::open_window(cx, move |_, _| Demo {
        group: g,
        theme: Theme::dark(),
        width,
        blocks,
    });
    (group, vcx)
}

fn plain(blocks: &[(&'static str, Gap)]) -> Vec<Block> {
    blocks.iter().map(|(t, g)| (*t, *g, Vec::new())).collect()
}

/// The point at the boundary before byte `offset` of `seg`, vertically centered on its row.
fn at(group: &SelectionGroup, seg: usize, offset: usize) -> Point<Pixels> {
    let p = group
        .position_of(SelPos::new(seg, offset))
        .expect("segment painted");
    point(p.x, p.y + px(8.0))
}

fn down(cx: &mut VisualTestContext, p: Point<Pixels>, clicks: usize, mods: Modifiers) {
    cx.simulate_event(MouseDownEvent {
        position: p,
        modifiers: mods,
        button: MouseButton::Left,
        click_count: clicks,
        first_mouse: false,
    });
}

fn up(cx: &mut VisualTestContext, p: Point<Pixels>, clicks: usize) {
    cx.simulate_event(MouseUpEvent {
        position: p,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: clicks,
    });
}

fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    down(cx, from, 1, Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    up(cx, to, 1);
}

fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|i| i.text())
}

#[gpui::test]
fn dragging_selects_the_text_between_the_two_points(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("hello selectable world", Gap::Block)]));
    drag(cx, at(&group, 0, 6), at(&group, 0, 16));
    assert_eq!(group.selected_text().as_deref(), Some("selectable"));
    // The other direction gives the same selection.
    drag(cx, at(&group, 0, 16), at(&group, 0, 6));
    assert_eq!(group.selected_text().as_deref(), Some("selectable"));
}

#[gpui::test]
fn a_plain_click_selects_nothing_and_a_drag_past_the_end_takes_the_rest(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("hello world", Gap::Block)]));
    let p = at(&group, 0, 4);
    cx.simulate_click(p, Modifiers::none());
    assert!(!group.has_selection());
    let b = group.bounds_of(0).unwrap();
    drag(cx, at(&group, 0, 6), point(b.right() + px(80.0), p.y));
    assert_eq!(group.selected_text().as_deref(), Some("world"));
}

#[gpui::test]
fn double_click_selects_a_word_and_triple_click_the_paragraph(cx: &mut TestAppContext) {
    let (group, cx) = open(
        cx,
        400.0,
        plain(&[
            ("alpha beta_gamma delta", Gap::Block),
            ("second", Gap::Block),
        ]),
    );
    let p = at(&group, 0, 8);
    down(cx, p, 2, Modifiers::none());
    up(cx, p, 2);
    assert_eq!(group.selected_text().as_deref(), Some("beta_gamma"));
    down(cx, p, 3, Modifiers::none());
    up(cx, p, 3);
    assert_eq!(
        group.selected_text().as_deref(),
        Some("alpha beta_gamma delta")
    );
}

#[gpui::test]
fn double_click_then_drag_extends_by_whole_words(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("one two three four", Gap::Block)]));
    down(cx, at(&group, 0, 5), 2, Modifiers::none());
    cx.simulate_mouse_move(at(&group, 0, 12), MouseButton::Left, Modifiers::none());
    up(cx, at(&group, 0, 12), 2);
    assert_eq!(group.selected_text().as_deref(), Some("two three"));
}

#[gpui::test]
fn shift_click_extends_the_selection(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("abcdefghij", Gap::Block)]));
    drag(cx, at(&group, 0, 2), at(&group, 0, 4));
    assert_eq!(group.selected_text().as_deref(), Some("cd"));
    let p = at(&group, 0, 8);
    down(cx, p, 1, Modifiers::shift());
    up(cx, p, 1);
    assert_eq!(group.selected_text().as_deref(), Some("cdefgh"));
}

#[gpui::test]
fn ctrl_c_copies_and_ctrl_a_selects_the_whole_group(cx: &mut TestAppContext) {
    let (group, cx) = open(
        cx,
        400.0,
        plain(&[
            ("first", Gap::Block),
            ("second", Gap::Block),
            ("third", Gap::Line),
        ]),
    );
    drag(cx, at(&group, 0, 1), at(&group, 0, 4));
    testing::press(cx, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("irs"));
    testing::press(cx, "ctrl-a");
    assert_eq!(
        group.selected_text().as_deref(),
        Some("first\n\nsecond\nthird")
    );
    testing::press(cx, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("first\n\nsecond\nthird"));
}

#[gpui::test]
fn a_selection_spans_blocks_and_joins_them_by_their_gap(cx: &mut TestAppContext) {
    let (group, cx) = open(
        cx,
        400.0,
        vec![
            ("title", Gap::Block, vec![]),
            ("code a", Gap::Block, vec![]),
            ("code b", Gap::Line, vec![]),
            ("cell", Gap::Tab, vec![]),
        ],
    );
    drag(cx, at(&group, 0, 2), at(&group, 3, 2));
    assert_eq!(
        group.selected_text().as_deref(),
        Some("tle\n\ncode a\ncode b\tce")
    );
}

#[gpui::test]
fn wrapped_text_is_hit_tested_per_visual_row(cx: &mut TestAppContext) {
    // ~8 px per glyph at 14 px: 80 px wraps after about ten characters.
    let (group, cx) = open(
        cx,
        80.0,
        plain(&[("aaaa bbbb cccc dddd eeee ffff", Gap::Block)]),
    );
    let b = group.bounds_of(0).unwrap();
    assert!(b.size.height > px(20.0), "the text wrapped: {b:?}");
    // From the start to the last row's end.
    let first = point(b.left() + px(1.0), b.top() + px(2.0));
    let last = point(b.right() - px(1.0), b.bottom() - px(2.0));
    drag(cx, first, last);
    let sel = group.selected_text().unwrap();
    assert!(sel.starts_with("aaaa"), "{sel}");
    assert!(sel.ends_with("ffff"), "{sel}");
    // A point in the middle row lands inside the text, not at the start or end.
    let mid = point(b.left() + px(20.0), b.top() + b.size.height / 2.0);
    drag(cx, first, mid);
    let part = group.selected_text().unwrap();
    assert!(part.len() > 4 && part.len() < 20, "{part}");
}

#[gpui::test]
fn a_click_on_a_link_opens_it_but_a_drag_over_it_selects(cx: &mut TestAppContext) {
    let text = "see the docs now";
    let (group, cx) = open(
        cx,
        400.0,
        vec![(text, Gap::Block, vec![(8..12, "https://example.dev")])],
    );
    let opened = Rc::new(RefCell::new(Vec::<String>::new()));
    let o = opened.clone();
    group.on_link(move |url, _, _| o.borrow_mut().push(url.to_string()));
    // Click in the middle of "docs".
    let p = at(&group, 0, 10);
    cx.simulate_click(p, Modifiers::none());
    assert_eq!(*opened.borrow(), vec!["https://example.dev".to_string()]);
    assert!(!group.has_selection());
    // Dragging across it selects and does not open.
    drag(cx, at(&group, 0, 4), at(&group, 0, 14));
    assert_eq!(opened.borrow().len(), 1);
    assert_eq!(group.selected_text().as_deref(), Some("the docs n"));
    // A click elsewhere in the text is not a link click.
    cx.simulate_click(at(&group, 0, 1), Modifiers::none());
    assert_eq!(opened.borrow().len(), 1);
}

#[gpui::test]
fn on_copy_replaces_the_clipboard_write(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("password=hunter2", Gap::Block)]));
    group.on_copy(|text, _, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            text.replace("hunter2", "***"),
        ))
    });
    drag(cx, at(&group, 0, 0), at(&group, 0, 16));
    testing::press(cx, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("password=***"));
}

#[gpui::test]
fn pressing_in_another_group_clears_the_first_selection(cx: &mut TestAppContext) {
    testing::init(cx);
    let (a, b) = cx.update(|cx| (SelectionGroup::new(cx), SelectionGroup::new(cx)));
    struct Two(SelectionGroup, SelectionGroup, Theme);
    impl Render for Two {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let mut one = Segments::new();
            let i = one.push("left group text", Gap::Block);
            self.0.set_segments(one);
            let mut two = Segments::new();
            let j = two.push("right group text", Gap::Block);
            self.1.set_segments(two);
            div()
                .flex()
                .flex_col()
                .gap(px(20.0))
                .text_size(px(14.0))
                .child(
                    self.0
                        .container("a", &self.2)
                        .w(px(300.0))
                        .child(SelectableText::new(&self.0, i, "left group text")),
                )
                .child(
                    self.1
                        .container("b", &self.2)
                        .w(px(300.0))
                        .child(SelectableText::new(&self.1, j, "right group text")),
                )
        }
    }
    let (ga, gb) = (a.clone(), b.clone());
    let (_v, cx) = testing::open_window(cx, move |_, _| Two(ga, gb, Theme::dark()));
    drag(cx, at(&a, 0, 0), at(&a, 0, 4));
    assert!(a.has_selection());
    drag(cx, at(&b, 0, 1), at(&b, 0, 5));
    assert!(!a.has_selection(), "the first group lost its selection");
    assert_eq!(b.selected_text().as_deref(), Some("ight"));
    // A press outside both groups clears the live one.
    cx.simulate_click(point(px(700.0), px(500.0)), Modifiers::none());
    assert!(!b.has_selection());
}

#[gpui::test]
fn the_container_takes_keyboard_focus_on_press(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("focus me", Gap::Block)]));
    assert!(!testing::is_focused(cx, group.focus_handle()));
    cx.simulate_click(at(&group, 0, 2), Modifiers::none());
    assert!(testing::is_focused(cx, group.focus_handle()));
}

#[gpui::test]
fn offsets_are_model_coordinates_so_they_survive_a_relayout(cx: &mut TestAppContext) {
    let (group, cx) = open(cx, 400.0, plain(&[("stable selection model", Gap::Block)]));
    drag(cx, at(&group, 0, 7), at(&group, 0, 16));
    let before = group.selection();
    cx.simulate_resize(gpui::size(px(500.0), px(300.0)));
    cx.run_until_parked();
    assert_eq!(group.selection(), before);
    assert_eq!(group.selected_text().as_deref(), Some("selection"));
}

#[gpui::test]
fn the_selection_paints_one_quad_per_visual_row(cx: &mut TestAppContext) {
    let (group, cx) = open(
        cx,
        80.0,
        plain(&[
            ("aaaa bbbb cccc dddd eeee ffff", Gap::Block),
            ("tail", Gap::Block),
        ]),
    );
    assert!(group.selection_rects(0).is_empty());
    let b = group.bounds_of(0).unwrap();
    group.set_selection(SelPos::new(0, 0), SelPos::new(1, 2));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let rects = group.selection_rects(0);
    assert!(rects.len() >= 3, "wrapped into rows: {rects:?}");
    assert!(rects.iter().all(|r| b.contains(&r.origin)));
    // The segment the selection passes through entirely, and the one it ends in.
    let tail = group.selection_rects(1);
    assert_eq!(tail.len(), 1);
    assert!(tail[0].size.width > px(0.0));
    // Rows below the last glyph of the first segment carry the line-break stub.
    let last = rects.last().unwrap();
    assert!(last.size.width >= px(4.0));
}

#[gpui::test]
fn selectable_text_wraps_inside_shrink_to_fit_flex_bubble(cx: &mut TestAppContext) {
    testing::init(cx);
    let group = cx.update(SelectionGroup::new);
    struct Bubble(SelectionGroup, Theme);
    impl Render for Bubble {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let text = "Esta es una frase larga dentro de una burbuja alineada a la derecha";
            let mut segs = Segments::new();
            let ix = segs.push(text, Gap::Block);
            self.0.set_segments(segs);
            div().w(px(200.0)).flex().flex_row().justify_end().child(
                div()
                    .max_w(gpui::relative(0.82))
                    .min_w_0()
                    .px(px(12.0))
                    .flex()
                    .flex_col()
                    .text_size(px(14.0))
                    .child(
                        self.0
                            .container("bubble", &self.1)
                            .w_full()
                            .min_w_0()
                            .child(SelectableText::new(&self.0, ix, text)),
                    ),
            )
        }
    }
    let g = group.clone();
    let (_v, cx) = testing::open_window(cx, move |_, _| Bubble(g, Theme::dark()));
    let b = group.bounds_of(0).expect("painted");
    assert!(
        b.left() >= px(0.0) && b.right() <= px(200.0),
        "text escaped 200px container: {b:?}"
    );
    assert!(b.size.height > px(30.0), "text should wrap: {b:?}");
    // Select all and verify every visual row stays within bounds.
    group.select_all();
    cx.update(|w, _| w.refresh());
    cx.run_until_parked();
    let rects = group.selection_rects(0);
    assert!(rects.len() >= 3, "expected >=3 wrapped rows: {rects:?}");
    for r in &rects {
        assert!(
            r.right() <= b.right() + px(1.0),
            "wrapped row {r:?} exceeded text bounds {b:?}"
        );
    }
}
