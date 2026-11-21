//! Settings controls: `SegmentedControl`, `Toggle`, `Slider` and `SwatchRow`.
//!
//! All are stateless: the caller owns the value and re-renders from the `on_change` callback.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ElementId, Empty, IntoElement, Pixels, RenderOnce, Rgba, SharedString, Window,
    canvas, div, prelude::*, px,
};
use theme::Theme;

use crate::icon::{Icon, icon};

type IndexFn = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;
type BoolFn = dyn Fn(bool, &mut Window, &mut App) + 'static;
type FloatFn = dyn Fn(f32, &mut Window, &mut App) + 'static;

// ---------------------------------------------------------------------------------------------
// SegmentedControl
// ---------------------------------------------------------------------------------------------

/// One option of a [`SegmentedControl`].
#[derive(Clone, Debug)]
pub struct Segment {
    pub label: SharedString,
    pub icon: Option<Icon>,
}

impl Segment {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), icon: None }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// A row of mutually exclusive options on a tonal track (System / Dark / Light in Settings).
/// The selected segment is a `surface_3` pill; the others are muted text.
///
/// ```ignore
/// SegmentedControl::new("theme", &theme, ["Sistema", "Oscuro", "Claro"])
///     .selected(1)
///     .on_change(|ix, _, cx| { /* store ix */ })
/// ```
#[derive(IntoElement)]
pub struct SegmentedControl {
    id: SharedString,
    theme: Theme,
    segments: Vec<Segment>,
    selected: usize,
    track: Option<Rgba>,
    on_change: Option<IndexFn>,
}

impl SegmentedControl {
    pub fn new<S: Into<Segment>>(
        id: impl Into<SharedString>,
        theme: &Theme,
        segments: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            segments: segments.into_iter().map(Into::into).collect(),
            selected: 0,
            track: None,
            on_change: None,
        }
    }

    pub fn selected(mut self, ix: usize) -> Self {
        self.selected = ix;
        self
    }

    /// Track color; defaults to `surface_1`. Use `surface_2` when the control sits on a
    /// `surface_1` card.
    pub fn track(mut self, track: Rgba) -> Self {
        self.track = Some(track);
        self
    }

    pub fn on_change(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }
}

impl From<&str> for Segment {
    fn from(label: &str) -> Self {
        Segment::new(label.to_string())
    }
}

impl From<String> for Segment {
    fn from(label: String) -> Self {
        Segment::new(label)
    }
}

impl From<SharedString> for Segment {
    fn from(label: SharedString) -> Self {
        Segment::new(label)
    }
}

