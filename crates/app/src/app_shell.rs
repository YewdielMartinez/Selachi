//! The window's coordinator (shell v2): zones (`rail`, `sidebar`, `tabs`, `main`, `right`,
//! `status`) laid out from [`store::ResolvedLayout`], the tab list and the split panes.
//!
//! `AppShell` does not paint the sidebar or the title bar and does not own the chat hub or the
//! session writer: those are entities of their own that it composes and talks to through events
//! (docs/auditoria-gpui.md H4, H5):
//!
//! | Entity | Owns | Talks to the shell with |
//! |---|---|---|
//! | [`SidebarView`] (x4 placements) | row / space hover, zone keyboard focus | [`SidebarEvent`] |
//! | [`TitleBarView`] | `...` and "all tabs" menus, tab strip scroll | [`TitleBarEvent`] |
//! | [`ChatHost`] | `chat.db` loading, the hub, projects, thread attention | [`ChatHostEvent`] |
//! | [`SessionPersistence`] | state db, save debounce, deferred restore | [`SessionEvent`] |
//!
//! **Render only reads.** `render` never mutates state, opens a database or spawns a task. What
//! used to be synchronised there happens where the cause is: the window's bounds observer
//! (`on_resize`), the activation observer (`sync_tree_poll`) and [`AppShell::refresh`], the one
//! place that follows every state change: it pushes the models of the chrome views (only the ones
//! that changed repaint) and, when the right panel, the glass background or a panel's width class
//! is out of date, schedules *one* `sync_window` for the next tick (the right panel needs
//! `&mut Window` to be built).
//!
//! The code is split by concern over several files that all extend this type:
//! `tabs` (tab model), `layout` (layout, config, theme, overlays, right panel, dragging),
//! `chrome` (zones, chrome models, zone focus), `render`, `integrations` (git, viewer, palette,
//! settings, onboarding, session) and `chat`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chat_core::ThreadId;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, AnyWindowHandle, App, AppContext as _, Bounds,
    Context, Div, ElementId, Entity, FocusHandle, Hsla, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task, Window,
    WindowBackgroundAppearance, actions, canvas, div, prelude::*, px, relative,
};
use panel_api::{
    CloseCheck, PanelCtx, PanelId, PanelRegistry, PanelZone, TabHandle, TabId, TabInfo, WidthClass,
};
use store::config::ThemeMode;
use store::{
    BottomBar, Config, ConfigEvent, ConfigWatcher, Density, LayoutConfig, RailVisibility,
    ResolvedLayout, SidebarSide, Splits, TabsPosition, layout_changed,
};
use theme::{Mode, Radius, Theme, motion::SHEET, motion::duration::BASE};
use ui_chat::ChatTab;
use ui_review::{AttentionEvent, FocusTarget, ReviewTab};
use ui_viewer::{ViewerEvent, ViewerTab};

use crate::adapt::{
    AdaptedLayout, Breakpoint, COLLAPSED_SIDEBAR_W, RIGHT_MAX_W, RIGHT_MIN_W, SidebarMode,
    adapt_layout, right_panel_width,
};
use crate::chat_host::{ChatHost, ChatHostEvent};
use crate::git_monitor::GitMonitor;
use crate::hint::Hint;
use crate::notify;
use crate::palette::{self, id as action_id};
use crate::rail::{RAIL_W as RAIL_WIDTH, RailAction, RailItem};
use crate::services::Services;
use crate::session::{SessionState, TabKind};
use crate::session_persistence::{SessionEvent, SessionPersistence};
use crate::sidebar_view::{SidebarEvent, SidebarOpts, SidebarView};
use crate::splits::{PaneSet, clamp_ratio, pane_count};
use crate::status_bar::StatusBar;
use crate::terminal_tab::TerminalTab;
use crate::terminal_view::TermEnv;
use crate::titlebar::TITLEBAR_H;
use crate::titlebar_view::{TitleBarEvent, TitleBarView};
use crate::usage_monitor::UsageMonitor;

