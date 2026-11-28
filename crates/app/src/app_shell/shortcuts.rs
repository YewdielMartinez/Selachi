//! What the shell does when the shortcuts or the OS appearance change.

use super::*;

impl AppShell {
    /// The palette lists each action with its shortcut: rebuild the list after a rebinding.
    pub(super) fn refresh_palette_actions(&mut self, cx: &mut Context<Self>) {
        let actions = palette::palette_actions(&self.config.keybindings, cx);
        self.palette.update(cx, |p, _| {
            p.set_source(Arc::new(ui_palette::ActionsSource::new(actions)))
        });
    }

    /// The OS switched between light and dark (a Windows notification, nothing polls): a
    /// "Sistema" theme follows it. Light, Dark and OLED are explicit and stay as they are.
    pub(super) fn os_appearance_changed(
        &mut self,
        appearance: WindowAppearance,
        cx: &mut Context<Self>,
    ) {
        if self.config.appearance.mode == ThemeMode::System {
            self.set_theme(theme_for(&self.config, appearance), cx);
        }
    }
}
