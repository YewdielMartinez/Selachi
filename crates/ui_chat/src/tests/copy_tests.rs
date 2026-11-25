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
