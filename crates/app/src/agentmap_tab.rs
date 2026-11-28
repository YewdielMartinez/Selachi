//! The subagent map as a tab of the main zone («Abrir en pestaña»).
//!
//! `ui_agentmap::AgentMapPanelProvider` builds the view; this wrapper gives it a title and an icon
//! so the shell lists it like any other tab. The view only listens to the registry and the chat
//! while its tab is in front of a pane: the shell calls [`AgentMapTab::set_visible`] whenever the
//! visible panes change, so a map tab behind another tab costs nothing.

use gpui::{AnyView, App, Context, Entity, EventEmitter, SharedString};
use panel_api::{TabEvent, TabIcon, TabModel};
use ui_agentmap::AgentMapView;

pub struct AgentMapTab {
    view: Entity<AgentMapView>,
    content: AnyView,
}

impl AgentMapTab {
    pub fn new(view: Entity<AgentMapView>, content: AnyView) -> Self {
        Self { view, content }
    }

    /// Attaches the map to its sources while the tab is on screen, detaches it otherwise.
    pub fn set_visible(&self, visible: bool, cx: &mut App) {
        self.view.update(cx, |v, cx| v.set_active(visible, cx));
    }
}

impl EventEmitter<TabEvent> for AgentMapTab {}

impl TabModel for AgentMapTab {
    fn title(&self, _cx: &App) -> SharedString {
        "Mapa de subagentes".into()
    }

    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Kind(ui_agentmap::MAP_ICON)
    }

    fn content(&self, _cx: &App) -> AnyView {
        self.content.clone()
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        // release the model right away (the view detaches when it is dropped, this is just earlier)
        self.view.update(cx, |v, cx| v.set_active(false, cx));
    }
}
