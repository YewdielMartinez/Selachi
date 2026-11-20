//! The Selachi panel contract (docs/PLAN.md sections 16 and 17).
//!
//! The shell (`crates/app`) renders *zones* (rail, sidebar, tabs, main, right panel, bottom bar)
//! from a [`store::ResolvedLayout`]. Everything that fills a zone goes through two small traits so
//! that no panel knows where it is mounted and no tab knows how it is painted:
//!
//! * [`PanelProvider`]: a view factory (git, context, processes, viewer, usage, chat...). A
//!   provider declares the zones it may live in ([`PanelProvider::allowed_zones`]) and builds its
//!   view for a [`PanelCtx`]. The view **adapts to [`PanelCtx::width_class`]** and never assumes
//!   its zone: the same panel can be a 220 px right column today and a full pane tomorrow.
//! * [`TabModel`]: a long-lived entity (a terminal session, a chat thread, a file viewer) that the
//!   shell lists as a top *pill* or as a sidebar *row* from the same data ([`TabInfo`]). The model
//!   owns its resources: when the shell drops the tab (closing it), everything the model owns
//!   (PTY session, watchers, tasks) is dropped with it.
//!
//! Providers are registered once at startup in a [`PanelRegistry`] (a gpui `Global`), and looked
//! up by [`PanelId`] (the id stored in `layout.right_panel`).
//!
//! # Contract summary
//!
//! * `build` is called on the UI thread whenever the shell needs a fresh view: when the panel is
//!   first shown, when its zone or width class changes, or when [`PanelProvider::context_changed`]
//!   returns `false`. Keep it cheap and do the slow work in a gpui task.
//! * Every asynchronous update **must** end in `cx.notify()` on the owning entity. gpui only
//!   redraws on demand, which is what keeps the idle CPU at 0 %: never poll on a timer to
//!   "refresh" a view.
//! * A [`TabModel`] emits [`TabEvent`]s (through gpui's `EventEmitter`) when its title, subtitle or
//!   status change; the shell re-renders its lists on those events and **not** on every repaint of
//!   the tab's content (a terminal that scrolls does not repaint the sidebar).
//!
//! A complete, runnable example lives in `tests/example_panel.rs`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyView, App, Context, Entity, EntityId, EventEmitter, Global, SharedString, Subscription,
    Window,
};
use theme::Theme;

pub use store::PanelId;
pub use ui_kit::{Icon, RowStatus};

// ---------------------------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------------------------

/// A place where a panel may be mounted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelZone {
    /// The sidebar column (a section of it).
    Sidebar,
    /// A pane of the main (split) area.
    Main,
    /// The right-hand panel column.
    Right,
    /// The bottom bar.
    Bottom,
}

/// How much room the panel got. A panel switches between a dense list and a roomy layout with
/// this, not with its zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WidthClass {
    /// Narrower than [`WidthClass::COMPACT_BELOW_PX`]: one line per item, no secondary columns.
    Compact,
    Regular,
}

impl WidthClass {
    /// Widths (logical px) below this are [`WidthClass::Compact`].
    pub const COMPACT_BELOW_PX: f32 = 320.0;

    pub fn from_width(width_px: f32) -> Self {
        if width_px < Self::COMPACT_BELOW_PX { Self::Compact } else { Self::Regular }
    }
}

/// What the shell tells a panel when it builds (or re-targets) its view.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelCtx {
    /// Where the view is mounted. Informational (for example, to pick a header style); a panel must
    /// work in every zone listed in its `allowed_zones`.
    pub zone: PanelZone,
    pub width_class: WidthClass,
    /// Working directory of the active tab, if it has one.
    pub cwd: Option<PathBuf>,
    /// Root of the project (git root or the project folder) that contains `cwd`, if known.
    pub project_root: Option<PathBuf>,
    pub theme: Theme,
}

