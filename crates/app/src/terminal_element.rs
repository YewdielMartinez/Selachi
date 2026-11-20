//! Custom gpui element that paints the terminal grid.
//!
//! Per frame (in `prepaint`) it takes one [`term_core::Snapshot`] (into a reused buffer) and, for
//! every row, looks up a [`CachedRow`] by the row's content hash. Only rows whose content changed
//! are rebuilt:
//! * background runs, merged horizontally and only where the cell background differs from the
//!   terminal background,
//! * one shaped line per text segment of a row (colors/weights as `TextRun`s), with glyphs
//!   pinned to the cell grid via `force_width`; wide characters get their own segment.
//!
//! The cache is keyed by content, not by row index, so scrolling reuses the shaped lines of the
//! rows that merely moved. Rows not used in the last frame are evicted, so memory is bounded by
//! the viewport. `paint` replays cached rows plus the cursor quad (block, or hollow when the
//! window is unfocused), so a frame in which nothing changed shapes no text at all.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    App, BorderStyle, Bounds, ElementInputHandler, Entity, Font, FontStyle, FontWeight,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, SharedString,
    ShapedLine, StrikethroughStyle, Style, TextRun, UnderlineStyle, Window, fill, font, outline,
    point, px, relative, size,
};
use term_core::{CellView, Rgb8, Snapshot};

use crate::terminal_view::{TerminalView, hsla};

pub struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

/// Number of prepaints since startup (debug counter: must not grow while the terminal is idle).
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Everything computed in `prepaint` and replayed in `paint`.
#[derive(Default)]
pub struct Frame {
    /// `(y, row)` for every viewport row.
    rows: Vec<(Pixels, Rc<CachedRow>)>,
    origin_x: Pixels,
    cell_width: Pixels,
    cell_height: Pixels,
    cursor: Option<PaintQuad>,
}

/// The paintable form of one row: independent of its y position.
#[derive(Default)]
struct CachedRow {
    /// Background runs `(start col, end col exclusive, color)`.
    backgrounds: Vec<(usize, usize, Rgb8)>,
    /// Shaped text segments with their start column.
    lines: Vec<(usize, ShapedLine)>,
}

/// Everything that invalidates every cached row at once.
#[derive(Clone, Copy, PartialEq)]
struct Epoch {
    cell_width: f32,
    cell_height: f32,
    scale: f32,
    background: u32,
    family: &'static str,
    font_size: f32,
}

struct CacheEntry {
    last_used: u64,
    row: Rc<CachedRow>,
}

/// Per-view paint state that survives between frames: the snapshot buffer and the row cache.
#[derive(Default)]
pub struct PaintState {
    snap: Snapshot,
    rows: HashMap<u64, CacheEntry>,
    epoch: Option<Epoch>,
    fonts: Option<Fonts>,
    generation: u64,
}

/// Style attributes that force a new `TextRun` when they change.
#[derive(Clone, Copy, PartialEq)]
struct RunStyle {
    fg: Rgb8,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// A contiguous piece of a row that is shaped as one line.
struct Segment {
    col: usize,
    text: String,
    runs: Vec<TextRun>,
    last_style: Option<RunStyle>,
}

impl Segment {
    fn new(col: usize) -> Self {
        Self { col, text: String::new(), runs: Vec::new(), last_style: None }
    }

    fn push(&mut self, ch: char, style: RunStyle, fonts: &Fonts) {
        self.text.push(ch);
        let len = ch.len_utf8();
        if self.last_style == Some(style)
            && let Some(run) = self.runs.last_mut()
        {
            run.len += len;
            return;
        }
        self.last_style = Some(style);
        self.runs.push(TextRun {
            len,
            font: fonts.pick(style.bold, style.italic),
            color: hsla(style.fg),
            background_color: None,
            underline: style.underline.then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(hsla(style.fg)),
                wavy: false,
            }),
            strikethrough: style.strike.then(|| StrikethroughStyle {
                thickness: px(1.0),
                color: Some(hsla(style.fg)),
            }),
        });
    }

    /// Drops trailing spaces (they draw nothing), keeping `runs` consistent with `text`.
    fn trim_end(&mut self) {
        while self.text.ends_with(' ') {
            self.text.pop();
            if let Some(run) = self.runs.last_mut() {
                run.len -= 1;
                if run.len == 0 {
                    self.runs.pop();
                }
            }
        }
    }
}

