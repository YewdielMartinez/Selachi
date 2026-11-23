//! Keyboard focus for the interactive components.
//!
//! * Every clickable component (`Button`, `IconButton`, `Pill`, `Toggle`, ...) is a tab stop
//!   (`tab_index(0)`, dispatch-tree order). `Enter` / `Space` on the focused one runs its click
//!   handler (gpui does that for any focused element with a click listener).
//! * `Tab` / `Shift+Tab` move focus: this vendored gpui does **not** do it by itself, so
//!   [`init`](crate::init) binds `tab` / `shift-tab` to [`FocusNext`] / [`FocusPrev`] and every
//!   focusable component handles them. Where nothing handles them (the terminal) the key falls
//!   through unchanged, so a shell still receives its `Tab`.
//! * The ring (accent, 2 px **outside** the control, same radii) is painted only for keyboard
//!   focus: gpui has no `:focus-visible`, so the last input modality is tracked instead (a
//!   keystroke = keyboard, a mouse press = pointer).
//! * A press on a component does **not** take focus (a toolbar click must not steal it from the
//!   terminal), so a ring never follows a mouse click.
//! * Opt-out for decorative use: every component has `.focusable(false)`; disabled ones are never
//!   focusable.

use std::cell::{Cell, RefCell};

use gpui::{
    App, BorderStyle, Bounds, Corners, Div, Edges, ElementId, FocusHandle, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Rgba, SharedString, Stateful, Styled, Window, actions,
    canvas, prelude::*, px, quad, transparent_black,
};

actions!(
    ui_kit_focus,
    [
        /// Moves focus to the next tab stop (`Tab`).
        FocusNext,
        /// Moves focus to the previous tab stop (`Shift+Tab`).
        FocusPrev,
    ]
);

/// How the user last interacted: decides whether a focused control shows its ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputModality {
    /// The last input was a mouse press (or nothing yet): no ring.
    Pointer,
    /// The last input was a keystroke: focused controls show their ring.
    Keyboard,
}

thread_local! {
    static MODALITY: Cell<InputModality> = const { Cell::new(InputModality::Pointer) };
    static FORCED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The last input modality seen by any window of this thread.
pub fn input_modality() -> InputModality {
    MODALITY.with(Cell::get)
}

/// Sets the modality; `true` when it changed.
pub fn set_input_modality(m: InputModality) -> bool {
    MODALITY.with(|c| c.replace(m) != m)
}

/// Shows the focus ring on the components with these ids whether or not they are focused
/// (galleries and screenshots; no synthetic input is needed). An empty slice clears it.
pub fn debug_force_focus_ring<S: AsRef<str>>(ids: &[S]) {
    FORCED.with(|f| *f.borrow_mut() = ids.iter().map(|s| s.as_ref().to_string()).collect());
}

fn forced(id: &str) -> bool {
    FORCED.with(|f| {
        let f = f.borrow();
        !f.is_empty() && f.iter().any(|x| x == id)
    })
}

/// The decision, pure: a ring shows for a forced id, or for a focused control after keyboard input.
pub fn ring_visible(focused: bool, modality: InputModality, forced: bool) -> bool {
    forced || (focused && modality == InputModality::Keyboard)
}

/// Installed once per app: any real keystroke switches to keyboard modality, and `Tab` /
/// `Shift+Tab` move focus when **nothing** is focused (no control is then on the dispatch path to
/// run [`FocusNext`]; with a focused control its own handler does it, and a terminal that has
/// focus keeps its `Tab`).
pub(crate) fn install_modality_tracking(cx: &mut App) {
    cx.intercept_keystrokes(|ev, window, cx| {
        let k = &ev.keystroke;
        if matches!(k.key.as_str(), "shift" | "control" | "alt" | "platform" | "function") {
            return;
        }
        if set_input_modality(InputModality::Keyboard) {
            window.refresh();
        }
        let m = k.modifiers;
        if k.key == "tab" && !(m.control || m.alt || m.platform) && window.focused(cx).is_none() {
            if m.shift {
                window.focus_prev();
            } else {
                window.focus_next();
            }
            cx.stop_propagation();
        }
    })
    .detach();
}

fn focus_key(id: &ElementId) -> ElementId {
    ElementId::NamedChild(Box::new(id.clone()), SharedString::new_static("ui-kit-focus"))
}

/// The tab-stop handle of the component with element id `id`, kept across frames.
pub(crate) fn handle(window: &mut Window, cx: &mut App, id: &ElementId) -> FocusHandle {
    crate::init(cx);
    let state = window.use_keyed_state(focus_key(id), cx, |_, cx| cx.focus_handle().tab_index(0).tab_stop(true));
    state.read(cx).clone()
}

/// The ring, as the last child of a `relative` control: paints nothing unless
/// [`ring_visible`], and while painted it also records mouse presses (pointer modality).
pub(crate) fn ring_element(handle: FocusHandle, id: String, radius: f32, color: Rgba) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, (), window, _| {
            window.on_mouse_event(|_: &MouseDownEvent, phase, window, _| {
                if phase == gpui::DispatchPhase::Capture && set_input_modality(InputModality::Pointer) {
                    window.refresh();
                }
            });
            if ring_visible(handle.is_focused(window), input_modality(), forced(&id)) {
                window.paint_quad(quad(
                    bounds.dilate(px(2.0)),
                    Corners::all(px(radius + 2.0)),
                    transparent_black(),
                    Edges::all(px(2.0)),
                    color,
                    BorderStyle::Solid,
                ));
            }
        },
    )
    .absolute()
    // Without explicit insets an absolute child sits at its static position (after the padding).
    .top(px(0.0))
    .left(px(0.0))
    .size_full()
}

