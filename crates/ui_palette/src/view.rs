//! The `CommandPalette` entity: a centered overlay card with a search field, grouped results and
//! the inline parameter form. All the logic lives in [`crate::model`]; this file schedules the
//! source queries (background executor, 30 ms debounce for store-backed sources, stale queries
//! cancelled) and paints.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    IntoElement, KeyBinding, ListAlignment, ListOffset, ListState, MouseButton, ParentElement,
    Render, SharedString, Styled, Subscription, Task, Window, actions, div, list, point,
    prelude::*, px,
};
use panel_api::{TabId, TabInfo};
use store::StateDb;
use theme::{Radius, Theme};
use ui_kit::text_field::act as text_act;
use ui_kit::{Icon, ListRow, Nav, Pill, SectionLabel, TextField, TextFieldEvent, icon};

use crate::item::{Effect, PaletteEvent};
use crate::model::{Confirmed, LayoutRow, PaletteModel, Section, plan, run_source};
use crate::query::{Query, Scope};
use crate::source::{Cancel, Latency, PaletteSource, SourceCtx};

actions!(
    ui_palette,
    [
        /// Open the palette if closed, close it if open. Bind it to Ctrl+K ([`bind_toggle_key`]).
        TogglePalette,
        SelectNext,
        SelectPrev,
        PageDown,
        PageUp,
        Confirm,
        ConfirmAlt,
        Dismiss,
        NextScope,
        PrevScope,
    ]
);

/// Key context of the palette card.
pub const CONTEXT: &str = "CommandPalette";

/// Quiet period before the store-backed sources (history, tasks, snippets, projects) are queried
/// after a keystroke. In-memory sources are queried on every keystroke without waiting.
pub const DEBOUNCE: Duration = Duration::from_millis(30);

const CARD_WIDTH: f32 = 640.0;
const HEADER_H: f32 = 30.0;
const ITEM_H: f32 = 44.0;
const LIST_PAD: f32 = 8.0;
const INPUT_ROW_H: f32 = 56.0;
const FOOTER_H: f32 = 34.0;
const PAGE: isize = 7;

/// Registers the palette's key bindings (`Ctrl+P` / `Ctrl+N` / `PageUp` / `PageDown`) and the
/// kit's. The search field is a `ui_kit::TextField` with navigation capture: `Up` / `Down` / `Tab`
/// reach the palette as [`TextFieldEvent::Nav`], `Enter` as `Submit` and `Esc` as `Cancel`;
/// `Ctrl+Enter`, `Shift+Tab` and (on a choice parameter) `Left` / `Right` are intercepted on the
/// card, before the field. Call once at startup. Does **not** bind the opening shortcut: see
/// [`bind_toggle_key`].
pub fn init(cx: &mut App) {
    ui_kit::init(cx);
    cx.bind_keys([
        KeyBinding::new("ctrl-p", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(CONTEXT)),
        KeyBinding::new("pageup", PageUp, Some(CONTEXT)),
        KeyBinding::new("pagedown", PageDown, Some(CONTEXT)),
    ]);
}

/// The default opening shortcut, in gpui's keystroke syntax.
pub const DEFAULT_TOGGLE_KEY: &str = "ctrl-k";

/// The binding of [`TogglePalette`] to `keystroke` (gpui syntax, `ctrl-k`), everywhere. A host
/// with rebindable shortcuts builds this one from its keymap instead of calling
/// [`bind_toggle_key`], so the palette follows the same mechanism as every other shortcut.
pub fn toggle_binding(keystroke: &str) -> KeyBinding {
    KeyBinding::new(keystroke, TogglePalette, None)
}

/// Binds `Ctrl+K` (everywhere) to [`TogglePalette`]. The host handles the action with
/// `cx.on_action(|_: &TogglePalette, cx| ...)`. Hosts that let the user rebind it use
/// [`toggle_binding`] instead.
pub fn bind_toggle_key(cx: &mut App) {
    cx.bind_keys([toggle_binding(DEFAULT_TOGGLE_KEY)]);
}

/// What the host tells the palette when it opens.
#[derive(Clone, Debug, Default)]
pub struct OpenContext {
    /// Working directory of the active tab.
    pub cwd: Option<std::path::PathBuf>,
    /// Git root / project folder that contains `cwd`.
    pub project_root: Option<std::path::PathBuf>,
    /// Snapshot of every tab (`TabHandle::info`).
    pub tabs: Vec<TabInfo>,
    pub active_tab: Option<TabId>,
    /// Text to start with: `">"` opens in the actions scope, `"!"` in tasks...
    pub initial: String,
}

