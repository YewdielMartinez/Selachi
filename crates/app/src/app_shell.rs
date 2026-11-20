//! Window layout (DESIGN.md): icon rail | titlebar + floating terminal card + status bar.
//! No divider lines: zones are separated by tone only.

use std::path::Path;

use gpui::{
    Context, Entity, IntoElement, Render, SharedString, Subscription, Window, actions, div,
    prelude::*, px,
};
use theme::{Mode, Radius, Theme};

use crate::git_monitor::GitMonitor;
use crate::paths;
use crate::rail::rail;
use crate::status_bar::StatusBar;
use crate::terminal_view::{TerminalEvent, TerminalView};
use crate::usage_monitor::UsageMonitor;
use crate::titlebar::titlebar;

pub use crate::rail::RAIL_W;
/// Height of the titlebar (named `TAB_H` for the initial terminal size estimate).
pub use crate::titlebar::TITLEBAR_H as TAB_H;
/// Padding between the terminal element edge and the character grid.
pub const TERM_PAD: f32 = 8.0;
/// Gap between the terminal card and the window / rail edges.
const CARD_MARGIN: f32 = 8.0;
/// Inner padding of the card; must exceed r * (1 - 1/sqrt 2) so the terminal's square
/// background never pokes out of the card's rounded corners.
const CARD_PAD_X: f32 = 8.0;
const CARD_PAD_Y: f32 = 5.0;

actions!(selachi, [ToggleTheme]);

pub struct AppShell {
    theme: Theme,
    terminal: Entity<TerminalView>,
    status: Entity<StatusBar>,
    git: Entity<GitMonitor>,
    usage: Entity<UsageMonitor>,
    /// Observers and subscriptions live as long as the shell; dropping it (closing the window)
    /// drops the monitors, which cancels their tasks and watchers.
    _subs: Vec<Subscription>,
}

impl AppShell {
    pub fn new(theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal = cx.new(|cx| TerminalView::new(theme, window, cx));
        let git = cx.new(|_| GitMonitor::new());
        let usage = cx.new(UsageMonitor::new);
        let status = cx.new(|cx| StatusBar::new(theme, git.clone(), usage.clone(), cx));
        let focused = window.is_window_active();
        status.update(cx, |s, cx| s.set_active(focused, cx));

        if let Some(cwd) = terminal.read(cx).cwd().map(Path::to_path_buf) {
            git.update(cx, |g, cx| g.set_cwd(cwd, cx));
        }
        let subs = vec![
            // Repaint the breadcrumb and tab when the terminal's cwd changes.
            cx.observe(&terminal, |_, _, cx| cx.notify()),
            cx.subscribe(&terminal, |this, _, event: &TerminalEvent, cx| match event {
                TerminalEvent::CwdChanged(cwd) => {
                    this.git.update(cx, |g, cx| g.set_cwd(cwd.clone(), cx));
                }
                TerminalEvent::CommandFinished => this.git.update(cx, |g, cx| g.refresh(cx)),
            }),
            // Usage is rescanned every 60 s only while the window is focused.
            cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                this.usage.update(cx, |u, cx| u.set_active(active, cx));
                this.status.update(cx, |s, cx| s.set_active(active, cx));
            }),
        ];
        Self { theme, terminal, status, git, usage, _subs: subs }
    }

    /// Dev shortcut (Ctrl+Shift+T): Dark -> Light -> OLED -> Dark. Restyles chrome and terminal.
    fn toggle_theme(&mut self, _: &ToggleTheme, _window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.theme.mode {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Oled,
            Mode::Oled => Mode::Dark,
        };
        self.theme = Theme::for_mode(next);
        let theme = self.theme;
        self.status.update(cx, |s, cx| s.set_theme(theme, cx));
        self.terminal.update(cx, |t, cx| t.set_theme(theme, cx));
        cx.notify();
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let term = self.terminal.read(cx);
        let (tab_label, detail): (SharedString, SharedString) = match term.cwd() {
            Some(cwd) => (paths::tab_label(cwd).into(), paths::breadcrumb(cwd).into()),
            None => (term.shell_name().clone(), "~".into()),
        };
        // The terminal background equals surface_0 in every mode (theme test), so the card and
        // the terminal are one surface.
        let card_bg = c.surface_0;

        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(ty.ui_family)
            .key_context("AppShell")
            .on_action(cx.listener(Self::toggle_theme))
            .child(rail(&self.theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(titlebar(&self.theme, tab_label, detail, window.is_maximized()))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .mx(px(CARD_MARGIN))
                            .px(px(CARD_PAD_X))
                            .py(px(CARD_PAD_Y))
                            .rounded(px(Radius::MD))
                            .bg(card_bg)
                            .flex()
                            .flex_col()
                            .child(self.terminal.clone()),
                    )
                    .child(self.status.clone()),
            )
    }
}
