//! Bottom status bar (28 px): compact pills; git branch + usage meter (placeholders), live RAM.

use std::time::Duration;

use gpui::{Context, Div, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use theme::Theme;

use crate::icons::icon;
use crate::mem;

/// Height of the status bar in logical pixels.
pub const STATUS_H: f32 = 28.0;
const PILL_H: f32 = 20.0;
const METER_SEGMENTS: usize = 8;
const REFRESH: Duration = Duration::from_secs(2);

pub struct StatusBar {
    theme: Theme,
    mem_label: SharedString,
}

impl StatusBar {
    pub fn new(theme: Theme, cx: &mut Context<Self>) -> Self {
        // Refresh the memory reading on a gpui timer; only repaint when the text changes.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                let alive = this
                    .update(cx, |bar, cx| {
                        let label = Self::read_label();
                        if label != bar.mem_label {
                            bar.mem_label = label;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        Self {
            theme,
            mem_label: Self::read_label(),
        }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn pill(&self) -> Div {
        let c = self.theme.colors;
        div()
            .h(px(PILL_H))
            .flex_none()
            .px(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(8.0))
            .bg(c.surface_1)
            .text_color(c.muted)
    }

    /// Dither meter: `filled` solid segments, then one half-tone (checkerboard) segment.
    fn meter(&self, filled: usize) -> impl IntoElement {
        let c = self.theme.colors;
        let cell = 2.0;
        let seg = |i: usize| {
            let col = div()
                .flex_none()
                .flex()
                .flex_row()
                .flex_wrap()
                .w(px(cell * 2.0))
                .h(px(cell * 5.0));
            if i < filled {
                col.bg(c.accent)
            } else if i == filled {
                col.children((0..10).map(|k| {
                    let on = (k / 2 + k % 2) % 2 == 0;
                    div().size(px(cell)).when(on, |d| d.bg(c.accent))
                }))
            } else {
                col.bg(c.surface_3)
            }
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .children((0..METER_SEGMENTS).map(seg))
    }

    fn read_label() -> SharedString {
        match mem::working_set_bytes() {
            Some(b) => format!("RAM {}", mem::format_mb(b)).into(),
            None => "RAM --".into(),
        }
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .h(px(STATUS_H))
            .flex_none()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_sm))
            // Placeholder: no git integration yet.
            .child(
                self.pill()
                    .child(icon("git-branch", 16.0, c.muted))
                    .child("main"),
            )
            // Placeholder: no usage source yet.
            .child(self.pill().child("Claude").child(self.meter(5)))
            .child(div().flex_1())
            .child(self.pill().child(self.mem_label.clone()))
    }
}
