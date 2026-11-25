//! Memory regressions of long sessions (docs/olas/ola-9.md, 9C): rows that left the viewport do
//! not keep their shaped text alive, command outputs are shaped only in part, and a composer
//! thumbnail is a small bitmap that goes away with its attachment.

use super::*;
use crate::rows::RowKind;
use agents_driver::ToolKind;
use gpui::{px, size};
use serde_json::json;

const COMMANDS: usize = 120;
const OUT_BYTES: usize = 40_000;

fn output(n: usize) -> String {
    let mut s = String::new();
    let mut k = 0;
    while s.len() < OUT_BYTES {
        s.push_str(&format!("   Compiling dep-{n}-{k} v0.1.0 (path/dep-{k})\n"));
        k += 1;
    }
    s
}

fn long_script() -> Arc<FakeFactory> {
    FakeFactory::new(|_, _| {
        let mut v = vec![Step::now(AgentEvent::Status(AgentStatus::Working))];
        for n in 0..COMMANDS {
            let id = format!("c{n}");
            v.push(Step::now(AgentEvent::ToolCall {
                id: id.clone(),
                kind: ToolKind::Command,
                title: format!("cargo test {n}"),
                input_json: json!({ "command": format!("cargo test {n}") }),
            }));
            v.push(Step::now(AgentEvent::ToolResult {
                id,
                ok: true,
                summary: output(n),
            }));
        }
        v.push(Step::now(AgentEvent::Status(AgentStatus::Done)));
        v
    })
}

fn repaint(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.update(|w, _| w.refresh());
        vcx.run_until_parked();
    }
}

fn long_view(cx: &mut TestAppContext) -> (Env, Entity<ChatView>, &mut VisualTestContext) {
    let e = env(cx, long_script());
    let (view, vcx, rt) = thread_view(cx, &e);
    vcx.simulate_resize(size(px(1100.0), px(800.0)));
    view.update(vcx, |v, cx| v.send("corre todo".into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(20), |s| s.status == ThreadStatus::Done));
    settle(vcx, "rows", |vcx| {
        view.update(vcx, |v, cx| v.sync(cx));
        view.read_with(vcx, |v, _| {
            v.rows
                .iter()
                .filter(|r| matches!(r.kind, RowKind::Command { .. }))
                .count()
                == COMMANDS
        })
    });
    (e, view, vcx)
}

#[gpui::test]
fn rows_out_of_the_viewport_release_their_selection_groups(cx: &mut TestAppContext) {
    let (_e, view, vcx) = long_view(cx);
    view.update(vcx, |v, cx| v.debug_expand_all(cx));
    repaint(vcx);
    let at_bottom = view.read_with(vcx, |v, _| v.selection_group_count());
    assert!(
        (1..COMMANDS / 4).contains(&at_bottom),
        "only the painted rows have a group, got {at_bottom} of {COMMANDS}"
    );
    // Walk the whole thread from the top: the live groups never pile up.
    let rows = view.read_with(vcx, |v, _| v.rows.len());
    let mut peak = 0;
    for ix in (0..rows).step_by(2) {
        view.update(vcx, |v, cx| v.debug_reveal(ix, cx));
        repaint(vcx);
        peak = peak.max(view.read_with(vcx, |v, _| v.selection_group_count()));
    }
    assert!(
        peak < COMMANDS / 4,
        "groups alive while scrolling: {peak} (rows: {rows})"
    );
}

#[gpui::test]
fn a_group_with_a_selection_survives_scrolling_away(cx: &mut TestAppContext) {
    let (_e, view, vcx) = long_view(cx);
    view.update(vcx, |v, cx| v.debug_expand_all(cx));
    repaint(vcx);
    let (key, group) = view.read_with(vcx, |v, _| {
        let (k, g) = v.selections.iter().next().expect("a painted group");
        (k.clone(), g.clone())
    });
    group.set_selection(ui_kit::SelPos::new(0, 0), ui_kit::SelPos::new(0, 5));
    view.update(vcx, |v, cx| v.debug_reveal(0, cx));
    repaint(vcx);
    assert!(
        view.read_with(vcx, |v, _| v.selections.contains_key(&key)),
        "the user's selection must not vanish"
    );
}

#[gpui::test]
fn a_big_output_is_shown_by_its_tail_and_copied_whole(cx: &mut TestAppContext) {
    let (_e, view, vcx) = long_view(cx);
    let key = view.read_with(vcx, |v, _| {
        v.rows
            .iter()
            .rev()
            .find(|r| matches!(r.kind, RowKind::Command { .. }))
            .map(|r| r.key.clone())
            .unwrap()
    });
    view.update(vcx, |v, cx| v.toggle(&key, cx));
    repaint(vcx);
    let g = view
        .read_with(vcx, |v, _| v.selections.get(&(key.clone(), 0)).cloned())
        .expect("the expanded output is painted");
    g.select_all();
    let (_, end) = g.selection().unwrap();
    assert!(
        end.offset < crate::util::OUTPUT_SHOW_BYTES + 200,
        "only the tail is shaped: {}",
        end.offset
    );
    view.update(vcx, |v, cx| v.copy_command_output(&key, cx));
    let copied = vcx
        .read_from_clipboard()
        .and_then(|i| i.text())
        .unwrap_or_default();
    assert!(
        copied.len() >= OUT_BYTES,
        "Copiar salida copies it all: {}",
        copied.len()
    );
}

#[gpui::test]
fn composer_thumbnails_are_small_and_leave_with_the_attachment(cx: &mut TestAppContext) {
    let e = env(cx, FakeFactory::silent());
    let (c, vcx, _events) = composer(cx, &e);
    let path = e.dir.path().join("shot.png");
    image::RgbaImage::from_fn(2000, 1200, |x, y| image::Rgba([x as u8, y as u8, 1, 255]))
        .save(&path)
        .unwrap();
    c.update(vcx, |c, cx| c.add_attachments([path.clone()], cx));
    settle(vcx, "the thumbnail decodes", |vcx| {
        vcx.update(|w, _| w.refresh());
        c.read_with(vcx, |c, _| c.thumb_ready_count()) == 1
    });
    // Sending takes the attachments; the next frame drops their thumbnails.
    c.update(vcx, |c, cx| {
        c.set_text("mira", cx);
        c.submit(cx);
    });
    vcx.update(|w, _| w.refresh());
    vcx.run_until_parked();
    assert_eq!(c.read_with(vcx, |c, _| c.thumb_count()), 0);
}
