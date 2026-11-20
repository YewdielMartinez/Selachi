//! Bottom status bar (28 px): compact pills. Git branch (hidden outside repositories), Claude
//! usage with a dither meter, and the live RAM of this process.

use std::time::Duration;

use gpui::{Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window, div, prelude::*, px};
use theme::{Theme, rgb};
use ui_kit::{DitherMeter, Icon, Pill};

use crate::git_monitor::{GitInfo, GitMonitor};
use crate::mem;
use crate::usage_monitor::{UsageInfo, UsageMonitor, compact_tokens, now_ms, usage_tooltip};

/// Height of the status bar in logical pixels.
pub const STATUS_H: f32 = 28.0;
/// How often the RAM reading is refreshed, only while the window is focused.
const REFRESH: Duration = Duration::from_secs(2);

pub struct StatusBar {
    theme: Theme,
    mem_label: SharedString,
    git: Entity<GitMonitor>,
    usage: Entity<UsageMonitor>,
    /// RAM refresh loop; `Some` only while the window is focused (dropping it cancels the timer,
    /// so a background window has no wake-ups from here).
    ram_timer: Option<Task<()>>,
    _subs: [Subscription; 2],
}

impl StatusBar {
    pub fn new(
        theme: Theme,
        git: Entity<GitMonitor>,
        usage: Entity<UsageMonitor>,
        cx: &mut Context<Self>,
    ) -> Self {
        // The monitors call `cx.notify()` when their data changes: repaint with it.
        let subs = [
            cx.observe(&git, |_, _, cx| cx.notify()),
            cx.observe(&usage, |_, _, cx| cx.notify()),
        ];
        Self { theme, mem_label: Self::read_label(), git, usage, ram_timer: None, _subs: subs }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// The window gained or lost focus. The RAM label is refreshed every 2 s only while focused,
    /// and only repainted when the displayed text actually changes.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if !active {
            self.ram_timer = None;
            return;
        }
        self.refresh_ram(cx);
        if self.ram_timer.is_some() {
            return;
        }
        self.ram_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |bar, cx| bar.refresh_ram(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn refresh_ram(&mut self, cx: &mut Context<Self>) {
        let label = Self::read_label();
        if label != self.mem_label {
            self.mem_label = label;
            cx.notify();
        }
    }

    fn git_pill(&self, info: &GitInfo) -> Pill {
        let yellow = rgb(self.theme.terminal.ansi[3]);
        let mut tip = format!("Rama {}", info.branch);
        tip.push_str(if info.dirty { " · cambios sin confirmar" } else { " · limpia" });
        if info.ahead > 0 {
            tip.push_str(&format!(" · {} por subir", info.ahead));
        }
        if info.behind > 0 {
            tip.push_str(&format!(" · {} por bajar", info.behind));
        }
        let mut pill = Pill::compact("git-pill", &self.theme)
            .icon(Icon::GitBranch)
            .label(info.branch.clone())
            .tooltip(tip);
        if info.dirty {
            pill = pill.child(div().size(px(6.0)).flex_none().rounded_full().bg(yellow));
        }
        if info.ahead > 0 {
            pill = pill.child(format!("↑{}", info.ahead));
        }
        if info.behind > 0 {
            pill = pill.child(format!("↓{}", info.behind));
        }
        pill
    }

    fn usage_pill(&self, info: &UsageInfo) -> Pill {
        let official = info.fresh_official().cloned();
        let pill = Pill::compact("usage-pill", &self.theme)
            // The headline is what is billed: input + output + cache creation (not cache reads).
            .label(format!("Claude {} · sem", compact_tokens(info.week.billable())))
            .tooltip(usage_tooltip(info, now_ms()));
        match official {
            Some(o) => pill.child(DitherMeter::new(&self.theme).value(Some((o.percent / 100.0) as f32))),
            None => pill,
        }
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
            .child(Pill::compact("ram-pill", &self.theme).label(self.mem_label.clone()))
    }
}
