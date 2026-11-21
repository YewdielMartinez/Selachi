//! Bottom bar. `Compact` (28 px): pills with the git branch (hidden outside repositories), Claude
//! usage with a dither meter, and the live RAM of this process. `Full` (52 px): one block per
//! agent with its meters (5 h window, weekly) like Orca's bottom bar, then the git pill and RAM.
//! Meters show official percentages only; without an official snapshot they stay empty and the
//! block says so (nothing is estimated into a percentage).

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use store::BottomBar;
use theme::{Radius, Theme, rgb};
use ui_kit::{AgentMark, DitherMeter, Icon, Pill, agent_icon};

use crate::git_monitor::{GitInfo, GitMonitor};
use crate::mem;
use crate::usage_monitor::{Official, UsageInfo, UsageMonitor, compact_tokens, now_ms, usage_tooltip};

/// Height of the compact bar in logical pixels.
pub const STATUS_H: f32 = 28.0;
/// Height of the full bar (per-agent meters).
pub const STATUS_FULL_H: f32 = 52.0;
/// Window widths (logical px) from which each piece of the bar is shown. When the bar does not
/// fit, pieces disappear in this order: full meters, estimated usage, RAM; the branch (and the
/// future orphan alert) go last. See docs/DESIGN.md, "Responsive".
pub const FULL_FROM: f32 = 1024.0;
pub const USAGE_FROM: f32 = 720.0;
pub const RAM_FROM: f32 = 380.0;

/// Which pieces fit in a bar of `width` px.
pub fn visible_pieces(width: f32) -> (bool, bool, bool) {
    (width >= FULL_FROM, width >= USAGE_FROM, width >= RAM_FROM)
}

/// How often the RAM reading is refreshed, only while the window is focused.
const REFRESH: Duration = Duration::from_secs(2);

/// Height of the bottom bar for a mode (0 when hidden).
pub fn bar_height(mode: BottomBar) -> f32 {
    match mode {
        BottomBar::Compact => STATUS_H,
        BottomBar::Full => STATUS_FULL_H,
        BottomBar::Hidden => 0.0,
    }
}

