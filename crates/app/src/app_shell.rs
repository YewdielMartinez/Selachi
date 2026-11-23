//! The declarative shell: renders the zones (rail, sidebar, tabs, main splits, right panel, bottom
//! bar) from a [`ResolvedLayout`] (docs/PLAN.md section 17).
//!
//! * The layout is `Config::layout_for(active tab cwd)`. It is recomputed (and the zones animated)
//!   only when something under `[layout]` changed: a hot reload of `config.toml`
//!   ([`layout_changed`]), a change made from the layout popover, or a different per-project
//!   override for the active tab.
//! * Tabs are [`TabModel`]s listed as top pills or as sidebar rows from the same [`TabInfo`]. Each
//!   split pane hosts one tab ([`PaneSet`]).
//! * Edits made here (sidebar width) are stored as global overrides and saved, debounced; the
//!   file watcher's echo of our own save is recognised and ignored.
//! * No divider lines: zones are separated by tone only. Idle cost is zero: every update here
//!   comes from a user action, a monitor's `notify()` or a one-shot animation.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use ui_git::{GitPanelEvent, OpFinished};
use ui_review::{AttentionEvent, AttentionItem, FocusTarget, ReviewEvent, ReviewSource, ReviewTab};
use ui_viewer::{GitMark, ViewerConfig, ViewerEvent, ViewerTab};
use chat_core::ThreadId;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Bounds, Context, Div, ElementId, Entity, FocusHandle, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    ScrollHandle, SharedString, Subscription, Task, WeakEntity, Window, WindowBackgroundAppearance, actions, canvas,
    div, prelude::*, px, relative,
};
use ui_chat::{ChatHub, ChatTab};
use panel_api::{
    CloseCheck, PanelCtx, PanelId, PanelRegistry, PanelZone, TabHandle, TabId, TabInfo, WidthClass,
};
use store::{
    BottomBar, Config, ConfigEvent, ConfigWatcher, Density, LayoutConfig, RailVisibility, ResolvedLayout,
    SidebarLayout, SidebarSection, SidebarSide, Splits, TabsPosition, layout_changed,
};
use store::config::ThemeMode;
use theme::{Mode, Radius, Theme, motion::SHEET, motion::duration::BASE};
use ui_kit::{Button, Card, Icon, IconButton, ListRow, Placement, Popover, glass_tint, icon};

use crate::adapt::{
    AdaptedLayout, Breakpoint, COLLAPSED_SIDEBAR_W, RIGHT_MAX_W, RIGHT_MIN_W, SidebarMode, adapt_layout,
    right_panel_width,
};
use crate::git_monitor::{GitInfo, GitMonitor};
use crate::hint::{Hint, HintAction, hint_card};
use crate::layout_host::{LayoutPanelArgs, build_layout_panel};
use crate::notify;
use crate::palette::{self, id as action_id};
use crate::paths;
use crate::rail::{RailItem, rail};
use crate::session::{SavedTab, SessionState, TabKind};
use crate::settings_tab::SettingsTab;
use crate::sidebar::{SidebarProps, sidebar_collapsed, sidebar_content};
use crate::spaces;
use crate::unified_sidebar::{UnifiedProps, Usage, unified_collapsed, unified_content};
use crate::splits::{PaneSet, clamp_ratio, pane_count, ratio_at};
use crate::status_bar::{StatusBar, bar_height};
use crate::terminal_tab::{TermRequest, TerminalTab};
use crate::services::Services;
use crate::terminal_view::TermEnv;
use crate::titlebar::{TITLEBAR_H, breadcrumb, new_tab_button, tab_pill, title_row, window_controls};
use crate::usage_monitor::UsageMonitor;
use crate::viewer_host;

mod chat;

pub use crate::rail::RAIL_W;
/// Height of the titlebar (named `TAB_H` for the initial terminal size estimate).
pub use crate::titlebar::TITLEBAR_H as TAB_H;
/// Padding between the terminal element edge and the character grid.
pub const TERM_PAD: f32 = 8.0;
/// Gap between two cards, also the hit area of the resize handles.
const GUTTER: f32 = 8.0;
/// Zen: width of the sidebar that slides in from the left edge.
const ZEN_SIDEBAR_W: f32 = 260.0;
/// Hover strip at the window edge that reveals the chrome in zen mode.
const EDGE_STRIP: f32 = 12.0;
/// How long the zone transition runs (the animation itself is [`BASE`]); the transition state is
/// dropped on the first render after this.
const TRANSITION_KEEP: Duration = Duration::from_millis(260);
/// Quiet time before the layout is written to `config.toml`.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
/// The session is written at most this often, and only when it changed.
const SESSION_DEBOUNCE: Duration = Duration::from_secs(2);
/// How long an inline hint stays.
const HINT_FOR: Duration = Duration::from_secs(8);
/// Narrowest a tab pill gets: the strip scrolls instead of squeezing tabs.
const TAB_MIN_W: f32 = 96.0;
/// Width of the sidebar drawer / overlay and of the right panel overlay.
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
            Density::Comfortable => Self { margin: 8.0, pad_x: 8.0, pad_y: 5.0 },
            // Inner padding must exceed r * (1 - 1/sqrt 2) so the terminal's square background
            // never pokes out of the card's rounded corners.
            Density::Compact => Self { margin: 4.0, pad_x: 8.0, pad_y: 5.0 },
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
    started: Instant,
    epoch: u64,
}

