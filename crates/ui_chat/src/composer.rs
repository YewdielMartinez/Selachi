//! The composer: a rounded card with the text field, `+`, the context meter, the permission /
//! model / agent pills and the round send-or-stop button; the plan bar above it and the
//! branch and path line below. It never talks to chat_core: it shows a [`ComposerState`] pushed
//! by the view and reports what the user did as [`ComposerEvent`]s.

use std::path::PathBuf;

use agents_driver::{AgentKind, PermissionMode};
use chat_core::{ContextUsage, Plan};
use agents_driver::TaskState;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FontWeight, IntoElement, ParentElement as _, PathPromptOptions,
    Render, SharedString, Styled as _, Subscription, Window, div, prelude::*, px, relative,
};
use theme::{Radius, Theme};
use ui_kit::{Button, DitherMeter, Icon, IconButton, Pill, Placement, Popover, Spinner, Tooltip, icon};

use crate::field::{ChatField, FieldEvent, Nav};
use crate::hub::{ChatHub, FilesReady};
use crate::mentions::{MentionQuery, filter_files, mention_at};
use crate::options::{
    AgentOption, PERMISSION_ORDER, is_dangerous, model_label, models_for, permission_hint, permission_label,
};
use crate::util::{file_name, format_tokens, format_window, tail_path};
use crate::widgets::{amber, file_chip, green, menu_row};

/// How many files the `@` picker lists.
const MENTION_ROWS: usize = 8;

/// Everything the composer displays, pushed by the owner.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposerState {
    /// A turn is running: the round button is Stop.
    pub working: bool,
    pub context: Option<ContextUsage>,
    pub permission: PermissionMode,
    /// The pill is inert while a turn runs.
    pub can_edit_permission: bool,
    pub agent: AgentKind,
    pub model: Option<String>,
    pub agents: Vec<AgentOption>,
    /// Root of the project: where `@` mentions come from.
    pub project_root: Option<PathBuf>,
    /// Branch under the composer.
    pub branch: Option<String>,
    /// Worktree or working folder under the composer.
    pub path: Option<PathBuf>,
    pub plan: Option<Plan>,
    /// Placeholder of the field.
    pub placeholder: SharedString,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            working: false,
            context: None,
            permission: PermissionMode::Ask,
            can_edit_permission: true,
            agent: AgentKind::ClaudeCode,
            model: None,
            agents: crate::options::default_agents(),
            project_root: None,
            branch: None,
            path: None,
            plan: None,
            placeholder: "Pide algo al agente…  (@ para mencionar archivos)".into(),
        }
    }
}

/// What the round button does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonMode {
    /// A turn is running: interrupt it.
    Stop,
    /// Send the message.
    Send,
    /// Nothing to send.
    Disabled,
}

/// The round button's mode for a composer that is `working` and has (or not) something to send.
pub fn button_mode(working: bool, has_content: bool) -> ButtonMode {
    if working {
        ButtonMode::Stop
    } else if has_content {
        ButtonMode::Send
    } else {
        ButtonMode::Disabled
    }
}

/// What the user did in the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerEvent {
    Send { text: String, attachments: Vec<PathBuf> },
    Stop,
    SetPermission(PermissionMode),
    SetModel(Option<String>),
    /// The user picked another agent in the menu: the owner decides (a draft just switches; a
    /// thread asks for the handoff preview and calls [`Composer::set_handoff_preview`]).
    PickAgent(AgentKind),
    /// The user confirmed the handoff popover.
    ConfirmSwitch { agent: AgentKind, model: Option<String> },
}

/// The handoff text of a pending agent switch.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Loading,
    /// The thread has nothing to hand over yet.
    Empty,
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Popup {
    Plus,
    Permission,
    Model,
    Agent,
    Handoff { agent: AgentKind, preview: Preview },
}

struct Mention {
    query: MentionQuery,
    items: Vec<String>,
    selected: usize,
}

