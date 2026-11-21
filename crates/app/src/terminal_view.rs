//! The terminal view entity: owns the PTY session, pumps its events, handles input.
//!
//! Painting lives in [`crate::terminal_element`].

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use gpui::{
    App, Bounds, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Render, ScrollWheelEvent, SharedString, Task,
    UTF16Selection, Window, div, font, prelude::*, px,
};
use term_core::{KeyInput, Palette, Rgb8, Session, SessionConfig, SessionEvent, encode_key};
use theme::{Radius, TerminalColors, Theme, Typography};

use crate::app_shell::{RAIL_W, TAB_H, TERM_PAD};
use crate::status_bar::STATUS_H;
use crate::terminal_element::{PaintState, TerminalElement};

/// What a terminal tells the rest of the window (breadcrumb, tab label, git pill).
#[derive(Clone, Debug)]
pub enum TerminalEvent {
    /// The shell's working directory changed (OSC 7).
    CwdChanged(PathBuf),
    /// A command was submitted: Enter pressed on a non-empty command line (see
    /// [`TerminalView::note_input`]; term_core does not surface OSC 133 `C` yet).
    CommandStarted,
    /// A command finished (OSC 133 `D`), with its exit code when the shell reported one.
    CommandFinished(Option<i32>),
}

/// Scrollback lines kept per session.
const SCROLLBACK: usize = 10_000;

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
    /// The command line holds something that Enter would run (typed, pasted or recalled text).
    line_has_text: bool,
    /// In-progress IME / dead-key composition (not rendered yet).
    marked: Option<String>,
    /// A deferred session (restored tabs): spawned the first time the view is rendered, so tabs
    /// that are never shown cost no shell process.
    pending: Option<SessionConfig>,
    _pump: Option<Task<()>>,
}

impl TerminalView {
    /// `cwd` is the directory the shell starts in (`None` = the process directory).
    pub fn new(theme: Theme, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(theme, cwd, false, window, cx)
    }

    /// Like [`TerminalView::new`] but the shell starts the first time the view is painted.
    pub fn new_deferred(theme: Theme, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(theme, cwd, true, window, cx)
    }

