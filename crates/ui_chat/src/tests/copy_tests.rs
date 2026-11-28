//! Copying and selecting text (PLAN 21.1): drag / Ctrl+C / Ctrl+A in chat messages, the copy
//! modes of a message, redaction of command output and the conversation copy.

use super::*;
use crate::copy::CopyKind;
use crate::copy_ui::CopyMode;
use crate::rows::{RowKey, RowKind};
use agents_driver::ToolKind;
use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, point};
use serde_json::json;
use ui_kit::{SelPos, SelectionGroup};

const KEY: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
const ANSWER: &str =
    "Primer párrafo con `código` y [docs](https://x.dev).\n\n- uno\n- dos\n\n```sh\nls -la\n```\n";

fn script() -> Arc<FakeFactory> {
    FakeFactory::new(|_, _| {
        vec![
            Step::now(AgentEvent::Status(AgentStatus::Working)),
            Step::now(AgentEvent::Text {
                text: ANSWER.into(),
                delta: false,
            }),
            Step::now(AgentEvent::ToolCall {
                id: "c1".into(),
                kind: ToolKind::Command,
                title: "env".into(),
                input_json: json!({ "command": "env" }),
            }),
            Step::now(AgentEvent::ToolResult {
                id: "c1".into(),
                ok: true,
                summary: format!("HOME=/x\nGITHUB_TOKEN={KEY}\nDONE"),
            }),
            Step::now(AgentEvent::Status(AgentStatus::Done)),
        ]
    })
}

fn opened(cx: &mut TestAppContext) -> (Env, Entity<ChatView>, &mut VisualTestContext) {
    let e = env(cx, script());
    let (view, vcx, rt) = thread_view(cx, &e);
    view.update(vcx, |v, cx| v.send("corre env".into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status == ThreadStatus::Done));
    settle(vcx, "rows", |vcx| {
        view.update(vcx, |v, cx| v.sync(cx));
        view.read_with(vcx, |v, _| {
            v.rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::Command { .. }))
        })
    });
    (e, view, vcx)
}

fn row_key(view: &Entity<ChatView>, vcx: &mut VisualTestContext, text: bool) -> RowKey {
    view.read_with(vcx, |v, _| {
        v.rows
            .iter()
            .find(|r| {
                if text {
                    matches!(r.kind, RowKind::Text { .. })
                } else {
                    matches!(r.kind, RowKind::Command { .. })
                }
            })
            .map(|r| r.key.clone())
            .unwrap()
    })
}

fn group(view: &Entity<ChatView>, vcx: &mut VisualTestContext, key: &RowKey) -> SelectionGroup {
    view.read_with(vcx, |v, _| v.selections.get(&(key.clone(), 0)).cloned())
        .expect("the row was painted")
}

fn repaint(vcx: &mut VisualTestContext) {
    vcx.update(|w, _| w.refresh());
    vcx.run_until_parked();
}

fn at(g: &SelectionGroup, seg: usize, off: usize) -> Point<Pixels> {
    let p = g.position_of(SelPos::new(seg, off)).expect("painted");
    point(p.x, p.y + gpui::px(6.0))
}

