//! The terminal view entity: owns the PTY session, pumps its events, handles input.
//!
//! Painting lives in [`crate::terminal_element`].

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use gpui::{
    App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    Hsla, KeyDownEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollWheelEvent, SharedString, Task, UTF16Selection, Window,
    div, font, prelude::*, px,
};
use store::config::{Config, CursorStyle, Profile};
use term_core::{
    Block, JobAttachment, KeyInput, Palette, Rgb8, Session, SessionConfig, SessionEvent,
    SpawnOptions, encode_key,
};
use theme::{Radius, TerminalColors, Theme, Typography};

use crate::app_shell::{RAIL_W, TAB_H, TERM_PAD};
use crate::block_ui::BlockUi;
use crate::status_bar::STATUS_H;
use crate::terminal_element::{PaintState, TerminalElement};

/// What a terminal tells the rest of the window (breadcrumb, tab label, git pill).
#[derive(Clone, Debug)]
pub enum TerminalEvent {
    /// The shell's working directory changed (OSC 7).
    CwdChanged(PathBuf),
    /// A command block started (OSC 133 `C`): the tab is `Working`.
    CommandStarted,
    /// A command block finished, with its exit code when the shell reported one.
    CommandFinished(Option<i32>),
    /// The finished block itself (command, duration, exit code), for notifications.
    BlockFinished(Block),
    /// Ctrl+click on a path of the output that exists on disk.
    OpenPath {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
        is_dir: bool,
    },
    /// "Arreglar con agente" on a failed block: the block and its output (not redacted yet).
    FixWithAgent { block: Block, output: String },
}

/// The process job a terminal's shell is born in (`proc_svc` owner of its tab): the shell and every
/// process it starts are tracked, and what outlives the tab is reported as an orphan.
#[derive(Clone)]
pub struct JobCtx {
    pub tracker: ui_procs::SharedTracker,
    /// Owner id in the tracker (the tab's entity id).
    pub owner: u64,
    /// Raw handle of the owner's job (owned by the tracker; valid until the owner is removed).
    pub handle: isize,
}

/// Environment the shell integration reads (`[terminal.completion]` of `config.toml`). The shell
/// installs it as a global and refreshes it on every config reload, so new tabs pick up changes.
#[derive(Default)]
pub struct TermEnv(pub Vec<(String, String)>);

impl gpui::Global for TermEnv {}

/// The `[terminal]` settings and the shell `[[profiles]]` of `config.toml`. The shell installs it as
/// a global and refreshes it on every config reload. New tabs read the shell, the profile and the
/// scrollback when they are built; the cursor, copy-on-select and Ctrl+V are read live, every time
/// they matter.
#[derive(Clone, Debug, PartialEq)]
pub struct TermSettings {
    /// `terminal.default_profile`: name of the profile new tabs open (empty = automatic).
    pub default_profile: String,
    pub profiles: Vec<Profile>,
    /// `terminal.scrollback`, as written (clamped by [`TermSettings::scrollback_lines`]).
    pub scrollback: u32,
    pub cursor_style: CursorStyle,
    pub copy_on_select: bool,
    pub paste_ctrl_v: bool,
}

impl Default for TermSettings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl gpui::Global for TermSettings {}

/// Most scrollback lines `config.toml` accepts (`Config::validate`).
pub const MAX_SCROLLBACK: usize = 1_000_000;

impl TermSettings {
    pub fn from_config(config: &Config) -> Self {
        let t = &config.terminal;
        Self {
            default_profile: t.default_profile.clone(),
            profiles: config.profiles.clone(),
            scrollback: t.scrollback,
            cursor_style: t.cursor_style,
            copy_on_select: t.copy_on_select,
            paste_ctrl_v: t.paste_ctrl_v,
        }
    }

    /// Scrollback lines a new session keeps, within what the config accepts.
    pub fn scrollback_lines(&self) -> usize {
        (self.scrollback as usize).min(MAX_SCROLLBACK)
    }

    /// The settings in force: the shell's global, or the defaults (examples, tests).
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }
}