    fn build(theme: Theme, cwd: Option<PathBuf>, defer: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        log::info!("[startup] TerminalView::new at {} since process start", crate::mem::uptime_label());
        let focus_handle = cx.focus_handle();
        if !defer {
            focus_handle.focus(window);
        }

        let metrics = measure_cell(window, &theme.typography);
        // Estimate the terminal grid from the window so the shell starts at the right size.
        let vp = window.viewport_size();
        let avail_w = f32::from(vp.width) - RAIL_W - 2.0 * TERM_PAD;
        let avail_h = f32::from(vp.height) - TAB_H - STATUS_H - 2.0 * TERM_PAD;
        let cols = (avail_w / f32::from(metrics.width)).floor().clamp(10.0, 500.0) as u16;
        let rows = (avail_h / f32::from(metrics.height)).floor().clamp(3.0, 300.0) as u16;

        let (program, args) = term_core::default_shell();
        let shell_name: SharedString =
            program.strip_suffix(".exe").unwrap_or(&program).to_string().into();
        let cfg = SessionConfig {
            program: program.clone(),
            args,
            cwd: cwd.clone(),
            cols,
            rows,
            cell_width: f32::from(metrics.width) as u16,
            cell_height: f32::from(metrics.height) as u16,
            palette: palette_from(&theme.terminal),
            scrollback: SCROLLBACK,
        };

        let mut view = Self {
            session: None,
            theme,
            focus_handle,
            app_cursor: Cell::new(false),
            display_offset: Cell::new(0),
            cursor_bounds: Cell::new(None),
            paint_state: RefCell::default(),
            metrics: Cell::new(Some(metrics)),
            shell_name,
            cwd: cwd.or_else(|| std::env::current_dir().ok()),
            exited: false,
            got_output: false,
            spawn_error: None,
            scroll_accum: 0.0,
            line_has_text: false,
            marked: None,
            pending: None,
            _pump: None,
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
        let Some(mut cfg) = self.pending.take() else { return };
        // The real size is known now: the pane's card is laid out like the window, so estimate
        // from the window again (the element resizes the PTY on its first paint anyway).
        let metrics = self.metrics(window);
        cfg.cell_width = f32::from(metrics.width) as u16;
        cfg.cell_height = f32::from(metrics.height) as u16;
        log::info!("[lazy] spawning deferred shell at {}", crate::mem::uptime_label());
        self.start_session(cfg, cx);
    }

    fn start_session(&mut self, cfg: SessionConfig, cx: &mut Context<Self>) {
        let view = self;
        let program = cfg.program.clone();
        let (cols, rows) = (cfg.cols, cfg.rows);
        match Session::spawn(cfg) {
            Ok((session, mut rx)) => {
                log::info!(
                    "[startup] spawned {program} (pid {:?}) at {cols}x{rows}, {} since process start",
                    session.pid(),
                    crate::mem::uptime_label()
                );
                view.session = Some(session);
                // One repaint per batch: Wakeup is coalesced by term_core until the next snapshot.
                view._pump = Some(cx.spawn(async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        if this.update(cx, |view, cx| view.on_session_event(event, cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => {
                log::error!("failed to spawn shell: {e}");
                view.spawn_error = Some(format!("No se pudo iniciar {program}: {e}").into());
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
        self.session = None;
        self._pump = None;
    }

    /// Applies a new theme: the terminal palette is swapped in the session (re-resolved on the
    /// next snapshot) and the view repaints; the row cache is invalidated by its epoch.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
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
            SessionEvent::CommandFinished { exit_code } => {
                log::debug!("command finished (exit code {exit_code:?})");
                self.line_has_text = false;
                cx.emit(TerminalEvent::CommandFinished(exit_code));
            }
            SessionEvent::Exit => {
                log::info!("shell exited");
                self.exited = true;
                cx.notify();
            }
        }
    }

    /// Writes bytes to the shell and snaps the viewport back to the live bottom.
    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        self.note_input(&bytes, cx);
        if let Some(session) = &self.session {
            session.scroll_to_bottom();
            session.write(bytes);
        }
        if self.display_offset.get() > 0 {
            self.display_offset.set(0);
            cx.notify();
        }
    }

    /// Tracks whether the command line has text so that Enter can be told apart from an empty
    /// Enter: only a submitted, non-empty line starts a command (the status "Working" of the tab).
    /// History recall (Up/Down) counts as text; Ctrl+C clears the line.
    fn note_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        match bytes {
            b"\r" => {
                if std::mem::take(&mut self.line_has_text) {
                    cx.emit(TerminalEvent::CommandStarted);
                }
            },
            [0x03] => self.line_has_text = false,
            [0x1b, b'[' | b'O', b'A' | b'B'] => self.line_has_text = true,
            _ => {
                if bytes.iter().any(|b| !b.is_ascii_whitespace() && *b >= 0x20 && *b != 0x1b) {
                    self.line_has_text = true;
                }
            },
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

    /// Ctrl+Shift+C. TODO: no selection model yet, so there is nothing to copy.
    fn copy(&mut self, _cx: &mut Context<Self>) {
        log::debug!("copy requested: selection is not implemented yet");
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;

        // Clipboard shortcuts. Ctrl+V pastes too, like Windows Terminal; Ctrl+Shift+C copies.
        if m.control && !m.alt && !m.platform {
            match (ks.key.as_str(), m.shift) {
                ("v", _) => {
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
        Some(UTF16Selection { range: 0..0, reversed: false })
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
            None
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, _cx| {
                    window.focus(&this.focus_handle);
                }),
            )
            .child(div().size_full().p(px(TERM_PAD)).child(TerminalElement::new(cx.entity())))
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