fn drag(vcx: &mut VisualTestContext, a: Point<Pixels>, b: Point<Pixels>) {
    vcx.simulate_event(MouseDownEvent {
        position: a,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_mouse_move(b, MouseButton::Left, Modifiers::none());
    vcx.simulate_event(MouseUpEvent {
        position: b,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
}

fn clipboard(vcx: &mut VisualTestContext) -> String {
    vcx.read_from_clipboard()
        .and_then(|i| i.text())
        .unwrap_or_default()
}

fn toast(view: &Entity<ChatView>, vcx: &mut VisualTestContext) -> Option<(String, bool)> {
    view.read_with(vcx, |v, _| {
        v.toast.as_ref().map(|t| (t.text.clone(), t.hidden))
    })
}

#[gpui::test]
fn a_drag_selects_across_paragraph_list_and_code_and_ctrl_c_copies_it(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let g = group(&view, vcx, &key);
    let painted = g.painted_segments();
    assert!(
        painted.len() >= 4,
        "paragraph, two items, code line: {painted:?}"
    );
    let last = *painted.last().unwrap();
    drag(vcx, at(&g, 0, 0), at(&g, last, "ls -la".len()));
    ui_kit::testing::press(vcx, "ctrl-c");
    assert_eq!(
        clipboard(vcx),
        "Primer párrafo con código y docs.\n\nuno\ndos\n\nls -la",
        "inline code and links lose their syntax, blocks are separated, fences stay out"
    );
    assert_eq!(toast(&view, vcx), Some(("Copiado".into(), false)));
}

#[gpui::test]
fn ctrl_a_selects_the_whole_focused_message(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let g = group(&view, vcx, &key);
    // A plain click focuses the message (and selects nothing).
    vcx.simulate_click(at(&g, 0, 3), Modifiers::none());
    ui_kit::testing::press(vcx, "ctrl-a ctrl-c");
    let out = clipboard(vcx);
    assert!(out.starts_with("Primer párrafo"), "{out}");
    assert!(out.ends_with("ls -la"), "{out}");
}

#[gpui::test]
fn message_copy_modes_give_markdown_or_plain_text(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    let key = row_key(&view, vcx, true);
    view.update(vcx, |v, cx| v.copy_message(&key, CopyMode::Markdown, cx));
    assert_eq!(
        clipboard(vcx),
        ANSWER,
        "the original Markdown, fences included"
    );
    view.update(vcx, |v, cx| v.copy_message(&key, CopyMode::Plain, cx));
    let plain = clipboard(vcx);
    assert!(
        !plain.contains("```") && !plain.contains('`') && !plain.contains("]("),
        "{plain}"
    );
    assert!(
        plain.contains("- uno\n- dos") && plain.contains("ls -la"),
        "{plain}"
    );
}

#[gpui::test]
fn command_output_is_redacted_on_copy_by_default_and_says_so(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    let key = row_key(&view, vcx, false);
    view.update(vcx, |v, cx| v.copy_command_output(&key, cx));
    let out = clipboard(vcx);
    assert!(
        !out.contains(KEY) && out.contains("HOME=/x") && out.contains("DONE"),
        "{out}"
    );
    assert_eq!(
        toast(&view, vcx),
        Some(("Copiado · incluye 1 secreto oculto".into(), true))
    );
    // Turning the toggle off copies the output as is, and tells the host to save it to `[chat]`.
    let (hub, seen) = (
        view.read_with(vcx, |v, _| v.hub.clone()),
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
    );
    let log = seen.clone();
    let _sub = hub.update(vcx, |_, cx| {
        cx.subscribe(&hub, move |_, _, ev: &crate::hub::ChatEvent, _| {
            log.borrow_mut().push(ev.clone())
        })
    });
    view.update(vcx, |v, cx| v.toggle_redaction(CopyKind::Command, cx));
    assert_eq!(
        seen.borrow().as_slice(),
        [crate::hub::ChatEvent::CopySettingsChanged(
            crate::copy::CopySettings {
                redact_commands: false,
                redact_text: false
            }
        )]
    );
    view.update(vcx, |v, cx| v.copy_command_output(&key, cx));
    assert!(clipboard(vcx).contains(KEY));
    assert_eq!(toast(&view, vcx), Some(("Copiado".into(), false)));
}

#[gpui::test]
fn selecting_and_copying_command_output_applies_the_same_redaction(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    let key = row_key(&view, vcx, false);
    view.update(vcx, |v, cx| v.toggle(&key, cx));
    repaint(vcx);
    let g = group(&view, vcx, &key);
    assert!(g.bounds_of(0).is_some(), "the expanded output is painted");
    // Focus the output with a click, then Ctrl+A / Ctrl+C.
    vcx.simulate_click(at(&g, 0, 2), Modifiers::none());
    ui_kit::testing::press(vcx, "ctrl-a ctrl-c");
    let out = clipboard(vcx);
    assert!(out.contains("DONE") && !out.contains(KEY), "{out}");
    assert_eq!(
        toast(&view, vcx),
        Some(("Copiado · incluye 1 secreto oculto".into(), true))
    );
}

#[gpui::test]
fn assistant_text_is_not_redacted_unless_the_user_turns_it_on(cx: &mut TestAppContext) {
    let e = env(
        cx,
        FakeFactory::new(|_, _| {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::Text {
                    text: format!("Tu clave es {KEY}"),
                    delta: false,
                }),
                Step::now(AgentEvent::Status(AgentStatus::Done)),
            ]
        }),
    );
    let (view, vcx, rt) = thread_view(cx, &e);
    view.update(vcx, |v, cx| v.send("dime".into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status == ThreadStatus::Done));
    view.update(vcx, |v, cx| v.sync(cx));
    let key = row_key(&view, vcx, true);
    view.update(vcx, |v, cx| v.copy_message(&key, CopyMode::Markdown, cx));
    assert!(clipboard(vcx).contains(KEY));
    view.update(vcx, |v, cx| v.toggle_redaction(CopyKind::Text, cx));
    view.update(vcx, |v, cx| v.copy_message(&key, CopyMode::Markdown, cx));
    assert!(!clipboard(vcx).contains(KEY));
}

#[gpui::test]
fn copy_conversation_has_roles_and_follows_the_toggles(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    view.update(vcx, |v, cx| v.copy_conversation(cx));
    let md = clipboard(vcx);
    assert!(
        md.starts_with("## Usuario\n\ncorre env\n\n## Asistente\n\nPrimer párrafo"),
        "{md}"
    );
    assert!(md.contains("$ env") && md.contains("DONE"), "{md}");
    assert!(!md.contains(KEY), "command output is redacted by default");
    assert_eq!(
        toast(&view, vcx),
        Some(("Copiado · incluye 1 secreto oculto".into(), true))
    );
}

// ---------------------------------------------------------------------------------------------
// The hover action bar of a message ("Copiar" + `⋯`): stable under the pointer, no layout shift
// ---------------------------------------------------------------------------------------------

/// Bounds of the element whose debug selector is `name`, in the last frame.
fn bounds(vcx: &mut VisualTestContext, name: &str) -> Option<gpui::Bounds<Pixels>> {
    ui_kit::testing::bounds_of(vcx, name)
}

/// Was the bar of the row built in the last frame? (`debug_bounds` keeps stale entries.)
fn bar_shown(view: &Entity<ChatView>, vcx: &mut VisualTestContext, key: &RowKey) -> bool {
    view.read_with(vcx, |v, _| v.bar_built.as_ref() == Some(key))
}

/// Moves the pointer (no button) and paints, as the real app does after each mouse event.
fn hover_at(vcx: &mut VisualTestContext, p: Point<Pixels>) {
    vcx.simulate_mouse_move(p, None::<MouseButton>, Modifiers::none());
    repaint(vcx);
}

fn after(vcx: &mut VisualTestContext, ms: u64) {
    vcx.executor().advance_clock(Duration::from_millis(ms));
    repaint(vcx);
}

/// The pointer far from every row (top-left corner of the window).
const AWAY: gpui::Point<Pixels> = Point {
    x: gpui::px(2.0),
    y: gpui::px(2.0),
};

/// A point on the first line of the message text (left side, away from the bar).
fn on_text(vcx: &mut VisualTestContext, id: usize) -> Point<Pixels> {
    let r = bounds(vcx, &format!("row-hover-{id}")).expect("row painted");
    point(r.origin.x + gpui::px(40.0), r.origin.y + gpui::px(14.0))
}

#[gpui::test]
fn the_bar_stays_while_the_pointer_travels_from_the_text_onto_its_buttons(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let id = crate::row_view::row_id(&key);
    assert!(
        !bar_shown(&view, vcx, &key),
        "no bar until the pointer is over the row"
    );

    let start = on_text(vcx, id);
    hover_at(vcx, start);
    assert!(
        bar_shown(&view, vcx, &key),
        "the bar appears over the hovered row"
    );

    // Walk onto "Copiar" and then onto `⋯`, repainting after every event.
    let mut pos = start;
    for target in [format!("msg-copy-{id}"), format!("msg-more-{id}")] {
        let goal = bounds(vcx, &target)
            .unwrap_or_else(|| panic!("{target} is on screen"))
            .center();
        let steps = 12;
        let origin = pos;
        for i in 1..=steps {
            let f = i as f32 / steps as f32;
            let p = point(
                origin.x + (goal.x - origin.x) * f,
                origin.y + (goal.y - origin.y) * f,
            );
            hover_at(vcx, p);
            pos = p;
            assert!(
                bar_shown(&view, vcx, &key),
                "bar vanished at step {i} towards {target}"
            );
        }
        // Resting on the button, the pointer jitters: the bar must not blink.
        for (dx, dy) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0), (1.0, 1.0)] {
            hover_at(vcx, point(goal.x + gpui::px(dx), goal.y + gpui::px(dy)));
            assert!(bar_shown(&view, vcx, &key), "bar blinked over {target}");
        }
        assert_eq!(
            bounds(vcx, &target).map(|b| b.center()),
            Some(goal),
            "{target} did not move under the pointer"
        );
    }
}

