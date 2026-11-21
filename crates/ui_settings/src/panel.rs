//! `PanelProvider` for the Main zone and the 1-bit thumbnail for the layout panel.

use gpui::{AnyView, App, Entity, SharedString, Window};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone};
use theme::Theme;
use ui_kit::{WireFill, WireRect, Wireframe};

use crate::view::SettingsView;

/// Panel id for the registry.
pub const PANEL_ID: &str = "settings";

/// Registers the settings view as the `settings` panel. The shell mounts it in a Main pane (or as
/// a tab) when the rail button or `Ctrl+,` asks for it.
pub struct SettingsPanelProvider {
    view: Entity<SettingsView>,
}

impl SettingsPanelProvider {
    pub fn new(view: Entity<SettingsView>) -> Self {
        Self { view }
    }
}

impl PanelProvider for SettingsPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Ajustes".into()
    }

    fn icon(&self) -> Icon {
        Icon::Settings
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Main]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let view = self.view.clone();
        view.update(cx, |v, cx| v.set_theme(ctx.theme, cx));
        view.into()
    }

    fn context_changed(&self, _view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        self.view.update(cx, |v, cx| v.set_theme(ctx.theme, cx));
        true
    }
}

/// 1-bit thumbnail for the layout panel: the section list and a column of setting rows.
pub fn thumbnail(theme: &Theme) -> Wireframe {
    use WireFill::{Dither, Outline, Solid};
    let r = WireRect::new;
    Wireframe::new(theme).rects([
        r(1, 1, 8, 2, Solid),
        r(1, 4, 8, 2, Dither),
        r(1, 7, 8, 2, Dither),
        r(1, 10, 8, 2, Dither),
        r(1, 13, 8, 2, Dither),
        r(11, 1, 14, 2, Solid),
        r(11, 4, 20, 4, Outline),
        r(13, 5, 8, 1, Dither),
        r(26, 5, 4, 2, Solid),
        r(11, 9, 20, 4, Outline),
        r(13, 10, 10, 1, Dither),
        r(11, 14, 20, 4, Outline),
        r(13, 15, 6, 1, Dither),
    ])
}
