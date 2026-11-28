//! The chrome: where the zones are, the models pushed to the sidebar and title bar entities, the
//! window-level synchronisation that cannot happen in `render` (right panel, glass background,
//! a panel's width class) and the keyboard focus between zones.

use super::*;
use crate::spaces;
use crate::titlebar_view::{TitleBarModel, TitleButton};

/// Where the zones are now, and where they were while a layout transition animates.
pub(super) struct Zones {
    /// What is docked now.
    pub l: ResolvedLayout,
    /// The layout the zones animate from (`l` itself when nothing animates).
    pub from: ResolvedLayout,
    pub left_w: f32,
    pub left_from: f32,
    /// The sidebar docked on the right.
    pub rsb_w: f32,
    pub rsb_from: f32,
    pub right_w: f32,
    pub right_from: f32,
    /// Which zone holds the window controls: the outermost one on the right.
    pub controls_in_sidebar: bool,
    pub controls_in_right: bool,
    pub controls_in_center: bool,
    /// The layout whose content a sidebar zone should draw (the previous one while it animates out).
    pub sidebar_layout: ResolvedLayout,
}

impl Zones {
    pub fn left_visible(&self) -> bool {
        self.left_w > 0.0
    }
}

impl AppShell {
    // ------------------------------------------------------------------------------------
    // Geometry
    // ------------------------------------------------------------------------------------

    pub(super) fn sidebar_visible(l: &ResolvedLayout) -> bool {
        l.sidebar.side != SidebarSide::Hidden && !l.zen
    }

    /// Width of the sidebar zone (card plus its outer margin) for a layout, per side.
    pub(super) fn sidebar_zone_w(l: &ResolvedLayout, side: SidebarSide) -> f32 {
        if Self::sidebar_visible(l) && l.sidebar.side == side {
            f32::from(l.sidebar.width_px) + Metrics::of(l.density).margin
        } else {
            0.0
        }
    }

    pub(super) fn rail_w(l: &ResolvedLayout) -> f32 {
        if l.rail == RailVisibility::Visible && !l.zen {
            RAIL_WIDTH
        } else {
            0.0
        }
    }

    pub(super) fn right_zone_w(&self, l: &ResolvedLayout) -> f32 {
        if l.right_panel.is_some() && !l.zen {
            self.right_width(l, self.host.width) + GUTTER + Metrics::of(l.density).margin
        } else {
            0.0
        }
    }

    /// Width of the docked right panel for layout `l` in a window of `window_w`.
    pub(super) fn right_width(&self, l: &ResolvedLayout, window_w: f32) -> f32 {
        let margin = Metrics::of(l.density).margin;
        let others = Self::rail_w(l)
            + Self::sidebar_zone_w(l, SidebarSide::Left)
            + Self::sidebar_zone_w(l, SidebarSide::Right)
            + 2.0 * margin
            + GUTTER;
        right_panel_width(window_w, self.splits.right_w, others)
    }

    /// Width of the right panel as mounted now (overlay / drawer or docked).
    pub(super) fn right_mounted_width(&self) -> f32 {
        if self.drawer.open == Some(Overlay::Right) {
            drawer_width(self.host.width)
        } else {
            self.right_width(&self.eff.layout, self.host.width)
        }
    }

    /// The layout whose content a zone should draw: the current one, or the previous one while the
    /// zone is animating out.
    pub(super) fn content_layout(
        &self,
        visible: impl Fn(&ResolvedLayout) -> bool,
    ) -> &ResolvedLayout {
        match &self.anim.transition {
            Some(t) if !visible(&self.layout) && visible(&t.from) => &t.from,
            _ => &self.layout,
        }
    }

    /// Shell v2 layouts: the tabs are rows of the sidebar's spaces.
    pub(super) fn is_unified(l: &ResolvedLayout) -> bool {
        l.sidebar.nested_tabs
            && l.sidebar.side != SidebarSide::Hidden
            && l.tabs_position == TabsPosition::Sidebar
            && !l.zen
    }