/// What a new tab runs.
#[derive(Debug, PartialEq)]
pub(crate) struct ShellPlan {
    pub program: String,
    pub args: Vec<String>,
    /// The profile's starting folder (only used when the tab has no folder of its own).
    pub cwd: Option<PathBuf>,
    /// The profile's environment variables (they win over the app's).
    pub env: Vec<(String, String)>,
    /// Shown over the terminal: the profile could not be used.
    pub hint: Option<String>,
    /// Started instead when the profile's program fails to spawn.
    pub fallback: Option<(String, Vec<String>)>,
}

/// Is `program` a PowerShell (it then gets the shell integration of the automatic shell)?
fn is_powershell(program: &str) -> bool {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim_matches('"')
        .to_ascii_lowercase();
    matches!(name.trim_end_matches(".exe"), "pwsh" | "powershell")
}

/// Picks the shell of a new tab: `SELACHI_SHELL` (a developer override) first, then the profile
/// named by `terminal.default_profile`, else the automatic shell. A profile whose program does
/// not exist falls back to the automatic shell with a hint, so a tab is never dead.
pub(crate) fn resolve_shell(
    settings: &TermSettings,
    override_program: Option<String>,
    exists: impl Fn(&str) -> bool,
    automatic: impl Fn() -> (String, Vec<String>),
) -> ShellPlan {
    let plain = |(program, args): (String, Vec<String>), hint: Option<String>| ShellPlan {
        program,
        args,
        cwd: None,
        env: Vec::new(),
        hint,
        fallback: None,
    };
    // `SELACHI_SHELL=powershell.exe` picks another PowerShell (same shell integration). Useful
    // because the Microsoft Store `pwsh.exe` (an app execution alias) runs in the package's own
    // job and cannot be put into a process job: its children are then not tracked.
    if let Some(program) = override_program {
        return plain((program, automatic().1), None);
    }
    let wanted = settings.default_profile.trim();
    let profile = (!wanted.is_empty())
        .then(|| {
            settings
                .profiles
                .iter()
                .find(|p| p.name.trim().eq_ignore_ascii_case(wanted))
        })
        .flatten();
    let Some(profile) = profile else {
        return plain(automatic(), None);
    };
    let (auto_program, auto_args) = automatic();
    if !exists(&profile.program) {
        let hint = format!(
            "El perfil «{}» usa «{}», que no existe: se abrió {} en su lugar.",
            profile.name.trim(),
            profile.program.trim(),
            auto_program.strip_suffix(".exe").unwrap_or(&auto_program),
        );
        return plain((auto_program, auto_args), Some(hint));
    }
    let args = if profile.args.is_empty() && is_powershell(&profile.program) {
        auto_args.clone()
    } else {
        profile.args.clone()
    };
    ShellPlan {
        program: profile.program.trim().trim_matches('"').to_string(),
        args,
        cwd: profile
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(PathBuf::from),
        env: profile
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        hint: None,
        fallback: Some((auto_program, auto_args)),
    }
}

/// Size of one terminal cell in logical pixels, snapped to the device pixel grid.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub width: Pixels,
    pub height: Pixels,
    scale: f32,
}

/// Measures the cell size from the mono font: advance of `m` and `size * line_height`.
pub fn measure_cell(window: &Window, ty: &Typography) -> CellMetrics {
    let scale = window.scale_factor();
    let ts = window.text_system();
    let font_id = ts.resolve_font(&font(ty.mono_family));
    let advance = ts
        .advance(font_id, px(ty.mono_size), 'm')
        .map(|s| f32::from(s.width))
        .unwrap_or_else(|e| {
            log::warn!("could not measure mono advance, guessing: {e}");
            ty.mono_size * 0.6
        });
    let snap = |v: f32| (v * scale).round().max(1.0) / scale;
    CellMetrics {
        width: px(snap(advance)),
        height: px(snap(ty.mono_size * ty.mono_line_height)),
        scale,
    }
}