/// What a pointer drag is resizing.
#[derive(Clone, Copy)]
enum Drag {
    Sidebar { side: SidebarSide, start_x: f32, start_w: u16 },
    Right { start_x: f32, start_w: f32 },
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

pub struct AppShell {
    theme: Theme,
    config: Arc<Config>,
    /// The layout the config asks for (the intent).
    layout: ResolvedLayout,
    /// What is docked for the current window width ([`adapt_layout`] of `layout`).
    eff: AdaptedLayout,
    /// Logical width of the window as of the last render / resize check.
    window_w: f32,
    overlay: Option<Overlay>,
    /// Panel shown by the right overlay / drawer.
    overlay_right: PanelId,
    overlay_epoch: u64,
    menu_open: bool,
    tabs_menu_open: bool,
    tab_scroll: ScrollHandle,
    scroll_active: bool,
    /// Right panel width the user dragged to (`None` = the default share of the window).
    right_w: Option<f32>,
    palette: Entity<ui_palette::CommandPalette>,
    _palette_sub: Subscription,
    _toast_task: Option<Task<()>>,
    /// Wakes the shell once after a transition so the animated zones settle into plain boxes.
    transition_task: Option<Task<()>>,
    session_db: Option<store::StateDb>,
    session_last: String,
    session_task: Option<Task<()>>,
    save_inflight: bool,
    save_dirty: bool,
    transition: Option<Transition>,
    epoch: u64,
    tabs: Vec<TabSlot>,
    panes: PaneSet<TabId>,
    ratio_x: f32,
    ratio_y: f32,
    split_bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    drag_moved: bool,
    status: Entity<StatusBar>,
    usage: Entity<UsageMonitor>,
    right: Option<RightPanel>,
    /// Inline hint above the status bar, and the one-shot timer that dismisses it.
    hint: Option<Hint>,
    hint_task: Option<Task<()>>,
    /// Grammar plugins found at startup (for the doctor).
    grammars: Vec<String>,
    /// Keeps the file tree's git markers in sync with the active tab's repository.
    marks_sub: Option<Subscription>,
    _viewer_task: Option<Task<()>>,
    /// Owners (process jobs) of tabs that were closed and still have processes.
    closed_owners: Vec<u64>,
    _grammar_task: Option<Task<()>>,
    layout_open: bool,
    layout_panel: Option<AnyView>,
    confirm_close: Option<(TabId, SharedString)>,
    reveal: Reveal,
    /// Layouts written by us whose file-watcher echo has not arrived yet.
    echoes: Vec<LayoutConfig>,
    save_task: Option<Task<()>>,
    blurred: bool,
    /// Spaces (keys of [`spaces::Space`]) folded in the unified sidebar.
    folded_spaces: HashSet<PathBuf>,
    /// Ctrl+B: the sidebar shows only icons even when there is room for it.
    sidebar_collapsed: bool,
    /// The unified-sidebar tab row / space header under the pointer (drives the close and "+"
    /// buttons: state, not group-hover, so they paint deterministically).
    hovered_row: Option<TabId>,
    hovered_space: Option<usize>,
    /// Favorite and recent projects (path, label): listed as spaces without tabs.
    known_projects: Vec<(PathBuf, String)>,
    _projects_task: Option<Task<()>>,
    /// The settings screen, created the first time it is opened.
    settings: Option<Entity<ui_settings::SettingsView>>,
    _settings_sub: Option<Subscription>,
    /// The first-run wizard (or "Volver a configurar"): replaces the shell while it is open.
    onboarding: Option<Entity<ui_onboarding::OnboardingView>>,
    _onboarding_sub: Option<Subscription>,
    /// The chat hub, created the first time chat is used (`chat_failed`: `chat.db` could not be
    /// opened, do not retry).
    chat: Option<Entity<ChatHub>>,
    chat_failed: bool,
    _chat_subs: Vec<Subscription>,
    _chat_task: Option<Task<()>>,
    /// Projects last given to the hub (it only hears about changes).
    chat_projects: Vec<PathBuf>,
    /// What each chat thread last asked of the user (a toast fires when it changes).
    chat_attn: HashMap<ThreadId, ui_review::AttentionKind>,
    /// The "Nueva pestaña" menu (Terminal / Chat) and its focus (keys `T`, `C`, Enter, Esc).
    new_tab_menu: bool,
    menu_focus: FocusHandle,
    restore_confirm: Option<RestoreConfirm>,
    /// Background refresh of tree-tracked process owners; only alive while it is useful.
    tree_poll: Option<Task<()>>,
    _watcher: Option<ConfigWatcher>,
    _config_task: Option<Task<()>>,
    /// Observers and subscriptions live as long as the shell; dropping it (closing the window)
    /// drops the tabs and monitors, which cancels their tasks and watchers.
    _subs: Vec<Subscription>,
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
    (f32::from(start_w) + delta)
        .round()
        .clamp(f32::from(store::config::SIDEBAR_MIN_WIDTH), f32::from(store::config::SIDEBAR_MAX_WIDTH)) as u16
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
    if new.sidebar == SidebarMode::Docked && new.layout.sidebar.nested_tabs && new.layout.sidebar.footer_usage && new.layout.bottom_bar == BottomBar::Full {
        new.layout.bottom_bar = BottomBar::Compact;
    }
    new
}

/// The sidebar footer shows the usage meter: the bottom bar leaves its usage pill out.
fn dedupes_usage(eff: &AdaptedLayout) -> bool {
    eff.sidebar == SidebarMode::Docked && eff.layout.sidebar.nested_tabs && eff.layout.sidebar.footer_usage
}

impl AppShell {
    pub fn new(theme: Theme, boot: ConfigBoot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ConfigBoot { config, watcher, session } = boot;
        let cwd0 = std::env::current_dir().unwrap_or_default();
        let layout = config.layout_for(&cwd0);
        ui_kit::set_reduced_motion(layout.reduced_motion);
        let eff = adapt_for_shell(&layout, f32::from(window.viewport_size().width), false);

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
                        weak.update(cx, |shell, cx| shell.toggle_right_panel(PanelId::new(ui_procs::PANEL_ID), cx)).ok();
                    },
                    cx,
                );
            });
            let weak = cx.weak_entity();
            status.update(cx, |s, cx| {
                s.set_on_usage(
                    move |_, cx| {
                        weak.update(cx, |shell, cx| shell.toggle_right_panel(PanelId::new(ui_usage::PANEL_ID), cx)).ok();
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
                    if this.update(cx, |shell, cx| shell.on_config_event(event, cx)).is_err() {
                        break;
                    }
                }
            })
        });

        let subs = vec![
            // Usage is rescanned every 60 s only while the window is focused.
            cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                notify::set_window_active(active);
                this.usage.update(cx, |u, cx| u.set_active(active, cx));
                this.status.update(cx, |s, cx| s.set_active(active, cx));
            }),
        ];
        notify::set_window_active(window.is_window_active());
        // New tabs read the completion settings from `[terminal.completion]`.
        cx.set_global(TermEnv(config.completion_env()));
        // A click on a long-command toast brings the window and its tab forward.
        let mut clicks = notify::click_receiver();
        let toast_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(tab) = clicks.next().await {
                let done = this.update_in(cx, |shell, window, cx| shell.focus_tab_from_toast(tab, window, cx));
                if done.is_err() {
                    break;
                }
            }
        });
        // Closing the window: write the session now (and wait for the writer thread).
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |this, cx| this.save_session_now(true, cx)).ok();
            true
        });
        let SessionBoot { db: session_db, restore } = session;
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
        let custom: Vec<String> = config.layout.custom_presets.iter().map(|c| c.name.clone()).collect();
        let sources = ui_palette::standard_sources(session_db.clone(), actions, custom);
        let palette = ui_palette::command_palette(theme, session_db.clone(), sources, cx);
        let palette_sub = cx.subscribe_in(&palette, window, |this, _, ev: &ui_palette::PaletteEvent, window, cx| {
            this.on_palette(ev.clone(), window, cx);
        });

        let mut shell = Self {
            _palette_sub: palette_sub,
            theme,
            config,
            panes: PaneSet::new(pane_count(layout.splits)),
            layout,
            eff,
            window_w: f32::from(window.viewport_size().width),
            overlay: None,
            overlay_right: PanelId::new(store::config::panel_ids::GIT),
            overlay_epoch: 0,
            menu_open: false,
            tabs_menu_open: false,
            tab_scroll: ScrollHandle::new(),
            scroll_active: false,
            right_w: None,
            palette,
            _toast_task: Some(toast_task),
            transition_task: None,
            session_db,
            session_last: String::new(),
            session_task: None,
            save_inflight: false,
            save_dirty: false,
            transition: None,
            epoch: 0,
            tabs: Vec::new(),
            ratio_x: 0.5,
            ratio_y: 0.5,
            split_bounds: Rc::new(Cell::new(Bounds::default())),
            drag: None,
            drag_moved: false,
            status,
            usage,
            right: None,
            hint: None,
            hint_task: None,
            grammars: Vec::new(),
            marks_sub: None,
            _viewer_task: None,
            closed_owners: Vec::new(),
            _grammar_task: None,
            layout_open: false,
            layout_panel: None,
            confirm_close: None,
            reveal: Reveal::default(),
            echoes: Vec::new(),
            save_task: None,
            blurred: false,
            folded_spaces: HashSet::new(),
            sidebar_collapsed: false,
            hovered_row: None,
            hovered_space: None,
            known_projects: Vec::new(),
            _projects_task: None,
            settings: None,
            _settings_sub: None,
            onboarding: None,
            _onboarding_sub: None,
            chat: None,
            chat_failed: false,
            _chat_subs: Vec::new(),
            _chat_task: None,
            chat_projects: Vec::new(),
            chat_attn: HashMap::new(),
            new_tab_menu: false,
            menu_focus: cx.focus_handle(),
            restore_confirm: None,
            tree_poll: None,
            _watcher: watcher,
            _config_task: config_task,
            _subs: subs,
        };
        match restore {
            Some(state) => shell.restore_session(state, window, cx),
            None => shell.open_tab(Some(cwd0), window, cx),
        }
        shell.mount_panels(window, cx);
        shell.load_known_projects(cx);
        if let Some(db) = shell.session_db.clone()
            && ui_onboarding::needs_onboarding(&db)
        {
            shell.start_onboarding(window, cx);
        }
        shell.run_demo(window, cx);
        shell
    }

    /// Wires the git panel, the file tree and the grammar plugins to the shell. Nothing here reads
    /// a repository or a folder: the panels do that when they are first shown.
    fn mount_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(services) = cx.try_global::<Services>().cloned() else { return };
        // Closed tabs keep their job until it is empty (orphans); the process model notifies on job
        // events, which is when to check.
        self._subs.push(cx.observe(&services.procs, |this, _, cx| this.prune_closed_owners(cx)));
        // Git panel: its events and the end of its operations.
        self._subs.push(cx.subscribe_in(&services.git, window, |this, _, ev: &GitPanelEvent, window, cx| {
            this.on_git_event(ev.clone(), window, cx);
        }));
        self._subs.push(cx.subscribe_in(&services.git, window, |this, _, _: &OpFinished, _, cx| {
            this.after_git_operation(cx);
        }));
        // Attention queue: a click on an item brings its tab forward.
        self._subs.push(cx.subscribe_in(&services.attention, window, |this, _, ev: &AttentionEvent, window, cx| match ev {
            AttentionEvent::Jump(target) => this.focus_attention_target(target, window, cx),
            AttentionEvent::Dismiss(_) | AttentionEvent::Changed { .. } => cx.notify(),
        }));
        // Status bar badge: shows the count, opens the attention panel.
        let weak = cx.weak_entity();
        self.status.update(cx, |s, cx| {
            s.set_attention(
                services.attention.clone(),
                move |_, cx| {
                    weak.update(cx, |shell, cx| {
                        shell.toggle_right_panel(PanelId::new(ui_review::ATTENTION_PANEL_ID), cx);
                    })
                    .ok();
                },
                cx,
            );
        });
        // File tree: events come through a channel because the provider is built before the shell.
        if let Some(mut rx) = services.viewer_events.borrow_mut().take() {
            self._viewer_task = Some(cx.spawn_in(window, async move |this, cx| {
                while let Some(ev) = rx.next().await {
                    if this.update_in(cx, |shell, window, cx| shell.on_viewer_event(ev, window, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        // Grammar plugins: scanned off the UI thread; documents opened before it ends are plain.
        self._grammar_task = Some(cx.spawn(async move |this, cx| {
            let ids = cx.background_executor().spawn(async { viewer_host::init_grammars() }).await;
            this.update(cx, |shell, cx| {
                shell.grammars = ids;
                shell.update_doctor_stats(cx);
            })
            .ok();
        }));
        self.sync_git_marks(cx);
    }

    // ------------------------------------------------------------------------------------
    // Session (A5)
    // ------------------------------------------------------------------------------------

    /// Reopens the saved tabs in their directories. Shells start the first time their tab is
    /// painted, so tabs that are never shown cost no process (`TerminalView::new_deferred`).
    /// `SELACHI_EAGER_RESTORE=1` spawns them all at once (to measure the difference).
    fn restore_session(&mut self, state: SessionState, window: &mut Window, cx: &mut Context<Self>) {
        let deferred = std::env::var_os("SELACHI_EAGER_RESTORE").is_none();
        // One entry per saved tab; a chat tab that cannot be reopened leaves a hole.
        let mut ids: Vec<Option<TabId>> = Vec::new();
        for tab in &state.tabs {
            ids.push(match tab.kind {
                TabKind::Terminal => Some(self.add_tab(tab.cwd.clone(), deferred, window, cx)),
                TabKind::Chat => tab.thread.and_then(|t| self.restore_chat_tab(ThreadId(t), window, cx)),
            });
        }
        if ids.iter().all(Option::is_none) {
            // Nothing could be reopened (a session of chats and the chat database is gone).
            let cwd = std::env::current_dir().ok();
            self.open_tab(cwd, window, cx);
            return;
        }
        let panes: Vec<Option<TabId>> = (0..self.panes.len())
            .map(|i| state.panes.get(i).copied().flatten().and_then(|t| ids.get(t).copied().flatten()))
            .collect();
        self.panes = PaneSet::from_panes(panes, state.focused);
        if self.panes.focused_tab().is_none()
            && let Some(first) = ids.iter().flatten().next().copied()
        {
            self.panes.show(first);
        }
        self.ratio_x = clamp_ratio(state.ratio_x);
        self.ratio_y = clamp_ratio(state.ratio_y);
        self.right_w = state.right_w.map(|w| w.clamp(RIGHT_MIN_W, RIGHT_MAX_W));
        if state.sidebar_collapsed != self.sidebar_collapsed {
            self.sidebar_collapsed = state.sidebar_collapsed;
            self.apply_adapted(false, cx);
        }
        self.session_last = state.to_json();
        self.after_active_changed(window, cx);
    }

    /// The tabs worth saving, with their ids: terminals and chat threads that exist (a chat draft
    /// has no thread yet). Viewer, review and settings tabs are files or views that reopen in a click.
    fn saved_tabs(&self, cx: &App) -> Vec<(TabId, SavedTab)> {
        self.tabs
            .iter()
            .filter_map(|t| {
                let info = t.handle.info(cx);
                if t.handle.downcast::<TerminalTab>().is_some() {
                    return Some((t.handle.id(), SavedTab::terminal(info.cwd)));
                }
                let thread = t.handle.downcast::<ChatTab>()?.read(cx).thread_id()?;
                Some((t.handle.id(), SavedTab::chat(thread.0, info.cwd)))
            })
            .collect()
    }

    /// The window's shape as a session.
    fn capture_session(&self, cx: &App) -> SessionState {
        let saved = self.saved_tabs(cx);
        let ids: Vec<TabId> = saved.iter().map(|(id, _)| *id).collect();
        SessionState {
            v: crate::session::VERSION,
            tabs: saved.into_iter().map(|(_, t)| t).collect(),
            panes: self.panes.panes().iter().map(|p| p.and_then(|id| ids.iter().position(|i| *i == id))).collect(),
            focused: self.panes.focused(),
            ratio_x: self.ratio_x,
            ratio_y: self.ratio_y,
            sidebar_w: Some(self.layout.sidebar.width_px),
            right_w: self.right_w,
            sidebar_collapsed: self.sidebar_collapsed,
        }
    }

    /// Something that belongs to the session changed: write it within 2 s (once, if it changed).
    fn mark_session_dirty(&mut self, cx: &mut Context<Self>) {
        if self.session_db.is_none() || self.session_task.is_some() {
            return;
        }
        self.session_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SESSION_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.session_task = None;
                this.save_session_now(false, cx);
            })
            .ok();
        }));
    }

    /// Writes the session if it differs from the last one written; `flush` waits for the writer.
    fn save_session_now(&mut self, flush: bool, cx: &mut Context<Self>) {
        let Some(db) = &self.session_db else { return };
        if self.saved_tabs(cx).is_empty() {
            return;
        }
        let json = self.capture_session(cx).to_json();
        if json != self.session_last {
            if let Err(e) = db.save_session_layout(&json) {
                log::warn!("could not save the session: {e}");
            }
            self.session_last = json;
        }
        if flush && let Err(e) = db.flush() {
            log::warn!("could not flush the session: {e}");
        }
    }

    // ------------------------------------------------------------------------------------
    // Tabs
    // ------------------------------------------------------------------------------------

    fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.iter().map(|t| t.handle.id()).collect()
    }

    fn infos(&self, cx: &App) -> Vec<TabInfo> {
        self.tabs.iter().map(|t| t.handle.info(cx)).collect()
    }

    fn slot(&self, id: TabId) -> Option<&TabSlot> {
        self.tabs.iter().find(|t| t.handle.id() == id)
    }

    fn active_id(&self) -> Option<TabId> {
        self.panes.focused_tab()
    }

    fn active_info(&self, cx: &App) -> Option<TabInfo> {
        self.active_id().and_then(|id| self.slot(id)).map(|s| s.handle.info(cx))
    }

    fn active_cwd(&self, cx: &App) -> Option<PathBuf> {
        self.active_info(cx).and_then(|i| i.cwd)
    }

    /// Creates a terminal tab (not shown yet). `deferred`: its shell starts when first painted.
    fn add_tab(&mut self, cwd: Option<PathBuf>, deferred: bool, window: &mut Window, cx: &mut Context<Self>) -> TabId {
        let theme = self.theme;
        let entity = cx.new(|cx| {
            if deferred {
                TerminalTab::new_deferred(theme, cwd, window, cx)
            } else {
                TerminalTab::new(theme, cwd, window, cx)
            }
        });
        let handle = TabHandle::new(entity.clone());
        let id = handle.id();
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        // Ctrl+click on a path and "Arreglar con agente" come as requests of the terminal.
        let requests = cx.subscribe_in(&entity, window, move |this, _, req: &TermRequest, window, cx| {
            this.on_term_request(id, req.clone(), window, cx);
        });
        self.tabs.push(TabSlot { handle, _subs: vec![sub, requests] });
        self.update_doctor_stats(cx);
        id
    }

    /// Drops the tracker owner of every closed tab whose job has no processes left.
    fn prune_closed_owners(&mut self, cx: &mut Context<Self>) {
        if self.closed_owners.is_empty() {
            return;
        }
        let Some(services) = cx.try_global::<Services>().cloned() else { return };
        let mut tracker = services.tracker.lock().unwrap_or_else(|p| p.into_inner());
        self.closed_owners.retain(|owner| {
            let members = tracker.job(*owner).map(|j| j.members());
            log::debug!("closed owner {owner}: members {members:?}");
            let empty = members.is_none_or(|m| m.is_empty());
            if empty {
                drop(tracker.remove_owner(*owner));
            }
            !empty
        });
    }

    /// Terminals among the tabs (viewer tabs do not run a shell).
    fn terminal_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.handle.downcast::<TerminalTab>().is_some()).count()
    }

    /// Tells the doctor panel how many shells run and which grammars are available.
    fn update_doctor_stats(&mut self, cx: &mut Context<Self>) {
        let Some(services) = cx.try_global::<Services>().cloned() else { return };
        let shells = self.terminal_count();
        let tree_tracked = {
            let tracker = services.tracker.lock().unwrap_or_else(|p| p.into_inner());
            self.tabs
                .iter()
                .filter(|t| t.handle.downcast::<TerminalTab>().is_some())
                .filter(|t| tracker.attachment_hint(t.handle.id().as_u64()) == proc_svc::TrackingMode::TreeOnly)
                .count()
        };
        let stats = ui_doctor::AppStats { shells: Some(shells), agents: None, grammars: self.grammars.clone(), tree_tracked };
        services.doctor.update(cx, |d, cx| d.set_app_stats(stats, cx));
    }

    /// Starts a terminal in `cwd` and shows it (first empty pane, else the focused pane).
    fn open_tab(&mut self, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.add_tab(cwd, false, window, cx);
        self.panes.open(id);
        self.scroll_active = true;
        self.after_active_changed(window, cx);
    }

    /// A long-command toast was clicked: show the window and the tab that ran the command.
    fn focus_tab_from_toast(&mut self, tab: u64, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        if let Some(id) = self.tab_ids().into_iter().find(|id| id.as_u64() == tab) {
            self.select_tab(id, window, cx);
        }
    }

    /// "+": a new terminal in the active tab's directory.
    pub fn new_tab_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        self.open_tab(cwd, window, cx);
    }

    /// A tab's title, subtitle or status changed: repaint, and re-resolve the per-project layout
    /// (the active tab may have moved to another project).
    fn on_tab_event(&mut self, cx: &mut Context<Self>) {
        // A deferred shell registers its process owner when it starts: refresh the doctor's
        // "tracked by tree" count (equality-guarded, so nothing repaints when it is the same).
        self.update_doctor_stats(cx);
        self.recompute_layout(true, cx);
        self.sync_chat_active(cx);
        self.mark_session_dirty(cx);
        cx.notify();
    }

    pub fn select_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.slot(id).is_none() {
            return;
        }
        self.panes.show_within(id, pane_count(self.eff.layout.splits));
        self.scroll_active = true;
        self.tabs_menu_open = false;
        self.after_active_changed(window, cx);
    }

    fn focus_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.focused() != pane {
            self.panes.focus_pane(pane);
            self.after_active_changed(window, cx);
        }
    }

    /// The focused tab changed: point the status bar at its git monitor, re-resolve the layout
    /// and give its content the keyboard focus.
    fn after_active_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let git: Option<Entity<GitMonitor>> = self
            .active_id()
            .and_then(|id| self.slot(id))
            .and_then(|s| s.handle.downcast::<TerminalTab>())
            .map(|t| t.read(cx).git().clone());
        self.status.update(cx, |s, cx| s.set_git(git, cx));
        self.sync_git_marks(cx);
        self.clear_attention_of_active(cx);
        self.sync_chat_projects(cx);
        self.sync_chat_active(cx);
        self.recompute_layout(true, cx);
        self.mark_session_dirty(cx);
        self.sync_layout_panel_cwd(cx);
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Ctrl+W / the close buttons: asks first when the tab says so.
    pub fn request_close(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = self.slot(id) else { return };
        match slot.handle.can_close(cx) {
            CloseCheck::Allow => self.close_tab(id, window, cx),
            // [general] confirm_close_running = false: close without asking.
            CloseCheck::Confirm(_) if !self.config.general.confirm_close_running => self.close_tab(id, window, cx),
            CloseCheck::Confirm(why) => {
                self.confirm_close = Some((id, why));
                cx.notify();
            },
            CloseCheck::Deny => {},
        }
    }

    /// Closes the tab now: the model releases its resources (PTY session, watchers, tasks) and the
    /// handle is dropped. The last tab closes the window.
    fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|t| t.handle.id() == id) else { return };
        let slot = self.tabs.remove(ix);
        if let Some(thread) = slot.handle.downcast::<ChatTab>().and_then(|t| t.read(cx).thread_id()) {
            self.chat_attn.remove(&thread);
        }
        if let Some(services) = cx.try_global::<Services>().cloned() {
            let target = FocusTarget::new(id.as_u64().to_string());
            services.attention.update(cx, |m, cx| m.remove_target(&target, cx));
        }
        // The owner stays in the tracker: closing the tab ends the shell (its root), so whatever
        // the shell started and left running becomes an orphan of that owner and shows up in the
        // procs panel and the status bar. It is dropped once the job is empty.
        self.closed_owners.push(id.as_u64());
        self.prune_closed_owners(cx);
        self.update_doctor_stats(cx);
        slot.handle.close(cx);
        drop(slot);
        self.confirm_close = None;
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        let remaining = self.tab_ids();
        self.panes.remove(id, &remaining, ix);
        // The focused pane must show something.
        if self.panes.focused_tab().is_none() {
            let next = remaining.get(ix).or_else(|| remaining.last()).copied();
            if let Some(t) = next {
                self.panes.show(t);
            }
        }
        self.after_active_changed(window, cx);
    }

    fn cycle_tab(&mut self, dir: isize, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.tab_ids();
        let cur = self.active_id().and_then(|a| ids.iter().position(|i| *i == a)).unwrap_or(0);
        if let Some(id) = ids.get(cycle_index(ids.len(), cur, dir)).copied() {
            self.select_tab(id, window, cx);
        }
    }

    fn jump_tab(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tab_ids().get(n).copied() {
            self.select_tab(id, window, cx);
        }
    }

    // ------------------------------------------------------------------------------------
    // Layout and configuration
    // ------------------------------------------------------------------------------------

    /// Re-resolves the layout for the active tab's directory. Animates the zones when it changed.
    fn recompute_layout(&mut self, animate: bool, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
        let new = self.config.layout_for(&cwd);
        if new == self.layout {
            return;
        }
        self.layout = new;
        self.apply_adapted(animate, cx);
        cx.notify();
    }

    /// Adapts the intent to the window width. Crossing a breakpoint (or a layout change) runs one
    /// 200 ms zone transition; resizing inside a class changes nothing here, so no per-pixel work.
    fn apply_adapted(&mut self, animate: bool, cx: &mut Context<Self>) {
        let new = adapt_for_shell(&self.layout, self.window_w, self.sidebar_collapsed);
        if new == self.eff {
            return;
        }
        let old = std::mem::replace(&mut self.eff, new);
        if old.breakpoint != self.eff.breakpoint {
            self.overlay = None;
            self.menu_open = false;
            self.tabs_menu_open = false;
        }
        ui_kit::set_reduced_motion(self.layout.reduced_motion);
        if animate && !self.layout.reduced_motion {
            self.epoch += 1;
            self.transition = Some(Transition { from: old.layout, started: Instant::now(), epoch: self.epoch });
            // Nothing else repaints when the animation ends: one wake-up drops the transition
            // state (and the oversized, clipped zone contents) right after it.
            self.transition_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TRANSITION_KEEP + Duration::from_millis(40)).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            }));
        } else {
            self.transition = None;
        }
        let all = self.tab_ids();
        self.panes.resize(pane_count(self.layout.splits), &all);
        self.panes.clamp_focus(pane_count(self.eff.layout.splits));
        let mode = self.eff.layout.bottom_bar;
        let dedupe = dedupes_usage(&self.eff);
        self.status.update(cx, |s, cx| {
            s.set_mode(mode, cx);
            s.set_dedupe_usage(dedupe, cx);
        });
    }

    /// The window was resized (checked on render): re-adapt when the width class changed.
    fn on_resize(&mut self, width: f32, cx: &mut Context<Self>) {
        if (width - self.window_w).abs() < 0.5 {
            return;
        }
        self.window_w = width;
        self.apply_adapted(true, cx);
    }

    fn on_config_event(&mut self, event: ConfigEvent, cx: &mut Context<Self>) {
        match event {
            ConfigEvent::Reloaded { config, warnings } => {
                for w in warnings {
                    log::warn!("config.toml: {w}");
                }
                // Our own save coming back: nothing to do.
                if let Some(i) = self.echoes.iter().position(|l| *l == config.layout) {
                    self.echoes.remove(i);
                    return;
                }
                self.adopt_config(config.clone(), cx);
                if let Some(settings) = self.settings.clone() {
                    settings.update(cx, |s, cx| s.set_config((*config).clone(), cx));
                }
            },
            ConfigEvent::Error(msg) => log::warn!("config.toml is invalid, keeping the last good one: {msg}"),
        }
    }

    /// Runs `f` on the open layout panel (hot reload, theme and cwd changes keep it in sync).
    fn sync_layout_panel(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ui_layout::LayoutPanel, &Config, Theme, &mut Context<ui_layout::LayoutPanel>),
    ) {
        let Some(view) = self.layout_panel.clone() else { return };
        let Ok(panel) = view.downcast::<ui_layout::LayoutPanel>() else { return };
        let (config, theme) = (self.config.clone(), self.theme);
        panel.update(cx, |p, cx| f(p, &config, theme, cx));
    }

    /// The active tab changed directory: the panel's "this project" scope follows.
    fn sync_layout_panel_cwd(&mut self, cx: &mut Context<Self>) {
        if self.layout_panel.is_none() {
            return;
        }
        let cwd = self.active_cwd(cx);
        self.sync_layout_panel(cx, move |p, _, _, cx| p.set_cwd(cwd, cx));
    }

    /// Applies a layout coming from the popover (or any editor): live, then saved.
    pub fn set_layout_config(&mut self, layout: LayoutConfig, animate: bool, cx: &mut Context<Self>) {
        if self.config.layout == layout {
            return;
        }
        let mut cfg = (*self.config).clone();
        cfg.set_layout(layout);
        self.config = Arc::new(cfg);
        self.schedule_save(cx);
        self.recompute_layout(animate, cx);
        self.mark_session_dirty(cx);
        if let Some(settings) = self.settings.clone() {
            let cfg = (*self.config).clone();
            settings.update(cx, |s, cx| s.set_config(cfg, cx));
        }
    }

    /// Edits the global layout (the preset plus global overrides) with `f` and stores only the
    /// difference from the preset.
    fn tweak_global(&mut self, animate: bool, cx: &mut Context<Self>, f: impl FnOnce(&mut ResolvedLayout)) {
        let mut layout = self.config.layout.clone();
        let mut target = self.config.global_layout();
        f(&mut target);
        layout.set_resolved(&target);
        self.set_layout_config(layout, animate, cx);
    }

    /// Saves `config.toml` 400 ms after the last change, one write at a time: the sliders of the
    /// layout panel emit on every move.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.echoes.push(self.config.layout.clone());
        if self.echoes.len() > 8 {
            self.echoes.remove(0);
        }
        self.save_dirty = true;
        // Replacing the task restarts the debounce.
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |s, cx| {
                s.save_task = None;
                s.run_save(cx);
            })
            .ok();
        }));
    }

    fn run_save(&mut self, cx: &mut Context<Self>) {
        // A write is running: it re-arms the save when it finishes (the config moved on).
        if self.save_inflight || !self.save_dirty {
            return;
        }
        self.save_inflight = true;
        self.save_dirty = false;
        let cfg = self.config.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { cfg.save() }).await;
            if let Err(e) = result {
                log::warn!("could not save the layout: {e}");
            }
            this.update(cx, |s, cx| {
                s.save_inflight = false;
                if s.save_dirty && s.save_task.is_none() {
                    s.schedule_save_again(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-arms the debounce without pushing another echo (the config did not change since).
    fn schedule_save_again(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |s, cx| {
                s.save_task = None;
                s.run_save(cx);
            })
            .ok();
        }));
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, _window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.theme.mode {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Oled,
            Mode::Oled => Mode::Dark,
        };
        self.set_theme(themed(next, &self.config), cx);
    }

    /// Re-themes every part of the window.
    fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        // Mode or accent: anything that differs is applied to every live view.
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        self.status.update(cx, |s, cx| s.set_theme(theme, cx));
        if let Some(hub) = self.chat.clone() {
            hub.update(cx, |h, cx| h.set_theme(theme, cx));
        }
        for slot in &self.tabs {
            slot.handle.theme_changed(theme, cx);
        }
        self.sync_layout_panel(cx, |p, _, theme, cx| p.set_theme(theme, cx));
        self.palette.update(cx, |p, cx| p.set_theme(theme, cx));
        if let Some(settings) = self.settings.clone() {
            settings.update(cx, |s, cx| s.set_theme(theme, cx));
        }
        if let Some(ob) = self.onboarding.clone() {
            ob.update(cx, |o, cx| o.set_theme(theme, cx));
        }
        cx.notify();
    }

    /// A new configuration (hot reload, settings screen, wizard): layout, completion settings and
    /// theme follow it. Nothing happens for what did not change.
    fn adopt_config(&mut self, config: Arc<Config>, cx: &mut Context<Self>) {
        let layout_moved = layout_changed(&self.config, &config);
        let theme_moved =
            self.config.appearance.mode != config.appearance.mode || self.config.appearance.accent != config.appearance.accent;
        let completion_moved = self.config.terminal.completion != config.terminal.completion;
        let panel_config = config.layout.clone();
        self.config = config;
        if completion_moved {
            cx.set_global(TermEnv(self.config.completion_env()));
        }
        if theme_moved {
            self.set_theme(theme_for(&self.config), cx);
        }
        if layout_moved {
            self.recompute_layout(true, cx);
            self.sync_layout_panel(cx, move |p, _, _, cx| p.set_config(panel_config, cx));
        }
    }

    fn toggle_layout_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout_open = !self.layout_open;
        self.layout_panel = if self.layout_open {
            self.menu_open = false;
            let shell = cx.weak_entity();
            let args = LayoutPanelArgs {
                theme: self.theme,
                layout: self.config.layout.clone(),
                cwd: self.active_cwd(cx),
                on_change: Rc::new(move |layout, cx| apply_from_panel(&shell, layout, cx)),
            };
            let view = build_layout_panel(args, window, cx);
            if let Ok(panel) = view.clone().downcast::<ui_layout::LayoutPanel>() {
                panel.read(cx).focus(window);
            }
            Some(view)
        } else {
            None
        };
        cx.notify();
    }

    fn close_layout_panel(&mut self, cx: &mut Context<Self>) {
        if self.layout_open {
            self.layout_open = false;
            self.layout_panel = None;
            cx.notify();
        }
    }

    // ------------------------------------------------------------------------------------
    // Panels, drawers and menus
    // ------------------------------------------------------------------------------------

    /// Is the right panel `id` showing (docked, or open as an overlay / drawer)?
    fn right_panel_active(&self, id: &PanelId) -> bool {
        if self.eff.breakpoint <= Breakpoint::S {
            self.overlay == Some(Overlay::Right) && &self.overlay_right == id
        } else {
            self.layout.right_panel.as_ref() == Some(id)
        }
    }

    /// Rail / menu button of a panel: docks it on the right (a saved layout tweak), or opens it as
    /// an overlay / drawer when the window is narrow (not saved: the intent does not change).
    pub(crate) fn toggle_right_panel(&mut self, id: PanelId, cx: &mut Context<Self>) {
        if self.eff.breakpoint <= Breakpoint::S {
            if self.right_panel_active(&id) {
                self.overlay = None;
            } else {
                self.overlay_right = id;
                self.overlay = Some(Overlay::Right);
            }
            self.overlay_epoch += 1;
            self.menu_open = false;
            cx.notify();
            return;
        }
        self.tweak_global(true, cx, |l| {
            l.right_panel = if l.right_panel.as_ref() == Some(&id) { None } else { Some(id.clone()) };
        });
    }

    fn toggle_overlay(&mut self, which: Overlay, cx: &mut Context<Self>) {
        self.overlay = if self.overlay == Some(which) { None } else { Some(which) };
        self.overlay_epoch += 1;
        self.menu_open = false;
        cx.notify();
    }

    fn close_overlay(&mut self, cx: &mut Context<Self>) {
        if self.overlay.take().is_some() {
            cx.notify();
        }
    }

    /// The rail's buttons: the terminal view plus one per registered panel that can live on the
    /// right. Nothing is listed that has no provider behind it.
    fn rail_items(&self, cx: &mut Context<Self>) -> Vec<RailItem> {
        let shell = cx.entity();
        let mut items = vec![RailItem {
            id: "rail-terminal".into(),
            icon: Icon::Terminal,
            title: "Terminal".into(),
            active: true,
            on_click: None,
        }];
        // "Revisión": opens a review tab (main zone), not a right panel.
        let review_shell = shell.clone();
        items.push(RailItem {
            id: "rail-review".into(),
            icon: Icon::Review,
            title: "Revisar cambios".into(),
            active: self.active_id().and_then(|id| self.slot(id)).is_some_and(|s| s.handle.downcast::<ReviewTab>().is_some()),
            on_click: Some(Rc::new(move |window, cx| {
                review_shell.update(cx, |s, cx| s.open_review(window, cx));
            })),
        });
        if let Some(reg) = cx.try_global::<PanelRegistry>() {
            for p in reg.for_zone(PanelZone::Right) {
                let id = p.id();
                let shell = shell.clone();
                items.push(RailItem {
                    id: format!("rail-{}", id.as_str()).into(),
                    icon: p.icon(),
                    title: p.title(),
                    active: self.right_panel_active(&id),
                    on_click: Some(Rc::new(move |_, cx| {
                        shell.update(cx, |s, cx| s.toggle_right_panel(id.clone(), cx));
                    })),
                });
            }
        }
        items
    }

    // ------------------------------------------------------------------------------------
    // Dragging
    // ------------------------------------------------------------------------------------

    fn start_drag(&mut self, drag: Drag) {
        self.drag = Some(drag);
        self.drag_moved = false;
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        if e.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let b = self.split_bounds.get();
        match drag {
            Drag::Sidebar { side, start_x, start_w } => {
                let w = dragged_width(side, start_w, f32::from(e.position.x) - start_x);
                if self.layout.sidebar.width_px != w {
                    self.layout.sidebar.width_px = w;
                    self.eff.layout.sidebar.width_px = w;
                    self.drag_moved = true;
                    cx.notify();
                }
            },
            Drag::Right { start_x, start_w } => {
                let w = dragged_right_width(start_w, f32::from(e.position.x) - start_x);
                if self.right_w != Some(w) {
                    self.right_w = Some(w);
                    self.drag_moved = true;
                    cx.notify();
                }
            },
            Drag::SplitX => {
                self.ratio_x = ratio_at(f32::from(e.position.x), f32::from(b.origin.x), f32::from(b.size.width));
                self.drag_moved = true;
                cx.notify();
            },
            Drag::SplitY => {
                self.ratio_y = ratio_at(f32::from(e.position.y), f32::from(b.origin.y), f32::from(b.size.height));
                self.drag_moved = true;
                cx.notify();
            },
        }
    }

    /// Releases a drag; a resized sidebar is persisted (debounced), and so is the session (right
    /// panel width, split ratios).
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        if !std::mem::take(&mut self.drag_moved) {
            return;
        }
        if let Drag::Sidebar { .. } = drag {
            let w = self.layout.sidebar.width_px;
            // A collapsed (S) sidebar is not resizable, so this is always the configured column.
            self.tweak_global(false, cx, |l| l.sidebar.width_px = w);
        }
        self.mark_session_dirty(cx);
    }

    // ------------------------------------------------------------------------------------
    // Right panel
    // ------------------------------------------------------------------------------------

    /// Right panel id to render now: the open overlay's, the layout's, or the previous docked one
    /// while it animates out.
    fn right_id(&self) -> Option<PanelId> {
        if self.overlay == Some(Overlay::Right) {
            return Some(self.overlay_right.clone());
        }
        self.eff
            .layout
            .right_panel
            .clone()
            .or_else(|| self.transition.as_ref().and_then(|t| t.from.right_panel.clone()))
    }

    /// Width of the docked right panel for layout `l` in a window of `window_w`.
    fn right_width(&self, l: &ResolvedLayout, window_w: f32) -> f32 {
        let margin = Metrics::of(l.density).margin;
        let others = Self::rail_w(l)
            + Self::sidebar_zone_w(l, SidebarSide::Left)
            + Self::sidebar_zone_w(l, SidebarSide::Right)
            + 2.0 * margin
            + GUTTER;
        right_panel_width(window_w, self.right_w, others)
    }

    /// Width of the right panel as mounted now (overlay / drawer or docked).
    fn right_mounted_width(&self) -> f32 {
        if self.overlay == Some(Overlay::Right) {
            drawer_width(self.window_w)
        } else {
            self.right_width(&self.eff.layout, self.window_w)
        }
    }

    /// Builds (or re-targets) the right panel's view for the active tab.
    fn ensure_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.right_id() else {
            self.right = None;
            return;
        };
        // The threads panel needs the chat hub: created now, before the panel is built.
        if id.as_str() == ui_chat::THREADS_PANEL_ID {
            self.ensure_chat(window, cx);
        }
        let info = self.active_info(cx);
        let ctx = PanelCtx {
            zone: PanelZone::Right,
            // The class comes from the zone's real width, not from the window's.
            width_class: WidthClass::from_width(self.right_mounted_width()),
            cwd: info.as_ref().and_then(|i| i.cwd.clone()),
            project_root: info.and_then(|i| i.project_root),
            theme: self.theme,
        };
        if let Some(r) = &self.right
            && r.id == id
            && r.ctx == ctx
        {
            return;
        }
        let provider = cx.try_global::<PanelRegistry>().and_then(|r| r.get(&id));
        let retargeted = match (&self.right, &provider) {
            (Some(r), Some(p)) if r.id == id => {
                r.view.as_ref().is_some_and(|v| p.context_changed(v, &ctx, window, cx))
            },
            _ => false,
        };
        if retargeted {
            if let Some(r) = &mut self.right {
                r.ctx = ctx;
            }
            return;
        }
        let view = provider.map(|p| p.build(ctx.clone(), window, cx));
        self.right = Some(RightPanel { id, ctx, view });
    }

    // ------------------------------------------------------------------------------------
    // Window background (glass)
    // ------------------------------------------------------------------------------------

    /// Is a docked zone painted as glass right now? (Sidebar on either side, collapsed or not,
    /// and the right panel.)
    fn glass_zones_visible(&self) -> bool {
        let l = &self.eff.layout;
        let sidebar = Self::sidebar_visible(l);
        let right = l.right_panel.is_some() && !l.zen;
        sidebar || right
    }

    /// Acrylic is enabled for the whole window only while a docked glass zone asks for
    /// translucency; everything else paints opaque, so only those zones show the blurred backdrop.
    fn sync_background(&mut self, window: &mut Window) {
        let want = self.layout.sidebar_translucency > 0 && self.glass_zones_visible();
        if want != self.blurred {
            self.blurred = want;
            window.set_background_appearance(if want {
                WindowBackgroundAppearance::Blurred
            } else {
                WindowBackgroundAppearance::Opaque
            });
        }
    }

    // ------------------------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------------------------

    /// Wraps `content` in a zone of animated size. With no transition (or equal sizes) it is a
    /// plain fixed-size box; otherwise the box animates `from -> to` while the content keeps its
    /// final size and is clipped, so nothing reflows during the 200 ms.
    fn zone(
        &self,
        name: &'static str,
        horizontal: bool,
        from: f32,
        to: f32,
        content: impl IntoElement,
    ) -> AnyElement {
        let animate = self.transition.is_some() && (from - to).abs() > 0.5;
        // The content is always laid out at its *target* size (the old size only when the zone is
        // going away): the new content shows immediately and never reflows or swaps at the end;
        // only the clipping box moves (SHEET easing, no overshoot).
        let full = if animate && to <= 0.5 { from } else { to };
        let inner = if horizontal {
            div().w(px(full)).h_full().flex_none().child(content)
        } else {
            div().h(px(full)).w_full().flex_none().child(content)
        };
        let outer = div().flex_none().overflow_hidden().child(inner);
        let outer = if horizontal { outer.h_full() } else { outer.w_full() };
        if !animate {
            return if horizontal { outer.w(px(to)) } else { outer.h(px(to)) }.into_any_element();
        }
        let epoch = self.transition.as_ref().map_or(0, |t| t.epoch);
        let outer = if horizontal { outer.w(px(from)) } else { outer.h(px(from)) };
        outer
            .with_animation(
                ElementId::from((name, epoch as usize)),
                Animation::new(BASE).with_easing(|t| SHEET.solve(t).clamp(0.0, 1.0)),
                move |el, t| {
                    let v = from + (to - from) * t;
                    if horizontal { el.w(px(v)) } else { el.h(px(v)) }
                },
            )
            .into_any_element()
    }

    fn sidebar_visible(l: &ResolvedLayout) -> bool {
        l.sidebar.side != SidebarSide::Hidden && !l.zen
    }

    /// Width of the sidebar zone (card plus its outer margin) for a layout, per side.
    fn sidebar_zone_w(l: &ResolvedLayout, side: SidebarSide) -> f32 {
        if Self::sidebar_visible(l) && l.sidebar.side == side {
            f32::from(l.sidebar.width_px) + Metrics::of(l.density).margin
        } else {
            0.0
        }
    }

    fn rail_w(l: &ResolvedLayout) -> f32 {
        if l.rail == RailVisibility::Visible && !l.zen { RAIL_W } else { 0.0 }
    }

    fn right_zone_w(&self, l: &ResolvedLayout) -> f32 {
        if l.right_panel.is_some() && !l.zen {
            self.right_width(l, self.window_w) + GUTTER + Metrics::of(l.density).margin
        } else {
            0.0
        }
    }

    /// The layout whose content a zone should draw: the current one, or the previous one while the
    /// zone is animating out.
    fn content_layout(&self, visible: impl Fn(&ResolvedLayout) -> bool) -> &ResolvedLayout {
        match &self.transition {
            Some(t) if !visible(&self.layout) && visible(&t.from) => &t.from,
            _ => &self.layout,
        }
    }

    fn layout_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.entity();
        let dismiss = shell.clone();
        // BottomEnd: the button sits at the right of the titlebar, so the panel grows leftwards
        // from the button's right edge (clamped to the window); in XS it is a bottom sheet.
        let popover = Popover::new("layout-pop", &self.theme)
            .open(self.layout_open)
            .placement(Placement::BottomEnd)
            .padding(0.0)
            .width(ui_layout::PANEL_WIDTH)
            .sheet_when_narrow(true)
            .on_dismiss(move |_, cx| dismiss.update(cx, |s, cx| s.close_layout_panel(cx)))
            .trigger(
                IconButton::new("layout-btn", &self.theme, Icon::Layout)
                    .size(28.0)
                    .active(self.layout_open)
                    .emphasize_on_hover(true)
                    .tooltip("Layout (Ctrl+Shift+L)")
                    .on_click(move |_, window, cx| {
                        shell.update(cx, |s, cx| s.toggle_layout_panel(window, cx));
                    }),
            )
            .children(self.layout_panel.clone());
        div().h_full().flex().flex_col().justify_center().child(popover)
    }

    /// The `...` menu of XS: the rail's buttons and the sidebar drawer.
    fn rail_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.entity();
        let dismiss = shell.clone();
        let items = self.rail_items(cx);
        let mut rows = div().flex().flex_col().w_full();
        if self.eff.sidebar == SidebarMode::Drawer {
            let s = shell.clone();
            rows = rows.child(
                ListRow::new("menu-sidebar", &self.theme)
                    .icon(Icon::Folder)
                    .title("Barra lateral")
                    .selected(self.overlay == Some(Overlay::Left))
                    .on_click(move |_, _, cx| s.update(cx, |s, cx| s.toggle_overlay(Overlay::Left, cx))),
            );
        }
        for item in items {
            let RailItem { id, icon, title, active, on_click } = item;
            let mut row = ListRow::new(id, &self.theme).icon(icon).title(title).selected(active);
            if let Some(f) = on_click {
                row = row.on_click(move |_, window, cx| f(window, cx));
            }
            rows = rows.child(row);
        }
        let popover = Popover::new("rail-menu", &self.theme)
            .open(self.menu_open)
            .placement(Placement::BottomEnd)
            .padding(4.0)
            .width(240.0)
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, |s, cx| {
                    s.menu_open = false;
                    cx.notify();
                });
            })
            .trigger(
                IconButton::new("menu-btn", &self.theme, Icon::Dots)
                    .size(28.0)
                    .active(self.menu_open)
                    .emphasize_on_hover(true)
                    .tooltip("Menú")
                    .on_click(move |_, _, cx| {
                        shell.update(cx, |s, cx| {
                            s.menu_open = !s.menu_open;
                            s.tabs_menu_open = false;
                            cx.notify();
                        });
                    }),
            )
            .child(rows);
        div().h_full().flex().flex_col().justify_center().child(popover)
    }

    /// The "all tabs" menu (the XS tab selector): every tab in a list.
    fn tabs_menu(&self, infos: &[TabInfo], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.entity();
        let dismiss = shell.clone();
        let active = self.active_id();
        let mut rows = div().flex().flex_col().w_full().max_h(px(360.0)).overflow_hidden();
        for info in infos {
            let id = info.id;
            let s = shell.clone();
            let mut row = ListRow::new(("alltab", id), &self.theme)
                .icon(info.icon.icon())
                .when_some(crate::agents::agent_of(&info.icon), |r, m| r.agent(m))
                .title(info.title.clone())
                .selected(Some(id) == active)
                .on_click(move |_, window, cx| s.update(cx, |s, cx| s.select_tab(id, window, cx)));
            if let Some(sub) = info.subtitle.clone() {
                row = row.subtitle(sub);
            }
            if let Some(st) = info.status {
                row = row.status(st);
            }
            rows = rows.child(row);
        }
        let popover = Popover::new("tabs-menu", &self.theme)
            .open(self.tabs_menu_open)
            .placement(Placement::BottomEnd)
            .padding(4.0)
            .width(280.0)
            .sheet_when_narrow(true)
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, |s, cx| {
                    s.tabs_menu_open = false;
                    cx.notify();
                });
            })
            .trigger(
                IconButton::new("tabs-menu-btn", &self.theme, Icon::ChevronDown)
                    .size(28.0)
                    .active(self.tabs_menu_open)
                    .emphasize_on_hover(true)
                    .tooltip("Todas las pestañas")
                    .on_click(move |_, _, cx| {
                        shell.update(cx, |s, cx| {
                            s.tabs_menu_open = !s.tabs_menu_open;
                            s.menu_open = false;
                            cx.notify();
                        });
                    }),
            )
            .child(rows);
        div().h_full().flex().flex_col().justify_center().child(popover)
    }

    /// The tab pills in a horizontally scrolling strip (never narrower than [`TAB_MIN_W`] each),
    /// with arrows that appear when the tabs overflow.
    fn tab_strip(&mut self, infos: &[TabInfo], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = self.theme;
        let c = theme.colors;
        let shell = cx.entity();
        let active = self.active_id();
        let handle = self.tab_scroll.clone();
        if std::mem::take(&mut self.scroll_active)
            && let Some(ix) = infos.iter().position(|i| Some(i.id) == active)
        {
            handle.scroll_to_item(ix);
        }
        let max = f32::from(handle.max_offset().width);
        let offset = f32::from(handle.offset().x);
        let overflow = max > 0.5;
        let mut pills = div()
            .id("tab-strip")
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .overflow_x_scroll()
            .track_scroll(&handle);
        for info in infos {
            let id = info.id;
            let (s1, s2) = (shell.clone(), shell.clone());
            pills = pills.child(
                div().flex_none().min_w(px(TAB_MIN_W)).child(tab_pill(
                    &theme,
                    info,
                    Some(id) == active,
                    move |_, window, cx| s1.update(cx, |s, cx| s.select_tab(id, window, cx)),
                    move |_, window, cx| s2.update(cx, |s, cx| s.request_close(id, window, cx)),
                )),
            );
        }
        // The chevron-right icon, turned half a circle for "previous".
        let arrow = |id: &'static str, left: bool, enabled: bool| {
            let h = handle.clone();
            let shell = shell.clone();
            let mut glyph = icon(Icon::ChevronRight, 12.0, if enabled { c.muted } else { c.outline });
            if left {
                glyph = glyph.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI)));
            }
            div()
                .id(id)
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(c.surface_2)))
                .on_click(move |_, _, cx| {
                    if enabled {
                        let o = h.offset();
                        let step = if left { 160.0 } else { -160.0 };
                        let x = (f32::from(o.x) + step).clamp(-f32::from(h.max_offset().width), 0.0);
                        h.set_offset(gpui::point(px(x), o.y));
                        shell.update(cx, |_, cx| cx.notify());
                    }
                })
                .child(glyph)
        };
        div()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .children(overflow.then(|| arrow("tabs-left", true, offset < -0.5)))
            .child(pills)
            .children(overflow.then(|| arrow("tabs-right", false, offset > -max + 0.5)))
    }

    /// The center column's top row: breadcrumb, tab strip (when tabs are on top, or always in XS),
    /// layout button, the `...` menu in XS and, if nothing sits to its right, the window controls.
    fn title_bar(
        &mut self,
        infos: &[TabInfo],
        window: &Window,
        cx: &mut Context<Self>,
        with_controls: bool,
        pad_left: f32,
    ) -> Div {
        let theme = self.theme;
        let shell = cx.entity();
        let mut row = title_row(&theme).pl(px(pad_left));
        let unified = Self::is_unified(&self.eff.layout) && self.eff.breakpoint != Breakpoint::Xs;
        if unified {
            row = row.child(self.title_heading(infos, cx));
        } else if self.eff.layout.breadcrumb {
            let last_only = self.eff.breadcrumb_last_only;
            let detail: SharedString = match self.active_cwd(cx) {
                Some(cwd) if last_only => paths::tab_label(&cwd).into(),
                Some(cwd) => paths::breadcrumb(&cwd).into(),
                None => "~".into(),
            };
            row = row.child(breadcrumb(&theme, detail, last_only));
        }
        let xs = self.eff.breakpoint == Breakpoint::Xs;
        if self.eff.layout.tabs_position == TabsPosition::Top || xs {
            row = row.child(self.tab_strip(infos, cx));
            if xs || infos.len() > 1 {
                row = row.child(self.tabs_menu(infos, cx));
            }
            row = row.child(new_tab_button(&theme, move |_, window, cx| {
                shell.update(cx, |s, cx| s.new_tab_here(window, cx));
            }));
        }
        row = row.child(div().flex_1()).child(self.titlebar_buttons(cx));
        if self.eff.rail_in_menu {
            row = row.child(self.rail_menu(cx));
        }
        if with_controls {
            row = row.child(window_controls(&theme, window.is_maximized()));
        } else {
            row = row.pr(px(8.0));
        }
        row
    }

    /// Shell v2 layouts: the tabs are rows of the sidebar's spaces.
    fn is_unified(l: &ResolvedLayout) -> bool {
        l.sidebar.nested_tabs && l.sidebar.side != SidebarSide::Hidden && l.tabs_position == TabsPosition::Sidebar && !l.zen
    }

    /// Left of the titlebar in the unified layout: the active tab's title and, muted, its project
    /// (or the branch when the tab is named after the project).
    fn title_heading(&self, infos: &[TabInfo], cx: &App) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let active = self.active_id().and_then(|id| infos.iter().find(|i| i.id == id));
        let title: SharedString = active.map_or_else(|| "Selachi".into(), |i| i.title.clone());
        let project = active.and_then(crate::grouping::project_key).map(|p| crate::grouping::project_name(&p));
        let muted: Option<SharedString> = match (project, active.and_then(|i| i.subtitle.clone())) {
            (Some(p), _) if p != title.as_ref() => Some(p.into()),
            (_, branch) => branch,
        };
        let _ = cx;
        div()
            .min_w_0()
            .flex()
            .flex_row()
            .items_baseline()
            .gap(px(10.0))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_size(px(ty.ui_size_lg))
                    .text_color(c.text)
                    .child(title),
            )
            .children(muted.map(|m| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.muted)
                    .child(m)
            }))
    }

    /// The panel buttons at the right of the titlebar (`layout.titlebar.panel_buttons`): one per
    /// registered right panel, the review tab and the layout popover, with their active state.
    /// Ids nobody registered are skipped. The XS menu already lists them, so only the layout
    /// popover stays there.
    fn titlebar_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let l = &self.eff.layout;
        let xs = self.eff.rail_in_menu;
        let shell = cx.entity();
        let mut group = div().h_full().flex_none().flex().flex_row().items_center().gap(px(2.0));
        let ids: Vec<PanelId> = l.titlebar.panel_buttons.clone();
        for id in ids {
            let id_str = id.as_str();
            if id_str == store::config::panel_ids::LAYOUT {
                group = group.child(self.layout_button(cx));
                continue;
            }
            if xs {
                continue;
            }
            let (ic, title, active): (Icon, SharedString, bool) = if id_str == store::config::panel_ids::REVIEW {
                let active =
                    self.active_id().and_then(|t| self.slot(t)).is_some_and(|s| s.handle.downcast::<ReviewTab>().is_some());
                (Icon::Review, "Revisar cambios".into(), active)
            } else {
                let Some(p) = cx.try_global::<PanelRegistry>().and_then(|r| r.get(&id)) else { continue };
                if !p.allowed_zones().contains(&PanelZone::Right) {
                    continue;
                }
                (p.icon(), p.title(), self.right_panel_active(&id))
            };
            let shell = shell.clone();
            let click_id = id.clone();
            group = group.child(
                div().h_full().flex().flex_col().justify_center().child(
                    IconButton::new(SharedString::from(format!("tb-{id_str}")), &self.theme, ic)
                        .size(28.0)
                        .active(active)
                        .emphasize_on_hover(true)
                        .tooltip(title)
                        .on_click(move |_, window, cx| {
                            let click_id = click_id.clone();
                            shell.update(cx, |s, cx| {
                                if click_id.as_str() == store::config::panel_ids::REVIEW {
                                    s.open_review(window, cx);
                                } else {
                                    s.toggle_right_panel(click_id, cx);
                                }
                            });
                        }),
                ),
            );
        }
        group
    }

    fn pane_header(&self, info: &TabInfo) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .h(px(22.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(4.0))
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .child(crate::agents::lead_icon(&info.icon, 14.0, c.muted))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(info.title.clone()))
            .children(info.subtitle.clone().map(|s| div().text_color(c.outline).child(s)))
            .child(div().flex_1())
            .children(info.status.map(|s| ui_kit::StatusGlyph::new(&self.theme, s)))
    }

    /// One pane: a floating card hosting a tab (or an empty-pane prompt).
    fn pane(&self, ix: usize, infos: &[TabInfo], m: Metrics, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let multi = pane_count(self.eff.layout.splits) > 1;
        let focused = self.panes.focused() == ix;
        let shell = cx.entity();
        let tab = self.panes.panes()[ix];
        let card = div()
            .id(("pane", ix))
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .px(px(m.pad_x))
            .py(px(m.pad_y))
            .rounded(px(Radius::MD))
            .bg(c.surface_0)
            .border_1()
            .border_color(if multi && focused { hsla(c.accent).opacity(0.55) } else { hsla(c.surface_0) })
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, {
                let shell = shell.clone();
                move |_, window, cx| shell.update(cx, |s, cx| s.focus_pane(ix, window, cx))
            });
        match tab.and_then(|id| self.slot(id).map(|s| (id, s))) {
            Some((id, slot)) => {
                let info = infos.iter().find(|i| i.id == id);
                let is_viewer = slot.handle.downcast::<ViewerTab>().is_some();
                let drop = is_viewer.then(|| self.viewer_drop_zone("viewer-drop", Radius::MD, cx));
                card.relative()
                    .children(multi.then(|| info.map(|i| self.pane_header(i))).flatten())
                    .child(slot.handle.content(cx))
                    .children(drop)
            },
            None => card.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.muted)
                    .child("Panel vacío")
                    .child(Button::tonal(format!("pane-new-{ix}"), &self.theme, "Nueva terminal").icon(Icon::Plus).on_click(
                        move |_, window, cx| {
                            shell.update(cx, |s, cx| {
                                s.focus_pane(ix, window, cx);
                                s.new_tab_here(window, cx);
                            });
                        },
                    )),
            ),
        }
    }

    /// A resize handle between two panes.
    fn gutter(&self, vertical_bar: bool, drag: Drag, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.entity();
        let d = div().flex_none();
        let d = if vertical_bar {
            d.w(px(GUTTER)).h_full().cursor_col_resize()
        } else {
            d.h(px(GUTTER)).w_full().cursor_row_resize()
        };
        d.on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
            shell.update(cx, |s, _| s.start_drag(drag));
        })
    }

    /// A flex child that takes `share` of its parent's main axis.
    fn share(share: f32) -> Div {
        let mut d = div().min_w_0().min_h_0().flex().flex_col();
        let s = d.style();
        s.flex_grow = Some(share);
        s.flex_shrink = Some(1.0);
        s.flex_basis = Some(relative(0.0).into());
        d
    }

    /// The split area, laid out as `layout.splits`.
    fn pane_area(&self, infos: &[TabInfo], m: Metrics, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (rx, ry) = (clamp_ratio(self.ratio_x), clamp_ratio(self.ratio_y));
        let bounds = self.split_bounds.clone();
        let tracker = canvas(move |b, _, _| bounds.set(b), |_, (), _, _| {}).absolute().size_full();
        let row = || div().size_full().min_w_0().min_h_0().flex().flex_row();
        let col = || div().size_full().min_w_0().min_h_0().flex().flex_col();
        let body: AnyElement = match self.eff.layout.splits {
            Splits::Single => self.pane(0, infos, m, cx).into_any_element(),
            Splits::TwoVertical => row()
                .child(Self::share(rx).child(self.pane(0, infos, m, cx)))
                .child(self.gutter(true, Drag::SplitX, cx))
                .child(Self::share(1.0 - rx).child(self.pane(1, infos, m, cx)))
                .into_any_element(),
            Splits::TwoHorizontal => col()
                .child(Self::share(ry).child(self.pane(0, infos, m, cx)))
                .child(self.gutter(false, Drag::SplitY, cx))
                .child(Self::share(1.0 - ry).child(self.pane(1, infos, m, cx)))
                .into_any_element(),
            Splits::OneAndTwoStacked => row()
                .child(Self::share(rx).child(self.pane(0, infos, m, cx)))
                .child(self.gutter(true, Drag::SplitX, cx))
                .child(
                    Self::share(1.0 - rx)
                        .child(Self::share(ry).child(self.pane(1, infos, m, cx)))
                        .child(self.gutter(false, Drag::SplitY, cx))
                        .child(Self::share(1.0 - ry).child(self.pane(2, infos, m, cx))),
                )
                .into_any_element(),
            Splits::Grid2x2 => col()
                .child(
                    Self::share(ry).child(
                        row()
                            .child(Self::share(rx).child(self.pane(0, infos, m, cx)))
                            .child(self.gutter(true, Drag::SplitX, cx))
                            .child(Self::share(1.0 - rx).child(self.pane(1, infos, m, cx))),
                    ),
                )
                .child(self.gutter(false, Drag::SplitY, cx))
                .child(
                    Self::share(1.0 - ry).child(
                        row()
                            .child(Self::share(rx).child(self.pane(2, infos, m, cx)))
                            .child(self.gutter(true, Drag::SplitX, cx))
                            .child(Self::share(1.0 - rx).child(self.pane(3, infos, m, cx))),
                    ),
                )
                .into_any_element(),
        };
        div().relative().flex_1().min_w_0().min_h_0().child(body).child(tracker)
    }

    /// Tint of a glass zone, or `None` when translucency is 0 (the zone keeps its opaque card).
    fn glass(&self, l: &ResolvedLayout) -> Option<Hsla> {
        (l.sidebar_translucency > 0).then(|| glass_tint(&self.theme, l.sidebar_translucency))
    }

    /// In-window overlays (drawers): glass, but nearly opaque: the desktop blur does not reach the
    /// terminal text behind them.
    fn overlay_bg(&self) -> Hsla {
        match self.layout.sidebar_translucency {
            0 => hsla(self.theme.colors.surface_1),
            t => glass_tint(&self.theme, t.min(OVERLAY_MAX_TRANSLUCENCY)),
        }
    }

    /// The sidebar card (header with the logo when the rail is hidden, then the sections). On
    /// glass the whole column is tinted (blurred desktop behind), with no card of its own.
    fn sidebar_card(
        &self,
        l: &ResolvedLayout,
        infos: &[TabInfo],
        window: &Window,
        cx: &mut Context<Self>,
        opts: SidebarOpts,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let m = Metrics::of(l.density);
        let shell = cx.entity();
        let tint = if opts.overlay { Some(self.overlay_bg()) } else { self.glass(l) };
        let props = SidebarProps {
            theme: &self.theme,
            layout: l,
            tabs: infos,
            active: self.active_id(),
            shell: &shell,
            hovered_row: self.hovered_row,
            glass: tint.is_some(),
        };
        let right_side = l.sidebar.side == SidebarSide::Right;
        let mut header = div()
            .h(px(TITLEBAR_H))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .window_control_area(gpui::WindowControlArea::Drag);
        if opts.collapsed {
            header = header.justify_center();
            if l.rail == RailVisibility::Hidden {
                header = header.child(ui_kit::Logo::new(self.theme.mode).size(28.0));
            }
        } else if l.rail == RailVisibility::Hidden && !right_side {
            header = header.pl(px(m.margin + 8.0)).child(ui_kit::Logo::new(self.theme.mode).size(28.0));
        } else {
            header = header.pl(px(m.margin));
        }
        let unified = l.sidebar.nested_tabs;
        if !opts.collapsed {
            header = header.child(div().flex_1());
            if unified {
                let shell = shell.clone();
                header = header.child(
                    div().pr(px(6.0)).child(crate::unified_sidebar::collapse_button(&self.theme, tint.is_some(), move |_, cx| {
                        shell.update(cx, |s, cx| s.toggle_sidebar_collapsed(cx));
                    })),
                );
            } else if l.tabs_position == TabsPosition::Sidebar {
                let shell = shell.clone();
                header = header.child(new_tab_button(&self.theme, move |_, window, cx| {
                    shell.update(cx, |s, cx| s.new_tab_here(window, cx));
                }));
            }
        }
        if opts.with_controls {
            header = header.child(window_controls(&self.theme, window.is_maximized()));
        }
        let body: AnyElement = if unified {
            let git = self.space_git(cx);
            let up = UnifiedProps {
                theme: &self.theme,
                layout: l,
                tabs: infos,
                active: self.active_id(),
                shell: &shell,
                glass: tint.is_some(),
                spaces: spaces::build_spaces(infos, l.sidebar.group_by, &git, &self.known_projects),
                folded: &self.folded_spaces,
                hovered_row: self.hovered_row,
                hovered_space: self.hovered_space,
                usage: self.usage_summary(cx),
            };
            let panel_items: Vec<RailItem> =
                self.rail_items(cx).into_iter().filter(|i| i.id.as_ref() != "rail-terminal").collect();
            if opts.collapsed {
                unified_collapsed(&up, panel_items).into_any_element()
            } else {
                unified_content(&up, panel_items).into_any_element()
            }
        } else if opts.collapsed {
            sidebar_collapsed(&props).into_any_element()
        } else {
            sidebar_content(&props).into_any_element()
        };
        let card = div()
            .flex_1()
            .min_h_0()
            .mb(px(m.margin))
            .rounded(px(Radius::MD))
            .overflow_hidden()
            .when(tint.is_none(), |d| d.bg(hsla(c.surface_1)))
            .child(body);
        let mut col = div().size_full().flex().flex_col().child(header);
        if let Some(t) = tint {
            col = col.bg(t);
        }
        let card = if opts.collapsed {
            card.mx(px(4.0))
        } else if right_side {
            card.mr(px(m.margin))
        } else {
            card.ml(px(m.margin))
        };
        col.child(card)
    }

    /// The right panel: header (title, window controls) and the provider's view. `overlay`: it is
    /// the S overlay / XS drawer (no resize handle, near-opaque glass).
    fn right_card(
        &self,
        window: &Window,
        with_controls: bool,
        m: Metrics,
        overlay: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let tint = if overlay { Some(self.overlay_bg()) } else { self.glass(&self.eff.layout) };
        let registry_info = self.right.as_ref().and_then(|r| {
            cx.try_global::<PanelRegistry>().and_then(|reg| reg.get(&r.id)).map(|p| (p.title(), p.icon()))
        });
        let mut header = div()
            .h(px(TITLEBAR_H))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .pl(px(GUTTER + 6.0))
            .window_control_area(gpui::WindowControlArea::Drag)
            .text_size(px(ty.ui_size_md))
            .text_color(c.muted);
        if let Some((title, ic)) = registry_info {
            header = header.child(icon(ic, 16.0, c.muted)).child(title);
        }
        header = header.child(div().flex_1());
        if with_controls {
            header = header.child(window_controls(&self.theme, window.is_maximized()));
        }
        let is_tree = self.right.as_ref().is_some_and(|r| r.id.as_str() == ui_viewer::FILES_PANEL_ID);
        let body: AnyElement = match &self.right {
            Some(RightPanel { view: Some(v), .. }) if is_tree => div()
                .relative()
                .size_full()
                .child(v.clone())
                .child(self.viewer_drop_zone("tree-drop", Radius::MD, cx))
                .into_any_element(),
            Some(RightPanel { view: Some(v), .. }) => v.clone().into_any_element(),
            Some(RightPanel { id, view: None, .. }) => Card::new(&self.theme)
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(c.text).text_size(px(ty.ui_size_md)).child("Panel no disponible"))
                .child(
                    div()
                        .text_color(c.muted)
                        .text_size(px(ty.ui_size_sm))
                        .child(format!("«{}» no está registrado.", id.as_str())),
                )
                .into_any_element(),
            None => div().into_any_element(),
        };
        let shell = cx.entity();
        let w = self.right_mounted_width();
        // The drag handle: the 8 px gutter on the card's left edge.
        let handle = div().w(px(GUTTER)).h_full().flex_none().when(!overlay, |d| {
            d.cursor_col_resize().on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                let x = f32::from(e.position.x);
                shell.update(cx, |s, _| s.start_drag(Drag::Right { start_x: x, start_w: w }));
            })
        });
        let mut col = div().size_full().flex().flex_col().child(header);
        if let Some(t) = tint {
            col = col.bg(t);
        }
        col.child(
            div()
                .flex_1()
                .min_h_0()
                .mb(px(m.margin))
                .mr(px(m.margin))
                .flex()
                .flex_row()
                .child(handle)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .rounded(px(Radius::MD))
                        .when(tint.is_none(), |d| d.bg(c.surface_1))
                        .overflow_hidden()
                        .child(body),
                ),
        )
    }

    /// The "close this tab?" prompt.
    fn confirm_overlay(&self, why: &SharedString, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let shell = cx.entity();
        let cancel = shell.clone();
        let id = self.confirm_close.as_ref().map(|(id, _)| *id);
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(Hsla { h: 0.0, s: 0.0, l: 0.0, a: 0.4 })
            .child(
                Card::new(&self.theme)
                    .radius(Radius::MD)
                    .p_4()
                    .w(px(380.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(div().text_size(px(ty.ui_size_lg)).text_color(c.text).child("¿Cerrar la pestaña?"))
                    .child(div().text_size(px(ty.ui_size_md)).text_color(c.muted).child(why.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(Button::ghost("confirm-cancel", &self.theme, "Cancelar").on_click(move |_, _, cx| {
                                cancel.update(cx, |s, cx| {
                                    s.confirm_close = None;
                                    cx.notify();
                                });
                            }))
                            .child(Button::primary("confirm-close", &self.theme, "Cerrar").on_click(
                                move |_, window, cx| {
                                    if let Some(id) = id {
                                        shell.update(cx, |s, cx| s.close_tab(id, window, cx));
                                    }
                                },
                            )),
                    ),
            )
    }
}

/// Callback of the layout popover: hands the new layout to the shell.
/// The file tree's markers for the changed files of a repository (absolute paths).
fn git_marks(info: &GitInfo) -> std::collections::HashMap<PathBuf, GitMark> {
    let mut out = std::collections::HashMap::new();
    for f in &info.marks {
        let mark = match f.code {
            'A' | 'C' => GitMark::Added,
            'D' => GitMark::Deleted,
            '?' => GitMark::Untracked,
            '!' => GitMark::Conflict,
            _ => GitMark::Modified,
        };
        // Conflicts come first in the list and must not be overwritten by a later entry.
        out.entry(info.root.join(&f.path)).or_insert(mark);
    }
    out
}

fn apply_from_panel(shell: &WeakEntity<AppShell>, layout: LayoutConfig, cx: &mut App) {
    shell.update(cx, |s, cx| s.set_layout_config(layout, true, cx)).ok();
}

/// How a sidebar card is drawn.
#[derive(Clone, Copy, Default)]
struct SidebarOpts {
    /// The window controls live in this zone (it is the outermost one on the right).
    with_controls: bool,
    /// Icon-only column (S).
    collapsed: bool,
    /// A drawer / expanded overlay over the main area.
    overlay: bool,
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ob) = self.onboarding.clone() {
            return self.onboarding_root(ob, window).into_any_element();
        }
        self.shell_root(window, cx)
    }
}

