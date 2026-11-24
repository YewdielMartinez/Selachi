//! `ThreadListView`: the list of threads, grouped Working / Esperando / Settled (collapsible, with
//! counts) or by project. It is embeddable (a section of the unified sidebar) and also a
//! `PanelProvider` with id `"threads"`.
//!
//! Rows: agent mark, title (ellipsis), subtitle (state, project, model), elapsed time and the
//! status glyph (the shark-bars spinner while Working). On hover the row offers rename, archive
//! and delete. Delete first asks the store for a [`DeletePreview`] and then shows a confirmation
//! that lists the worktree, its branch and git's dirty warnings: nothing is deleted on one click.
//!
//! Zero idle cost: the view re-renders when the hub notifies (status changes, list changes), never
//! on a timer; `elapsed` is computed when a row is painted.

use std::collections::HashSet;
use std::path::PathBuf;

use chat_core::{
    DeleteOptions, DeletePreview, ThreadGroups, ThreadId, ThreadRecord, ThreadStatus,
    group_by_project, now_ms,
};
use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, Entity, FontWeight, IntoElement, ListAlignment,
    ListState, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
    list, prelude::*, px,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass, format_elapsed};
use theme::{Radius, Theme};
use ui_kit::{
    Button, GhostButton, Icon, IconButton, Pill, StatusGlyph, TextField, TextFieldEvent, Toggle,
    agent_icon, icon,
};

use crate::hub::{ChatEvent, ChatHub};
use crate::options::agent_mark;
use crate::rows::diff_rows;
use crate::util::project_name;

/// Id of the panel (`layout.right_panel`, sidebar sections).
pub const PANEL_ID: &str = "threads";

/// How the list is grouped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GroupMode {
    /// Working / Esperando / Settled.
    #[default]
    Status,
    /// One section per project.
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GroupKey {
    Working,
    Waiting,
    Settled,
    Project(String),
}

/// Stable element id fragment of a group (the project path for project groups).
fn group_slug(key: &GroupKey) -> String {
    match key {
        GroupKey::Working => "working".into(),
        GroupKey::Waiting => "waiting".into(),
        GroupKey::Settled => "settled".into(),
        GroupKey::Project(p) => format!("project-{p}"),
    }
}

/// One line of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum ListItem {
    Header {
        key: GroupKey,
        label: String,
        count: usize,
        collapsed: bool,
    },
    Thread(ThreadRecord),
}

/// Identity of an item (what the list splices on).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKey {
    Header(GroupKey),
    Thread(ThreadId),
}

impl ListItem {
    pub fn key(&self) -> ItemKey {
        match self {
            ListItem::Header { key, .. } => ItemKey::Header(key.clone()),
            ListItem::Thread(t) => ItemKey::Thread(t.id),
        }
    }
}

/// Flattens `threads` into headers and rows. Collapsed groups keep their header only.
pub fn build_items(
    threads: &[ThreadRecord],
    mode: GroupMode,
    collapsed: &HashSet<GroupKey>,
) -> Vec<ListItem> {
    let mut out = Vec::new();
    let section =
        |key: GroupKey, label: String, list: Vec<ThreadRecord>, out: &mut Vec<ListItem>| {
            if list.is_empty() {
                return;
            }
            let is_collapsed = collapsed.contains(&key);
            out.push(ListItem::Header {
                key,
                label,
                count: list.len(),
                collapsed: is_collapsed,
            });
            if !is_collapsed {
                out.extend(list.into_iter().map(ListItem::Thread));
            }
        };
    match mode {
        GroupMode::Status => {
            let g = ThreadGroups::from_threads(threads);
            section(GroupKey::Working, "Working".into(), g.working, &mut out);
            section(GroupKey::Waiting, "Esperando".into(), g.waiting, &mut out);
            section(GroupKey::Settled, "Settled".into(), g.settled, &mut out);
        }
        GroupMode::Project => {
            for (root, list) in group_by_project(threads) {
                let key =
                    GroupKey::Project(root.to_string_lossy().replace('\\', "/").to_lowercase());
                section(key, project_name(&root), list, &mut out);
            }
        }
    }
    out
}