/// Converts a `0xRRGGBB` color into a term_core color.
fn rgb8(hex: u32) -> Rgb8 {
    Rgb8::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Builds the term_core palette from the theme's terminal colors.
pub fn palette_from(t: &TerminalColors) -> Palette {
    Palette {
        ansi: t.ansi.map(rgb8),
        foreground: rgb8(t.foreground),
        background: rgb8(t.background),
        cursor: rgb8(t.cursor),
    }
}

/// Converts a term_core color into a gpui color.
pub fn hsla(c: Rgb8) -> Hsla {
    gpui::rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into()
}

/// `SELACHI_PARENT_PERMISSION` of a terminal's shell: a delegation started from a terminal never has
/// more than "ask" (the user is at the keyboard and the approvals go to the attention queue).
const PARENT_PERMISSION: &str = "ask";

/// How a tab's shell is spawned: the app's extra environment, the job it is born in and, for a tab,
/// `SELACHI_PARENT_TAB=<tab id>` / `SELACHI_PARENT_PERMISSION=ask`.
fn spawn_options(
    env: Vec<(String, String)>,
    job: Option<isize>,
    tab_id: Option<u64>,
) -> SpawnOptions {
    let options = SpawnOptions { env, job };
    match tab_id {
        Some(tab) => options.with_parent_tab(tab, PARENT_PERMISSION),
        None => options,
    }
}

pub struct TerminalView {
    pub(crate) session: Option<Session>,
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    /// Last known application-cursor mode (DECCKM), updated on every paint.
    pub(crate) app_cursor: Cell<bool>,
    /// Last known scrollback offset, updated on every paint.
    pub(crate) display_offset: Cell<usize>,
    /// Last painted cursor cell, used to place the IME candidate window.
    pub(crate) cursor_bounds: Cell<Option<Bounds<Pixels>>>,
    /// The part of that cell the cursor covers (its shape follows `terminal.cursor_style`).
    pub(crate) cursor_shape: Cell<Option<Bounds<Pixels>>>,
    /// Snapshot buffer and shaped-row cache, reused between frames.
    pub(crate) paint_state: RefCell<PaintState>,
    metrics: Cell<Option<CellMetrics>>,
    shell_name: SharedString,
    /// Last known working directory (starts as the process cwd, then follows OSC 7).
    cwd: Option<PathBuf>,
    exited: bool,
    got_output: bool,
    spawn_error: Option<SharedString>,
    scroll_accum: f32,
    /// In-progress IME / dead-key composition (not rendered yet).
    marked: Option<String>,
    /// A deferred session (restored tabs): spawned the first time the view is rendered, so tabs
    /// that are never shown cost no shell process.
    pending: Option<SessionConfig>,
    /// Text sent to a tab before its shell exists; written as soon as it starts.
    queued: Vec<u8>,
    /// The shell is being created on a background thread (see [`TerminalView::start_session`]).
    starting: bool,
    /// The view was shut down: a shell that finishes starting afterwards is dropped.
    closed: bool,
    _start: Option<Task<()>>,
    _pump: Option<Task<()>>,
    /// The job the shell is born in, if the app tracks processes.
    job: Option<JobCtx>,
    /// The id of the tab this shell belongs to (`TabId::as_u64`). Exported to the shell as
    /// `SELACHI_PARENT_TAB` (with `SELACHI_PARENT_PERMISSION`) so a `selachi-agent run` started
    /// from here hangs from this tab in the agent map.
    tab_id: Option<u64>,
    /// Command blocks: gutter, folding, the hover bar and Ctrl+click paths.
    pub(crate) block_ui: BlockUi,
    /// The profile's environment variables (over the app's).
    profile_env: Vec<(String, String)>,
    /// What runs when the profile's program fails to start.
    fallback: Option<(String, Vec<String>)>,
    /// A note over the terminal (the profile could not be used); dropped on the first key.
    hint: Option<SharedString>,
    /// The left button is down and extending a text selection.
    selecting: bool,
    /// Something is selected (so typing clears it).
    has_selection: bool,
}

impl TerminalView {
    /// `cwd` is the directory the shell starts in (`None` = the process directory).
    #[allow(dead_code)] // the `empty_window` example has no job tracker
    pub fn new(
        theme: Theme,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(theme, cwd, false, None, None, window, cx)
    }

    /// Like [`TerminalView::new`], with the shell born in `job`.
    pub fn new_in_job(
        theme: Theme,
        cwd: Option<PathBuf>,
        job: Option<JobCtx>,
        tab_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(theme, cwd, false, job, tab_id, window, cx)
    }

    /// Like [`TerminalView::new`] but the shell starts the first time the view is painted.
    #[allow(dead_code)] // the `empty_window` example has no job tracker
    pub fn new_deferred(
        theme: Theme,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(theme, cwd, true, None, None, window, cx)
    }

    /// Deferred, with the shell born in `job` when it starts.
    pub fn new_deferred_in_job(
        theme: Theme,
        cwd: Option<PathBuf>,
        job: Option<JobCtx>,
        tab_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(theme, cwd, true, job, tab_id, window, cx)
    }

    fn build(
        theme: Theme,
        cwd: Option<PathBuf>,
        defer: bool,
        job: Option<JobCtx>,
        tab_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        log::info!(
            "[startup] TerminalView::new at {} since process start",
            crate::mem::uptime_label()
        );
        let focus_handle = cx.focus_handle();
        if !defer {
            focus_handle.focus(window);
        }

        let metrics = measure_cell(window, &theme.typography);
        // Estimate the terminal grid from the window so the shell starts at the right size.
        let vp = window.viewport_size();
        let avail_w = f32::from(vp.width) - RAIL_W - 2.0 * TERM_PAD;
        let avail_h = f32::from(vp.height) - TAB_H - STATUS_H - 2.0 * TERM_PAD;
        let cols = (avail_w / f32::from(metrics.width))
            .floor()
            .clamp(10.0, 500.0) as u16;
        let rows = (avail_h / f32::from(metrics.height))
            .floor()
            .clamp(3.0, 300.0) as u16;

        let settings = TermSettings::current(cx);
        let ShellPlan {
            program,
            args,
            cwd: profile_cwd,
            env: profile_env,
            hint,
            fallback,
        } = resolve_shell(
            &settings,
            std::env::var_os("SELACHI_SHELL").and_then(|p| p.into_string().ok()),
            term_core::program_exists,
            term_core::default_shell,
        );
        // A folder the tab was opened in (the active tab's, a restored one) wins over the
        // profile's starting folder.
        let cwd = cwd.or_else(|| profile_cwd.filter(|c| c.is_dir()));
        let shell_name: SharedString = program
            .strip_suffix(".exe")
            .unwrap_or(&program)
            .to_string()
            .into();
        let cfg = SessionConfig {
            program,
            args,
            cwd: cwd.clone(),
            cols,
            rows,
            cell_width: f32::from(metrics.width) as u16,
            cell_height: f32::from(metrics.height) as u16,
            palette: palette_from(&theme.terminal),
            scrollback: settings.scrollback_lines(),
        };

        let mut view = Self {
            session: None,
            theme,
            focus_handle,
            app_cursor: Cell::new(false),
            display_offset: Cell::new(0),
            cursor_bounds: Cell::new(None),
            cursor_shape: Cell::new(None),
            paint_state: RefCell::default(),
            metrics: Cell::new(Some(metrics)),
            shell_name,
            cwd: cwd.or_else(|| std::env::current_dir().ok()),
            exited: false,
            got_output: false,
            spawn_error: None,
            scroll_accum: 0.0,
            marked: None,
            pending: None,
            queued: Vec::new(),
            starting: false,
            closed: false,
            _start: None,
            _pump: None,
            job,
            tab_id,
            block_ui: BlockUi::default(),
            profile_env,
            fallback,
            hint: hint.map(Into::into),
            selecting: false,
            has_selection: false,
        };

        if defer {
            view.pending = Some(cfg);
        } else {
            view.start_session(cfg, cx);
        }
        view
    }

    /// Spawns the shell of a deferred view (no-op when it already runs).
    fn spawn_pending(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(mut cfg) = self.pending.take() else {
            return;
        };
        // The real size is known now: the pane's card is laid out like the window, so estimate
        // from the window again (the element resizes the PTY on its first paint anyway).
        let metrics = self.metrics(window);
        cfg.cell_width = f32::from(metrics.width) as u16;
        cfg.cell_height = f32::from(metrics.height) as u16;
        log::info!(
            "[lazy] spawning deferred shell at {}",
            crate::mem::uptime_label()
        );
        self.start_session(cfg, cx);
    }

    /// Creates the shell **off the UI thread**. `CreateProcessW` of an app-execution alias (the
    /// Microsoft Store `pwsh`, `wt`...) waits inside COM with a nested message loop, which
    /// dispatches `WM_PAINT` to our window while gpui still holds the `App` borrow of the entity
    /// being built: gpui then logged "RefCell already borrowed" and skipped that frame (once at
    /// startup, once per new tab). On a worker thread nothing of ours is re-entered.
    fn start_session(&mut self, cfg: SessionConfig, cx: &mut Context<Self>) {
        let program = cfg.program.clone();
        let (cols, rows) = (cfg.cols, cfg.rows);
        let mut env = cx
            .try_global::<TermEnv>()
            .map(|e| e.0.clone())
            .unwrap_or_default();
        // The profile's variables win over the app's.
        for (k, v) in &self.profile_env {
            env.retain(|(name, _)| name != k);
            env.push((k.clone(), v.clone()));
        }
        log::debug!(
            "spawning with job {:?}",
            self.job.as_ref().map(|j| (j.owner, j.handle))
        );
        let options = spawn_options(env, self.job.as_ref().map(|j| j.handle), self.tab_id);
        self.starting = true;
        let spawn = cx
            .background_executor()
            .spawn(async move { Session::spawn_with_options(cfg, options) });
        self._start = Some(cx.spawn(async move |this, cx| {
            let result = spawn.await;
            this.update(cx, |view, cx| {
                view.finish_start(result, &program, (cols, rows), cx)
            })
            .ok();
        }));
    }

    /// The shell exists (or failed): wire it up, flush what was typed meanwhile, repaint.
    fn finish_start(
        &mut self,
        result: Result<
            (
                Session,
                futures::channel::mpsc::UnboundedReceiver<SessionEvent>,
            ),
            term_core::TermError,
        >,
        program: &str,
        (cols, rows): (u16, u16),
        cx: &mut Context<Self>,
    ) {
        let view = self;
        view.starting = false;
        view._start = None;
        if view.closed {
            return; // the tab was closed while the shell started: dropping the session ends it
        }
        match result {
            Ok((session, mut rx)) => {
                // The shell is in the job already: record it as the owner's root process. When
                // Windows refused the job (the Microsoft Store `pwsh`) the owner is tracked by
                // process tree instead (`proc_svc::TrackingMode::TreeOnly`).
                let attachment = session.job_attachment();
                if let (Some(job), Some(pid)) = (&view.job, session.pid()) {
                    let mut tracker = job.tracker.lock().unwrap_or_else(|p| p.into_inner());
                    let result = if matches!(
                        attachment,
                        JobAttachment::AssignFailed | JobAttachment::None
                    ) {
                        log::info!(
                            "shell pid {pid} has no job ({attachment:?}): tracking its process tree"
                        );
                        tracker.register_root_untracked_job(job.owner, pid)
                    } else {
                        tracker.register_root(job.owner, pid)
                    };
                    if let Err(e) = result {
                        log::warn!("could not register the shell (pid {pid}) as a root: {e}");
                    }
                }
                log::info!("shell job attachment: {attachment:?}");
                log::info!(
                    "[startup] spawned {program} (pid {:?}) at {cols}x{rows}, {} since process start",
                    session.pid(),
                    crate::mem::uptime_label()
                );
                view.session = Some(session);
                // One repaint per batch: Wakeup is coalesced by term_core until the next snapshot.
                view._pump = Some(cx.spawn(async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        if this
                            .update(cx, |view, cx| view.on_session_event(event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
                let queued = std::mem::take(&mut view.queued);
                view.send(queued, cx);
                cx.notify();
            }
            Err(e) => {
                log::error!("failed to spawn shell: {e}");
                // A profile that exists but does not start (a broken alias, no rights): the
                // automatic shell opens instead, so the tab is never dead.
                if let Some((fallback, args)) = view.fallback.take() {
                    view.hint = Some(
                        format!("No se pudo iniciar «{program}»: se abrió {fallback} en su lugar.")
                            .into(),
                    );
                    view.shell_name = fallback
                        .strip_suffix(".exe")
                        .unwrap_or(&fallback)
                        .to_string()
                        .into();
                    view.profile_env.clear();
                    let cfg = SessionConfig {
                        program: fallback,
                        args,
                        cwd: view.cwd.clone(),
                        cols,
                        rows,
                        cell_width: view.metrics.get().map_or(8, |m| f32::from(m.width) as u16),
                        cell_height: view
                            .metrics
                            .get()
                            .map_or(16, |m| f32::from(m.height) as u16),
                        palette: palette_from(&view.theme.terminal),
                        scrollback: TermSettings::current(cx).scrollback_lines(),
                    };
                    view.start_session(cfg, cx);
                    cx.notify();
                    return;
                }
                view.spawn_error = Some(format!("No se pudo iniciar {program}: {e}").into());
                cx.notify();
            }
        }
    }

    /// Shell display name (`pwsh`).
    pub fn shell_name(&self) -> &SharedString {
        &self.shell_name
    }

    /// Working directory of the shell, if known.
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Drops the session (kills the shell, stops its threads). The view stays as an inert surface.
    pub fn shutdown(&mut self) {
        self.closed = true;
        self.session = None;
        self._pump = None;
        self._start = None;
    }

    /// Applies a new theme: the terminal palette is swapped in the session (re-resolved on the
    /// next snapshot) and the view repaints; the row cache is invalidated by its epoch.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        // A new font, size or line height changes the cell: it is measured again on the next paint
        // and the element resizes the PTY grid to the new cell count.
        if self.theme.typography != theme.typography {
            self.metrics.set(None);
        }
        self.theme = theme;
        if let Some(session) = &self.session {
            session.set_palette(palette_from(&theme.terminal));
        }
        cx.notify();
    }

    /// Cell metrics, re-measured if the display scale changed.
    pub(crate) fn metrics(&self, window: &Window) -> CellMetrics {
        if let Some(m) = self.metrics.get()
            && (m.scale - window.scale_factor()).abs() < f32::EPSILON
        {
            return m;
        }
        let m = measure_cell(window, &self.theme.typography);
        self.metrics.set(Some(m));
        m
    }

    fn on_session_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Wakeup => {
                if !self.got_output {
                    self.got_output = true;
                    log::info!(
                        "[startup] first PTY output at {} since process start",
                        crate::mem::uptime_label()
                    );
                }
                cx.notify();
            }
            // The breadcrumb shows the cwd (OSC 7), not the window title.
            SessionEvent::Title(_) | SessionEvent::Bell => {}
            SessionEvent::Cwd(path) => {
                if self.cwd.as_deref() != Some(path.as_path()) {
                    self.cwd = Some(path.clone());
                    cx.emit(TerminalEvent::CwdChanged(path));
                    cx.notify();
                }
            }
            // The block events below carry the same information (and also cover commands that
            // never reported `D`), so the tab's status is driven by them.
            SessionEvent::CommandFinished { exit_code } => {
                log::debug!("command finished (exit code {exit_code:?})");
            }
            SessionEvent::BlockStarted(_) => {
                self.refresh_blocks();
                cx.emit(TerminalEvent::CommandStarted);
                cx.notify();
            }
            SessionEvent::BlockFinished { id, exit_code, .. } => {
                self.refresh_blocks();
                cx.notify();
                cx.emit(TerminalEvent::CommandFinished(exit_code));
                let block = self
                    .session
                    .as_ref()
                    .and_then(|s| s.blocks().into_iter().find(|b| b.id == id));
                if let Some(block) = block {
                    cx.emit(TerminalEvent::BlockFinished(block));
                }
            }
            SessionEvent::Exit => {
                log::info!("shell exited");
                self.exited = true;
                cx.notify();
            }
        }
    }

    /// Types `text` at the prompt (newlines become Enter); `enter` presses Enter at the end. Used
    /// by the command palette: run a command, insert a snippet, `cd` into a project.
    pub fn send_text(&mut self, text: &str, enter: bool, cx: &mut Context<Self>) {
        let mut bytes = text.replace("\r\n", "\r").replace('\n', "\r").into_bytes();
        if enter {
            bytes.push(b'\r');
        }
        if self.session.is_none() && (self.pending.is_some() || self.starting) {
            self.queued.extend(bytes);
            return;
        }
        self.send(bytes, cx);
    }

    /// Files dropped from Explorer: their paths, quoted for this shell and space-separated, typed
    /// at the prompt **without Enter**.
    pub fn insert_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let text = crate::drop::quote_paths(crate::drop::shell_kind(&self.shell_name), paths);
        if !text.is_empty() {
            self.send_text(&text, false, cx);
        }
    }

    /// Writes bytes to the shell and snaps the viewport back to the live bottom.
    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(session) = &self.session {
            session.scroll_to_bottom();
            session.write(bytes);
        }
        self.clear_selection(cx);
        if self.display_offset.get() > 0 {
            self.display_offset.set(0);
            cx.notify();
        }
    }

    /// Ctrl+V / Ctrl+Shift+V. Newlines become `\r`, as typed Enter would send.
    /// No bracketed paste yet (term_core does not expose the mode).
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        self.send(text.into_bytes(), cx);
    }

    /// Ctrl+Shift+C: copies the selection (nothing happens without one).
    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// The selected text, if any.
    pub(crate) fn selection_text(&self) -> Option<String> {
        self.session.as_ref().and_then(Session::selection_text)
    }

    /// Drops the selection (typing, a click elsewhere).
    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.has_selection {
            self.has_selection = false;
            if let Some(session) = &self.session {
                session.selection_clear();
            }
            cx.notify();
        }
    }

    /// The cell under `position` as `(terminal row, column, right half of the cell)`; `None` over
    /// a fold summary or outside the grid.
    fn selection_cell(&self, position: gpui::Point<Pixels>) -> Option<(usize, usize, bool)> {
        let ui = &self.block_ui;
        let (cw, ch) = ui.cell.get();
        let origin = ui.origin.get();
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let x = f32::from(position.x) - f32::from(origin.x);
        let y = f32::from(position.y) - f32::from(origin.y);
        if y < 0.0 {
            return None;
        }
        let vis = (y / ch) as usize;
        let row = ui.rows.borrow().get(vis)?.term_row?;
        let col = if x < 0.0 { 0 } else { (x / cw) as usize };
        let right = x >= 0.0 && (x / cw).fract() >= 0.5;
        Some((row, col, right))
    }

    /// Left button down on the grid: a new selection starts here.
    pub(crate) fn begin_selection(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.selecting = false;
        let Some((row, col, right)) = self.selection_cell(position) else {
            self.clear_selection(cx);
            return;
        };
        if let Some(session) = &self.session {
            session.selection_start(row, col, right);
            self.selecting = true;
            self.has_selection = true;
            cx.notify();
        }
    }

    /// The pointer moved with the button down: the selection grows to the cell under it.
    pub(crate) fn extend_selection(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting {
            return;
        }
        if let Some((row, col, right)) = self.selection_cell(position)
            && let Some(session) = &self.session
        {
            session.selection_update(row, col, right);
            cx.notify();
        }
    }

    /// Left button up: the selection is final; with `terminal.copy_on_select` it is copied now.
    pub(crate) fn end_selection(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.selecting) {
            return;
        }
        let text = self.selection_text();
        self.has_selection = text.is_some();
        if let Some(text) = text
            && TermSettings::current(cx).copy_on_select
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;
        if self.hint.take().is_some() {
            cx.notify();
        }

        // Previous / next command block (Ctrl+Up / Ctrl+Down by default; the shell gets the keys
        // when there are no blocks).
        // The chords are the ones of Teclado (`block.prev` / `block.next`).
        if let Some(dir) = crate::keybindings::block_direction(ks, cx)
            && self.jump_block(dir, cx)
        {
            cx.stop_propagation();
            return;
        }

        // Clipboard shortcuts. Ctrl+V pastes too, like Windows Terminal (`terminal.paste_ctrl_v`;
        // off, the program gets ^V); Ctrl+Shift+V always pastes; Ctrl+Shift+C copies.
        if m.control && !m.alt && !m.platform {
            match (ks.key.as_str(), m.shift) {
                ("v", shift) if shift || TermSettings::current(cx).paste_ctrl_v => {
                    self.paste(cx);
                    cx.stop_propagation();
                    return;
                }
                ("c", true) => {
                    self.copy(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        // Printable text (also AltGr = Ctrl+Alt) is left to the OS text pipeline: it arrives via
        // `replace_text_in_range` and handles dead keys and IMEs correctly.
        let printable = ks.key.chars().count() == 1 && ks.key_char.is_some();
        if printable && !m.platform && ((!m.control && !m.alt) || (m.control && m.alt)) {
            return;
        }

        let input = KeyInput {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
        };
        if let Some(bytes) = encode_key(&input, self.app_cursor.get()) {
            self.send(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let line_h = self.metrics(window).height;
        let dy = event.delta.pixel_delta(line_h).y;
        // Positive wheel delta = scroll up into history, which is also session.scroll's sign.
        self.scroll_accum += f32::from(dy) / f32::from(line_h);
        let whole = self.scroll_accum.trunc();
        self.scroll_accum -= whole;
        if whole != 0.0
            && let Some(session) = &self.session
        {
            session.scroll(whole as i32);
            cx.notify();
        }
    }
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        // Control characters never come through here (Enter, Tab, ... are encoded on key down).
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.send(clean.into_bytes(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_string());
    }

    fn bounds_for_range(
        &mut self,
        _range: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.cursor_bounds.get().unwrap_or(element_bounds))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.spawn_pending(window, cx);
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let notice: Option<SharedString> = if let Some(e) = &self.spawn_error {
            Some(e.clone())
        } else if self.exited {
            Some("[proceso terminado]".into())
        } else {
            self.hint.clone()
        };

        div()
            .id("terminal")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(hsla(rgb8(self.theme.terminal.background)))
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .when(self.block_ui.link.is_some(), |d| d.cursor_pointer())
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                this.on_pointer_move(e, cx);
                if e.pressed_button == Some(MouseButton::Left) {
                    this.extend_selection(e.position, cx);
                } else {
                    // The button was released outside the view: the drag is over.
                    this.end_selection(cx);
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.on_pointer_leave(cx);
                }
            }))
            .on_modifiers_changed(
                cx.listener(|this, e: &ModifiersChangedEvent, _, cx| this.on_modifiers(e, cx)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle);
                    if !this.click_special(e, cx) {
                        this.begin_selection(e.position, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_selection(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_selection(cx)),
            )
            .child(
                div()
                    .size_full()
                    .p(px(TERM_PAD))
                    .child(TerminalElement::new(cx.entity())),
            )
            .children(self.block_bar(cx))
            .child({
                let this = cx.entity();
                ui_kit::DropZone::new("terminal-drop", &self.theme, "Soltar para pegar las rutas")
                    .radius(Radius::MD)
                    .on_drop(move |paths, _, cx| this.update(cx, |t, cx| t.insert_paths(paths, cx)))
            })
            .children(notice.map(|text| {
                div()
                    .absolute()
                    .left(px(TERM_PAD * 2.0))
                    .bottom(px(TERM_PAD * 2.0))
                    .px(px(12.0))
                    .py(px(6.0))
                    .rounded(px(Radius::XS))
                    .bg(c.surface_3)
                    .text_color(c.muted)
                    .font_family(ty.ui_family)
                    .text_size(px(ty.ui_size_md))
                    .child(text)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(o: &SpawnOptions, key: &str) -> Option<String> {
        o.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// 8B: the shell of a tab learns which tab it is, so a `selachi-agent run` started in it hangs
    /// from that tab in the map (the harness reads `SELACHI_PARENT_TAB`).
    #[test]
    fn a_tab_shell_gets_its_tab_id_and_the_ask_permission() {
        let o = spawn_options(Vec::new(), None, Some(4_294_967_297));
        assert_eq!(
            env_of(&o, "SELACHI_PARENT_TAB").as_deref(),
            Some("4294967297")
        );
        assert_eq!(
            env_of(&o, "SELACHI_PARENT_PERMISSION").as_deref(),
            Some("ask")
        );
        // the app's own variables stay, and a stale parent from the environment is replaced
        let o = spawn_options(
            vec![
                ("FOO".into(), "1".into()),
                ("SELACHI_PARENT_TAB".into(), "stale".into()),
            ],
            Some(7),
            Some(9),
        );
        assert_eq!(env_of(&o, "FOO").as_deref(), Some("1"));
        assert_eq!(env_of(&o, "SELACHI_PARENT_TAB").as_deref(), Some("9"));
        assert_eq!(o.job, Some(7));
        // a view without a tab (examples) exports nothing
        let o = spawn_options(Vec::new(), None, None);
        assert!(env_of(&o, "SELACHI_PARENT_TAB").is_none());
    }
}

#[cfg(test)]
#[path = "terminal_view/settings_tests.rs"]
mod settings_tests;
