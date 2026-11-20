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
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Bounds, Context, Div, ElementId, Entity, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString,
    Subscription, Task, WeakEntity, Window, WindowBackgroundAppearance, actions, canvas, div,
    prelude::*, px, relative,
};
use panel_api::{
    CloseCheck, PanelCtx, PanelId, PanelRegistry, PanelZone, TabHandle, TabId, TabInfo, WidthClass,
};
use store::{
    BottomBar, Config, ConfigEvent, ConfigWatcher, Density, LayoutConfig, RailVisibility, ResolvedLayout,
    SidebarLayout, SidebarSection, SidebarSide, Splits, TabsPosition, layout_changed,
};
use theme::{Mode, Radius, Theme, motion::SHEET, motion::duration::BASE};
use ui_kit::{Button, Card, IconButton, Icon, Placement, Popover, icon};

use crate::git_monitor::GitMonitor;
use crate::layout_host::{LayoutPanelArgs, build_layout_panel};
use crate::paths;
use crate::rail::rail;
use crate::sidebar::{SidebarProps, sidebar_content};
use crate::splits::{PaneSet, clamp_ratio, pane_count, ratio_at};
use crate::status_bar::{StatusBar, bar_height};
use crate::terminal_tab::TerminalTab;
use crate::titlebar::{TITLEBAR_H, breadcrumb, new_tab_button, tab_pill, title_row, window_controls};
use crate::usage_monitor::UsageMonitor;

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
const EDGE_STRIP: f32 = 5.0;
/// How long the zone transition runs (the animation itself is [`BASE`]); the transition state is
/// dropped on the first render after this.
const TRANSITION_KEEP: Duration = Duration::from_millis(260);
/// Quiet time before the layout is written to `config.toml`.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