    pub(super) fn zones(&self) -> Zones {
        let l = self.eff.layout.clone();
        let from = self
            .anim
            .transition
            .as_ref()
            .map_or_else(|| l.clone(), |t| t.from.clone());
        let left_w = Self::sidebar_zone_w(&l, SidebarSide::Left);
        let rsb_w = Self::sidebar_zone_w(&l, SidebarSide::Right);
        let right_w = self.right_zone_w(&l);
        let controls_in_sidebar = rsb_w > 0.0;
        let controls_in_right = !controls_in_sidebar && right_w > 0.0;
        Zones {
            left_from: Self::sidebar_zone_w(&from, SidebarSide::Left),
            rsb_from: Self::sidebar_zone_w(&from, SidebarSide::Right),
            right_from: self.right_zone_w(&from),
            controls_in_center: !controls_in_sidebar && !controls_in_right,
            controls_in_sidebar,
            controls_in_right,
            sidebar_layout: self.content_layout(Self::sidebar_visible).clone(),
            l,
            from,
            left_w,
            rsb_w,
            right_w,
        }
    }

    /// Tint of a glass zone, or `None` when translucency is 0 (the zone keeps its opaque card).
    pub(super) fn glass(&self, l: &ResolvedLayout) -> Option<Hsla> {
        (l.sidebar_translucency > 0)
            .then(|| ui_kit::glass_tint(&self.theme, l.sidebar_translucency))
    }

    /// In-window overlays (drawers): glass, but nearly opaque: the desktop blur does not reach the
    /// terminal text behind them.
    pub(super) fn overlay_bg(&self) -> Hsla {
        match self.layout.sidebar_translucency {
            0 => hsla(self.theme.colors.surface_1),
            t => ui_kit::glass_tint(&self.theme, t.min(OVERLAY_MAX_TRANSLUCENCY)),
        }
    }

    // ------------------------------------------------------------------------------------
    // Rail items
    // ------------------------------------------------------------------------------------

    /// The rail's buttons: the terminal view plus one per registered panel that can live on the
    /// right. Nothing is listed that has no provider behind it.
    pub(super) fn rail_items(&self, cx: &App) -> Vec<RailItem> {
        let mut items = vec![
            RailItem {
                id: "rail-terminal".into(),
                icon: ui_kit::Icon::Terminal,
                title: "Terminal".into(),
                active: true,
                action: None,
            },
            // "Revisión": opens a review tab (main zone), not a right panel.
            RailItem {
                id: "rail-review".into(),
                icon: ui_kit::Icon::Review,
                title: "Revisar cambios".into(),
                active: self.review_active(),
                action: Some(RailAction::Review),
            },
        ];
        if let Some(reg) = cx.try_global::<PanelRegistry>() {
            for p in reg.for_zone(PanelZone::Right) {
                let id = p.id();
                items.push(RailItem {
                    id: format!("rail-{}", id.as_str()).into(),
                    icon: p.icon(),
                    title: p.title(),
                    active: self.right_panel_active(&id),
                    action: Some(RailAction::Panel(id)),
                });
            }
        }
        items
    }

    fn review_active(&self) -> bool {
        self.active_id()
            .and_then(|id| self.slot(id))
            .is_some_and(|s| s.handle.downcast::<ReviewTab>().is_some())
    }

    /// The sidebar's panel icons: the rail's, without the terminal view.
    fn panel_items(&self, cx: &App) -> Vec<RailItem> {
        self.rail_items(cx)
            .into_iter()
            .filter(|i| i.id.as_ref() != "rail-terminal")
            .collect()
    }

