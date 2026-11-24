//! `FileTreePanel`: the lazily loaded project tree, as a `PanelProvider` with id `files`.
//!
//! Zero idle cost: no watcher and no timers. Directory listings and the quick filter run on the
//! background executor once, when the user expands a folder or types, and end in `cx.notify()`.
//! The host calls [`FileTreePanel::refresh`] when it knows the disk changed.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    IntoElement, KeyBinding, MouseButton, ParentElement as _, Render, ScrollStrategy, SharedString,
    Styled as _, Subscription, WeakEntity, Window, actions, div, prelude::*, px, transparent_black,
};
use panel_api::{PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Radius, Theme};
use ui_kit::text_field::act as text_act;
use ui_kit::{
    FocusNext, FocusPrev, Icon, InputModality, TextField, TextFieldEvent, UniformListScrollHandle,
    VirtualList, icon, input_modality,
};

use crate::event::ViewerEvent;
use crate::file_icon::FileGlyph;
use crate::tree_model::{
    FileTree, FilterHit, GitMark, Toggle, dirty_dirs, filter_files, list_dir, normalize_marks,
};

/// Panel id for the registry and `layout.right_panel`.
pub const PANEL_ID: &str = "files";

/// Fixed row height of the tree (px).
pub const ROW_H: f32 = 28.0;

actions!(
    ui_viewer_tree,
    [
        /// Cursor up one row.
        TreeUp,
        /// Cursor down one row.
        TreeDown,
        /// Collapse the folder under the cursor, or go to its parent.
        TreeLeft,
        /// Expand the folder under the cursor, or go to its first child.
        TreeRight,
        /// Open the file (or toggle the folder) under the cursor.
        TreeOpen,
        /// Cursor to the first row.
        TreeFirst,
        /// Cursor to the last row.
        TreeLast,
    ]
);

/// Key context of the tree / hit list (its bindings live in [`bind_keys`]).
pub const TREE_CONTEXT: &str = "FileTree";

/// Binds the keyboard navigation of the tree (called by `ui_viewer::bind_keys`).
pub fn bind_keys(cx: &mut App) {
    let c = Some(TREE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", TreeUp, c),
        KeyBinding::new("down", TreeDown, c),
        KeyBinding::new("left", TreeLeft, c),
        KeyBinding::new("right", TreeRight, c),
        KeyBinding::new("enter", TreeOpen, c),
        KeyBinding::new("space", TreeOpen, c),
        KeyBinding::new("home", TreeFirst, c),
        KeyBinding::new("end", TreeLast, c),
    ]);
}

type EventHandler = Rc<dyn Fn(&ViewerEvent, &mut App)>;

#[derive(Default)]
struct Shared {
    marks: HashMap<PathBuf, GitMark>,
    panels: Vec<WeakEntity<FileTreePanel>>,
}

/// The `files` panel provider. `Clone` it before registering to keep a handle for
/// [`FileTreeProvider::set_git_status`].
///
/// ```ignore
/// let files = FileTreeProvider::new().on_event(|ev, cx| host.handle_viewer_event(ev, cx));
/// registry.register(files.clone());
/// // later, when git_svc reports a new status:
/// files.set_git_status(marks, cx);
/// ```
#[derive(Clone, Default)]
pub struct FileTreeProvider {
    on_event: Option<EventHandler>,
    shared: Rc<RefCell<Shared>>,
}

impl FileTreeProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called for every [`ViewerEvent`] the built panels emit (so the host does not need to
    /// downcast the `AnyView` to subscribe).
    pub fn on_event(mut self, f: impl Fn(&ViewerEvent, &mut App) + 'static) -> Self {
        self.on_event = Some(Rc::new(f));
        self
    }

    /// Feeds git markers (`M` / `A` / `?`) to every live panel and to the ones built later. Paths
    /// may be absolute or relative to the project root.
    pub fn set_git_status(&self, marks: HashMap<PathBuf, GitMark>, cx: &mut App) {
        let panels: Vec<_> = {
            let mut shared = self.shared.borrow_mut();
            shared.marks = marks.clone();
            shared.panels.retain(|p| p.upgrade().is_some());
            shared.panels.clone()
        };
        for p in panels {
            if let Some(p) = p.upgrade() {
                let marks = marks.clone();
                p.update(cx, |panel, cx| panel.set_git_status(marks, cx));
            }
        }
    }

    /// Re-lists every live panel's open folders (after a git operation or an agent run).
    pub fn refresh(&self, cx: &mut App) {
        let panels: Vec<_> = self
            .shared
            .borrow()
            .panels
            .iter()
            .filter_map(WeakEntity::upgrade)
            .collect();
        for p in panels {
            p.update(cx, |panel, cx| panel.refresh(cx));
        }
    }
}