pub struct Composer {
    hub: Entity<ChatHub>,
    theme: Theme,
    state: ComposerState,
    field: Entity<ChatField>,
    model_field: Entity<ChatField>,
    attachments: Vec<PathBuf>,
    popup: Option<Popup>,
    mention: Option<Mention>,
    plan_open: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(hub: Entity<ChatHub>, theme: Theme, cx: &mut Context<Self>) -> Self {
        let state = ComposerState::default();
        let field = cx.new(|cx| {
            let mut f = ChatField::new(theme, state.placeholder.clone(), true, cx);
            f.set_lines(1, 10);
            f
        });
        let model_field = cx.new(|cx| ChatField::new(theme, "Otro modelo…", false, cx));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&field, |this, _f, ev: &FieldEvent, cx| this.on_field(*ev, cx)));
        subs.push(cx.subscribe(&model_field, |this, f, ev: &FieldEvent, cx| match ev {
            FieldEvent::Submit => {
                let m = f.read(cx).text().trim().to_string();
                if !m.is_empty() {
                    this.popup = None;
                    f.update(cx, |f, cx| f.set_text("", cx));
                    cx.emit(ComposerEvent::SetModel(Some(m)));
                    cx.notify();
                }
            }
            FieldEvent::Cancel => this.close_popup(cx),
            _ => {}
        }));
        subs.push(cx.subscribe(&hub, |this, _hub, ev: &FilesReady, cx| {
            if this.mention.is_some() && this.state.project_root.as_deref() == Some(ev.root.as_path()) {
                this.refresh_mention(cx);
            }
        }));
        Self { hub, theme, state, field, model_field, attachments: Vec::new(), popup: None, mention: None, plan_open: false, _subs: subs }
    }

    // ----- state ---------------------------------------------------------------------------

    pub fn state(&self) -> &ComposerState {
        &self.state
    }

    /// Replaces what is displayed. Returns whether anything changed (the owner notifies).
    pub fn set_state(&mut self, state: ComposerState, cx: &mut Context<Self>) -> bool {
        if self.state == state {
            return false;
        }
        if self.state.placeholder != state.placeholder {
            let p = state.placeholder.clone();
            self.field.update(cx, |f, _| f.set_placeholder(p));
        }
        if state.working != self.state.working {
            // A running turn closes pickers that would change it under its feet.
            if state.working && matches!(self.popup, Some(Popup::Permission)) {
                self.popup = None;
            }
        }
        self.state = state;
        true
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        self.field.update(cx, |f, _| f.set_theme(theme));
        self.model_field.update(cx, |f, _| f.set_theme(theme));
        cx.notify();
    }

    pub fn field(&self) -> &Entity<ChatField> {
        &self.field
    }

    pub fn text(&self, cx: &App) -> String {
        self.field.read(cx).text().to_string()
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.field.update(cx, |f, cx| f.set_text(text, cx));
        self.refresh_mention(cx);
        cx.notify();
    }

    pub fn attachments(&self) -> &[PathBuf] {
        &self.attachments
    }

    pub fn add_attachments(&mut self, paths: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for p in paths {
            if !self.attachments.contains(&p) {
                self.attachments.push(p);
            }
        }
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.field.read(cx).focus(window);
    }

    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    pub fn close_popup(&mut self, cx: &mut Context<Self>) {
        if self.popup.take().is_some() {
            cx.notify();
        }
    }

    pub fn is_mention_open(&self) -> bool {
        self.mention.is_some()
    }

    /// The answer to [`ComposerEvent::PickAgent`] for a thread: the handoff text, or `None` when
    /// there is nothing to hand over.
    pub fn set_handoff_preview(&mut self, agent: &AgentKind, text: Option<String>, cx: &mut Context<Self>) {
        if let Some(Popup::Handoff { agent: a, preview }) = &mut self.popup
            && a == agent
        {
            *preview = match text {
                Some(t) => Preview::Text(t),
                None => Preview::Empty,
            };
            cx.notify();
        }
    }

    pub fn has_content(&self, cx: &App) -> bool {
        !self.field.read(cx).is_empty() || !self.attachments.is_empty()
    }

    pub fn button_mode(&self, cx: &App) -> ButtonMode {
        button_mode(self.state.working, self.has_content(cx))
    }

    // ----- actions -------------------------------------------------------------------------

    /// Sends what is typed (`Enter`, or the round button).
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.field.read(cx).text().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        let attachments = std::mem::take(&mut self.attachments);
        self.field.update(cx, |f, cx| {
            f.take_text(cx);
            f.set_capture_nav(false);
        });
        self.mention = None;
        self.popup = None;
        cx.emit(ComposerEvent::Send { text, attachments });
        cx.notify();
    }

    /// The round button.
    pub fn press_round_button(&mut self, cx: &mut Context<Self>) {
        match self.button_mode(cx) {
            ButtonMode::Stop => cx.emit(ComposerEvent::Stop),
            ButtonMode::Send => self.submit(cx),
            ButtonMode::Disabled => {}
        }
    }

    fn on_field(&mut self, ev: FieldEvent, cx: &mut Context<Self>) {
        match ev {
            FieldEvent::Changed => self.refresh_mention(cx),
            FieldEvent::Submit => {
                if self.mention.as_ref().is_some_and(|m| !m.items.is_empty()) {
                    self.accept_mention(None, cx);
                } else {
                    self.submit(cx);
                }
            }
            FieldEvent::Cancel => {
                if self.mention.is_some() || self.popup.is_some() {
                    self.mention = None;
                    self.popup = None;
                    self.field.update(cx, |f, _| f.set_capture_nav(false));
                    cx.notify();
                } else if self.state.working {
                    cx.emit(ComposerEvent::Stop);
                }
            }
            FieldEvent::Nav(nav) => {
                if let Some(m) = &mut self.mention {
                    let n = m.items.len();
                    if n > 0 {
                        match nav {
                            Nav::Up => m.selected = (m.selected + n - 1) % n,
                            Nav::Down => m.selected = (m.selected + 1) % n,
                            Nav::Tab => {
                                self.accept_mention(None, cx);
                                return;
                            }
                        }
                        cx.notify();
                    }
                }
            }
        }
    }

    /// Recomputes the `@` picker from the text and the caret.
    fn refresh_mention(&mut self, cx: &mut Context<Self>) {
        let (text, caret) = {
            let f = self.field.read(cx);
            (f.text().to_string(), f.caret())
        };
        let Some(q) = mention_at(&text, caret) else {
            self.close_mention(cx);
            return;
        };
        let Some(root) = self.state.project_root.clone() else {
            self.close_mention(cx);
            return;
        };
        let files = self.hub.read(cx).files_for(&root);
        let items = match files {
            Some(f) => filter_files(&f, &q.query, MENTION_ROWS),
            None => {
                self.hub.update(cx, |h, cx| h.load_files(root, cx));
                Vec::new()
            }
        };
        let selected = match &self.mention {
            Some(m) if m.query.range.start == q.range.start => m.selected.min(items.len().saturating_sub(1)),
            _ => 0,
        };
        let capture = !items.is_empty();
        self.mention = Some(Mention { query: q, items, selected });
        self.field.update(cx, |f, _| f.set_capture_nav(capture));
        cx.notify();
    }

    fn close_mention(&mut self, cx: &mut Context<Self>) {
        if self.mention.take().is_some() {
            self.field.update(cx, |f, _| f.set_capture_nav(false));
            cx.notify();
        }
    }

    /// Inserts the chosen path (`index`, or the highlighted one) in place of the `@query`.
    pub fn accept_mention(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(m) = self.mention.take() else { return };
        let ix = index.unwrap_or(m.selected);
        let Some(path) = m.items.get(ix) else { return };
        let insert = format!("@{path} ");
        self.field.update(cx, |f, cx| {
            f.set_capture_nav(false);
            f.replace_range(m.query.range.clone(), &insert, cx);
        });
        // `replace_range` re-ran the mention logic through `Changed`: the token is closed now.
        self.mention = None;
        cx.notify();
    }

    /// The `@` of the "mencionar archivo" menu entry.
    fn insert_at_sign(&mut self, cx: &mut Context<Self>) {
        self.popup = None;
        self.field.update(cx, |f, cx| f.insert("@", cx));
        cx.notify();
    }

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        self.popup = None;
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |c, cx| c.add_attachments(paths, cx));
            }
        })
        .detach();
        cx.notify();
    }

    fn toggle_popup(&mut self, p: Popup, cx: &mut Context<Self>) {
        self.popup = if self.popup.as_ref().map(std::mem::discriminant) == Some(std::mem::discriminant(&p)) { None } else { Some(p) };
        cx.notify();
    }

    fn pick_agent(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if kind == self.state.agent {
            self.popup = None;
        } else {
            self.popup = Some(Popup::Handoff { agent: kind.clone(), preview: Preview::Loading });
            cx.emit(ComposerEvent::PickAgent(kind));
        }
        cx.notify();
    }

    fn confirm_switch(&mut self, agent: AgentKind, cx: &mut Context<Self>) {
        self.popup = None;
        cx.emit(ComposerEvent::ConfirmSwitch { agent, model: None });
        cx.notify();
    }
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// `8% ▬ 1M` plus its tooltip, or `None` while the agent has not reported a window.
fn context_meter(theme: &Theme, ctx: Option<ContextUsage>, compact: bool) -> Option<AnyElement> {
    let ctx = ctx?;
    let c = theme.colors;
    let ty = theme.typography;
    let pct = if ctx.percent > 0.0 && ctx.percent < 1.0 { "<1%".to_string() } else { format!("{}%", ctx.percent.round() as u32) };
    let tip = format!(
        "Contexto: {} de {} tokens ({pct})",
        format_tokens(ctx.used_tokens),
        format_tokens(ctx.window_tokens)
    );
    let t = *theme;
    Some(
        div()
            .id("cmp-context")
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(6.0))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .tooltip(Tooltip::text(&t, tip))
            .child(pct)
            .child(DitherMeter::new(theme).segments(6).value(Some((ctx.percent / 100.0).clamp(0.0, 1.0))))
            .when(!compact, |d| d.child(format_window(ctx.window_tokens)))
            .into_any_element(),
    )
}

