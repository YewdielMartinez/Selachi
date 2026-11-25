//! Layout tests: narrow chat column (~240 px) must keep message bubbles and text inside the
//! column without horizontal overflow.

use super::*;
use crate::row_view::row_id;
use crate::rows::RowKind;
use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Window, div,
    px, size,
};

const LONG_USER_MSG: &str = "Esta es una frase bastante larga que debería envolverse dentro de la burbuja del usuario sin salirse del borde derecho";
const LONG_TOKEN_MSG: &str = "Reinicia C:\\Users\\venta\\Desktop\\Terminal\\crates\\ui_chat\\src\\very_long_unbroken_token_path_name.rs ahora";
const LONG_ASSISTANT_MD: &str = "Aquí tienes un párrafo largo de respuesta del asistente que debe envolverse correctamente dentro de la columna estrecha con `un_identificador_muy_largo_sin_espacios_para_probar_el_corte`.\n\n- Primer elemento de lista con texto largo que también debe ajustarse a la columna\n\n```rust\nlet very_long_line_of_code = \"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\";\n```\n";

fn narrow_thread<'a>(
    cx: &'a mut TestAppContext,
    user_msg: &str,
    assistant_md: &str,
    width: f32,
) -> (Env, Entity<ChatView>, &'a mut VisualTestContext) {
    let md = assistant_md.to_string();
    let e = env(
        cx,
        FakeFactory::new(move |_, _| {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::Text {
                    text: md.clone(),
                    delta: false,
                }),
                Step::now(AgentEvent::Status(AgentStatus::Done)),
            ]
        }),
    );
    let (view, vcx, rt) = thread_view(cx, &e);
    vcx.simulate_resize(size(px(width), px(1800.0)));
    vcx.run_until_parked();
    view.update(vcx, |v, cx| v.send(user_msg.into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status == ThreadStatus::Done));
    settle(vcx, "rows synced", |vcx| {
        view.update(vcx, |v, cx| v.sync(cx));
        view.read_with(vcx, |v, _| {
            v.rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::Text { .. }))
        })
    });
    vcx.update(|w, _| w.refresh());
    vcx.run_until_parked();
    (e, view, vcx)
}

#[gpui::test]
fn narrow_chat_wraps_user_and_assistant_bubbles_within_column(cx: &mut TestAppContext) {
    let width = 240.0;
    let (_e, view, vcx) = narrow_thread(cx, LONG_USER_MSG, LONG_ASSISTANT_MD, width);

    let (user_key, text_key) = view.read_with(vcx, |v, _| {
        let u = v
            .rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::User { .. }))
            .map(|r| r.key.clone())
            .expect("user row");
        let t = v
            .rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::Text { .. }))
            .map(|r| r.key.clone())
            .expect("text row");
        (u, t)
    });

    let bubble_sel = format!("user-bubble-{}", row_id(&user_key));
    let bubble_b = ui_kit::testing::bounds_of(vcx, &bubble_sel).expect("user bubble bounds");
    assert!(
        bubble_b.left() >= px(0.0) && bubble_b.right() <= px(width),
        "user bubble escaped column 0..{width}: {bubble_b:?}"
    );

    let user_g = view
        .read_with(vcx, |v, _| {
            v.selections.get(&(user_key.clone(), 0)).cloned()
        })
        .expect("user selection group");
    let user_b = user_g.bounds_of(0).expect("user text bounds");
    assert!(
        user_b.left() >= bubble_b.left() && user_b.right() <= bubble_b.right(),
        "user text {user_b:?} escaped its bubble {bubble_b:?}"
    );
    assert!(
        user_b.size.height > px(30.0),
        "long user text should wrap onto multiple lines: {user_b:?}"
    );

    let text_g = view
        .read_with(vcx, |v, _| {
            v.selections.get(&(text_key.clone(), 0)).cloned()
        })
        .expect("assistant selection group");
    for seg in text_g.painted_segments() {
        let b = text_g.bounds_of(seg).expect("segment bounds");
        assert!(
            b.left() >= px(0.0) && b.right() <= px(width),
            "assistant seg {seg} escaped column 0..{width}: {b:?}"
        );
    }
}