actions!(
    selachi,
    [
        ToggleTheme,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleLayoutPanel,
        ConfirmClose,
        CancelClose,
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
    _sub: Subscription,
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

pub struct AppShell {
    theme: Theme,
    config: Arc<Config>,
    layout: ResolvedLayout,
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
    layout_open: bool,
    layout_panel: Option<AnyView>,
    confirm_close: Option<(TabId, SharedString)>,
    reveal: Reveal,
    /// Layouts written by us whose file-watcher echo has not arrived yet.
    echoes: Vec<LayoutConfig>,
    save_task: Option<Task<()>>,
    blurred: bool,
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

fn hsla(c: theme::Rgba) -> Hsla {
    c.into()
}

impl AppShell {
    pub fn new(theme: Theme, boot: ConfigBoot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ConfigBoot { config, watcher } = boot;
        let cwd0 = std::env::current_dir().unwrap_or_default();
        let layout = config.layout_for(&cwd0);
        ui_kit::set_reduced_motion(layout.reduced_motion);

        let usage = cx.new(UsageMonitor::new);
        let status = cx.new(|cx| StatusBar::new(theme, layout.bottom_bar, usage.clone(), cx));
        let focused = window.is_window_active();
        status.update(cx, |s, cx| s.set_active(focused, cx));

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
                this.usage.update(cx, |u, cx| u.set_active(active, cx));
                this.status.update(cx, |s, cx| s.set_active(active, cx));
            }),
        ];

        let mut shell = Self {
            theme,
            config,
            panes: PaneSet::new(pane_count(layout.splits)),
            layout,
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
            layout_open: false,
            layout_panel: None,
            confirm_close: None,
            reveal: Reveal::default(),
            echoes: Vec::new(),
            save_task: None,
            blurred: false,
            _watcher: watcher,
            _config_task: config_task,
            _subs: subs,
        };
        shell.open_tab(Some(cwd0), window, cx);
        shell
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

    /// Starts a terminal in `cwd` and shows it (first empty pane, else the focused pane).
    fn open_tab(&mut self, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme;
        let entity = cx.new(|cx| TerminalTab::new(theme, cwd, window, cx));
        let handle = TabHandle::new(entity);
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        let id = handle.id();
        self.tabs.push(TabSlot { handle, _sub: sub });
        self.panes.open(id);
        self.after_active_changed(window, cx);
    }

    /// "+": a new terminal in the active tab's directory.
    pub fn new_tab_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        self.open_tab(cwd, window, cx);
    }

    /// A tab's title, subtitle or status changed: repaint, and re-resolve the per-project layout
    /// (the active tab may have moved to another project).
    fn on_tab_event(&mut self, cx: &mut Context<Self>) {
        self.recompute_layout(true, cx);
        cx.notify();
    }

    pub fn select_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.slot(id).is_none() {
            return;
        }
        self.panes.show(id);
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
        self.recompute_layout(true, cx);
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
        let old = std::mem::replace(&mut self.layout, new);
        ui_kit::set_reduced_motion(self.layout.reduced_motion);
        if animate && !self.layout.reduced_motion {
            self.epoch += 1;
            self.transition = Some(Transition { from: old, started: Instant::now(), epoch: self.epoch });
        } else {
            self.transition = None;
        }
        let all = self.tab_ids();
        self.panes.resize(pane_count(self.layout.splits), &all);
        self.status.update(cx, |s, cx| s.set_mode(self.layout.bottom_bar, cx));
        cx.notify();
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
                let changed = layout_changed(&self.config, &config);
                self.config = config;
                if changed {
                    self.recompute_layout(true, cx);
                }
            },
            ConfigEvent::Error(msg) => log::warn!("config.toml is invalid, keeping the last good one: {msg}"),
        }
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

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.echoes.push(self.config.layout.clone());
        if self.echoes.len() > 8 {
            self.echoes.remove(0);
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Ok(cfg) = this.read_with(cx, |s, _| s.config.clone()) else { return };
            let result = cx.background_executor().spawn(async move { cfg.save() }).await;
            if let Err(e) = result {
                log::warn!("could not save the layout: {e}");
            }
        }));
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, _window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.theme.mode {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Oled,
            Mode::Oled => Mode::Dark,
        };
        self.theme = Theme::for_mode(next);
        let theme = self.theme;
        self.status.update(cx, |s, cx| s.set_theme(theme, cx));
        for slot in &self.tabs {
            slot.handle.theme_changed(theme, cx);
        }
        self.layout_panel = None;
        self.layout_open = false;
        cx.notify();
    }

    fn toggle_layout_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout_open = !self.layout_open;
        self.layout_panel = if self.layout_open {
            let shell = cx.weak_entity();
            let args = LayoutPanelArgs {
                theme: self.theme,
                layout: self.config.layout.clone(),
                cwd: self.active_cwd(cx),
                on_change: Rc::new(move |layout, cx| apply_from_panel(&shell, layout, cx)),
            };
            Some(build_layout_panel(args, window, cx))
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

    /// Rail "git": toggles the git panel on the right (a layout tweak, saved).
    fn toggle_git_panel(&mut self, cx: &mut Context<Self>) {
        let git = PanelId::new(store::config::panel_ids::GIT);
        self.tweak_global(true, cx, |l| {
            l.right_panel = if l.right_panel.as_ref() == Some(&git) { None } else { Some(git.clone()) };
        });
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
                    self.drag_moved = true;
                    cx.notify();
                }
            },
            Drag::SplitX => {
                self.ratio_x = ratio_at(f32::from(e.position.x), f32::from(b.origin.x), f32::from(b.size.width));
                cx.notify();
            },
            Drag::SplitY => {
                self.ratio_y = ratio_at(f32::from(e.position.y), f32::from(b.origin.y), f32::from(b.size.height));
                cx.notify();
            },
        }
    }

    /// Releases a drag; a resized sidebar is persisted (debounced).
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        if let Drag::Sidebar { .. } = drag
            && std::mem::take(&mut self.drag_moved)
        {
            let w = self.layout.sidebar.width_px;
            self.tweak_global(false, cx, |l| l.sidebar.width_px = w);
        }
    }

    // ------------------------------------------------------------------------------------
    // Right panel
    // ------------------------------------------------------------------------------------

    /// Right panel id to render now: the layout's, or the previous one while it animates out.
    fn right_id(&self) -> Option<PanelId> {
        self.layout
            .right_panel
            .clone()
            .or_else(|| self.transition.as_ref().and_then(|t| t.from.right_panel.clone()))
    }

    fn right_width(&self, window: &Window) -> f32 {
        (f32::from(window.viewport_size().width) * 0.22).clamp(240.0, 420.0)
    }

    /// Builds (or re-targets) the right panel's view for the active tab.
    fn ensure_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.right_id() else {
            self.right = None;
            return;
        };
        let info = self.active_info(cx);
        let ctx = PanelCtx {
            zone: PanelZone::Right,
            width_class: WidthClass::from_width(self.right_width(window)),
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
    // Window background (sidebar translucency)
    // ------------------------------------------------------------------------------------

    /// Acrylic is enabled for the whole window only while a visible sidebar asks for translucency;
    /// everything else paints opaque, so only the sidebar shows the blurred backdrop.
    fn sync_background(&mut self, window: &mut Window) {
        let want = self.layout.sidebar_translucency > 0 && self.layout.sidebar.side != SidebarSide::Hidden;
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
        let full = if animate { from.max(to) } else { to };
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

    fn right_zone_w(&self, l: &ResolvedLayout, window: &Window) -> f32 {
        if l.right_panel.is_some() && !l.zen {
            self.right_width(window) + GUTTER + Metrics::of(l.density).margin
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
        // The popover root aligns to the start of the cross axis: center it in the row by hand.
        let popover = Popover::new("layout-pop", &self.theme)
            .open(self.layout_open)
            .placement(Placement::BottomEnd)
            .width(440.0)
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

    /// The center column's top row: breadcrumb, tab pills (when tabs are on top), layout button
    /// and, if nothing sits to its right, the window controls.
    fn title_bar(
        &self,
        infos: &[TabInfo],
        window: &Window,
        cx: &mut Context<Self>,
        with_controls: bool,
        pad_left: f32,
    ) -> Div {
        let theme = self.theme;
        let shell = cx.entity();
        let active = self.active_id();
        let mut row = title_row(&theme).pl(px(pad_left));
        if self.layout.breadcrumb {
            let detail: SharedString = match self.active_cwd(cx) {
                Some(cwd) => paths::breadcrumb(&cwd).into(),
                None => "~".into(),
            };
            row = row.child(breadcrumb(&theme, detail));
        }
        if self.layout.tabs_position == TabsPosition::Top {
            let mut pills = div().min_w_0().flex().flex_row().items_center().gap(px(4.0)).overflow_hidden();
            for info in infos {
                let id = info.id;
                let (s1, s2) = (shell.clone(), shell.clone());
                pills = pills.child(tab_pill(
                    &theme,
                    info,
                    Some(id) == active,
                    move |_, window, cx| s1.update(cx, |s, cx| s.select_tab(id, window, cx)),
                    move |_, window, cx| s2.update(cx, |s, cx| s.request_close(id, window, cx)),
                ));
            }
            row = row
                .child(pills)
                .child(new_tab_button(&theme, move |_, window, cx| {
                    shell.update(cx, |s, cx| s.new_tab_here(window, cx));
                }));
        }
        row = row.child(div().flex_1()).child(self.layout_button(cx));
        if with_controls {
            row = row.child(window_controls(&theme, window.is_maximized()));
        } else {
            row = row.pr(px(8.0));
        }
        row
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
            .child(icon(info.icon.icon(), 14.0, c.muted))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(info.title.clone()))
            .children(info.subtitle.clone().map(|s| div().text_color(c.outline).child(s)))
            .child(div().flex_1())
            .children(info.status.map(|s| ui_kit::StatusGlyph::new(&self.theme, s)))
    }

    /// One pane: a floating card hosting a tab (or an empty-pane prompt).
    fn pane(&self, ix: usize, infos: &[TabInfo], m: Metrics, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let multi = self.panes.len() > 1;
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
                card.children(multi.then(|| info.map(|i| self.pane_header(i))).flatten()).child(slot.handle.content(cx))
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
        let body: AnyElement = match self.layout.splits {
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

    /// The sidebar card (header with the logo when the rail is hidden, then the sections).
    fn sidebar_card(
        &self,
        l: &ResolvedLayout,
        infos: &[TabInfo],
        window: &Window,
        cx: &mut Context<Self>,
        with_controls: bool,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let m = Metrics::of(l.density);
        let shell = cx.entity();
        let alpha = 1.0 - 0.85 * f32::from(l.sidebar_translucency) / 100.0;
        let card_bg = hsla(c.surface_1).opacity(alpha);
        let props = SidebarProps { theme: &self.theme, layout: l, tabs: infos, active: self.active_id(), shell: &shell };
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
        if l.rail == RailVisibility::Hidden && !right_side {
            header = header.pl(px(m.margin + 8.0)).child(ui_kit::Logo::new(self.theme.mode).size(28.0));
        } else {
            header = header.pl(px(m.margin));
        }
        header = header.child(div().flex_1());
        if with_controls {
            header = header.child(window_controls(&self.theme, window.is_maximized()));
        }
        let card = div()
            .flex_1()
            .min_h_0()
            .mb(px(m.margin))
            .rounded(px(Radius::MD))
            .bg(card_bg)
            .overflow_hidden()
            .child(sidebar_content(&props));
        let mut col = div().size_full().flex().flex_col().child(header);
        col = if right_side { col.child(card.mr(px(m.margin))) } else { col.child(card.ml(px(m.margin))) };
        col
    }

    fn right_card(&self, window: &Window, with_controls: bool, m: Metrics, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
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
        let body: AnyElement = match &self.right {
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
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mb(px(m.margin))
                    .ml(px(GUTTER))
                    .mr(px(m.margin))
                    .rounded(px(Radius::MD))
                    .bg(c.surface_1)
                    .overflow_hidden()
                    .child(body),
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
fn apply_from_panel(shell: &WeakEntity<AppShell>, layout: LayoutConfig, cx: &mut App) {
    shell.update(cx, |s, cx| s.set_layout_config(layout, true, cx)).ok();
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(t) = &self.transition
            && t.started.elapsed() > TRANSITION_KEEP
        {
            self.transition = None;
        }
        self.ensure_right_panel(window, cx);
        self.sync_background(window);

        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let l = self.layout.clone();
        let m = Metrics::of(l.density);
        let infos = self.infos(cx);
        let from = self.transition.as_ref().map(|t| t.from.clone()).unwrap_or_else(|| l.clone());
        let shell = cx.entity();

        // Which zone holds the window controls: the outermost one on the right.
        let right_sidebar_w = Self::sidebar_zone_w(&l, SidebarSide::Right);
        let right_panel_w = self.right_zone_w(&l, window);
        let controls_in_sidebar = right_sidebar_w > 0.0;
        let controls_in_right = !controls_in_sidebar && right_panel_w > 0.0;
        let controls_in_center = !controls_in_sidebar && !controls_in_right;

        // ---- rail
        let rail_el = self.zone(
            "zone-rail",
            true,
            Self::rail_w(&from),
            Self::rail_w(&l),
            rail(&theme, l.right_panel.as_ref().is_some_and(|p| p.as_str() == store::config::panel_ids::GIT), {
                let shell = shell.clone();
                move |_, _, cx| shell.update(cx, |s, cx| s.toggle_git_panel(cx))
            }),
        );

        // ---- sidebars (left / right)
        let sidebar_layout = self.content_layout(Self::sidebar_visible);
        let left_w = Self::sidebar_zone_w(&l, SidebarSide::Left);
        let left_from = Self::sidebar_zone_w(&from, SidebarSide::Left);
        let left_el = (left_w > 0.0 || left_from > 0.0).then(|| {
            let sl = if left_w > 0.0 { &l } else { sidebar_layout };
            self.zone("zone-left", true, left_from, left_w, self.sidebar_card(sl, &infos, window, cx, false))
        });
        let right_sb_from = Self::sidebar_zone_w(&from, SidebarSide::Right);
        let right_sb_el = (right_sidebar_w > 0.0 || right_sb_from > 0.0).then(|| {
            let sl = if right_sidebar_w > 0.0 { &l } else { sidebar_layout };
            self.zone(
                "zone-rsb",
                true,
                right_sb_from,
                right_sidebar_w,
                self.sidebar_card(sl, &infos, window, cx, controls_in_sidebar),
            )
        });
        let left_visible = left_w > 0.0;
        let right_visible = right_panel_w > 0.0 || right_sidebar_w > 0.0;
        let gutter_left = left_visible.then(|| {
            let shell2 = shell.clone();
            let side = l.sidebar.side;
            let w = l.sidebar.width_px;
            div()
                .w(px(GUTTER))
                .h_full()
                .flex_none()
                .cursor_col_resize()
                .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                    let x = f32::from(e.position.x);
                    shell2.update(cx, |s, _| s.start_drag(Drag::Sidebar { side, start_x: x, start_w: w }));
                })
        });
        let gutter_right = (right_sidebar_w > 0.0).then(|| {
            let shell2 = shell.clone();
            let w = l.sidebar.width_px;
            div()
                .w(px(GUTTER))
                .h_full()
                .flex_none()
                .cursor_col_resize()
                .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                    let x = f32::from(e.position.x);
                    shell2.update(cx, |s, _| {
                        s.start_drag(Drag::Sidebar { side: SidebarSide::Right, start_x: x, start_w: w })
                    });
                })
        });

        // ---- right panel
        let right_from = self.right_zone_w(&from, window);
        let right_el = (right_panel_w > 0.0 || right_from > 0.0)
            .then(|| self.zone("zone-right", true, right_from, right_panel_w, self.right_card(window, controls_in_right, m, cx)));

        // ---- center: title row + panes
        let show_title_row = !l.zen;
        let pad_left = if left_visible { 6.0 } else { m.margin + 4.0 };
        let title_row_el = show_title_row.then(|| self.title_bar(&infos, window, cx, controls_in_center, pad_left));
        let panes = self.pane_area(&infos, m, cx);
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
                    .mt(px(if l.zen { m.margin } else { 0.0 }))
                    .child(panes),
            );

        // ---- bottom bar
        let bar_mode = if l.bottom_bar != BottomBar::Hidden { l.bottom_bar } else { from.bottom_bar };
        self.status.update(cx, |s, cx| s.set_mode(bar_mode, cx));
        let status_el = (bar_height(l.bottom_bar) > 0.0 || bar_height(from.bottom_bar) > 0.0)
            .then(|| self.zone("zone-status", false, bar_height(from.bottom_bar), bar_height(l.bottom_bar), self.status.clone()));

        // ---- zen reveal overlays
        let zen_overlays = l.zen.then(|| self.zen_overlays(&infos, window, cx));

        let translucent = self.blurred;
        let mut body = div().flex_1().min_h_0().w_full().flex().flex_row();
        body = body.child(div().h_full().bg(c.bg).child(rail_el));
        if l.sidebar.side != SidebarSide::Right || left_el.is_some() {
            body = body.children(left_el.map(|e| {
                // Transparent only here: the blurred backdrop shows through the sidebar card.
                div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)
            }));
            body = body.children(gutter_left.map(|g| div().h_full().bg(c.bg).child(g)));
        }
        body = body.child(center);
        body = body.children(right_el.map(|e| div().h_full().bg(c.bg).child(e)));
        if right_sidebar_w > 0.0 || right_sb_from > 0.0 {
            body = body.children(gutter_right.map(|g| div().h_full().bg(c.bg).child(g)));
            body = body.children(right_sb_el.map(|e| div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)));
        }

        let key_context = if self.confirm_close.is_some() { "AppShell CloseConfirm" } else { "AppShell" };
        let confirm = self.confirm_close.as_ref().map(|(_, why)| why.clone());
        div()
            .id("app-shell")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .when(!translucent, |d| d.bg(c.bg))
            .text_color(c.text)
            .font_family(ty.ui_family)
            .key_context(key_context)
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab_here(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(id) = this.active_id() {
                    this.request_close(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(-1, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleLayoutPanel, window, cx| this.toggle_layout_panel(window, cx)))
            .on_action(cx.listener(|this, _: &ConfirmClose, window, cx| {
                if let Some((id, _)) = this.confirm_close.clone() {
                    this.close_tab(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CancelClose, _, cx| {
                this.confirm_close = None;
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
            .child(body)
            .children(status_el.map(|s| div().w_full().bg(c.bg).child(s)))
            .children(zen_overlays)
            .children(confirm.map(|why| self.confirm_overlay(&why, cx)))
    }
}

impl AppShell {
    /// Zen mode: invisible hover strips at the top and left edges reveal the tab bar (with the
    /// window controls) and a floating sidebar. They hide again when the pointer leaves them, and
    /// stay while the layout popover is open.
    fn zen_overlays(&self, infos: &[TabInfo], window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let shell = cx.entity();
        let reveal = self.reveal;
        let keep = self.layout_open;
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
                        if top && !s.layout_open {
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
                },
                ..self.layout.clone()
            };
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
                        .bg(c.surface_1)
                        .overflow_hidden()
                        .child(sidebar_content(&SidebarProps {
                            theme: &self.theme,
                            layout: &sl,
                            tabs: infos,
                            active: self.active_id(),
                            shell: &shell,
                        })),
                )
                .into_any_element()
        } else {
            strip("zen-left-strip", false).into_any_element()
        };

        div().absolute().size_full().child(left_panel).child(top_bar)
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
    fn pane_counts_match_the_layouts() {
        assert_eq!(pane_count(Splits::Grid2x2), 4);
    }
}
