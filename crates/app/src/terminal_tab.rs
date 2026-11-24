//! A terminal as a [`TabModel`]: the shell lists it as a pill or as a sidebar row, and hosts its
//! [`TerminalView`] in a pane.
//!
//! Status comes from the shell integration: `Working` from the moment a non-empty command line is
//! submitted until OSC 133 `D` arrives, then `Done` (exit code 0 or unknown) or `Error`. The
//! subtitle is the git branch of the working directory, from the tab's own [`GitMonitor`]. The tab
//! owns the PTY session, the monitor and their tasks: closing the tab drops all of it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, SharedString, Subscription,
    Window,
};
use panel_api::{CloseCheck, Icon, RowStatus, TabEvent, TabIcon, TabModel};
use theme::Theme;

use crate::git_monitor::GitMonitor;
use crate::paths;
use crate::terminal_view::{TerminalEvent, TerminalView};

/// What a terminal asks of the shell beyond `TabEvent`s (the shell subscribes to the tab entity).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermRequest {
    /// Ctrl+click on a path that exists.
    OpenPath {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
        is_dir: bool,
    },
    /// "Arreglar con agente": a prompt already redacted, plus how many secrets were hidden.
    AskAgent { markdown: String, redactions: usize },
    /// A command ended in a way that deserves the user's attention (failed, long). The shell
    /// decides whether to queue it (not when the user is looking at the tab).
    Attention {
        id: String,
        label: String,
        kind: ui_review::AttentionKind,
    },
}

pub struct TerminalTab {
    view: Entity<TerminalView>,
    git: Entity<GitMonitor>,
    status: Option<RowStatus>,
    last_activity: Instant,
    _subs: Vec<Subscription>,
}

impl TerminalTab {
    /// Starts a shell in `cwd` (`None` = the process directory).
    pub fn new(
        theme: Theme,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The job must exist before the shell is spawned: the shell is born inside it.
        let job = crate::services::open_tab_job(cx.entity_id().as_u64(), cwd.as_deref(), cx);
        Self::with_view(
            cx.new(|cx| TerminalView::new_in_job(theme, cwd, job, window, cx)),
            cx,
        )
    }

    /// A restored tab: the shell starts the first time the tab is painted.
    pub fn new_deferred(
        theme: Theme,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let job = crate::services::open_tab_job(cx.entity_id().as_u64(), cwd.as_deref(), cx);
        Self::with_view(
            cx.new(|cx| TerminalView::new_deferred_in_job(theme, cwd, job, window, cx)),
            cx,
        )
    }

    fn with_view(view: Entity<TerminalView>, cx: &mut Context<Self>) -> Self {
        let git = cx.new(|_| GitMonitor::new());
        if let Some(dir) = view.read(cx).cwd().map(Path::to_path_buf) {
            git.update(cx, |g, cx| g.set_cwd(dir, cx));
        }
        let subs = vec![
            cx.subscribe(&view, |this, _, event: &TerminalEvent, cx| {
                this.on_terminal_event(event, cx)
            }),
            // The branch changed (or appeared): the subtitle is stale.
            cx.observe(&git, |_, _, cx| {
                cx.emit(TabEvent::SubtitleChanged);
                cx.notify();
            }),
        ];
        Self {
            view,
            git,
            status: None,
            last_activity: Instant::now(),
            _subs: subs,
        }
    }