/// `2 h 10 min` / `45 min` / `3 d 5 h` until a reset, from unix milliseconds.
pub fn format_reset(resets_at_ms: i64, now: i64) -> String {
    let mins = ((resets_at_ms - now).max(0) / 60_000) as u64;
    match (mins / 1440, mins / 60 % 24, mins % 60) {
        (0, 0, m) => format!("{m} min"),
        (0, h, m) => format!("{h} h {m:02} min"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

/// Click handler of the orphan pill.
type OrphanClick = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct StatusBar {
    theme: Theme,
    mem_label: SharedString,
    mode: BottomBar,
    /// Process model: its orphan summary feeds the orphan pill (hidden when there are none).
    procs: Option<Entity<ui_procs::ProcsModel>>,
    procs_sub: Option<Subscription>,
    on_orphans: Option<OrphanClick>,
    on_usage: Option<OrphanClick>,
    /// Attention queue: its count feeds the badge; `on_attention` opens the panel.
    attention: Option<Entity<ui_review::AttentionModel>>,
    attention_sub: Option<Subscription>,
    on_attention: Option<OrphanClick>,
    /// Git monitor of the active tab.
    git: Option<Entity<GitMonitor>>,
    git_sub: Option<Subscription>,
    usage: Entity<UsageMonitor>,
    /// RAM refresh loop; `Some` only while the window is focused (dropping it cancels the timer,
    /// so a background window has no wake-ups from here).
    ram_timer: Option<Task<()>>,
    _usage_sub: Subscription,
}

impl StatusBar {
    pub fn new(theme: Theme, mode: BottomBar, usage: Entity<UsageMonitor>, cx: &mut Context<Self>) -> Self {
        // The monitors call `cx.notify()` when their data changes: repaint with it.
        let usage_sub = cx.observe(&usage, |_, _, cx| cx.notify());
        Self {
            theme,
            mem_label: Self::read_label(),
            mode,
            procs: None,
            procs_sub: None,
            on_orphans: None,
            on_usage: None,
            attention: None,
            attention_sub: None,
            on_attention: None,
            git: None,
            git_sub: None,
            usage,
            ram_timer: None,
            _usage_sub: usage_sub,
        }
    }

    /// Follows the process model for the orphan pill; `on_click` opens the processes panel.
    pub fn set_procs(
        &mut self,
        procs: Entity<ui_procs::ProcsModel>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.procs_sub = Some(cx.observe(&procs, |_, _, cx| cx.notify()));
        self.procs = Some(procs);
        self.on_orphans = Some(Rc::new(on_click));
        cx.notify();
    }

    /// Clicking the usage pill (or the full bar's usage block) calls `on_click`: opens the usage panel.
    pub fn set_on_usage(&mut self, on_click: impl Fn(&mut Window, &mut App) + 'static, cx: &mut Context<Self>) {
        self.on_usage = Some(Rc::new(on_click));
        cx.notify();
    }

    /// Follows the attention queue for the badge; `on_click` opens its panel.
    pub fn set_attention(
        &mut self,
        model: Entity<ui_review::AttentionModel>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.attention_sub = Some(cx.observe(&model, |_, _, cx| cx.notify()));
        self.attention = Some(model);
        self.on_attention = Some(Rc::new(on_click));
        cx.notify();
    }

    /// The attention badge: shown only while something waits for the user.
    fn attention_pill(&self, cx: &Context<Self>) -> Option<Pill> {
        let n = self.attention.as_ref().map(|m| m.read(cx).count())?;
        if n == 0 {
            return None;
        }
        let mut pill = Pill::compact("attention-pill", &self.theme)
            .icon(Icon::Attention)
            .label(n.to_string())
            .tooltip("Algo espera tu atención (Ctrl+Shift+A salta al siguiente). Clic para ver la cola.");
        if let Some(f) = self.on_attention.clone() {
            pill = pill.on_click(move |_, window, cx| f(window, cx));
        }
        Some(pill)
    }

    /// The orphan alert: shown only while there are orphans; the last thing to hide.
    fn orphan_pill(&self, cx: &Context<Self>) -> Option<Pill> {
        let label = self.procs.as_ref().and_then(|p| p.read(cx).orphan_summary().label())?;
        let mut pill = Pill::compact("orphan-pill", &self.theme)
            .icon(Icon::Alert)
            .label(label)
            .tooltip("Procesos que sobrevivieron a su pestaña. Clic para verlos.");
        if let Some(f) = self.on_orphans.clone() {
            pill = pill.on_click(move |_, window, cx| f(window, cx));
        }
        Some(pill)
    }

    /// The active tab changed: follow its git monitor.
    pub fn set_git(&mut self, git: Option<Entity<GitMonitor>>, cx: &mut Context<Self>) {
        if self.git == git {
            return;
        }
        self.git_sub = git.as_ref().map(|g| cx.observe(g, |_, _, cx| cx.notify()));
        self.git = git;
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: BottomBar, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
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
        let pill = match self.on_usage.clone() {
            Some(f) => pill.on_click(move |_, window, cx| f(window, cx)),
            None => pill,
        };
        match official {
            Some(o) => pill.child(DitherMeter::new(&self.theme).value(Some((o.percent / 100.0) as f32))),
            None => pill,
        }
    }

    /// One meter line of the full bar: `5 h  ▮▮▮▯  42 %  ·  reinicia en 2 h 10 min`. Without an
    /// official snapshot the meter is empty and the text says `sin datos`.
    fn meter_cell(&self, label: &'static str, official: Option<&Official>) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (value, text) = match official {
            Some(o) => (
                Some((o.percent / 100.0) as f32),
                format!("{:.0} %  ·  reinicia en {}", o.percent, format_reset(o.resets_at_ms, now_ms())),
            ),
            None => (None, "sin datos".to_string()),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(div().w(px(34.0)).text_color(c.muted).text_size(px(ty.ui_size_sm)).child(label))
            .child(DitherMeter::new(&self.theme).value(value))
            .child(div().text_color(c.text).text_size(px(ty.ui_size_sm)).child(text))
    }

    /// The per-agent block of the full bar.
    fn agent_block(&self, name: &'static str, info: &UsageInfo) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let week = info
            .fresh_official()
            .is_none()
            .then(|| format!("{} sem · estimado", compact_tokens(info.week.billable())));
        div()
            .id(SharedString::from(format!("agent-{name}")))
            .h(px(42.0))
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .rounded(px(Radius::XS))
            .bg(c.surface_1)
            .font_family(ty.ui_family)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(agent_icon(AgentMark::from_name(name), 16.0, c.text))
                    .child(div().text_size(px(ty.ui_size_md)).text_color(c.text).child(name)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(self.meter_cell("5 h", info.fresh_five_hour()))
                    .child(self.meter_cell("sem", info.fresh_official())),
            )
            .children(week.map(|w| div().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(w)))
            .cursor_pointer()
            .hover(move |s| s.bg(c.surface_2))
            .when_some(self.on_usage.clone(), |d, f| d.on_click(move |_, window, cx| f(window, cx)))
    }

    fn read_label() -> SharedString {
        match mem::working_set_bytes() {
            Some(b) => format!("RAM {}", mem::format_mb(b)).into(),
            None => "RAM --".into(),
        }
    }
}

impl Render for StatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ty = self.theme.typography;
        let (full_fits, usage_fits, ram_fits) = visible_pieces(f32::from(window.viewport_size().width));
        let git = self.git.as_ref().and_then(|g| g.read(cx).info().cloned());
        let usage = self.usage.read(cx).info().cloned();
        let orphans = self.orphan_pill(cx);
        let orphans_compact = self.orphan_pill(cx);
        let attention = self.attention_pill(cx);
        let attention_compact = self.attention_pill(cx);
        // The shell already asks for the compact bar when the window is narrow; this keeps the
        // contents consistent if it did not.
        if self.mode == BottomBar::Full && full_fits {
            return div()
                .h(px(STATUS_FULL_H))
                .flex_none()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .font_family(ty.ui_family)
                .text_size(px(ty.ui_size_sm))
                .children(usage.as_ref().map(|u| self.agent_block("Claude", u)))
                .children(
                    usage
                        .is_none()
                        .then(|| div().text_color(self.theme.colors.muted).child("Sin datos de uso todavía")),
                )
                .children(git.map(|g| self.git_pill(&g)))
                .children(orphans)
                .children(attention)
                .child(div().flex_1())
                .children(ram_fits.then(|| Pill::compact("ram-pill", &self.theme).label(self.mem_label.clone())))
                .into_any_element();
        }
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
            .children(orphans_compact)
            .children(attention_compact)
            .children(usage.filter(|_| usage_fits).map(|u| self.usage_pill(&u)))
            .child(div().flex_1())
            .children(ram_fits.then(|| Pill::compact("ram-pill", &self.theme).label(self.mem_label.clone())))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_formatting() {
        let now = 1_000_000;
        assert_eq!(format_reset(now + 45 * 60_000, now), "45 min");
        assert_eq!(format_reset(now + 130 * 60_000, now), "2 h 10 min");
        assert_eq!(format_reset(now + (3 * 24 + 5) * 3_600_000, now), "3 d 5 h");
        assert_eq!(format_reset(now - 5, now), "0 min");
    }

    #[test]
    fn pieces_hide_in_priority_order() {
        assert_eq!(visible_pieces(1440.0), (true, true, true));
        assert_eq!(visible_pieces(1000.0), (false, true, true));
        assert_eq!(visible_pieces(600.0), (false, false, true));
        assert_eq!(visible_pieces(320.0), (false, false, false));
    }

    #[test]
    fn bar_heights() {
        assert_eq!(bar_height(BottomBar::Hidden), 0.0);
        assert!(bar_height(BottomBar::Full) > bar_height(BottomBar::Compact));
    }
}