struct Fonts {
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
}

impl Fonts {
    fn new(family: &'static str) -> Self {
        let base = font(family);
        let with = |weight, style| Font { weight, style, ..base.clone() };
        Self {
            regular: base.clone(),
            bold: with(FontWeight::BOLD, FontStyle::Normal),
            italic: with(FontWeight::NORMAL, FontStyle::Italic),
            bold_italic: with(FontWeight::BOLD, FontStyle::Italic),
        }
    }

    fn pick(&self, bold: bool, italic: bool) -> Font {
        match (bold, italic) {
            (false, false) => self.regular.clone(),
            (true, false) => self.bold.clone(),
            (false, true) => self.italic.clone(),
            (true, true) => self.bold_italic.clone(),
        }
    }
}

impl gpui::Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let view = self.view.read(cx);
        let metrics = view.metrics(window);
        let (cw, ch) = (metrics.width, metrics.height);
        let mut frame = Frame {
            origin_x: bounds.origin.x,
            cell_width: cw,
            cell_height: ch,
            ..Frame::default()
        };
        let Some(session) = &view.session else {
            return frame;
        };

        // Resize only when the grid size changed (Session::resize is a no-op otherwise).
        let cols = ((bounds.size.width / cw).floor() as usize).clamp(2, 1000);
        let rows = ((bounds.size.height / ch).floor() as usize).clamp(1, 1000);
        session.resize(cols as u16, rows as u16, f32::from(cw) as u16, f32::from(ch) as u16);

        let theme = view.theme;
        let mut state = view.paint_state.borrow_mut();
        let state = &mut *state;
        session.snapshot_into(&mut state.snap);
        let snap = &state.snap;
        view.app_cursor.set(snap.app_cursor);
        view.display_offset.set(snap.display_offset);

        let default_bg = rgb8_of(theme.terminal.background);
        let cursor_color = hsla(rgb8_of(theme.terminal.cursor));
        let focused = view.focus_handle.is_focused(window) && window.is_window_active();
        let block_cursor = focused;
        let font_size = px(theme.typography.mono_size);
        let origin = bounds.origin;

        // Anything that changes how a row looks for the same cell content drops the whole cache.
        let epoch = Epoch {
            cell_width: f32::from(cw),
            cell_height: f32::from(ch),
            scale: window.scale_factor(),
            background: theme.terminal.background,
            family: theme.typography.mono_family,
            font_size: theme.typography.mono_size,
        };
        if state.epoch != Some(epoch) {
            state.rows.clear();
            state.fonts = Some(Fonts::new(epoch.family));
            state.epoch = Some(epoch);
        }
        let fonts = state.fonts.get_or_insert_with(|| Fonts::new(epoch.family));
        state.generation += 1;
        let generation = state.generation;

        let mut shaped = 0usize;
        frame.rows.reserve(snap.rows);
        for (r, row) in snap.lines.iter().enumerate() {
            let y = origin.y + ch * r as f32;
            // Only the cursor row depends on something besides its cells (the glyph under a block
            // cursor is drawn in the background color).
            let block_col = snap.cursor.filter(|&(cr, _)| block_cursor && cr == r).map(|(_, c)| c);
            let key = match block_col {
                Some(c) => snap.row_hash[r] ^ (c as u64 + 1).wrapping_mul(0xA24B_AED4_963E_E407),
                None => snap.row_hash[r],
            };
            let entry = state.rows.entry(key).or_insert_with(|| {
                shaped += 1;
                CacheEntry {
                    last_used: 0,
                    row: Rc::new(build_row(row, block_col, default_bg, fonts, font_size, cw, window)),
                }
            });
            entry.last_used = generation;
            frame.rows.push((y, entry.row.clone()));
        }
        state.rows.retain(|_, e| e.last_used == generation);

        let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::info!(
                "[startup] first terminal frame prepared at {} since process start",
                crate::mem::uptime_label()
            );
        }
        log::debug!(
            "terminal frame #{n}: shaped {shaped}/{} rows, cache {} rows",
            snap.rows,
            state.rows.len()
        );

        // Cursor.
        view.cursor_bounds.set(None);
        if let Some((cr, cc)) = snap.cursor {
            let wide = snap.lines.get(cr).and_then(|l| l.get(cc)).is_some_and(|c| c.flags.wide);
            let width = if wide { cw * 2.0 } else { cw };
            let cb = Bounds::new(
                point(origin.x + cw * cc as f32, origin.y + ch * cr as f32),
                size(width, ch),
            );
            view.cursor_bounds.set(Some(cb));
            frame.cursor = Some(if block_cursor {
                fill(cb, cursor_color)
            } else {
                outline(cb, cursor_color, BorderStyle::Solid)
            });
        }
        frame
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        frame: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.view.clone()), cx);

        let (x0, cw, ch) = (frame.origin_x, frame.cell_width, frame.cell_height);
        for (y, row) in &frame.rows {
            for &(start, end, color) in &row.backgrounds {
                window.paint_quad(bg_quad(x0, *y, cw, ch, start, end, color));
            }
        }
        if let Some(cursor) = frame.cursor.take() {
            window.paint_quad(cursor);
        }
        for (y, row) in &frame.rows {
            for (col, line) in &row.lines {
                let origin = point(x0 + cw * *col as f32, *y);
                if let Err(e) = line.paint(origin, ch, window, cx) {
                    log::warn!("failed to paint terminal line: {e}");
                }
            }
        }
    }
}

