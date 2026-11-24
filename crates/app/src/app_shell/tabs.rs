//! The tab model: opening, selecting, closing and cycling tabs, the split panes' focus, and the
//! session (what is saved and how it is restored).

use super::*;
use crate::session::SavedTab;
use crate::terminal_tab::TermRequest;

impl AppShell {
    // ------------------------------------------------------------------------------------
    // Session (A5)
    // ------------------------------------------------------------------------------------

    /// Reopens the saved tabs in their directories. Shells start the first time their tab is
    /// painted, so tabs that are never shown cost no process (`TerminalView::new_deferred`).
    /// `SELACHI_EAGER_RESTORE=1` spawns them all at once (to measure the difference).
    pub(super) fn restore_session(
        &mut self,
        state: SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let deferred = std::env::var_os("SELACHI_EAGER_RESTORE").is_none();
        // One entry per saved tab; a chat tab that cannot be reopened leaves a hole.
        let mut ids: Vec<Option<TabId>> = Vec::new();
        for tab in &state.tabs {
            ids.push(match tab.kind {
                TabKind::Terminal => Some(self.add_tab(tab.cwd.clone(), deferred, window, cx)),
                TabKind::Chat => tab
                    .thread
                    .and_then(|t| self.restore_chat_tab(ThreadId(t), window, cx)),
            });
        }
        if ids.iter().all(Option::is_none) {
            // Nothing could be reopened (a session of chats and the chat database is gone).
            let cwd = std::env::current_dir().ok();
            self.open_tab(cwd, window, cx);
            return;
        }
        let panes: Vec<Option<TabId>> = (0..self.splits.panes.len())
            .map(|i| {
                state
                    .panes
                    .get(i)
                    .copied()
                    .flatten()
                    .and_then(|t| ids.get(t).copied().flatten())
            })
            .collect();
        self.splits.panes = PaneSet::from_panes(panes, state.focused);
        if self.splits.panes.focused_tab().is_none()
            && let Some(first) = ids.iter().flatten().next().copied()
        {
            self.splits.panes.show(first);
        }
        self.splits.ratio_x = clamp_ratio(state.ratio_x);
        self.splits.ratio_y = clamp_ratio(state.ratio_y);
        self.splits.right_w = state.right_w.map(|w| w.clamp(RIGHT_MIN_W, RIGHT_MAX_W));
        if state.sidebar_collapsed != self.sidebar_collapsed {
            self.sidebar_collapsed = state.sidebar_collapsed;
            self.apply_adapted(false, cx);
        }
        let json = state.to_json();
        self.session.update(cx, |s, _| s.set_baseline(json));
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
            panes: self
                .splits
                .panes
                .panes()
                .iter()
                .map(|p| p.and_then(|id| ids.iter().position(|i| *i == id)))
                .collect(),
            focused: self.splits.panes.focused(),
            ratio_x: self.splits.ratio_x,
            ratio_y: self.splits.ratio_y,
            sidebar_w: Some(self.layout.sidebar.width_px),
            right_w: self.splits.right_w,
            sidebar_collapsed: self.sidebar_collapsed,
        }
    }

    /// Something that belongs to the session changed: it is written within 2 s (once).
    pub(super) fn mark_session_dirty(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| s.mark_dirty(cx));
    }

    /// Writes the session if it differs from the last one written; `flush` waits for the writer.
    pub(super) fn save_session_now(&mut self, flush: bool, cx: &mut Context<Self>) {
        if !self.session.read(cx).enabled() || self.saved_tabs(cx).is_empty() {
            return;
        }
        let json = self.capture_session(cx).to_json();
        self.session.update(cx, |s, _| s.save(json, flush));
    }

    // ------------------------------------------------------------------------------------
    // Tabs
    // ------------------------------------------------------------------------------------

    pub(super) fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.iter().map(|t| t.handle.id()).collect()
    }

    pub(super) fn infos(&self, cx: &App) -> Vec<TabInfo> {
        self.tabs.iter().map(|t| t.handle.info(cx)).collect()
    }

    pub(super) fn slot(&self, id: TabId) -> Option<&TabSlot> {
        self.tabs.iter().find(|t| t.handle.id() == id)
    }

    pub(super) fn active_id(&self) -> Option<TabId> {
        self.splits.panes.focused_tab()
    }

    pub(super) fn active_info(&self, cx: &App) -> Option<TabInfo> {
        self.active_id()
            .and_then(|id| self.slot(id))
            .map(|s| s.handle.info(cx))
    }

    pub(super) fn active_cwd(&self, cx: &App) -> Option<PathBuf> {
        self.active_info(cx).and_then(|i| i.cwd)
    }

    /// The tab strip scrolls to the active tab the next time it paints.
    pub(super) fn reveal_active_tab(&mut self, cx: &mut Context<Self>) {
        self.chrome.titlebar.update(cx, |t, cx| t.reveal_active(cx));
    }

    /// Wires a tab model's handle into the shell (its events refresh the chrome) and lists it.
    pub(super) fn push_tab(
        &mut self,
        handle: TabHandle,
        extra: Vec<Subscription>,
        cx: &mut Context<Self>,
    ) {
        let shell = cx.weak_entity();
        let sub = handle.subscribe(cx, move |_, cx| {
            shell.update(cx, |this, cx| this.on_tab_event(cx)).ok();
        });
        let mut subs = vec![sub];
        subs.extend(extra);
        self.tabs.push(TabSlot {
            handle,
            _subs: subs,
        });
    }

    /// Shows a freshly listed tab: first empty pane, else the focused one, and focuses it.
    pub(super) fn show_new_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.splits.panes.open(id);
        self.reveal_active_tab(cx);
        self.after_active_changed(window, cx);
    }

    /// Creates a terminal tab (not shown yet). `deferred`: its shell starts when first painted.
    pub(super) fn add_tab(
        &mut self,
        cwd: Option<PathBuf>,
        deferred: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabId {
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
        // Ctrl+click on a path and "Arreglar con agente" come as requests of the terminal.
        let requests = cx.subscribe_in(
            &entity,
            window,
            move |this, _, req: &TermRequest, window, cx| {
                this.on_term_request(id, req.clone(), window, cx);
            },
        );
        self.push_tab(handle, vec![requests], cx);
        self.update_doctor_stats(cx);
        id
    }

    /// Drops the tracker owner of every closed tab whose job has no processes left.
    pub(super) fn prune_closed_owners(&mut self, cx: &mut Context<Self>) {
        if self.closed_owners.is_empty() {
            return;
        }
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
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
        self.tabs
            .iter()
            .filter(|t| t.handle.downcast::<TerminalTab>().is_some())
            .count()
    }

    /// Tells the doctor panel how many shells run and which grammars are available.
    pub(super) fn update_doctor_stats(&mut self, cx: &mut Context<Self>) {
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        let shells = self.terminal_count();
        let tree_tracked = {
            let tracker = services.tracker.lock().unwrap_or_else(|p| p.into_inner());
            self.tabs
                .iter()
                .filter(|t| t.handle.downcast::<TerminalTab>().is_some())
                .filter(|t| {
                    tracker.attachment_hint(t.handle.id().as_u64())
                        == proc_svc::TrackingMode::TreeOnly
                })
                .count()
        };
        let stats = ui_doctor::AppStats::new()
            .with_shells(shells)
            .with_grammars(self.grammars.clone())
            .with_tree_tracked(tree_tracked);
        services
            .doctor
            .update(cx, |d, cx| d.set_app_stats(stats, cx));
    }

    /// Starts a terminal in `cwd` and shows it (first empty pane, else the focused pane).
    pub(super) fn open_tab(
        &mut self,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.add_tab(cwd, false, window, cx);
        self.show_new_tab(id, window, cx);
    }

    /// A long-command toast was clicked: show the window and the tab that ran the command.
    pub(super) fn focus_tab_from_toast(
        &mut self,
        tab: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Another launch of the app (the Explorer entry) asked for a tab in `dir`: open it and bring
    /// the window forward.
    pub(crate) fn open_in_folder(
        &mut self,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.activate_window();
        if self.session.read(cx).restore_pending() {
            // Still restoring: the folder opens right after the session.
            self.session
                .update(cx, |s, _| s.set_open_after_restore(dir));
            return;
        }
        self.new_tab_in(dir, window, cx);
    }

    /// A tab's title, subtitle or status changed: repaint, and re-resolve the per-project layout
    /// (the active tab may have moved to another project).
    pub(super) fn on_tab_event(&mut self, cx: &mut Context<Self>) {
        // A deferred shell registers its process owner when it starts: refresh the doctor's
        // "tracked by tree" count (equality-guarded, so nothing repaints when it is the same).
        self.update_doctor_stats(cx);
        self.recompute_layout(true, cx);
        self.sync_chat_active(cx);
        self.mark_session_dirty(cx);
        self.refresh(cx);
    }

    pub fn select_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.slot(id).is_none() {
            return;
        }
        self.splits
            .panes
            .show_within(id, pane_count(self.eff.layout.splits));
        self.chrome.titlebar.update(cx, |t, cx| {
            t.close_tabs_menu(cx);
            t.reveal_active(cx);
        });
        self.after_active_changed(window, cx);
    }

    pub(super) fn focus_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.splits.panes.focused() != pane {
            self.splits.panes.focus_pane(pane);
            self.after_active_changed(window, cx);
        }
    }

    /// The focused tab changed: point the status bar at its git monitor, re-resolve the layout
    /// and give its content the keyboard focus.
    pub(super) fn after_active_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.refresh(cx);
    }

    /// Ctrl+W / the close buttons: asks first when the tab says so.
    pub fn request_close(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = self.slot(id) else { return };
        match slot.handle.can_close(cx) {
            CloseCheck::Allow => self.close_tab(id, window, cx),
            // [general] confirm_close_running = false: close without asking.
            CloseCheck::Confirm(_) if !self.config.general.confirm_close_running => {
                self.close_tab(id, window, cx)
            }
            CloseCheck::Confirm(why) => {
                self.prompts.confirm_close = Some((id, why));
                self.refresh(cx);
            }
            CloseCheck::Deny => {}
        }
    }

    /// Closes the tab now: the model releases its resources (PTY session, watchers, tasks) and the
    /// handle is dropped. The last tab closes the window.
    pub(super) fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|t| t.handle.id() == id) else {
            return;
        };
        let slot = self.tabs.remove(ix);
        if let Some(thread) = slot
            .handle
            .downcast::<ChatTab>()
            .and_then(|t| t.read(cx).thread_id())
        {
            self.chat.update(cx, |c, _| c.set_attention(thread, None));
        }
        if let Some(services) = cx.try_global::<Services>().cloned() {
            let target = FocusTarget::new(id.as_u64().to_string());
            services
                .attention
                .update(cx, |m, cx| m.remove_target(&target, cx));
        }
        // The owner stays in the tracker: closing the tab ends the shell (its root), so whatever
        // the shell started and left running becomes an orphan of that owner and shows up in the
        // procs panel and the status bar. It is dropped once the job is empty.
        self.closed_owners.push(id.as_u64());
        self.prune_closed_owners(cx);
        self.update_doctor_stats(cx);
        slot.handle.close(cx);
        drop(slot);
        self.prompts.confirm_close = None;
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        let remaining = self.tab_ids();
        self.splits.panes.remove(id, &remaining, ix);
        // The focused pane must show something.
        if self.splits.panes.focused_tab().is_none() {
            let next = remaining.get(ix).or_else(|| remaining.last()).copied();
            if let Some(t) = next {
                self.splits.panes.show(t);
            }
        }
        self.after_active_changed(window, cx);
    }

    pub(super) fn cycle_tab(&mut self, dir: isize, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.tab_ids();
        let cur = self
            .active_id()
            .and_then(|a| ids.iter().position(|i| *i == a))
            .unwrap_or(0);
        if let Some(id) = ids.get(cycle_index(ids.len(), cur, dir)).copied() {
            self.select_tab(id, window, cx);
        }
    }

    pub(super) fn jump_tab(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tab_ids().get(n).copied() {
            self.select_tab(id, window, cx);
        }
    }

    // ------------------------------------------------------------------------------------
    // Spaces of the unified sidebar
    // ------------------------------------------------------------------------------------

    /// Folds or unfolds a space of the unified sidebar.
    pub(crate) fn toggle_space(&mut self, key: PathBuf, cx: &mut Context<Self>) {
        if !self.spaces.folded.remove(&key) {
            self.spaces.folded.insert(key);
        }
        self.refresh(cx);
    }

    /// "+" of a space (or a click on a project without tabs): a new terminal in its folder.
    pub(crate) fn new_tab_in(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(db) = self.session.read(cx).db() {
            let _ = db.mark_project_opened(&root.to_string_lossy());
        }
        self.spaces.folded.remove(&root);
        self.open_tab(Some(root), window, cx);
    }

    /// Reads the favorite and recent projects off the UI thread; the sidebar lists them as spaces.
    pub(super) fn load_known_projects(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.session.read(cx).db().cloned() else {
            return;
        };
        self.spaces.task = Some(cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move {
                    let mut out: Vec<(PathBuf, String)> = Vec::new();
                    for f in db.favorites().unwrap_or_default() {
                        out.push((PathBuf::from(f.path), f.label));
                    }
                    let mut recent: Vec<_> = db
                        .projects()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|p| p.last_opened.is_some())
                        .collect();
                    recent.sort_by_key(|p| std::cmp::Reverse(p.last_opened));
                    for p in recent.into_iter().take(crate::spaces::MAX_CLOSED_SPACES) {
                        out.push((PathBuf::from(p.path), p.name));
                    }
                    out
                })
                .await;
            this.update(cx, |shell, cx| {
                if shell.spaces.known != list {
                    shell.spaces.known = list;
                    shell.sync_chat_projects(cx);
                    shell.refresh(cx);
                }
            })
            .ok();
        }));
    }

    /// The drop target of the file tree / viewer: opens the dropped files in the viewer.
    pub(crate) fn drop_on_viewer(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for f in crate::drop::split_for_viewer(paths).files {
            self.open_viewer(f, None, false, None, window, cx);
        }
    }

    /// A folder dropped on a space row of the sidebar: a new terminal tab in it.
    pub(crate) fn drop_on_space(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dir) = crate::drop::folder_to_open(paths) {
            self.new_tab_in(dir, window, cx);
        }
    }
}