/// "Working · Opus" style second line of a row.
pub fn subtitle(t: &ThreadRecord, with_project: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    match t.status {
        ThreadStatus::Working => parts.push("Working".into()),
        ThreadStatus::WaitingForInput => parts.push("Esperando".into()),
        ThreadStatus::Error => parts.push("Error".into()),
        _ => {}
    }
    if with_project {
        parts.push(project_name(&t.project_root));
    }
    parts.push(
        match crate::options::model_effort_label(t.model.as_deref(), t.effort.as_deref()) {
            Some(label) => label,
            None => chat_core::agent_label(&t.agent),
        },
    );
    if let Some(w) = &t.worktree_path {
        parts.push(project_name(w));
    }
    parts.join(" · ")
}

// ---------------------------------------------------------------------------------------------
// Delete confirmation (pure)
// ---------------------------------------------------------------------------------------------

/// The state of the "Eliminar hilo" confirmation. Nothing is deleted until [`DeleteDialog::can_confirm`]
/// and the user pressed the button; removing a worktree that has unsaved changes additionally
/// needs the explicit "descartar cambios" switch.
#[derive(Clone, Debug, PartialEq)]
pub struct DeleteDialog {
    pub preview: DeletePreview,
    /// Also remove the worktree directory (the branch is always kept).
    pub remove_worktree: bool,
    /// Acknowledged: discard uncommitted changes / a locked worktree.
    pub force: bool,
}

impl DeleteDialog {
    pub fn new(preview: DeletePreview) -> Self {
        Self {
            preview,
            remove_worktree: false,
            force: false,
        }
    }

    /// The thread has a worktree that is still on disk.
    pub fn has_worktree(&self) -> bool {
        self.preview.worktree.as_ref().is_some_and(|w| w.exists)
    }

    /// Removing the worktree would lose something: unsaved changes or git asked to confirm.
    pub fn is_risky(&self) -> bool {
        self.preview
            .worktree
            .as_ref()
            .is_some_and(|w| w.exists && (w.requires_confirmation || w.dirty == Some(true)))
    }

    /// The "descartar cambios" switch is needed before confirming.
    pub fn needs_force(&self) -> bool {
        self.remove_worktree && self.has_worktree() && self.is_risky()
    }

    /// The confirm button may be pressed.
    pub fn can_confirm(&self) -> bool {
        !self.needs_force() || self.force
    }

    /// What to pass to `ChatStore::delete_thread`.
    pub fn options(&self) -> DeleteOptions {
        let remove = self.remove_worktree && self.has_worktree();
        DeleteOptions {
            remove_worktree: remove,
            force: remove && self.force,
        }
    }

    /// Lines of the dialog body, in order.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let n = self.preview.message_count;
        out.push(format!(
            "Se borrará el historial de este hilo ({n} {}).",
            if n == 1 { "mensaje" } else { "mensajes" }
        ));
        if let Some(w) = &self.preview.worktree {
            if w.exists {
                out.push(format!("Worktree: {}", w.path.display()));
                if let Some(b) = &w.branch {
                    out.push(format!("Rama: {b} (siempre se conserva)"));
                }
                if w.dirty == Some(true) {
                    out.push("Tiene cambios sin guardar.".into());
                }
                out.extend(w.warnings.iter().cloned());
                if let Some(stat) = &w.diff_stat {
                    out.extend(stat.lines().take(6).map(str::to_string));
                }
            } else {
                out.push("Su worktree ya no existe en disco.".into());
            }
        }
        out
    }
}

enum DeleteState {
    Loading(ThreadId),
    Ready(Box<DeleteDialog>),
}

struct Renaming {
    id: ThreadId,
    field: Entity<TextField>,
    _subs: Vec<Subscription>,
}

// ---------------------------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------------------------

/// See the module docs. Give it a definite height (`.flex_1().min_h_0()` or `.h(px(..))`): the
/// rows scroll inside it.
pub struct ThreadListView {
    hub: Entity<ChatHub>,
    theme: Theme,
    width_class: WidthClass,
    mode: GroupMode,
    collapsed: HashSet<GroupKey>,
    settled_seeded: bool,
    items: Vec<ListItem>,
    keys: Vec<ItemKey>,
    list: ListState,
    renaming: Option<Renaming>,
    /// The thread row under the pointer: drives the row actions (state, not group-hover).
    hovered: Option<ThreadId>,
    delete: Option<DeleteState>,
    show_header: bool,
    primary_action: bool,
    _subs: Vec<Subscription>,
}