mod chat;
mod chrome;
mod integrations;
mod layout;
mod render;
mod tabs;

pub use crate::rail::RAIL_W;
pub use crate::titlebar::TITLEBAR_H as TAB_H;

pub const TERM_PAD: f32 = 8.0;
/// Gap between a zone's card and the next one (also the drag-handle width).
const GUTTER: f32 = 8.0;
/// Width of the floating sidebar of zen mode.
const ZEN_SIDEBAR_W: f32 = 260.0;
/// Thickness of the invisible hover strips of zen mode.
const EDGE_STRIP: f32 = 12.0;
/// A layout transition is kept this long (the animation lasts 200 ms), then dropped.
const TRANSITION_KEEP: Duration = Duration::from_millis(260);
/// `config.toml` is saved this long after the last layout change.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
/// How long an inline hint stays.
const HINT_FOR: Duration = Duration::from_secs(8);
/// Width of the drawers (XS) and overlays (S).
const DRAWER_W: f32 = 320.0;
/// Alpha floor of in-window overlays: the desktop blur cannot reach what is *inside* the window,
/// so an overlay stays mostly opaque over the terminal text.
const OVERLAY_MAX_TRANSLUCENCY: u8 = 20;

actions!(
    selachi,
    [
        ToggleTheme,
        NewTab,
        NewTerminalTab,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleLayoutPanel,
        ConfirmClose,
        CancelClose,
        CancelOverlay,
        ToggleFullscreen,
        NextAttention,
        OpenSettings,
        ToggleSidebar,
        /// Ctrl+Shift+E: the keyboard focus goes to the sidebar.
        FocusSidebar,
        /// Ctrl+Shift+`: the keyboard focus goes back to the terminal.
        FocusTerminal,
        /// F6: the next zone (sidebar, main, right panel).
        NextZone,
        /// Shift+F6: the previous zone.
        PrevZone,
        Jump1,
        Jump2,
        Jump3,
        Jump4,
        Jump5,
        Jump6,
        Jump7,
        Jump8,
        Jump9
    ]
);

/// Everything the shell needs from the configuration at startup.
pub struct ConfigBoot {
    pub config: Arc<Config>,
    /// Keeps the file watcher alive and streams reloads.
    pub watcher: Option<(ConfigWatcher, UnboundedReceiver<ConfigEvent>)>,
    pub session: SessionBoot,
    /// `--cwd <dir>` / a positional folder: where the new tab opens (the saved session, if any,
    /// is restored first).
    pub start_dir: Option<PathBuf>,
}

/// The state database and the session to restore (if enabled and saved).
#[derive(Default)]
pub struct SessionBoot {
    pub db: Option<store::StateDb>,
    pub restore: Option<SessionState>,
}

/// A drawer / overlay that is open over the main area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    /// The sidebar: a drawer in XS, the expanded sidebar over its icon column in S.
    Left,
    /// The right panel: an overlay in S, a drawer in XS.
    Right,
}

/// The zones the keyboard can move between (F6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Zone {
    Sidebar,
    Main,
    Right,
}

/// Card paddings per density.
#[derive(Clone, Copy)]
struct Metrics {
    /// Outer margin of the cards.
    margin: f32,
    pad_x: f32,
    pad_y: f32,
}

impl Metrics {
    fn of(d: Density) -> Self {
        match d {
            Density::Comfortable => Self {
                margin: 8.0,
                pad_x: 8.0,
                pad_y: 5.0,
            },
            // Inner padding must exceed r * (1 - 1/sqrt 2) so the terminal's square background
            // never pokes out of the card's rounded corners.
            Density::Compact => Self {
                margin: 4.0,
                pad_x: 8.0,
                pad_y: 5.0,
            },
        }
    }
}

struct TabSlot {
    handle: TabHandle,
    _subs: Vec<Subscription>,
}

/// A layout change in flight: the zones animate from `from` to the current layout.
struct Transition {
    from: ResolvedLayout,
    epoch: u64,
}

