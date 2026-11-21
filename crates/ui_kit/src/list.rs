//! Lists: `VirtualList` (renders only the visible rows), `ListRow` (thread / agent / project
//! row) and the status glyphs, including the working spinner.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, IntoElement, ParentElement, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, WindowId, canvas, div, fill, point, prelude::*,
    px, size, uniform_list,
};
use theme::{Rgba, Theme, rgb};

use crate::button::ClickFn;
use crate::icon::{Icon, icon};
use crate::motion::reduced_motion;

pub use gpui::UniformListScrollHandle;

// ---------------------------------------------------------------------------------------------
// VirtualList
// ---------------------------------------------------------------------------------------------

type RowRenderer = Box<dyn Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static>;

/// A scrolling list of `count` rows of one fixed height. Only the rows inside the viewport (plus
/// the scroll offset) are built each frame, so 10 000 rows cost the same as 20. It is `Styled`:
/// give it a definite height (`.h(px(..))` or `.flex_1().min_h_0()`).
///
/// ```ignore
/// VirtualList::new("threads", items.len(), ListRow::HEIGHT, move |range, _, _| {
///     range.map(|ix| ListRow::new(("thread", ix), &theme).title(items[ix].clone()).into_any_element()).collect()
/// })
/// .flex_1()
/// ```
#[derive(IntoElement)]
pub struct VirtualList {
    base: Div,
    id: ElementId,
    count: usize,
    row_height: f32,
    render: RowRenderer,
    scroll: Option<UniformListScrollHandle>,
}

impl VirtualList {
    pub fn new(
        id: impl Into<ElementId>,
        count: usize,
        row_height: f32,
        render: impl Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static,
    ) -> Self {
        Self {
            base: div().min_h_0(),
            id: id.into(),
            count,
            row_height,
            render: Box::new(render),
            scroll: None,
        }
    }

    /// Keeps the scroll position in `handle` (also lets the caller `scroll_to_item`).
    pub fn track_scroll(mut self, handle: UniformListScrollHandle) -> Self {
        self.scroll = Some(handle);
        self
    }
}

impl Styled for VirtualList {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for VirtualList {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let row_h = self.row_height;
        let render = self.render;
        let list = uniform_list(self.id, self.count, move |range, window, cx| {
            render(range, window, cx)
                .into_iter()
                // Every row gets exactly `row_h`, whatever the caller built.
                .map(|row| div().w_full().h(px(row_h)).flex_none().child(row))
                .collect::<Vec<_>>()
        })
        .size_full();
        let list = match self.scroll {
            Some(h) => list.track_scroll(h),
            None => list,
        };
        self.base.child(list)
    }
}

// ---------------------------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------------------------

/// Lifecycle of an agent / thread, drawn by [`StatusGlyph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus {
    /// Running: animated bars (accent, it is progress).
    Working,
    /// Needs the user: amber dots.
    Waiting,
    /// Finished fine: check.
    Done,
    /// Failed: alert mark in `error`.
    Error,
}

/// The 16 px status mark of a row.
///
/// ```ignore
/// StatusGlyph::new(&theme, RowStatus::Done)
/// ```
#[derive(IntoElement)]
pub struct StatusGlyph {
    theme: Theme,
    status: RowStatus,
}

impl StatusGlyph {
    pub fn new(theme: &Theme, status: RowStatus) -> Self {
        Self { theme: *theme, status }
    }
}

