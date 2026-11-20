//! Example of both halves of the contract: a panel (`PanelProvider`) that adapts to its width
//! class, and a tab (`TabModel`) painted by the shell as a pill or as a sidebar row.
//!
//! Copy this file as the starting point of a new panel crate (`ui_git`, `ui_viewer`...).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gpui::{
    AnyView, App, AppContext as _, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use panel_api::{
    CloseCheck, Icon, PanelCtx, PanelId, PanelProvider, PanelRegistry, PanelZone, RowStatus, TabEvent,
    TabIcon, TabModel, WidthClass,
};

/// The view. It reads `ctx.width_class` and never looks at `ctx.zone` to decide what to show.
struct NotesPanel {
    ctx: PanelCtx,
    lines: Vec<SharedString>,
}

impl Render for NotesPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let roomy = self.ctx.width_class == WidthClass::Regular;
        div()
            .size_full()
            .p(px(if roomy { 16.0 } else { 8.0 }))
            .children(self.lines.iter().map(|l| {
                // Compact: only the first word of each line. Regular: the whole line.
                let text: SharedString = if roomy {
                    l.clone()
                } else {
                    l.split_whitespace().next().unwrap_or_default().to_string().into()
                };
                div().child(text)
            }))
    }
}

/// The factory. One value registered at startup; it carries no per-view state.
struct NotesProvider;

impl PanelProvider for NotesProvider {
    fn id(&self) -> PanelId {
        PanelId::new("notes")
    }
    fn title(&self) -> SharedString {
        "Notas".into()
    }
    fn icon(&self) -> Icon {
        Icon::Files
    }
    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Right, PanelZone::Sidebar, PanelZone::Main]
    }
    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| NotesPanel { ctx, lines: vec!["una nota larga".into(), "otra nota".into()] })
            .into()
    }

    /// Cheap re-targeting: keep the view, swap the context.
    fn context_changed(&self, view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        let Ok(panel) = view.clone().downcast::<NotesPanel>() else { return false };
        panel.update(cx, |p, cx| {
            p.ctx = ctx.clone();
            cx.notify(); // every async or external update ends in notify()
        });
        true
    }
}

/// A tab model. The shell never sees this type, only a `TabHandle` wrapping its entity.
struct ChatThread {
    title: SharedString,
    working: bool,
    body: AnyView,
}

impl EventEmitter<TabEvent> for ChatThread {}

impl TabModel for ChatThread {
    fn title(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }
    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Agent("claude".into())
    }
    fn subtitle(&self, _cx: &App) -> Option<SharedString> {
        Some("opus-5.5 · feat/ui".into())
    }
    fn status(&self, _cx: &App) -> Option<RowStatus> {
        Some(if self.working { RowStatus::Working } else { RowStatus::Done })
    }
    fn content(&self, _cx: &App) -> AnyView {
        self.body.clone()
    }
    fn can_close(&self, _cx: &App) -> CloseCheck {
        if self.working {
            CloseCheck::Confirm("El agente sigue trabajando".into())
        } else {
            CloseCheck::Allow
        }
    }
}

#[test]
fn registry_registers_looks_up_and_filters_by_zone() {
    let mut registry = PanelRegistry::default();
    assert!(registry.is_empty());
    registry.register(NotesProvider);
    assert_eq!(registry.len(), 1);

    let found = registry.get(&PanelId::new("notes")).expect("registered");
    assert_eq!(found.title(), "Notas");
    assert_eq!(found.icon(), Icon::Files);
    assert!(registry.get(&PanelId::new("missing")).is_none());

    assert_eq!(registry.for_zone(PanelZone::Right).len(), 1);
    assert!(registry.for_zone(PanelZone::Bottom).is_empty());
}

#[test]
fn registering_the_same_id_replaces_the_provider() {
    struct Other;
    impl PanelProvider for Other {
        fn id(&self) -> PanelId {
            PanelId::new("notes")
        }
        fn title(&self) -> SharedString {
            "Otras notas".into()
        }
        fn icon(&self) -> Icon {
            Icon::History
        }
        fn allowed_zones(&self) -> &'static [PanelZone] {
            &[PanelZone::Bottom]
        }
        fn build(&self, _ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
            cx.new(|_| gpui::Empty).into()
        }
    }
    let mut registry = PanelRegistry::default();
    registry.register(NotesProvider);
    registry.register(Other);
    assert_eq!(registry.len(), 1);
    let p = registry.get(&PanelId::new("notes")).unwrap();
    assert_eq!(p.title(), "Otras notas");
    assert_eq!(registry.for_zone(PanelZone::Bottom).len(), 1);
}

#[test]
fn panel_ids_are_normalized() {
    assert_eq!(PanelId::new(" Git "), PanelId::new("git"));
}

// Compile-time check that `ChatThread` satisfies the contract and can be wrapped.
#[allow(dead_code)]
fn wraps(cx: &mut App, body: AnyView) -> panel_api::TabHandle {
    panel_api::TabHandle::new(cx.new(|_| ChatThread { title: "t".into(), working: false, body }))
}
