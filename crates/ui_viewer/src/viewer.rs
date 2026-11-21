//! `FileViewer`: the read-only file view (PLAN section 13, "ver, no editar").
//!
//! One entity per open file. It owns the `viewer_svc::Document` (dropped with the tab), opens it on
//! the background executor, polls the index / highlight progress with a short timer **only while
//! they run**, and routes the file to a body: code, diff, markdown, structured tree, image or hex.
//! Everything asynchronous ends in `cx.notify()`; when nothing is loading the view costs nothing.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Point, Render,
    ScrollHandle, ScrollStrategy, SharedString, Styled as _, Subscription, Task, Window, actions, canvas, div, fill, img,
    point, prelude::*, px, size,
};
use theme::{Radius, Theme};
use ui_kit::{
    GhostButton, Icon, Pill, Segment, SegmentedControl, Spinner, UniformListScrollHandle, VirtualList, icon,
};
use viewer_svc::{Document, FileKind, GitBase, Hunk, Limits, MdBlock};

use crate::data_tree::DataTree;
use crate::diff_rows::{self, SplitRow, UnifiedRow};
use crate::event::ViewerEvent;
use crate::field::{TextField, TextFieldEvent};
use crate::format::{breadcrumb, display_path, find_repo_root, format_bytes};
use crate::image_info::image_dimensions;
use crate::md_view::{self, MdRender};
use crate::minimap::row_at;
use crate::minimap_view::{MinimapShared, MinimapSource, STRIP_W, minimap as minimap_el};
use crate::rows::{self, DATA_ROW_H, ROW_H};
use crate::spans::{LineSpans, MAX_LINE_BYTES, prepare_line};
use crate::state::{
    Body, FindState, PluginHint, default_body, has_minimap, new_cancel, parse_goto, plugin_hint, reading_cap,
    rendered_label, search_document, source_body, supports_diff,
};

actions!(
    ui_viewer,
    [OpenFind, OpenGoto, CloseBars, FindNext, FindPrev, PageDown, PageUp, ScrollTop, ScrollBottom]
);

/// Key context of the viewer (its bindings live in [`bind_keys`]).
pub const CONTEXT: &str = "FileViewer";

/// Installs the viewer's key bindings (and the text field's). Call once at startup.
pub fn bind_keys(cx: &mut App) {
    crate::field::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("ctrl-f", OpenFind, Some(CONTEXT)),
        KeyBinding::new("ctrl-g", OpenGoto, Some(CONTEXT)),
        KeyBinding::new("escape", CloseBars, Some(CONTEXT)),
        KeyBinding::new("f3", FindNext, Some(CONTEXT)),
        KeyBinding::new("shift-f3", FindPrev, Some(CONTEXT)),
        KeyBinding::new("pagedown", PageDown, Some(CONTEXT)),
        KeyBinding::new("pageup", PageUp, Some(CONTEXT)),
        KeyBinding::new("ctrl-home", ScrollTop, Some(CONTEXT)),
        KeyBinding::new("ctrl-end", ScrollBottom, Some(CONTEXT)),
    ]);
}

/// Hex rows shown at most (keeps scroll offsets precise in `f32`).
pub const MAX_HEX_ROWS: usize = 800_000;
/// Markdown files above this size open as source (rendering 5 MB of prose is not a use case).
pub const MAX_MARKDOWN_BYTES: u64 = 2 * 1024 * 1024;

/// How to open a file.
#[derive(Clone, Debug)]
pub struct ViewerConfig {
    /// Project (git) root: breadcrumb is relative to it and diffs run in it. Found by walking up
    /// from the file when `None`.
    pub project_root: Option<PathBuf>,
    pub limits: Limits,
    /// Revision "Ver diff" compares against.
    pub diff_base: GitBase,
    /// Text for the base chip, e.g. `checkpoint a1b2c3d` (defaults to `HEAD`).
    pub base_label: Option<SharedString>,
    /// Open straight in diff mode.
    pub open_in_diff: bool,
    /// 1-based line to scroll to after opening.
    pub line: Option<usize>,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            project_root: None,
            limits: Limits::default(),
            diff_base: GitBase::Head,
            base_label: None,
            open_in_diff: false,
            line: None,
        }
    }
}

#[derive(Debug)]
enum Load {
    Loading,
    Ready,
    Failed(String),
}

struct DiffData {
    added: usize,
    removed: usize,
    digits: usize,
    hunks: usize,
    unified: Rc<Vec<UnifiedRow>>,
    split: Rc<Vec<SplitRow>>,
    unified_mini: MiniRows,
    split_mini: MiniRows,
}

struct MiniRows {
    lens: Rc<Vec<u16>>,
    changed: Rc<Vec<usize>>,
}

enum DiffState {
    Idle,
    Loading,
    Ready(Box<DiffData>),
    Failed(String),
}

enum DataState {
    Idle,
    Loading,
    Ready(Box<DataTree>),
    Failed(String),
}

#[derive(Default)]
struct MdState {
    blocks: Option<Rc<Vec<MdBlock>>>,
    highlights: HashMap<usize, Vec<LineSpans>>,
}

/// The viewer entity.
pub struct FileViewer {
    theme: Theme,
    cfg: ViewerConfig,
    path: PathBuf,
    root: Option<PathBuf>,
    load: Load,
    doc: Option<Document>,
    kind: Option<FileKind>,
    size: u64,
    body: Body,
    image_dims: Option<(u32, u32)>,

    scroll: UniformListScrollHandle,
    data_scroll: UniformListScrollHandle,
    md_scroll: ScrollHandle,
    focus: FocusHandle,

    // find / go to line
    find: FindState,
    find_open: bool,
    find_field: Entity<TextField>,
    find_cancel: Arc<AtomicBool>,
    goto_open: bool,
    goto_field: Entity<TextField>,

    // minimap
    mini: MinimapShared,
    mini_gen: u64,
    mini_drag: bool,

    // bodies
    diff: DiffState,
    diff_split: bool,
    data: DataState,
    md: MdState,
    image_fit: bool,

    // background work (dropping a task cancels it)
    load_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    diff_task: Option<Task<()>>,
    md_task: Option<Task<()>>,
    data_task: Option<Task<()>>,
    find_task: Option<Task<()>>,