/// Everything a focusable control needs on its root: tab stop, ring, `Tab` handling and no
/// focus-steal on press. `radius` is the control's corner radius in px.
pub(crate) fn wire(
    el: Stateful<Div>,
    handle: Option<FocusHandle>,
    id: &ElementId,
    radius: f32,
    accent: Rgba,
) -> Stateful<Div> {
    wire_with(el, handle, id, radius, accent, false)
}

/// [`wire`], where `press_focuses` lets a mouse press focus the control (entities that own the
/// focus, like `Select`).
pub(crate) fn wire_with(
    el: Stateful<Div>,
    handle: Option<FocusHandle>,
    id: &ElementId,
    radius: f32,
    accent: Rgba,
    press_focuses: bool,
) -> Stateful<Div> {
    // A no-op outside tests (`testing::click_id` finds the control by it).
    let el = el.debug_selector({
        let id = id.to_string();
        move || id
    });
    let Some(handle) = handle else { return el };
    el.relative()
        .track_focus(&handle)
        .on_action(|_: &FocusNext, window, _| window.focus_next())
        .on_action(|_: &FocusPrev, window, _| window.focus_prev())
        .when(!press_focuses, |d| d.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default()))
        .child(ring_element(handle, id.to_string(), radius, accent))
}

/// Binds `Tab` / `Shift+Tab` (called by [`init`](crate::init)).
pub(crate) fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([KeyBinding::new("tab", FocusNext, None), KeyBinding::new("shift-tab", FocusPrev, None)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_needs_keyboard_focus() {
        use InputModality::{Keyboard, Pointer};
        assert!(ring_visible(true, Keyboard, false));
        assert!(!ring_visible(true, Pointer, false), "a mouse-focused control shows no ring");
        assert!(!ring_visible(false, Keyboard, false), "an unfocused control shows no ring");
        assert!(ring_visible(false, Pointer, true), "forced for screenshots");
    }

    #[test]
    fn forcing_is_by_id_and_clearable() {
        debug_force_focus_ring(&["a", "b"]);
        assert!(forced("a") && forced("b") && !forced("c"));
        debug_force_focus_ring::<&str>(&[]);
        assert!(!forced("a"));
    }

    #[test]
    fn modality_reports_changes() {
        set_input_modality(InputModality::Pointer);
        assert!(set_input_modality(InputModality::Keyboard));
        assert!(!set_input_modality(InputModality::Keyboard));
        assert_eq!(input_modality(), InputModality::Keyboard);
        set_input_modality(InputModality::Pointer);
    }
}