    pub(super) fn run_rail_action(
        &mut self,
        action: RailAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            RailAction::Review => self.open_review(window, cx),
            RailAction::Panel(id) => self.toggle_right_panel(id, cx),
        }
    }

    /// Branch and dirty flag of every repository the open terminals are in.
    fn space_git(&self, cx: &App) -> HashMap<PathBuf, spaces::GitSummary> {
        self.tabs
            .iter()
            .filter_map(|t| t.handle.downcast::<TerminalTab>())
            .filter_map(|t| {
                t.read(cx)
                    .git()
                    .read(cx)
                    .info()
                    .map(|i| (i.root.clone(), (i.branch.clone(), i.dirty)))
            })
            .collect()
    }

    // ------------------------------------------------------------------------------------
    // Models pushed to the chrome entities
    // ------------------------------------------------------------------------------------

    fn sidebar_model(
        &self,
        sl: &ResolvedLayout,
        opts: SidebarOpts,
        tint: Option<Hsla>,
        shared: &Shared,
    ) -> crate::sidebar_view::SidebarModel {
        // Only the unified sidebar paints spaces and panel icons: the classic one must not
        // repaint when they change.
        let unified = sl.sidebar.nested_tabs;
        crate::sidebar_view::SidebarModel {
            theme: self.theme,
            layout: sl.clone(),
            infos: shared.infos.clone(),
            active: shared.active,
            git: if unified {
                shared.git.clone()
            } else {
                HashMap::new()
            },
            known: if unified {
                self.spaces.known.clone()
            } else {
                Vec::new()
            },
            selected: if unified {
                self.projects.selected.clone()
            } else {
                None
            },
            dest: self.dest(),
            missing: if unified {
                self.projects.missing.clone()
            } else {
                HashSet::new()
            },
            folded: if unified {
                self.spaces.folded.clone()
            } else {
                HashSet::new()
            },
            panel_items: if unified {
                shared.panel_items.clone()
            } else {
                Vec::new()
            },
            opts,
            tint,
            lineage: if unified {
                shared.lineage.clone()
            } else {
                Default::default()
            },
            margin: Metrics::of(sl.density).margin,
        }
    }

    /// The layout of the drawer over the main area (the expanded sidebar of XS / S).
    pub(super) fn drawer_layout(&self) -> ResolvedLayout {
        let w = drawer_width(self.host.width);
        let mut sl = self.layout.clone();
        sl.sidebar.side = SidebarSide::Left;
        sl.sidebar.width_px = w as u16;
        sl.rail = self.eff.layout.rail;
        sl.zen = false;
        sl
    }

    /// The layout of zen mode's floating sidebar.
    pub(super) fn zen_sidebar_layout(&self) -> ResolvedLayout {
        ResolvedLayout {
            tabs_position: TabsPosition::Sidebar,
            sidebar: store::SidebarLayout {
                side: SidebarSide::Left,
                width_px: ZEN_SIDEBAR_W as u16,
                sections: vec![store::SidebarSection::Agents],
                group_by: store::GroupBy::None,
                ..store::SidebarLayout::default()
            },
            ..self.layout.clone()
        }
    }

    /// Pushes the model of every chrome view that is on screen. A view repaints only when its
    /// model differs from the last one it got.
    pub(super) fn sync_chrome(&mut self, cx: &mut Context<Self>) {
        let z = self.zones();
        // The bottom bar keeps its content while it animates out.
        let bar_mode = if z.l.bottom_bar != BottomBar::Hidden {
            z.l.bottom_bar
        } else {
            z.from.bottom_bar
        };
        self.status.update(cx, |s, cx| s.set_mode(bar_mode, cx));

        let shared = Shared {
            infos: self.infos(cx),
            active: self.active_id(),
            git: self.space_git(cx),
            panel_items: self.panel_items(cx),
            lineage: self.compute_lineage(cx),
        };
        let collapsed = |sl: &ResolvedLayout| sl.sidebar.width_px == COLLAPSED_SIDEBAR_W;
        if z.left_w > 0.0 || z.left_from > 0.0 {
            let sl = if z.left_w > 0.0 {
                &z.l
            } else {
                &z.sidebar_layout
            };
            let opts = SidebarOpts {
                collapsed: collapsed(sl),
                ..SidebarOpts::default()
            };
            let model = self.sidebar_model(sl, opts, self.glass(sl), &shared);
            self.chrome.left.update(cx, |v, cx| v.set_model(model, cx));
        }
        if z.rsb_w > 0.0 || z.rsb_from > 0.0 {
            let sl = if z.rsb_w > 0.0 {
                &z.l
            } else {
                &z.sidebar_layout
            };
            let opts = SidebarOpts {
                collapsed: collapsed(sl),
                with_controls: z.controls_in_sidebar,
                ..SidebarOpts::default()
            };
            let model = self.sidebar_model(sl, opts, self.glass(sl), &shared);
            self.chrome.right.update(cx, |v, cx| v.set_model(model, cx));
        }
        if self.drawer.open == Some(Overlay::Left) {
            let sl = self.drawer_layout();
            let opts = SidebarOpts {
                overlay: true,
                ..SidebarOpts::default()
            };
            let model = self.sidebar_model(&sl, opts, Some(self.overlay_bg()), &shared);
            self.chrome
                .drawer
                .update(cx, |v, cx| v.set_model(model, cx));
        }
        if z.l.zen && self.drawer.reveal.left {
            let sl = self.zen_sidebar_layout();
            let opts = SidebarOpts {
                bare: true,
                ..SidebarOpts::default()
            };
            let model = self.sidebar_model(&sl, opts, Some(self.overlay_bg()), &shared);
            self.chrome.zen.update(cx, |v, cx| v.set_model(model, cx));
        }
        self.sync_titlebar(&z, &shared, cx);
    }

    fn sync_titlebar(&mut self, z: &Zones, shared: &Shared, cx: &mut Context<Self>) {
        let keep = self.layout_panel.is_some() || self.chrome.titlebar.read(cx).menus_open();
        let (with_controls, pad_left, opaque) = if z.l.zen {
            if !(self.drawer.reveal.top || keep) {
                return;
            }
            (true, 12.0, true)
        } else {
            let m = Metrics::of(z.l.density);
            let pad = if z.left_visible() {
                6.0
            } else {
                m.margin + 4.0
            };
            (z.controls_in_center, pad, false)
        };
        let eff = &self.eff;
        let xs = eff.breakpoint == Breakpoint::Xs;
        let model = TitleBarModel {
            theme: self.theme,
            infos: shared.infos.clone(),
            active: shared.active,
            active_cwd: self.active_cwd(cx),
            unified_heading: Self::is_unified(&eff.layout) && !xs,
            breadcrumb: eff.layout.breadcrumb,
            breadcrumb_last_only: eff.breadcrumb_last_only,
            tab_strip: eff.layout.tabs_position == TabsPosition::Top || xs,
            xs,
            rail_in_menu: eff.rail_in_menu,
            drawer_sidebar: eff.sidebar == SidebarMode::Drawer,
            drawer_open: self.drawer.open == Some(Overlay::Left),
            buttons: self.titlebar_buttons(cx),
            rail_items: if eff.rail_in_menu {
                self.rail_items(cx)
            } else {
                Vec::new()
            },
            layout_panel: self.layout_panel.clone(),
            with_controls,
            pad_left,
            opaque,
        };
        self.chrome
            .titlebar
            .update(cx, |t, cx| t.set_model(model, cx));
    }

    /// The panel buttons at the right of the titlebar (`layout.titlebar.panel_buttons`): one per
    /// registered right panel, the review tab and the layout popover, with their active state.
    /// Ids nobody registered are skipped. The XS menu already lists them, so only the layout
    /// popover stays there.
    fn titlebar_buttons(&self, cx: &App) -> Vec<TitleButton> {
        let l = &self.eff.layout;
        let xs = self.eff.rail_in_menu;
        let mut out = Vec::new();
        for id in &l.titlebar.panel_buttons {
            let id_str = id.as_str();
            if id_str == store::config::panel_ids::LAYOUT {
                out.push(TitleButton::Layout);
                continue;
            }
            if xs {
                continue;
            }
            let (icon, title, active, action) = if id_str == store::config::panel_ids::REVIEW {
                (
                    ui_kit::Icon::Review,
                    SharedString::from("Revisar cambios"),
                    self.review_active(),
                    RailAction::Review,
                )
            } else {
                let Some(p) = cx.try_global::<PanelRegistry>().and_then(|r| r.get(id)) else {
                    continue;
                };
                if !p.allowed_zones().contains(&PanelZone::Right) {
                    continue;
                }
                (
                    p.icon(),
                    p.title(),
                    self.right_panel_active(id),
                    RailAction::Panel(id.clone()),
                )
            };
            out.push(TitleButton::Panel {
                id: id_str.to_string().into(),
                icon,
                title,
                active,
                action,
            });
        }
        out
    }

    // ------------------------------------------------------------------------------------
    // Events of the chrome entities
    // ------------------------------------------------------------------------------------

    pub(super) fn on_sidebar_event(
        &mut self,
        event: SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::Select(id) => self.select_tab(id, window, cx),
            SidebarEvent::OpenAgentMap(id) => self.open_agentmap_for_tab(id, cx),
            SidebarEvent::ChildMenu { thread, parent, at } => {
                self.open_child_menu(thread, parent, at, window, cx)
            }
            SidebarEvent::Close(id) => self.request_close(id, window, cx),
            SidebarEvent::NewHere => self.new_tab_here(window, cx),
            SidebarEvent::SelectSpace(root) => self.select_project(root, cx),
            SidebarEvent::AddProjectMenu(at) => self.open_add_project_menu(Some(at), window, cx),
            SidebarEvent::SpaceMenu { root, at, full } => {
                self.open_space_menu(root, at, full, window, cx)
            }
            SidebarEvent::ToggleSpace(key) => self.toggle_space(key, cx),
            SidebarEvent::NewMenu => self.open_new_tab_menu(window, cx),
            SidebarEvent::Palette => self.toggle_palette(window, cx),
            SidebarEvent::Settings => self.open_settings(window, cx),
            SidebarEvent::ToggleUsage => {
                self.toggle_right_panel(PanelId::new(ui_usage::PANEL_ID), cx)
            }
            SidebarEvent::Rail(action) => self.run_rail_action(action, window, cx),
            SidebarEvent::ToggleCollapsed => self.toggle_sidebar_collapsed(cx),
            SidebarEvent::ExpandOverlay => self.toggle_overlay(Overlay::Left, cx),
            SidebarEvent::DropOnSpace(paths) => self.drop_on_sidebar(&paths, cx),
            SidebarEvent::OpenThread(id) => {
                self.open_chat_thread(chat_core::ThreadId(id), window, cx)
            }
        }
    }

    pub(super) fn on_titlebar_event(
        &mut self,
        event: TitleBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TitleBarEvent::Select(id) => self.select_tab(id, window, cx),
            TitleBarEvent::Close(id) => self.request_close(id, window, cx),
            TitleBarEvent::NewTab => self.new_tab_here(window, cx),
            TitleBarEvent::ToggleLayout => self.toggle_layout_panel(window, cx),
            TitleBarEvent::DismissLayout => self.close_layout_panel(cx),
            TitleBarEvent::Rail(action) => self.run_rail_action(action, window, cx),
            TitleBarEvent::ToggleDrawer => self.toggle_overlay(Overlay::Left, cx),
            TitleBarEvent::MenusChanged => self.refresh(cx),
        }
    }

    // ------------------------------------------------------------------------------------
    // Window-level synchronisation (never from render)
    // ------------------------------------------------------------------------------------

    /// The right panel the layout wants now, with the context it gets.
    fn wanted_right(&self, cx: &App) -> Option<(PanelId, PanelCtx)> {
        let id = self.right_id()?;
        let info = self.active_info(cx);
        // The class comes from the zone's real width, not from the window's.
        let mut ctx = PanelCtx::new(
            PanelZone::Right,
            WidthClass::from_width(self.right_mounted_width()),
            self.theme,
        );
        if let Some(i) = info {
            if let Some(cwd) = i.cwd {
                ctx = ctx.with_cwd(cwd);
            }
            if let Some(root) = i.project_root {
                ctx = ctx.with_project_root(root);
            }
        }
        Some((id, ctx))
    }

    /// Acrylic is enabled for the whole window only while a docked glass zone asks for
    /// translucency; everything else paints opaque, so only those zones show the blurred backdrop.
    fn want_blur(&self) -> bool {
        let l = &self.eff.layout;
        let glass_zones = Self::sidebar_visible(l) || (l.right_panel.is_some() && !l.zen);
        self.layout.sidebar_translucency > 0 && glass_zones
    }

    /// Is something that needs `&mut Window` out of date?
    fn window_sync_needed(&self, cx: &App) -> bool {
        let right_stale = match (&self.right, self.wanted_right(cx)) {
            (None, None) => false,
            (Some(r), Some((id, ctx))) => r.id != id || r.ctx != ctx,
            _ => true,
        };
        right_stale
            || self.want_blur() != self.host.blurred
            || (self.settings_slot().is_some() && self.main_class() != self.host.main_class)
    }

    /// Schedules one [`sync_window`](Self::sync_window) for the next tick when it is needed.
    pub(super) fn queue_window_sync(&mut self, cx: &mut Context<Self>) {
        if self.host.sync_pending || !self.window_sync_needed(cx) {
            return;
        }
        self.host.sync_pending = true;
        let handle = self.host.window;
        cx.spawn(async move |this, cx| {
            cx.update_window(handle, |_, window, cx| {
                this.update(cx, |shell, cx| shell.sync_window(window, cx))
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    /// Builds or re-targets the right panel, re-targets the settings tab to its zone's width
    /// class and sets the window background: all of which need the window.
    pub(crate) fn sync_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.host.sync_pending = false;
        let mut changed = self.ensure_right_panel(window, cx);
        changed |= self.sync_settings_ctx(window, cx);
        changed |= self.sync_background(window);
        if changed {
            self.refresh(cx);
        }
    }

    /// Builds (or re-targets) the right panel's view for the active tab. `true` when it changed.
    fn ensure_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((id, ctx)) = self.wanted_right(cx) else {
            return self.right.take().is_some();
        };
        // The threads panel needs the chat hub: it opens off the UI thread and the panel is
        // rebuilt when it is ready (`on_chat_host_event`); until then the provider's stand-in shows.
        if id.as_str() == ui_chat::THREADS_PANEL_ID {
            self.chat.update(cx, |c, cx| c.ensure(cx));
        }
        if let Some(r) = &self.right
            && r.id == id
            && r.ctx == ctx
        {
            return false;
        }
        let provider = cx.try_global::<PanelRegistry>().and_then(|r| r.get(&id));
        let retargeted = match (&self.right, &provider) {
            (Some(r), Some(p)) if r.id == id => r
                .view
                .as_ref()
                .is_some_and(|v| p.context_changed(v, &ctx, window, cx)),
            _ => false,
        };
        if retargeted {
            if let Some(r) = &mut self.right {
                r.ctx = ctx;
            }
            return true;
        }
        let view = provider.map(|p| p.build(ctx.clone(), window, cx));
        self.right = Some(RightPanel { id, ctx, view });
        true
    }

    /// The window background follows the glass zones. `true` when it changed.
    fn sync_background(&mut self, window: &mut Window) -> bool {
        let want = self.want_blur();
        if want == self.host.blurred {
            return false;
        }
        self.host.blurred = want;
        window.set_background_appearance(if want {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        });
        true
    }

    fn settings_slot(&self) -> Option<&TabSlot> {
        self.tabs.iter().find(|t| {
            t.handle
                .downcast::<crate::settings_tab::SettingsTab>()
                .is_some()
        })
    }

    /// Width of the pane the settings tab is in (the main zone, split by the panes' ratios).
    pub(super) fn main_pane_width(&self) -> f32 {
        let l = &self.eff.layout;
        let margin = Metrics::of(l.density).margin;
        let zone = (self.host.width
            - Self::rail_w(l)
            - Self::sidebar_zone_w(l, SidebarSide::Left)
            - Self::sidebar_zone_w(l, SidebarSide::Right)
            - self.right_zone_w(l)
            - 2.0 * margin)
            .max(0.0);
        let ix = self
            .settings_slot()
            .and_then(|s| {
                let id = s.handle.id();
                self.splits
                    .panes
                    .panes()
                    .iter()
                    .position(|p| *p == Some(id))
            })
            .unwrap_or(0);
        let rx = clamp_ratio(self.splits.ratio_x);
        let share = match l.splits {
            Splits::Single | Splits::TwoHorizontal => 1.0,
            Splits::TwoVertical | Splits::Grid2x2 => {
                if ix.is_multiple_of(2) {
                    rx
                } else {
                    1.0 - rx
                }
            }
            Splits::OneAndTwoStacked => {
                if ix == 0 {
                    rx
                } else {
                    1.0 - rx
                }
            }
        };
        zone * share
    }

    pub(super) fn main_class(&self) -> WidthClass {
        WidthClass::from_width(self.main_pane_width())
    }

    /// The settings tab is a main-zone panel: it learns its zone's real width class (not the
    /// window's) whenever the class changes. `true` when it was told.
    fn sync_settings_ctx(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(slot) = self.settings_slot() else {
            return false;
        };
        let class = self.main_class();
        if class == self.host.main_class {
            return false;
        }
        let content = slot.handle.content(cx);
        self.host.main_class = class;
        let ctx = self.main_ctx(class, cx);
        let provider = cx
            .try_global::<PanelRegistry>()
            .and_then(|r| r.get(&PanelId::new(ui_settings::PANEL_ID)));
        if let Some(p) = provider {
            p.context_changed(&content, &ctx, window, cx);
        }
        true
    }

    /// The context of a panel mounted in the main zone.
    pub(super) fn main_ctx(&self, class: WidthClass, cx: &App) -> PanelCtx {
        let mut ctx = PanelCtx::new(PanelZone::Main, class, self.theme);
        if let Some(i) = self.active_info(cx) {
            if let Some(cwd) = i.cwd {
                ctx = ctx.with_cwd(cwd);
            }
            if let Some(root) = i.project_root {
                ctx = ctx.with_project_root(root);
            }
        }
        ctx
    }

    // ------------------------------------------------------------------------------------
    // Keyboard focus between zones
    // ------------------------------------------------------------------------------------

    /// The sidebar entity that is on screen now, if any.
    fn visible_sidebar(&self) -> Option<&Entity<SidebarView>> {
        let z = self.zones();
        if self.drawer.open == Some(Overlay::Left) {
            Some(&self.chrome.drawer)
        } else if z.left_w > 0.0 {
            Some(&self.chrome.left)
        } else if z.rsb_w > 0.0 {
            Some(&self.chrome.right)
        } else if self.eff.layout.zen && self.drawer.reveal.left {
            Some(&self.chrome.zen)
        } else {
            None
        }
    }

    fn right_zone_visible(&self) -> bool {
        self.right.is_some()
            && (self.drawer.open == Some(Overlay::Right)
                || self.right_zone_w(&self.eff.layout) > 0.0)
    }

    /// The zones that can take the keyboard focus now, left to right.
    pub(crate) fn visible_zones(&self) -> Vec<Zone> {
        let mut zones = Vec::new();
        if self.visible_sidebar().is_some() {
            zones.push(Zone::Sidebar);
        }
        zones.push(Zone::Main);
        if self.right_zone_visible() {
            zones.push(Zone::Right);
        }
        zones
    }

    /// The zone that holds the keyboard focus (the main one when nothing else does).
    pub(crate) fn current_zone(&self, window: &Window, cx: &App) -> Zone {
        if self
            .visible_sidebar()
            .is_some_and(|s| s.read(cx).contains_focus(window, cx))
        {
            Zone::Sidebar
        } else if self.right_zone_visible() && self.chrome.right_focus.contains_focused(window, cx)
        {
            Zone::Right
        } else {
            Zone::Main
        }
    }

    /// F6 / Shift+F6: the next / previous visible zone, wrapping.
    pub(crate) fn cycle_zone(&mut self, dir: isize, window: &mut Window, cx: &mut Context<Self>) {
        let zones = self.visible_zones();
        let cur = zones
            .iter()
            .position(|z| *z == self.current_zone(window, cx))
            .unwrap_or(0);
        if let Some(zone) = zones.get(cycle_index(zones.len(), cur, dir)).copied() {
            self.focus_zone(zone, window, cx);
        }
    }

    /// Moves the keyboard focus into `zone`. The sidebar opens its drawer first where it is not
    /// docked (S, XS).
    pub(crate) fn focus_zone(&mut self, zone: Zone, window: &mut Window, cx: &mut Context<Self>) {
        match zone {
            Zone::Main => {
                if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
                    slot.handle.focus(window, cx);
                }
            }
            Zone::Sidebar => match self.visible_sidebar().cloned() {
                Some(view) => view.read(cx).focus_first(window, cx),
                None if self.eff.sidebar != SidebarMode::Off => {
                    self.toggle_overlay(Overlay::Left, cx);
                    // The drawer paints on the next frame; its rows exist on the one after.
                    let view = self.chrome.drawer.clone();
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| {
                            view.read(cx).focus_first(window, cx);
                        });
                    });
                }
                None => {}
            },
            Zone::Right => {
                if self.right_zone_visible() {
                    window.focus(&self.chrome.right_focus);
                    window.focus_next();
                    if !self.chrome.right_focus.contains_focused(window, cx) {
                        window.focus(&self.chrome.right_focus);
                    }
                }
            }
        }
    }
}

/// What every chrome model shares, computed once per sync.
pub(super) struct Shared {
    infos: Vec<TabInfo>,
    active: Option<TabId>,
    git: HashMap<PathBuf, spaces::GitSummary>,
    panel_items: Vec<RailItem>,
    lineage: crate::sidebar_view::Lineage,
}
