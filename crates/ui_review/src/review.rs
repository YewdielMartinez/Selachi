//! `ReviewView`: the multi-file diff review (A9). A file list on the left, the unified diff of the
//! selected file on the right, and line notes: click the line-number gutter (shift-click another
//! line to select a range) and a note editor opens right under the line; saved notes stay as cards
//! with their state (Borrador / Enviada / Resuelta). "Enviar al agente" builds the Markdown message
//! and emits [`ReviewEvent::SendToAgent`]; the host delivers it and calls `mark_sent`.
//!
//! Zero idle cost: no watchers and no timers. The diff loads in the background executor on open and
//! on [`ReviewView::refresh`]; notes are re-anchored to the new lines when it arrives.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, Hsla,
    MouseButton, MouseDownEvent, ScrollHandle, SharedString, StyledText, Subscription, Task, Window, div, prelude::*,
    px,
};
use theme::{Mode, Radius, Theme, rgb};
use ui_kit::{ActionButton, Button, GhostButton, Icon, IconButton, SectionLabel, Spinner, icon};
use viewer_svc::{DiffLine, LineKind};

use crate::diff::{DiffRow, FileBody, FileDiff, FileStatus, ReviewSource, anchor_of, load_source};
use crate::field::{NoteField, NoteFieldEvent};
use crate::markdown::build_markdown;
use crate::note::{Note, NoteId, NoteRef, NoteSet, NoteState, Side};
use crate::store::{ReviewStore, draft_key};

/// Rows of one file painted before the "show more" button.
pub const ROW_PAGE: usize = 1_200;
/// Row height (Geist Mono 13).
const ROW_H: f32 = 19.0;
/// Approximate advance of one mono column at 13 px.
const CH: f32 = 7.9;
/// Lines longer than this are cut (a minified bundle is one huge line).
const MAX_LINE_BYTES: usize = 1_200;
/// Width of the review's own zone under which the file list starts hidden.
const COMPACT_BELOW_PX: f32 = 720.0;

/// What the review tells the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewEvent {
    /// Deliver `markdown` to the agent (a terminal tab running it, or a chat thread), then call
    /// [`ReviewView::mark_sent`] / `ReviewTab::mark_sent` with the `notes` ids.
    SendToAgent { markdown: String, notes: Vec<NoteRef> },
    /// Notes were added, edited, removed or changed state: persist `export_notes()` if you want to.
    NotesChanged { total: usize, drafts: usize },
}

enum Load {
    Loading,
    Ready,
    Failed(String),
}

struct Editor {
    path: String,
    side: Side,
    /// The line first clicked (the fixed end of a shift-click range).
    anchor: usize,
    start: usize,
    end: usize,
    editing: Option<NoteId>,
    field: Entity<NoteField>,
    _sub: Subscription,
}

/// The review content view.
pub struct ReviewView {
    source: ReviewSource,
    theme: Theme,
    load: Load,
    /// A (re)load is running in the background.
    loading: bool,
    files: Vec<FileDiff>,
    selected: usize,
    notes: NoteSet,
    editor: Option<Editor>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// `None` = decide from the width of the view's own zone.
    files_open: Option<bool>,
    measured: ui_kit::MeasuredWidth,
    shown_rows: usize,
    task: Option<Task<()>>,
}

impl EventEmitter<ReviewEvent> for ReviewView {}

impl Focusable for ReviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `ansi[i]` of the theme at `alpha`.
fn tint(theme: &Theme, ansi: usize, alpha: f32) -> Hsla {
    let mut c = rgb(theme.terminal.ansi[ansi]);
    c.a = alpha;
    c.into()
}

/// Foreground for text over a tint of `ansi_dark`/`ansi_light` (bright hues on dark, base on light).
fn ink(theme: &Theme, dark: usize, light: usize) -> Hsla {
    rgb(theme.terminal.ansi[if theme.mode == Mode::Light { light } else { dark }]).into()
}

fn state_colors(theme: &Theme, state: NoteState) -> (Hsla, Hsla) {
    match state {
        NoteState::Draft => (tint(theme, 3, 0.20), ink(theme, 11, 3)),
        NoteState::Sent => (tint(theme, 4, 0.20), ink(theme, 12, 4)),
        NoteState::Resolved => (tint(theme, 2, 0.20), ink(theme, 10, 2)),
    }
}