impl ThreadListView {
    pub fn new(
        hub: Entity<ChatHub>,
        theme: Theme,
        width_class: WidthClass,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&hub, |this, _h, cx| this.rebuild(cx));
        let mut v = Self {
            hub,
            theme,
            width_class,
            mode: GroupMode::Status,
            collapsed: HashSet::new(),
            settled_seeded: false,
            items: Vec::new(),
            keys: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.0)),
            renaming: None,
            hovered: None,
            delete: None,
            show_header: true,
            primary_action: false,
            _subs: vec![sub],
        };
        v.rebuild(cx);
        v
    }

    /// Hides the "Hilos / Nuevo hilo" header (the host draws its own).
    pub fn show_header(mut self, show: bool) -> Self {
        self.show_header = show;
        self
    }

    /// Paints "Nuevo hilo" as the accent action button (when the list is a panel of its own).
    /// Default: a tonal button, because a sidebar already has its own primary action.
    pub fn primary_action(mut self, primary: bool) -> Self {
        self.primary_action = primary;
        self
    }

    pub fn group_mode(&self) -> GroupMode {
        self.mode
    }

    pub fn set_group_mode(&mut self, mode: GroupMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.rebuild(cx);
        }
    }

    pub fn set_width_class(&mut self, wc: WidthClass, cx: &mut Context<Self>) {
        if self.width_class != wc {
            self.width_class = wc;
            cx.notify();
        }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// The flattened items currently shown.
    pub fn items(&self) -> &[ListItem] {
        &self.items
    }

    pub fn is_collapsed(&self, key: &GroupKey) -> bool {
        self.collapsed.contains(key)
    }

    /// The delete confirmation, once the preview arrived.
    pub fn delete_dialog(&self) -> Option<&DeleteDialog> {
        match &self.delete {
            Some(DeleteState::Ready(d)) => Some(d),
            _ => None,
        }
    }

    pub fn toggle_group(&mut self, key: &GroupKey, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.clone());
        }
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let (threads, loaded) = {
            let h = self.hub.read(cx);
            (h.threads().to_vec(), h.is_loaded())
        };
        if loaded && !self.settled_seeded {
            self.settled_seeded = true;
            // Like T3: a long history starts folded ("Settled 43 ›").
            if self.mode == GroupMode::Status
                && threads
                    .iter()
                    .filter(|t| t.group() == chat_core::ThreadGroup::Settled)
                    .count()
                    > 12
            {
                self.collapsed.insert(GroupKey::Settled);
            }
        }
        let items = build_items(&threads, self.mode, &self.collapsed);
        let keys: Vec<ItemKey> = items.iter().map(ListItem::key).collect();
        if let Some(sp) = diff_rows(&self.keys, &keys) {
            self.list.splice(sp.old, sp.new_len);
        }
        self.items = items;
        self.keys = keys;
        cx.notify();
    }

    // ----- actions -------------------------------------------------------------------------

    fn new_thread(&mut self, cx: &mut Context<Self>) {
        let project = self.hub.read(cx).default_project().map(PathBuf::from);
        self.hub.update(cx, |h, cx| {
            h.emit_event(ChatEvent::NewThread { project }, cx)
        });
    }

    fn open_thread(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        self.hub.update(cx, |h, cx| {
            h.set_active_thread(Some(id), cx);
            h.emit_event(ChatEvent::OpenThread(id), cx);
        });
    }

    fn begin_rename(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.hub.read(cx).thread(id).cloned() else {
            return;
        };
        let theme = self.theme;
        let title = rec.display_title().to_string();
        let field = cx.new(|cx| {
            let mut f = TextField::new(theme, cx)
                .bare()
                .placeholder("Título")
                .font_size(theme.typography.ui_size_md);
            f.set_text(&title, cx);
            f
        });
        let focus = gpui::Focusable::focus_handle(field.read(cx), cx);
        let subs = vec![
            cx.subscribe(&field, |this, f, ev: &TextFieldEvent, cx| match ev {
                TextFieldEvent::Submit => {
                    let t = f.read(cx).text().to_string();
                    this.commit_rename(t, cx);
                }
                TextFieldEvent::Cancel => {
                    this.renaming = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.on_focus_out(&focus, window, |this, _e, _w, cx| {
                if let Some(r) = &this.renaming {
                    let t = r.field.read(cx).text().to_string();
                    this.commit_rename(t, cx);
                }
            }),
        ];
        window.focus(&focus);
        self.renaming = Some(Renaming {
            id,
            field,
            _subs: subs,
        });
        cx.notify();
    }

    fn commit_rename(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(r) = self.renaming.take() else {
            return;
        };
        let text = text.trim().to_string();
        if !text.is_empty() {
            self.hub.update(cx, |h, cx| h.rename_thread(r.id, text, cx));
        }
        cx.notify();
    }

    /// Asks the store what deleting involves, then shows the confirmation.
    pub fn request_delete(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        self.delete = Some(DeleteState::Loading(id));
        let task = self.hub.update(cx, |h, cx| h.delete_preview(id, cx));
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(p) => {
                    if matches!(this.delete, Some(DeleteState::Loading(i)) if i == id) {
                        this.delete = Some(DeleteState::Ready(Box::new(DeleteDialog::new(p))));
                    }
                    cx.notify();
                }
                Err(e) => {
                    this.delete = None;
                    this.hub.update(cx, |h, cx| {
                        h.report_error("no se pudo preparar la eliminación", e, cx)
                    });
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        if self.delete.take().is_some() {
            cx.notify();
        }
    }

    /// The confirm button of the dialog. Does nothing while [`DeleteDialog::can_confirm`] is false.
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(DeleteState::Ready(d)) = &self.delete else {
            return;
        };
        if !d.can_confirm() {
            return;
        }
        let (id, opts) = (d.preview.thread.id, d.options());
        self.delete = None;
        self.hub
            .update(cx, |h, cx| h.delete_thread(id, opts, cx).detach());
        cx.notify();
    }

    pub fn set_remove_worktree(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(DeleteState::Ready(d)) = &mut self.delete {
            d.remove_worktree = on;
            if !on {
                d.force = false;
            }
            cx.notify();
        }
    }

    pub fn set_force(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(DeleteState::Ready(d)) = &mut self.delete {
            d.force = on;
            cx.notify();
        }
    }

    // ----- rendering -----------------------------------------------------------------------

    fn header_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let compact = self.width_class == WidthClass::Compact;
        let new_btn: AnyElement = if self.primary_action {
            Button::action("threads-new", &t, "Nuevo hilo")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.new_thread(cx)))
                .into_any_element()
        } else if compact {
            IconButton::new("threads-new", &t, Icon::Plus)
                .size(28.0)
                .tooltip("Nuevo hilo")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.new_thread(cx)))
                .into_any_element()
        } else {
            Button::tonal("threads-new", &t, "Nuevo hilo")
                .icon(Icon::Plus)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.new_thread(cx)))
                .into_any_element()
        };
        let mode_pill = |id: &'static str,
                         label: &'static str,
                         mode: GroupMode,
                         cx: &mut Context<Self>| {
            Pill::compact(id, &t)
                .label(label)
                .active(self.mode == mode)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_group_mode(mode, cx)),
                )
        };
        div()
            .w_full()
            .flex_none()
            .px(px(12.0))
            .pt(px(6.0))
            .pb(px(8.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .font_family(ty.ui_family)
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_lg))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child("Hilos"),
                    )
                    .child(div().flex_1())
                    .child(new_btn),
            )
            .when(!compact, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(4.0))
                        .child(mode_pill(
                            "threads-by-status",
                            "Estado",
                            GroupMode::Status,
                            cx,
                        ))
                        .child(mode_pill(
                            "threads-by-project",
                            "Proyecto",
                            GroupMode::Project,
                            cx,
                        )),
                )
            })
            .into_any_element()
    }

    fn group_header(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let ListItem::Header {
            key,
            label,
            count,
            collapsed,
        } = &self.items[ix]
        else {
            return div().into_any_element();
        };
        let key = key.clone();
        div()
            .id(SharedString::from(format!("thr-hdr-{}", group_slug(&key))))
            .w_full()
            .h(px(32.0))
            .px(px(18.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_xs))
            .font_weight(FontWeight::MEDIUM)
            .text_color(c.muted)
            .hover(move |s| s.text_color(c.text))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_group(&key, cx)))
            .child(SharedString::from(label.clone()))
            .child(div().text_color(c.outline).child(count.to_string()))
            .child(icon(
                if *collapsed {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                },
                12.0,
                c.muted,
            ))
            .into_any_element()
    }

    fn is_renaming(&self, id: ThreadId) -> bool {
        self.renaming.as_ref().is_some_and(|r| r.id == id)
    }

    /// The pointer entered / left a thread row. Repaints only on a change; a late "left" of the
    /// previous row never clears the next one.
    fn set_hovered(&mut self, id: ThreadId, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(id)
        } else if self.hovered == Some(id) {
            None
        } else {
            self.hovered
        };
        if next != self.hovered {
            self.hovered = next;
            cx.notify();
        }
    }

    fn thread_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let ListItem::Thread(rec) = &self.items[ix] else {
            return div().into_any_element();
        };
        let rec = rec.clone();
        let id = rec.id;
        let active = self.hub.read(cx).active_thread() == Some(id);
        let hovered = self.hovered == Some(id);
        let with_project =
            self.mode == GroupMode::Status && self.width_class == WidthClass::Regular;
        let elapsed = format_elapsed(((now_ms() - rec.updated_ms).max(0) / 1000) as u64);
        let glyph: Option<AnyElement> = match rec.status {
            ThreadStatus::Working => {
                Some(StatusGlyph::new(&t, ui_kit::RowStatus::Working).into_any_element())
            }
            ThreadStatus::WaitingForInput => {
                Some(icon(Icon::Dots, 16.0, t.colors.warning).into_any_element())
            }
            ThreadStatus::Done => {
                Some(StatusGlyph::new(&t, ui_kit::RowStatus::Done).into_any_element())
            }
            ThreadStatus::Error => {
                Some(StatusGlyph::new(&t, ui_kit::RowStatus::Error).into_any_element())
            }
            ThreadStatus::Idle => None,
        };
        let renaming = self
            .renaming
            .as_ref()
            .filter(|r| r.id == id)
            .map(|r| r.field.clone());
        let title_el: AnyElement = match renaming {
            Some(f) => div()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(Radius::CHIP))
                .bg(c.surface_3)
                .child(f)
                .into_any_element(),
            None => div()
                .w_full()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_md))
                .text_color(c.text)
                .child(SharedString::from(rec.display_title().to_string()))
                .into_any_element(),
        };
        let actions = div()
            .absolute()
            .right(px(0.0))
            .top(px(0.0))
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .child(
                IconButton::new(format!("thr-rename-{}", id.0), &t, Icon::Edit)
                    .passthrough(true)
                    .size(26.0)
                    .icon_size(14.0)
                    .tooltip("Renombrar")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.begin_rename(id, window, cx)
                    })),
            )
            .child(
                // TODO-SVG: `assets/icons/archive.svg` (16x16). Placeholder: the folder icon.
                IconButton::new(format!("thr-archive-{}", id.0), &t, Icon::Folder)
                    .passthrough(true)
                    .size(26.0)
                    .icon_size(14.0)
                    .tooltip("Archivar")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.hub.update(cx, |h, cx| h.archive_thread(id, cx))
                    })),
            )
            .child(
                IconButton::new(format!("thr-delete-{}", id.0), &t, Icon::Trash)
                    .passthrough(true)
                    .size(26.0)
                    .icon_size(14.0)
                    .tooltip("Eliminar…")
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.request_delete(id, cx)),
                    ),
            );
        let meta = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .when(hovered && !self.is_renaming(id), |d| d.invisible())
            .child(elapsed)
            .children(glyph);
        div()
            .w_full()
            .px(px(8.0))
            .py(px(2.0))
            .child(
                div()
                    .id(("thr", id.0 as usize))
                    .relative()
                    .occlude()
                    .w_full()
                    .h(px(40.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(Radius::PILL))
                    .cursor_pointer()
                    .font_family(ty.ui_family)
                    .when(active, |d| d.bg(c.surface_3))
                    .when(!active, |d| d.hover(move |s| s.bg(c.surface_2)))
                    .on_hover(
                        cx.listener(move |this, h: &bool, _, cx| this.set_hovered(id, *h, cx)),
                    )
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.open_thread(id, cx)),
                    )
                    .child(agent_icon(agent_mark(&rec.agent), 16.0, c.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(title_el)
                            .child(
                                div()
                                    .w_full()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(ty.ui_size_sm))
                                    .text_color(c.muted)
                                    .child(SharedString::from(subtitle(&rec, with_project))),
                            ),
                    )
                    .child(meta)
                    .children((hovered && !self.is_renaming(id)).then_some(actions)),
            )
            .into_any_element()
    }

    fn delete_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.delete.as_ref()?;
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let card = div()
            .id("delete-card")
            .occlude()
            .w_full()
            .max_w(px(380.0))
            .p(px(18.0))
            .rounded(px(Radius::MD))
            .bg(c.surface_2)
            .border_1()
            .border_color(c.outline_soft)
            .flex()
            .flex_col()
            .gap(px(12.0))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_md))
            .text_color(c.text);
        let card = match state {
            DeleteState::Loading(_) => card.child("Preparando la eliminación…"),
            DeleteState::Ready(d) => {
                let title = d.preview.thread.display_title().to_string();
                let lines = d.lines();
                let can = d.can_confirm();
                let (has_wt, remove, force, risky) =
                    (d.has_worktree(), d.remove_worktree, d.force, d.is_risky());
                let switch_row = |label: &str, hint: Option<&str>, toggle: Toggle| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(SharedString::from(label.to_string()))
                                .children(hint.map(|h| {
                                    div()
                                        .text_size(px(ty.ui_size_sm))
                                        .text_color(c.muted)
                                        .child(SharedString::from(h.to_string()))
                                })),
                        )
                        .child(toggle)
                };
                card.child(
                    div()
                        .text_size(px(ty.ui_size_lg))
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(format!("Eliminar «{title}»"))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .text_size(px(12.5))
                        .text_color(c.muted)
                        .children(
                            lines
                                .into_iter()
                                .map(|l| div().child(SharedString::from(l))),
                        ),
                )
                .when(has_wt, |d| {
                    d.child(switch_row(
                        "Eliminar también el worktree",
                        Some("La rama se conserva"),
                        Toggle::new("del-worktree", &t).on(remove).on_change(
                            cx.listener_for_toggle(|this, on, cx| this.set_remove_worktree(on, cx)),
                        ),
                    ))
                })
                .when(has_wt && remove && risky, |d| {
                    d.child(switch_row(
                        "Descartar los cambios sin guardar",
                        Some("Obligatorio para borrar un worktree con cambios"),
                        Toggle::new("del-force", &t).on(force).on_change(
                            cx.listener_for_toggle(|this, on, cx| this.set_force(on, cx)),
                        ),
                    ))
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(Button::tonal("del-cancel", &t, "Cancelar").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_delete(cx)),
                        ))
                        .child(
                            Button::primary(
                                "del-confirm",
                                &t,
                                if remove && has_wt {
                                    "Eliminar hilo y worktree"
                                } else {
                                    "Eliminar hilo"
                                },
                            )
                            .disabled(!can)
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_delete(cx)),
                            ),
                        ),
                )
            }
        };
        Some(
            div()
                .id("delete-scrim")
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.0))
                .bg(c.scrim)
                .occlude()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_delete(cx)))
                .child(card)
                .into_any_element(),
        )
    }
}