impl OpenContext {
    fn source_ctx(&self) -> SourceCtx {
        SourceCtx {
            cwd: self.cwd.clone(),
            project_root: self.project_root.clone(),
            tabs: self.tabs.clone(),
            active_tab: self.active_tab,
        }
    }
}

/// How long the last query took, for the demo and the host's own metrics.
#[derive(Clone, Debug, Default)]
pub struct Timings {
    /// Query start to the first source answering (UI thread time of that answer included).
    pub first_results: Option<Duration>,
    /// Query start to each source's answer, in arrival order.
    pub per_source: Vec<(&'static str, Duration)>,
}

pub struct CommandPalette {
    theme: Theme,
    db: Option<StateDb>,
    sources: Vec<Arc<dyn PaletteSource>>,
    input: Entity<TextField>,
    open: bool,
    ctx: Arc<SourceCtx>,
    query: Query,
    generation: u64,
    cancel: Cancel,
    tasks: Vec<Task<()>>,
    pending: usize,
    started: Instant,
    timings: Timings,
    model: PaletteModel,
    list: ListState,
    prev_focus: Option<FocusHandle>,
    _input_sub: Option<Subscription>,
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

/// Creates the palette entity. `db` is used to record snippet / project use after a pick.
pub fn command_palette(
    theme: Theme,
    db: Option<StateDb>,
    sources: Vec<Arc<dyn PaletteSource>>,
    cx: &mut App,
) -> Entity<CommandPalette> {
    cx.new(|cx| CommandPalette::new(theme, db, sources, cx))
}

impl CommandPalette {
    pub fn new(
        theme: Theme,
        db: Option<StateDb>,
        sources: Vec<Arc<dyn PaletteSource>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut field = TextField::new(theme, cx)
                .bare()
                .placeholder(Scope::All.placeholder());
            field.set_capture_nav(true);
            field
        });
        Self {
            theme,
            db,
            sources,
            input,
            open: false,
            ctx: Arc::new(SourceCtx::default()),
            query: Query::parse(""),
            generation: 0,
            cancel: Cancel::new(),
            tasks: Vec::new(),
            pending: 0,
            started: Instant::now(),
            timings: Timings::default(),
            model: PaletteModel::default(),
            list: ListState::new(0, ListAlignment::Top, px(200.0)),
            prev_focus: None,
            _input_sub: None,
        }
    }

    /// Listens to the search field. Needs the window (confirming and closing move the focus), so
    /// it is done by the first [`open`](Self::open) rather than by the constructor.
    fn listen_to_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._input_sub.is_some() {
            return;
        }
        self._input_sub = Some(cx.subscribe_in(
            &self.input,
            window,
            |this, _, event: &TextFieldEvent, window, cx| match event {
                TextFieldEvent::Changed => this.on_text_changed(cx),
                TextFieldEvent::Submit => this.confirm(false, window, cx),
                TextFieldEvent::Cancel => this.dismiss(window, cx),
                TextFieldEvent::Nav(Nav::Up) => this.move_selection(-1, false, cx),
                TextFieldEvent::Nav(Nav::Down) => this.move_selection(1, false, cx),
                TextFieldEvent::Nav(Nav::Tab) => this.tab(1, cx),
                TextFieldEvent::SubmitReverse | TextFieldEvent::Blur => {}
            },
        ));
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub(crate) fn input_text(&self, cx: &App) -> String {
        self.input.read(cx).text().to_string()
    }

    #[cfg(test)]
    pub(crate) fn in_form_for_test(&self) -> bool {
        self.in_form()
    }

    pub fn timings(&self) -> &Timings {
        &self.timings
    }

    /// Adds a source, or replaces the one with the same id (for example a rebuilt actions list).
    pub fn set_source(&mut self, source: Arc<dyn PaletteSource>) {
        match self.sources.iter_mut().find(|s| s.id() == source.id()) {
            Some(slot) => *slot = source,
            None => self.sources.push(source),
        }
    }

    pub fn remove_source(&mut self, id: &str) {
        self.sources.retain(|s| s.id() != id);
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        self.input.update(cx, |i, cx| i.set_theme(theme, cx));
        cx.notify();
    }