    /// Types `text` at this terminal's prompt (see `TerminalView::send_text`).
    pub fn input(&self, text: &str, enter: bool, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.send_text(text, enter, cx));
    }

    /// The tab's git monitor (shared with the status bar while the tab is active).
    pub fn git(&self) -> &Entity<GitMonitor> {
        &self.git
    }

    fn set_status(&mut self, status: Option<RowStatus>, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        if self.status != status {
            self.status = status;
            cx.emit(TabEvent::StatusChanged);
            cx.notify();
        }
    }

    fn on_terminal_event(&mut self, event: &TerminalEvent, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::CwdChanged(cwd) => {
                self.git.update(cx, |g, cx| g.set_cwd(cwd.clone(), cx));
                cx.emit(TabEvent::TitleChanged);
                cx.notify();
            }
            TerminalEvent::CommandStarted => self.set_status(Some(RowStatus::Working), cx),
            TerminalEvent::BlockFinished(block) => {
                if let Some(kind) = attention_kind(block) {
                    let id = format!("block-{}-{}", cx.entity_id().as_u64(), block.id.0);
                    let mut label = self.title(cx).to_string();
                    if let Some(cmd) = block.command.as_deref() {
                        let (clean, _) = term_core::redact(cmd);
                        label = format!("{label} · {}", clean.chars().take(48).collect::<String>());
                    }
                    cx.emit(TermRequest::Attention { id, label, kind });
                }
                let policy = term_core::LongCommandPolicy::default();
                if term_core::should_notify(block, crate::notify::window_active(), &policy) {
                    crate::notify::long_command_toast(cx.entity_id().as_u64(), block);
                }
            }
            TerminalEvent::OpenPath {
                path,
                line,
                col,
                is_dir,
            } => cx.emit(TermRequest::OpenPath {
                path: path.clone(),
                line: *line,
                col: *col,
                is_dir: *is_dir,
            }),
            TerminalEvent::FixWithAgent { block, output } => {
                let branch = self.git.read(cx).info().map(|i| i.branch.clone());
                let ctx = term_core::agent_context(block, output, branch.as_deref());
                cx.emit(TermRequest::AskAgent {
                    markdown: ctx.to_prompt_markdown(),
                    redactions: ctx.redactions,
                });
            }
            TerminalEvent::CommandFinished(code) => {
                self.git.update(cx, |g, cx| g.refresh(cx));
                let status = match code {
                    Some(c) if *c != 0 => RowStatus::Error,
                    _ => RowStatus::Done,
                };
                self.set_status(Some(status), cx);
            }
        }
    }
}

/// What a finished block means for the attention queue: a failure, or a long success.
fn attention_kind(block: &term_core::Block) -> Option<ui_review::AttentionKind> {
    match block.state {
        term_core::BlockState::Failed => Some(ui_review::AttentionKind::Failed {
            exit_code: block.exit_code.unwrap_or(-1),
        }),
        term_core::BlockState::Succeeded
            if block
                .duration
                .is_some_and(|d| d >= term_core::LongCommandPolicy::default().min_duration) =>
        {
            Some(ui_review::AttentionKind::LongCommandDone)
        }
        _ => None,
    }
}

impl EventEmitter<TabEvent> for TerminalTab {}
impl EventEmitter<TermRequest> for TerminalTab {}

impl TabModel for TerminalTab {
    fn title(&self, cx: &App) -> SharedString {
        let view = self.view.read(cx);
        match view.cwd() {
            Some(cwd) => paths::tab_label(cwd).into(),
            None => view.shell_name().clone(),
        }
    }

    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Kind(Icon::Terminal)
    }

    fn subtitle(&self, cx: &App) -> Option<SharedString> {
        let git = self.git.read(cx);
        git.info().map(|i| i.branch.clone().into())
    }

    fn status(&self, _cx: &App) -> Option<RowStatus> {
        self.status
    }

    fn last_activity(&self, _cx: &App) -> Option<Instant> {
        Some(self.last_activity)
    }

    fn cwd(&self, cx: &App) -> Option<PathBuf> {
        self.view.read(cx).cwd().map(Path::to_path_buf)
    }

    fn project_root(&self, cx: &App) -> Option<PathBuf> {
        self.git.read(cx).info().map(|i| i.root.clone())
    }

    fn content(&self, _cx: &App) -> AnyView {
        self.view.clone().into()
    }

    fn can_close(&self, _cx: &App) -> CloseCheck {
        if self.status == Some(RowStatus::Working) {
            CloseCheck::Confirm(
                "Hay un comando en ejecución. Cerrar la pestaña lo terminará.".into(),
            )
        } else {
            CloseCheck::Allow
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        // Drops the PTY session (and with it the shell and its reader threads).
        self.view.update(cx, |v, _| v.shutdown());
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        self.view.read(cx).focus_handle.focus(window);
    }

    fn theme_changed(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.set_theme(theme, cx));
    }
}
