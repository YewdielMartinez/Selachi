//! `Popover`: a floating panel with physical origin (PLAN section 11).
//!
//! It grows out of the trigger that opens it, closes with Esc or a click outside, and costs
//! nothing while open: the 200 ms `EASE.pop` entrance is a one-shot animation, afterwards the
//! window is idle again. With reduced motion (see [`crate::set_reduced_motion`]) it simply
//! appears.
//!
//! # Esc
//!
//! Two paths close an open popover, both ending in `on_dismiss`:
//!
//! * the panel is a key context ([`CONTEXT`]) that handles the [`Cancel`] action (`Esc` is bound
//!   to it by [`init`](crate::init)): it runs when focus is inside the panel and needs no help
//!   from the app;
//! * while open, an interceptor catches `Esc` wherever the focus is (a terminal behind the panel
//!   must not receive it). On by default, `.esc_interceptor(false)` leaves only the focus-scoped
//!   action, for hosts that route `Esc` themselves.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{Animation, AnimationExt};
use gpui::{
    AnyElement, App, Bounds, Context, Corner, DispatchPhase, IntoElement, Modifiers,
    MouseDownEvent, ParentElement, Pixels, RenderOnce, SharedString, Subscription, Window, actions,
    anchored, canvas, deferred, div, point, prelude::*, px,
};
use theme::{
    Radius, Theme,
    motion::{POP, SHEET, duration::BASE},
};

use crate::motion::{clamped, reduced_motion};

actions!(
    ui_kit_popover,
    [
        /// `Esc` inside an open popover: dismiss it (runs `on_dismiss`).
        Cancel,
    ]
);

/// Key context of the popover panel.
pub const CONTEXT: &str = "SelachiPopover";

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new("escape", Cancel, Some(CONTEXT))]);
}

/// Where the panel opens relative to its trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Placement {
    /// Below the trigger, left edges aligned.
    #[default]
    BottomStart,
    /// Below the trigger, right edges aligned.
    BottomEnd,
    /// Above the trigger, left edges aligned.
    TopStart,
    /// Above the trigger, right edges aligned.
    TopEnd,
}

impl Placement {
    fn below(self) -> bool {
        matches!(self, Placement::BottomStart | Placement::BottomEnd)
    }

    fn end(self) -> bool {
        matches!(self, Placement::BottomEnd | Placement::TopEnd)
    }

    /// Corner of the *panel* that touches the trigger.
    fn corner(self) -> Corner {
        match self {
            Placement::BottomStart => Corner::TopLeft,
            Placement::BottomEnd => Corner::TopRight,
            Placement::TopStart => Corner::BottomLeft,
            Placement::TopEnd => Corner::BottomRight,
        }
    }
}

/// Windows narrower than this (logical px) are "XS": a popover that asked for it opens as a
/// bottom sheet (docs/DESIGN.md, Responsive).
pub const SHEET_BELOW_WIDTH: f32 = 720.0;

type DismissFn = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// A trigger plus a floating panel. The caller owns `open` and flips it from the trigger's
/// click handler and from `on_dismiss` (Esc / click outside).
///
/// ```ignore
/// let this = cx.entity();
/// Popover::new("layout-pop", &theme)
///     .open(self.open)
///     .placement(Placement::BottomEnd)
///     .on_dismiss({
///         let this = this.clone();
///         move |_, cx| this.update(cx, |t, cx| { t.open = false; cx.notify(); })
///     })
///     .trigger(IconButton::new("layout-btn", &theme, Icon::Layout).on_click(/* toggle */))
///     .child("panel content")
/// ```
#[derive(IntoElement)]
pub struct Popover {
    id: SharedString,
    theme: Theme,
    trigger: Option<AnyElement>,
    open: bool,
    placement: Placement,
    width: Option<f32>,
    padding: f32,
    sheet_when_narrow: bool,
    esc_interceptor: bool,
    on_dismiss: Option<DismissFn>,
    children: Vec<AnyElement>,
}

impl Popover {
    pub fn new(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            trigger: None,
            open: false,
            placement: Placement::default(),
            width: None,
            padding: 8.0,
            sheet_when_narrow: false,
            esc_interceptor: true,
            on_dismiss: None,
            children: Vec::new(),
        }
    }

    /// The element the panel grows out of (always rendered).
    pub fn trigger(mut self, trigger: impl IntoElement) -> Self {
        self.trigger = Some(trigger.into_any_element());
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// Fixed panel width in px (default: fit content).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Inner padding in px (default 8).
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    /// In a window narrower than [`SHEET_BELOW_WIDTH`] open as a bottom sheet (full width, docked
    /// to the bottom edge, `EASE.sheet` entrance) instead of next to the trigger.
    pub fn sheet_when_narrow(mut self, on: bool) -> Self {
        self.sheet_when_narrow = on;
        self
    }

    /// Whether an open popover also catches `Esc` wherever the focus is (default `true`). `false`
    /// keeps only the [`Cancel`] action of the panel, which needs focus inside it.
    pub fn esc_interceptor(mut self, on: bool) -> Self {
        self.esc_interceptor = on;
        self
    }

    /// Called on Esc or a mouse press outside both the panel and the trigger.
    pub fn on_dismiss(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(f));
        self
    }
}

impl ParentElement for Popover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// Esc interceptor of an open popover. Dropped (and the subscription with it) as soon as a frame
/// is rendered without the popover being open.
struct EscWatch {
    dismiss: Rc<RefCell<Option<DismissFn>>>,
    _sub: Subscription,
}