/// `Toggle::on_change` wants `Fn(bool, &mut Window, &mut App)`: bridge it to the view.
trait ListenerForToggle {
    fn listener_for_toggle(
        &self,
        f: impl Fn(&mut ThreadListView, bool, &mut Context<ThreadListView>) + 'static,
    ) -> impl Fn(bool, &mut Window, &mut App) + 'static;
}

impl ListenerForToggle for Context<'_, ThreadListView> {
    fn listener_for_toggle(
        &self,
        f: impl Fn(&mut ThreadListView, bool, &mut Context<ThreadListView>) + 'static,
    ) -> impl Fn(bool, &mut Window, &mut App) + 'static {
        let view = self.entity();
        move |on, _w, cx| {
            view.update(cx, |v, cx| f(v, on, cx));
        }
    }
}

impl Render for ThreadListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let error = self.hub.read(cx).last_error().map(str::to_string);
        let body: AnyElement = if self.items.is_empty() {
            div()
                .flex_1()
                .px(px(18.0))
                .pt(px(12.0))
                .text_size(px(ty.ui_size_md))
                .text_color(c.muted)
                .child(if self.hub.read(cx).is_loaded() {
                    "Todavía no hay hilos."
                } else {
                    "Cargando hilos…"
                })
                .into_any_element()
        } else {
            list(
                self.list.clone(),
                cx.processor(|this, ix: usize, window, cx| match this.items.get(ix) {
                    Some(ListItem::Header { .. }) => this.group_header(ix, cx),
                    Some(ListItem::Thread(_)) => this.thread_row(ix, window, cx),
                    None => div().into_any_element(),
                }),
            )
            .flex_1()
            .into_any_element()
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ty.ui_family)
            .when(self.show_header, |d| d.child(self.header_row(cx)))
            .children(error.map(|e| {
                div()
                    .id("threads-error")
                    .mx(px(12.0))
                    .mb(px(6.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(Radius::PILL))
                    .bg(c.surface_2)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.error)
                    .child(div().flex_1().min_w_0().child(SharedString::from(e)))
                    .child(
                        GhostButton::new("threads-error-x", &t, "Cerrar").on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.hub.update(cx, |h, cx| h.clear_error(cx));
                            },
                        )),
                    )
            }))
            .child(body)
            .children(self.delete_overlay(cx))
    }
}

