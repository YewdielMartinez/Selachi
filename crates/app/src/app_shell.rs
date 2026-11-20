//! Window layout (DESIGN.md): icon rail | titlebar + floating terminal card + status bar.
//! No divider lines: zones are separated by tone only.

use gpui::{
    Context, Entity, IntoElement, Render, SharedString, Window, actions, div, prelude::*, px,
};
use theme::{Mode, Radius, Theme};

use crate::rail::rail;
use crate::status_bar::StatusBar;
use crate::terminal_view::TerminalView;
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
    /// Terminal colors are fixed at spawn (term_core has no palette update API yet), so the
    /// card keeps the theme the session was created with.
    term_theme: Theme,
    terminal: Entity<TerminalView>,
    status: Entity<StatusBar>,
}

impl AppShell {
    pub fn new(theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal = cx.new(|cx| TerminalView::new(theme, window, cx));
        let status = cx.new(|cx| StatusBar::new(theme, cx));
        // Repaint the titlebar when the terminal's title changes.
        cx.observe(&terminal, |_, _, cx| cx.notify()).detach();
        Self { theme, term_theme: theme, terminal, status }
    }

    /// Dev shortcut (Ctrl+Shift+T): Dark -> Light -> OLED -> Dark. Chrome only.
    fn toggle_theme(&mut self, _: &ToggleTheme, _window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.theme.mode {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Oled,
            Mode::Oled => Mode::Dark,
        };
        self.theme = Theme::for_mode(next);
        let theme = self.theme;
        self.status.update(cx, |s, cx| s.set_theme(theme, cx));
        cx.notify();
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let term = self.terminal.read(cx);
        let shell: SharedString = term.shell_name().clone();
        // Placeholder for the cwd until OSC 7 / shell integration exists: use the shell title.
        let detail: SharedString =
            if term.title().is_empty() { "~".into() } else { term.title().clone() };
        let card_bg = self.term_theme.colors.surface_0;

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
                    .child(titlebar(&self.theme, shell, detail, window.is_maximized()))
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