/// A view factory registered in the [`PanelRegistry`].
///
/// ```ignore
/// struct ClockPanel;
/// impl PanelProvider for ClockPanel {
///     fn id(&self) -> PanelId { PanelId::new("clock") }
///     fn title(&self) -> SharedString { "Reloj".into() }
///     fn icon(&self) -> Icon { Icon::History }
///     fn allowed_zones(&self) -> &'static [PanelZone] { &[PanelZone::Right, PanelZone::Sidebar] }
///     fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
///         cx.new(|_| Clock { ctx }).into()
///     }
/// }
/// ```
pub trait PanelProvider: 'static {
    /// Stable id (what `layout.right_panel` stores). Lowercase, e.g. `"git"`.
    fn id(&self) -> PanelId;
    /// Human title for menus and the panel header.
    fn title(&self) -> SharedString;
    /// 16 px pixel icon for the rail and menus.
    fn icon(&self) -> Icon;
    /// Zones this panel can be mounted in. The shell never mounts it anywhere else.
    fn allowed_zones(&self) -> &'static [PanelZone];
    /// Builds the view. See the module docs for the contract.
    fn build(&self, ctx: PanelCtx, window: &mut Window, cx: &mut App) -> AnyView;

    /// The active tab's directory (or the width class, theme...) changed. Update `view` in place
    /// and return `true`, or return `false` to have the shell call [`PanelProvider::build`] again.
    /// Updating in place is much cheaper for panels that own watchers (git, files).
    ///
    /// The default rebuilds.
    fn context_changed(
        &self,
        _view: &AnyView,
        _ctx: &PanelCtx,
        _window: &mut Window,
        _cx: &mut App,
    ) -> bool {
        false
    }
}

/// Providers by id, in registration order. Install it once as a gpui global:
///
/// ```ignore
/// let mut registry = PanelRegistry::default();
/// registry.register(GitPanelProvider);
/// cx.set_global(registry);
/// ```
#[derive(Default)]
pub struct PanelRegistry {
    providers: Vec<Rc<dyn PanelProvider>>,
}

impl Global for PanelRegistry {}

impl PanelRegistry {
    /// Adds a provider. A provider with an id that is already registered **replaces** the old one
    /// (so an app can override a built-in panel).
    pub fn register(&mut self, provider: impl PanelProvider) {
        let provider: Rc<dyn PanelProvider> = Rc::new(provider);
        let id = provider.id();
        match self.providers.iter_mut().find(|p| p.id() == id) {
            Some(slot) => *slot = provider,
            None => self.providers.push(provider),
        }
    }

    pub fn get(&self, id: &PanelId) -> Option<Rc<dyn PanelProvider>> {
        self.providers.iter().find(|p| &p.id() == id).cloned()
    }

    /// Every provider, in registration order (for the rail and menus).
    pub fn all(&self) -> impl Iterator<Item = &Rc<dyn PanelProvider>> {
        self.providers.iter()
    }

    /// The providers that may be mounted in `zone`.
    pub fn for_zone(&self, zone: PanelZone) -> Vec<Rc<dyn PanelProvider>> {
        self.providers.iter().filter(|p| p.allowed_zones().contains(&zone)).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------------------------

/// Leading mark of a tab: the kind of tab, or the agent that runs in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabIcon {
    /// A kind of tab (terminal, viewer, chat...).
    Kind(Icon),
    /// An agent session (`"claude"`, `"codex"`...). Painted with the agents mark.
    Agent(SharedString),
}

impl TabIcon {
    /// The pixel icon to paint.
    pub fn icon(&self) -> Icon {
        match self {
            TabIcon::Kind(i) => *i,
            TabIcon::Agent(_) => Icon::Agents,
        }
    }
}

/// Answer of [`TabModel::can_close`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseCheck {
    /// Close right away.
    Allow,
    /// Ask first; the text says what would be lost ("Hay un comando en ejecucion").
    Confirm(SharedString),
    /// The tab cannot be closed (a pinned tab).
    Deny,
}

/// What a [`TabModel`] tells the shell about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabEvent {
    TitleChanged,
    /// Subtitle (model, branch) or last-activity changed.
    SubtitleChanged,
    StatusChanged,
    /// The tab wants to go away (the shell removes it). For example the shell process exited and
    /// the user asked to close on exit.
    Closed,
}