impl RenderOnce for StatusGlyph {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let t = self.theme;
        match self.status {
            RowStatus::Working => Spinner::new(t.colors.accent).into_any_element(),
            RowStatus::Waiting => icon(Icon::Dots, 16.0, rgb(t.terminal.ansi[3])).into_any_element(),
            RowStatus::Done => icon(Icon::Check, 16.0, rgb(t.terminal.ansi[2])).into_any_element(),
            RowStatus::Error => icon(Icon::Alert, 16.0, t.colors.error).into_any_element(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------------------------

const SPIN_FRAME_MS: u64 = 110;
/// Bar heights (px) of the wave; each bar reads the pattern at a different phase.
const WAVE: [f32; 6] = [3.0, 5.0, 8.0, 12.0, 8.0, 5.0];

thread_local! {
    /// Per window: the last frame index for which a redraw was already scheduled.
    static SCHEDULED: RefCell<HashMap<WindowId, u64>> = RefCell::new(HashMap::new());
}

static EPOCH_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    // Keep the numbers small and stable across calls.
    let _ = EPOCH_MS.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
    now - EPOCH_MS.load(Ordering::Relaxed)
}

/// Heights (px) of the five bars at animation `frame`.
pub(crate) fn spinner_heights(frame: u64) -> [f32; 5] {
    let mut out = [0.0; 5];
    for (i, h) in out.iter_mut().enumerate() {
        *h = WAVE[(frame as usize + i) % WAVE.len()];
    }
    out
}

/// "Shark bars" spinner: five vertical bars of different height that ripple, like the logo.
///
/// It only animates while it is actually painted: each paint schedules at most one redraw
/// ~110 ms later, so a spinner that scrolls out of a [`VirtualList`], sits in a hidden tab or in
/// a minimised window stops costing anything by itself. With reduced motion it shows one still
/// frame.
///
/// ```ignore
/// Spinner::new(theme.colors.accent)
/// ```
#[derive(IntoElement)]
pub struct Spinner {
    color: Rgba,
}

impl Spinner {
    pub fn new(color: Rgba) -> Self {
        Self { color }
    }
}

impl RenderOnce for Spinner {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color;
        div().size(px(16.0)).flex_none().child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    let animate = !reduced_motion();
                    let frame = if animate { now_ms() / SPIN_FRAME_MS } else { 3 };
                    // 5 bars x 2 px + 4 gaps x 1 px = 14 px, centered in 16.
                    let x0 = bounds.origin.x + px(1.0);
                    let cy = bounds.origin.y + bounds.size.height / 2.0;
                    for (i, h) in spinner_heights(frame).into_iter().enumerate() {
                        let x = x0 + px(3.0 * i as f32);
                        let top = cy - px(h / 2.0).floor();
                        window.paint_quad(fill(
                            gpui::Bounds::new(point(x, top), size(px(2.0), px(h))),
                            color,
                        ));
                    }
                    if animate {
                        let id = window.window_handle().window_id();
                        let next = frame + 1;
                        let due = SCHEDULED.with(|s| {
                            let mut s = s.borrow_mut();
                            let seen = s.get(&id).copied().unwrap_or(0);
                            (seen < next).then(|| s.insert(id, next))
                        });
                        if due.is_some() {
                            let wait = SPIN_FRAME_MS - now_ms() % SPIN_FRAME_MS;
                            window
                                .spawn(cx, async move |cx| {
                                    cx.background_executor()
                                        .timer(Duration::from_millis(wait))
                                        .await;
                                    let _ = cx.update(|window, _| window.refresh());
                                })
                                .detach();
                        }
                    }
                },
            )
            .size_full(),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// ListRow
// ---------------------------------------------------------------------------------------------

/// A row of a thread / agent / project list: leading icon or status, title with an optional
/// subtitle, and trailing meta ("opus-5.5 49m").
///
/// ```ignore
/// ListRow::new(("agent", 0), &theme)
///     .title("Refactor del parser")
///     .subtitle("selachi · feat/ui-kit")
///     .meta("opus-5.5 49m")
///     .status(RowStatus::Working)
///     .selected(true)
/// ```
#[derive(IntoElement)]
pub struct ListRow {
    id: ElementId,
    theme: Theme,
    icon: Option<Icon>,
    agent: Option<crate::AgentMark>,
    title: SharedString,
    subtitle: Option<SharedString>,
    meta: Option<SharedString>,
    status: Option<RowStatus>,
    selected: bool,
    glass: bool,
    on_click: Option<ClickFn>,
}

impl ListRow {
    /// Fixed row height in px; pass it to [`VirtualList::new`].
    pub const HEIGHT: f32 = 44.0;

    pub fn new(id: impl Into<ElementId>, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            icon: None,
            agent: None,
            title: SharedString::default(),
            subtitle: None,
            meta: None,
            status: None,
            selected: false,
            glass: false,
            on_click: None,
        }
    }

    /// The row sits on a glass surface: hover / selected pills are translucent (white 8 / 12 %)
    /// so the blurred backdrop shows through.
    pub fn glass(mut self, glass: bool) -> Self {
        self.glass = glass;
        self
    }

    /// Leading agent mark (replaces [`ListRow::icon`]).
    pub fn agent(mut self, mark: crate::AgentMark) -> Self {
        self.agent = Some(mark);
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Right-aligned muted text.
    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// Status glyph; replaces the icon when both are set.
    pub fn status(mut self, status: RowStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for ListRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let theme = self.theme;
        let lead: Option<AnyElement> = match (self.status, self.agent, self.icon) {
            (Some(s), _, _) => Some(StatusGlyph::new(&theme, s).into_any_element()),
            (None, Some(m), _) => Some(crate::agent_icon(m, 16.0, c.muted).into_any_element()),
            (None, None, Some(i)) => Some(icon(i, 16.0, c.muted).into_any_element()),
            (None, None, None) => None,
        };
        let mut el = div()
            .id(self.id)
            .occlude()
            .w_full()
            .h(px(Self::HEIGHT - 4.0))
            .my(px(2.0))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .rounded(px(10.0))
            .font_family(ty.ui_family)
            .when(self.selected, |d| {
                d.bg(if self.glass { crate::glass_selected(&theme) } else { c.surface_3.into() })
            })
            .when(!self.selected, |d| {
                let hover: gpui::Hsla = if self.glass { crate::glass_hover(&theme) } else { c.surface_2.into() };
                d.hover(move |s| s.bg(hover))
            })
            .when_some(lead, |d, l| d.child(l))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.text)
                            .child(self.title),
                    )
                    .when_some(self.subtitle, |d, s| {
                        d.child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(ty.ui_size_sm))
                                .text_color(c.muted)
                                .child(s),
                        )
                    }),
            )
            .when_some(self.meta, |d, m| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .child(m),
                )
            });
        if let Some(f) = self.on_click {
            el = el.cursor_pointer().on_click(move |e, w, cx| f(e, w, cx));
        }
        el
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_wave_cycles() {
        assert_eq!(spinner_heights(0), spinner_heights(WAVE.len() as u64));
        assert_ne!(spinner_heights(0), spinner_heights(1));
        for f in 0..12 {
            for h in spinner_heights(f) {
                assert!((1.0..=16.0).contains(&h));
            }
        }
    }
}
