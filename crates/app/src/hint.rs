//! The shell's inline hint: one small card above the status bar ("Contexto copiado · pégalo en el
//! agente", "cd aquí"). It replaces toasts: no stacking, no timers while nothing is shown, and a
//! single one-shot timer to dismiss it.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px,
};
use theme::{Radius, Theme};
use ui_kit::{Button, Icon, IconButton};

/// What the hint's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintAction {
    /// Type `Set-Location` into the active terminal.
    Cd(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub text: SharedString,
    pub action: Option<(SharedString, HintAction)>,
}

impl Hint {
    pub fn text(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            action: None,
        }
    }

    pub fn with_action(mut self, label: impl Into<SharedString>, action: HintAction) -> Self {
        self.action = Some((label.into(), action));
        self
    }
}

/// The hint card. `on_action` runs the button, `on_close` dismisses.
pub fn hint_card(
    theme: &Theme,
    hint: &Hint,
    bottom: f32,
    on_action: impl Fn(HintAction, &mut Window, &mut App) + 'static,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    let mut card = div()
        .id("hint-card")
        .occlude()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .py(px(6.0))
        .pl(px(14.0))
        .pr(px(6.0))
        .rounded(px(Radius::MD))
        .bg(c.surface_2)
        .border_1()
        .border_color(c.outline_soft)
        .text_size(px(ty.ui_size_md))
        .text_color(c.text)
        .max_w(px(640.0))
        .child(div().min_w_0().child(hint.text.clone()));
    if let Some((label, action)) = hint.action.clone() {
        card = card.child(
            Button::tonal("hint-action", theme, label)
                .on_click(move |_, window, cx| on_action(action.clone(), window, cx)),
        );
    }
    card = card.child(
        IconButton::new("hint-close", theme, Icon::Close)
            .size(24.0)
            .icon_size(10.0)
            .on_click(move |_, window, cx| on_close(window, cx)),
    );
    div()
        .absolute()
        .bottom(px(bottom))
        .left_0()
        .right_0()
        .flex()
        .flex_row()
        .justify_center()
        .px(px(16.0))
        .child(card)
        .into_any_element()
}