/// A long-lived entity the shell lists as a pill or as a sidebar row. Implement it for the entity's
/// state type and wrap the entity in a [`TabHandle`]:
///
/// ```ignore
/// impl EventEmitter<TabEvent> for ChatThread {}
/// impl TabModel for ChatThread { /* ... */ }
/// let handle = TabHandle::new(cx.new(|cx| ChatThread::new(cx)));
/// ```
///
/// Getters take `&App` so they can read other entities (a git monitor, a settings global).
pub trait TabModel: EventEmitter<TabEvent> + Sized + 'static {
    fn title(&self, cx: &App) -> SharedString;
    /// Agent mark or kind icon.
    fn icon(&self, cx: &App) -> TabIcon;
    /// Second line of a sidebar row: the model, or the git branch.
    fn subtitle(&self, _cx: &App) -> Option<SharedString> {
        None
    }
    /// `None` = idle / not applicable (the row shows its icon instead of a status glyph).
    fn status(&self, _cx: &App) -> Option<RowStatus> {
        None
    }
    /// When the tab last changed state (status change or output). The shell shows the elapsed time
    /// ("49m") as the row's meta; it is recomputed on render, never ticking on its own.
    fn last_activity(&self, _cx: &App) -> Option<Instant> {
        None
    }
    /// Working directory, used to group by project and to resolve per-project layouts.
    fn cwd(&self, _cx: &App) -> Option<PathBuf> {
        None
    }
    /// Root of the project (git root) that contains `cwd`, when the model knows it. The shell groups
    /// by it (`group_by = project`) and falls back to `cwd` when it is `None`.
    fn project_root(&self, _cx: &App) -> Option<PathBuf> {
        None
    }
    /// Agent name when the tab is an agent session (used by `group_by = agent`).
    fn agent(&self, _cx: &App) -> Option<SharedString> {
        None
    }
    /// The tab's body, hosted by a pane of the main area. Returned each frame: it must be a cheap
    /// clone of a stored view.
    fn content(&self, cx: &App) -> AnyView;
    fn can_close(&self, _cx: &App) -> CloseCheck {
        CloseCheck::Allow
    }
    /// The shell is about to drop the tab: release PTYs, watchers and tasks now.
    fn close(&mut self, _cx: &mut Context<Self>) {}
    /// Give keyboard focus to the content.
    fn focus(&self, _window: &mut Window, _cx: &App) {}
    /// The app theme changed.
    fn theme_changed(&mut self, _theme: Theme, _cx: &mut Context<Self>) {}
}

/// Identity of a tab (the id of its entity).
pub type TabId = EntityId;

/// A snapshot of everything the shell paints for a tab.
#[derive(Clone, Debug, PartialEq)]
pub struct TabInfo {
    pub id: TabId,
    pub title: SharedString,
    pub icon: TabIcon,
    pub subtitle: Option<SharedString>,
    pub status: Option<RowStatus>,
    pub last_activity: Option<Instant>,
    pub cwd: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    pub agent: Option<SharedString>,
}

type TabEventFn = Box<dyn FnMut(TabEvent, &mut App)>;

/// Type-erased access to a [`TabModel`] entity.
trait TabOps {
    fn id(&self) -> TabId;
    fn info(&self, cx: &App) -> TabInfo;
    fn content(&self, cx: &App) -> AnyView;
    fn can_close(&self, cx: &App) -> CloseCheck;
    fn close(&self, cx: &mut App);
    fn focus(&self, window: &mut Window, cx: &App);
    fn theme_changed(&self, theme: Theme, cx: &mut App);
    fn subscribe(&self, cx: &mut App, f: TabEventFn) -> Subscription;
    fn as_any(&self) -> &dyn Any;
}

struct EntityOps<T: TabModel>(Entity<T>);

impl<T: TabModel> TabOps for EntityOps<T> {
    fn id(&self) -> TabId {
        self.0.entity_id()
    }