/// What a pointer drag is resizing.
#[derive(Clone, Copy)]
enum Drag {
    Sidebar {
        side: SidebarSide,
        start_x: f32,
        start_w: u16,
    },
    Right {
        start_x: f32,
        start_w: f32,
    },
    SplitX,
    SplitY,
}

struct RightPanel {
    id: PanelId,
    ctx: PanelCtx,
    /// `None` = no provider registered for `id` (the placeholder is shown).
    view: Option<AnyView>,
}

/// Which edges of the window show their hidden chrome (zen mode).
#[derive(Default, Clone, Copy)]
struct Reveal {
    top: bool,
    left: bool,
}

/// A checkpoint restore waiting for the user's confirmation (what `git_svc` would do, listed).
struct RestoreConfirm {
    id: String,
    cwd: PathBuf,
    warnings: Vec<String>,
    stat: Option<String>,
    busy: bool,
}

/// The window the shell lives in and what was last synchronised with it.
struct Host {
    window: AnyWindowHandle,
    /// Logical width of the window as of the last resize.
    width: f32,
    /// The window background is blurred (a docked glass zone asked for it).
    blurred: bool,
    /// A `sync_window` is already scheduled.
    sync_pending: bool,
    /// Width class last given to the settings tab (a main-zone panel).
    main_class: WidthClass,
}

/// Drawers and overlays over the main area, and the zen mode's revealed edges.
struct DrawerState {
    open: Option<Overlay>,
    /// Panel shown by the right overlay / drawer.
    right: PanelId,
    epoch: u64,
    reveal: Reveal,
}

/// The zone animation after a layout change.
#[derive(Default)]
struct Anim {
    transition: Option<Transition>,
    epoch: u64,
    /// Wakes the shell once after a transition so the animated zones settle into plain boxes.
    task: Option<Task<()>>,
}

/// The split panes and what a drag is resizing.
struct SplitState {
    panes: PaneSet<TabId>,
    ratio_x: f32,
    ratio_y: f32,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    drag_moved: bool,
    /// Right panel width the user dragged to (`None` = the default share of the window).
    right_w: Option<f32>,
}

/// Saving `config.toml` after layout changes.
#[derive(Default)]
struct ConfigSave {
    /// Layouts written by us whose file-watcher echo has not arrived yet.
    echoes: Vec<LayoutConfig>,
    task: Option<Task<()>>,
    inflight: bool,
    dirty: bool,
}

/// Modal-ish things that sit over the window: confirmations, the hint and the "new tab" menu.
struct Prompts {
    confirm_close: Option<(TabId, SharedString)>,
    restore: Option<RestoreConfirm>,
    /// The "Nueva pestaña" menu (Terminal / Chat) and its focus (keys `T`, `C`, Enter, Esc).
    new_tab_menu: bool,
    menu_focus: FocusHandle,
    /// Inline hint above the status bar, and the one-shot timer that dismisses it.
    hint: Option<Hint>,
    hint_task: Option<Task<()>>,
}

/// Spaces of the unified sidebar: what the shell remembers for it.
#[derive(Default)]
struct SpaceData {
    /// Spaces (keys of [`crate::spaces::Space`]) folded in the unified sidebar.
    folded: HashSet<PathBuf>,
    /// Favorite and recent projects (path, label): listed as spaces without tabs.
    known: Vec<(PathBuf, String)>,
    task: Option<Task<()>>,
}

/// The screens that replace or join the shell: the settings tab's view and the first-run wizard.
#[derive(Default)]
struct Modes {
    /// The settings screen, created the first time it is opened.
    settings: Option<(Entity<ui_settings::SettingsView>, Subscription)>,
    /// The first-run wizard (or "Volver a configurar"): replaces the shell while it is open.
    onboarding: Option<(Entity<ui_onboarding::OnboardingView>, Subscription)>,
}