    // perf probes
    opened_at: Instant,
    load_time: Option<Duration>,
    first_rows_at: Cell<Option<Instant>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ViewerEvent> for FileViewer {}

impl Focusable for FileViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileViewer {
    /// Starts opening `path`; the view shows a spinner until the document is ready.
    pub fn new(path: PathBuf, cfg: ViewerConfig, theme: Theme, cx: &mut Context<Self>) -> Self {
        let find_field = cx.new(|cx| TextField::new(theme, "Buscar en el archivo", cx));
        let goto_field = cx.new(|cx| TextField::new(theme, "Ir a la linea (42 o 42:7)", cx));
        let subs = vec![
            cx.subscribe(&find_field, |this, field, ev: &TextFieldEvent, cx| match ev {
                TextFieldEvent::Changed => {
                    let q = field.read(cx).text().to_owned();
                    this.run_find(&q, cx);
                }
                TextFieldEvent::Submit { reverse } => this.step_find(*reverse, cx),
                TextFieldEvent::Cancel => this.close_bars(cx),
            }),
            cx.subscribe(&goto_field, |this, field, ev: &TextFieldEvent, cx| match ev {
                TextFieldEvent::Submit { .. } => {
                    let text = field.read(cx).text().to_owned();
                    this.goto_text(&text, cx);
                }
                TextFieldEvent::Cancel => this.close_bars(cx),
                TextFieldEvent::Changed => {}
            }),
        ];
        let root = cfg.project_root.clone().or_else(|| find_repo_root(&path));
        let mut v = Self {
            theme,
            cfg,
            path,
            root,
            load: Load::Loading,
            doc: None,
            kind: None,
            size: 0,
            body: Body::Code,
            image_dims: None,
            scroll: UniformListScrollHandle::new(),
            data_scroll: UniformListScrollHandle::new(),
            md_scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            find: FindState::default(),
            find_open: false,
            find_field,
            find_cancel: new_cancel(),
            goto_open: false,
            goto_field,
            mini: MinimapShared::default(),
            mini_gen: 0,
            mini_drag: false,
            diff: DiffState::Idle,
            diff_split: false,
            data: DataState::Idle,
            md: MdState::default(),
            image_fit: true,
            load_task: None,
            poll_task: None,
            diff_task: None,
            md_task: None,
            data_task: None,
            find_task: None,
            opened_at: Instant::now(),
            load_time: None,
            first_rows_at: Cell::new(None),
            _subs: subs,
        };
        v.begin_load(cx);
        v
    }

    // ---- accessors ---------------------------------------------------------------------------

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn body(&self) -> Body {
        self.body
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.load, Load::Loading)
    }

    pub fn load_error(&self) -> Option<&str> {
        match &self.load {
            Load::Failed(e) => Some(e),
            _ => None,
        }
    }

    pub fn document(&self) -> Option<&Document> {
        self.doc.as_ref()
    }