impl AppShell {
    /// The first-run wizard, with our own window controls (the system titlebar is hidden).
    fn onboarding_root(&self, ob: Entity<ui_onboarding::OnboardingView>, window: &Window) -> impl IntoElement + use<> {
        let theme = self.theme;
        let c = theme.colors;
        div()
            .id("onboarding-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(theme.typography.ui_family)
            .child(title_row(&theme).child(div().flex_1()).child(window_controls(&theme, window.is_maximized())))
            .child(div().flex_1().min_h_0().w_full().overflow_hidden().child(ob))
    }

    fn shell_root(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_tree_poll(cx);
        if let Some(t) = &self.transition
            && t.started.elapsed() > TRANSITION_KEEP
        {
            self.transition = None;
        }
        self.on_resize(f32::from(window.viewport_size().width), cx);
        self.ensure_right_panel(window, cx);
        self.sync_background(window);

        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let l = self.eff.layout.clone();
        let m = Metrics::of(l.density);
        let infos = self.infos(cx);
        let from = self.transition.as_ref().map(|t| t.from.clone()).unwrap_or_else(|| l.clone());
        let shell = cx.entity();

        // Which zone holds the window controls: the outermost one on the right.
        let right_sidebar_w = Self::sidebar_zone_w(&l, SidebarSide::Right);
        let right_panel_w = self.right_zone_w(&l);
        let controls_in_sidebar = right_sidebar_w > 0.0;
        let controls_in_right = !controls_in_sidebar && right_panel_w > 0.0;
        let controls_in_center = !controls_in_sidebar && !controls_in_right;

        // ---- rail (its buttons come from the panel registry)
        let rail_items = self.rail_items(cx);
        let rail_el = self.zone("zone-rail", true, Self::rail_w(&from), Self::rail_w(&l), rail(&theme, rail_items));

        // ---- sidebars (left / right)
        let sidebar_layout = self.content_layout(Self::sidebar_visible);
        let left_w = Self::sidebar_zone_w(&l, SidebarSide::Left);
        let left_from = Self::sidebar_zone_w(&from, SidebarSide::Left);
        let collapsed = |sl: &ResolvedLayout| sl.sidebar.width_px == COLLAPSED_SIDEBAR_W;
        let left_el = (left_w > 0.0 || left_from > 0.0).then(|| {
            let sl = if left_w > 0.0 { &l } else { sidebar_layout };
            let opts = SidebarOpts { collapsed: collapsed(sl), ..Default::default() };
            self.zone("zone-left", true, left_from, left_w, self.sidebar_card(sl, &infos, window, cx, opts))
        });
        let right_sb_from = Self::sidebar_zone_w(&from, SidebarSide::Right);
        let right_sb_el = (right_sidebar_w > 0.0 || right_sb_from > 0.0).then(|| {
            let sl = if right_sidebar_w > 0.0 { &l } else { sidebar_layout };
            let opts = SidebarOpts { collapsed: collapsed(sl), with_controls: controls_in_sidebar, overlay: false };
            self.zone("zone-rsb", true, right_sb_from, right_sidebar_w, self.sidebar_card(sl, &infos, window, cx, opts))
        });
        let left_visible = left_w > 0.0;
        let right_visible = right_panel_w > 0.0 || right_sidebar_w > 0.0;
        let gutter_left = left_visible.then(|| {
            let shell2 = shell.clone();
            let side = l.sidebar.side;
            let w = l.sidebar.width_px;
            let resizable = !collapsed(&l);
            div().w(px(GUTTER)).h_full().flex_none().when(resizable, |d| {
                d.cursor_col_resize().on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                    let x = f32::from(e.position.x);
                    shell2.update(cx, |s, _| s.start_drag(Drag::Sidebar { side, start_x: x, start_w: w }));
                })
            })
        });
        let gutter_right = (right_sidebar_w > 0.0).then(|| {
            let shell2 = shell.clone();
            let w = l.sidebar.width_px;
            let resizable = !collapsed(&l);
            div().w(px(GUTTER)).h_full().flex_none().when(resizable, |d| {
                d.cursor_col_resize().on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                    let x = f32::from(e.position.x);
                    shell2.update(cx, |s, _| {
                        s.start_drag(Drag::Sidebar { side: SidebarSide::Right, start_x: x, start_w: w })
                    });
                })
            })
        });

        // ---- right panel (docked; the overlay / drawer is drawn over the body below)
        let right_from = self.right_zone_w(&from);
        let right_el = (right_panel_w > 0.0 || right_from > 0.0).then(|| {
            self.zone("zone-right", true, right_from, right_panel_w, self.right_card(window, controls_in_right, m, false, cx))
        });

        // ---- center: title row + panes
        let show_title_row = !l.zen;
        let pad_left = if left_visible { 6.0 } else { m.margin + 4.0 };
        let title_row_el = show_title_row.then(|| self.title_bar(&infos, window, cx, controls_in_center, pad_left));
        let panes = self.pane_area(&infos, m, cx);
        // Zen: while the top bar is revealed the terminal is pushed down by its height, so the bar
        // never covers the first line (the push is instant and only lasts while it is shown).
        let zen_push = if l.zen && (self.reveal.top || self.layout_open || self.menu_open) { TITLEBAR_H } else { 0.0 };
        let center = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(c.bg)
            .children(title_row_el)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .mb(px(m.margin))
                    .ml(px(if left_visible { 0.0 } else { m.margin }))
                    .mr(px(if right_visible { 0.0 } else { m.margin }))
                    .mt(px(if l.zen { m.margin + zen_push } else { 0.0 }))
                    .child(panes),
            );

        // ---- bottom bar
        let bar_mode = if l.bottom_bar != BottomBar::Hidden { l.bottom_bar } else { from.bottom_bar };
        self.status.update(cx, |s, cx| s.set_mode(bar_mode, cx));
        let status_el = (bar_height(l.bottom_bar) > 0.0 || bar_height(from.bottom_bar) > 0.0)
            .then(|| self.zone("zone-status", false, bar_height(from.bottom_bar), bar_height(l.bottom_bar), self.status.clone()));

        // ---- zen reveal overlays, drawers
        let zen_overlays = l.zen.then(|| self.zen_overlays(&infos, window, cx));
        let drawers = self.drawers(&infos, window, m, cx);

        let translucent = self.blurred;
        let mut body = div().relative().flex_1().min_h_0().w_full().flex().flex_row();
        body = body.child(div().h_full().bg(c.bg).child(rail_el));
        if l.sidebar.side != SidebarSide::Right || left_el.is_some() {
            body = body.children(left_el.map(|e| {
                // Transparent only here: the blurred backdrop shows through the glass column.
                div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)
            }));
            body = body.children(gutter_left.map(|g| div().h_full().bg(c.bg).child(g)));
        }
        body = body.child(center);
        body = body.children(right_el.map(|e| div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)));
        if right_sidebar_w > 0.0 || right_sb_from > 0.0 {
            body = body.children(gutter_right.map(|g| div().h_full().bg(c.bg).child(g)));
            body = body.children(right_sb_el.map(|e| div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)));
        }
        body = body.children(drawers);

        let mut key_context = String::from("AppShell");
        if self.confirm_close.is_some() || self.restore_confirm.is_some() {
            key_context.push_str(" CloseConfirm");
        }
        if self.overlay.is_some() {
            key_context.push_str(" Drawer");
        }
        let confirm = self.confirm_close.as_ref().map(|(_, why)| why.clone());
        let hint_el = self.hint.clone().map(|h| {
            let (a, b) = (shell.clone(), shell.clone());
            hint_card(
                &theme,
                &h,
                bar_height(l.bottom_bar) + m.margin + 8.0,
                move |action, _, cx| a.update(cx, |s, cx| s.run_hint_action(action, cx)),
                move |_, cx| b.update(cx, |s, cx| s.dismiss_hint(cx)),
            )
        });
        let frame_stats = cx.try_global::<Services>().map(|s| s.doctor.read(cx).frame_stats());
        div()
            .id("app-shell")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .when(!translucent, |d| d.bg(c.bg))
            .text_color(c.text)
            .font_family(ty.ui_family)
            .key_context(key_context.as_str())
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.open_new_tab_menu(window, cx)))
            .on_action(cx.listener(|this, _: &NewTerminalTab, window, cx| this.new_tab_here(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(id) = this.active_id() {
                    this.request_close(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(-1, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleLayoutPanel, window, cx| this.toggle_layout_panel(window, cx)))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|this, _: &NextAttention, _, cx| this.next_attention(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar_collapsed(cx)))
            .on_action(cx.listener(|this, _: &ui_palette::TogglePalette, window, cx| this.toggle_palette(window, cx)))
            .on_action(cx.listener(|this, _: &CancelOverlay, _, cx| this.close_overlay(cx)))
            .on_action(cx.listener(|this, _: &ConfirmClose, window, cx| {
                if let Some((id, _)) = this.confirm_close.clone() {
                    this.close_tab(id, window, cx);
                } else if this.restore_confirm.is_some() {
                    this.confirm_restore(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CancelClose, _, cx| {
                this.confirm_close = None;
                this.restore_confirm = None;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Jump1, w, cx| this.jump_tab(0, w, cx)))
            .on_action(cx.listener(|this, _: &Jump2, w, cx| this.jump_tab(1, w, cx)))
            .on_action(cx.listener(|this, _: &Jump3, w, cx| this.jump_tab(2, w, cx)))
            .on_action(cx.listener(|this, _: &Jump4, w, cx| this.jump_tab(3, w, cx)))
            .on_action(cx.listener(|this, _: &Jump5, w, cx| this.jump_tab(4, w, cx)))
            .on_action(cx.listener(|this, _: &Jump6, w, cx| this.jump_tab(5, w, cx)))
            .on_action(cx.listener(|this, _: &Jump7, w, cx| this.jump_tab(6, w, cx)))
            .on_action(cx.listener(|this, _: &Jump8, w, cx| this.jump_tab(7, w, cx)))
            .on_action(cx.listener(|this, _: &Jump9, w, cx| this.jump_tab(8, w, cx)))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| this.on_mouse_move(e, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_drag(cx)),
            )
            .child(ui_doctor::frame_begin())
            .child(body)
            .children(status_el.map(|s| div().w_full().bg(c.bg).child(s)))
            .children(zen_overlays)
            .children(hint_el)
            .children(confirm.map(|why| self.confirm_overlay(&why, cx)))
            .children(self.restore_confirm.as_ref().map(|r| self.restore_overlay(r, cx)))
            .children(self.new_tab_menu.then(|| self.new_tab_menu_el(window, cx)))
            // The palette is the last child: an absolute full-window overlay (nothing while closed).
            .child(self.palette.clone())
            .children(frame_stats.map(ui_doctor::frame_end))
            .into_any_element()
    }

    /// The sidebar / right panel as drawers (XS) or overlays (S) over the main area: a subtle scrim
    /// below the titlebar (so the window controls stay reachable) and the panel sliding in from its
    /// edge. Click on the scrim or Esc closes it.
    fn drawers(&self, infos: &[TabInfo], window: &Window, m: Metrics, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(which) = self.overlay else { return Vec::new() };
        let epoch = self.overlay_epoch as usize;
        let w = drawer_width(self.window_w);
        let shell = cx.entity();
        let scrim = div()
            .id("drawer-scrim")
            .absolute()
            .top(px(TITLEBAR_H))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .bg(Hsla { h: 0.0, s: 0.0, l: 0.0, a: 0.28 })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| shell.update(cx, |s, cx| s.close_overlay(cx)))
            .into_any_element();
        let animation = || Animation::new(BASE).with_easing(|t| SHEET.solve(t).clamp(0.0, 1.0));
        let panel = match which {
            Overlay::Left => {
                let x = if self.eff.layout.rail == RailVisibility::Visible { RAIL_W } else { 0.0 };
                let mut sl = self.layout.clone();
                sl.sidebar.side = SidebarSide::Left;
                sl.sidebar.width_px = w as u16;
                sl.rail = self.eff.layout.rail;
                sl.zen = false;
                let total = w + m.margin;
                div()
                    .id("drawer-left")
                    .bg(self.theme.colors.bg)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(x))
                    .w(px(total))
                    .occlude()
                    .child(self.sidebar_card(&sl, infos, window, cx, SidebarOpts { overlay: true, ..Default::default() }))
                    .with_animation(ElementId::from(("drawer-in-left", epoch)), animation(), move |el, t| {
                        el.left(px(x - total * (1.0 - t)))
                    })
                    .into_any_element()
            },
            Overlay::Right => {
                let total = w + GUTTER + m.margin;
                div()
                    .id("drawer-right")
                    .bg(self.theme.colors.bg)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(total))
                    .occlude()
                    .child(self.right_card(window, true, m, true, cx))
                    .with_animation(ElementId::from(("drawer-in-right", epoch)), animation(), move |el, t| {
                        el.right(px(-total * (1.0 - t)))
                    })
                    .into_any_element()
            },
        };
        vec![scrim, panel]
    }

    /// Zen mode: invisible hover strips at the top and left edges reveal the tab bar (with the
    /// window controls) and a floating sidebar. They hide again when the pointer leaves them, and
    /// stay while the layout popover is open. The top bar pushes the terminal down while shown
    /// (see `render`), the floating sidebar is glass over it.
    fn zen_overlays(&mut self, infos: &[TabInfo], window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let shell = cx.entity();
        let reveal = self.reveal;
        let keep = self.layout_open || self.menu_open || self.tabs_menu_open;
        let strip = |edge: &'static str, top: bool| {
            let shell = shell.clone();
            let d = div().id(edge).absolute();
            let d = if top {
                d.top_0().left_0().right_0().h(px(EDGE_STRIP))
            } else {
                d.top(px(TITLEBAR_H)).left_0().bottom_0().w(px(EDGE_STRIP))
            };
            d.on_hover(move |hovered, _, cx| {
                if *hovered {
                    shell.update(cx, |s, cx| {
                        if top {
                            s.reveal.top = true;
                        } else {
                            s.reveal.left = true;
                        }
                        cx.notify();
                    });
                }
            })
        };
        let hide = |top: bool| {
            let shell = shell.clone();
            move |hovered: &bool, _: &mut Window, cx: &mut App| {
                if !*hovered {
                    shell.update(cx, |s, cx| {
                        if top && !(s.layout_open || s.menu_open || s.tabs_menu_open) {
                            s.reveal.top = false;
                        } else if !top {
                            s.reveal.left = false;
                        }
                        cx.notify();
                    });
                }
            }
        };

        let top_bar = if reveal.top || keep {
            let mut bar = self.title_bar(infos, window, cx, true, 12.0);
            bar = bar.bg(c.bg);
            div()
                .id("zen-top")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .occlude()
                .on_hover(hide(true))
                .child(bar)
                .into_any_element()
        } else {
            strip("zen-top-strip", true).into_any_element()
        };

        let left_panel = if reveal.left {
            let sl = ResolvedLayout {
                tabs_position: TabsPosition::Sidebar,
                sidebar: SidebarLayout {
                    side: SidebarSide::Left,
                    width_px: ZEN_SIDEBAR_W as u16,
                    sections: vec![SidebarSection::Agents],
                    group_by: store::GroupBy::None,
                    ..SidebarLayout::default()
                },
                ..self.layout.clone()
            };
            let tint = self.overlay_bg();
            div()
                .id("zen-left")
                .absolute()
                .top(px(TITLEBAR_H))
                .left_0()
                .bottom_0()
                .w(px(ZEN_SIDEBAR_W + 8.0))
                .occlude()
                .on_hover(hide(false))
                .py(px(8.0))
                .pl(px(8.0))
                .child(
                    div()
                        .size_full()
                        .rounded(px(Radius::MD))
                        .bg(tint)
                        .overflow_hidden()
                        .child(sidebar_content(&SidebarProps {
                            theme: &self.theme,
                            layout: &sl,
                            tabs: infos,
                            active: self.active_id(),
                            shell: &shell,
                            hovered_row: self.hovered_row,
                            glass: true,
                        })),
                )
                .into_any_element()
        } else {
            strip("zen-left-strip", false).into_any_element()
        };

        div().absolute().size_full().child(left_panel).child(top_bar)
    }

    // ------------------------------------------------------------------------------------
    // Git panel, file viewer, terminal requests
    // ------------------------------------------------------------------------------------

    /// The user is looking at the active tab: whatever it had in the attention queue is seen.
    fn clear_attention_of_active(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id() else { return };
        if let Some(services) = cx.try_global::<Services>().cloned() {
            let target = FocusTarget::new(id.as_u64().to_string());
            services.attention.update(cx, |m, cx| m.remove_target(&target, cx));
        }
    }

    fn focus_attention_target(&mut self, target: &FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        if let Some(id) = self.tab_ids().into_iter().find(|id| id.as_u64().to_string() == target.0.as_ref()) {
            self.select_tab(id, window, cx);
        }
    }

    /// Ctrl+Shift+A: jump to the next thing that needs attention.
    fn next_attention(&mut self, cx: &mut Context<Self>) {
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services.attention.update(cx, |m, cx| {
                m.jump_next(cx);
            });
        }
    }

    /// Opens a review tab (diff with line notes) of the active tab's project, or brings the one
    /// that already shows it forward.
    fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let info = self.active_info(cx);
        let Some(root) = info.as_ref().and_then(|i| i.project_root.clone().or_else(|| i.cwd.clone())) else {
            self.show_hint(Hint::text("No hay un proyecto abierto que revisar"), cx);
            return;
        };
        let existing = self.tabs.iter().find_map(|t| {
            let tab = t.handle.downcast::<ReviewTab>()?;
            matches!(tab.read(cx).source(), ReviewSource::Worktree { root: r } if *r == root).then(|| t.handle.id())
        });
        if let Some(id) = existing {
            self.select_tab(id, window, cx);
            return;
        }
        let theme = self.theme;
        let entity = cx.new(|cx| ReviewTab::new(ReviewSource::Worktree { root }, theme, cx));
        let handle = TabHandle::new(entity.clone());
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        let tab_id = handle.id();
        let events = cx.subscribe_in(&entity, window, move |this, tab, ev: &ReviewEvent, window, cx| match ev {
            ReviewEvent::SendToAgent { markdown, notes } => {
                // The notes go through the redactor like any other context sent to an agent.
                let (clean, hidden) = term_core::redact(markdown);
                let cwd = tab.read(cx).source().root().to_path_buf();
                let ids: Vec<_> = notes.iter().map(|n| n.id).collect();
                this.send_to_agent(clean, hidden, Some(cwd), window, cx);
                let view = tab.read(cx).view().clone();
                view.update(cx, |v, cx| v.mark_sent(&ids, cx));
            },
            ReviewEvent::NotesChanged { .. } => {},
        });
        self.tabs.push(TabSlot { handle, _subs: vec![sub, events] });
        self.panes.open(tab_id);
        self.scroll_active = true;
        self.after_active_changed(window, cx);
    }

    /// Points the file tree's `M` / `A` / `?` markers at the active terminal's repository, and
    /// keeps them current while that repository's monitor reports changes.
    fn sync_git_marks(&mut self, cx: &mut Context<Self>) {
        let git: Option<Entity<GitMonitor>> = self
            .active_id()
            .and_then(|id| self.slot(id))
            .and_then(|s| s.handle.downcast::<TerminalTab>())
            .map(|t| t.read(cx).git().clone());
        // A viewer tab in front keeps the last markers (the tree still shows that project).
        let Some(git) = git else { return };
        self.marks_sub = Some(cx.observe(&git, |this, git, cx| this.feed_git_marks(&git, cx)));
        self.feed_git_marks(&git, cx);
    }

    fn feed_git_marks(&mut self, git: &Entity<GitMonitor>, cx: &mut Context<Self>) {
        let Some(info) = git.read(cx).info().cloned() else { return };
        let Some(services) = cx.try_global::<Services>().cloned() else { return };
        services.files.set_git_status(git_marks(&info), cx);
    }

    /// A git operation of the panel ended: the status bar and the tree re-read the repository.
    fn after_git_operation(&mut self, cx: &mut Context<Self>) {
        let git: Option<Entity<GitMonitor>> = self
            .active_id()
            .and_then(|id| self.slot(id))
            .and_then(|s| s.handle.downcast::<TerminalTab>())
            .map(|t| t.read(cx).git().clone());
        if let Some(git) = git {
            git.update(cx, |g, cx| g.refresh(cx));
        }
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services.files.refresh(cx);
        }
    }

    fn on_git_event(&mut self, event: GitPanelEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GitPanelEvent::RunInTerminal { argv, cwd } => self.run_in_terminal(&argv, &cwd, window, cx),
            GitPanelEvent::OpenFile { path, line } => self.open_viewer(path, line, false, None, window, cx),
            GitPanelEvent::OpenDiff { path } => self.open_viewer(path, None, true, None, window, cx),
            GitPanelEvent::AskAgent { context_markdown } => {
                // Backlog A13: nothing leaves the machine without going through the redactor.
                let (clean, hidden) = term_core::redact(&context_markdown);
                let cwd = cx.try_global::<Services>().and_then(|s| s.git.read(cx).root().map(PathBuf::from));
                self.send_to_agent(clean, hidden, cwd, window, cx);
            },
            GitPanelEvent::WorktreeSelected { path } => {
                let text = format!("Worktree {}", paths::tab_label(&path));
                self.show_hint(Hint::text(text).with_action("cd aquí", HintAction::Cd(path)), cx);
            },
        }
    }

    /// Types a confirmed git command in the active terminal (or a new one when the active tab is
    /// not a terminal). `-C` targets the panel's worktree without changing the shell's directory.
    fn run_in_terminal(&mut self, argv: &[String], cwd: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_cwd(cx);
        let line = palette::shell_line(&palette::with_dash_c(argv, cwd, active.as_deref()));
        if self.active_terminal().is_none() {
            self.open_tab(Some(cwd.to_path_buf()), window, cx);
        }
        self.type_in_active(&line, true, cx);
    }

    fn active_terminal(&self) -> Option<gpui::Entity<TerminalTab>> {
        self.active_id().and_then(|id| self.slot(id)).and_then(|s| s.handle.downcast::<TerminalTab>())
    }

    /// Opens a new terminal running `claude`, with the (already redacted) context copied to the
    /// clipboard: until the chat exists, the user pastes it in.
    fn send_to_agent(
        &mut self,
        markdown: String,
        redactions: usize,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(markdown));
        let cwd = cwd.or_else(|| self.active_cwd(cx));
        self.open_tab(cwd, window, cx);
        self.type_in_active("claude", true, cx);
        let secrets = if redactions > 0 { format!(" ({})", crate::block_ui::secrets_hint(redactions)) } else { String::new() };
        self.show_hint(Hint::text(format!("Contexto copiado{secrets} · pégalo en el agente con Ctrl+V")), cx);
    }

    fn on_term_request(&mut self, tab: TabId, req: TermRequest, window: &mut Window, cx: &mut Context<Self>) {
        match req {
            TermRequest::OpenPath { path, line, is_dir, .. } => {
                if is_dir {
                    if let Some(t) = self.slot(tab).and_then(|s| s.handle.downcast::<TerminalTab>()) {
                        let line = palette::cd_line(&path);
                        t.update(cx, |t, cx| t.input(&line, true, cx));
                    }
                } else {
                    self.open_viewer(path, line.map(|l| l as usize), false, None, window, cx);
                }
            },
            TermRequest::Attention { id, label, kind } => {
                // Nothing to flag when the user is looking at that tab right now.
                if self.active_id() == Some(tab) && notify::window_active() {
                    return;
                }
                let Some(services) = cx.try_global::<Services>().cloned() else { return };
                let item = AttentionItem {
                    id: id.into(),
                    owner_label: label.into(),
                    agent: None,
                    kind,
                    since: Instant::now(),
                    focus_target: FocusTarget::new(tab.as_u64().to_string()),
                };
                services.attention.update(cx, |m, cx| m.upsert(item, cx));
            },
            TermRequest::AskAgent { markdown, redactions } => {
                let cwd = self.slot(tab).and_then(|s| s.handle.info(cx).cwd);
                self.send_to_agent(markdown, redactions, cwd, window, cx);
            },
        }
    }

    fn on_viewer_event(&mut self, event: ViewerEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ViewerEvent::OpenFile { path } => self.open_viewer(path, None, false, None, window, cx),
            ViewerEvent::OpenDiff { path, base } => self.open_viewer(path, None, true, base, window, cx),
            ViewerEvent::OpenInEditor { path, line } => {
                let editor = self.config.git.external_editor.clone();
                cx.spawn(async move |this, cx| {
                    let res =
                        cx.background_executor().spawn(async move { viewer_host::spawn_editor(&path, line, &editor) }).await;
                    if let Err(e) = res {
                        this.update(cx, |shell, cx| shell.show_hint(Hint::text(e), cx)).ok();
                    }
                })
                .detach();
            },
            // The banner of a file without highlighting: Ajustes > Plugins with that grammar open.
            ViewerEvent::InstallGrammar { id } => {
                self.open_settings(window, cx);
                if let Some(settings) = self.settings.clone() {
                    settings.update(cx, |s, cx| s.handle(&ui_settings::SettingsEvent::InstallGrammar { id }, cx));
                }
            },
        }
    }

    /// Opens `path` in a viewer tab of the main zone (or brings the one that shows it forward).
    /// `base`: a checkpoint commit for diffs (HEAD when `None`).
    fn open_viewer(
        &mut self,
        path: PathBuf,
        line: Option<usize>,
        diff: bool,
        base: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.tabs.iter().find_map(|t| {
            let tab = t.handle.downcast::<ViewerTab>()?;
            let same = tab.read(cx).path() == path.as_path()
                && (tab.read(cx).viewer().read(cx).body() == ui_viewer::Body::Diff) == diff;
            same.then(|| (t.handle.id(), tab))
        });
        if let Some((id, tab)) = existing {
            if let Some(line) = line {
                tab.read(cx).viewer().clone().update(cx, |v, cx| v.goto_line(line, cx));
            }
            self.select_tab(id, window, cx);
            return;
        }
        let root = ui_viewer::find_repo_root(&path).or_else(|| self.active_info(cx).and_then(|i| i.project_root));
        let mut cfg = ViewerConfig { project_root: root, line, open_in_diff: diff, ..ViewerConfig::default() };
        if let Some(sha) = base {
            cfg.base_label = Some(format!("checkpoint {}", sha.chars().take(7).collect::<String>()).into());
            cfg.diff_base = viewer_svc::GitBase::Commit(sha);
        }
        let theme = self.theme;
        let entity = cx.new(|cx| ViewerTab::new(path, cfg, theme, cx));
        let handle = TabHandle::new(entity.clone());
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        let events = cx.subscribe_in(&entity, window, |this, _, ev: &ViewerEvent, window, cx| {
            this.on_viewer_event(ev.clone(), window, cx);
        });
        let id = handle.id();
        self.tabs.push(TabSlot { handle, _subs: vec![sub, events] });
        self.panes.open(id);
        self.scroll_active = true;
        self.after_active_changed(window, cx);
    }

    // ------------------------------------------------------------------------------------
    // Inline hint
    // ------------------------------------------------------------------------------------

    /// Shows `hint` above the status bar for a few seconds (replacing the previous one).
    fn show_hint(&mut self, hint: Hint, cx: &mut Context<Self>) {
        self.hint = Some(hint);
        // Replacing the task restarts the one-shot timer.
        self.hint_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HINT_FOR).await;
            this.update(cx, |shell, cx| {
                shell.hint = None;
                shell.hint_task = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn dismiss_hint(&mut self, cx: &mut Context<Self>) {
        if self.hint.take().is_some() {
            self.hint_task = None;
            cx.notify();
        }
    }

    fn run_hint_action(&mut self, action: HintAction, cx: &mut Context<Self>) {
        match action {
            HintAction::Cd(dir) => self.type_in_active(&palette::cd_line(&dir), true, cx),
        }
        self.dismiss_hint(cx);
    }

    // ------------------------------------------------------------------------------------
    // Command palette
    // ------------------------------------------------------------------------------------

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let info = self.active_info(cx);
        let ctx = ui_palette::OpenContext {
            cwd: info.as_ref().and_then(|i| i.cwd.clone()),
            project_root: info.and_then(|i| i.project_root),
            tabs: self.infos(cx),
            active_tab: self.active_id(),
            initial: String::new(),
        };
        self.palette.update(cx, |p, cx| p.toggle(ctx, window, cx));
    }

    /// Types `text` in the active terminal.
    fn type_in_active(&mut self, text: &str, enter: bool, cx: &mut Context<Self>) {
        let tab = self.active_id().and_then(|id| self.slot(id)).and_then(|s| s.handle.downcast::<TerminalTab>());
        if let Some(tab) = tab {
            tab.update(cx, |t, cx| t.input(text, enter, cx));
        }
    }

    /// Handles what the palette picked (it emits and closes; focus is already back on the terminal).
    fn on_palette(&mut self, event: ui_palette::PaletteEvent, window: &mut Window, cx: &mut Context<Self>) {
        use ui_palette::PaletteEvent as E;
        match event {
            E::RunCommand { line, cwd, new_tab } => {
                if new_tab {
                    self.open_tab(cwd, window, cx);
                } else if let Some(dir) = cwd.filter(|d| Some(d) != self.active_cwd(cx).as_ref()) {
                    self.type_in_active(&palette::cd_line(&dir), true, cx);
                }
                self.type_in_active(&line, true, cx);
            },
            E::InsertText(text) => self.type_in_active(&text, false, cx),
            E::OpenPath { path, new_tab } => {
                if new_tab {
                    self.open_tab(Some(path), window, cx);
                } else {
                    self.type_in_active(&palette::cd_line(&path), true, cx);
                }
            },
            E::FocusTab(id) => self.select_tab(id, window, cx),
            E::ApplyPreset(preset) => {
                let mut layout = self.config.layout.clone();
                layout.select_preset(preset);
                self.set_layout_config(layout, true, cx);
            },
            E::GitAction(action) => {
                // The command is typed where the user can see it. Sensitive ones (commit, push,
                // pull, switch...) are left at the prompt without Enter: pressing it is the
                // confirmation.
                let line = palette::shell_line(&action.argv());
                let sensitive = action.is_sensitive();
                self.type_in_active(&line, !sensitive, cx);
            },
            E::Action(id) => self.run_action(id.as_ref(), window, cx),
            E::Closed => {
                // Focus went back to what had it; make sure that is the terminal.
                if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
                    slot.handle.focus(window, cx);
                }
            },
        }
    }

    fn run_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            action_id::TAB_NEW => self.open_new_tab_menu(window, cx),
            action_id::TAB_NEW_TERMINAL => self.new_tab_here(window, cx),
            action_id::CHAT_NEW => {
                let project = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd));
                self.new_chat_tab(project, window, cx);
            },
            action_id::TAB_CLOSE => {
                if let Some(t) = self.active_id() {
                    self.request_close(t, window, cx);
                }
            },
            action_id::TAB_NEXT => self.cycle_tab(1, window, cx),
            action_id::TAB_PREV => self.cycle_tab(-1, window, cx),
            action_id::LAYOUT_TOGGLE => self.toggle_layout_panel(window, cx),
            action_id::THEME_TOGGLE => self.toggle_theme(&ToggleTheme, window, cx),
            action_id::FULLSCREEN => window.toggle_fullscreen(),
            action_id::REVIEW_OPEN => self.open_review(window, cx),
            action_id::ATTENTION_NEXT => self.next_attention(cx),
            action_id::SETTINGS_OPEN => self.open_settings(window, cx),
            action_id::SIDEBAR_TOGGLE => self.toggle_sidebar_collapsed(cx),
            other => {
                if let Some(panel) = other.strip_prefix(action_id::PANEL_PREFIX) {
                    self.toggle_right_panel(PanelId::new(panel), cx);
                } else {
                    log::warn!("unknown palette action {other}");
                }
            },
        }
    }

    /// Sidebar overlay: the collapsed (S) column's "expand" button.
    pub fn toggle_sidebar_overlay(&mut self, cx: &mut Context<Self>) {
        self.toggle_overlay(Overlay::Left, cx);
    }

    // ------------------------------------------------------------------------------------
    // Shell v2: spaces, sidebar collapse, settings, first-run wizard
    // ------------------------------------------------------------------------------------

    /// Folds or unfolds a space of the unified sidebar.
    pub(crate) fn toggle_space(&mut self, key: PathBuf, cx: &mut Context<Self>) {
        if !self.folded_spaces.remove(&key) {
            self.folded_spaces.insert(key);
        }
        cx.notify();
    }

    /// "+" of a space (or a click on a project without tabs): a new terminal in its folder.
    pub(crate) fn new_tab_in(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(db) = &self.session_db {
            let _ = db.mark_project_opened(&root.to_string_lossy());
        }
        self.folded_spaces.remove(&root);
        self.open_tab(Some(root), window, cx);
    }

    /// "Buscar": the command palette.
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_palette(window, cx);
    }

    /// Ctrl+B / the chevron of the sidebar: icons only and back. Where the sidebar is not docked
    /// (S, XS) it opens the overlay / drawer instead.
    pub(crate) fn toggle_sidebar_collapsed(&mut self, cx: &mut Context<Self>) {
        if self.eff.breakpoint <= Breakpoint::S {
            if self.eff.sidebar != SidebarMode::Off {
                self.toggle_overlay(Overlay::Left, cx);
            }
            return;
        }
        if self.eff.layout.sidebar.side == SidebarSide::Hidden {
            return;
        }
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.apply_adapted(true, cx);
        self.mark_session_dirty(cx);
        cx.notify();
    }

    /// The drop target of the file tree / viewer: opens the dropped files in the viewer.
    fn viewer_drop_zone(&self, id: &'static str, radius: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.entity();
        ui_kit::DropZone::new(id, &self.theme, "Soltar para abrir en el visor").radius(radius).on_drop(move |paths, window, cx| {
            let paths = paths.to_vec();
            shell.update(cx, |s, cx| s.drop_on_viewer(&paths, window, cx));
        })
    }

    /// Files dropped on the file tree / a viewer tab: each file opens in the viewer (a folder is
    /// not a file: it is ignored here, drop it on a space of the sidebar to work in it).
    pub(crate) fn drop_on_viewer(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        for f in crate::drop::split_for_viewer(paths).files {
            self.open_viewer(f, None, false, None, window, cx);
        }
    }

    /// A folder dropped on a space row of the sidebar: a new terminal tab in it.
    pub(crate) fn drop_on_space(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dir) = crate::drop::folder_to_open(paths) {
            self.new_tab_in(dir, window, cx);
        }
    }

    /// The pointer entered / left a tab row of the unified sidebar. Repaints only on a change.
    pub(crate) fn set_row_hover(&mut self, id: TabId, hovered: bool, cx: &mut Context<Self>) {
        let next = crate::unified_sidebar::hover_transition(self.hovered_row, id, hovered);
        if next != self.hovered_row {
            self.hovered_row = next;
            cx.notify();
        }
    }

    /// The pointer entered / left a space header of the unified sidebar.
    pub(crate) fn set_space_hover(&mut self, ix: usize, hovered: bool, cx: &mut Context<Self>) {
        let next = crate::unified_sidebar::hover_transition(self.hovered_space, ix, hovered);
        if next != self.hovered_space {
            self.hovered_space = next;
            cx.notify();
        }
    }

    /// Branch and dirty flag of every repository the open terminals are in.
    fn space_git(&self, cx: &App) -> HashMap<PathBuf, spaces::GitSummary> {
        self.tabs
            .iter()
            .filter_map(|t| t.handle.downcast::<TerminalTab>())
            .filter_map(|t| t.read(cx).git().read(cx).info().map(|i| (i.root.clone(), (i.branch.clone(), i.dirty))))
            .collect()
    }

    /// The footer meter: the official 5 h (else weekly) figure when Claude Code reported one,
    /// otherwise an empty meter labelled "estimado" (nothing is invented).
    fn usage_summary(&self, cx: &App) -> Usage {
        match self.usage.read(cx).info() {
            Some(u) => match u.fresh_five_hour().or_else(|| u.fresh_official()) {
                Some(o) => Usage {
                    fraction: Some((o.percent / 100.0).clamp(0.0, 1.0) as f32),
                    label: format!("{:.0} %", o.percent).into(),
                },
                None => Usage { fraction: None, label: "estimado".into() },
            },
            None => Usage { fraction: None, label: "…".into() },
        }
    }

    /// Reads the favorite and recent projects off the UI thread; the sidebar lists them as spaces.
    fn load_known_projects(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.session_db.clone() else { return };
        self._projects_task = Some(cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move {
                    let mut out: Vec<(PathBuf, String)> = Vec::new();
                    for f in db.favorites().unwrap_or_default() {
                        out.push((PathBuf::from(f.path), f.label));
                    }
                    let mut recent: Vec<_> = db.projects().unwrap_or_default().into_iter().filter(|p| p.last_opened.is_some()).collect();
                    recent.sort_by_key(|p| std::cmp::Reverse(p.last_opened));
                    for p in recent.into_iter().take(spaces::MAX_CLOSED_SPACES) {
                        out.push((PathBuf::from(p.path), p.name));
                    }
                    out
                })
                .await;
            this.update(cx, |shell, cx| {
                if shell.known_projects != list {
                    shell.known_projects = list;
                    shell.sync_chat_projects(cx);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Opens the settings tab (creating the view the first time), or brings it forward.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.onboarding.is_some() {
            return;
        }
        let existing = self.tabs.iter().find(|t| t.handle.downcast::<SettingsTab>().is_some()).map(|t| t.handle.id());
        if let Some(id) = existing {
            self.select_tab(id, window, cx);
            return;
        }
        let view = self.ensure_settings(window, cx);
        let info = self.active_info(cx);
        let ctx = PanelCtx {
            zone: PanelZone::Main,
            width_class: WidthClass::Regular,
            cwd: info.as_ref().and_then(|i| i.cwd.clone()),
            project_root: info.and_then(|i| i.project_root),
            theme: self.theme,
        };
        let Some(provider) = cx.try_global::<PanelRegistry>().and_then(|r| r.get(&PanelId::new(ui_settings::PANEL_ID))) else {
            return;
        };
        let content = provider.build(ctx, window, cx);
        let entity = cx.new(|_| SettingsTab::new(view, content));
        let handle = TabHandle::new(entity);
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        let id = handle.id();
        self.tabs.push(TabSlot { handle, _subs: vec![sub] });
        self.panes.open(id);
        self.scroll_active = true;
        self.after_active_changed(window, cx);
    }

    /// Mounts the settings: its view, the `settings` panel provider and the event wiring.
    fn ensure_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ui_settings::SettingsView> {
        if let Some(v) = &self.settings {
            return v.clone();
        }
        let path = store::paths::config_path().unwrap_or_default();
        let options = ui_settings::SettingsOptions::new(path, env!("CARGO_PKG_VERSION"));
        let view = ui_settings::settings_view((*self.config).clone(), self.theme, options, cx);
        let provider = ui_settings::SettingsPanelProvider::new(view.clone());
        cx.update_global::<PanelRegistry, _>(|registry, _| registry.register(provider));
        self._settings_sub = Some(cx.subscribe_in(&view, window, |this, _, ev: &ui_settings::SettingsEvent, window, cx| {
            this.on_settings_event(ev.clone(), window, cx);
        }));
        self.settings = Some(view.clone());
        view
    }

    fn on_settings_event(&mut self, event: ui_settings::SettingsEvent, window: &mut Window, cx: &mut Context<Self>) {
        use ui_settings::SettingsEvent as E;
        match event {
            E::ConfigChanged(config) => self.adopt_config(config, cx),
            E::OpenLayoutPanel => {
                if !self.layout_open {
                    self.toggle_layout_panel(window, cx);
                }
            },
            E::RerunOnboarding => self.start_onboarding(window, cx),
            E::PluginInstalled { id, restart } => {
                let tail = if restart { " (se activa al reiniciar)" } else { "" };
                self.show_hint(Hint::text(format!("Plugin {id} instalado{tail}")), cx);
            },
            E::PluginRemoved { id, restart } => {
                let tail = if restart { " (se quita al reiniciar)" } else { "" };
                self.show_hint(Hint::text(format!("Plugin {id} quitado{tail}")), cx);
            },
            E::SaveFailed(msg) => self.show_hint(Hint::text(format!("No se pudo guardar config.toml: {msg}")), cx),
            E::Saved | E::InstallGrammar { .. } => {},
        }
    }

    /// Shows the wizard instead of the shell (first run, or "Volver a configurar").
    fn start_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.onboarding.is_some() {
            return;
        }
        let view = ui_onboarding::onboarding((*self.config).clone(), self.session_db.clone(), self.theme, cx);
        self._onboarding_sub = Some(cx.subscribe_in(&view, window, |this, _, ev: &ui_onboarding::OnboardingEvent, window, cx| {
            this.on_onboarding_event(ev.clone(), window, cx);
        }));
        view.read(cx).focus(window);
        self.onboarding = Some(view);
        cx.notify();
    }

    fn on_onboarding_event(&mut self, event: ui_onboarding::OnboardingEvent, window: &mut Window, cx: &mut Context<Self>) {
        use ui_onboarding::OnboardingEvent as E;
        match event {
            E::Finished { config, .. } => {
                self.end_onboarding(window, cx);
                // The wizard already saved config.toml; adopt it (a "System" theme is dark here).
                self.adopt_config(Arc::new(config.clone()), cx);
                self.set_theme(theme_for(&config), cx);
                if let Some(settings) = self.settings.clone() {
                    settings.update(cx, |s, cx| s.set_config(config, cx));
                }
                self.load_known_projects(cx);
            },
            E::Skipped => self.end_onboarding(window, cx),
            E::OpenLoginTerminal { command, .. } => {
                let cwd = self.active_cwd(cx);
                self.open_tab(cwd, window, cx);
                self.type_in_active(&command, true, cx);
            },
            // A live preview: the window follows the choice, the config is untouched.
            E::ThemePreview(mode) => self.set_theme(themed(mode, &self.config), cx),
        }
    }

    fn end_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.onboarding = None;
        self._onboarding_sub = None;
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
        cx.notify();
    }

    // ------------------------------------------------------------------------------------
    // Process tracking without a job
    // ------------------------------------------------------------------------------------

    /// While tree-tracked owners exist (a shell Windows refused to put in a job, such as the
    /// Microsoft Store `pwsh`) **and** the procs panel is open and the window focused, refreshes
    /// them every `proc_svc::TREE_POLL_INTERVAL`. The task ends by itself as soon as one of those
    /// stops being true, so nothing ticks at rest.
    fn sync_tree_poll(&mut self, cx: &mut Context<Self>) {
        if self.tree_poll.is_some() || !self.right_panel_active(&PanelId::new(ui_procs::PANEL_ID)) {
            return;
        }
        let Some(services) = cx.try_global::<Services>().cloned() else { return };
        let tracker = services.tracker.clone();
        if !tracker.lock().unwrap_or_else(|p| p.into_inner()).needs_polling() {
            return;
        }
        let procs = services.procs;
        self.tree_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(proc_svc::TREE_POLL_INTERVAL).await;
                let t = tracker.clone();
                let more = cx
                    .background_executor()
                    .spawn(async move { t.lock().unwrap_or_else(|p| p.into_inner()).poll_tree() })
                    .await;
                let wanted = this
                    .update(cx, |shell, cx| {
                        procs.update(cx, |_, cx| cx.notify());
                        shell.right_panel_active(&PanelId::new(ui_procs::PANEL_ID)) && notify::window_active()
                    })
                    .unwrap_or(false);
                if !more || !wanted {
                    break;
                }
            }
            this.update(cx, |shell, _| shell.tree_poll = None).ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_cycling_wraps() {
        assert_eq!(cycle_index(3, 2, 1), 0);
        assert_eq!(cycle_index(3, 0, -1), 2);
        assert_eq!(cycle_index(3, 1, 1), 2);
        assert_eq!(cycle_index(0, 0, 1), 0);
        assert_eq!(cycle_index(1, 0, 1), 0);
    }

    #[test]
    fn sidebar_drag_clamps_and_flips_on_the_right() {
        assert_eq!(dragged_width(SidebarSide::Left, 240, 40.0), 280);
        assert_eq!(dragged_width(SidebarSide::Right, 240, 40.0), 200);
        assert_eq!(dragged_width(SidebarSide::Left, 240, -500.0), 180);
        assert_eq!(dragged_width(SidebarSide::Left, 240, 900.0), 480);
    }

    #[test]
    fn right_panel_drag_grows_leftwards_and_clamps() {
        assert_eq!(dragged_right_width(360.0, -40.0), 400.0);
        assert_eq!(dragged_right_width(360.0, 40.0), 320.0);
        assert_eq!(dragged_right_width(360.0, 900.0), 240.0);
        assert_eq!(dragged_right_width(360.0, -900.0), 520.0);
    }

    #[test]
    fn drawers_never_exceed_the_window() {
        assert_eq!(drawer_width(1000.0), 320.0);
        assert_eq!(drawer_width(300.0), 270.0);
    }

    #[test]
    fn zone_widths_follow_the_layout() {
        use store::BuiltinPreset as B;
        let term = ResolvedLayout::builtin(B::Terminal);
        assert_eq!(AppShell::sidebar_zone_w(&term, SidebarSide::Left), 0.0);
        assert_eq!(AppShell::rail_w(&term), RAIL_W);
        let orq = ResolvedLayout::builtin(B::Orquestador);
        assert_eq!(AppShell::sidebar_zone_w(&orq, SidebarSide::Left), 300.0 + 4.0);
        assert_eq!(AppShell::sidebar_zone_w(&orq, SidebarSide::Right), 0.0);
        let zen = ResolvedLayout::builtin(B::Zen);
        assert_eq!(AppShell::rail_w(&zen), 0.0);
        let chat = ResolvedLayout::builtin(B::Chat);
        assert_eq!(AppShell::rail_w(&chat), 0.0);
    }

    #[test]
    fn unified_layout_dedupes_usage_and_never_gets_the_full_bar() {
        use store::BuiltinPreset as B;
        let mut l = ResolvedLayout::builtin(B::EspaciosUnificados);
        l.bottom_bar = BottomBar::Full;
        let eff = adapt_for_shell(&l, 1440.0, false);
        assert!(dedupes_usage(&eff));
        assert_eq!(eff.layout.bottom_bar, BottomBar::Compact);
        // Icon-only column or a narrow window: the footer meter is not on screen, the pill returns.
        assert!(!dedupes_usage(&adapt_for_shell(&l, 1440.0, true)));
        assert!(!dedupes_usage(&adapt_for_shell(&l, 800.0, false)));
        // The classic presets keep their bar and their usage pill.
        let mut orq = ResolvedLayout::builtin(B::Orquestador);
        orq.bottom_bar = BottomBar::Full;
        let eff = adapt_for_shell(&orq, 1440.0, false);
        assert!(!dedupes_usage(&eff));
        assert_eq!(eff.layout.bottom_bar, BottomBar::Full);
    }

    #[test]
    fn unified_preset_shows_the_panel_buttons_once() {
        let l = ResolvedLayout::builtin(store::BuiltinPreset::EspaciosUnificados);
        // The titlebar holds them; the sidebar's icon row stays off unless the user asks for it.
        assert!(!l.titlebar.panel_buttons.is_empty());
        assert!(!l.sidebar.panel_icons);
    }

    #[test]
    fn pane_counts_match_the_layouts() {
        assert_eq!(pane_count(Splits::Grid2x2), 4);
    }
}