/// The entities that paint the chrome. The four sidebars are the zones that can show one at the
/// same time (the left / right column, the drawer over the main area, zen mode's floating one);
/// the title bar is shared by the normal row and zen's top bar, which are never visible together.
struct Chrome {
    left: Entity<SidebarView>,
    right: Entity<SidebarView>,
    drawer: Entity<SidebarView>,
    zen: Entity<SidebarView>,
    titlebar: Entity<TitleBarView>,
    /// Focus of the right panel's zone (F6).
    right_focus: FocusHandle,
}

/// Background work that lives as long as the shell.
#[derive(Default)]
struct Tasks {
    _toast: Option<Task<()>>,
    viewer: Option<Task<()>>,
    grammar: Option<Task<()>>,
    /// Background refresh of tree-tracked process owners; only alive while it is useful.
    tree_poll: Option<Task<()>>,
    _config: Option<Task<()>>,
    _watcher: Option<ConfigWatcher>,
}

pub struct AppShell {
    theme: Theme,
    config: Arc<Config>,
    /// The layout the config asks for (the intent).
    layout: ResolvedLayout,
    /// What is docked for the current window width ([`adapt_layout`] of `layout`).
    eff: AdaptedLayout,
    host: Host,
    drawer: DrawerState,
    anim: Anim,
    tabs: Vec<TabSlot>,
    splits: SplitState,
    status: Entity<StatusBar>,
    usage: Entity<UsageMonitor>,
    palette: Entity<ui_palette::CommandPalette>,
    right: Option<RightPanel>,
    /// Grammar plugins found at startup (for the doctor).
    grammars: Vec<String>,
    /// Keeps the file tree's git markers in sync with the active tab's repository.
    marks_sub: Option<Subscription>,
    /// Owners (process jobs) of tabs that were closed and still have processes.
    closed_owners: Vec<u64>,
    /// The layout popover's content (open exactly while it exists).
    layout_panel: Option<AnyView>,
    cfg_save: ConfigSave,
    spaces: SpaceData,
    /// Ctrl+B: the sidebar shows only icons even when there is room for it.
    sidebar_collapsed: bool,
    modes: Modes,
    prompts: Prompts,
    chat: Entity<ChatHost>,
    session: Entity<SessionPersistence>,
    chrome: Chrome,
    tasks: Tasks,
    /// Observers and subscriptions live as long as the shell; dropping it (closing the window)
    /// drops the tabs and monitors, which cancels their tasks and watchers.
    _subs: Vec<Subscription>,
}

/// What `new` does after `build`: the tabs.
pub(crate) struct Startup {
    restore: Option<SessionState>,
    start_dir: Option<PathBuf>,
    cwd0: PathBuf,
}

/// Next tab index when cycling by `dir` (+1 / -1), wrapping.
fn cycle_index(len: usize, current: usize, dir: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (current as isize + dir).rem_euclid(len as isize) as usize
}

/// Sidebar width after dragging by `dx` px (`side` is where the sidebar is docked).
fn dragged_width(side: SidebarSide, start_w: u16, dx: f32) -> u16 {
    let delta = if side == SidebarSide::Right { -dx } else { dx };
    (f32::from(start_w) + delta).round().clamp(
        f32::from(store::config::SIDEBAR_MIN_WIDTH),
        f32::from(store::config::SIDEBAR_MAX_WIDTH),
    ) as u16
}

/// Right panel width after dragging its left edge by `dx` px.
fn dragged_right_width(start_w: f32, dx: f32) -> f32 {
    (start_w - dx).round().clamp(RIGHT_MIN_W, RIGHT_MAX_W)
}

/// Width of the drawers and overlays: [`DRAWER_W`], but never wider than 90 % of the window.
fn drawer_width(window_w: f32) -> f32 {
    DRAWER_W.min(window_w * 0.9)
}

fn hsla(c: theme::Rgba) -> Hsla {
    c.into()
}

/// The theme mode of a configured one (a "System" theme is dark until the OS is followed).
pub(crate) fn mode_of(mode: ThemeMode) -> Mode {
    match mode {
        ThemeMode::Light => Mode::Light,
        ThemeMode::Oled => Mode::Oled,
        ThemeMode::Dark | ThemeMode::System => Mode::Dark,
    }
}