impl RenderOnce for SegmentedControl {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let track = self.track.unwrap_or(c.surface_1);
        // On a `surface_2` track the pill must still read: step one tone up.
        let pill = if track == c.surface_2 { c.surface_4 } else { c.surface_3 };
        let on_change = self.on_change;
        let id = self.id;
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded(px(12.0))
            .bg(track)
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .children(self.segments.into_iter().enumerate().map(move |(ix, seg)| {
                let selected = ix == self.selected;
                let fg = if selected { c.text } else { c.muted };
                let on_change = on_change.clone();
                div()
                    .id(ElementId::NamedInteger(id.clone(), ix as u64))
                    .occlude()
                    .h(px(28.0))
                    .px(px(14.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .rounded(px(9.0))
                    .text_color(fg)
                    .cursor_pointer()
                    .when(selected, |d| d.bg(pill))
                    .when(!selected, |d| d.hover(move |s| s.text_color(c.text)))
                    .when_some(seg.icon, |d, i| d.child(icon(i, 16.0, fg)))
                    .child(seg.label)
                    .on_click(move |_, window, cx| {
                        if let Some(f) = &on_change {
                            f(ix, window, cx);
                        }
                    })
            }))
    }
}

// ---------------------------------------------------------------------------------------------
// Toggle
// ---------------------------------------------------------------------------------------------

/// On/off switch, 36 x 20. On = `accent_strong` track (an active state), off = `surface_3`.
///
/// ```ignore
/// Toggle::new("reduce-motion", &theme).on(true).on_change(|on, _, cx| { /* store */ })
/// ```
#[derive(IntoElement)]
pub struct Toggle {
    id: SharedString,
    theme: Theme,
    on: bool,
    on_change: Option<Box<BoolFn>>,
}

impl Toggle {
    pub fn new(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self { id: id.into(), theme: *theme, on: false, on_change: None }
    }

    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Called with the *new* value when clicked.
    pub fn on_change(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }
}

impl RenderOnce for Toggle {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let on = self.on;
        let knob = if on { Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 } } else { c.outline };
        let mut el = div()
            .id(self.id)
            .occlude()
            .w(px(36.0))
            .h(px(20.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .px(px(3.0))
            .rounded_full()
            .cursor_pointer()
            .bg(if on { c.accent_strong } else { c.surface_3 })
            .when(on, |d| d.justify_end())
            .child(div().size(px(14.0)).rounded_full().bg(knob));
        if let Some(f) = self.on_change {
            el = el.on_click(move |_, w, cx| f(!on, w, cx));
        }
        el
    }
}

// ---------------------------------------------------------------------------------------------
// Slider
// ---------------------------------------------------------------------------------------------

/// Payload of a slider drag; identifies which slider owns it.
struct SliderDrag(SharedString);

/// Horizontal slider with an optional "default" mark (a small tick on the track; the value
/// snaps to it when dragged close).
///
/// ```ignore
/// Slider::new("sidebar-alpha", &theme)
///     .range(0.0, 100.0)
///     .value(60.0)
///     .default_value(100.0)
///     .on_change(|v, _, cx| { /* store v */ })
/// ```
#[derive(IntoElement)]
pub struct Slider {
    id: SharedString,
    theme: Theme,
    min: f32,
    max: f32,
    value: f32,
    default: Option<f32>,
    step: Option<f32>,
    width: f32,
    on_change: Option<Rc<FloatFn>>,
}

impl Slider {
    pub fn new(id: impl Into<SharedString>, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            min: 0.0,
            max: 1.0,
            value: 0.0,
            default: None,
            step: None,
            width: 200.0,
            on_change: None,
        }
    }

    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max.max(min + f32::EPSILON);
        self
    }

    pub fn value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    /// Marks the factory default on the track.
    pub fn default_value(mut self, default: f32) -> Self {
        self.default = Some(default);
        self
    }

    /// Rounds emitted values to a multiple of `step`.
    pub fn step(mut self, step: f32) -> Self {
        self.step = Some(step);
        self
    }

    /// Track length in px (default 200).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn on_change(mut self, f: impl Fn(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    fn fraction(&self, v: f32) -> f32 {
        ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }
}

/// Maps a pointer x inside `[left, left + width]` to a value of the range, with step rounding
/// and snapping to the default mark. Pure, so it is unit tested.
pub(crate) fn slider_value(
    x: f32,
    left: f32,
    width: f32,
    min: f32,
    max: f32,
    step: Option<f32>,
    default: Option<f32>,
) -> f32 {
    let t = if width > 0.0 { ((x - left) / width).clamp(0.0, 1.0) } else { 0.0 };
    let mut v = min + t * (max - min);
    if let Some(step) = step.filter(|s| *s > 0.0) {
        v = (min + ((v - min) / step).round() * step).clamp(min, max);
    }
    if let Some(d) = default
        && (v - d).abs() <= (max - min) * 0.02
    {
        v = d;
    }
    v
}

struct DragGhost;

impl gpui::Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl RenderOnce for Slider {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        const KNOB: f32 = 14.0;
        const TRACK_H: f32 = 4.0;
        let c = self.theme.colors;
        let t = self.fraction(self.value);
        let default_t = self.default.map(|d| self.fraction(d));
        let (min, max, step, default, width) =
            (self.min, self.max, self.step, self.default, self.width);
        let id = self.id.clone();
        let drag_id = self.id.clone();
        let on_change = self.on_change;
        let set = {
            let on_change = on_change.clone();
            move |x: f32, left: f32, w: f32, window: &mut Window, cx: &mut App| {
                if let Some(f) = &on_change {
                    f(slider_value(x, left, w, min, max, step, default), window, cx);
                }
            }
        };
        let set_move = set.clone();
        let set_down = set;
        // The element bounds are only known during paint; a canvas child records them for the
        // mouse-down handler (drag events carry their own bounds).
        let bounds_cell = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let down_bounds = bounds_cell.clone();
        let inner_w = width - KNOB; // the knob center travels `inner_w`