    pub fn find_state(&self) -> &FindState {
        &self.find
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Time the background `Document::open` took (index + highlight keep running after it).
    pub fn load_time(&self) -> Option<Duration> {
        self.load_time
    }

    /// Time from `new` until the first rows of the file were built for painting.
    pub fn first_screen_time(&self) -> Option<Duration> {
        self.first_rows_at.get().map(|t| t.duration_since(self.opened_at))
    }

    /// The plugin banner, when the source view should show one.
    pub fn plugin_hint(&self) -> Option<PluginHint> {
        let doc = self.doc.as_ref()?;
        if self.body != Body::Code {
            return None;
        }
        let kind = self.kind?;
        let lang_ok = kind.lang().is_some_and(viewer_svc::Lang::has_grammar);
        let ext = self.path.extension().and_then(|e| e.to_str());
        plugin_hint(kind, ext, doc.grammar_id().as_deref(), doc.highlight_warning().as_deref(), lang_ok)
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        self.find_field.update(cx, |f, _| f.set_theme(theme));
        self.goto_field.update(cx, |f, _| f.set_theme(theme));
        cx.notify();
    }

    /// Drops the document and every background task (called when the tab closes).
    pub fn release(&mut self) {
        self.doc = None;
        self.load_task = None;
        self.poll_task = None;
        self.diff_task = None;
        self.md_task = None;
        self.data_task = None;
        self.find_task = None;
        self.diff = DiffState::Idle;
        self.data = DataState::Idle;
        self.md = MdState::default();
        self.find.set_query("");
        self.find_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // ---- loading -----------------------------------------------------------------------------

    fn begin_load(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Loading;
        let path = self.path.clone();
        let limits = self.cfg.limits.clone();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let t0 = Instant::now();
            let result = cx
                .background_executor()
                .spawn(async move { Document::open(&path, limits).map_err(|e| e.to_string()) })
                .await;
            let elapsed = t0.elapsed();
            this.update(cx, |this, cx| this.finish_load(result, elapsed, cx)).ok();
        }));
        cx.notify();
    }

    fn finish_load(&mut self, result: Result<Document, String>, elapsed: Duration, cx: &mut Context<Self>) {
        self.load_time = Some(elapsed);
        match result {
            Err(e) => self.load = Load::Failed(e),
            Ok(doc) => {
                let kind = doc.kind();
                self.size = doc.len();
                self.image_dims = matches!(kind, FileKind::Image { .. })
                    .then(|| image_dimensions(&doc.raw_bytes()[..doc.raw_bytes().len().min(64 * 1024)]))
                    .flatten();
                self.kind = Some(kind);
                self.doc = Some(doc);
                self.load = Load::Ready;
                self.body = if self.cfg.open_in_diff && supports_diff(kind) { Body::Diff } else { default_body(kind) };
                self.ensure_body(cx);
                self.start_poll(cx);
                if let Some(line) = self.cfg.line {
                    self.scroll.scroll_to_item(line.saturating_sub(1), ScrollStrategy::Center);
                }
            }
        }
        cx.notify();
    }

    /// Re-renders while the line index or the highlight parse are still running, then stops.
    fn start_poll(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else { return };
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                let wait = if tick < 25 { 30 } else { 120 };
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                let done = doc.is_indexed() && (doc.grammar_id().is_none() || doc.highlight_ready());
                if this.update(cx, |_, cx| cx.notify()).is_err() || done || tick > 400 {
                    break;
                }
                tick += 1;
            }
        }));
    }

    /// Starts the background work the current body needs.
    fn ensure_body(&mut self, cx: &mut Context<Self>) {
        match self.body {
            Body::Diff if matches!(self.diff, DiffState::Idle) => self.start_diff(cx),
            Body::Markdown if self.md.blocks.is_none() => self.start_markdown(cx),
            Body::Tree(_) if matches!(self.data, DataState::Idle) => self.start_data(cx),
            _ => {}
        }
    }

    // ---- body switching ----------------------------------------------------------------------

    pub fn set_body(&mut self, body: Body, cx: &mut Context<Self>) {
        if self.body == body || self.doc.is_none() {
            return;
        }
        self.body = body;
        self.scroll = UniformListScrollHandle::new();
        self.mini_gen += 1;
        self.ensure_body(cx);
        cx.notify();
    }

    /// "Ver diff vs HEAD" toggle: diff <-> the file's default body.
    pub fn toggle_diff(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.kind else { return };
        if !supports_diff(kind) {
            return;
        }
        let next = if self.body == Body::Diff { default_body(kind) } else { Body::Diff };
        self.set_body(next, cx);
    }

    /// Rendered <-> source (markdown, JSON/TOML/YAML, SVG).
    pub fn toggle_source(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.kind else { return };
        let Some(source) = source_body(kind) else { return };
        let next = if self.body == source { default_body(kind) } else { source };
        self.set_body(next, cx);
    }

    pub fn set_diff_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if self.diff_split != split {
            self.diff_split = split;
            self.scroll = UniformListScrollHandle::new();
            self.mini_gen += 1;
            cx.notify();
        }
    }

    pub fn set_image_fit(&mut self, fit: bool, cx: &mut Context<Self>) {
        if self.image_fit != fit {
            self.image_fit = fit;
            cx.notify();
        }
    }

    // ---- diff --------------------------------------------------------------------------------

    fn base_label(&self) -> SharedString {
        self.cfg.base_label.clone().unwrap_or_else(|| "HEAD".into())
    }

    fn start_diff(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            self.diff = DiffState::Failed("Este archivo no esta dentro de un repositorio git".into());
            return;
        };
        self.diff = DiffState::Loading;
        let (path, base) = (self.path.clone(), self.cfg.diff_base.clone());
        self.diff_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { viewer_svc::diff_against_git(&root, &path, base).map_err(|e| e.to_string()) })
                .await;
            this.update(cx, |this, cx| {
                this.diff = match result {
                    Ok(hunks) => DiffState::Ready(Box::new(build_diff(&hunks))),
                    Err(e) => DiffState::Failed(e),
                };
                this.mini_gen += 1;
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- markdown / data trees ---------------------------------------------------------------

    fn start_markdown(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else { return };
        if doc.len() > MAX_MARKDOWN_BYTES {
            self.body = Body::Code;
            return;
        }
        self.md_task = Some(cx.spawn(async move |this, cx| {
            let (blocks, highlights) = cx
                .background_executor()
                .spawn(async move {
                    let blocks = viewer_svc::markdown_blocks(&doc.to_text());
                    let hl: HashMap<usize, Vec<LineSpans>> = md_view::code_blocks(&blocks)
                        .into_iter()
                        .map(|(i, lang, text)| (i, crate::snippet::highlight_snippet(&lang, &text)))
                        .collect();
                    (blocks, hl)
                })
                .await;
            this.update(cx, |this, cx| {
                this.md = MdState { blocks: Some(Rc::new(blocks)), highlights };
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_data(&mut self, cx: &mut Context<Self>) {
        let (Some(doc), Body::Tree(format)) = (self.doc.clone(), self.body) else { return };
        self.data = DataState::Loading;
        self.data_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { DataTree::parse(format, doc.raw_bytes()) })
                .await;
            this.update(cx, |this, cx| {
                this.data = match result {
                    Ok(t) => DataState::Ready(Box::new(t)),
                    Err(e) => DataState::Failed(e),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn toggle_data(&mut self, path: &[usize], cx: &mut Context<Self>) {
        if let DataState::Ready(t) = &mut self.data {
            t.toggle(path);
            cx.notify();
        }
    }

    fn more_data(&mut self, path: &[usize], cx: &mut Context<Self>) {
        if let DataState::Ready(t) = &mut self.data {
            t.load_more(path);
            cx.notify();
        }
    }

    // ---- scrolling, go to line, find ---------------------------------------------------------

    /// First visible line of the code / diff body (0-based).
    pub fn top_line(&self) -> usize {
        let y = f32::from(self.scroll.0.borrow().base_handle.offset().y);
        ((-y).max(0.0) / ROW_H) as usize
    }

    /// Scrolls the code body to `line` (0-based) and centers it.
    pub fn goto_line(&mut self, line: usize, cx: &mut Context<Self>) {
        if self.body != Body::Code {
            if self.kind.is_some_and(|k| k.is_textual()) {
                self.set_body(Body::Code, cx);
            } else {
                return;
            }
        }
        self.scroll.scroll_to_item(line, ScrollStrategy::Center);
        cx.notify();
    }

    fn goto_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let total = self.doc.as_ref().map_or(0, |d| d.line_count().max(d.estimated_line_count()));
        if let Some(target) = parse_goto(text, total) {
            self.goto_line(target.line, cx);
            self.goto_open = false;
        }
        cx.notify();
    }

    fn close_bars(&mut self, cx: &mut Context<Self>) {
        self.find_open = false;
        self.goto_open = false;
        cx.notify();
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.body != Body::Code {
            return;
        }
        self.find_open = true;
        self.goto_open = false;
        self.find_field.read(cx).focus(window);
        cx.notify();
    }

    /// Opens the find bar with `query` already searched (used by "find in file" entry points).
    pub fn show_find(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.body != Body::Code {
            return;
        }
        self.find_open = true;
        self.goto_open = false;
        self.find_field.update(cx, |f, cx| f.set_text(query, cx));
        self.find_field.read(cx).focus(window);
        self.run_find(query, cx);
    }

    fn open_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc.is_none() || !self.kind.is_some_and(|k| k.is_textual()) {
            return;
        }
        self.goto_open = true;
        self.find_open = false;
        self.goto_field.update(cx, |f, cx| f.set_text("", cx));
        self.goto_field.read(cx).focus(window);
        cx.notify();
    }

    fn run_find(&mut self, query: &str, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        self.find_cancel.store(true, Ordering::Relaxed);
        self.find_cancel = new_cancel();
        self.find.set_query(query);
        cx.notify();
        let Some(doc) = self.doc.clone() else { return };
        if query.is_empty() {
            self.find_task = None;
            return;
        }
        let (q, cancel, from) = (query.to_owned(), self.find_cancel.clone(), self.top_line());
        self.find_task = Some(cx.spawn(async move |this, cx| {
            let q2 = q.clone();
            let (hits, capped) = cx.background_executor().spawn(async move { search_document(&doc, &q2, &cancel) }).await;
            this.update(cx, |this, cx| {
                if this.find.query == q {
                    this.find.set_matches(hits, capped, from);
                    if let Some(line) = this.find.current_line() {
                        this.scroll.scroll_to_item(line, ScrollStrategy::Center);
                    }
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn step_find(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let line = if reverse { self.find.prev_match() } else { self.find.next_match() };
        if let Some(line) = line {
            self.scroll.scroll_to_item(line, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn scroll_by_pages(&mut self, pages: f32, cx: &mut Context<Self>) {
        let st = self.scroll.0.borrow();
        let Some(item) = st.last_item_size else { return };
        let (viewport, contents) = (f32::from(item.item.height), f32::from(item.contents.height));
        let y = f32::from(st.base_handle.offset().y);
        let target = (y - pages * viewport * 0.9).clamp(-(contents - viewport).max(0.0), 0.0);
        st.base_handle.set_offset(point(px(0.0), px(target)));
        drop(st);
        cx.notify();
    }

    fn scroll_to_edge(&mut self, bottom: bool, cx: &mut Context<Self>) {
        let st = self.scroll.0.borrow();
        let Some(item) = st.last_item_size else { return };
        let max = (f32::from(item.contents.height) - f32::from(item.item.height)).max(0.0);
        st.base_handle.set_offset(point(px(0.0), px(if bottom { -max } else { 0.0 })));
        drop(st);
        cx.notify();
    }

    // ---- minimap interaction -----------------------------------------------------------------

    fn minimap_total(&self) -> usize {
        match (self.body, &self.diff, &self.doc) {
            (Body::Diff, DiffState::Ready(d), _) => {
                if self.diff_split { d.split.len() } else { d.unified.len() }
            }
            (Body::Code, _, Some(doc)) => doc.line_count().max(doc.estimated_line_count()),
            _ => 0,
        }
    }

    fn minimap_pointer(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds: Bounds<Pixels> = self.mini.bounds.get();
        let total = self.minimap_total();
        if total == 0 {
            return;
        }
        let y = f32::from(position.y - bounds.origin.y);
        let row = row_at(y, f32::from(bounds.size.height), total);
        let st = self.scroll.0.borrow();
        let Some(item) = st.last_item_size else { return };
        let (viewport, contents) = (f32::from(item.item.height), f32::from(item.contents.height));
        let target = (row as f32 * ROW_H - viewport / 2.0).clamp(0.0, (contents - viewport).max(0.0));
        st.base_handle.set_offset(point(px(0.0), px(-target)));
        drop(st);
        cx.notify();
    }

    // ---- row building (called from the virtual lists while painting) -------------------------

    fn code_rows(&self, range: std::ops::Range<usize>) -> Vec<AnyElement> {
        let Some(doc) = self.doc.as_ref() else { return Vec::new() };
        if self.first_rows_at.get().is_none() {
            self.first_rows_at.set(Some(Instant::now()));
        }
        let lines = doc.lines(range.clone());
        let spans = doc.highlight_lines(range.clone());
        let total = doc.line_count().max(doc.estimated_line_count());
        let digits = total.max(1).to_string().len().max(3);
        let query = (!self.find.query.is_empty()).then_some(self.find.query.as_str());
        let current = self.find.current_line();
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let ix = range.start + i;
                let empty = Vec::new();
                let raw = spans.get(i).unwrap_or(&empty);
                let p = prepare_line(line, raw, MAX_LINE_BYTES);
                rows::code_row(&self.theme, ix + 1, digits, &p.text, &p.spans, query, current == Some(ix))
            })
            .collect()
    }

    // ---- painting pieces ---------------------------------------------------------------------

    fn chip(&self, text: impl Into<SharedString>) -> gpui::Div {
        let c = self.theme.colors;
        div()
            .flex_none()
            .h(px(22.0))
            .px(px(9.0))
            .flex()
            .items_center()
            .rounded(px(8.0))
            .bg(c.surface_3)
            .text_size(px(self.theme.typography.ui_size_sm))
            .text_color(c.muted)
            .child(text.into())
    }

    fn language_label(&self) -> SharedString {
        match self.kind {
            None => "...".into(),
            Some(FileKind::Binary) => "Binario".into(),
            Some(FileKind::TooLarge) => "Demasiado grande".into(),
            Some(FileKind::Svg) => "SVG".into(),
            Some(FileKind::Image { format }) => format.name().into(),
            Some(k) => k.lang().map_or("Texto", |l| l.name()).into(),
        }
    }

    fn header(&self, this: &Entity<Self>, compact: bool) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let shown = display_path(self.root.as_deref(), &self.path);
        let crumbs = breadcrumb(&shown);
        let last = crumbs.len().saturating_sub(1);
        let mut crumb_row = div().flex_1().min_w_0().flex().flex_row().items_center().gap(px(4.0)).overflow_hidden();
        // On narrow panes only the last two segments fit.
        let skip = if compact { last.saturating_sub(1) } else { 0 };
        for (i, seg) in crumbs.iter().enumerate().skip(skip) {
            if i > skip {
                crumb_row = crumb_row.child(icon(Icon::ChevronRight, 12.0, c.outline));
            }
            crumb_row = crumb_row.child(
                div()
                    .flex_none()
                    .text_size(px(ty.ui_size_lg))
                    .font_weight(if i == last { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
                    .text_color(if i == last { c.text } else { c.muted })
                    .child(SharedString::from(seg.clone())),
            );
        }

        let mut chips = div().flex().flex_row().flex_wrap().items_center().gap(px(6.0));
        chips = chips.child(self.chip(self.language_label()));
        if self.kind.is_some() {
            chips = chips.child(self.chip(format_bytes(self.size)));
        }
        if let Some((w, h)) = self.image_dims {
            chips = chips.child(self.chip(format!("{w} x {h}")));
        }
        if let (Some(doc), Some(k)) = (&self.doc, self.kind) {
            if k.is_textual() && self.body != Body::Diff {
                let n = doc.line_count().max(doc.estimated_line_count());
                let label = if doc.is_indexed() { format!("{n} lineas") } else { format!("~{n} lineas") };
                chips = chips.child(self.chip(label));
            }
            if doc.encoding() != viewer_svc::Encoding::Utf8 && k.is_textual() {
                chips = chips.child(self.chip(doc.encoding().label()));
            }
        }
        if self.body == Body::Diff {
            chips = chips.child(self.chip(format!("vs {}", self.base_label())));
            if let DiffState::Ready(d) = &self.diff {
                let a = rows::tint(&theme, if theme.mode == theme::Mode::Light { 2 } else { 10 }, 1.0);
                let r = rows::tint(&theme, if theme.mode == theme::Mode::Light { 1 } else { 9 }, 1.0);
                chips = chips
                    .child(self.chip(format!("+{}", d.added)).text_color(a))
                    .child(self.chip(format!("-{}", d.removed)).text_color(r));
            }
        }

        let mut actions = div().flex().flex_row().flex_wrap().items_center().gap(px(6.0)).justify_end();
        if let Some(k) = self.kind {
            if source_body(k).is_some() {
                let on_source = self.body == source_body(k).unwrap_or(Body::Code);
                let t = this.clone();
                actions = actions.child(
                    SegmentedControl::new("viewer-source", &theme, [Segment::new(rendered_label(k)), Segment::new("Fuente")])
                        .selected(usize::from(on_source))
                        .on_change(move |ix, _, cx| {
                            let want_source = ix == 1;
                            t.update(cx, |v, cx| {
                                let is_source = v.body == source_body(k).unwrap_or(Body::Code);
                                if want_source != is_source {
                                    v.toggle_source(cx);
                                }
                            });
                        }),
                );
            }
            if self.body == Body::Diff {
                let t = this.clone();
                actions = actions.child(
                    SegmentedControl::new("viewer-split", &theme, ["Unificado", "Lado a lado"])
                        .selected(usize::from(self.diff_split))
                        .on_change(move |ix, _, cx| {
                            t.update(cx, |v, cx| v.set_diff_split(ix == 1, cx));
                        }),
                );
            }
            if matches!(k, FileKind::Image { .. } | FileKind::Svg) && self.body == Body::Image {
                let t = this.clone();
                actions = actions.child(
                    SegmentedControl::new("viewer-zoom", &theme, ["Ajustar", "1:1"])
                        .selected(usize::from(!self.image_fit))
                        .on_change(move |ix, _, cx| {
                            t.update(cx, |v, cx| v.set_image_fit(ix == 0, cx));
                        }),
                );
            }
            if supports_diff(k) {
                let t = this.clone();
                actions = actions.child(
                    Pill::new("viewer-diff", &theme)
                        .icon(Icon::GitBranch)
                        .label(format!("Ver diff vs {}", self.base_label()))
                        .active(self.body == Body::Diff)
                        .on_click(move |_, _, cx| {
                            t.update(cx, |v, cx| v.toggle_diff(cx));
                        }),
                );
            }
        }
        let t = this.clone();
        actions = actions.child(GhostButton::new("viewer-editor", &theme, "Abrir en editor").on_click(move |_, _, cx| {
            t.update(cx, |v, cx| {
                let line = (v.body == Body::Code).then(|| v.top_line() + 1);
                cx.emit(ViewerEvent::OpenInEditor { path: v.path.clone(), line });
            });
        }));

        div()
            .flex_none()
            .px(px(16.0))
            .pt(px(12.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .bg(c.surface_1)
            .child(div().flex().flex_row().items_center().gap(px(8.0)).child(crumb_row))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(chips)
                    .child(actions),
            )
            .into_any_element()
    }

    fn banner(&self, this: &Entity<Self>) -> Option<AnyElement> {
        let hint = self.plugin_hint()?;
        let theme = self.theme;
        let c = theme.colors;
        let t = this.clone();
        let id = hint.id.clone();
        Some(
            div()
                .flex_none()
                .mx(px(12.0))
                .mt(px(8.0))
                .px(px(12.0))
                .py(px(4.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .rounded(px(10.0))
                .bg(c.surface_1)
                .text_size(px(theme.typography.ui_size_sm))
                .text_color(if hint.failed { c.error } else { c.muted })
                .child(icon(Icon::Alert, 14.0, if hint.failed { c.error } else { c.outline }))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(SharedString::from(hint.message)))
                .child(
                    GhostButton::new("viewer-install-grammar", &theme, if hint.failed { "Reinstalar" } else { "Instalar" })
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            t.update(cx, |_, cx| cx.emit(ViewerEvent::InstallGrammar { id }));
                        }),
                )
                .into_any_element(),
        )
    }

    /// The floating find / go-to-line bar (a popover: the one place a 1 px `outline_soft` is allowed).
    fn bar(&self, this: &Entity<Self>) -> Option<AnyElement> {
        let theme = self.theme;
        let c = theme.colors;
        let (field, status, find) = if self.find_open {
            (self.find_field.clone(), self.find.status(), true)
        } else if self.goto_open {
            (self.goto_field.clone(), String::new(), false)
        } else {
            return None;
        };
        let mut row = div()
            .absolute()
            .top(px(8.0))
            .right(px(16.0))
            .w(px(if find { 380.0 } else { 300.0 }))
            .max_w_full()
            .h(px(40.0))
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(12.0))
            .bg(c.surface_2)
            .border_1()
            .border_color(c.outline_soft)
            .child(icon(if find { Icon::Search } else { Icon::ChevronRight }, 14.0, c.muted))
            .child(field);
        if find {
            let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
            row = row
                .child(div().flex_none().text_size(px(theme.typography.ui_size_sm)).text_color(c.muted).child(SharedString::from(status)))
                .child(GhostButton::new("find-prev", &theme, "Ant.").on_click(move |_, _, cx| {
                    t1.update(cx, |v, cx| v.step_find(true, cx));
                }))
                .child(GhostButton::new("find-next", &theme, "Sig.").on_click(move |_, _, cx| {
                    t2.update(cx, |v, cx| v.step_find(false, cx));
                }))
                .child(ui_kit::IconButton::new("find-close", &theme, Icon::Close).size(28.0).icon_size(12.0).on_click(
                    move |_, _, cx| {
                        t3.update(cx, |v, cx| v.close_bars(cx));
                    },
                ));
        }
        Some(row.into_any_element())
    }

    fn message_view(&self, icon_kind: Icon, text: impl Into<SharedString>) -> AnyElement {
        let c = self.theme.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .text_size(px(self.theme.typography.ui_size_md))
            .text_color(c.muted)
            .child(icon(icon_kind, 16.0, c.outline))
            .child(text.into())
            .into_any_element()
    }

    fn loading_view(&self) -> AnyElement {
        let c = self.theme.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .text_size(px(self.theme.typography.ui_size_md))
            .text_color(c.muted)
            .child(Spinner::new(c.accent))
            .child("Abriendo...")
            .into_any_element()
    }

    // ---- bodies ------------------------------------------------------------------------------

    fn code_body(&self, this: &Entity<Self>) -> AnyElement {
        let Some(doc) = &self.doc else { return div().into_any_element() };
        let total = doc.line_count().max(doc.estimated_line_count());
        if total == 0 || doc.is_empty() {
            return self.message_view(Icon::Files, "Archivo vacio");
        }
        let t = this.clone();
        VirtualList::new("viewer-code", total, ROW_H, move |range, _, cx| t.read(cx).code_rows(range))
            .track_scroll(self.scroll.clone())
            .size_full()
            .py(px(6.0))
            .into_any_element()
    }

    fn diff_body(&self) -> AnyElement {
        let theme = self.theme;
        match &self.diff {
            DiffState::Idle | DiffState::Loading => self.loading_view(),
            DiffState::Failed(e) => self.message_view(Icon::Alert, e.clone()),
            DiffState::Ready(d) => {
                if d.hunks == 0 {
                    return self.message_view(Icon::Check, format!("Sin cambios respecto a {}", self.base_label()));
                }
                let digits = d.digits;
                if self.diff_split {
                    let rows = d.split.clone();
                    VirtualList::new("viewer-diff-split", rows.len(), ROW_H, move |range, _, _| {
                        range
                            .filter_map(|i| rows.get(i))
                            .map(|r| match r {
                                SplitRow::Hunk(h) => rows::hunk_row(&theme, h),
                                SplitRow::Pair { left, right } => rows::split_row(&theme, left.as_ref(), right.as_ref(), digits),
                            })
                            .collect()
                    })
                    .track_scroll(self.scroll.clone())
                    .size_full()
                    .into_any_element()
                } else {
                    let rows = d.unified.clone();
                    VirtualList::new("viewer-diff", rows.len(), ROW_H, move |range, _, _| {
                        range
                            .filter_map(|i| rows.get(i))
                            .map(|r| match r {
                                UnifiedRow::Hunk(h) => rows::hunk_row(&theme, h),
                                UnifiedRow::Line(l) => rows::unified_row(&theme, l, digits),
                            })
                            .collect()
                    })
                    .track_scroll(self.scroll.clone())
                    .size_full()
                    .into_any_element()
                }
            }
        }
    }

    fn markdown_body(&self) -> AnyElement {
        let theme = self.theme;
        let Some(blocks) = self.md.blocks.clone() else { return self.loading_view() };
        let base_dir = self.path.parent().map(Path::to_path_buf);
        let render = MdRender::new(theme, base_dir, &self.md.highlights);
        let children = md_view::render_blocks(&blocks, &render);
        div()
            .id("viewer-md")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.md_scroll)
            .child(
                div()
                    .w_full()
                    .px(px(32.0))
                    .py(px(20.0))
                    .font_family(theme.typography.ui_family)
                    .text_color(theme.colors.text)
                    .flex()
                    .flex_col()
                    .children(children),
            )
            .into_any_element()
    }

    fn data_body(&self, this: &Entity<Self>) -> AnyElement {
        let theme = self.theme;
        match &self.data {
            DataState::Idle | DataState::Loading => self.loading_view(),
            DataState::Failed(e) => {
                let source = source_body(self.kind.unwrap_or(FileKind::Binary)).is_some();
                self.message_view(
                    Icon::Alert,
                    format!("No se pudo leer como arbol: {e}{}", if source { " (usa Fuente para ver el texto)" } else { "" }),
                )
            }
            DataState::Ready(t) => {
                let n = t.len();
                let this = this.clone();
                VirtualList::new("viewer-data", n, DATA_ROW_H, move |range, _, cx| {
                    let me = this.read(cx);
                    let DataState::Ready(t) = &me.data else { return Vec::new() };
                    range
                        .filter_map(|i| t.rows().get(i).map(|r| (i, r.clone())))
                        .map(|(i, row)| {
                            let path = row.path.clone();
                            let more = row.more.is_some();
                            let expandable = row.expandable;
                            let handle = this.clone();
                            rows::data_row(&theme, &row)
                                .id(("data-row", i))
                                .when(expandable || more, |d| d.cursor_pointer())
                                .hover(|s| s.bg(theme.colors.surface_1))
                                .on_click(move |_, _, cx| {
                                    let path = path.clone();
                                    handle.update(cx, |v, cx| {
                                        if more {
                                            v.more_data(&path, cx);
                                        } else if expandable {
                                            v.toggle_data(&path, cx);
                                        }
                                    });
                                })
                                .into_any_element()
                        })
                        .collect()
                })
                .track_scroll(self.data_scroll.clone())
                .size_full()
                .py(px(8.0))
                .into_any_element()
            }
        }
    }

    fn image_body(&self) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let checker = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| paint_checker(bounds, window, c.surface_1, c.surface_3, 10.0),
        )
        .absolute()
        .size_full();
        let content: AnyElement = if self.image_fit {
            div()
                .absolute()
                .size_full()
                .p(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .child(img(self.path.clone()).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                .into_any_element()
        } else {
            let mut image = img(self.path.clone()).object_fit(ObjectFit::None).flex_none();
            if let Some((w, h)) = self.image_dims {
                image = image.w(px(w as f32)).h(px(h as f32));
            }
            div()
                .id("viewer-image-scroll")
                .absolute()
                .size_full()
                .overflow_scroll()
                .child(div().p(px(20.0)).child(image))
                .into_any_element()
        };
        div().relative().size_full().overflow_hidden().child(checker).child(content).into_any_element()
    }

    fn hex_body(&self) -> AnyElement {
        let Some(doc) = self.doc.clone() else { return div().into_any_element() };
        let len = usize::try_from(doc.len()).unwrap_or(usize::MAX);
        let total_rows = len.div_ceil(viewer_svc::HEX_ROW);
        let n = total_rows.min(MAX_HEX_ROWS);
        if n == 0 {
            return self.message_view(Icon::Files, "Archivo vacio");
        }
        let theme = self.theme;
        let note = match self.kind {
            Some(FileKind::TooLarge) => {
                format!("Archivo demasiado grande ({}): solo vista hex", format_bytes(self.size))
            }
            _ => format!("Archivo binario ({}): vista hex", format_bytes(self.size)),
        };
        let c = theme.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .mx(px(12.0))
                    .mt(px(8.0))
                    .mb(px(4.0))
                    .px(px(12.0))
                    .py(px(6.0))
                    .rounded(px(10.0))
                    .bg(c.surface_1)
                    .text_size(px(theme.typography.ui_size_sm))
                    .text_color(c.muted)
                    .child(SharedString::from(note)),
            )
            .child(
                VirtualList::new("viewer-hex", n, ROW_H, move |range, _, _| {
                    let start = range.start * viewer_svc::HEX_ROW;
                    let count = range.len() * viewer_svc::HEX_ROW;
                    doc.hex_preview(start as u64, count).iter().map(|l| rows::hex_row(&theme, l)).collect()
                })
                .track_scroll(self.scroll.clone())
                .flex_1()
                .py(px(4.0)),
            )
            .into_any_element()
    }

    fn minimap_source(&self) -> Option<MinimapSource> {
        match (self.body, &self.diff, &self.doc) {
            (Body::Code, _, Some(doc)) => Some(MinimapSource::Doc(doc.clone())),
            (Body::Diff, DiffState::Ready(d), _) if d.hunks > 0 => {
                let m = if self.diff_split { &d.split_mini } else { &d.unified_mini };
                Some(MinimapSource::Rows { lens: m.lens.clone(), changed: m.changed.clone() })
            }
            _ => None,
        }
    }

    fn body_view(&self, this: &Entity<Self>) -> AnyElement {
        match self.body {
            Body::Code => self.code_body(this),
            Body::Diff => self.diff_body(),
            Body::Markdown => self.markdown_body(),
            Body::Tree(_) => self.data_body(this),
            Body::Image => self.image_body(),
            Body::Hex => self.hex_body(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pure-ish helpers
// ---------------------------------------------------------------------------------------------

fn build_diff(hunks: &[Hunk]) -> DiffData {
    let unified = diff_rows::unified_rows(hunks);
    let split = diff_rows::split_rows(hunks);
    let (ulens, uchanged) = diff_rows::unified_minimap_source(&unified);
    let (slens, schanged) = diff_rows::split_minimap_source(&split);
    let (added, removed) = diff_rows::totals(hunks);
    DiffData {
        added,
        removed,
        digits: diff_rows::gutter_digits(hunks),
        hunks: hunks.len(),
        unified: Rc::new(unified),
        split: Rc::new(split),
        unified_mini: MiniRows { lens: Rc::new(ulens), changed: Rc::new(uchanged) },
        split_mini: MiniRows { lens: Rc::new(slens), changed: Rc::new(schanged) },
    }
}

/// Number of cells of the "dark" tone in a `w` x `h` checkerboard of `cell`-px squares (the
/// light tone is the background, so only these are painted).
pub fn checker_dark_cells(w: f32, h: f32, cell: f32) -> usize {
    if cell <= 0.0 || w <= 0.0 || h <= 0.0 {
        return 0;
    }
    let (cols, rows) = ((w / cell).ceil() as usize, (h / cell).ceil() as usize);
    (cols * rows) / 2
}

/// The dither checkerboard behind images: one fill for the light tone, one quad per dark cell.
fn paint_checker(bounds: Bounds<Pixels>, window: &mut Window, light: theme::Rgba, dark: theme::Rgba, cell: f32) {
    window.paint_quad(fill(bounds, light));
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (cols, rows) = ((w / cell).ceil() as usize, (h / cell).ceil() as usize);
    for r in 0..rows {
        for col in 0..cols {
            if (r + col) % 2 == 1 {
                let x = bounds.origin.x + px(col as f32 * cell);
                let y = bounds.origin.y + px(r as f32 * cell);
                let cw = px(cell).min(bounds.size.width - px(col as f32 * cell));
                let ch = px(cell).min(bounds.size.height - px(r as f32 * cell));
                window.paint_quad(fill(Bounds::new(point(x, y), size(cw, ch)), dark));
            }
        }
    }
}

impl Render for FileViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let c = theme.colors;
        let this = cx.entity();
        let window_w = f32::from(window.viewport_size().width);

        let body: AnyElement = match &self.load {
            Load::Loading => self.loading_view(),
            Load::Failed(e) => self.message_view(Icon::Alert, e.clone()),
            Load::Ready => {
                let inner = self.body_view(&this);
                let capped = match reading_cap(window_w, self.body) {
                    Some(cap) => div()
                        .size_full()
                        .flex()
                        .justify_center()
                        .child(div().w_full().max_w(px(cap)).h_full().child(inner))
                        .into_any_element(),
                    None => inner,
                };
                let mini = has_minimap(self.body).then(|| self.minimap_source()).flatten();
                match mini {
                    Some(source) => {
                        let scroll = self.scroll.clone();
                        let strip = minimap_el(theme, source, scroll, ROW_H, &self.mini, self.mini_gen);
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .child(div().flex_1().min_w_0().h_full().child(capped))
                            .child(
                                div()
                                    .id("viewer-minimap")
                                    .flex_none()
                                    .w(px(STRIP_W))
                                    .h_full()
                                    .py(px(6.0))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                            this.mini_drag = true;
                                            this.minimap_pointer(e.position, cx);
                                        }),
                                    )
                                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                                        if this.mini_drag && e.dragging() {
                                            this.minimap_pointer(e.position, cx);
                                        }
                                    }))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseUpEvent, _, _| this.mini_drag = false),
                                    )
                                    .on_mouse_up_out(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseUpEvent, _, _| this.mini_drag = false),
                                    )
                                    .child(strip),
                            )
                            .into_any_element()
                    }
                    None => capped,
                }
            }
        };

        let compact = window_w < 720.0;
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenFind, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &OpenGoto, window, cx| this.open_goto(window, cx)))
            .on_action(cx.listener(|this, _: &CloseBars, _, cx| this.close_bars(cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.step_find(false, cx)))
            .on_action(cx.listener(|this, _: &FindPrev, _, cx| this.step_find(true, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.scroll_by_pages(1.0, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.scroll_by_pages(-1.0, cx)))
            .on_action(cx.listener(|this, _: &ScrollTop, _, cx| this.scroll_to_edge(false, cx)))
            .on_action(cx.listener(|this, _: &ScrollBottom, _, cx| this.scroll_to_edge(true, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, _| window.focus(&this.focus)),
            )
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(Radius::MD))
            .bg(c.surface_0)
            .font_family(theme.typography.ui_family)
            .text_color(c.text)
            .child(self.header(&this, compact))
            .children(self.banner(&this))
            .child(div().flex_1().min_h_0().child(body))
            .children(self.bar(&this))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::fs;

    fn open(cx: &mut TestAppContext, path: &Path, cfg: ViewerConfig) -> Entity<FileViewer> {
        let path = path.to_path_buf();
        let v = cx.update(|cx| cx.new(|cx| FileViewer::new(path, cfg, Theme::dark(), cx)));
        cx.executor().run_until_parked();
        v
    }

    fn numbered(dir: &Path, name: &str, n: usize) -> PathBuf {
        let p = dir.join(name);
        let body: String = (1..=n).map(|i| if i % 100 == 0 { format!("needle {i}\n") } else { format!("line {i}\n") }).collect();
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn checker_cell_count() {
        assert_eq!(checker_dark_cells(100.0, 100.0, 10.0), 50);
        assert_eq!(checker_dark_cells(0.0, 100.0, 10.0), 0);
        assert_eq!(checker_dark_cells(100.0, 100.0, 0.0), 0);
        // partial cells at the edge still count as cells
        assert_eq!(checker_dark_cells(25.0, 10.0, 10.0), 1);
    }

    #[test]
    fn default_config_compares_against_head() {
        let c = ViewerConfig::default();
        assert_eq!(c.diff_base, GitBase::Head);
        assert!(!c.open_in_diff);
        assert!(c.line.is_none());
    }

    #[gpui::test]
    async fn text_opens_as_code_and_goes_to_a_line(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = numbered(dir.path(), "a.txt", 1_000);
        let v = open(cx, &path, ViewerConfig::default());
        v.read_with(cx, |v, _| {
            assert!(!v.is_loading());
            assert_eq!(v.body(), Body::Code);
            let doc = v.document().unwrap();
            doc.wait_indexed();
            assert_eq!(doc.line_count(), 1_000);
        });
        v.update(cx, |v, cx| v.goto_text("250", cx));
        let target = v.read_with(cx, |v, _| v.scroll.0.borrow().deferred_scroll_to_item.map(|d| d.item_index));
        assert_eq!(target, Some(249));
        // past the end clamps to the last line
        v.update(cx, |v, cx| v.goto_text("99999", cx));
        let target = v.read_with(cx, |v, _| v.scroll.0.borrow().deferred_scroll_to_item.map(|d| d.item_index));
        assert_eq!(target, Some(999));
    }

    #[gpui::test]
    async fn find_collects_matching_lines_and_steps(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = numbered(dir.path(), "a.txt", 1_000);
        let v = open(cx, &path, ViewerConfig::default());
        v.update(cx, |v, cx| v.run_find("NEEDLE", cx));
        cx.executor().run_until_parked();
        v.read_with(cx, |v, _| {
            assert_eq!(v.find_state().matches.len(), 10);
            assert_eq!(v.find_state().current_line(), Some(99));
            assert_eq!(v.find_state().status(), "1 / 10");
        });
        v.update(cx, |v, cx| v.step_find(false, cx));
        assert_eq!(v.read_with(cx, |v, _| v.find_state().current_line()), Some(199));
        v.update(cx, |v, cx| v.step_find(true, cx));
        v.update(cx, |v, cx| v.step_find(true, cx));
        assert_eq!(v.read_with(cx, |v, _| v.find_state().current_line()), Some(999), "wraps backwards");
        v.update(cx, |v, cx| v.run_find("zzz-not-there", cx));
        cx.executor().run_until_parked();
        assert_eq!(v.read_with(cx, |v, _| v.find_state().status()), "sin resultados");
    }

    #[gpui::test]
    async fn kinds_route_to_their_bodies(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("a.md");
        fs::write(&md, "# Title\n\nSome *text*.\n").unwrap();
        let json = dir.path().join("a.json");
        fs::write(&json, r#"{"a":{"b":[1,2,3]}}"#).unwrap();
        let bin = dir.path().join("a.bin");
        fs::write(&bin, [0u8, 1, 2, 3, 0, 255, 254, 0]).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        png.extend(8u32.to_be_bytes());
        png.extend(4u32.to_be_bytes());
        png.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        let img = dir.path().join("a.png");
        fs::write(&img, png).unwrap();

        let v = open(cx, &md, ViewerConfig::default());
        v.read_with(cx, |v, _| {
            assert_eq!(v.body(), Body::Markdown);
            assert!(v.md.blocks.as_ref().is_some_and(|b| !b.is_empty()));
        });
        // rendered <-> source
        v.update(cx, |v, cx| v.toggle_source(cx));
        assert_eq!(v.read_with(cx, |v, _| v.body()), Body::Code);
        v.update(cx, |v, cx| v.toggle_source(cx));
        assert_eq!(v.read_with(cx, |v, _| v.body()), Body::Markdown);

        let v = open(cx, &json, ViewerConfig::default());
        v.read_with(cx, |v, _| {
            assert_eq!(v.body(), Body::Tree(viewer_svc::Format::Json));
            assert!(matches!(&v.data, DataState::Ready(t) if t.len() >= 3));
        });

        let v = open(cx, &bin, ViewerConfig::default());
        assert_eq!(v.read_with(cx, |v, _| v.body()), Body::Hex);
        // binaries cannot be diffed
        v.update(cx, |v, cx| v.toggle_diff(cx));
        assert_eq!(v.read_with(cx, |v, _| v.body()), Body::Hex);

        let v = open(cx, &img, ViewerConfig::default());
        v.read_with(cx, |v, _| {
            assert_eq!(v.body(), Body::Image);
            assert_eq!(v.image_dims, Some((8, 4)));
        });
    }

    #[gpui::test]
    async fn missing_file_reports_an_error(cx: &mut TestAppContext) {
        let v = open(cx, Path::new("C:/definitely/not/here.rs"), ViewerConfig::default());
        v.read_with(cx, |v, _| assert!(v.load_error().is_some()));
    }

    #[gpui::test]
    async fn plugin_banner_for_a_language_without_grammar(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let cs = dir.path().join("Program.cs");
        fs::write(&cs, "class A {}\n").unwrap();
        let v = open(cx, &cs, ViewerConfig::default());
        let hint = v.read_with(cx, |v, _| v.plugin_hint());
        let hint = hint.expect("C# is not in the vanilla build");
        assert_eq!(hint.id, "csharp");
        let txt = dir.path().join("a.txt");
        fs::write(&txt, "hello\n").unwrap();
        let v = open(cx, &txt, ViewerConfig::default());
        assert!(v.read_with(cx, |v, _| v.plugin_hint()).is_none());
    }

    #[gpui::test]
    async fn release_drops_the_document(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = numbered(dir.path(), "a.txt", 100);
        let v = open(cx, &path, ViewerConfig::default());
        v.update(cx, |v, _| v.release());
        assert!(v.read_with(cx, |v, _| v.document().is_none()));
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[gpui::test]
    async fn diff_mode_runs_git_and_counts_changes(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();
        if !(git(dir.path(), &["init", "-q"]) && git(dir.path(), &["add", "."]) && git(dir.path(), &["commit", "-q", "-m", "init"])) {
            return; // no git on this machine
        }
        fs::write(&file, "one\nTWO\nthree\nfour\n").unwrap();
        let cfg = ViewerConfig { project_root: Some(dir.path().to_path_buf()), open_in_diff: true, ..ViewerConfig::default() };
        let v = open(cx, &file, cfg);
        // git runs on the background executor: give it real time.
        for _ in 0..50 {
            if v.read_with(cx, |v, _| matches!(v.diff, DiffState::Ready(_) | DiffState::Failed(_))) {
                break;
            }
            cx.executor().allow_parking();
            std::thread::sleep(Duration::from_millis(50));
            cx.executor().run_until_parked();
        }
        v.read_with(cx, |v, _| {
            assert_eq!(v.body(), Body::Diff);
            let DiffState::Ready(d) = &v.diff else { panic!("diff not ready") };
            assert_eq!((d.added, d.removed), (2, 1));
            assert!(!d.unified.is_empty() && !d.split.is_empty());
        });
        // Toggling back returns to the file's own body.
        v.update(cx, |v, cx| v.toggle_diff(cx));
        assert_eq!(v.read_with(cx, |v, _| v.body()), Body::Code);
    }

    #[gpui::test]
    async fn diff_outside_a_repository_explains_itself(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = numbered(dir.path(), "a.txt", 10);
        // The temp dir may itself sit inside a repo on a dev machine: only assert when it does not.
        if find_repo_root(&path).is_some() {
            return;
        }
        let v = open(cx, &path, ViewerConfig::default());
        v.update(cx, |v, cx| v.toggle_diff(cx));
        assert!(v.read_with(cx, |v, _| matches!(v.diff, DiffState::Failed(_))));
    }

    #[gpui::test]
    async fn tab_wraps_the_viewer_and_closes_it(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = numbered(dir.path(), "main.rs", 20);
        let handle = cx.update(|cx| crate::tab::open_tab(path.clone(), ViewerConfig::default(), Theme::dark(), cx));
        cx.executor().run_until_parked();
        let info = cx.update(|cx| handle.info(cx));
        assert_eq!(info.title.as_ref(), "main.rs");
        assert_eq!(info.cwd.as_deref(), Some(dir.path()));
        let tab = handle.downcast::<crate::tab::ViewerTab>().unwrap();
        let viewer = tab.read_with(cx, |t, _| t.viewer().clone());
        assert!(viewer.read_with(cx, |v, _| v.document().is_some()));
        cx.update(|cx| handle.close(cx));
        assert!(viewer.read_with(cx, |v, _| v.document().is_none()), "closing the tab drops the document");
    }
}