    fn info(&self, cx: &App) -> TabInfo {
        let m = self.0.read(cx);
        TabInfo {
            id: self.0.entity_id(),
            title: m.title(cx),
            icon: m.icon(cx),
            subtitle: m.subtitle(cx),
            status: m.status(cx),
            last_activity: m.last_activity(cx),
            cwd: m.cwd(cx),
            project_root: m.project_root(cx),
            agent: m.agent(cx),
        }
    }

    fn content(&self, cx: &App) -> AnyView {
        self.0.read(cx).content(cx)
    }

    fn can_close(&self, cx: &App) -> CloseCheck {
        self.0.read(cx).can_close(cx)
    }

    fn close(&self, cx: &mut App) {
        self.0.update(cx, |m, cx| m.close(cx));
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        self.0.read(cx).focus(window, cx);
    }

    fn theme_changed(&self, theme: Theme, cx: &mut App) {
        self.0.update(cx, |m, cx| m.theme_changed(theme, cx));
    }

    fn subscribe(&self, cx: &mut App, mut f: TabEventFn) -> Subscription {
        cx.subscribe(&self.0, move |_, event: &TabEvent, cx| f(*event, cx))
    }

    fn as_any(&self) -> &dyn Any {
        &self.0
    }
}

/// A cheap, cloneable, type-erased handle to a [`TabModel`] entity. The shell stores these; the
/// last clone being dropped drops the entity (and everything the model owns).
#[derive(Clone)]
pub struct TabHandle(Rc<dyn TabOps>);

impl TabHandle {
    pub fn new<T: TabModel>(entity: Entity<T>) -> Self {
        Self(Rc::new(EntityOps(entity)))
    }

    pub fn id(&self) -> TabId {
        self.0.id()
    }

    pub fn info(&self, cx: &App) -> TabInfo {
        self.0.info(cx)
    }

    pub fn content(&self, cx: &App) -> AnyView {
        self.0.content(cx)
    }

    pub fn can_close(&self, cx: &App) -> CloseCheck {
        self.0.can_close(cx)
    }

    /// Releases the model's resources. Call before dropping the handle.
    pub fn close(&self, cx: &mut App) {
        self.0.close(cx);
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.0.focus(window, cx);
    }

    pub fn theme_changed(&self, theme: Theme, cx: &mut App) {
        self.0.theme_changed(theme, cx);
    }

    /// Calls `f` for every [`TabEvent`] the model emits, until the subscription is dropped.
    pub fn subscribe(&self, cx: &mut App, f: impl FnMut(TabEvent, &mut App) + 'static) -> Subscription {
        self.0.subscribe(cx, Box::new(f))
    }

    /// The entity back as its concrete type, if it is a `T`.
    pub fn downcast<T: TabModel>(&self) -> Option<Entity<T>> {
        self.0.as_any().downcast_ref::<Entity<T>>().cloned()
    }
}

/// Short elapsed time for a row's meta: `ahora`, `5m`, `2h`, `3d`.
pub fn format_elapsed(secs: u64) -> String {
    match secs {
        0..=59 => "ahora".to_string(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_class_threshold() {
        assert_eq!(WidthClass::from_width(220.0), WidthClass::Compact);
        assert_eq!(WidthClass::from_width(319.9), WidthClass::Compact);
        assert_eq!(WidthClass::from_width(320.0), WidthClass::Regular);
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(0), "ahora");
        assert_eq!(format_elapsed(59), "ahora");
        assert_eq!(format_elapsed(60), "1m");
        assert_eq!(format_elapsed(49 * 60), "49m");
        assert_eq!(format_elapsed(2 * 3_600 + 5), "2h");
        assert_eq!(format_elapsed(3 * 86_400), "3d");
    }

    #[test]
    fn tab_icon_maps_to_icon() {
        assert_eq!(TabIcon::Kind(Icon::Terminal).icon(), Icon::Terminal);
        assert_eq!(TabIcon::Agent("claude".into()).icon(), Icon::Agents);
    }
}