        div()
            .id(self.id)
            .occlude()
            .w(px(width))
            .h(px(20.0))
            .flex_none()
            .relative()
            .cursor_pointer()
            .child(
                canvas(move |b, _, _| bounds_cell.set(b), |_, (), _, _| {}).absolute().size_full(),
            )
            // Track.
            .child(
                div()
                    .absolute()
                    .left(px(KNOB / 2.0))
                    .right(px(KNOB / 2.0))
                    .top(px((20.0 - TRACK_H) / 2.0))
                    .h(px(TRACK_H))
                    .rounded_full()
                    .bg(c.surface_3),
            )
            // Filled part (strong selection -> accent_strong).
            .child(
                div()
                    .absolute()
                    .left(px(KNOB / 2.0))
                    .top(px((20.0 - TRACK_H) / 2.0))
                    .h(px(TRACK_H))
                    .w(px(inner_w * t))
                    .rounded_full()
                    .bg(c.accent_strong),
            )
            // Default mark.
            .when_some(default_t, |d, dt| {
                d.child(
                    div()
                        .absolute()
                        .left(px(KNOB / 2.0 + inner_w * dt - 1.0))
                        .top(px(2.0))
                        .w(px(2.0))
                        .h(px(5.0))
                        .rounded(px(1.0))
                        .bg(c.outline),
                )
            })
            // Knob.
            .child(
                div()
                    .absolute()
                    .left(px(inner_w * t))
                    .top(px((20.0 - KNOB) / 2.0))
                    .size(px(KNOB))
                    .rounded_full()
                    .bg(c.text),
            )
            .on_mouse_down(gpui::MouseButton::Left, move |e, window, cx| {
                let b = down_bounds.get();
                set_down(
                    f32::from(e.position.x) - KNOB / 2.0,
                    f32::from(b.left()),
                    f32::from(b.size.width) - KNOB,
                    window,
                    cx,
                );
            })
            .on_drag(SliderDrag(drag_id), |_, _, _, cx| cx.new(|_| DragGhost))
            .on_drag_move::<SliderDrag>(move |ev, window, cx| {
                if ev.drag(cx).0 == id {
                    let b = ev.bounds;
                    set_move(
                        f32::from(ev.event.position.x) - KNOB / 2.0,
                        f32::from(b.left()),
                        f32::from(b.size.width) - KNOB,
                        window,
                        cx,
                    );
                }
            })
    }
}

// ---------------------------------------------------------------------------------------------
// SwatchRow
// ---------------------------------------------------------------------------------------------

/// A row of round color swatches (accent choices). The selected one gets a `text` colored ring.
///
/// ```ignore
/// SwatchRow::new("accent", &theme, [rgb(0x00AAFF), rgb(0x7FCB8F)])
///     .selected(Some(0))
///     .on_select(|ix, _, cx| { /* store ix */ })
/// ```
#[derive(IntoElement)]
pub struct SwatchRow {
    id: SharedString,
    theme: Theme,
    colors: Vec<Rgba>,
    selected: Option<usize>,
    on_select: Option<IndexFn>,
}

impl SwatchRow {
    pub fn new(
        id: impl Into<SharedString>,
        theme: &Theme,
        colors: impl IntoIterator<Item = Rgba>,
    ) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            colors: colors.into_iter().collect(),
            selected: None,
            on_select: None,
        }
    }

    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_select(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for SwatchRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let id = self.id;
        let on_select = self.on_select;
        let selected = self.selected;
        div().flex().flex_row().items_center().gap(px(6.0)).children(
            self.colors.into_iter().enumerate().map(move |(ix, color)| {
                let on_select = on_select.clone();
                let is_sel = selected == Some(ix);
                div()
                    .id(ElementId::NamedInteger(id.clone(), ix as u64))
                    .occlude()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .when(is_sel, |d| d.border_2().border_color(c.text))
                    .child(div().size(px(if is_sel { 12.0 } else { 18.0 })).rounded_full().bg(color))
                    .on_click(move |_, window, cx| {
                        if let Some(f) = &on_select {
                            f(ix, window, cx);
                        }
                    })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_maps_pointer_to_range() {
        let v = |x| slider_value(x, 100.0, 200.0, 0.0, 10.0, None, None);
        assert_eq!(v(100.0), 0.0);
        assert_eq!(v(300.0), 10.0);
        assert_eq!(v(200.0), 5.0);
        assert_eq!(v(0.0), 0.0, "clamped left");
        assert_eq!(v(999.0), 10.0, "clamped right");
    }

    #[test]
    fn slider_steps_and_snaps_to_default() {
        let stepped = slider_value(137.0, 100.0, 200.0, 0.0, 100.0, Some(10.0), None);
        assert_eq!(stepped, 20.0);
        // 61.0 is within 2 % of the default 60 -> snaps.
        let snapped = slider_value(100.0 + 200.0 * 0.61, 100.0, 200.0, 0.0, 100.0, None, Some(60.0));
        assert_eq!(snapped, 60.0);
        let free = slider_value(100.0 + 200.0 * 0.70, 100.0, 200.0, 0.0, 100.0, None, Some(60.0));
        assert!((free - 70.0).abs() < 1e-3);
    }
}