/// `PanelProvider` `"threads"`: the list as a sidebar section or a panel of its own.
pub struct ThreadsPanelProvider {
    hub: Entity<ChatHub>,
}

impl ThreadsPanelProvider {
    pub fn new(hub: Entity<ChatHub>) -> Self {
        Self { hub }
    }
}

impl PanelProvider for ThreadsPanelProvider {
    fn id(&self) -> panel_api::PanelId {
        panel_api::PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Hilos".into()
    }

    fn icon(&self) -> Icon {
        Icon::Agents
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Sidebar, PanelZone::Right, PanelZone::Main]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let hub = self.hub.clone();
        let primary = ctx.zone != PanelZone::Sidebar;
        cx.new(|cx| {
            ThreadListView::new(hub, ctx.theme, ctx.width_class, cx).primary_action(primary)
        })
        .into()
    }

    fn context_changed(
        &self,
        view: &AnyView,
        ctx: &PanelCtx,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        match view.clone().downcast::<ThreadListView>() {
            Ok(v) => {
                v.update(cx, |v, cx| {
                    v.set_width_class(ctx.width_class, cx);
                    v.set_theme(ctx.theme, cx);
                });
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents_driver::{AgentKind, PermissionMode};
    use chat_core::{DeletePreview, WorktreePreview};

    fn rec(id: i64, status: ThreadStatus, project: &str, updated: i64) -> ThreadRecord {
        ThreadRecord {
            id: ThreadId(id),
            title: format!("hilo {id}"),
            project_root: PathBuf::from(project),
            worktree_path: None,
            agent: AgentKind::ClaudeCode,
            model: Some("opus".into()),
            effort: None,
            permission: PermissionMode::Ask,
            status,
            created_ms: 0,
            updated_ms: updated,
            agent_session_id: None,
            archived: false,
            checkpoint_before_turn: false,
            link: chat_core::ThreadLink::default(),
        }
    }

    fn sample() -> Vec<ThreadRecord> {
        vec![
            rec(1, ThreadStatus::Working, "C:\\p\\a", 50),
            rec(2, ThreadStatus::Done, "C:\\p\\a", 40),
            rec(3, ThreadStatus::WaitingForInput, "C:\\p\\b", 60),
            rec(4, ThreadStatus::Idle, "C:\\p\\b", 30),
            rec(5, ThreadStatus::Error, "C:\\p\\a", 20),
        ]
    }

    fn headers(items: &[ListItem]) -> Vec<(String, usize, bool)> {
        items
            .iter()
            .filter_map(|i| match i {
                ListItem::Header {
                    label,
                    count,
                    collapsed,
                    ..
                } => Some((label.clone(), *count, *collapsed)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn groups_by_status_with_counts() {
        let items = build_items(&sample(), GroupMode::Status, &HashSet::new());
        assert_eq!(
            headers(&items),
            vec![
                ("Working".into(), 1, false),
                ("Esperando".into(), 1, false),
                ("Settled".into(), 3, false)
            ]
        );
        // header, 1, header, 3, header, 2, 4, 5 (newest first inside Settled: 2 (40), 4 (30), 5 (20))
        let ids: Vec<Option<i64>> = items
            .iter()
            .map(|i| {
                if let ListItem::Thread(t) = i {
                    Some(t.id.0)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            ids,
            vec![
                None,
                Some(1),
                None,
                Some(3),
                None,
                Some(2),
                Some(4),
                Some(5)
            ]
        );
    }

    #[test]
    fn collapsing_a_group_keeps_only_its_header() {
        let mut c = HashSet::new();
        c.insert(GroupKey::Settled);
        let items = build_items(&sample(), GroupMode::Status, &c);
        assert_eq!(items.len(), 5);
        assert_eq!(
            headers(&items).last(),
            Some(&("Settled".to_string(), 3, true))
        );
    }

    #[test]
    fn empty_groups_have_no_header() {
        let only_done = vec![rec(1, ThreadStatus::Done, "C:\\p\\a", 1)];
        let items = build_items(&only_done, GroupMode::Status, &HashSet::new());
        assert_eq!(headers(&items), vec![("Settled".into(), 1, false)]);
        assert!(build_items(&[], GroupMode::Status, &HashSet::new()).is_empty());
    }

    #[test]
    fn groups_by_project_most_recent_first() {
        let items = build_items(&sample(), GroupMode::Project, &HashSet::new());
        // project b has the newest thread (60), then a
        assert_eq!(
            headers(&items),
            vec![("b".into(), 2, false), ("a".into(), 3, false)]
        );
        let first_thread = items.iter().find_map(|i| {
            if let ListItem::Thread(t) = i {
                Some(t.id.0)
            } else {
                None
            }
        });
        assert_eq!(first_thread, Some(3));
    }

    #[test]
    fn subtitles_say_what_matters() {
        let mut t = rec(1, ThreadStatus::Working, "C:\\p\\terminal", 0);
        assert_eq!(subtitle(&t, false), "Working · Opus 5.5");
        assert_eq!(subtitle(&t, true), "Working · terminal · Opus 5.5");
        t.effort = Some("high".into());
        assert_eq!(subtitle(&t, false), "Working · Opus 5.5 · Alto");
        t.effort = None;
        t.status = ThreadStatus::Done;
        t.model = None;
        t.worktree_path = Some(PathBuf::from("C:\\p\\terminal-wt"));
        assert_eq!(subtitle(&t, false), "Claude Code · terminal-wt");
    }

    fn preview(wt: Option<WorktreePreview>) -> DeletePreview {
        DeletePreview {
            thread: rec(9, ThreadStatus::Done, "C:\\p\\a", 0),
            message_count: 3,
            worktree: wt,
        }
    }

    fn worktree(dirty: Option<bool>, requires: bool) -> WorktreePreview {
        WorktreePreview {
            path: PathBuf::from("C:\\p\\a-wt"),
            exists: true,
            branch: Some("selachi/x".into()),
            dirty,
            warnings: if requires {
                vec!["2 archivos sin guardar".into()]
            } else {
                vec![]
            },
            diff_stat: Some(" a.rs | 2 +-".into()),
            requires_confirmation: requires,
        }
    }

    #[test]
    fn deleting_a_plain_thread_needs_no_extra_ack() {
        let d = DeleteDialog::new(preview(None));
        assert!(d.can_confirm());
        assert_eq!(d.options(), DeleteOptions::default());
        assert!(d.lines()[0].contains("3 mensajes"));
    }

    #[test]
    fn worktree_is_kept_unless_asked_and_dirty_ones_need_force() {
        let mut d = DeleteDialog::new(preview(Some(worktree(Some(true), true))));
        // default: only the thread goes
        assert!(d.can_confirm());
        assert_eq!(
            d.options(),
            DeleteOptions {
                remove_worktree: false,
                force: false
            }
        );
        // asking for the dirty worktree to go is blocked until the user acknowledges
        d.remove_worktree = true;
        assert!(d.needs_force());
        assert!(!d.can_confirm(), "dirty worktree removal must be gated");
        d.force = true;
        assert!(d.can_confirm());
        assert_eq!(
            d.options(),
            DeleteOptions {
                remove_worktree: true,
                force: true
            }
        );
        let text = d.lines().join("\n");
        assert!(
            text.contains("cambios sin guardar")
                && text.contains("selachi/x")
                && text.contains("2 archivos"),
            "{text}"
        );
    }

    #[test]
    fn clean_worktree_can_be_removed_without_force() {
        let mut d = DeleteDialog::new(preview(Some(worktree(Some(false), false))));
        d.remove_worktree = true;
        assert!(!d.needs_force());
        assert!(d.can_confirm());
        assert_eq!(
            d.options(),
            DeleteOptions {
                remove_worktree: true,
                force: false
            }
        );
    }

    #[test]
    fn missing_worktree_directory_is_never_removed() {
        let mut wt = worktree(None, false);
        wt.exists = false;
        let mut d = DeleteDialog::new(preview(Some(wt)));
        d.remove_worktree = true;
        assert!(!d.has_worktree());
        assert_eq!(d.options(), DeleteOptions::default());
        assert!(d.lines().iter().any(|l| l.contains("ya no existe")));
    }
}
