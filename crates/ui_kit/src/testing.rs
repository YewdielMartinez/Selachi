//! Test helpers for crates that test their views (enable the `test-support` feature of `ui_kit`
//! in their `dev-dependencies`). They drive the window through gpui's `VisualTestContext`
//! (never the real desktop): keystrokes and clicks are dispatched straight into the window.
//!
//! ```ignore
//! // Cargo.toml: [dev-dependencies] ui_kit = { path = "../ui_kit", features = ["test-support"] }
//! #[gpui::test]
//! fn tab_then_enter_activates_the_button(cx: &mut TestAppContext) {
//!     let hits = Rc::new(Cell::new(0));
//!     let (_view, vcx) = ui_kit::testing::open_window(cx, |_, _| MyView::new(hits.clone()));
//!     ui_kit::testing::press(vcx, "tab enter");
//!     assert_eq!(hits.get(), 1);
//!     ui_kit::testing::click_id(vcx, "save");
//!     assert_eq!(hits.get(), 2);
//! }
//! ```
//!
//! `click_id` finds a ui_kit control by its element id (every clickable component records it;
//! a `Select` trigger is `<id>-trigger` and its options `<id>-<value>`).
#![allow(clippy::expect_used, clippy::unwrap_used)]

pub use gpui::{TestAppContext, VisualTestContext};

use gpui::{
    Bounds, Context, Entity, FocusHandle, KeyUpEvent, Keystroke, Modifiers, Pixels, Point, Render,
    Window,
};

/// Registers the kit's key bindings in a test app (windows opened with [`open_window`] get it for free).
pub fn init(cx: &mut TestAppContext) {
    cx.update(crate::init);
}

/// Opens a window whose root view is built by `build`, with the kit initialised. Returns the root
/// entity and the context that stands for that window.
pub fn open_window<V: Render + 'static>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
) -> (Entity<V>, &mut VisualTestContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(build);
    // An inactive window delivers no focus-change events (blur, focus-in); a real one is active.
    // The first frame must exist before bounds can be asked for.
    vcx.update(|window, _| {
        window.activate_window();
        window.refresh();
    });
    vcx.run_until_parked();
    (view, vcx)
}

/// Types space-separated keystrokes: `press(cx, "tab enter")`, `press(cx, "ctrl-a backspace")`.
/// Like a keyboard, each key is a press **and** a release (gpui runs the `Enter` / `Space` click
/// of a focused control on the release).
pub fn press(cx: &mut VisualTestContext, keys: &str) {
    for key in keys.split(' ').filter(|k| !k.is_empty()) {
        cx.simulate_keystrokes(key);
        if let Ok(keystroke) = Keystroke::parse(key) {
            cx.simulate_event(KeyUpEvent { keystroke });
        }
    }
}

/// Types plain text into the focused text input (IME path, like real typing).
pub fn type_text(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
}

/// `Tab` (`shift-tab` for [`focus_prev`]).
pub fn focus_next(cx: &mut VisualTestContext) {
    press(cx, "tab");
}

pub fn focus_prev(cx: &mut VisualTestContext) {
    press(cx, "shift-tab");
}

/// A left click at `position` (window coordinates).
pub fn click_at(cx: &mut VisualTestContext, position: Point<Pixels>) {
    cx.simulate_click(position, Modifiers::none());
}

/// Bounds of the ui_kit control with element id `id` in the last frame.
pub fn bounds_of(cx: &mut VisualTestContext, id: &str) -> Option<Bounds<Pixels>> {
    // gpui wants a `'static` selector; leaking a few bytes per call is fine in a test.
    cx.debug_bounds(Box::leak(id.to_string().into_boxed_str()))
}

/// A left click at the center of the control with element id `id`. Panics when it is not on screen.
pub fn click_id(cx: &mut VisualTestContext, id: &str) {
    let b =
        bounds_of(cx, id).unwrap_or_else(|| panic!("no control with id `{id}` in the last frame"));
    click_at(cx, b.center());
    cx.run_until_parked();
}

/// Is `handle` the focused element of the window?
pub fn is_focused(cx: &mut VisualTestContext, handle: &FocusHandle) -> bool {
    cx.update(|window, _| handle.is_focused(window))
}