/// The theme of a configuration: its mode, with its `[appearance] accent`.
pub(crate) fn theme_for(config: &Config) -> Theme {
    themed(mode_of(config.appearance.mode), config)
}

/// `mode` with the configured accent (the Ctrl+Shift+T cycle and the wizard's preview keep the
/// accent while the mode changes).
pub(crate) fn themed(mode: Mode, config: &Config) -> Theme {
    Theme::for_mode(mode).with_accent_str(&config.appearance.accent)
}

/// [`adapt_layout`] plus what only the shell knows: Ctrl+B folds a docked sidebar to its icon
/// column, and the unified sidebar (whose footer carries the usage meter) gets the compact bottom
/// bar, never the per-agent one that would show the usage a second time.
fn adapt_for_shell(intent: &ResolvedLayout, width: f32, collapsed: bool) -> AdaptedLayout {
    let mut new = adapt_layout(intent, width);
    if collapsed && new.sidebar == SidebarMode::Docked {
        new.layout.sidebar.width_px = COLLAPSED_SIDEBAR_W;
        new.sidebar = SidebarMode::Collapsed;
    }
    if new.sidebar == SidebarMode::Docked
        && new.layout.sidebar.nested_tabs
        && new.layout.sidebar.footer_usage
        && new.layout.bottom_bar == BottomBar::Full
    {
        new.layout.bottom_bar = BottomBar::Compact;
    }
    new
}

/// The sidebar footer shows the usage meter: the bottom bar leaves its usage pill out.
fn dedupes_usage(eff: &AdaptedLayout) -> bool {
    eff.sidebar == SidebarMode::Docked
        && eff.layout.sidebar.nested_tabs
        && eff.layout.sidebar.footer_usage
}

/// A cached child view of `entity` filling its parent (the chrome views repaint only when they
/// are notified, not every time the shell renders).
fn cached<V: Render>(entity: &Entity<V>) -> AnyView {
    AnyView::from(entity.clone()).cached(StyleRefinement::default().size_full())
}

impl AppShell {
    /// A window with its first tab(s): the restored session, or one terminal.
    pub fn new(
        theme: Theme,
        boot: ConfigBoot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (mut shell, start) = Self::build(theme, boot, window, cx);
        let Startup {
            restore,
            start_dir,
            cwd0,
        } = start;
        let restore_has_chat = restore
            .as_ref()
            .is_some_and(|s| s.tabs.iter().any(|t| t.kind == TabKind::Chat));
        match restore {
            // Chat tabs need the hub, which opens `chat.db` off the UI thread: the session is
            // restored when it is ready (`on_chat_host_event`).
            Some(state) if restore_has_chat => {
                shell
                    .session
                    .update(cx, |s, _| s.defer_restore(state, start_dir));
                shell.chat.update(cx, |c, cx| c.ensure(cx));
            }
            Some(state) => {
                shell.restore_session(state, window, cx);
                if let Some(dir) = start_dir {
                    shell.open_tab(Some(dir), window, cx);
                }
            }
            None => shell.open_tab(Some(start_dir.unwrap_or(cwd0)), window, cx),
        }
        shell.finish_startup(window, cx);
        shell
    }