    /// Opens the palette (no-op when it already is), focuses the search field and queries the
    /// in-memory sources right away.
    pub fn open(&mut self, ctx: OpenContext, window: &mut Window, cx: &mut Context<Self>) {
        self.listen_to_input(window, cx);
        if self.open {
            self.input.read(cx).focus(window);
            return;
        }
        self.prev_focus = window.focused(cx);
        self.ctx = Arc::new(ctx.source_ctx());
        self.open = true;
        self.model.clear();
        self.query = Query::parse(&ctx.initial);
        self.input.update(cx, |i, cx| {
            i.set_text(&ctx.initial, cx);
            i.set_placeholder(self.query.scope.placeholder(), cx);
            i.set_read_only(false, cx);
        });
        self.input.read(cx).focus(window);
        self.run_query(true, cx);
        cx.notify();
    }

    pub fn toggle(&mut self, ctx: OpenContext, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.open(ctx, window, cx);
        }
    }

    /// Closes the palette, gives the focus back and emits [`PaletteEvent::Closed`]. Drops every
    /// task and cache: a closed palette costs nothing.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.close_silently(window, cx);
        cx.emit(PaletteEvent::Closed);
    }

    fn close_silently(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.cancel.cancel();
        self.tasks.clear();
        self.pending = 0;
        self.model.clear();
        self.list.reset(0);
        self.ctx = Arc::new(SourceCtx::default());
        for s in &self.sources {
            s.on_close();
        }
        self.input.update(cx, |i, cx| {
            i.set_text("", cx);
            i.set_read_only(false, cx);
        });
        if let Some(prev) = self.prev_focus.take() {
            window.focus(&prev);
        }
        cx.notify();
    }

    // ---- querying ---------------------------------------------------------------------------

    fn on_text_changed(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        let text = self.input.read(cx).text().to_string();
        if let Some(form) = &mut self.model.form {
            form.set_active_value(&text);
            cx.notify();
            return;
        }
        let query = Query::parse(&text);
        if query.scope != self.query.scope {
            self.input
                .update(cx, |i, cx| i.set_placeholder(query.scope.placeholder(), cx));
        }
        self.query = query;
        self.run_query(false, cx);
        cx.notify();
    }

    /// Starts a new generation: cancels the previous one and queries every applicable source on
    /// the background executor, merging answers as they arrive.
    fn run_query(&mut self, immediate: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.cancel.cancel();
        self.cancel = Cancel::new();
        self.tasks.clear();
        self.started = Instant::now();
        self.timings = Timings::default();

        let planned = plan(&self.sources, &self.query);
        self.model
            .results
            .begin(generation, |id| planned.iter().any(|(s, _)| s.id() == id));
        self.model.query_changed();
        self.pending = planned.len();
        self.relayout(cx);

        for (source, limit) in planned {
            let query = self.query.clone();
            let ctx = self.ctx.clone();
            let cancel = self.cancel.clone();
            let started = self.started;
            let wait = (!immediate && source.latency() == Latency::Store).then_some(DEBOUNCE);
            let task = cx.spawn(async move |this, cx| {
                if let Some(wait) = wait {
                    cx.background_executor().timer(wait).await;
                }
                let section = cx
                    .background_executor()
                    .spawn(async move { run_source(source.as_ref(), limit, &query, &ctx, &cancel) })
                    .await;
                let elapsed = started.elapsed();
                let _ = this.update(cx, |p, cx| p.on_section(generation, section, elapsed, cx));
            });
            self.tasks.push(task);
        }
    }

    fn on_section(
        &mut self,
        generation: u64,
        section: Section,
        elapsed: Duration,
        cx: &mut Context<Self>,
    ) {
        let id = section.id;
        if !self.open || !self.model.results.push(generation, section) {
            return;
        }
        self.pending = self.pending.saturating_sub(1);
        self.timings.first_results.get_or_insert(elapsed);
        self.timings.per_source.push((id, elapsed));
        self.relayout(cx);
        cx.notify();
    }

    fn relayout(&mut self, _cx: &mut Context<Self>) {
        self.model.relayout(self.query.is_empty());
        let old = self.list.item_count();
        self.list.splice(0..old, self.model.rows.len());
        self.reveal_selection();
    }

    fn reveal_selection(&self) {
        let Some(row) = self.model.selected_row() else {
            return;
        };
        // The first item sits under its section header: show the header too.
        let first_item = self
            .model
            .rows
            .iter()
            .position(|r| matches!(r, LayoutRow::Item(_)));
        if first_item == Some(row) {
            self.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.0),
            });
        } else {
            self.list.scroll_to_reveal_item(row);
        }
    }

    // ---- actions ----------------------------------------------------------------------------

    fn in_form(&self) -> bool {
        self.model.form.is_some()
    }

    fn move_selection(&mut self, delta: isize, page: bool, cx: &mut Context<Self>) {
        if self.in_form() {
            if let Some(f) = &mut self.model.form {
                f.move_active(delta.signum());
            }
            self.sync_form_input(cx);
            return;
        }
        if page {
            self.model.page_selection(delta);
        } else {
            self.model.move_selection(delta);
        }
        self.reveal_selection();
        cx.notify();
    }

    fn confirm(&mut self, alt: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.model.confirm(alt) {
            Confirmed::Emit(event, effect) => {
                if let Some(effect) = effect {
                    self.apply_effect(&effect);
                }
                self.close_silently(window, cx);
                cx.emit(event);
                cx.emit(PaletteEvent::Closed);
            }
            Confirmed::FormOpened => {
                // The list is replaced by the form: stop querying.
                self.cancel.cancel();
                self.tasks.clear();
                self.sync_form_input(cx);
            }
            Confirmed::NextField => self.sync_form_input(cx),
            Confirmed::FormInvalid | Confirmed::Nothing => cx.notify(),
        }
    }

    fn apply_effect(&self, effect: &Effect) {
        let Some(db) = &self.db else { return };
        // Bookkeeping only: a failure must never stop the pick.
        match effect {
            Effect::SnippetUsed(id) => {
                let _ = db.record_snippet_use(*id);
            }
            Effect::PathOpened(path) => {
                let _ = db.mark_project_opened(path);
                let _ = db.record_visit(path);
            }
        }
    }

    /// Esc: leaves the form (back to the results) or closes the palette.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_form() {
            self.model.close_form();
            let raw = self.query.rewritten(self.query.scope);
            self.input.update(cx, |i, cx| {
                i.set_text(&raw, cx);
                i.set_read_only(false, cx);
                i.set_placeholder(self.query.scope.placeholder(), cx);
            });
            self.run_query(true, cx);
            cx.notify();
        } else {
            self.close(window, cx);
        }
    }

    /// `Tab` / `Shift+Tab`: the next / previous form field, or outside a form the next scope prefix.
    fn tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.in_form() {
            self.move_selection(delta, false, cx);
        } else {
            self.cycle_scope(delta, cx);
        }
    }

    /// Tab outside a form: next scope prefix.
    fn cycle_scope(&mut self, delta: isize, cx: &mut Context<Self>) {
        let scope = if delta >= 0 {
            self.query.scope.next()
        } else {
            self.query.scope.prev()
        };
        let raw = self.query.rewritten(scope);
        self.input.update(cx, |i, cx| i.set_text(&raw, cx));
        self.on_text_changed(cx);
    }

    /// Puts the active form field's value into the shared input.
    fn sync_form_input(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.model.form else { return };
        let value = form.active_value().to_string();
        let choice = form.active_is_choice();
        let hint = form
            .active_param()
            .map(|p| {
                p.default
                    .as_ref()
                    .map_or_else(|| p.name.clone(), |d| format!("{} ({d})", p.name))
            })
            .unwrap_or_default();
        self.input.update(cx, |i, cx| {
            i.set_text(&value, cx);
            i.set_read_only(choice, cx);
            i.set_placeholder(hint, cx);
        });
        cx.notify();
    }

    /// The active form parameter is a pick-one (its field is read-only: arrows cycle the options).
    fn on_choice(&self) -> bool {
        self.model
            .form
            .as_ref()
            .is_some_and(|f| f.active_is_choice())
    }

    fn cycle_choice(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(f) = &mut self.model.form
            && f.active_is_choice()
        {
            f.cycle_choice(delta);
            self.sync_form_input(cx);
        }
    }

    fn select_field(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(f) = &mut self.model.form {
            f.active = ix.min(f.len().saturating_sub(1));
        }
        self.sync_form_input(cx);
    }

    // ---- painting ---------------------------------------------------------------------------

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let selected = self.model.selected_row() == Some(ix);
        match self.model.rows.get(ix) {
            Some(LayoutRow::Header(title)) => div()
                .h(px(HEADER_H))
                .px(px(18.0))
                .pt(px(12.0))
                .child(SectionLabel::new(&theme, *title))
                .into_any_element(),
            Some(LayoutRow::Item(item)) => {
                let mut row = ListRow::new(
                    SharedString::from(format!("palette-row-{}", item.key)),
                    &theme,
                )
                .title(item.title.clone())
                .selected(selected)
                // The search field keeps the keyboard focus; the arrows move `selected`.
                .focusable(false);
                if let Some(s) = &item.subtitle {
                    row = row.subtitle(s.clone());
                }
                if let Some(m) = &item.meta {
                    row = row.meta(m.clone());
                }
                if let Some(i) = item.icon {
                    row = row.icon(i);
                }
                if let Some(s) = item.status {
                    row = row.status(s);
                }
                let this = cx.entity();
                row = row.on_click(move |ev: &ClickEvent, window, cx| {
                    let alt = ev.modifiers().control;
                    this.update(cx, |p, cx| {
                        p.model.select_at_row(ix);
                        p.confirm(alt, window, cx);
                    });
                });
                div().px(px(6.0)).child(row).into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn content_height(&self) -> f32 {
        let rows: f32 = self
            .model
            .rows
            .iter()
            .map(|r| match r {
                LayoutRow::Header(_) => HEADER_H,
                LayoutRow::Item(_) => ITEM_H,
            })
            .sum();
        rows + LIST_PAD
    }

    fn render_keys(&self) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let (enter, alt) = self.model.hints();
        let mut hints: Vec<(&str, String)> = Vec::new();
        if self.in_form() {
            hints.push(("Tab", "siguiente".into()));
        } else {
            hints.push(("↑↓", "navegar".into()));
        }
        if let Some(e) = enter {
            hints.push(("Enter", e.to_lowercase()));
        }
        if let Some(a) = alt {
            hints.push(("Ctrl+Enter", a.to_lowercase()));
        }
        if self.in_form() {
            hints.push(("Esc", "volver".into()));
        } else {
            hints.push(("Tab", "ámbito".into()));
            hints.push(("Esc", "cerrar".into()));
        }
        div()
            .h(px(FOOTER_H))
            .flex_none()
            .px(px(18.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .text_size(px(t.typography.ui_size_sm))
            .text_color(c.muted)
            .children(hints.into_iter().map(|(k, label)| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .h(px(18.0))
                            .px(px(6.0))
                            .flex()
                            .items_center()
                            .rounded(px(Radius::CHIP))
                            .bg(c.surface_3)
                            .text_size(px(t.typography.ui_size_xs))
                            .text_color(c.text)
                            .child(k),
                    )
                    .child(label)
            }))
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let Some(form) = &self.model.form else {
            return div().into_any_element();
        };
        let preview = form.preview();
        let this = cx.entity();
        let mut col = div().flex().flex_col().gap(px(2.0)).p(px(10.0)).child(
            div().px(px(8.0)).pb(px(6.0)).child(SectionLabel::new(
                &t,
                format!("{} · parámetros", form.spec.title),
            )),
        );
        for (i, p) in form.spec.params.iter().enumerate() {
            let active = i == form.active;
            let value = form.values.get(i).cloned().unwrap_or_default();
            let label: SharedString = p.name.replace(['_', '-'], " ").into();
            let this = this.clone();
            let field: AnyElement = if p.options.is_empty() {
                let shown: SharedString = if value.is_empty() {
                    "—".into()
                } else {
                    value.into()
                };
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(if shown == "—" { c.muted } else { c.text })
                    .child(shown)
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .children(p.options.iter().map(|o| {
                        Pill::compact(format!("palette-opt-{}-{o}", p.name), &t)
                            .label(o.clone())
                            .active(*o == value)
                            .focusable(false)
                    }))
                    .into_any_element()
            };
            col = col.child(
                div()
                    .id(SharedString::from(format!("palette-field-{}", p.name)))
                    .h(px(36.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .rounded(px(Radius::PILL))
                    .cursor_pointer()
                    .when(active, |d| d.bg(c.surface_3))
                    .when(!active, |d| d.hover(move |s| s.bg(c.surface_2)))
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.select_field(i, cx)))
                    .child(
                        div()
                            .w(px(110.0))
                            .flex_none()
                            .text_color(c.muted)
                            .child(label),
                    )
                    .child(field),
            );
        }
        let (text, color, mono) = match preview {
            Ok(line) => (line, c.text, true),
            Err(e) => (form.error.clone().unwrap_or(e), c.error, false),
        };
        col = col.child(
            div()
                .mt(px(8.0))
                .mx(px(2.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(Radius::PILL))
                .bg(c.surface_0)
                .text_color(color)
                .when(mono, |d| {
                    d.font_family(t.typography.mono_family)
                        .text_size(px(t.typography.mono_size))
                })
                .when(!mono, |d| d.text_size(px(t.typography.ui_size_md)))
                .child(text),
        );
        col.into_any_element()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().hidden().into_any_element();
        }
        let t = self.theme;
        let c = t.colors;
        let vp = window.viewport_size();
        let card_w = CARD_WIDTH.min(f32::from(vp.width) - 32.0).max(280.0);
        let max_card_h = f32::from(vp.height) * 0.6;
        let top = (f32::from(vp.height) * 0.14).max(24.0);
        let in_form = self.in_form();

        // Space left for the list: card max height minus input row, footer and margins.
        let list_budget = (max_card_h - INPUT_ROW_H - FOOTER_H - 8.0).max(120.0);
        let empty = !in_form && self.model.rows.is_empty();
        let list_h = if empty {
            72.0
        } else {
            self.content_height().min(list_budget)
        };

        let chip: Option<SharedString> = if let Some(f) = &self.model.form {
            f.active_param()
                .map(|p| SharedString::from(p.name.replace(['_', '-'], " ")))
        } else if self.query.scope != Scope::All {
            Some(self.query.scope.label().into())
        } else {
            None
        };

        let header = div()
            .h(px(INPUT_ROW_H))
            .flex_none()
            .px(px(18.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(icon(Icon::Search, 16.0, c.muted))
            .when_some(chip, |d, label| {
                d.child(
                    Pill::compact("palette-scope", &t)
                        .label(label)
                        .active(true)
                        .focusable(false),
                )
            })
            .child(self.input.clone());

        let body: AnyElement = if in_form {
            self.render_form(cx)
        } else if empty {
            let msg: SharedString = if self.pending > 0 || self.model.results.generation() == 0 {
                SharedString::default()
            } else if self.query.is_empty() {
                "Nada que mostrar".into()
            } else {
                format!("Sin resultados para «{}»", self.query.text).into()
            };
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(t.typography.ui_size_md))
                .text_color(c.muted)
                .child(msg)
                .into_any_element()
        } else {
            div()
                .size_full()
                .pt(px(LIST_PAD / 2.0))
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix, _window, cx| this.render_row(ix, cx)),
                    )
                    .size_full(),
                )
                .into_any_element()
        };

        let panel = div()
            .mx(px(8.0))
            .rounded(px(Radius::SM))
            .bg(c.surface_1)
            .overflow_hidden()
            .flex_none()
            .when(!in_form, |d| d.h(px(list_h)))
            .child(body);

        let card = div()
            .id("palette-card")
            .key_context(CONTEXT)
            .occlude()
            .w(px(card_w))
            .max_h(px(max_card_h))
            .flex()
            .flex_col()
            .rounded(px(Radius::LG))
            .bg(c.surface_2)
            .border_1()
            .border_color(c.outline_soft)
            .shadow(vec![BoxShadow {
                color: gpui::Rgba { a: 0.35, ..c.scrim }.into(),
                offset: point(px(0.0), px(18.0)),
                blur_radius: px(48.0),
                spread_radius: px(0.0),
            }])
            .overflow_hidden()
            .font_family(t.typography.ui_family)
            .text_size(px(t.typography.ui_size_md))
            .text_color(c.text)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, false, cx)),
            )
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.move_selection(PAGE, true, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.move_selection(-PAGE, true, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(false, window, cx)))
            .on_action(
                cx.listener(|this, _: &ConfirmAlt, window, cx| this.confirm(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &NextScope, _, cx| this.tab(1, cx)))
            .on_action(cx.listener(|this, _: &PrevScope, _, cx| this.tab(-1, cx)))
            // The field is deeper in the tree, so its key bindings win; these four are taken on
            // the way down (capture), before it sees them. What the field would do instead is
            // either nothing useful in a palette (`Shift+Tab` moves focus away, `Ctrl+Enter` is
            // a plain submit) or only wanted when the active parameter is a choice.
            .capture_action(cx.listener(|this, _: &text_act::CtrlEnter, window, cx| {
                this.confirm(true, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &text_act::BackTab, _, cx| {
                this.tab(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &text_act::Left, _, cx| {
                if this.on_choice() {
                    this.cycle_choice(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &text_act::Right, _, cx| {
                if this.on_choice() {
                    this.cycle_choice(1, cx);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(panel)
            .child(self.render_keys());

        div()
            .id("palette-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(c.scrim)
            .pt(px(top))
            .flex()
            .flex_col()
            .items_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close(window, cx)),
            )
            .child(card)
            .into_any_element()
    }
}
