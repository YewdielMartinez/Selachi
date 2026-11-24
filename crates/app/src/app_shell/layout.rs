//! Layout and configuration: resolving the layout for the active tab, adapting it to the window
//! width, hot-reloaded config, theme changes, the layout popover, drawers / overlays, the right
//! panel's identity and dragging.

use super::*;
use crate::layout_host::{LayoutPanelArgs, build_layout_panel};

impl AppShell {
    // ------------------------------------------------------------------------------------
    // Layout and configuration
    // ------------------------------------------------------------------------------------

    /// Re-resolves the layout for the active tab's directory. Animates the zones when it changed.
    pub(super) fn recompute_layout(&mut self, animate: bool, cx: &mut Context<Self>) {
        let cwd = self
            .active_cwd(cx)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let new = self.config.layout_for(&cwd);
        if new == self.layout {
            return;
        }
        self.layout = new;
        self.apply_adapted(animate, cx);
        self.refresh(cx);
    }

    /// Adapts the intent to the window width. Crossing a breakpoint (or a layout change) runs one
    /// 200 ms zone transition; resizing inside a class changes nothing here, so no per-pixel work.
    pub(super) fn apply_adapted(&mut self, animate: bool, cx: &mut Context<Self>) {
        let new = adapt_for_shell(&self.layout, self.host.width, self.sidebar_collapsed);
        if new == self.eff {
            return;
        }
        let old = std::mem::replace(&mut self.eff, new);
        if old.breakpoint != self.eff.breakpoint {
            self.drawer.open = None;
            self.chrome.titlebar.update(cx, |t, cx| t.close_menus(cx));
        }
        ui_kit::set_reduced_motion(self.layout.reduced_motion);
        if animate && !self.layout.reduced_motion {
            self.anim.epoch += 1;
            self.anim.transition = Some(Transition {
                from: old.layout,
                epoch: self.anim.epoch,
            });
            // Nothing else repaints when the animation ends: one wake-up drops the transition
            // state (and the oversized, clipped zone contents) right after it.
            self.anim.task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(TRANSITION_KEEP + Duration::from_millis(40))
                    .await;
                this.update(cx, |shell, cx| {
                    shell.anim.transition = None;
                    shell.refresh(cx);
                })
                .ok();
            }));
        } else {
            self.anim.transition = None;
        }
        let all = self.tab_ids();
        self.splits
            .panes
            .resize(pane_count(self.layout.splits), &all);
        self.splits
            .panes
            .clamp_focus(pane_count(self.eff.layout.splits));
        let dedupe = dedupes_usage(&self.eff);
        self.status
            .update(cx, |s, cx| s.set_dedupe_usage(dedupe, cx));
    }

    /// The window was resized (the bounds observer): re-adapt when the width class changed.
    pub(super) fn on_resize(&mut self, width: f32, cx: &mut Context<Self>) {
        if (width - self.host.width).abs() < 0.5 {
            return;
        }
        self.host.width = width;
        self.apply_adapted(true, cx);
        self.refresh(cx);
    }

    pub(super) fn on_config_event(&mut self, event: ConfigEvent, cx: &mut Context<Self>) {
        match event {
            ConfigEvent::Reloaded { config, warnings } => {
                for w in warnings {
                    log::warn!("config.toml: {w}");
                }
                // Our own save coming back: nothing to do.
                if let Some(i) = self
                    .cfg_save
                    .echoes
                    .iter()
                    .position(|l| *l == config.layout)
                {
                    self.cfg_save.echoes.remove(i);
                    return;
                }
                self.adopt_config(config.clone(), cx);
                if let Some(settings) = self.settings_view() {
                    settings.update(cx, |s, cx| s.set_config((*config).clone(), cx));
                }
            }
            ConfigEvent::Error(msg) => {
                log::warn!("config.toml is invalid, keeping the last good one: {msg}")
            }
        }
    }

    /// Runs `f` on the open layout panel (hot reload, theme and cwd changes keep it in sync).
    pub(super) fn sync_layout_panel(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(
            &mut ui_layout::LayoutPanel,
            &Config,
            Theme,
            &mut Context<ui_layout::LayoutPanel>,
        ),
    ) {
        let Some(view) = self.layout_panel.clone() else {
            return;
        };
        let Ok(panel) = view.downcast::<ui_layout::LayoutPanel>() else {
            return;
        };
        let (config, theme) = (self.config.clone(), self.theme);
        panel.update(cx, |p, cx| f(p, &config, theme, cx));
    }

    /// The active tab changed directory: the panel's "this project" scope follows.
    pub(super) fn sync_layout_panel_cwd(&mut self, cx: &mut Context<Self>) {
        if self.layout_panel.is_none() {
            return;
        }
        let cwd = self.active_cwd(cx);
        self.sync_layout_panel(cx, move |p, _, _, cx| p.set_cwd(cwd, cx));
    }

    /// Applies a layout coming from the popover (or any editor): live, then saved.
    pub fn set_layout_config(
        &mut self,
        layout: LayoutConfig,
        animate: bool,
        cx: &mut Context<Self>,
    ) {
        if self.config.layout == layout {
            return;
        }
        let mut cfg = (*self.config).clone();
        cfg.set_layout(layout);
        self.config = Arc::new(cfg);
        self.schedule_save(cx);
        self.recompute_layout(animate, cx);
        self.mark_session_dirty(cx);
        if let Some(settings) = self.settings_view() {
            let cfg = (*self.config).clone();
            settings.update(cx, |s, cx| s.set_config(cfg, cx));
        }
    }

    /// Edits the global layout (the preset plus global overrides) with `f` and stores only the
    /// difference from the preset.
    pub(super) fn tweak_global(
        &mut self,
        animate: bool,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ResolvedLayout),
    ) {
        let mut layout = self.config.layout.clone();
        let mut target = self.config.global_layout();
        f(&mut target);
        layout.set_resolved(&target);
        self.set_layout_config(layout, animate, cx);
    }

    /// Saves `config.toml` 400 ms after the last change, one write at a time: the sliders of the
    /// layout panel emit on every move.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.cfg_save.echoes.push(self.config.layout.clone());
        if self.cfg_save.echoes.len() > 8 {
            self.cfg_save.echoes.remove(0);
        }
        self.cfg_save.dirty = true;
        self.arm_save(cx);
    }

    /// (Re)starts the debounce: replacing the task restarts the timer.
    fn arm_save(&mut self, cx: &mut Context<Self>) {
        self.cfg_save.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |s, cx| {
                s.cfg_save.task = None;
                s.run_save(cx);
            })
            .ok();
        }));
    }

    fn run_save(&mut self, cx: &mut Context<Self>) {
        // A write is running: it re-arms the save when it finishes (the config moved on).
        if self.cfg_save.inflight || !self.cfg_save.dirty {
            return;
        }
        self.cfg_save.inflight = true;
        self.cfg_save.dirty = false;
        let cfg = self.config.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { cfg.save() })
                .await;
            if let Err(e) = result {
                log::warn!("could not save the layout: {e}");
            }
            this.update(cx, |s, cx| {
                s.cfg_save.inflight = false;
                // Re-arms the debounce without pushing another echo (the config did not change).
                if s.cfg_save.dirty && s.cfg_save.task.is_none() {
                    s.arm_save(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn toggle_theme(
        &mut self,
        _: &ToggleTheme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = match self.theme.mode {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Oled,
            Mode::Oled => Mode::Dark,
        };
        self.set_theme(themed(next, &self.config), cx);
    }

    /// Re-themes every part of the window.
    pub(super) fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        // Mode or accent: anything that differs is applied to every live view.
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        self.status.update(cx, |s, cx| s.set_theme(theme, cx));
        self.chat.update(cx, |c, cx| c.set_theme(theme, cx));
        for slot in &self.tabs {
            slot.handle.theme_changed(theme, cx);
        }
        self.sync_layout_panel(cx, |p, _, theme, cx| p.set_theme(theme, cx));
        self.palette.update(cx, |p, cx| p.set_theme(theme, cx));
        if let Some(settings) = self.settings_view() {
            settings.update(cx, |s, cx| s.set_theme(theme, cx));
        }
        if let Some(ob) = self.onboarding_view() {
            ob.update(cx, |o, cx| o.set_theme(theme, cx));
        }
        self.refresh(cx);
    }

    /// A new configuration (hot reload, settings screen, wizard): layout, completion settings and
    /// theme follow it. Nothing happens for what did not change.
    pub(super) fn adopt_config(&mut self, config: Arc<Config>, cx: &mut Context<Self>) {
        let layout_moved = layout_changed(&self.config, &config);
        let theme_moved = self.config.appearance.mode != config.appearance.mode
            || self.config.appearance.accent != config.appearance.accent;
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

    pub(super) fn toggle_layout_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout_panel.is_some() {
            self.layout_panel = None;
        } else {
            self.chrome
                .titlebar
                .update(cx, |t, cx| t.close_main_menu(cx));
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
            self.layout_panel = Some(view);
        }
        self.refresh(cx);
    }

    pub(super) fn close_layout_panel(&mut self, cx: &mut Context<Self>) {
        if self.layout_panel.take().is_some() {
            self.refresh(cx);
        }
    }

    // ------------------------------------------------------------------------------------
    // Panels, drawers and menus
    // ------------------------------------------------------------------------------------

    /// Is the right panel `id` showing (docked, or open as an overlay / drawer)?
    pub(super) fn right_panel_active(&self, id: &PanelId) -> bool {
        if self.eff.breakpoint <= Breakpoint::S {
            self.drawer.open == Some(Overlay::Right) && &self.drawer.right == id
        } else {
            self.layout.right_panel.as_ref() == Some(id)
        }
    }

    /// Rail / menu button of a panel: docks it on the right (a saved layout tweak), or opens it as
    /// an overlay / drawer when the window is narrow (not saved: the intent does not change).
    pub(crate) fn toggle_right_panel(&mut self, id: PanelId, cx: &mut Context<Self>) {
        if self.eff.breakpoint <= Breakpoint::S {
            if self.right_panel_active(&id) {
                self.drawer.open = None;
            } else {
                self.drawer.right = id;
                self.drawer.open = Some(Overlay::Right);
            }
            self.drawer.epoch += 1;
            self.chrome
                .titlebar
                .update(cx, |t, cx| t.close_main_menu(cx));
            self.refresh(cx);
            return;
        }
        self.tweak_global(true, cx, |l| {
            l.right_panel = if l.right_panel.as_ref() == Some(&id) {
                None
            } else {
                Some(id.clone())
            };
        });
    }

    pub(super) fn toggle_overlay(&mut self, which: Overlay, cx: &mut Context<Self>) {
        self.drawer.open = if self.drawer.open == Some(which) {
            None
        } else {
            Some(which)
        };
        self.drawer.epoch += 1;
        self.chrome
            .titlebar
            .update(cx, |t, cx| t.close_main_menu(cx));
        self.refresh(cx);
    }

    pub(super) fn close_overlay(&mut self, cx: &mut Context<Self>) {
        if self.drawer.open.take().is_some() {
            self.refresh(cx);
        }
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
        self.refresh(cx);
    }

    /// Right panel id to render now: the open overlay's, the layout's, or the previous docked one
    /// while it animates out.
    pub(super) fn right_id(&self) -> Option<PanelId> {
        if self.drawer.open == Some(Overlay::Right) {
            return Some(self.drawer.right.clone());
        }
        self.eff.layout.right_panel.clone().or_else(|| {
            self.anim
                .transition
                .as_ref()
                .and_then(|t| t.from.right_panel.clone())
        })
    }

    // ------------------------------------------------------------------------------------
    // Dragging
    // ------------------------------------------------------------------------------------

    pub(super) fn start_drag(&mut self, drag: Drag) {
        self.splits.drag = Some(drag);
        self.splits.drag_moved = false;
    }

    pub(super) fn on_mouse_move(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.splits.drag else { return };
        if e.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let b = self.splits.bounds.get();
        match drag {
            Drag::Sidebar {
                side,
                start_x,
                start_w,
            } => {
                let w = dragged_width(side, start_w, f32::from(e.position.x) - start_x);
                if self.layout.sidebar.width_px != w {
                    self.layout.sidebar.width_px = w;
                    self.eff.layout.sidebar.width_px = w;
                    self.splits.drag_moved = true;
                    self.refresh(cx);
                }
            }
            Drag::Right { start_x, start_w } => {
                let w = dragged_right_width(start_w, f32::from(e.position.x) - start_x);
                if self.splits.right_w != Some(w) {
                    self.splits.right_w = Some(w);
                    self.splits.drag_moved = true;
                    self.refresh(cx);
                }
            }
            Drag::SplitX => {
                self.splits.ratio_x = crate::splits::ratio_at(
                    f32::from(e.position.x),
                    f32::from(b.origin.x),
                    f32::from(b.size.width),
                );
                self.splits.drag_moved = true;
                self.refresh(cx);
            }
            Drag::SplitY => {
                self.splits.ratio_y = crate::splits::ratio_at(
                    f32::from(e.position.y),
                    f32::from(b.origin.y),
                    f32::from(b.size.height),
                );
                self.splits.drag_moved = true;
                self.refresh(cx);
            }
        }
    }

    /// Releases a drag; a resized sidebar is persisted (debounced), and so is the session (right
    /// panel width, split ratios).
    pub(super) fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.splits.drag.take() else {
            return;
        };
        if !std::mem::take(&mut self.splits.drag_moved) {
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
    // Process tracking without a job
    // ------------------------------------------------------------------------------------

    /// While tree-tracked owners exist (a shell Windows refused to put in a job, such as the
    /// Microsoft Store `pwsh`) **and** the procs panel is open and the window focused, refreshes
    /// them every `proc_svc::TREE_POLL_INTERVAL`. The task ends by itself as soon as one of those
    /// stops being true, so nothing ticks at rest. Called when the right panel or the window's
    /// focus changes, never from `render`.
    pub(super) fn sync_tree_poll(&mut self, cx: &mut Context<Self>) {
        if self.tasks.tree_poll.is_some()
            || !self.right_panel_active(&PanelId::new(ui_procs::PANEL_ID))
        {
            return;
        }
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        let tracker = services.tracker.clone();
        if !tracker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .needs_polling()
        {
            return;
        }
        let procs = services.procs;
        self.tasks.tree_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(proc_svc::TREE_POLL_INTERVAL)
                    .await;
                let t = tracker.clone();
                let more = cx
                    .background_executor()
                    .spawn(async move { t.lock().unwrap_or_else(|p| p.into_inner()).poll_tree() })
                    .await;
                let wanted = this
                    .update(cx, |shell, cx| {
                        procs.update(cx, |_, cx| cx.notify());
                        shell.right_panel_active(&PanelId::new(ui_procs::PANEL_ID))
                            && notify::window_active()
                    })
                    .unwrap_or(false);
                if !more || !wanted {
                    break;
                }
            }
            this.update(cx, |shell, _| shell.tasks.tree_poll = None)
                .ok();
        }));
    }
}

/// Callback of the layout popover: hands the new layout to the shell.
fn apply_from_panel(shell: &WeakEntity<AppShell>, layout: LayoutConfig, cx: &mut App) {
    shell
        .update(cx, |s, cx| s.set_layout_config(layout, true, cx))
        .ok();
}

use gpui::WeakEntity;