impl PanelProvider for FileTreeProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Archivos".into()
    }

    fn icon(&self) -> Icon {
        Icon::Files
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Sidebar, PanelZone::Right]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let panel = cx.new(|cx| FileTreePanel::new(ctx, cx));
        let marks = self.shared.borrow().marks.clone();
        if !marks.is_empty() {
            panel.update(cx, |p, cx| p.set_git_status(marks, cx));
        }
        self.shared.borrow_mut().panels.push(panel.downgrade());
        if let Some(handler) = self.on_event.clone() {
            cx.subscribe(&panel, move |_, ev: &ViewerEvent, cx| handler(ev, cx))
                .detach();
        }
        panel.into()
    }

    fn context_changed(
        &self,
        view: &AnyView,
        ctx: &PanelCtx,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Ok(panel) = view.clone().downcast::<FileTreePanel>() else {
            return false;
        };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

/// What the panel shows right now.
enum Mode {
    Tree,
    /// The quick filter is active: a flat list of hits.
    Filter {
        hits: Vec<FilterHit>,
        searching: bool,
    },
}

/// The tree view entity.
pub struct FileTreePanel {
    ctx: PanelCtx,
    tree: Option<FileTree>,
    /// Bumped when the root changes so late listings of the old root are dropped.
    epoch: u64,
    marks: HashMap<PathBuf, GitMark>,
    dirty: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    hit_scroll: UniformListScrollHandle,
    /// Focus of the tree / hit list (one tab stop); the filter field has its own.
    focus: FocusHandle,
    filter: Entity<TextField>,
    mode: Mode,
    filter_epoch: u64,
    /// A file to bring into view once the folders above it are listed.
    pending_reveal: Option<PathBuf>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ViewerEvent> for FileTreePanel {}

impl FileTreePanel {
    pub fn new(ctx: PanelCtx, cx: &mut Context<Self>) -> Self {
        let theme = ctx.theme;
        // The header draws the box around the field: the field is bare.
        let filter = cx.new(|cx| {
            TextField::new(theme, cx)
                .bare()
                .placeholder("Filtrar archivos")
        });
        let sub = cx.subscribe(&filter, |this, field, ev: &TextFieldEvent, cx| match ev {
            TextFieldEvent::Changed => {
                let q = field.read(cx).text().to_owned();
                this.run_filter(&q, cx);
            }
            TextFieldEvent::Submit => this.open_cursor(cx),
            TextFieldEvent::Cancel => {
                field.update(cx, |f, cx| f.set_text("", cx));
                this.run_filter("", cx);
            }
            TextFieldEvent::SubmitReverse | TextFieldEvent::Blur | TextFieldEvent::Nav(_) => {}
        });
        let mut panel = Self {
            ctx,
            tree: None,
            epoch: 0,
            marks: HashMap::new(),
            dirty: HashSet::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            hit_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle().tab_stop(true),
            filter,
            mode: Mode::Tree,
            filter_epoch: 0,
            pending_reveal: None,
            _subs: vec![sub],
        };
        panel.set_root(
            panel
                .ctx
                .project_root
                .clone()
                .or_else(|| panel.ctx.cwd.clone()),
            cx,
        );
        panel
    }

    /// The shell moved to another tab / theme / width.
    pub fn retarget(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        let root = ctx.project_root.clone().or_else(|| ctx.cwd.clone());
        let theme_changed = ctx.theme != self.ctx.theme;
        self.ctx = ctx;
        if theme_changed {
            let theme = self.ctx.theme;
            self.filter.update(cx, |f, cx| f.set_theme(theme, cx));
        }
        if root.as_deref() != self.tree.as_ref().map(FileTree::root) {
            self.set_root(root, cx);
        }
        cx.notify();
    }

    fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.mode = Mode::Tree;
        self.tree = root.filter(|r| r.is_dir()).map(FileTree::new);
        self.selected = None;
        self.marks.clear();
        self.dirty.clear();
        self.kick_loads(cx);
        cx.notify();
    }

    /// Root folder being shown.
    pub fn root(&self) -> Option<&Path> {
        self.tree.as_ref().map(FileTree::root)
    }

    /// Replaces the git markers (absolute paths, or relative to the root).
    pub fn set_git_status(&mut self, marks: HashMap<PathBuf, GitMark>, cx: &mut Context<Self>) {
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
        self.marks = normalize_marks(&root, marks);
        self.dirty = dirty_dirs(&root, &self.marks);
        cx.notify();
    }

    /// Re-lists the open folders from disk, keeping what is expanded.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = &mut self.tree {
            t.invalidate();
        }
        self.epoch += 1;
        self.kick_loads(cx);
        cx.notify();
    }

    /// Expands the folders above `path`, selects it and scrolls to it (for "reveal in tree"). Folders
    /// that are not listed yet load in the background and the reveal completes when they arrive.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.pending_reveal = Some(path.to_path_buf());
        self.continue_reveal(cx);
    }

    fn continue_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.pending_reveal.clone() else {
            return;
        };
        let Some(tree) = &mut self.tree else { return };
        tree.reveal(&path);
        let position = tree
            .rows()
            .iter()
            .position(|&n| tree.node(n).is_some_and(|n| n.path == path));
        self.kick_loads(cx);
        if let Some(i) = position {
            self.selected = Some(path);
            self.pending_reveal = None;
            self.scroll.scroll_to_item(i, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Starts a background listing for every open folder that has none.
    fn kick_loads(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else { return };
        let respect = tree.respect_ignore;
        let pending = tree.pending_loads();
        for (index, path) in pending {
            tree.mark_loading(index);
            let epoch = self.epoch;
            cx.spawn(async move |this, cx| {
                let entries = cx
                    .background_executor()
                    .spawn(async move { list_dir(&path, respect) })
                    .await;
                this.update(cx, |this, cx| {
                    if this.epoch != epoch {
                        return;
                    }
                    if let Some(t) = &mut this.tree {
                        t.set_children(index, entries);
                    }
                    // Folders that were expanded before a refresh are listed in turn.
                    this.kick_loads(cx);
                    this.continue_reveal(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Types `text` into the quick filter (what the host does for "find file" shortcuts).
    pub fn set_filter_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.filter.update(cx, |f, cx| f.set_text(text, cx));
        self.run_filter(text, cx);
    }

    fn run_filter(&mut self, query: &str, cx: &mut Context<Self>) {
        self.filter_epoch += 1;
        let query = query.trim().to_owned();
        let Some(root) = self
            .root()
            .map(Path::to_path_buf)
            .filter(|_| !query.is_empty())
        else {
            self.mode = Mode::Tree;
            cx.notify();
            return;
        };
        let respect = self.tree.as_ref().is_none_or(|t| t.respect_ignore);
        let epoch = self.filter_epoch;
        self.mode = Mode::Filter {
            hits: Vec::new(),
            searching: true,
        };
        cx.spawn(async move |this, cx| {
            let hits = cx
                .background_executor()
                .spawn(async move { filter_files(&root, &query, respect) })
                .await;
            this.update(cx, |this, cx| {
                if this.filter_epoch == epoch {
                    // The first hit is what Enter opens: show it as selected.
                    if let Some(first) = hits.first() {
                        this.selected = Some(first.path.clone());
                    }
                    this.mode = Mode::Filter {
                        hits,
                        searching: false,
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- keyboard ----------------------------------------------------------------------------

    /// Number of rows of what is showing (tree rows, or filter hits).
    fn row_count(&self) -> usize {
        match &self.mode {
            Mode::Tree => self.tree.as_ref().map_or(0, FileTree::len),
            Mode::Filter { hits, .. } => hits.len(),
        }
    }

    /// The path of row `row` of what is showing.
    fn row_path(&self, row: usize) -> Option<PathBuf> {
        match &self.mode {
            Mode::Tree => {
                let tree = self.tree.as_ref()?;
                Some(tree.node(*tree.rows().get(row)?)?.path.clone())
            }
            Mode::Filter { hits, .. } => hits.get(row).map(|h| h.path.clone()),
        }
    }

    /// The row the keyboard acts on: the selected one, else the first.
    fn cursor(&self) -> Option<usize> {
        let n = self.row_count();
        if n == 0 {
            return None;
        }
        let at = self
            .selected
            .as_deref()
            .and_then(|sel| (0..n).find(|&i| self.row_path(i).as_deref() == Some(sel)));
        Some(at.unwrap_or(0))
    }

    /// Selects row `row` and brings it into view (`down` = the cursor moved down).
    fn select_row(&mut self, row: usize, down: bool, cx: &mut Context<Self>) {
        let Some(path) = self.row_path(row) else {
            return;
        };
        self.selected = Some(path);
        let handle = match self.mode {
            Mode::Tree => &self.scroll,
            Mode::Filter { .. } => &self.hit_scroll,
        };
        handle.scroll_to_item(
            row,
            if down {
                ScrollStrategy::Bottom
            } else {
                ScrollStrategy::Top
            },
        );
        cx.notify();
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(cur) = self.cursor() else { return };
        let last = self.row_count().saturating_sub(1);
        let next = cur.saturating_add_signed(delta).min(last);
        self.select_row(next, delta > 0, cx);
    }

    fn cursor_to(&mut self, last: bool, cx: &mut Context<Self>) {
        let n = self.row_count();
        if n > 0 {
            self.select_row(if last { n - 1 } else { 0 }, last, cx);
        }
    }

    /// `Right`: expands the folder under the cursor; on an open one, goes to its first child.
    fn cursor_right(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.mode, Mode::Tree) {
            return;
        }
        let Some(row) = self.cursor() else { return };
        let Some(tree) = &mut self.tree else { return };
        let Some(&idx) = tree.rows().get(row) else {
            return;
        };
        let Some(node) = tree.node(idx) else { return };
        if !node.is_dir {
            return;
        }
        if node.expanded {
            let first_child = tree
                .rows()
                .get(row + 1)
                .and_then(|&c| tree.node(c))
                .is_some_and(|c| c.parent == Some(idx));
            if first_child {
                self.select_row(row + 1, true, cx);
            }
        } else {
            self.selected = Some(node.path.clone());
            if let Toggle::Load { .. } = tree.toggle(idx) {
                self.start_listing(idx, cx);
            }
            cx.notify();
        }
    }

    /// `Left`: collapses the open folder under the cursor; otherwise goes to the parent folder.
    fn cursor_left(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.mode, Mode::Tree) {
            return;
        }
        let Some(row) = self.cursor() else { return };
        let Some(tree) = &mut self.tree else { return };
        let Some(&idx) = tree.rows().get(row) else {
            return;
        };
        let Some(node) = tree.node(idx) else { return };
        if node.is_dir && node.expanded {
            self.selected = Some(node.path.clone());
            tree.toggle(idx);
            cx.notify();
        } else if let Some(parent) = node.parent
            && let Some(prow) = tree.rows().iter().position(|&n| n == parent)
        {
            self.select_row(prow, false, cx);
        }
    }

    /// `Enter`: opens the file (or toggles the folder) under the cursor.
    fn open_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.cursor() else { return };
        match &self.mode {
            Mode::Tree => self.click_row(row, cx),
            Mode::Filter { hits, .. } => {
                if let Some(path) = hits.get(row).map(|h| h.path.clone()) {
                    self.activate_file(path, cx);
                }
            }
        }
    }

    fn activate_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        cx.emit(ViewerEvent::OpenFile { path });
        cx.notify();
    }

    fn click_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else { return };
        let Some(&idx) = tree.rows().get(row) else {
            return;
        };
        let Some(node) = tree.node(idx) else { return };
        let (is_dir, path) = (node.is_dir, node.path.clone());
        if is_dir {
            self.selected = Some(path);
            if let Toggle::Load { .. } = tree.toggle(idx) {
                // `toggle` already marked it as loading; list it now.
                self.start_listing(idx, cx);
            }
            cx.notify();
        } else {
            self.activate_file(path, cx);
        }
    }

    fn start_listing(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else { return };
        let Some(path) = tree.node(index).map(|n| n.path.clone()) else {
            return;
        };
        let respect = tree.respect_ignore;
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let entries = cx
                .background_executor()
                .spawn(async move { list_dir(&path, respect) })
                .await;
            this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                if let Some(t) = &mut this.tree {
                    t.set_children(index, entries);
                }
                this.kick_loads(cx);
                this.continue_reveal(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- painting ----------------------------------------------------------------------------

    fn mark_color(theme: &Theme, mark: GitMark) -> gpui::Rgba {
        let c = theme.colors;
        match mark {
            GitMark::Modified => c.warning,
            GitMark::Added => c.success,
            GitMark::Untracked => c.info,
            GitMark::Deleted | GitMark::Conflict => c.error,
        }
    }

    /// `ring`: the keyboard cursor is on this row and the tree has keyboard focus.
    fn tree_row(&self, row: usize, ring: bool, this: &Entity<Self>) -> Option<AnyElement> {
        let tree = self.tree.as_ref()?;
        let node = tree.node(*tree.rows().get(row)?)?;
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.ctx.width_class == WidthClass::Compact;
        let indent = if compact { 10.0 } else { 14.0 };
        let selected = self.selected.as_deref() == Some(node.path.as_path());
        let mark = self.marks.get(&node.path).copied();
        let dirty_dir = node.is_dir && self.dirty.contains(&node.path);

        let lead: AnyElement = if node.is_dir {
            let chev = if node.expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            icon(chev, 12.0, c.muted).into_any_element()
        } else {
            div().w(px(12.0)).flex_none().into_any_element()
        };
        let glyph: AnyElement = if node.is_dir {
            icon(
                if node.expanded {
                    Icon::FolderOpen
                } else {
                    Icon::Folder
                },
                16.0,
                c.muted,
            )
            .into_any_element()
        } else {
            let g = FileGlyph::of(&node.name);
            ui_kit::file_icon(ui_kit::FileIcon::for_name(&node.name), 16.0, g.tint(&theme))
                .into_any_element()
        };
        let name_color = match mark {
            Some(GitMark::Deleted) => c.muted,
            _ => c.text,
        };
        let this = this.clone();
        Some(
            div()
                // Keyed by path: expanding a folder above does not hand this row's hover to another.
                .id(SharedString::from(format!(
                    "tree-row-{}",
                    node.path.display()
                )))
                .w_full()
                .h(px(ROW_H))
                .pl(px(4.0 + f32::from(node.depth) * indent))
                .pr(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .rounded(px(Radius::CONTROL))
                .border_2()
                .border_color(if ring {
                    c.accent.into()
                } else {
                    transparent_black()
                })
                .cursor_pointer()
                .when(selected, |d| d.bg(c.surface_3))
                .when(!selected, |d| d.hover(move |s| s.bg(c.surface_2)))
                .child(lead)
                .child(glyph)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(theme.typography.ui_size_md))
                        .text_color(name_color)
                        .child(SharedString::from(node.name.clone())),
                )
                .when_some(mark, |d, m| {
                    d.child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .text_size(px(theme.typography.ui_size_xs))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(Self::mark_color(&theme, m))
                            .child(m.letter()),
                    )
                })
                .when(dirty_dir && mark.is_none(), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .size(px(5.0))
                            .rounded_full()
                            .bg(Self::mark_color(&theme, GitMark::Modified))
                            .opacity(0.7),
                    )
                })
                .on_click(move |_, _, cx| this.update(cx, |p, cx| p.click_row(row, cx)))
                .into_any_element(),
        )
    }

    fn hit_row(&self, hit: &FilterHit, ring: bool, this: &Entity<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.ctx.width_class == WidthClass::Compact;
        let g = FileGlyph::of(&hit.name);
        // Compact panes keep only the parent folder: enough to tell `view.rs` from `view.rs`.
        let dir = hit
            .rel
            .rsplit_once('/')
            .map(|(d, _)| {
                if compact {
                    d.rsplit('/').next().unwrap_or(d).to_owned()
                } else {
                    d.to_owned()
                }
            })
            .unwrap_or_default();
        let selected = self.selected.as_deref() == Some(hit.path.as_path());
        let path = hit.path.clone();
        let this = this.clone();
        div()
            .id(SharedString::from(format!(
                "hit-row-{}",
                hit.path.display()
            )))
            .w_full()
            .h(px(ROW_H))
            .px(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::CONTROL))
            .border_2()
            .border_color(if ring {
                c.accent.into()
            } else {
                transparent_black()
            })
            .cursor_pointer()
            .when(selected, |d| d.bg(c.surface_3))
            .when(!selected, |d| d.hover(move |s| s.bg(c.surface_2)))
            .child(ui_kit::file_icon(
                ui_kit::FileIcon::for_name(&hit.name),
                16.0,
                g.tint(&theme),
            ))
            .child(
                div()
                    .flex_none()
                    .max_w(px(180.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.ui_size_md))
                    .text_color(c.text)
                    .child(SharedString::from(hit.name.clone())),
            )
            .when(!dir.is_empty(), |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(theme.typography.ui_size_sm))
                        .text_color(c.outline)
                        .child(SharedString::from(dir)),
                )
            })
            .on_click(move |_, _, cx| {
                let path = path.clone();
                this.update(cx, |p, cx| p.activate_file(path, cx));
            })
            .into_any_element()
    }

    fn message(&self, text: &str) -> AnyElement {
        let theme = self.ctx.theme;
        div()
            .w_full()
            .px(px(12.0))
            .py(px(16.0))
            .text_size(px(theme.typography.ui_size_sm))
            .text_color(theme.colors.muted)
            .child(SharedString::from(text.to_owned()))
            .into_any_element()
    }
}

impl Render for FileTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let this = cx.entity();
        // The ring marks the keyboard cursor, only while the list has focus from the keyboard.
        let ring_row = (self.focus.is_focused(window)
            && input_modality() == InputModality::Keyboard)
            .then(|| self.cursor())
            .flatten();

        let header = div()
            .flex_none()
            .px(px(12.0))
            .pt(px(10.0))
            .pb(px(6.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div().flex().flex_row().items_center().gap(px(6.0)).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(ty.ui_size_lg))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c.text)
                        .child(SharedString::from(
                            self.tree
                                .as_ref()
                                .map_or("Archivos", FileTree::root_name)
                                .to_owned(),
                        )),
                ),
            )
            .child(
                div()
                    .h(px(32.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(Radius::PILL))
                    .bg(c.surface_2)
                    .child(icon(Icon::Search, 14.0, c.muted))
                    .child(self.filter.clone()),
            )
            // Up / Down in the filter field walk the hits (the field itself has no rows).
            .capture_action(cx.listener(|this, _: &text_act::Up, _, cx| {
                if matches!(this.mode, Mode::Filter { .. }) {
                    this.move_cursor(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &text_act::Down, _, cx| {
                if matches!(this.mode, Mode::Filter { .. }) {
                    this.move_cursor(1, cx);
                    cx.stop_propagation();
                }
            }));

        let body: AnyElement = match (&self.tree, &self.mode) {
            (None, _) => self.message("Abre una carpeta de proyecto para ver sus archivos"),
            (Some(_), Mode::Filter { hits, searching }) => {
                if hits.is_empty() {
                    self.message(if *searching {
                        "Buscando..."
                    } else {
                        "Sin coincidencias"
                    })
                } else {
                    let n = hits.len();
                    VirtualList::new("hit-list", n, ROW_H, move |range, _, cx| {
                        let me = this.read(cx);
                        let Mode::Filter { hits, .. } = &me.mode else {
                            return Vec::new();
                        };
                        range
                            .filter_map(|i| {
                                hits.get(i)
                                    .map(|h| me.hit_row(h, ring_row == Some(i), &this))
                            })
                            .collect()
                    })
                    .track_scroll(self.hit_scroll.clone())
                    .flex_1()
                    .px(px(6.0))
                    .into_any_element()
                }
            }
            (Some(tree), Mode::Tree) => {
                if tree.is_empty() {
                    let loaded = tree.node(0).is_some_and(|n| n.loaded);
                    if loaded {
                        self.message("Carpeta vacia")
                    } else {
                        div().into_any_element()
                    }
                } else {
                    let n = tree.len();
                    VirtualList::new("tree-list", n, ROW_H, move |range, _, cx| {
                        let me = this.read(cx);
                        range
                            .filter_map(|i| me.tree_row(i, ring_row == Some(i), &this))
                            .collect()
                    })
                    .track_scroll(self.scroll.clone())
                    .flex_1()
                    .px(px(6.0))
                    .into_any_element()
                }
            }
        };

        // The list (tree or hits) is one tab stop with arrow-key navigation; a click focuses it.
        let list = div()
            .id("file-tree-list")
            .key_context(TREE_CONTEXT)
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.focus)),
            )
            .on_action(cx.listener(|this, _: &TreeUp, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &TreeDown, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(|this, _: &TreeLeft, _, cx| this.cursor_left(cx)))
            .on_action(cx.listener(|this, _: &TreeRight, _, cx| this.cursor_right(cx)))
            .on_action(cx.listener(|this, _: &TreeOpen, _, cx| this.open_cursor(cx)))
            .on_action(cx.listener(|this, _: &TreeFirst, _, cx| this.cursor_to(false, cx)))
            .on_action(cx.listener(|this, _: &TreeLast, _, cx| this.cursor_to(true, cx)))
            .on_action(|_: &FocusNext, window, _| window.focus_next())
            .on_action(|_: &FocusPrev, window, _| window.focus_prev())
            .child(body);

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .font_family(ty.ui_family)
            .text_color(c.text)
            .child(header)
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::fs;

    fn ctx(root: &Path) -> PanelCtx {
        PanelCtx {
            zone: PanelZone::Sidebar,
            width_class: WidthClass::Regular,
            cwd: Some(root.to_path_buf()),
            project_root: Some(root.to_path_buf()),
            theme: Theme::dark(),
        }
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("README.md"), "# hi").unwrap();
        dir
    }

    #[gpui::test]
    async fn lists_root_lazily_and_opens_files(cx: &mut TestAppContext) {
        let p = project();
        let root = p.path().to_path_buf();
        let panel = cx.update(|cx| cx.new(|cx| FileTreePanel::new(ctx(&root), cx)));
        cx.executor().run_until_parked();
        let rows = panel.read_with(cx, |p, _| p.tree.as_ref().map(FileTree::len));
        assert_eq!(rows, Some(2), "src + README.md; src is not listed yet");

        // Expand src.
        panel.update(cx, |p, cx| p.click_row(0, cx));
        cx.executor().run_until_parked();
        let rows = panel.read_with(cx, |p, _| p.tree.as_ref().map(FileTree::len));
        assert_eq!(rows, Some(3));

        // Clicking a file emits OpenFile.
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&panel, move |_, ev: &ViewerEvent, _| {
                sink.borrow_mut().push(ev.clone())
            })
        });
        panel.update(cx, |p, cx| p.click_row(1, cx));
        assert_eq!(
            events.borrow().as_slice(),
            [ViewerEvent::OpenFile {
                path: root.join("src/main.rs")
            }]
        );
    }

    #[gpui::test]
    async fn quick_filter_lists_hits(cx: &mut TestAppContext) {
        let p = project();
        let root = p.path().to_path_buf();
        let panel = cx.update(|cx| cx.new(|cx| FileTreePanel::new(ctx(&root), cx)));
        cx.executor().run_until_parked();
        panel.update(cx, |p, cx| p.run_filter("main", cx));
        cx.executor().run_until_parked();
        let hits = panel.read_with(cx, |p, _| match &p.mode {
            Mode::Filter { hits, .. } => hits.iter().map(|h| h.rel.clone()).collect::<Vec<_>>(),
            Mode::Tree => Vec::new(),
        });
        assert_eq!(hits, ["src/main.rs"]);
        panel.update(cx, |p, cx| p.run_filter("", cx));
        assert!(panel.read_with(cx, |p, _| matches!(p.mode, Mode::Tree)));
    }

    /// The panel in a (test) window with the tree focused: keys go through the real keymap.
    fn window_panel<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
    ) -> (
        Entity<FileTreePanel>,
        &'a mut gpui::VisualTestContext,
        Rc<RefCell<Vec<ViewerEvent>>>,
    ) {
        cx.update(crate::viewer::bind_keys);
        let c = ctx(root);
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let (panel, vcx) = ui_kit::testing::open_window(cx, move |_, cx| FileTreePanel::new(c, cx));
        let _sub = vcx.update(|_, cx| {
            cx.subscribe(&panel, move |_, ev: &ViewerEvent, _| {
                sink.borrow_mut().push(ev.clone())
            })
        });
        std::mem::forget(_sub);
        vcx.run_until_parked();
        panel.update_in(vcx, |p, window, _| window.focus(&p.focus));
        (panel, vcx, events)
    }

    fn selected(panel: &Entity<FileTreePanel>, vcx: &gpui::VisualTestContext) -> Option<PathBuf> {
        panel.read_with(vcx, |p, _| p.selected.clone())
    }

    fn rows(panel: &Entity<FileTreePanel>, vcx: &gpui::VisualTestContext) -> usize {
        panel.read_with(vcx, |p, _| p.row_count())
    }

    #[gpui::test]
    async fn arrow_keys_walk_expand_collapse_and_open(cx: &mut TestAppContext) {
        use ui_kit::testing::press;
        let p = project();
        let root = p.path().to_path_buf();
        let (panel, vcx, events) = window_panel(cx, &root);
        assert_eq!(rows(&panel, vcx), 2, "src and README.md");

        // The cursor starts on the first row (src): Right expands it.
        press(vcx, "right");
        vcx.run_until_parked();
        assert_eq!(rows(&panel, vcx), 3);
        assert_eq!(selected(&panel, vcx), Some(root.join("src")));
        // Right on an open folder goes into it; Down / Up move one row.
        press(vcx, "right");
        assert_eq!(selected(&panel, vcx), Some(root.join("src/main.rs")));
        press(vcx, "down");
        assert_eq!(selected(&panel, vcx), Some(root.join("README.md")));
        press(vcx, "down");
        assert_eq!(
            selected(&panel, vcx),
            Some(root.join("README.md")),
            "stops at the last row"
        );
        press(vcx, "up up");
        assert_eq!(selected(&panel, vcx), Some(root.join("src")));
        press(vcx, "end");
        assert_eq!(selected(&panel, vcx), Some(root.join("README.md")));
        press(vcx, "home");
        assert_eq!(selected(&panel, vcx), Some(root.join("src")));

        // Left on a file goes to its folder; Left on an open folder collapses it.
        press(vcx, "down left");
        assert_eq!(selected(&panel, vcx), Some(root.join("src")));
        press(vcx, "left");
        vcx.run_until_parked();
        assert_eq!(rows(&panel, vcx), 2, "collapsed");

        // Enter toggles a folder and opens a file.
        press(vcx, "enter");
        vcx.run_until_parked();
        assert_eq!(rows(&panel, vcx), 3, "Enter on a folder expands it");
        assert!(events.borrow().is_empty(), "folders emit nothing");
        press(vcx, "down enter");
        assert_eq!(
            events.borrow().as_slice(),
            [ViewerEvent::OpenFile {
                path: root.join("src/main.rs")
            }]
        );
    }

    #[gpui::test]
    async fn tab_enters_the_tree_and_the_filter_arrows_pick_a_hit(cx: &mut TestAppContext) {
        use ui_kit::testing::{press, type_text};
        let p = project();
        let root = p.path().to_path_buf();
        let (panel, vcx, events) = window_panel(cx, &root);
        panel.update_in(vcx, |p, window, cx| p.filter.read(cx).focus(window));
        type_text(vcx, "r");
        vcx.run_until_parked();
        let hits = panel.read_with(vcx, |p, _| match &p.mode {
            Mode::Filter { hits, .. } => hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
            Mode::Tree => Vec::new(),
        });
        assert!(hits.len() >= 2, "{hits:?}");
        assert_eq!(
            selected(&panel, vcx),
            Some(hits[0].clone()),
            "the first hit is selected"
        );
        // Down in the field walks the hits without leaving the field; Enter opens the selected one.
        press(vcx, "down");
        assert_eq!(selected(&panel, vcx), Some(hits[1].clone()));
        press(vcx, "enter");
        assert_eq!(
            events.borrow().as_slice(),
            [ViewerEvent::OpenFile {
                path: hits[1].clone()
            }]
        );
        // Typing still goes to the field.
        type_text(vcx, "e");
        assert_eq!(
            panel.read_with(vcx, |p, cx| p.filter.read(cx).text().to_string()),
            "re"
        );
        // Tab leaves the field for the list (one tab stop).
        press(vcx, "tab");
        let list = panel.read_with(vcx, |p, _| p.focus.clone());
        assert!(
            ui_kit::testing::is_focused(vcx, &list),
            "Tab moved focus from the field to the list"
        );
    }

    #[gpui::test]
    async fn git_marks_normalize(cx: &mut TestAppContext) {
        let p = project();
        let root = p.path().to_path_buf();
        let panel = cx.update(|cx| cx.new(|cx| FileTreePanel::new(ctx(&root), cx)));
        panel.update(cx, |p, cx| {
            p.set_git_status(
                [(PathBuf::from("src/main.rs"), GitMark::Modified)].into(),
                cx,
            );
        });
        panel.read_with(cx, |p, _| {
            assert_eq!(
                p.marks.get(&root.join("src/main.rs")),
                Some(&GitMark::Modified)
            );
            assert!(p.dirty.contains(&root.join("src")));
        });
    }

    #[gpui::test]
    async fn retarget_to_another_root_resets_the_tree(cx: &mut TestAppContext) {
        let a = project();
        let b = tempfile::tempdir().unwrap();
        fs::write(b.path().join("only.txt"), "x").unwrap();
        let panel = cx.update(|cx| cx.new(|cx| FileTreePanel::new(ctx(a.path()), cx)));
        cx.executor().run_until_parked();
        panel.update(cx, |p, cx| p.retarget(ctx(b.path()), cx));
        cx.executor().run_until_parked();
        let rows = panel.read_with(cx, |p, _| p.tree.as_ref().map(FileTree::len));
        assert_eq!(rows, Some(1));
        assert_eq!(
            panel.read_with(cx, |p, _| p.root().map(Path::to_path_buf)),
            Some(b.path().to_path_buf())
        );
    }
}
