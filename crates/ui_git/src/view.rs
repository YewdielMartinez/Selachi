//! The git panel: header (branch, worktrees, sync), Changes with inline diff, commit box,
//! history and checkpoints. It adapts to [`WidthClass`] and the measured width, never to its zone:
//!
//! * `Compact` (< 320 px): one column with tabs (Cambios | Historial | Checkpoints).
//! * `Regular`: one scrolling column with every section; the commit box stays pinned below it.
//! * Regular and at least [`WIDE_FROM_PX`] wide: two columns (changes + commit | history +
//!   checkpoints).

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::SystemTime;

use git_svc::{DeltaKind, GitAction, Worktree};
use gpui::{
    AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, Entity, Focusable,
    FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, canvas, div, hsla, prelude::*, px,
};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::Theme;
use ui_kit::{
    ActionSize, Button, GhostButton, ListRow, Pill, Placement, Popover,
    Tooltip, VirtualList, WireFill, WireRect, Wireframe, icon,
};

use crate::cmd::{GitOp, Slot, commit_message};
use crate::gate::Gate;
use crate::data::{DiffTarget, GitData, same_path, worktree_label};
use crate::fmt::{plural, relative_time, truncate_end, truncate_middle, unix_secs};
use crate::input::{TextField, TextFieldEvent};
use crate::model::{CheckpointFiles, GitModel, Load, OpFinished};
use crate::sections::{FileRow, Section, build_sections, total_changes};
use crate::widgets::{
    diff_card, empty_state, file_row, gate_card, mini_button, quiet_error_card, section_header, tag,
    working_target,
};

/// Panel id stored in `layout.right_panel`.
pub const PANEL_ID: &str = "git";

/// Measured width from which the Regular layout uses two columns.
pub const WIDE_FROM_PX: f32 = 760.0;
/// Rows of one section painted before "Mostrar todos".
const SECTION_LIMIT: usize = 100;
/// History rows visible at once when the history is not the whole tab.
const HISTORY_ROWS: usize = 8;

/// Registers as `git`.
pub struct GitPanelProvider {
    model: Entity<GitModel>,
}

impl GitPanelProvider {
    /// A provider over the app's [`GitModel`]. Keep a clone of the entity to subscribe to
    /// [`crate::GitPanelEvent`] before moving the provider into the registry.
    pub fn new(model: Entity<GitModel>) -> Self {
        Self { model }
    }
}

fn root_of(ctx: &PanelCtx) -> Option<std::path::PathBuf> {
    ctx.project_root.clone().or_else(|| ctx.cwd.clone())
}

impl PanelProvider for GitPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Git".into()
    }

    fn icon(&self) -> Icon {
        Icon::GitBranch
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Right, PanelZone::Sidebar, PanelZone::Main]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let model = self.model.clone();
        model.update(cx, |m, cx| m.set_root(root_of(&ctx), cx));
        cx.new(|cx| GitPanel::new(ctx, model, cx)).into()
    }

    fn context_changed(&self, view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        let Ok(panel) = view.clone().downcast::<GitPanel>() else { return false };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

/// 1-bit thumbnail for the layout panel: branch pill, grouped file rows and the commit box.
pub fn thumbnail(theme: &Theme) -> Wireframe {
    use WireFill::{Dither, Outline, Solid};
    let r = WireRect::new;
    Wireframe::new(theme).rects([
        r(1, 1, 10, 2, Solid),
        r(12, 1, 4, 2, Dither),
        r(22, 1, 9, 2, Outline),
        r(1, 5, 8, 1, Solid),
        r(2, 7, 3, 1, Solid),
        r(6, 7, 17, 1, Dither),
        r(2, 9, 3, 1, Solid),
        r(6, 9, 13, 1, Dither),
        r(1, 12, 11, 1, Solid),
        r(2, 14, 3, 1, Solid),
        r(6, 14, 19, 1, Dither),
        r(1, 16, 30, 3, Outline),
        r(22, 17, 8, 1, Solid),
    ])
}

// ---------------------------------------------------------------------------------------------
// Pure layout decisions
// ---------------------------------------------------------------------------------------------

/// How the panel arranges its sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Compact: one column, one section at a time behind tabs.
    Tabs,
    /// One scrolling column with every section.
    Stacked,
    /// Two columns.
    Columns,
}

/// The layout for a width class and the measured width of the panel.
pub fn layout_for(class: WidthClass, width_px: f32) -> Layout {
    match class {
        WidthClass::Compact => Layout::Tabs,
        WidthClass::Regular if width_px >= WIDE_FROM_PX => Layout::Columns,
        WidthClass::Regular => Layout::Stacked,
    }
}

