//! Selachi layout customization panel (docs/PLAN.md section 17).
//!
//! A gpui view meant to be the content of the titlebar popover: preset cards drawn as
//! pixel-art wireframes, every fine option, a global / per-project scope switch and a live
//! preview. Every change is applied at once: the panel stores only the difference against the
//! preset (`LayoutConfig::set_resolved`), validates it (`normalize`, with the corrections shown
//! as a muted note) and calls `on_change` with the whole new `LayoutConfig`.
//!
//! ```ignore
//! ui_layout::init(cx); // once: key bindings (Tab / Shift+Tab, text field)
//! let panel = ui_layout::layout_panel(config.layout.clone(), Some(cwd), theme, |layout, _, cx| {
//!     // persist: config.set_layout(layout); config.save(); ... re-resolve the shell
//! }, cx);
//! // on config hot reload:  panel.update(cx, |p, cx| p.set_config(new.layout.clone(), cx));
//! // on theme change:       panel.update(cx, |p, cx| p.set_theme(theme, cx));
//! // when the popover opens: panel.read(cx).focus(window);
//! ```
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod state;
mod text_field;
mod view;
mod wire;

pub use state::{LayoutState, Scope, normalize_es};
pub use view::{LayoutPanel, PANEL_WIDTH, layout_panel};

use gpui::{App, KeyBinding, actions};

actions!(ui_layout, [FocusNextField, FocusPrevField]);

/// Key context of the panel root.
pub(crate) const PANEL_CONTEXT: &str = "LayoutPanel";

/// Register the key bindings (Tab / Shift+Tab focus order, text input). Call once at startup.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextField, Some(PANEL_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevField, Some(PANEL_CONTEXT)),
    ]);
    text_field::bind_keys(cx);
}