fn chevron(theme: &Theme) -> AnyElement {
    icon(Icon::ChevronDown, 14.0, theme.colors.muted).into_any_element()
}

impl Composer {
    fn plan_bar(&self, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (done, total) = plan.progress();
        let current = plan
            .items
            .iter()
            .find(|i| i.state == TaskState::InProgress)
            .or_else(|| plan.items.iter().find(|i| i.state != TaskState::Done))
            .map(|i| i.text.clone());
        let open = self.plan_open;
        let header = div()
            .id("cmp-plan-toggle")
            .h(px(34.0))
            .px(px(14.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .text_size(px(ty.ui_size_md))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.plan_open = !this.plan_open;
                cx.notify();
            }))
            .child(div().font_weight(FontWeight::MEDIUM).text_color(c.text).child(format!("{done}/{total}")))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(c.muted)
                    .child(SharedString::from(match (&current, done == total) {
                        (_, true) => "Plan completado".to_string(),
                        (Some(cur), _) if !open => cur.clone(),
                        _ => "Plan".to_string(),
                    })),
            )
            .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 14.0, c.muted));
        let items = open.then(|| {
            div().px(px(14.0)).pb(px(10.0)).flex().flex_col().gap(px(6.0)).children(plan.items.iter().enumerate().map(|(i, item)| {
                let glyph: AnyElement = match item.state {
                    TaskState::Done => icon(Icon::Check, 16.0, green(&t)).into_any_element(),
                    TaskState::InProgress => Spinner::new(c.accent).into_any_element(),
                    TaskState::Pending => div().size(px(16.0)).flex().items_center().justify_center().child(div().size(px(8.0)).rounded_full().bg(c.surface_4)).into_any_element(),
                };
                div()
                    .id(("plan-item", i))
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(10.0))
                    .text_size(px(13.0))
                    .text_color(if item.state == TaskState::Done { c.muted } else { c.text })
                    .child(glyph)
                    .child(div().flex_1().min_w_0().child(SharedString::from(item.text.clone())))
            }))
        });
        div().w_full().rounded(px(Radius::MD)).bg(c.surface_1).flex().flex_col().child(header).children(items).into_any_element()
    }

    fn mention_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.mention.as_ref()?;
        let t = self.theme;
        let c = t.colors;
        let body: AnyElement = if m.items.is_empty() {
            let loading = self.state.project_root.as_ref().is_some_and(|r| self.hub.read(cx).files_for(r).is_none());
            div()
                .px(px(10.0))
                .py(px(8.0))
                .text_color(c.muted)
                .child(if loading { "Buscando archivos…" } else { "Sin coincidencias" })
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(m.items.iter().enumerate().map(|(i, path)| {
                    let name = file_name(path).to_string();
                    let dir = path.strip_suffix(name.as_str()).unwrap_or("").trim_end_matches('/').to_string();
                    let lead = crate::widgets::file_glyph(&name, c.muted);
                    let hint = (!dir.is_empty()).then(|| SharedString::from(dir));
                    menu_row(("mention", i), &t, Some(lead), name, hint, i == m.selected, true, cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.accept_mention(Some(i), cx);
                    }))
                }))
                .into_any_element()
        };
        Some(
            div()
                .absolute()
                .bottom(relative(1.0))
                .left(px(0.0))
                .mb(px(8.0))
                .w(px(420.0))
                .max_w_full()
                .p(px(6.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_2)
                .border_1()
                .border_color(c.outline_soft)
                .occlude()
                .child(body)
                .into_any_element(),
        )
    }

    fn permission_popover(&self, pill_open: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let mode = self.state.permission;
        let this = cx.entity();
        let mut pill = Pill::new("cmp-perm", &t)
            .label(permission_label(mode))
            .child(chevron(&t))
            .tooltip(if self.state.can_edit_permission { permission_hint(mode).to_string() } else { "No se puede cambiar mientras el agente trabaja".to_string() });
        if is_dangerous(mode) {
            pill = pill.icon(Icon::Alert);
        }
        if self.state.can_edit_permission {
            pill = pill.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Permission, cx)));
        }
        Popover::new("cmp-perm-pop", &t)
            .open(pill_open && self.state.can_edit_permission)
            .placement(Placement::TopEnd)
            .width(300.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(pill)
            .children(PERMISSION_ORDER.iter().map(|m| {
                let m = *m;
                let lead = icon(if is_dangerous(m) { Icon::Alert } else { Icon::Check }, 16.0, if is_dangerous(m) { amber(&t) } else { c.muted });
                menu_row(
                    ("perm", m as usize),
                    &t,
                    Some(lead.into_any_element()),
                    permission_label(m),
                    Some(permission_hint(m).into()),
                    m == mode,
                    true,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.popup = None;
                        if this.state.permission != m {
                            cx.emit(ComposerEvent::SetPermission(m));
                        }
                        cx.notify();
                    }),
                )
                .into_any_element()
            }))
            .into_any_element()
    }

    fn model_popover(&self, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let current = self.state.model.clone();
        let pill = Pill::new("cmp-model", &t)
            .label(model_label(current.as_deref()))
            .child(chevron(&t))
            .tooltip("Modelo")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Model, cx)));
        let known = models_for(&self.state.agent);
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            menu_row("model-default", &t, None, "Predeterminado del agente", None, current.is_none(), true, cx.listener(|this, _: &ClickEvent, _, cx| {
                this.popup = None;
                cx.emit(ComposerEvent::SetModel(None));
                cx.notify();
            }))
            .into_any_element(),
        );
        for (i, m) in known.iter().enumerate() {
            let id = m.id;
            rows.push(
                menu_row(("model", i), &t, None, m.label, Some(id.into()), current.as_deref() == Some(id), true, cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.popup = None;
                    cx.emit(ComposerEvent::SetModel(Some(id.to_string())));
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        if let Some(cur) = current.as_deref().filter(|m| !known.iter().any(|k| k.id == *m)) {
            rows.push(menu_row("model-current", &t, None, model_label(Some(cur)), Some(cur.to_string().into()), true, false, |_, _, _| {}).into_any_element());
        }
        rows.push(
            div()
                .mt(px(4.0))
                .mx(px(4.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(10.0))
                .bg(c.surface_3)
                .child(self.model_field.clone())
                .into_any_element(),
        );
        Popover::new("cmp-model-pop", &t)
            .open(open)
            .placement(Placement::TopEnd)
            .width(260.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(pill)
            .children(rows)
            .into_any_element()
    }

    fn agent_popover(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let cur = self.state.agents.iter().find(|a| a.kind == self.state.agent).cloned().unwrap_or_else(|| AgentOption::new(self.state.agent.clone()));
        let mut pill = Pill::new("cmp-agent", &t)
            .agent(cur.mark())
            .child(chevron(&t))
            .tooltip(format!("Agente: {}", cur.label))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Agent, cx)));
        if !compact {
            pill = pill.label(cur.label);
        }
        let open = matches!(self.popup, Some(Popup::Agent) | Some(Popup::Handoff { .. }));
        let body: Vec<AnyElement> = match &self.popup {
            Some(Popup::Handoff { agent, preview }) => {
                let label = self.state.agents.iter().find(|a| &a.kind == agent).map(|a| a.label.to_string()).unwrap_or_else(|| chat_core::agent_label(agent));
                let text: AnyElement = match preview {
                    Preview::Loading => div().text_color(c.muted).child("Preparando el resumen…").into_any_element(),
                    Preview::Empty => div().text_color(c.muted).child("Todavía no hay conversación que traspasar.").into_any_element(),
                    Preview::Text(s) => div()
                        .id("handoff-text")
                        .max_h(px(220.0))
                        .overflow_y_scroll()
                        .p(px(10.0))
                        .rounded(px(10.0))
                        .bg(c.surface_3)
                        .text_size(px(12.0))
                        .text_color(c.muted)
                        .child(SharedString::from(s.clone()))
                        .into_any_element(),
                };
                let target = agent.clone();
                let ready = !matches!(preview, Preview::Loading);
                vec![
                    div()
                        .px(px(6.0))
                        .pt(px(4.0))
                        .pb(px(8.0))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(div().font_weight(FontWeight::MEDIUM).child(format!("Cambiar a {label}")))
                        .child(div().text_size(px(12.0)).text_color(c.muted).child("El nuevo agente recibirá este resumen como primer mensaje:"))
                        .child(text)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(8.0))
                                .child(Button::tonal("handoff-cancel", &t, "Cancelar").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_popup(cx))))
                                .child(Button::primary("handoff-ok", &t, "Cambiar").disabled(!ready).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.confirm_switch(target.clone(), cx);
                                }))),
                        )
                        .into_any_element(),
                ]
            }
            _ => self
                .state
                .agents
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let kind = a.kind.clone();
                    let hint: Option<SharedString> = if !a.available {
                        Some("No instalado".into())
                    } else {
                        a.version.clone().map(Into::into)
                    };
                    menu_row(
                        ("agent", i),
                        &t,
                        Some(ui_kit::agent_icon(a.mark(), 16.0, c.muted).into_any_element()),
                        a.label.clone(),
                        hint,
                        a.kind == self.state.agent,
                        a.available,
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_agent(kind.clone(), cx)),
                    )
                    .into_any_element()
                })
                .collect(),
        };
        Popover::new("cmp-agent-pop", &t)
            .open(open)
            .placement(Placement::TopEnd)
            .width(if matches!(self.popup, Some(Popup::Handoff { .. })) { 380.0 } else { 240.0 })
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(pill)
            .children(body)
            .into_any_element()
    }

    fn plus_popover(&self, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let btn = IconButton::new("cmp-plus", &t, Icon::Plus)
            .size(32.0)
            .emphasize_on_hover(true)
            .tooltip("Adjuntar o mencionar archivos")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Plus, cx)));
        Popover::new("cmp-plus-pop", &t)
            .open(open)
            .placement(Placement::TopStart)
            .width(260.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(btn)
            .child(menu_row("plus-attach", &t, Some(icon(Icon::Files, 16.0, c.muted).into_any_element()), "Adjuntar archivos…", Some("Se envían como rutas en el mensaje".into()), false, true, cx.listener(|this, _: &ClickEvent, _, cx| this.pick_files(cx))))
            .child(menu_row("plus-mention", &t, Some(icon(Icon::Search, 16.0, c.muted).into_any_element()), "Mencionar archivo", Some("Escribe @ y elige del proyecto".into()), false, self.state.project_root.is_some(), cx.listener(|this, _: &ClickEvent, _, cx| this.insert_at_sign(cx))))
            .into_any_element()
    }

    fn round_button(&self, mode: ButtonMode, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let (bg, fg) = match mode {
            ButtonMode::Disabled => (c.surface_3, c.muted),
            _ => (c.text, c.bg),
        };
        let inner: AnyElement = match mode {
            // TODO-SVG: `assets/icons/stop.svg` (16x16, currentColor). Until it exists the stop mark
            // is a plain rounded square.
            ButtonMode::Stop => div().size(px(11.0)).rounded(px(2.5)).bg(fg).into_any_element(),
            _ => icon(Icon::Send, 16.0, fg).into_any_element(),
        };
        let tip = match mode {
            ButtonMode::Stop => "Detener",
            ButtonMode::Send => "Enviar (Enter)",
            ButtonMode::Disabled => "Escribe un mensaje",
        };
        div()
            .id("cmp-send")
            .occlude()
            .size(px(34.0))
            .flex_none()
            .rounded_full()
            .bg(bg)
            .flex()
            .items_center()
            .justify_center()
            .when(mode != ButtonMode::Disabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.88)))
            .tooltip(Tooltip::text(&t, tip))
            .child(inner)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.press_round_button(cx)))
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let compact = f32::from(window.viewport_size().width) < 760.0;
        let mode = self.button_mode(cx);
        let popup = self.popup.clone();

        let chips = (!self.attachments.is_empty()).then(|| {
            div().w_full().flex().flex_row().flex_wrap().gap(px(6.0)).pb(px(8.0)).children(self.attachments.iter().enumerate().map(|(i, p)| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string());
                file_chip(&t, &name, c.text).id(("attach", i)).tooltip(Tooltip::text(&t, p.display().to_string())).child(
                    div()
                        .id(("attach-x", i))
                        .size(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(move |s| s.bg(c.surface_4))
                        .child(icon(Icon::Close, 10.0, c.muted))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if i < this.attachments.len() {
                                this.attachments.remove(i);
                            }
                            cx.notify();
                        })),
                )
            }))
        });

        let controls = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(self.plus_popover(matches!(popup, Some(Popup::Plus)), cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap(px(2.0))
                    .children(context_meter(&t, self.state.context, compact))
                    .child(self.permission_popover(matches!(popup, Some(Popup::Permission)), cx))
                    .child(self.model_popover(matches!(popup, Some(Popup::Model)), cx))
                    .child(self.agent_popover(compact, cx))
                    .child(div().w(px(6.0)).h(px(1.0)))
                    .child(self.round_button(mode, cx)),
            );

        let card = div()
            .relative()
            .w_full()
            .rounded(px(Radius::LG))
            .bg(c.surface_1)
            .px(px(16.0))
            .pt(px(14.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(chips)
            .child(self.field.clone())
            .child(controls)
            .children(self.mention_overlay(cx));

        let path_line = (self.state.branch.is_some() || self.state.path.is_some()).then(|| {
            div()
                .w_full()
                .px(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .font_family(ty.ui_family)
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .when_some(self.state.branch.clone(), |d, b| {
                    d.child(icon(Icon::GitBranch, 14.0, c.muted)).child(div().flex_none().child(SharedString::from(b)))
                })
                .when_some(self.state.path.clone(), |d, p| {
                    d.child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(SharedString::from(tail_path(&p, 3))))
                })
        });

        let plan = self.state.plan.clone().filter(|p| !p.items.is_empty()).map(|p| self.plan_bar(&p, cx));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .font_family(ty.ui_family)
            .children(plan)
            .child(card)
            .children(path_line)
    }
}
