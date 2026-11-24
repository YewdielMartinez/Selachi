//! gpui-level tests of the panel's keyboard and pointer contracts (`ui_kit::testing`: events go
//! straight into the test window, nothing touches the real desktop).

use std::cell::RefCell;

use gpui::{Context, Entity, TestAppContext, VisualTestContext, Window, px, size};
use store::{BuiltinPreset, LayoutConfig, PresetId};
use theme::Theme;
use ui_kit::testing;

use super::*;

struct Host {
    panel: Entity<LayoutPanel>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.panel.clone()
    }
}

type Seen = Rc<RefCell<Vec<LayoutConfig>>>;

/// The panel in a narrow window (rows stack their control under the label; the test text system
/// is wider than a real font, so a side-by-side control would sit outside the window).
fn open(cx: &mut TestAppContext) -> (Entity<LayoutPanel>, Seen, &mut VisualTestContext) {
    cx.update(crate::init);
    let seen: Seen = Rc::default();
    let sink = seen.clone();
    let (host, vcx) = testing::open_window(cx, |_, cx| {
        let panel = layout_panel(
            LayoutConfig::default(),
            None,
            Theme::dark(),
            move |layout, _, _| sink.borrow_mut().push(layout),
            cx,
        );
        panel.update(cx, |p, cx| p.set_embedded(true, cx));
        Host { panel }
    });
    let panel = host.read_with(vcx, |h, _| h.panel.clone());
    vcx.simulate_resize(size(px(420.0), px(4000.0)));
    for _ in 0..3 {
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
    }
    (panel, seen, vcx)
}

#[test]
fn a_roving_list_steps_and_clamps() {
    assert_eq!(stepped(None, 5, 1), 0);
    assert_eq!(stepped(None, 5, -1), 0);
    assert_eq!(stepped(Some(2), 5, 1), 3);
    assert_eq!(stepped(Some(2), 5, -1), 1);
    assert_eq!(stepped(Some(4), 5, 1), 4, "clamps at the end");
    assert_eq!(stepped(Some(0), 5, -1), 0, "clamps at the start");
}

#[test]
fn preset_ids_follow_the_preset_not_its_position() {
    let a = preset_key(&PresetId::Custom("mio".into()));
    let b = preset_key(&PresetId::Builtin(BuiltinPreset::Zen));
    assert_ne!(a, b);
    // Adding or deleting other presets never changes a preset's key.
    assert_eq!(a, preset_key(&PresetId::Custom("mio".into())));
    assert_ne!(
        preset_key(&PresetId::Custom("Zen".into())),
        b,
        "a custom named like a built-in is distinct"
    );
}

#[gpui::test]
fn clicking_a_toggle_applies_the_layout(cx: &mut TestAppContext) {
    let (panel, seen, vcx) = open(cx);
    assert!(!panel.read_with(vcx, |p, _| p.layout().zen));
    testing::click_id(vcx, "zen");
    vcx.run_until_parked();
    assert!(
        panel.read_with(vcx, |p, _| p.layout().zen),
        "the click reached the toggle"
    );
    assert!(
        seen.borrow().last().is_some_and(|c| c.resolve().zen),
        "and on_change got the new config"
    );
}

#[gpui::test]
fn tab_reaches_the_presets_and_the_arrows_pick_one(cx: &mut TestAppContext) {
    let (panel, seen, vcx) = open(cx);
    // The first tab stop is the preset grid (the disabled reset button is skipped).
    testing::press(vcx, "tab");
    let handle = panel.read_with(vcx, |p, _| p.presets_focus.clone());
    assert!(
        testing::is_focused(vcx, &handle),
        "Tab landed on the preset list"
    );
    let before = panel.read_with(vcx, |p, _| p.state.active_preset());
    testing::press(vcx, "right");
    vcx.run_until_parked();
    let after = panel.read_with(vcx, |p, _| p.state.active_preset());
    assert_ne!(before, after, "the Right action picked the next preset");
    assert_eq!(seen.borrow().len(), 1);
    testing::press(vcx, "left");
    vcx.run_until_parked();
    assert_eq!(panel.read_with(vcx, |p, _| p.state.active_preset()), before);
}

#[gpui::test]
fn naming_a_preset_with_the_text_field(cx: &mut TestAppContext) {
    let (panel, seen, vcx) = open(cx);
    testing::click_id(vcx, "preset-new");
    vcx.run_until_parked();
    let field = panel
        .read_with(vcx, |p, _| p.naming.clone())
        .expect("the name field opened");
    let focus = field.read_with(vcx, gpui::Focusable::focus_handle);
    assert!(testing::is_focused(vcx, &focus), "the field has the focus");
    testing::type_text(vcx, "Mi preset");
    testing::press(vcx, "enter");
    vcx.run_until_parked();
    assert!(
        panel.read_with(vcx, |p, _| p.naming.is_none()),
        "Enter confirmed"
    );
    let last = seen.borrow().last().cloned().expect("on_change fired");
    assert!(
        last.custom_presets.iter().any(|p| p.name == "Mi preset"),
        "{:?}",
        last.custom_presets
    );
}

#[gpui::test]
fn escape_cancels_the_naming_and_returns_the_focus(cx: &mut TestAppContext) {
    let (panel, _, vcx) = open(cx);
    testing::click_id(vcx, "preset-new");
    vcx.run_until_parked();
    testing::press(vcx, "escape");
    vcx.run_until_parked();
    assert!(panel.read_with(vcx, |p, _| p.naming.is_none()));
    let root = panel.read_with(vcx, |p, _| p.root_focus.clone());
    assert!(testing::is_focused(vcx, &root));
}