/// Builds the paintable form of one row (backgrounds + shaped text segments).
fn build_row(
    row: &[CellView],
    block_col: Option<usize>,
    default_bg: Rgb8,
    fonts: &Fonts,
    font_size: Pixels,
    cw: Pixels,
    window: &mut Window,
) -> CachedRow {
    let mut out = CachedRow::default();

    // Backgrounds: merge adjacent cells with the same non-default bg into one run.
    let mut run: Option<(usize, usize, Rgb8)> = None; // (start col, end col exclusive, bg)
    for (c, cell) in row.iter().enumerate() {
        let bg = (cell.bg != default_bg).then_some(cell.bg);
        match (&mut run, bg) {
            (Some((_, end, rbg)), Some(b)) if *rbg == b && *end == c => *end = c + 1,
            (slot, bg) => {
                if let Some(done) = slot.take() {
                    out.backgrounds.push(done);
                }
                *slot = bg.map(|b| (c, c + 1, b));
            }
        }
    }
    out.backgrounds.extend(run);

    // Text segments.
    let mut seg: Option<Segment> = None;
    let flush = |seg: &mut Option<Segment>, out: &mut CachedRow, window: &mut Window| {
        if let Some(mut s) = seg.take() {
            s.trim_end();
            if s.text.is_empty() {
                return;
            }
            let line = window.text_system().shape_line(
                SharedString::from(s.text),
                font_size,
                &s.runs,
                Some(cw),
            );
            out.lines.push((s.col, line));
        }
    };
    for (c, cell) in row.iter().enumerate() {
        if cell.flags.wide_spacer {
            continue;
        }
        let under_block_cursor = block_col == Some(c);
        let style = run_style(cell, under_block_cursor.then_some(default_bg));
        let ch_ = if cell.ch.is_control() { ' ' } else { cell.ch };
        if cell.flags.wide {
            flush(&mut seg, &mut out, window);
            let mut s = Segment::new(c);
            s.push(ch_, style, fonts);
            seg = Some(s);
            flush(&mut seg, &mut out, window);
            continue;
        }
        if seg.is_none() {
            if ch_ == ' ' && !style.underline && !style.strike {
                continue; // never start a segment on blank space
            }
            seg = Some(Segment::new(c));
        }
        if let Some(s) = &mut seg {
            s.push(ch_, style, fonts);
        }
    }
    flush(&mut seg, &mut out, window);
    out
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn rgb8_of(hex: u32) -> Rgb8 {
    Rgb8::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn bg_quad(
    x0: Pixels,
    y: Pixels,
    cw: Pixels,
    ch: Pixels,
    start: usize,
    end: usize,
    bg: Rgb8,
) -> PaintQuad {
    let bounds = Bounds::new(point(x0 + cw * start as f32, y), size(cw * (end - start) as f32, ch));
    fill(bounds, hsla(bg))
}

fn run_style(cell: &CellView, fg_override: Option<Rgb8>) -> RunStyle {
    RunStyle {
        fg: fg_override.unwrap_or(cell.fg),
        bold: cell.flags.bold,
        italic: cell.flags.italic,
        underline: cell.flags.underline,
        strike: cell.flags.strikethrough,
    }
}
