//! What the shell does for the panels and services around it: the git panel, the file tree and
//! viewer tabs, the review tab, terminal requests, the attention queue, the inline hint, the
//! command palette, the settings tab and the first-run wizard.

use super::*;
use crate::git_monitor::GitInfo;
use crate::hint::HintAction;
use crate::settings_tab::SettingsTab;
use crate::terminal_tab::TermRequest;
use crate::viewer_host;
use ui_git::{GitPanelEvent, OpFinished};
use ui_review::{AttentionItem, ReviewEvent, ReviewSource};
use ui_viewer::{GitMark, ViewerConfig};

/// The file tree's markers for the changed files of a repository (absolute paths).
fn git_marks(info: &GitInfo) -> HashMap<PathBuf, GitMark> {
    let mut out = HashMap::new();
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

impl AppShell {
    pub(super) fn settings_view(&self) -> Option<Entity<ui_settings::SettingsView>> {
        self.modes.settings.as_ref().map(|(v, _)| v.clone())
    }

    pub(super) fn onboarding_view(&self) -> Option<Entity<ui_onboarding::OnboardingView>> {
        self.modes.onboarding.as_ref().map(|(v, _)| v.clone())
    }

    /// Wires the git panel, the file tree and the grammar plugins to the shell. Nothing here reads
    /// a repository or a folder: the panels do that when they are first shown.
    pub(super) fn mount_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        // Closed tabs keep their job until it is empty (orphans); the process model notifies on job
        // events, which is when to check.
        self._subs
            .push(cx.observe(&services.procs, |this, _, cx| this.prune_closed_owners(cx)));
        // Git panel: its events and the end of its operations.
        self._subs.push(cx.subscribe_in(
            &services.git,
            window,
            |this, _, ev: &GitPanelEvent, window, cx| {
                this.on_git_event(ev.clone(), window, cx);
            },
        ));
        self._subs.push(cx.subscribe_in(
            &services.git,
            window,
            |this, _, _: &OpFinished, _, cx| {
                this.after_git_operation(cx);
            },
        ));
        // Attention queue: a click on an item brings its tab forward.
        self._subs.push(cx.subscribe_in(
            &services.attention,
            window,
            |this, _, ev: &AttentionEvent, window, cx| match ev {
                AttentionEvent::Jump(target) => this.focus_attention_target(target, window, cx),
                AttentionEvent::Dismiss(_) | AttentionEvent::Changed { .. } => this.refresh(cx),
            },
        ));
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
            self.tasks.viewer = Some(cx.spawn_in(window, async move |this, cx| {
                while let Some(ev) = rx.next().await {
                    if this
                        .update_in(cx, |shell, window, cx| {
                            shell.on_viewer_event(ev, window, cx)
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        // Grammar plugins: scanned off the UI thread; documents opened before it ends are plain.
        self.tasks.grammar = Some(cx.spawn(async move |this, cx| {
            let ids = cx
                .background_executor()
                .spawn(async { viewer_host::init_grammars() })
                .await;
            this.update(cx, |shell, cx| {
                shell.grammars = ids;
                shell.update_doctor_stats(cx);
            })
            .ok();
        }));
        self.sync_git_marks(cx);
    }

    // ------------------------------------------------------------------------------------
    // Git panel, file viewer, terminal requests
    // ------------------------------------------------------------------------------------

    /// The user is looking at the active tab: whatever it had in the attention queue is seen.
    pub(super) fn clear_attention_of_active(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id() else { return };
        if let Some(services) = cx.try_global::<Services>().cloned() {
            let target = FocusTarget::new(id.as_u64().to_string());
            services
                .attention
                .update(cx, |m, cx| m.remove_target(&target, cx));
        }
    }

    fn focus_attention_target(
        &mut self,
        target: &FocusTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.activate_window();
        if let Some(id) = self
            .tab_ids()
            .into_iter()
            .find(|id| id.as_u64().to_string() == target.0.as_ref())
        {
            self.select_tab(id, window, cx);
        }
    }

    /// Ctrl+Shift+A: jump to the next thing that needs attention.
    pub(super) fn next_attention(&mut self, cx: &mut Context<Self>) {
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services.attention.update(cx, |m, cx| {
                m.jump_next(cx);
            });
        }
    }

    /// Opens a review tab (diff with line notes) of the active tab's project, or brings the one
    /// that already shows it forward.
    pub(super) fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let info = self.active_info(cx);
        let Some(root) = info
            .as_ref()
            .and_then(|i| i.project_root.clone().or_else(|| i.cwd.clone()))
        else {
            self.show_hint(Hint::text("No hay un proyecto abierto que revisar"), cx);
            return;
        };
        let existing = self.tabs.iter().find_map(|t| {
            let tab = t.handle.downcast::<ReviewTab>()?;
            matches!(tab.read(cx).source(), ReviewSource::Worktree { root: r } if *r == root)
                .then(|| t.handle.id())
        });
        if let Some(id) = existing {
            self.select_tab(id, window, cx);
            return;
        }
        let theme = self.theme;
        let entity = cx.new(|cx| ReviewTab::new(ReviewSource::Worktree { root }, theme, cx));
        let handle = TabHandle::new(entity.clone());
        let tab_id = handle.id();
        let events = cx.subscribe_in(
            &entity,
            window,
            move |this, tab, ev: &ReviewEvent, window, cx| match ev {
                ReviewEvent::SendToAgent { markdown, notes } => {
                    // The notes go through the redactor like any other context sent to an agent.
                    let (clean, hidden) = term_core::redact(markdown);
                    let cwd = tab.read(cx).source().root().to_path_buf();
                    let ids: Vec<_> = notes.iter().map(|n| n.id).collect();
                    this.send_to_agent(clean, hidden, Some(cwd), window, cx);
                    let view = tab.read(cx).view().clone();
                    view.update(cx, |v, cx| v.mark_sent(&ids, cx));
                }
                ReviewEvent::NotesChanged { .. } => {}
            },
        );
        self.push_tab(handle, vec![events], cx);
        self.show_new_tab(tab_id, window, cx);
    }

    /// Points the file tree's `M` / `A` / `?` markers at the active terminal's repository, and
    /// keeps them current while that repository's monitor reports changes.
    pub(super) fn sync_git_marks(&mut self, cx: &mut Context<Self>) {
        let git = self.active_git(cx);
        // A viewer tab in front keeps the last markers (the tree still shows that project).
        let Some(git) = git else { return };
        self.marks_sub = Some(cx.observe(&git, |this, git, cx| this.feed_git_marks(&git, cx)));
        self.feed_git_marks(&git, cx);
    }

    fn active_git(&self, cx: &App) -> Option<Entity<GitMonitor>> {
        self.active_terminal().map(|t| t.read(cx).git().clone())
    }

    fn feed_git_marks(&mut self, git: &Entity<GitMonitor>, cx: &mut Context<Self>) {
        let Some(info) = git.read(cx).info().cloned() else {
            return;
        };
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        services.files.set_git_status(git_marks(&info), cx);
    }

    /// A git operation of the panel ended: the status bar and the tree re-read the repository.
    pub(super) fn after_git_operation(&mut self, cx: &mut Context<Self>) {
        if let Some(git) = self.active_git(cx) {
            git.update(cx, |g, cx| g.refresh(cx));
        }
        if let Some(services) = cx.try_global::<Services>().cloned() {
            services.files.refresh(cx);
        }
    }

    fn on_git_event(&mut self, event: GitPanelEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GitPanelEvent::RunInTerminal { argv, cwd } => {
                self.run_in_terminal(&argv, &cwd, window, cx)
            }
            GitPanelEvent::OpenFile { path, line } => {
                self.open_viewer(path, line, false, None, window, cx)
            }
            GitPanelEvent::OpenDiff { path } => {
                self.open_viewer(path, None, true, None, window, cx)
            }
            GitPanelEvent::AskAgent { context_markdown } => {
                // Backlog A13: nothing leaves the machine without going through the redactor.
                let (clean, hidden) = term_core::redact(&context_markdown);
                let cwd = cx
                    .try_global::<Services>()
                    .and_then(|s| s.git.read(cx).root().map(PathBuf::from));
                self.send_to_agent(clean, hidden, cwd, window, cx);
            }
            GitPanelEvent::WorktreeSelected { path } => {
                let text = format!("Worktree {}", crate::paths::tab_label(&path));
                self.show_hint(
                    Hint::text(text).with_action("cd aquí", HintAction::Cd(path)),
                    cx,
                );
            }
        }
    }

    /// Types a confirmed git command in the active terminal (or a new one when the active tab is
    /// not a terminal). `-C` targets the panel's worktree without changing the shell's directory.
    fn run_in_terminal(
        &mut self,
        argv: &[String],
        cwd: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active_cwd(cx);
        let line = palette::shell_line(&palette::with_dash_c(argv, cwd, active.as_deref()));
        if self.active_terminal().is_none() {
            self.open_tab(Some(cwd.to_path_buf()), window, cx);
        }
        self.type_in_active(&line, true, cx);
    }

    pub(super) fn active_terminal(&self) -> Option<Entity<TerminalTab>> {
        self.active_id()
            .and_then(|id| self.slot(id))
            .and_then(|s| s.handle.downcast::<TerminalTab>())
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
        let secrets = if redactions > 0 {
            format!(" ({})", crate::block_ui::secrets_hint(redactions))
        } else {
            String::new()
        };
        self.show_hint(
            Hint::text(format!(
                "Contexto copiado{secrets} · pégalo en el agente con Ctrl+V"
            )),
            cx,
        );
    }

    pub(super) fn on_term_request(
        &mut self,
        tab: TabId,
        req: TermRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match req {
            TermRequest::OpenPath {
                path, line, is_dir, ..
            } => {
                if is_dir {
                    if let Some(t) = self
                        .slot(tab)
                        .and_then(|s| s.handle.downcast::<TerminalTab>())
                    {
                        let line = palette::cd_line(&path);
                        t.update(cx, |t, cx| t.input(&line, true, cx));
                    }
                } else {
                    self.open_viewer(path, line.map(|l| l as usize), false, None, window, cx);
                }
            }
            TermRequest::Attention { id, label, kind } => {
                // Nothing to flag when the user is looking at that tab right now.
                if self.active_id() == Some(tab) && notify::window_active() {
                    return;
                }
                let Some(services) = cx.try_global::<Services>().cloned() else {
                    return;
                };
                let item = AttentionItem {
                    id: id.into(),
                    owner_label: label.into(),
                    agent: None,
                    kind,
                    since: Instant::now(),
                    focus_target: FocusTarget::new(tab.as_u64().to_string()),
                };
                services.attention.update(cx, |m, cx| m.upsert(item, cx));
            }
            TermRequest::AskAgent {
                markdown,
                redactions,
            } => {
                let cwd = self.slot(tab).and_then(|s| s.handle.info(cx).cwd);
                self.send_to_agent(markdown, redactions, cwd, window, cx);
            }
        }
    }

    fn on_viewer_event(&mut self, event: ViewerEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ViewerEvent::OpenFile { path } => self.open_viewer(path, None, false, None, window, cx),
            ViewerEvent::OpenDiff { path, base } => {
                self.open_viewer(path, None, true, base, window, cx)
            }
            ViewerEvent::OpenInEditor { path, line } => {
                let editor = self.config.git.external_editor.clone();
                cx.spawn(async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn(async move { viewer_host::spawn_editor(&path, line, &editor) })
                        .await;
                    if let Err(e) = res {
                        this.update(cx, |shell, cx| shell.show_hint(Hint::text(e), cx))
                            .ok();
                    }
                })
                .detach();
            }
            // The banner of a file without highlighting: Ajustes > Plugins with that grammar open.
            ViewerEvent::InstallGrammar { id } => {
                self.open_settings(window, cx);
                if let Some(settings) = self.settings_view() {
                    settings.update(cx, |s, cx| {
                        s.handle(&ui_settings::SettingsEvent::InstallGrammar { id }, cx)
                    });
                }
            }
        }
    }

    /// Opens `path` in a viewer tab of the main zone (or brings the one that shows it forward).
    /// `base`: a checkpoint commit for diffs (HEAD when `None`).
    pub(super) fn open_viewer(
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
                tab.read(cx)
                    .viewer()
                    .clone()
                    .update(cx, |v, cx| v.goto_line(line, cx));
            }
            self.select_tab(id, window, cx);
            return;
        }
        let root = ui_viewer::find_repo_root(&path)
            .or_else(|| self.active_info(cx).and_then(|i| i.project_root));
        let mut cfg = ViewerConfig {
            project_root: root,
            line,
            open_in_diff: diff,
            ..ViewerConfig::default()
        };
        if let Some(sha) = base {
            cfg.base_label =
                Some(format!("checkpoint {}", sha.chars().take(7).collect::<String>()).into());
            cfg.diff_base = viewer_svc::GitBase::Commit(sha);
        }
        let theme = self.theme;
        let entity = cx.new(|cx| ViewerTab::new(path, cfg, theme, cx));
        let handle = TabHandle::new(entity.clone());
        let events = cx.subscribe_in(&entity, window, |this, _, ev: &ViewerEvent, window, cx| {
            this.on_viewer_event(ev.clone(), window, cx);
        });
        let id = handle.id();
        self.push_tab(handle, vec![events], cx);
        self.show_new_tab(id, window, cx);
    }

    /// The drop target of the file tree / viewer: opens the dropped files in the viewer.
    pub(super) fn viewer_drop_zone(
        &self,
        id: &'static str,
        radius: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shell = cx.entity();
        ui_kit::DropZone::new(id, &self.theme, "Soltar para abrir en el visor")
            .radius(radius)
            .on_drop(move |paths, window, cx| {
                let paths = paths.to_vec();
                shell.update(cx, |s, cx| s.drop_on_viewer(&paths, window, cx));
            })
    }

    // ------------------------------------------------------------------------------------
    // Inline hint
    // ------------------------------------------------------------------------------------

    /// Shows `hint` above the status bar for a few seconds (replacing the previous one).
    pub(super) fn show_hint(&mut self, hint: Hint, cx: &mut Context<Self>) {
        self.prompts.hint = Some(hint);
        // Replacing the task restarts the one-shot timer.
        self.prompts.hint_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HINT_FOR).await;
            this.update(cx, |shell, cx| {
                shell.prompts.hint = None;
                shell.prompts.hint_task = None;
                shell.refresh(cx);
            })
            .ok();
        }));
        self.refresh(cx);
    }

    pub(super) fn dismiss_hint(&mut self, cx: &mut Context<Self>) {
        if self.prompts.hint.take().is_some() {
            self.prompts.hint_task = None;
            self.refresh(cx);
        }
    }

    pub(super) fn run_hint_action(&mut self, action: HintAction, cx: &mut Context<Self>) {
        match action {
            HintAction::Cd(dir) => self.type_in_active(&palette::cd_line(&dir), true, cx),
        }
        self.dismiss_hint(cx);
    }

    // ------------------------------------------------------------------------------------
    // Command palette
    // ------------------------------------------------------------------------------------

    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn type_in_active(&mut self, text: &str, enter: bool, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_terminal() {
            tab.update(cx, |t, cx| t.input(text, enter, cx));
        }
    }

    /// Handles what the palette picked (it emits and closes; focus is already back on the terminal).
    pub(super) fn on_palette(
        &mut self,
        event: ui_palette::PaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ui_palette::PaletteEvent as E;
        match event {
            E::RunCommand { line, cwd, new_tab } => {
                if new_tab {
                    self.open_tab(cwd, window, cx);
                } else if let Some(dir) = cwd.filter(|d| Some(d) != self.active_cwd(cx).as_ref()) {
                    self.type_in_active(&palette::cd_line(&dir), true, cx);
                }
                self.type_in_active(&line, true, cx);
            }
            E::InsertText(text) => self.type_in_active(&text, false, cx),
            E::OpenPath { path, new_tab } => {
                if new_tab {
                    self.open_tab(Some(path), window, cx);
                } else {
                    self.type_in_active(&palette::cd_line(&path), true, cx);
                }
            }
            E::FocusTab(id) => self.select_tab(id, window, cx),
            E::ApplyPreset(preset) => {
                let mut layout = self.config.layout.clone();
                layout.select_preset(preset);
                self.set_layout_config(layout, true, cx);
            }
            E::GitAction(action) => {
                // The command is typed where the user can see it. Sensitive ones (commit, push,
                // pull, switch...) are left at the prompt without Enter: pressing it is the
                // confirmation.
                let line = palette::shell_line(&action.argv());
                let sensitive = action.is_sensitive();
                self.type_in_active(&line, !sensitive, cx);
            }
            E::Action(id) => self.run_action(id.as_ref(), window, cx),
            E::Closed => {
                // Focus went back to what had it; make sure that is the terminal.
                if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
                    slot.handle.focus(window, cx);
                }
            }
        }
    }

    fn run_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            action_id::TAB_NEW => self.open_new_tab_menu(window, cx),
            action_id::TAB_NEW_TERMINAL => self.new_tab_here(window, cx),
            action_id::CHAT_NEW => {
                let project = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd));
                self.new_chat_tab(project, window, cx);
            }
            action_id::TAB_CLOSE => {
                if let Some(t) = self.active_id() {
                    self.request_close(t, window, cx);
                }
            }
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
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Settings and the first-run wizard
    // ------------------------------------------------------------------------------------

    /// Opens the settings tab (creating the view the first time), or brings it forward.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modes.onboarding.is_some() {
            return;
        }
        let existing = self
            .tabs
            .iter()
            .find(|t| t.handle.downcast::<SettingsTab>().is_some())
            .map(|t| t.handle.id());
        if let Some(id) = existing {
            self.select_tab(id, window, cx);
            return;
        }
        let view = self.ensure_settings(window, cx);
        // The settings live in the main zone: they get that zone's real width class.
        let class = self.main_class();
        self.host.main_class = class;
        let ctx = self.main_ctx(class, cx);
        let Some(provider) = cx
            .try_global::<PanelRegistry>()
            .and_then(|r| r.get(&PanelId::new(ui_settings::PANEL_ID)))
        else {
            return;
        };
        let content = provider.build(ctx, window, cx);
        let entity = cx.new(|_| SettingsTab::new(view, content));
        let handle = TabHandle::new(entity);
        let id = handle.id();
        self.push_tab(handle, Vec::new(), cx);
        self.show_new_tab(id, window, cx);
    }

    /// Mounts the settings: its view, the `settings` panel provider and the event wiring.
    fn ensure_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ui_settings::SettingsView> {
        if let Some(v) = self.settings_view() {
            return v;
        }
        let path = store::paths::config_path().unwrap_or_default();
        let options = ui_settings::SettingsOptions::new(path, env!("CARGO_PKG_VERSION"));
        let view = ui_settings::settings_view((*self.config).clone(), self.theme, options, cx);
        let provider = ui_settings::SettingsPanelProvider::new(view.clone());
        cx.update_global::<PanelRegistry, _>(|registry, _| registry.register(provider));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, ev: &ui_settings::SettingsEvent, window, cx| {
                this.on_settings_event(ev.clone(), window, cx);
            },
        );
        self.modes.settings = Some((view.clone(), sub));
        view
    }

    fn on_settings_event(
        &mut self,
        event: ui_settings::SettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ui_settings::SettingsEvent as E;
        match event {
            E::ConfigChanged(config) => self.adopt_config(config, cx),
            E::OpenLayoutPanel => {
                if self.layout_panel.is_none() {
                    self.toggle_layout_panel(window, cx);
                }
            }
            E::RerunOnboarding => self.start_onboarding(window, cx),
            E::PluginInstalled { id, restart } => {
                let tail = if restart {
                    " (se activa al reiniciar)"
                } else {
                    ""
                };
                self.show_hint(Hint::text(format!("Plugin {id} instalado{tail}")), cx);
            }
            E::PluginRemoved { id, restart } => {
                let tail = if restart {
                    " (se quita al reiniciar)"
                } else {
                    ""
                };
                self.show_hint(Hint::text(format!("Plugin {id} quitado{tail}")), cx);
            }
            E::SaveFailed(msg) => self.show_hint(
                Hint::text(format!("No se pudo guardar config.toml: {msg}")),
                cx,
            ),
            E::Saved | E::InstallGrammar { .. } => {}
        }
    }

    /// Shows the wizard instead of the shell (first run, or "Volver a configurar").
    pub(super) fn start_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modes.onboarding.is_some() {
            return;
        }
        let db = self.session.read(cx).db().cloned();
        let view = ui_onboarding::onboarding((*self.config).clone(), db, self.theme, cx);
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, ev: &ui_onboarding::OnboardingEvent, window, cx| {
                this.on_onboarding_event(ev.clone(), window, cx);
            },
        );
        view.read(cx).focus(window);
        self.modes.onboarding = Some((view, sub));
        self.refresh(cx);
    }

    fn on_onboarding_event(
        &mut self,
        event: ui_onboarding::OnboardingEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ui_onboarding::OnboardingEvent as E;
        match event {
            E::Finished { config, .. } => {
                self.end_onboarding(window, cx);
                // The wizard already saved config.toml; adopt it (a "System" theme is dark here).
                self.adopt_config(Arc::new(config.clone()), cx);
                self.set_theme(theme_for(&config), cx);
                if let Some(settings) = self.settings_view() {
                    settings.update(cx, |s, cx| s.set_config(config, cx));
                }
                self.load_known_projects(cx);
            }
            E::Skipped => self.end_onboarding(window, cx),
            E::OpenLoginTerminal { command, .. } => {
                let cwd = self.active_cwd(cx);
                self.open_tab(cwd, window, cx);
                self.type_in_active(&command, true, cx);
            }
            // A live preview: the window follows the choice, the config is untouched.
            E::ThemePreview(mode) => self.set_theme(themed(mode, &self.config), cx),
        }
    }

    fn end_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modes.onboarding = None;
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
        self.refresh(cx);
    }
}