/// The compact tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Changes,
    History,
    Checkpoints,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Changes, Tab::History, Tab::Checkpoints];

    pub const fn label(self) -> &'static str {
        match self {
            Tab::Changes => "Cambios",
            Tab::History => "Historial",
            Tab::Checkpoints => "Checkpoints",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// Characters a path may use on a file row in a column `width_px` wide.
pub fn path_budget(width_px: f32, compact: bool) -> usize {
    let overhead = 24.0 /* padding */ + 26.0 /* letter */ + 28.0 /* stage button */ + 14.0
        + if compact { 0.0 } else { 62.0 /* +n -m */ };
    (((width_px - overhead) / 6.6).floor() as i64).clamp(10, 120) as usize
}

// ---------------------------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------------------------

pub struct GitPanel {
    ctx: PanelCtx,
    model: Entity<GitModel>,
    subject: Entity<TextField>,
    body: Entity<TextField>,
    wt_name: Entity<TextField>,
    cp_label: Entity<TextField>,
    tab: Tab,
    collapsed: HashSet<&'static str>,
    show_all: HashSet<Section>,
    wt_open: bool,
    wt_new: bool,
    cp_new: bool,
    /// Measured width of the panel (logical px).
    width: f32,
    probe: Rc<Cell<f32>>,
    _subs: Vec<Subscription>,
}

impl GitPanel {
    pub fn new(ctx: PanelCtx, model: Entity<GitModel>, cx: &mut Context<Self>) -> Self {
        model.update(cx, |m, _| m.attach_view());
        let theme = ctx.theme;
        let subject = cx.new(|cx| TextField::new(theme, "Mensaje del commit", cx));
        let body = cx.new(|cx| TextField::new(theme, "Descripción (opcional)", cx));
        let wt_name = cx.new(|cx| TextField::new(theme, "nombre del worktree", cx));
        let cp_label = cx.new(|cx| TextField::new(theme, "etiqueta (opcional)", cx));

        let mut subs = vec![cx.observe(&model, |_, _, cx| cx.notify())];
        subs.push(cx.subscribe(&subject, |this, _, ev: &TextFieldEvent, cx| match ev {
            TextFieldEvent::Changed => cx.notify(),
            TextFieldEvent::Submit => this.begin_commit(false, cx),
            TextFieldEvent::Cancel => {}
        }));
        subs.push(cx.subscribe(&body, |_, _, ev: &TextFieldEvent, cx| {
            if matches!(ev, TextFieldEvent::Changed) {
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&wt_name, |this, _, ev: &TextFieldEvent, cx| match ev {
            TextFieldEvent::Changed => cx.notify(),
            TextFieldEvent::Submit => this.create_worktree(cx),
            TextFieldEvent::Cancel => {
                this.wt_new = false;
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&cp_label, |this, _, ev: &TextFieldEvent, cx| match ev {
            TextFieldEvent::Changed => {}
            TextFieldEvent::Submit => this.create_checkpoint(cx),
            TextFieldEvent::Cancel => {
                this.cp_new = false;
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&model, |this, _, ev: &OpFinished, cx| this.op_finished(ev, cx)));
        let for_release = model.clone();
        subs.push(cx.on_release(move |_, cx| for_release.update(cx, |m, cx| m.detach_view(cx))));

        let width = if ctx.width_class == WidthClass::Compact { 260.0 } else { 420.0 };
        Self {
            ctx,
            model,
            subject,
            body,
            wt_name,
            cp_label,
            tab: Tab::Changes,
            collapsed: HashSet::new(),
            show_all: HashSet::new(),
            wt_open: false,
            wt_new: false,
            cp_new: false,
            width,
            probe: Rc::new(Cell::new(width)),
            _subs: subs,
        }
    }

    /// The shell moved / resized the panel or the active tab changed directory.
    pub fn retarget(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        let root = root_of(&ctx);
        if self.ctx.theme != ctx.theme {
            for f in [&self.subject, &self.body, &self.wt_name, &self.cp_label] {
                let theme = ctx.theme;
                f.update(cx, |f, cx| {
                    f.set_theme(theme);
                    cx.notify();
                });
            }
        }
        self.ctx = ctx;
        self.model.update(cx, |m, cx| m.set_root(root, cx));
        cx.notify();
    }

    pub fn is_compact(&self) -> bool {
        self.ctx.width_class == WidthClass::Compact
    }

    /// The arrangement the next frame uses.
    pub fn layout(&self) -> Layout {
        layout_for(self.ctx.width_class, self.width)
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// Opens or closes the worktree switcher (the host's demo / tests drive it).
    pub fn set_worktree_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.wt_open != open {
            self.wt_open = open;
            if open {
                self.model.update(cx, |m, cx| m.refresh_worktrees(cx));
            }
            cx.notify();
        }
    }

    pub fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    /// The commit message typed so far (subject plus optional body).
    pub fn message(&self, cx: &App) -> String {
        commit_message(self.subject.read(cx).text(), self.body.read(cx).text())
    }

    pub fn set_message(&mut self, subject: &str, body: &str, cx: &mut Context<Self>) {
        self.subject.update(cx, |f, cx| f.set_text(subject, cx));
        self.body.update(cx, |f, cx| f.set_text(body, cx));
    }

    fn toggle(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key);
        }
        cx.notify();
    }

    fn begin_commit(&mut self, push: bool, cx: &mut Context<Self>) {
        let subject = self.subject.read(cx).text().to_string();
        if subject.trim().is_empty() {
            return;
        }
        let message = commit_message(&subject, self.body.read(cx).text());
        let op = if push {
            GitOp::CommitAndPush { message }
        } else {
            GitOp::Action(GitAction::Commit { message })
        };
        self.model.update(cx, |m, cx| m.request(op, cx));
    }

    fn create_worktree(&mut self, cx: &mut Context<Self>) {
        let name = self.wt_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.model.update(cx, |m, cx| m.request(GitOp::CreateWorktree { name }, cx));
    }

    fn create_checkpoint(&mut self, cx: &mut Context<Self>) {
        let label = self.cp_label.read(cx).text().trim().to_string();
        self.model.update(cx, |m, cx| m.request(GitOp::CreateCheckpoint { label }, cx));
    }

    fn op_finished(&mut self, ev: &OpFinished, cx: &mut Context<Self>) {
        if !ev.ok {
            return;
        }
        match &ev.op {
            GitOp::Action(GitAction::Commit { .. }) | GitOp::CommitAndPush { .. } => self.set_message("", "", cx),
            GitOp::CreateWorktree { .. } => {
                self.wt_name.update(cx, |f, cx| f.set_text("", cx));
                self.wt_new = false;
            }
            GitOp::CreateCheckpoint { .. } => {
                self.cp_label.update(cx, |f, cx| f.set_text("", cx));
                self.cp_new = false;
            }
            _ => {}
        }
        cx.notify();
    }

    fn act(&self, op: GitOp) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static + use<> {
        let model = self.model.clone();
        move |_, _, cx| {
            model.update(cx, |m, cx| m.request(op.clone(), cx));
        }
    }

    fn input_box(&self, field: &Entity<TextField>, window: &Window, cx: &App) -> gpui::Div {
        let c = self.ctx.theme.colors;
        let focused = field.read(cx).focus_handle(cx).is_focused(window);
        div()
            .w_full()
            .h(px(32.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded(px(10.0))
            .bg(c.surface_3)
            .border_1()
            .border_color(if focused { c.accent.into() } else { hsla(0.0, 0.0, 0.0, 0.0) })
            .child(field.clone())
    }
}

/// Binds the text field's keys once (idempotent). Call it at startup, before the first panel.
pub fn init(cx: &mut App) {
    struct Bound;
    impl gpui::Global for Bound {}
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    crate::input::bind_keys(cx);
}

// ---------------------------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------------------------

impl GitPanel {
    fn header(&mut self, data: Option<&GitData>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let Some(data) = data else {
            return div().h(px(28.0)).into_any_element();
        };
        let st = &data.status;
        let branch = match (&st.branch, st.detached) {
            (Some(b), _) => truncate_middle(b, if compact { 22 } else { 36 }),
            (None, true) => format!("HEAD separado {}", st.head_oid.as_deref().map_or("", |h| &h[..h.len().min(7)])),
            (None, false) => "sin commits".to_string(),
        };
        let dirty = st.is_dirty() || !st.untracked.is_empty();
        let mut pill = div()
            .h(px(28.0))
            .min_w_0()
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(10.0))
            .bg(c.surface_2)
            .child(icon(Icon::GitBranch, 16.0, c.muted))
            .child(
                div()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_size(px(theme.typography.ui_size_md))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.text)
                    .child(branch),
            );
        if let (Some(up), false) = (&st.upstream, compact) {
            pill = pill.child(
                div()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.ui_size_sm))
                    .text_color(c.muted)
                    .child(truncate_middle(up, 22)),
            );
        }
        if st.ahead > 0 || st.behind > 0 {
            let mut counts = String::new();
            if st.ahead > 0 {
                counts.push_str(&format!("↑{}", st.ahead));
            }
            if st.behind > 0 {
                if !counts.is_empty() {
                    counts.push(' ');
                }
                counts.push_str(&format!("↓{}", st.behind));
            }
            pill = pill.child(div().text_size(px(11.0)).text_color(c.muted).child(counts));
        }
        if dirty {
            pill = pill.child(
                div()
                    .id("git-dirty")
                    .size(px(7.0))
                    .flex_none()
                    .rounded_full()
                    .bg(c.accent)
                    .tooltip(Tooltip::text(&theme, "Hay cambios sin confirmar")),
            );
        }
        let _ = window;
        let this = cx.entity();
        let wt_name = data
            .worktree_at(self.model.read(cx).root().unwrap_or(std::path::Path::new("")))
            .map(|w| worktree_label(w).0)
            .unwrap_or_else(|| "worktree".to_string());
        let trigger = Pill::new("git-wt-trigger", &theme)
            .icon(Icon::Folder)
            .when(!compact, |p| p.label(truncate_middle(&wt_name, 22)))
            .active(self.wt_open)
            .tooltip(format!("Worktree: {wt_name}"))
            .child(icon(Icon::ChevronDown, 16.0, c.muted))
            .on_click({
                let this = this.clone();
                let model = self.model.clone();
                move |_, _, cx| {
                    this.update(cx, |p, cx| {
                        p.wt_open = !p.wt_open;
                        if p.wt_open {
                            model.update(cx, |m, cx| m.refresh_worktrees(cx));
                        }
                        cx.notify();
                    });
                }
            });
        let menu = self.worktree_menu(data, cx);
        let popover = Popover::new("git-wt-pop", &theme)
            .open(self.wt_open)
            .placement(Placement::BottomEnd)
            .width(if compact { 260.0 } else { 310.0 })
            .on_dismiss({
                move |_, cx| {
                    this.update(cx, |p, cx| {
                        p.wt_open = false;
                        cx.notify();
                    });
                }
            })
            .trigger(trigger)
            .child(menu);

        let busy = self.model.read(cx).gate().is_busy();
        let upstream = st.upstream.is_some();
        let pull_label = if st.behind > 0 { format!("Pull ↓{}", st.behind) } else { "Pull".to_string() };
        let (push_label, push_op) = match (&st.branch, upstream) {
            (Some(b), false) => ("Publicar".to_string(), GitOp::PublishBranch { branch: b.clone() }),
            (_, _) if st.ahead > 0 => (format!("Push ↑{}", st.ahead), GitOp::Action(GitAction::Push)),
            _ => ("Push".to_string(), GitOp::Action(GitAction::Push)),
        };
        let sync = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.0))
            .child(Button::tonal("git-fetch", &theme, "Fetch").disabled(busy).on_click(self.act(GitOp::Action(GitAction::Fetch))))
            .child(Button::tonal("git-pull", &theme, pull_label).disabled(busy).on_click(self.act(GitOp::Action(GitAction::Pull))))
            .child(Button::tonal("git-push", &theme, push_label).disabled(busy).on_click(self.act(push_op)));

        let mut col = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(div().min_w_0().flex_shrink().child(pill))
                    .child(popover),
            )
            .child(sync);
        let gate = self.model.read(cx).gate().clone();
        if let Some(card) = gate_card(&theme, &gate, Slot::Header, &self.model) {
            col = col.child(card);
        }
        if let Some(e) = self.model.read(cx).quiet_error() {
            col = col.child(quiet_error_card(&theme, e, &self.model));
        }
        col.into_any_element()
    }

    fn worktree_menu(&mut self, data: &GitData, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let current = self.model.read(cx).root().map(std::path::Path::to_path_buf);
        let this = cx.entity();
        let mut col = div().w_full().flex().flex_col().gap(px(2.0)).font_family(theme.typography.ui_family);
        for (i, w) in data.worktrees.iter().enumerate() {
            let is_current = current.as_deref().is_some_and(|r| same_path(r, &w.path));
            col = col.child(self.worktree_row(i, w, is_current, &this));
        }
        let toggle = {
            let this = this.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                this.update(cx, |p, cx| {
                    p.wt_new = !p.wt_new;
                    cx.notify();
                });
            }
        };
        col = col.child(
            div().pt(px(6.0)).child(GhostButton::new("git-wt-new", &theme, "Nuevo worktree…").icon(Icon::Plus).on_click(toggle)),
        );
        if self.wt_new {
            let name = self.wt_name.read(cx).text().to_string();
            let problem = if name.trim().is_empty() { None } else { git_svc::validate_worktree_name(name.trim()).err().map(|e| e.to_string()) };
            let ready = !name.trim().is_empty() && problem.is_none();
            let model = self.model.clone();
            let this2 = this;
            col = col
                .child(
                    div()
                        .w_full()
                        .h(px(32.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .rounded(px(10.0))
                        .bg(c.surface_3)
                        .child(self.wt_name.clone()),
                )
                .when_some(problem, |d, p| d.child(div().px(px(4.0)).text_size(px(11.0)).text_color(c.error).child(p)))
                .child(
                    div().pt(px(4.0)).flex().flex_row().gap(px(6.0)).child(
                        Button::tonal("git-wt-create", &theme, "Crear")
                            .disabled(!ready)
                            .tooltip("Crea la rama selachi/<nombre> y su carpeta junto al repositorio")
                            .on_click(move |_, _, cx| {
                                let name = this2.read(cx).wt_name.read(cx).text().trim().to_string();
                                model.update(cx, |m, cx| m.request(GitOp::CreateWorktree { name }, cx));
                            }),
                    ),
                );
        }
        col.into_any_element()
    }

    fn worktree_row(&self, i: usize, w: &Worktree, is_current: bool, this: &Entity<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let (name, branch) = worktree_label(w);
        let path = w.path.clone();
        let model = self.model.clone();
        let pick = {
            let (model, this, path) = (model.clone(), this.clone(), path.clone());
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                if is_current {
                    return;
                }
                this.update(cx, |p, cx| {
                    p.wt_open = false;
                    cx.notify();
                });
                model.update(cx, |m, cx| m.switch_worktree(path.clone(), cx));
            }
        };
        let dirty = w.dirty == Some(true);
        let main = div()
            .id(SharedString::from(format!("git-wt-{i}")))
            .occlude()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .when(!is_current, |d| d.cursor_pointer())
            .child(div().size(px(7.0)).flex_none().rounded_full().when(dirty, |d| d.bg(c.accent)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().overflow_hidden().whitespace_nowrap().text_size(px(theme.typography.ui_size_md)).text_color(c.text).child(truncate_middle(&name, 26)))
                    .child(div().overflow_hidden().whitespace_nowrap().text_size(px(11.0)).text_color(c.muted).child(truncate_middle(&branch, 30))),
            )
            .when(is_current, |d| d.child(tag(&theme, "actual")))
            .on_click(pick);
        let (m_edit, m_rm) = (model.clone(), model);
        let (p_edit, p_rm) = (path.clone(), path);
        let removable = !w.is_main && !is_current;
        let force = dirty;
        div()
            .w_full()
            .h(px(40.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .rounded(px(8.0))
            .when(is_current, |d| d.bg(c.surface_3))
            .when(!is_current, |d| d.hover(move |s| s.bg(c.surface_2)))
            .child(main)
            .child(GhostButton::new(SharedString::from(format!("git-wt-edit-{i}")), &theme, "Editor").tooltip("Abrir en el editor").on_click(
                move |_, _, cx| {
                    m_edit.update(cx, |m, cx| m.open_in_editor(&p_edit, cx));
                },
            ))
            .when(removable, |d| {
                d.child(mini_button(format!("git-wt-rm-{i}"), &theme, "×", "Eliminar worktree…", move |_, _, cx| {
                    let op = GitOp::RemoveWorktree { path: p_rm.clone(), force };
                    m_rm.update(cx, |m, cx| m.request(op, cx));
                }))
            })
            .into_any_element()
    }

    /// The four change groups with their file rows and open diff.
    fn changes_block(&mut self, data: &GitData, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let compact = self.is_compact();
        let col_width = if self.layout() == Layout::Columns { self.width * 0.5 } else { self.width };
        let budget = path_budget(col_width, compact);
        let sections = build_sections(&data.status, &data.counts);
        let mut out: Vec<AnyElement> = Vec::new();
        if sections.is_empty() {
            out.push(empty_state(&theme, "Sin cambios", "El árbol de trabajo está limpio."));
            return out;
        }
        let unborn = data.unborn();
        let (diff, gate_busy) = {
            let m = self.model.read(cx);
            (m.diff().cloned(), m.gate().is_busy())
        };
        let _ = gate_busy;
        let stage_all_at = sections.iter().find(|(s, _)| matches!(s, Section::Changes | Section::Untracked)).map(|(s, _)| *s);
        for (section, rows) in &sections {
            let key = section_key(*section);
            let collapsed = self.collapsed.contains(key);
            let action: Option<(&'static str, crate::widgets::OnClick)> = match section {
                Section::Staged => Some(("Quitar todo", Box::new(self.act(GitOp::UnstageAll { unborn })))),
                s if Some(*s) == stage_all_at => Some(("Preparar todo", Box::new(self.act(GitOp::StageAll)))),
                _ => None,
            };
            out.push(
                section_header(
                    key,
                    &theme,
                    section.title(),
                    Some(rows.len()),
                    collapsed,
                    cx.listener(move |this, _, _, cx| this.toggle(key, cx)),
                    action,
                )
                .into_any_element(),
            );
            if collapsed {
                continue;
            }
            let all = self.show_all.contains(section);
            let limit = if all { rows.len() } else { rows.len().min(SECTION_LIMIT) };
            for (i, row) in rows.iter().take(limit).enumerate() {
                let target = working_target(row);
                let selected = diff.as_ref().is_some_and(|d| d.target == target);
                let (btn, op) = if row.section == Section::Staged {
                    ("−", GitOp::Unstage { path: row.path.clone(), unborn })
                } else {
                    ("+", GitOp::Stage(row.path.clone()))
                };
                let tip = if row.section == Section::Staged { "Quitar de staged" } else { "Preparar (stage)" };
                let action = mini_button(format!("{key}-act-{i}"), &theme, btn, tip, self.act(op)).into_any_element();
                let model = self.model.clone();
                let t = target.clone();
                out.push(
                    file_row(
                        &theme,
                        SharedString::from(format!("{key}-row-{i}")),
                        row,
                        selected,
                        compact,
                        budget,
                        move |_, _, cx| model.update(cx, |m, cx| m.toggle_diff(t.clone(), cx)),
                        Some(action),
                    )
                    .into_any_element(),
                );
                if selected && let Some(slot) = &diff {
                    out.push(diff_card(&theme, slot, compact, &self.model));
                }
            }
            if rows.len() > limit {
                let sec = *section;
                out.push(
                    div()
                        .pl(px(6.0))
                        .flex_none()
                        .child(
                            GhostButton::new(SharedString::from(format!("{key}-all")), &theme, format!("Mostrar los {} archivos", rows.len()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.show_all.insert(sec);
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        out
    }

    fn commit_block(&mut self, data: &GitData, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let staged = data.status.staged.len();
        let subject = self.subject.read(cx).text().trim().to_string();
        let busy = self.model.read(cx).gate().is_busy();
        let can = !subject.is_empty() && staged > 0 && !busy;
        let gate = self.model.read(cx).gate().clone();
        // The optional description appears as soon as there is a subject (or already has text).
        let show_body = !subject.is_empty() || !self.body.read(cx).text().is_empty();
        let mut card = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(12.0))
            .bg(c.surface_2)
            .child(self.input_box(&self.subject, window, cx))
            .when(show_body, |d| d.child(self.input_box(&self.body, window, cx)));
        if let Some(review) = gate_card(&theme, &gate, Slot::Commit, &self.model) {
            card = card.child(review);
        }
        let card_active = matches!(
            &gate,
            Gate::Preflighting { op, .. } | Gate::Confirm { op, .. } | Gate::Running { op } if op.slot() == Slot::Commit
        );
        let hint = if staged == 0 && total_changes(&data.status) > 0 {
            Some("Nada en staged: usa + en un archivo o «Preparar todo»".to_string())
        } else if staged > 0 {
            Some(format!("{} en staged", plural(staged, "archivo", "archivos")))
        } else {
            None
        };
        let commit = Button::action("git-commit", &theme, "Commit")
            .icon(Icon::Check)
            .size(ActionSize::Md)
            .disabled(!can)
            .tooltip(if staged == 0 { "Prepara archivos primero" } else { "Revisa y confirma el commit" })
            .on_click(cx.listener(|this, _, _, cx| this.begin_commit(false, cx)));
        let push = Button::tonal("git-commit-push", &theme, if self.is_compact() { "+ Push" } else { "Commit y push" })
            .tooltip("Commit y push")
            .disabled(!can)
            .on_click(cx.listener(|this, _, _, cx| this.begin_commit(true, cx)));
        if card_active {
            return card.into_any_element();
        }
        card = card
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(commit)
                    .child(push),
            )
            .when_some(hint, |d, h| d.child(div().text_size(px(11.0)).text_color(c.muted).child(h)));
        card.into_any_element()
    }

    fn history_block(&mut self, data: &GitData, fill: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let mut out: Vec<AnyElement> = Vec::new();
        let key = "history";
        let collapsed = self.collapsed.contains(key) && !fill;
        if !fill {
            out.push(
                section_header(key, &theme, "Historial", Some(data.commits.len()), collapsed, cx.listener(move |this, _, _, cx| this.toggle(key, cx)), None)
                    .into_any_element(),
            );
        }
        if collapsed {
            return out;
        }
        if data.commits.is_empty() {
            out.push(empty_state(&theme, "Sin commits", "Aún no hay historial en esta rama."));
            return out;
        }
        let commits = data.commits.clone();
        let now = unix_secs(SystemTime::now());
        let budget = (((self.width - 90.0) / 6.6) as usize).clamp(14, 90);
        let t = theme;
        let list = VirtualList::new("git-history", commits.len(), ListRow::HEIGHT, move |range, _, _| {
            range
                .filter_map(|i| commits.get(i))
                .map(|cm| {
                    let sub = format!("{} · {}", cm.short_hash, truncate_middle(&cm.author, 18));
                    div()
                        .id(SharedString::from(format!("commit-{}", cm.short_hash)))
                        .child(
                            ListRow::new(SharedString::from(format!("commit-row-{}", cm.short_hash)), &t)
                                .title(truncate_end(&cm.subject, budget))
                                .subtitle(sub)
                                .meta(relative_time(cm.timestamp, now)),
                        )
                        .tooltip(Tooltip::text(&t, format!("{}\n{}", cm.hash, cm.subject)))
                        .into_any_element()
                })
                .collect()
        })
        .w_full();
        let _ = (c, compact);
        let list = if fill {
            list.flex_1().min_h(px(120.0))
        } else {
            list.flex_none().h(px(data.commits.len().min(HISTORY_ROWS) as f32 * ListRow::HEIGHT))
        };
        out.push(list.into_any_element());
        out
    }

    fn checkpoints_block(&mut self, data: &GitData, with_header: bool, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let mut out: Vec<AnyElement> = Vec::new();
        let key = "checkpoints";
        let collapsed = self.collapsed.contains(key) && with_header;
        let create = GhostButton::new("git-cp-new", &theme, "Crear checkpoint")
            .tooltip("Guarda el estado actual para poder volver")
            .on_click(cx.listener(|this, _, _, cx| {
                this.cp_new = !this.cp_new;
                cx.notify();
            }));
        if with_header {
            out.push(
                div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().flex_1().min_w_0().child(section_header(
                        key,
                        &theme,
                        "Checkpoints",
                        Some(data.checkpoints.len()),
                        collapsed,
                        cx.listener(move |this, _, _, cx| this.toggle(key, cx)),
                        None,
                    )))
                    .child(create)
                    .into_any_element(),
            );
        } else {
            out.push(div().w_full().flex_none().flex().flex_row().justify_end().child(create).into_any_element());
        }
        if collapsed {
            return out;
        }
        if self.cp_new {
            let model = self.model.clone();
            let this = cx.entity();
            out.push(
                div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().flex_1().min_w_0().child(self.input_box(&self.cp_label, window, cx)))
                    .child(Button::tonal("git-cp-create", &theme, "Crear").on_click(move |_, _, cx| {
                        let label = this.read(cx).cp_label.read(cx).text().trim().to_string();
                        model.update(cx, |m, cx| m.request(GitOp::CreateCheckpoint { label }, cx));
                    }))
                    .into_any_element(),
            );
        }
        let gate = self.model.read(cx).gate().clone();
        if let Some(card) = gate_card(&theme, &gate, Slot::Checkpoints, &self.model) {
            out.push(card);
        }
        if data.checkpoints.is_empty() {
            out.push(empty_state(&theme, "Sin checkpoints", "Crea uno antes de dejar trabajar a un agente."));
            return out;
        }
        let now = unix_secs(SystemTime::now());
        let (view, diff) = {
            let m = self.model.read(cx);
            (m.checkpoint_view().cloned(), m.diff().cloned())
        };
        let busy = self.model.read(cx).gate().is_busy();
        for (i, cp) in data.checkpoints.iter().enumerate() {
            let open = view.as_ref().is_some_and(|v| v.id == cp.id);
            let (m_view, m_restore) = (self.model.clone(), self.model.clone());
            let (id_view, id_restore) = (cp.id.clone(), cp.id.clone());
            let label = truncate_middle(&cp.label, if compact { 24 } else { 40 });
            let mut card = div()
                .w_full()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(8.0))
                .rounded(px(10.0))
                .bg(c.surface_2)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_size(px(theme.typography.ui_size_md))
                                .text_color(c.text)
                                .child(label),
                        )
                        .when(cp.auto, |d| d.child(tag(&theme, "auto")))
                        .child(div().flex_none().text_size(px(11.0)).text_color(c.muted).child(relative_time(unix_secs(cp.created), now))),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(2.0))
                        .child(
                            GhostButton::new(SharedString::from(format!("cp-view-{i}")), &theme, match (open, compact) {
                                (true, _) => "Ocultar cambios",
                                (false, true) => "Ver cambios",
                                (false, false) => "Ver cambios desde aquí",
                            })
                                .on_click(move |_, _, cx| m_view.update(cx, |m, cx| m.toggle_checkpoint_view(id_view.clone(), cx))),
                        )
                        .child(
                            GhostButton::new(SharedString::from(format!("cp-restore-{i}")), &theme, "Restaurar…").on_click(move |_, _, cx| {
                                if !busy {
                                    let op = GitOp::RestoreCheckpoint { id: id_restore.clone() };
                                    m_restore.update(cx, |m, cx| m.request(op, cx));
                                }
                            }),
                        ),
                );
            if open && let Some(v) = &view {
                card = card.children(self.checkpoint_files(&v.id, &v.state, diff.as_ref(), cx));
            }
            out.push(card.into_any_element());
        }
        out
    }

    fn checkpoint_files(
        &self,
        id: &str,
        state: &CheckpointFiles,
        diff: Option<&crate::model::DiffSlot>,
        _cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let note = |t: String| div().px(px(6.0)).py(px(4.0)).text_size(px(12.0)).text_color(c.muted).child(t).into_any_element();
        let files = match state {
            CheckpointFiles::Loading => return vec![note("Comparando…".into())],
            CheckpointFiles::Failed(e) => return vec![note(format!("No se pudo comparar: {e}"))],
            CheckpointFiles::Ready(f) if f.is_empty() => return vec![note("Sin cambios desde este checkpoint".into())],
            CheckpointFiles::Ready(f) => f,
        };
        let budget = path_budget(self.width - 16.0, true);
        let mut out = Vec::new();
        for (i, f) in files.iter().enumerate() {
            let letter = match f.kind {
                DeltaKind::Added => 'A',
                DeltaKind::Modified => 'M',
                DeltaKind::Deleted => 'D',
                DeltaKind::TypeChanged => 'T',
            };
            let row = FileRow {
                section: Section::Changes,
                path: f.path.clone(),
                text: crate::fmt::slashes(&f.path),
                letter,
                orig: None,
                counts: None,
                counted: false,
            };
            let target = DiffTarget::Checkpoint { id: id.to_string(), path: f.path.clone() };
            let selected = diff.is_some_and(|d| d.target == target);
            let model = self.model.clone();
            let t = target.clone();
            out.push(
                file_row(
                    &theme,
                    SharedString::from(format!("cpf-{i}")),
                    &row,
                    selected,
                    compact,
                    budget,
                    move |_, _, cx| model.update(cx, |m, cx| m.toggle_diff(t.clone(), cx)),
                    None,
                )
                .into_any_element(),
            );
            if selected && let Some(slot) = diff {
                out.push(diff_card(&theme, slot, compact, &self.model));
            }
        }
        out
    }

    fn scroll(&self, id: &'static str, children: Vec<AnyElement>) -> AnyElement {
        div()
            .id(id)
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .pb(px(8.0))
            .children(children)
            .into_any_element()
    }

    fn spaced(&self, blocks: Vec<Vec<AnyElement>>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        for (i, b) in blocks.into_iter().enumerate() {
            if i > 0 {
                out.push(div().h(px(10.0)).flex_none().into_any_element());
            }
            out.extend(b);
        }
        out
    }

    fn body(&mut self, data: &GitData, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        match self.layout() {
            Layout::Tabs => {
                let this = cx.entity();
                let tabs = div().flex().flex_row().flex_wrap().gap(px(4.0)).children(Tab::ALL.map(|t| {
                    let this = this.clone();
                    Pill::compact(SharedString::from(format!("git-tab-{}", t.index())), &theme)
                        .label(t.label())
                        .active(self.tab == t)
                        .on_click(move |_, _, cx| this.update(cx, |p, cx| p.select_tab(t, cx)))
                }));
                let content = match self.tab {
                    Tab::Changes => {
                        let rows = self.changes_block(data, cx);
                        let commit = self.commit_block(data, window, cx);
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(self.scroll("git-scroll-changes", rows))
                            .child(commit)
                            .into_any_element()
                    }
                    Tab::History => {
                        let rows = self.history_block(data, true, cx);
                        div().flex_1().min_h_0().w_full().flex().flex_col().children(rows).into_any_element()
                    }
                    Tab::Checkpoints => {
                        let rows = self.checkpoints_block(data, false, window, cx);
                        self.scroll("git-scroll-cp", rows)
                    }
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().flex_none().child(tabs))
                    .child(content)
                    .into_any_element()
            }
            Layout::Stacked => {
                let blocks = vec![
                    self.changes_block(data, cx),
                    self.history_block(data, false, cx),
                    self.checkpoints_block(data, true, window, cx),
                ];
                let commit = self.commit_block(data, window, cx);
                let rows = self.spaced(blocks);
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(self.scroll("git-scroll", rows))
                    .child(commit)
                    .into_any_element()
            }
            Layout::Columns => {
                let left_rows = self.changes_block(data, cx);
                let commit = self.commit_block(data, window, cx);
                let right_blocks = vec![self.history_block(data, false, cx), self.checkpoints_block(data, true, window, cx)];
                let right_rows = self.spaced(right_blocks);
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_row()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(self.scroll("git-scroll-left", left_rows))
                            .child(commit),
                    )
                    .child(div().flex_1().min_w_0().h_full().flex().flex_col().child(self.scroll("git-scroll-right", right_rows)))
                    .into_any_element()
            }
        }
    }

    /// Zero-height element at the root that reports the real width of the panel.
    fn probe(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let seen = self.probe.clone();
        let this = cx.entity();
        canvas(
            move |bounds, window, cx| {
                let w = f32::from(bounds.size.width);
                if (seen.get() - w).abs() > 6.0 {
                    seen.set(w);
                    window.defer(cx, move |_, cx| {
                        this.update(cx, |p, cx| {
                            p.width = w;
                            cx.notify();
                        });
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(px(0.0))
    }
}

fn section_key(s: Section) -> &'static str {
    match s {
        Section::Conflicts => "conflicts",
        Section::Staged => "staged",
        Section::Changes => "changes",
        Section::Untracked => "untracked",
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A painted panel is a visible panel: (re)starts the watcher and takes a fresh read.
        self.model.update(cx, |m, cx| m.touch(cx));

        let theme = self.ctx.theme;
        let c = theme.colors;
        let pad = if self.is_compact() { 12.0 } else { 16.0 };
        let (load, data) = {
            let m = self.model.read(cx);
            (m.load_state().clone(), m.data().cloned())
        };

        let content: AnyElement = match (&load, &data) {
            (Load::NoRoot, _) => empty_state(&theme, "Sin carpeta", "Abre una terminal dentro de un repositorio."),
            (Load::NotARepo, _) => empty_state(&theme, "No es un repositorio git", "Esta carpeta no está bajo control de versiones."),
            (Load::Failed(e), None) => empty_state(&theme, "No se pudo leer git", e),
            (_, None) => empty_state(&theme, "Leyendo repositorio…", ""),
            (_, Some(d)) => {
                let d = d.clone();
                let header = self.header(Some(&d), window, cx);
                let body = self.body(&d, window, cx);
                div().flex_1().min_h_0().w_full().flex().flex_col().gap(px(10.0)).child(header).child(body).into_any_element()
            }
        };

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .pt(px(pad))
            .px(px(pad))
            .pb(px(pad - 4.0))
            .font_family(theme.typography.ui_family)
            .text_color(c.text)
            .child(self.probe(cx))
            .child(content)
    }
}