    /// Everything except the first tab (tests add their own).
    pub(crate) fn build(
        theme: Theme,
        boot: ConfigBoot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Self, Startup) {
        let ConfigBoot {
            config,
            watcher,
            session,
            start_dir,
        } = boot;
        let cwd0 = std::env::current_dir().unwrap_or_default();
        let layout = config.layout_for(start_dir.as_deref().unwrap_or(&cwd0));
        ui_kit::set_reduced_motion(layout.reduced_motion);
        let width = f32::from(window.viewport_size().width);
        let eff = adapt_for_shell(&layout, width, false);

        let usage = cx.new(UsageMonitor::new);
        let status = cx.new(|cx| {
            let mut bar = StatusBar::new(theme, eff.layout.bottom_bar, usage.clone(), cx);
            bar.set_dedupe_usage(dedupes_usage(&eff), cx);
            bar
        });
        let focused = window.is_window_active();
        status.update(cx, |s, cx| s.set_active(focused, cx));
        // A window that opens already focused gets no activation event: tell the monitor now.
        usage.update(cx, |u, cx| u.set_active(focused, cx));
        if let Some(services) = cx.try_global::<Services>().cloned() {
            let weak = cx.weak_entity();
            status.update(cx, |s, cx| {
                s.set_procs(
                    services.procs.clone(),
                    move |_, cx| {
                        weak.update(cx, |shell, cx| {
                            shell.toggle_right_panel(PanelId::new(ui_procs::PANEL_ID), cx)
                        })
                        .ok();
                    },
                    cx,
                );
            });
            let weak = cx.weak_entity();
            status.update(cx, |s, cx| {
                s.set_on_usage(
                    move |_, cx| {
                        weak.update(cx, |shell, cx| {
                            shell.toggle_right_panel(PanelId::new(ui_usage::PANEL_ID), cx)
                        })
                        .ok();
                    },
                    cx,
                );
            });
            let mut paths = vec![];
            if let Ok(p) = store::paths::state_db_path() {
                paths.push(("state.db".to_string(), p));
            }
            services.doctor.update(cx, |d, _| d.set_db_paths(paths));
        }

        let (watcher, rx) = match watcher {
            Some((w, rx)) => (Some(w), Some(rx)),
            None => (None, None),
        };
        let config_task = rx.map(|mut rx| {
            cx.spawn(async move |this, cx| {
                while let Some(event) = rx.next().await {
                    if this
                        .update(cx, |shell, cx| shell.on_config_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });

        let mut subs = vec![
            // Usage is rescanned every 60 s only while the window is focused.
            cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                notify::set_window_active(active);
                this.usage.update(cx, |u, cx| u.set_active(active, cx));
                this.status.update(cx, |s, cx| s.set_active(active, cx));
                // The tree poll ends by itself when the window loses focus: restart it here.
                this.sync_tree_poll(cx);
            }),
            // The window was resized: re-adapt when the width class changed (never from render).
            cx.observe_window_bounds(window, |this, window, cx| {
                this.on_resize(f32::from(window.viewport_size().width), cx);
            }),
        ];
        notify::set_window_active(window.is_window_active());
        // New tabs read the completion settings from `[terminal.completion]`.
        cx.set_global(TermEnv(config.completion_env()));
        // A click on a long-command toast brings the window and its tab forward.
        let mut clicks = notify::click_receiver();
        let toast_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(tab) = clicks.next().await {
                let done = this.update_in(cx, |shell, window, cx| {
                    shell.focus_tab_from_toast(tab, window, cx)
                });
                if done.is_err() {
                    break;
                }
            }
        });
        // Closing the window: write the session now (and wait for the writer thread).
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |this, cx| this.save_session_now(true, cx))
                .ok();
            true
        });
        let SessionBoot {
            db: session_db,
            restore,
        } = session;
        // The command palette (Ctrl+K): host actions plus one per panel that can live on the right.
        let mut actions = palette::base_actions();
        if let Some(reg) = cx.try_global::<PanelRegistry>() {
            for p in reg.for_zone(PanelZone::Right) {
                actions.push(
                    ui_palette::ActionEntry::new(
                        format!("{}{}", action_id::PANEL_PREFIX, p.id().as_str()),
                        format!("Panel: {}", p.title()),
                    )
                    .icon(p.icon()),
                );
            }
        }
        let custom: Vec<String> = config
            .layout
            .custom_presets
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let sources = ui_palette::standard_sources(session_db.clone(), actions, custom);
        let palette = ui_palette::command_palette(theme, session_db.clone(), sources, cx);
        subs.push(cx.subscribe_in(
            &palette,
            window,
            |this, _, ev: &ui_palette::PaletteEvent, window, cx| {
                this.on_palette(ev.clone(), window, cx);
            },
        ));

        // The entities the shell composes, and the events it listens to.
        let chat = cx.new(|_| ChatHost::new(theme));
        subs.push(
            cx.subscribe_in(&chat, window, |this, _, ev: &ChatHostEvent, window, cx| {
                this.on_chat_host_event(ev, window, cx)
            }),
        );
        let session_entity = cx.new(|_| SessionPersistence::new(session_db));
        subs.push(
            cx.subscribe(&session_entity, |this, _, _: &SessionEvent, cx| {
                this.save_session_now(false, cx)
            }),
        );
        let sidebar = |cx: &mut Context<Self>| {
            let model = crate::sidebar_view::SidebarModel::blank(theme, layout.clone());
            cx.new(|cx| SidebarView::new(model, usage.clone(), cx))
        };
        let (left, right, drawer, zen) = (sidebar(cx), sidebar(cx), sidebar(cx), sidebar(cx));
        let titlebar =
            cx.new(|_| TitleBarView::new(crate::titlebar_view::TitleBarModel::blank(theme)));
        for view in [&left, &right, &drawer, &zen] {
            subs.push(
                cx.subscribe_in(view, window, |this, _, ev: &SidebarEvent, window, cx| {
                    this.on_sidebar_event(ev.clone(), window, cx)
                }),
            );
        }
        subs.push(cx.subscribe_in(
            &titlebar,
            window,
            |this, _, ev: &TitleBarEvent, window, cx| {
                this.on_titlebar_event(ev.clone(), window, cx)
            },
        ));

        let shell = Self {
            theme,
            config,
            layout: layout.clone(),
            eff,
            host: Host {
                window: window.window_handle(),
                width,
                blurred: false,
                sync_pending: false,
                main_class: WidthClass::Regular,
            },
            drawer: DrawerState {
                open: None,
                right: PanelId::new(store::config::panel_ids::GIT),
                epoch: 0,
                reveal: Reveal::default(),
            },
            anim: Anim::default(),
            tabs: Vec::new(),
            splits: SplitState {
                panes: PaneSet::new(pane_count(layout.splits)),
                ratio_x: 0.5,
                ratio_y: 0.5,
                bounds: Rc::new(Cell::new(Bounds::default())),
                drag: None,
                drag_moved: false,
                right_w: None,
            },
            status,
            usage,
            palette,
            right: None,
            grammars: Vec::new(),
            marks_sub: None,
            closed_owners: Vec::new(),
            layout_panel: None,
            cfg_save: ConfigSave::default(),
            spaces: SpaceData::default(),
            sidebar_collapsed: false,
            modes: Modes::default(),
            prompts: Prompts {
                confirm_close: None,
                restore: None,
                new_tab_menu: false,
                menu_focus: cx.focus_handle(),
                hint: None,
                hint_task: None,
            },
            chat,
            session: session_entity,
            chrome: Chrome {
                left,
                right,
                drawer,
                zen,
                titlebar,
                right_focus: cx.focus_handle().tab_index(0).tab_stop(false),
            },
            tasks: Tasks {
                _toast: Some(toast_task),
                _config: config_task,
                _watcher: watcher,
                ..Tasks::default()
            },
            _subs: subs,
        };
        (
            shell,
            Startup {
                restore,
                start_dir,
                cwd0,
            },
        )
    }

    /// What follows the first tab: panels, known projects, the wizard and the demo.
    fn finish_startup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mount_panels(window, cx);
        self.load_known_projects(cx);
        let db = self.session.read(cx).db().cloned();
        if let Some(db) = db
            && ui_onboarding::needs_onboarding(&db)
        {
            self.start_onboarding(window, cx);
        }
        self.run_demo(window, cx);
        self.refresh(cx);
    }

    /// The shell's one way to say "something changed": pushes the chrome models (the views repaint
    /// only when theirs differ), keeps the status bar and the tree poll current, schedules the
    /// window-level sync when needed and repaints the shell itself.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.sync_chrome(cx);
        self.sync_tree_poll(cx);
        self.queue_window_sync(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
