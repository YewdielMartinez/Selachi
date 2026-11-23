//! The settings screen as a tab of the main zone (`Ctrl+,`, the gear of the sidebar footer).
//!
//! `ui_settings::SettingsPanelProvider` builds the view (it re-themes it); this wrapper only gives
//! it a title and an icon so the shell lists it like any other tab.

use gpui::{AnyView, App, Entity, EventEmitter, Focusable, SharedString, Window};
use panel_api::{Icon, TabEvent, TabIcon, TabModel};
use ui_settings::SettingsView;

pub struct SettingsTab {
    view: Entity<SettingsView>,
    content: AnyView,
}

impl SettingsTab {
    pub fn new(view: Entity<SettingsView>, content: AnyView) -> Self {
        Self { view, content }
    }
}

impl EventEmitter<TabEvent> for SettingsTab {}

impl TabModel for SettingsTab {
    fn title(&self, _cx: &App) -> SharedString {
        "Ajustes".into()
    }

    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Kind(Icon::Settings)
    }

    fn content(&self, _cx: &App) -> AnyView {
        self.content.clone()
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        self.view.focus_handle(cx).focus(window);
    }
}
