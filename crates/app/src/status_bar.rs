//! Bottom status bar (28 px): compact pills. Git branch (hidden outside repositories), Claude
//! usage with a dither meter, and the live RAM of this process.

use std::time::Duration;

use gpui::{
    AnyView, Context, Div, Entity, IntoElement, Render, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use theme::{Theme, rgb};

use crate::git_monitor::{GitInfo, GitMonitor};
use crate::icons::icon;
use crate::mem;
use crate::usage_monitor::{UsageInfo, UsageMonitor, compact_tokens, now_ms};

/// Height of the status bar in logical pixels.
pub const STATUS_H: f32 = 28.0;
const PILL_H: f32 = 20.0;
const METER_SEGMENTS: usize = 8;
const REFRESH: Duration = Duration::from_secs(2);

/// Floating hint shown while hovering a pill (the only popover style: `outline_soft` 1 px).
struct Tip {
    text: SharedString,
    theme: Theme,
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(c.surface_3)
            .border_1()
            .border_color(c.outline_soft)
            .text_color(c.text)
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_sm))
            .child(self.text.clone())
    }
}

pub struct StatusBar {
    theme: Theme,
    mem_label: SharedString,
    git: Entity<GitMonitor>,
    usage: Entity<UsageMonitor>,
    _subs: [Subscription; 2],
}

impl StatusBar {
    pub fn new(
        theme: Theme,
        git: Entity<GitMonitor>,
        usage: Entity<UsageMonitor>,
        cx: &mut Context<Self>,
    ) -> Self {
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
        // The monitors call `cx.notify()` when their data changes: repaint with it.
        let subs = [
            cx.observe(&git, |_, _, cx| cx.notify()),
            cx.observe(&usage, |_, _, cx| cx.notify()),
        ];
        Self { theme, mem_label: Self::read_label(), git, usage, _subs: subs }
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

    fn tip(&self, text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut gpui::App) -> AnyView + 'static {
        let text: SharedString = text.into();
        let theme = self.theme;
        move |_window, cx| cx.new(|_| Tip { text: text.clone(), theme }).into()
    }

    /// Dither meter for `percent` (0..=100): solid segments, then one half-tone segment when the
    /// remainder is at least a quarter segment.
    fn meter(&self, percent: f64) -> impl IntoElement {
        let c = self.theme.colors;
        let cell = 2.0;
        let exact = (percent.clamp(0.0, 100.0) / 100.0) * METER_SEGMENTS as f64;
        let filled = exact.floor() as usize;
        let half = exact - exact.floor() >= 0.25;
        let seg = move |i: usize| {
            let col = div().flex_none().flex().flex_row().flex_wrap().w(px(cell * 2.0)).h(px(cell * 5.0));
            if i < filled {
                col.bg(c.accent)
            } else if i == filled && half {
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

    fn git_pill(&self, info: &GitInfo) -> impl IntoElement {
        let c = self.theme.colors;
        let yellow = rgb(self.theme.terminal.ansi[3]);
        let mut tip = format!("Rama {}", info.branch);
        tip.push_str(if info.dirty { " · cambios sin confirmar" } else { " · limpia" });
        if info.ahead > 0 {
            tip.push_str(&format!(" · {} por subir", info.ahead));
        }
        if info.behind > 0 {
            tip.push_str(&format!(" · {} por bajar", info.behind));
        }
        self.pill()
            .id("git-pill")
            .child(icon("git-branch", 16.0, c.muted))
            .child(info.branch.clone())
            .when(info.dirty, |d| d.child(div().size(px(6.0)).flex_none().rounded_full().bg(yellow)))
            .when(info.ahead > 0, |d| d.child(format!("↑{}", info.ahead)))
            .when(info.behind > 0, |d| d.child(format!("↓{}", info.behind)))
            .tooltip(self.tip(tip))
    }

    fn usage_pill(&self, info: &UsageInfo) -> impl IntoElement {
        let official = info.fresh_official().cloned();
        let tip = match &official {
            Some(o) => {
                let mins = ((now_ms() - o.observed_ms).max(0) / 60_000).max(0);
                format!("oficial · {:.0} % del límite semanal · hace {mins} min", o.percent)
            },
            None => "estimado · tokens locales de Claude esta semana (incluye caché)".to_string(),
        };
        self.pill()
            .id("usage-pill")
            .child(format!("Claude {} · sem", compact_tokens(info.week_tokens)))
            .children(official.map(|o| self.meter(o.percent)))
            .tooltip(self.tip(tip))
    }

    fn read_label() -> SharedString {
        match mem::working_set_bytes() {
            Some(b) => format!("RAM {}", mem::format_mb(b)).into(),
            None => "RAM --".into(),
        }
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ty = self.theme.typography;
        let git = self.git.read(cx).info().cloned();
        let usage = self.usage.read(cx).info().cloned();
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
            .children(git.map(|g| self.git_pill(&g)))
            .children(usage.map(|u| self.usage_pill(&u)))
            .child(div().flex_1())
            .child(self.pill().child(self.mem_label.clone()))
    }
}