#[gpui::test]
fn clicking_the_bar_button_after_hovering_it_copies_the_message(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let id = crate::row_view::row_id(&key);
    let start = on_text(vcx, id);
    hover_at(vcx, start);
    let goal = bounds(vcx, &format!("msg-copy-{id}")).unwrap().center();
    hover_at(vcx, point(goal.x, goal.y - gpui::px(30.0)));
    hover_at(vcx, goal);
    hover_at(vcx, point(goal.x + gpui::px(1.0), goal.y));
    vcx.simulate_click(goal, Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(
        clipboard(vcx),
        ANSWER,
        "the click on the bar copied the Markdown"
    );
    assert_eq!(toast(&view, vcx), Some(("Copiado".into(), false)));
}

#[gpui::test]
fn leaving_the_row_hides_the_bar_only_after_a_short_grace(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let id = crate::row_view::row_id(&key);
    let start = on_text(vcx, id);
    hover_at(vcx, start);
    assert!(bar_shown(&view, vcx, &key));

    hover_at(vcx, AWAY);
    assert!(
        bar_shown(&view, vcx, &key),
        "still there right after leaving (grace)"
    );
    after(vcx, 60);
    assert!(
        bar_shown(&view, vcx, &key),
        "still there inside the grace period"
    );
    // Coming back inside the grace cancels the hide.
    hover_at(vcx, start);
    after(vcx, 400);
    assert!(
        bar_shown(&view, vcx, &key),
        "re-entering cancelled the hide"
    );

    hover_at(vcx, AWAY);
    after(vcx, 250);
    assert!(!bar_shown(&view, vcx, &key), "gone once the grace passed");
}

#[gpui::test]
fn the_bar_appearing_does_not_move_the_row_or_its_text(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let id = crate::row_view::row_id(&key);
    let row = format!("row-hover-{id}");
    let before_row = bounds(vcx, &row).unwrap();
    let g = group(&view, vcx, &key);
    let before_text = g.bounds_of(0).unwrap();
    let before_body = bounds(vcx, "thread-body").unwrap();

    let p = on_text(vcx, id);
    hover_at(vcx, p);
    assert!(bar_shown(&view, vcx, &key));
    assert_eq!(
        bounds(vcx, &row),
        Some(before_row),
        "the row keeps its bounds"
    );
    assert_eq!(
        g.bounds_of(0),
        Some(before_text),
        "the text keeps its place"
    );
    assert_eq!(bounds(vcx, "thread-body"), Some(before_body));
}

#[gpui::test]
fn the_copied_hint_never_resizes_the_conversation(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let id = crate::row_view::row_id(&key);
    let before = bounds(vcx, "thread-body").unwrap();
    let row = bounds(vcx, &format!("row-hover-{id}")).unwrap();
    view.update(vcx, |v, cx| v.copy_message(&key, CopyMode::Markdown, cx));
    repaint(vcx);
    assert!(toast(&view, vcx).is_some());
    assert_eq!(
        bounds(vcx, "thread-body"),
        Some(before),
        "the list area keeps its size"
    );
    assert_eq!(bounds(vcx, &format!("row-hover-{id}")), Some(row));
}

#[gpui::test]
fn the_bar_is_reachable_and_stays_with_the_keyboard(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let g = group(&view, vcx, &key);
    // Click the message (focuses it), then take the pointer away.
    vcx.simulate_click(at(&g, 0, 3), Modifiers::none());
    hover_at(vcx, AWAY);
    after(vcx, 500);
    assert!(
        bar_shown(&view, vcx, &key),
        "a focused message keeps its bar for the keyboard"
    );
    ui_kit::testing::press(vcx, "tab enter");
    assert_eq!(clipboard(vcx), ANSWER, "Tab reached `Copiar`, Enter copied");
    after(vcx, 500);
    assert!(
        bar_shown(&view, vcx, &key),
        "focus inside the bar keeps it open"
    );
}

#[gpui::test]
fn the_bar_hanging_over_the_row_above_keeps_the_pointer_on_its_own_row(cx: &mut TestAppContext) {
    let (_e, view, vcx) = opened(cx);
    repaint(vcx);
    let key = row_key(&view, vcx, true);
    let id = crate::row_view::row_id(&key);
    let start = on_text(vcx, id);
    hover_at(vcx, start);
    let bar = bounds(vcx, &format!("msg-bar-{id}")).expect("bar painted");
    let row = bounds(vcx, &format!("row-hover-{id}")).unwrap();
    assert!(
        bar.origin.y < row.origin.y,
        "the bar hangs over the row above"
    );
    // Slide along the part of the bar that is outside its row, then back into the row.
    let y = bar.origin.y + gpui::px(3.0);
    for i in 0..10 {
        let x = bar.origin.x + gpui::px(4.0 + 6.0 * i as f32);
        hover_at(vcx, point(x, y));
        assert!(
            bar_shown(&view, vcx, &key),
            "bar lost over its overhang at x step {i}"
        );
        assert_eq!(
            view.read_with(vcx, |v, _| v.hover_row.clone()),
            Some(key.clone()),
            "the row above must not take the pointer"
        );
    }
    assert!(!view.read_with(vcx, |v, _| v.hover_leave.is_some()));
}