fn gutter_width(digits: usize) -> f32 {
    (digits as f32 * CH).ceil() + 20.0
}

fn digits_of(file: &FileDiff) -> usize {
    let max = file
        .rows
        .iter()
        .filter_map(|r| if let DiffRow::Line(l) = r { Some(l) } else { None })
        .flat_map(|l| [l.old_line, l.new_line])
        .flatten()
        .max()
        .unwrap_or(1);
    max.to_string().len().max(2)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl ReviewView {
    pub fn new(source: ReviewSource, theme: Theme, cx: &mut Context<Self>) -> Self {
        let notes = ReviewStore::global(cx).get(&draft_key(&source)).cloned().unwrap_or_default();
        let mut this = Self {
            source,
            theme,
            load: Load::Loading,
            loading: false,
            files: Vec::new(),
            selected: 0,
            notes,
            editor: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            files_open: None,
            measured: ui_kit::MeasuredWidth::new(COMPACT_BELOW_PX),
            shown_rows: ROW_PAGE,
            task: None,
        };
        this.refresh(cx);
        this
    }

    // ------------------------------------------------------------------------- public API

    pub fn source(&self) -> &ReviewSource {
        &self.source
    }

    /// The notes of this review.
    pub fn notes(&self) -> &NoteSet {
        &self.notes
    }

    /// The loaded files (empty while loading).
    pub fn files(&self) -> &[FileDiff] {
        &self.files
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn load_error(&self) -> Option<&str> {
        match &self.load {
            Load::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// Path of the selected file.
    pub fn selected_path(&self) -> Option<&str> {
        self.files.get(self.selected).map(|f| f.path.as_str())
    }

    /// Number of notes in the review and how many are still drafts.
    pub fn counts(&self) -> (usize, usize) {
        (self.notes.len(), self.notes.count(NoteState::Draft))
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        if let Some(ed) = &self.editor {
            ed.field.update(cx, |f, _| f.set_theme(theme));
        }
        cx.notify();
    }

    /// Reads the diff again in the background and re-anchors the notes to the new lines.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let source = self.source.clone();
        let bg = cx.background_executor().spawn(async move { load_source(&source) });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = bg.await;
            let _ = this.update(cx, |this, cx| this.finish_load(result, cx));
        }));
        cx.notify();
    }

    fn finish_load(&mut self, result: Result<Vec<FileDiff>, String>, cx: &mut Context<Self>) {
        self.loading = false;
        self.task = None;
        match result {
            Ok(files) => {
                let keep = self.selected_path().map(str::to_owned);
                let present: std::collections::BTreeSet<String> = files.iter().map(|f| f.path.clone()).collect();
                for f in &files {
                    if self.notes.paths().contains(f.path.as_str()) {
                        self.notes.reanchor_path(&f.path, &f.side_lines(Side::Old), &f.side_lines(Side::New));
                    }
                }
                self.notes.mark_missing_stale(&present);
                self.files = files;
                self.selected = keep.and_then(|p| self.files.iter().position(|f| f.path == p)).unwrap_or(0);
                self.shown_rows = ROW_PAGE;
                self.load = Load::Ready;
                self.persist(cx);
            }
            Err(e) => self.load = Load::Failed(e),
        }
        // An open editor whose line vanished keeps working: it only needs its path to exist.
        if let Some(ed) = &self.editor
            && !self.files.iter().any(|f| f.path == ed.path)
        {
            self.editor = None;
        }
        cx.notify();
    }

    /// Selects a file by its repository-relative path.
    pub fn select_path(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            self.select(i, cx);
        }
    }

    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.files.len() && i != self.selected {
            self.selected = i;
            self.shown_rows = ROW_PAGE;
            self.editor = None;
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    /// The host delivered the message: `Draft` -> `Sent` for those notes.
    pub fn mark_sent(&mut self, ids: &[NoteId], cx: &mut Context<Self>) {
        if self.notes.mark_sent(ids) > 0 {
            self.notes_changed(cx);
        }
    }

    /// `Sent` / `Draft` -> `Resolved`.
    pub fn mark_resolved(&mut self, ids: &[NoteId], cx: &mut Context<Self>) {
        let mut any = false;
        for id in ids {
            any |= self.notes.set_state(*id, NoteState::Resolved);
        }
        if any {
            self.notes_changed(cx);
        }
    }

    /// The notes of this review as JSON (for the host to store).
    pub fn export_notes(&self) -> String {
        self.notes.to_json()
    }

    /// Merges notes exported earlier; returns how many were added. Notes are re-anchored on the
    /// next [`ReviewView::refresh`] (or right away when the diff is loaded).
    pub fn import_notes(&mut self, json: &str, cx: &mut Context<Self>) -> Result<usize, String> {
        let added = self.notes.merge_json(json)?;
        if added > 0 {
            for f in &self.files {
                self.notes.reanchor_path(&f.path, &f.side_lines(Side::Old), &f.side_lines(Side::New));
            }
            self.notes_changed(cx);
        }
        Ok(added)
    }

    /// Builds the message for the drafts and emits [`ReviewEvent::SendToAgent`]. Returns `false`
    /// when there is nothing to send.
    pub fn send(&mut self, cx: &mut Context<Self>) -> bool {
        let drafts: Vec<&Note> = self.notes.drafts().collect();
        if drafts.is_empty() {
            return false;
        }
        let message = build_markdown(&self.source.phrase(), &drafts);
        let refs: Vec<NoteRef> = drafts.iter().map(|n| NoteRef::from(*n)).collect();
        cx.emit(ReviewEvent::SendToAgent { markdown: message.markdown, notes: refs });
        true
    }

    // ------------------------------------------------------------------------- editing

    /// Opens the editor on `line` of `side` in the selected file (what a gutter click does).
    /// With `extend` (shift) and an editor already open on that side, the range grows instead.
    pub fn begin_note(&mut self, side: Side, line: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected_path().map(str::to_owned) else { return };
        if extend
            && let Some(ed) = &mut self.editor
            && ed.editing.is_none()
            && ed.path == path
            && ed.side == side
        {
            ed.start = ed.anchor.min(line);
            ed.end = ed.anchor.max(line);
            cx.notify();
            return;
        }
        self.open_editor(path, side, line, line, None, "", window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn open_editor(
        &mut self,
        path: String,
        side: Side,
        start: usize,
        end: usize,
        editing: Option<NoteId>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme;
        let field = cx.new(|cx| NoteField::new(theme, "Escribe tu nota para el agente...", cx));
        field.update(cx, |f, cx| f.set_text(text, cx));
        let sub = cx.subscribe(&field, |this, _, ev: &NoteFieldEvent, cx| match ev {
            NoteFieldEvent::Submit => this.save_editor(cx),
            NoteFieldEvent::Cancel => this.cancel_editor(cx),
            NoteFieldEvent::Changed => cx.notify(),
        });
        field.read(cx).focus(window);
        self.editor = Some(Editor { path, side, anchor: start, start, end, editing, field, _sub: sub });
        cx.notify();
    }

    /// Saves the editor's text as a new note (or as the new text of the note being edited). An
    /// empty text just closes the editor.
    pub fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.editor.take() else { return };
        let text = ed.field.read(cx).text().trim().to_string();
        if text.is_empty() {
            cx.notify();
            return;
        }
        match ed.editing {
            Some(id) => {
                self.notes.set_text(id, &text);
            }
            None => {
                self.add_note(&ed.path, ed.side, ed.start, ed.end, &text, cx);
                return;
            }
        }
        self.notes_changed(cx);
    }

    /// Adds a draft note on `start..=end` of `side` of `path` (what saving the editor does; also
    /// for hosts that create notes themselves). The excerpt comes from the loaded diff; `None`
    /// when `text` is empty.
    pub fn add_note(&mut self, path: &str, side: Side, start: usize, end: usize, text: &str, cx: &mut Context<Self>) -> Option<NoteId> {
        if text.trim().is_empty() {
            return None;
        }
        let lines = self.files.iter().find(|f| f.path == path).map(|f| f.side_lines(side)).unwrap_or_default();
        let id = self.notes.next_id();
        self.notes.push(Note::new(id, path, side, start, end, text, &lines, now_secs()));
        self.notes_changed(cx);
        Some(id)
    }

    pub fn cancel_editor(&mut self, cx: &mut Context<Self>) {
        if self.editor.take().is_some() {
            cx.notify();
        }
    }

    /// Whether the note editor is open.
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    /// The text typed in the editor, if it is open.
    pub fn editor_text(&self, cx: &App) -> Option<String> {
        self.editor.as_ref().map(|e| e.field.read(cx).text().to_string())
    }

    /// Sets the editor text (tests, scripted demos).
    pub fn set_editor_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(ed) = &self.editor {
            ed.field.update(cx, |f, cx| f.set_text(text, cx));
        }
    }

    pub(crate) fn edit_note(&mut self, id: NoteId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.notes.get(id) else { return };
        let (path, side, start, end, text) = (n.path.clone(), n.side, n.start, n.end, n.text.clone());
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            self.select(i, cx);
        }
        self.open_editor(path, side, start, end, Some(id), &text, window, cx);
    }

    pub(crate) fn delete_note(&mut self, id: NoteId, cx: &mut Context<Self>) {
        if self.editor.as_ref().is_some_and(|e| e.editing == Some(id)) {
            self.editor = None;
        }
        if self.notes.remove(id) {
            self.notes_changed(cx);
        }
    }

    fn set_note_state(&mut self, id: NoteId, state: NoteState, cx: &mut Context<Self>) {
        if self.notes.set_state(id, state) {
            self.notes_changed(cx);
        }
    }

    fn notes_changed(&mut self, cx: &mut Context<Self>) {
        self.persist(cx);
        let (total, drafts) = self.counts();
        cx.emit(ReviewEvent::NotesChanged { total, drafts });
        cx.notify();
    }

    fn persist(&self, cx: &mut Context<Self>) {
        ReviewStore::global(cx).set(draft_key(&self.source), self.notes.clone());
    }

    /// Stops background work (the diff load). Notes stay in the store.
    pub fn release(&mut self) {
        self.task = None;
        self.loading = false;
    }

    // ------------------------------------------------------------------------- rendering

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

    fn header(&self, cx: &mut Context<Self>, compact: bool) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let (total, drafts) = self.counts();
        let (added, removed) = self.files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let files_open = self.files_open.unwrap_or(!compact);

        let make_chips = || {
            let mut chips = div().flex().flex_row().flex_wrap().items_center().gap(px(6.0));
            if !self.files.is_empty() {
                chips = chips
                    .child(self.chip(plural(self.files.len(), "archivo", "archivos")))
                    .child(self.chip(format!("+{added} \u{2212}{removed}")));
            }
            chips = chips.child(self.chip(plural(total, "nota", "notas")));
            if total > 0 && drafts != total {
                chips = chips.child(self.chip(plural(drafts, "borrador", "borradores")));
            }
            chips
        };

        let send_hint = if drafts == 0 {
            "Haz clic en el número de una línea para escribir una nota".to_string()
        } else {
            format!("Envía {} al agente", plural(drafts, "nota", "notas"))
        };
        let row = div()
            .w_full()
            .min_h(px(52.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                IconButton::new("review-files-toggle", &theme, Icon::Files)
                    .active(files_open)
                    .tooltip(if files_open { "Ocultar archivos" } else { "Mostrar archivos" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.files_open = Some(!files_open);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ty.ui_size_lg))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child(SharedString::from(self.source.title())),
                    )
                    .when(!compact, |d| d.child(make_chips())),
            )
            .child({
                let mut refresh = GhostButton::new("review-refresh", &theme, if self.loading { "Leyendo..." } else { "Actualizar" })
                    .tooltip("Vuelve a leer los cambios; las notas siguen a sus líneas");
                if !self.loading {
                    refresh = refresh.on_click(cx.listener(|this, _, _, cx| this.refresh(cx)));
                }
                refresh
            })
            .child(
                ActionButton::new("review-send", &theme, "Enviar al agente")
                    .icon(Icon::Send)
                    .disabled(drafts == 0)
                    .tooltip(send_hint)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send(cx);
                    })),
            );
        div()
            .flex_none()
            .px(px(16.0))
            .py(px(8.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .font_family(ty.ui_family)
            .child(row)
            .when(compact, |d| d.child(make_chips()))
            .into_any_element()
    }

    fn file_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let mut list = div().flex().flex_col().gap(px(2.0)).px(px(8.0)).pb(px(8.0));
        for (i, f) in self.files.iter().enumerate() {
            let selected = i == self.selected;
            let notes = self.notes.for_path(&f.path).len();
            let (mark_bg, mark_fg) = match f.status {
                FileStatus::Added => (tint(&theme, 2, 0.20), ink(&theme, 10, 2)),
                FileStatus::Modified => (tint(&theme, 3, 0.20), ink(&theme, 11, 3)),
                FileStatus::Deleted => (tint(&theme, 1, 0.20), ink(&theme, 9, 1)),
            };
            let stats: AnyElement = match f.body {
                FileBody::Text => div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap(px(5.0))
                    .text_size(px(ty.ui_size_sm))
                    .font_family(ty.mono_family)
                    .child(div().text_color(ink(&theme, 10, 2)).child(SharedString::from(format!("+{}", f.added))))
                    .child(div().text_color(ink(&theme, 9, 1)).child(SharedString::from(format!("\u{2212}{}", f.removed))))
                    .into_any_element(),
                FileBody::Binary => div().flex_none().text_size(px(ty.ui_size_sm)).text_color(c.muted).child("binario").into_any_element(),
                FileBody::TooLarge => div().flex_none().text_size(px(ty.ui_size_sm)).text_color(c.muted).child("grande").into_any_element(),
            };
            list = list.child(
                div()
                    .id(("review-file", i))
                    .occlude()
                    .w_full()
                    .h(px(48.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(10.0))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(c.surface_3))
                    .when(!selected, |d| d.hover(move |s| s.bg(c.surface_2)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(20.0))
                            .h(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(mark_bg)
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(mark_fg)
                            .child(f.status.mark()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(ty.ui_size_md))
                                    .text_color(c.text)
                                    .child(SharedString::from(f.name().to_owned())),
                            )
                            .when(!f.dir().is_empty(), |d| {
                                d.child(
                                    div()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(ty.ui_size_sm))
                                        .text_color(c.muted)
                                        .child(SharedString::from(f.dir().to_owned())),
                                )
                            }),
                    )
                    .child(stats)
                    .when(notes > 0, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .min_w(px(18.0))
                                .h(px(18.0))
                                .px(px(5.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(9.0))
                                .bg(tint(&theme, 3, 0.24))
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ink(&theme, 11, 3))
                                .child(SharedString::from(notes.to_string())),
                        )
                    }),
            );
        }
        div()
            .flex_none()
            .w(px(272.0))
            .h_full()
            .flex()
            .flex_col()
            .bg(c.surface_1)
            .rounded(px(Radius::SM))
            .m(px(8.0))
            .mr(px(0.0))
            .child(div().px(px(16.0)).pt(px(14.0)).pb(px(8.0)).child(SectionLabel::new(&theme, "Archivos")))
            .child(div().id("review-files").flex_1().min_h_0().overflow_y_scroll().child(list))
            .into_any_element()
    }

    fn number_cell(&self, n: Option<usize>, digits: usize) -> AnyElement {
        div()
            .flex_none()
            .w(px(gutter_width(digits)))
            .pr(px(10.0))
            .flex()
            .justify_end()
            .text_size(px(12.0))
            .child(SharedString::from(n.map(|n| n.to_string()).unwrap_or_default()))
            .into_any_element()
    }

    /// One diff line. The gutter (both numbers) is the click target for notes.
    fn line_row(&self, ix: usize, line: &DiffLine, digits: usize, in_range: bool, noted: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let bg: Option<Hsla> = if in_range {
            Some(tint(&theme, 4, 0.16))
        } else {
            match line.kind {
                LineKind::Added => Some(tint(&theme, 2, 0.13)),
                LineKind::Removed => Some(tint(&theme, 1, 0.13)),
                LineKind::Context => None,
            }
        };
        let (sign, sign_color): (&str, Hsla) = match line.kind {
            LineKind::Added => ("+", ink(&theme, 10, 2)),
            LineKind::Removed => ("-", ink(&theme, 9, 1)),
            LineKind::Context => (" ", c.outline.into()),
        };
        let strong = match line.kind {
            LineKind::Added => tint(&theme, 2, 0.34),
            _ => tint(&theme, 1, 0.34),
        };
        let mut text = line.text.clone();
        let mut marks: Vec<_> = line.inline.iter().map(|r| (r.clone(), HighlightStyle { background_color: Some(strong), ..HighlightStyle::default() })).collect();
        if text.contains('\t') {
            text = text.replace('\t', "    ");
            marks.clear();
        }
        if text.len() > MAX_LINE_BYTES {
            let mut cut = MAX_LINE_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push('\u{2026}');
            marks.clear();
        }
        let anchor = anchor_of(line);
        let gutter_bg: Option<Hsla> = if noted { Some(tint(&theme, 3, 0.28)) } else { None };
        let hover = c.surface_4;
        let mut gutter = div()
            .id(("review-gutter", ix))
            .flex_none()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .text_color(if noted { ink(&theme, 11, 3) } else { c.outline.into() })
            .when_some(gutter_bg, |d, b| d.bg(b))
            // The 10 px note marker (a fixed slot, so numbers do not shift when a note appears).
            .child(div().flex_none().w(px(16.0)).pl(px(4.0)).when(noted, |d| d.child(icon(Icon::Note, 10.0, ink(&theme, 11, 3)))))
            .child(self.number_cell(line.old_line, digits))
            .child(self.number_cell(line.new_line, digits));
        if let Some((side, n)) = anchor {
            gutter = gutter
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(ui_kit::Tooltip::text(&theme, "Clic: nota en esta línea · Mayús+clic: rango"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, window, cx| this.begin_note(side, n, e.modifiers.shift, window, cx)),
                );
        }
        div()
            .w_full()
            .h(px(ROW_H))
            .flex()
            .flex_row()
            .items_center()
            .font_family(theme.typography.mono_family)
            .text_size(px(theme.typography.mono_size))
            .line_height(px(ROW_H))
            .text_color(c.text)
            .when_some(bg, |d, b| d.bg(b))
            .child(gutter)
            .child(div().flex_none().w(px(14.0)).text_color(sign_color).child(sign))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(StyledText::new(SharedString::from(text)).with_highlights(marks)),
            )
            .into_any_element()
    }

    fn hunk_row(&self, header: &str) -> AnyElement {
        let c = self.theme.colors;
        div()
            .w_full()
            .h(px(ROW_H + 3.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .bg(c.surface_1)
            .font_family(self.theme.typography.mono_family)
            .text_size(px(12.0))
            .text_color(c.outline)
            .child(SharedString::from(header.to_owned()))
            .into_any_element()
    }

    /// A saved note as a card.
    fn note_card(&self, note: &Note, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let id = note.id;
        let (pill_bg, pill_fg) = state_colors(&theme, note.state);
        let range = if note.start == note.end { format!("L{}", note.start) } else { format!("L{}\u{2013}{}", note.start, note.end) };
        let side_tag = if note.side == Side::Old { " · versión anterior" } else { "" };
        let ix = id.0 as usize;
        let mut actions = div().flex_none().flex().flex_row().items_center().gap(px(2.0));
        match note.state {
            NoteState::Draft => {}
            NoteState::Sent => {
                actions = actions.child(
                    GhostButton::new(format!("review-resolve-{ix}"), &theme, "Resolver")
                        .on_click(cx.listener(move |this, _, _, cx| this.set_note_state(id, NoteState::Resolved, cx))),
                );
            }
            NoteState::Resolved => {
                actions = actions.child(
                    GhostButton::new(format!("review-reopen-{ix}"), &theme, "Reabrir")
                        .on_click(cx.listener(move |this, _, _, cx| this.set_note_state(id, NoteState::Draft, cx))),
                );
            }
        }
        actions = actions
            .child(
                IconButton::new(format!("review-edit-{ix}"), &theme, Icon::Edit)
                    .size(28.0)
                    .tooltip("Editar nota")
                    .on_click(cx.listener(move |this, _, window, cx| this.edit_note(id, window, cx))),
            )
            .child(
                IconButton::new(format!("review-del-{ix}"), &theme, Icon::Trash)
                    .size(28.0)
                    .tooltip("Eliminar nota")
                    .on_click(cx.listener(move |this, _, _, cx| this.delete_note(id, cx))),
            );
        div()
            .w_full()
            .rounded(px(12.0))
            .bg(c.surface_2)
            .px(px(14.0))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .font_family(ty.ui_family)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_none()
                            .h(px(20.0))
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .bg(pill_bg)
                            .text_size(px(ty.ui_size_sm))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(pill_fg)
                            .child(note.state.label()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child(SharedString::from(format!("{range}{side_tag}"))),
                    )
                    .when(note.stale, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(px(ty.ui_size_sm))
                                .text_color(ink(&theme, 11, 3))
                                .child("Las líneas cambiaron"),
                        )
                    })
                    .child(actions),
            )
            .child(div().text_size(px(ty.ui_size_md)).line_height(px(20.0)).text_color(c.text).child(SharedString::from(note.text.clone())))
            .into_any_element()
    }

    fn editor_card(&self, ed: &Editor, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let range = if ed.start == ed.end { format!("línea {}", ed.start) } else { format!("líneas {}\u{2013}{}", ed.start, ed.end) };
        let title = if ed.editing.is_some() { format!("Editar nota · {range}") } else { format!("Nota en {range}") };
        let focused = ed.field.read(cx).focus_handle(cx).is_focused(window);
        div()
            .w_full()
            .rounded(px(12.0))
            .bg(c.surface_2)
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .font_family(ty.ui_family)
            .child(div().text_size(px(ty.ui_size_sm)).font_weight(FontWeight::MEDIUM).text_color(c.muted).child(SharedString::from(title)))
            .child(
                div()
                    .w_full()
                    .rounded(px(10.0))
                    .bg(c.surface_1)
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_1()
                    .border_color(if focused { c.accent.into() } else { gpui::transparent_black() })
                    .child(ed.field.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child("Ctrl+Intro guarda · Esc cancela · Mayús+clic en otra línea amplía el rango"),
                    )
                    .child(Button::tonal("review-note-cancel", &theme, "Cancelar").on_click(cx.listener(|this, _, _, cx| this.cancel_editor(cx))))
                    .child(Button::primary("review-note-save", &theme, "Guardar nota").on_click(cx.listener(|this, _, _, cx| this.save_editor(cx)))),
            )
            .into_any_element()
    }

    /// Wraps a card so it lines up under the code (right of the gutter).
    fn indented(&self, digits: usize, child: AnyElement) -> AnyElement {
        div()
            .w_full()
            .pl(px(gutter_width(digits) * 2.0 + 14.0))
            .pr(px(16.0))
            .py(px(6.0))
            .child(child)
            .into_any_element()
    }

    fn diff_body(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let Some(file) = self.files.get(self.selected) else {
            return self.message(Icon::Files, "Selecciona un archivo");
        };
        match file.body {
            FileBody::Binary => return self.message(Icon::Files, "Archivo binario: no hay diff de texto"),
            FileBody::TooLarge => return self.message(Icon::Files, "Archivo demasiado grande para mostrar su diff"),
            FileBody::Text => {}
        }
        let digits = digits_of(file);
        let file_notes = self.notes.for_path(&file.path);

        // Notes by the row they sit under; the rest are shown above the diff.
        let mut under: std::collections::HashMap<usize, Vec<&Note>> = std::collections::HashMap::new();
        let mut loose: Vec<&Note> = Vec::new();
        let mut noted_rows: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for n in &file_notes {
            let end_row = if n.stale { None } else { file.row_of(n.side, n.end) };
            match end_row {
                Some(r) if r < self.shown_rows.min(file.rows.len()) => under.entry(r).or_default().push(n),
                _ => loose.push(n),
            }
            if !n.stale {
                for l in n.start..=n.end {
                    if let Some(r) = file.row_of(n.side, l) {
                        noted_rows.insert(r);
                    }
                }
            }
        }
        let editor_under = self.editor.as_ref().filter(|e| e.path == file.path).and_then(|e| file.row_of(e.side, e.end));

        let mut rows = div().w_full().flex().flex_col().pb(px(24.0));
        if !loose.is_empty() {
            let mut block = div().w_full().flex().flex_col().gap(px(8.0)).px(px(16.0)).pt(px(12.0)).pb(px(8.0));
            block = block.child(SectionLabel::new(&theme, "Notas sin línea en el diff"));
            for n in &loose {
                block = block.child(self.note_card(n, cx));
            }
            rows = rows.child(block);
        }
        let shown = self.shown_rows.min(file.rows.len());
        for (ix, row) in file.rows.iter().enumerate().take(shown) {
            match row {
                DiffRow::Hunk(h) => rows = rows.child(self.hunk_row(h)),
                DiffRow::Line(l) => {
                    let in_range = self.editor.as_ref().is_some_and(|e| {
                        e.path == file.path
                            && anchor_of(l).is_some_and(|(side, n)| side == e.side && (e.start..=e.end).contains(&n))
                    });
                    rows = rows.child(self.line_row(ix, l, digits, in_range, noted_rows.contains(&ix), cx));
                }
            }
            if let Some(notes) = under.get(&ix) {
                for n in notes {
                    rows = rows.child(self.indented(digits, self.note_card(n, cx)));
                }
            }
            if editor_under == Some(ix)
                && let Some(ed) = &self.editor
            {
                rows = rows.child(self.indented(digits, self.editor_card(ed, window, cx)));
            }
        }
        if shown < file.rows.len() {
            let more = (file.rows.len() - shown).min(ROW_PAGE);
            rows = rows.child(
                div().w_full().p(px(16.0)).flex().justify_center().child(
                    Button::tonal("review-more", &theme, format!("Mostrar {more} filas más ({} sin mostrar)", file.rows.len() - shown))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.shown_rows += ROW_PAGE;
                            cx.notify();
                        })),
                ),
            );
        }

        div()
            .id("review-diff")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .py(px(4.0))
            .bg(c.surface_0)
            .child(
                div()
                    .w_full()
                    .px(px(16.0))
                    .py(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(theme.typography.ui_size_md))
                    .text_color(c.muted)
                    .child(ui_kit::file_icon(ui_kit::FileIcon::for_path(std::path::Path::new(&file.path)), 16.0, c.muted))
                    .child(div().text_color(c.text).font_weight(FontWeight::MEDIUM).child(SharedString::from(file.path.clone())))
                    .child(SharedString::from(format!("+{} \u{2212}{}", file.added, file.removed))),
            )
            .child(rows)
            .into_any_element()
    }

    fn message(&self, glyph: Icon, text: impl Into<SharedString>) -> AnyElement {
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
            .child(icon(glyph, 24.0, c.outline))
            .child(text.into())
            .into_any_element()
    }
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let c = theme.colors;
        let compact = self.measured.narrow(window);
        let files_open = self.files_open.unwrap_or(!compact);

        let body: AnyElement = match &self.load {
            Load::Loading => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .text_size(px(theme.typography.ui_size_md))
                .text_color(c.muted)
                .child(Spinner::new(c.accent))
                .child("Leyendo los cambios...")
                .into_any_element(),
            Load::Failed(e) => self.message(Icon::Alert, e.clone()),
            Load::Ready if self.files.is_empty() => self.message(Icon::Check, "No hay cambios que revisar"),
            Load::Ready => {
                let diff = self.diff_body(window, cx);
                let mut row = div().size_full().flex().flex_row();
                if files_open {
                    row = row.child(self.file_list(cx));
                }
                row.child(div().flex_1().min_w_0().h_full().child(diff)).into_any_element()
            }
        };

        div()
            .key_context("ReviewView")
            .track_focus(&self.focus)
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
            .child(self.measured.probe())
            .child(self.header(cx, compact))
            .child(div().flex_1().min_h_0().child(body))
    }
}