impl EscWatch {
    fn new(cx: &mut Context<Self>) -> Self {
        let dismiss: Rc<RefCell<Option<DismissFn>>> = Rc::new(RefCell::new(None));
        let handler = dismiss.clone();
        let sub = cx.intercept_keystrokes(move |ev, window, cx| {
            if ev.keystroke.key == "escape" && ev.keystroke.modifiers == Modifiers::default() {
                let f = handler.borrow().clone();
                if let Some(f) = f {
                    f(window, cx);
                    // The terminal must not also receive this Esc.
                    cx.stop_propagation();
                }
            }
        });
        Self { dismiss, _sub: sub }
    }
}

/// Distance between the trigger and the panel.
const GAP: f32 = 6.0;
/// How far the panel travels while it fades in.
const TRAVEL: f32 = 8.0;

impl RenderOnce for Popover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        crate::ensure_init(cx);
        if self.open && self.esc_interceptor {
            // Esc is a window-level shortcut: dispatch-tree key listeners only run on the focused
            // path, so an interceptor (alive only while the popover is rendered) is used instead.
            let watch = window.use_keyed_state(
                SharedString::from(format!("{}-esc", self.id)),
                cx,
                |_, cx| EscWatch::new(cx),
            );
            watch.update(cx, |w, _| *w.dismiss.borrow_mut() = self.on_dismiss.clone());
        }
        let place = self.placement;
        let sheet =
            self.sheet_when_narrow && f32::from(window.viewport_size().width) < SHEET_BELOW_WIDTH;
        let trigger_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let track_trigger = trigger_bounds.clone();

        // Zero-height strip on the side the panel opens to; the anchored panel's origin is this
        // strip, so the panel is attached to the trigger's edge without knowing its size.
        // For the end placements the content sits in a zero-width box at the trigger's right edge:
        // `anchored` takes its origin from its own layout slot, so a slot as wide as the panel
        // would push the panel left by its own width.
        let marker = |content: Option<AnyElement>| {
            let content = content.map(|c| {
                if place.end() {
                    div()
                        .w(px(0.0))
                        .h(px(0.0))
                        .flex_none()
                        .child(c)
                        .into_any_element()
                } else {
                    c
                }
            });
            div()
                .w_full()
                .h(px(0.0))
                .flex()
                .flex_row()
                .when(place.end(), |d| d.justify_end())
                .children(content)
        };

        let panel = self.open.then(|| {
            let panel_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
            let track_panel = panel_bounds.clone();
            let dismiss = self.on_dismiss.clone();
            let dir = if sheet {
                1.0
            } else if place.below() {
                -1.0
            } else {
                1.0
            };
            let travel = if sheet { 48.0 } else { TRAVEL };
            let on_cancel = self.on_dismiss.clone();
            let mut body = div()
                .relative()
                .key_context(CONTEXT)
                .on_action(move |_: &Cancel, window, cx| {
                    if let Some(f) = &on_cancel {
                        f(window, cx);
                    }
                })
                .when_some(self.width, |d, w| d.w(px(w)))
                .p(px(self.padding))
                .rounded(px(Radius::SM))
                .bg(c.surface_2)
                .border_1()
                .border_color(c.outline_soft)
                .text_color(c.text)
                .font_family(ty.ui_family)
                .text_size(px(ty.ui_size_md))
                // Swallow presses so the click-outside logic and elements behind never see them.
                .occlude()
                .child(
                    canvas(
                        move |b, _, _| track_panel.set(b),
                        move |_, (), window, _| {
                            let dismiss = dismiss.clone();
                            let panel = panel_bounds.clone();
                            let trigger = trigger_bounds.clone();
                            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                                if phase == DispatchPhase::Capture
                                    && !panel.get().contains(&ev.position)
                                    && !trigger.get().contains(&ev.position)
                                    && let Some(f) = &dismiss
                                {
                                    f(window, cx);
                                }
                            });
                        },
                    )
                    .absolute()
                    .size_full(),
                );
            body = body.children(self.children);

            let animated: AnyElement = if reduced_motion() {
                body.into_any_element()
            } else {
                body.with_animation(
                    SharedString::from(format!("{}-pop", self.id)),
                    Animation::new(BASE).with_easing(if sheet {
                        clamped(SHEET)
                    } else {
                        clamped(POP)
                    }),
                    move |el, t| el.opacity(t).top(px(dir * travel * (1.0 - t))),
                )
                .into_any_element()
            };

            if sheet {
                // Bottom sheet: full width, docked to the bottom edge of the window.
                let vp = window.viewport_size();
                let sheet_body = div()
                    .w(vp.width)
                    .max_h(vp.height * 0.85)
                    .flex()
                    .flex_row()
                    .justify_center()
                    .overflow_hidden()
                    .rounded_t(px(Radius::MD))
                    .bg(c.surface_2)
                    .child(animated);
                deferred(
                    anchored()
                        .position(point(px(0.0), vp.height))
                        .anchor(Corner::BottomLeft)
                        .child(sheet_body),
                )
                .with_priority(1)
                .into_any_element()
            } else {
                let gap = if place.below() { GAP } else { -GAP };
                deferred(
                    anchored()
                        .anchor(place.corner())
                        .offset(point(px(0.0), px(gap)))
                        .snap_to_window_with_margin(px(8.0))
                        .child(animated),
                )
                .with_priority(1)
                .into_any_element()
            }
        });

        let trigger = div()
            .flex_none()
            .child(self.trigger.unwrap_or_else(|| div().into_any_element()))
            .child(
                canvas(move |b, _, _| track_trigger.set(b), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .relative();

        let strip = marker(panel);
        // `self_start`: size to the trigger even inside a stretching column.
        let mut root = div().id(self.id).flex().flex_col().flex_none();
        root.style().align_self = Some(gpui::AlignSelf::FlexStart);
        if place.below() {
            root.child(trigger).child(strip)
        } else {
            root.child(strip).child(trigger)
        }
    }
}