#[gpui::test]
fn narrow_chat_contains_long_unbroken_token_in_user_bubble(cx: &mut TestAppContext) {
    let width = 240.0;
    let (_e, view, vcx) = narrow_thread(cx, LONG_TOKEN_MSG, "ok", width);
    let user_key = view.read_with(vcx, |v, _| {
        v.rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::User { .. }))
            .map(|r| r.key.clone())
            .expect("user row")
    });
    let bubble_sel = format!("user-bubble-{}", row_id(&user_key));
    let bubble_b = ui_kit::testing::bounds_of(vcx, &bubble_sel).expect("user bubble bounds");
    assert!(
        bubble_b.left() >= px(0.0) && bubble_b.right() <= px(width),
        "long token bubble escaped column 0..{width}: {bubble_b:?}"
    );
    let user_g = view
        .read_with(vcx, |v, _| v.selections.get(&(user_key, 0)).cloned())
        .expect("user selection group");
    let user_b = user_g.bounds_of(0).expect("user text bounds");
    assert!(
        user_b.left() >= bubble_b.left() && user_b.right() <= bubble_b.right(),
        "long token text {user_b:?} escaped its bubble {bubble_b:?}"
    );
    assert!(
        user_b.size.height > px(40.0),
        "long unbroken token should wrap onto multiple lines: {user_b:?}"
    );
}

#[gpui::test]
fn narrow_chat_in_split_pane_and_after_resize_stays_within_column(cx: &mut TestAppContext) {
    struct SplitHost {
        chat: Entity<ChatView>,
        pane_w: f32,
    }
    impl Render for SplitHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_row()
                .child(div().w(px(self.pane_w)).h_full().child(self.chat.clone()))
                .child(div().flex_1().h_full())
        }
    }

    let e = env(
        cx,
        FakeFactory::new(|_, _| {
            vec![
                Step::now(AgentEvent::Status(AgentStatus::Working)),
                Step::now(AgentEvent::Text {
                    text: LONG_ASSISTANT_MD.into(),
                    delta: false,
                }),
                Step::now(AgentEvent::Status(AgentStatus::Done)),
            ]
        }),
    );
    let mut spec = ThreadSpec::new(e.project(), AgentKind::ClaudeCode);
    spec.permission = PermissionMode::Ask;
    let rec = e.store.create_thread(spec).unwrap();
    let rt = e.store.open(rec.id).unwrap();
    let hub = e.hub;
    let chat_cell: Rc<RefCell<Option<Entity<ChatView>>>> = Rc::new(RefCell::new(None));
    let cell2 = chat_cell.clone();
    let (host, vcx) = cx.add_window_view(move |_, cx| {
        let chat = cx.new(|cx| ChatView::loading(hub, Theme::dark(), cx));
        *cell2.borrow_mut() = Some(chat.clone());
        SplitHost {
            chat,
            pane_w: 720.0,
        }
    });
    let view = chat_cell.borrow().clone().unwrap();
    let rt2 = rt.clone();
    view.update(vcx, |v, cx| v.bind_runtime(rt2, cx));
    vcx.simulate_resize(size(px(1200.0), px(1800.0)));
    vcx.run_until_parked();
    view.update(vcx, |v, cx| v.send(LONG_USER_MSG.into(), vec![], cx));
    assert!(rt.wait_for(Duration::from_secs(10), |s| s.status == ThreadStatus::Done));
    settle(vcx, "rows synced", |vcx| {
        view.update(vcx, |v, cx| v.sync(cx));
        view.read_with(vcx, |v, _| {
            v.rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::Text { .. }))
        })
    });
    vcx.update(|w, _| w.refresh());
    vcx.run_until_parked();

    // Now shrink the split pane from 720 px down to 240 px and repaint.
    host.update(vcx, |h, cx| {
        h.pane_w = 240.0;
        cx.notify();
    });
    vcx.update(|w, _| w.refresh());
    vcx.run_until_parked();

    let (user_key, text_key) = view.read_with(vcx, |v, _| {
        let u = v
            .rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::User { .. }))
            .map(|r| r.key.clone())
            .expect("user row");
        let t = v
            .rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::Text { .. }))
            .map(|r| r.key.clone())
            .expect("text row");
        (u, t)
    });
    let bubble_sel = format!("user-bubble-{}", row_id(&user_key));
    let bubble_b = ui_kit::testing::bounds_of(vcx, &bubble_sel).expect("split user bubble bounds");
    assert!(
        bubble_b.left() >= px(0.0) && bubble_b.right() <= px(240.0),
        "split user bubble escaped 240px pane after resize: {bubble_b:?}"
    );
    let user_g = view
        .read_with(vcx, |v, _| v.selections.get(&(user_key, 0)).cloned())
        .expect("user group");
    let user_b = user_g.bounds_of(0).expect("user bounds");
    assert!(
        user_b.left() >= bubble_b.left() && user_b.right() <= bubble_b.right(),
        "split user text {user_b:?} escaped its bubble {bubble_b:?}"
    );
    let text_g = view
        .read_with(vcx, |v, _| v.selections.get(&(text_key, 0)).cloned())
        .expect("text group");
    for seg in text_g.painted_segments() {
        let b = text_g.bounds_of(seg).expect("seg bounds");
        assert!(
            b.left() >= px(0.0) && b.right() <= px(240.0),
            "split assistant seg {seg} escaped 240px pane: {b:?}"
        );
    }
}
