//! Painting of the conversation rows (`ChatRow` -> gpui elements). The rows are the view-model of
//! [`crate::rows`]; this file only decides how each kind looks.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use chat_core::{ApprovalDecision, ApprovalState, CostSource, ToolClass, ToolStatus};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::*, px,
    relative,
};
use theme::Radius;
use ui_kit::{Button, Icon, Spinner, Tooltip, icon};

use crate::chat_view::ChatView;
use crate::md::render_md;
use crate::rows::{ChatRow, RowKey, RowKind, approval_outcome};
use crate::util::{file_name, format_cost, format_duration_ms, format_tokens};
use crate::widgets::{amber, file_chip, file_glyph, green, red, tool_line, working_bars};

pub(crate) fn row_id(key: &RowKey) -> usize {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish() as usize
}

impl ChatView {
    /// The list item of row `ix`, centered in the reading column.
    pub(crate) fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let expanded = self.expanded.contains(&row.key);
        let last = ix + 1 == self.rows.len();
        let body = self.row_body(&row, expanded, cx);
        let col_max = self.column_max();
        div()
            .w_full()
            .flex()
            .flex_row()
            .justify_center()
            .px(px(24.0))
            .when(ix == 0, |d| d.pt(px(18.0)))
            .when(last, |d| d.pb(px(20.0)))
            .child(div().w_full().min_w_0().when_some(col_max, |d, w| d.max_w(px(w))).child(body))
            .into_any_element()
    }

    fn row_body(&mut self, row: &Rc<ChatRow>, expanded: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let id = row_id(&row.key);
        let key = row.key.clone();
        let handler = |cx: &mut Context<Self>| -> Option<crate::widgets::Handler> {
            let key = key.clone();
            let f = cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(&key, cx));
            Some(Box::new(f))
        };
        match &row.kind {
            RowKind::User { text, attachments } => div()
                .w_full()
                .pt(px(14.0))
                .pb(px(6.0))
                .flex()
                .flex_row()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.82))
                        .px(px(16.0))
                        .py(px(11.0))
                        .rounded(px(Radius::MD))
                        .bg(c.surface_2)
                        .font_family(ty.ui_family)
                        .text_size(px(14.5))
                        .line_height(relative(1.55))
                        .text_color(c.text)
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(div().w_full().child(SharedString::from(text.clone())))
                        .when(!attachments.is_empty(), |d| {
                            d.child(div().flex().flex_row().flex_wrap().gap(px(6.0)).children(attachments.iter().map(|p| {
                                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                file_chip(&t, &name, c.muted)
                            })))
                        }),
                )
                .into_any_element(),
            RowKind::Checkpoint { id: cp, label } => {
                let cp = cp.clone();
                let thread = self.thread_id();
                div()
                    .w_full()
                    .py(px(4.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(12.5))
                    .text_color(c.muted)
                    .child(icon(Icon::History, 16.0, c.muted))
                    .child(format!("Checkpoint · {label}"))
                    .child(
                        Button::ghost(format!("restore-{id}"), &t, "Restaurar…")
                            .tooltip("Vuelve el repositorio a este punto (te pedirá confirmación)")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(thread) = thread {
                                    let ev = crate::hub::ChatEvent::RestoreCheckpoint { thread, id: cp.clone() };
                                    this.hub.update(cx, |h, cx| h.emit_event(ev, cx));
                                }
                            })),
                    )
                    .into_any_element()
            }
            RowKind::Text { body, .. } => div()
                .w_full()
                .py(px(2.0))
                .font_family(ty.ui_family)
                .child(render_md(body.parsed(), t, id))
                .into_any_element(),
            RowKind::Thinking { text, streaming } => {
                let lead: AnyElement = if *streaming { Spinner::new(c.accent).into_any_element() } else { icon(Icon::Agents, 16.0, c.muted).into_any_element() };
                let label = if *streaming { "Pensando…" } else { "Pensamiento" };
                let line = tool_line(("think", id), &t, lead, div().child(label).into_any_element(), None, (!text.trim().is_empty()).then_some(expanded), handler(cx));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(line)
                    .when(expanded, |d| {
                        d.child(
                            div()
                                .my(px(4.0))
                                .px(px(14.0))
                                .py(px(10.0))
                                .rounded(px(12.0))
                                .bg(c.surface_1)
                                .font_family(ty.ui_family)
                                .text_size(px(13.0))
                                .line_height(relative(1.55))
                                .text_color(c.muted)
                                .child(SharedString::from(text.clone())),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Explored { label, items, running } => {
                let lead: AnyElement = if *running { Spinner::new(c.accent).into_any_element() } else { icon(Icon::Search, 16.0, c.muted).into_any_element() };
                let line = tool_line(("explored", id), &t, lead, div().child(SharedString::from(label.clone())).into_any_element(), None, Some(expanded), handler(cx));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(line)
                    .when(expanded, |d| {
                        d.child(div().ml(px(24.0)).mb(px(4.0)).flex().flex_col().gap(px(2.0)).children(items.iter().map(|it| {
                            let glyph: AnyElement = match it.class {
                                ToolClass::List => icon(Icon::Folder, 14.0, c.muted).into_any_element(),
                                ToolClass::Search => icon(Icon::Search, 14.0, c.muted).into_any_element(),
                                _ => file_glyph(it.label.rsplit(' ').next().unwrap_or(""), c.muted),
                            };
                            div()
                                .h(px(24.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_size(px(12.5))
                                .text_color(c.muted)
                                .child(glyph)
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(SharedString::from(it.label.clone())))
                        })))
                    })
                    .into_any_element()
            }
            RowKind::Command { cmd, status, duration_ms, output } => {
                let (verb, lead): (&str, AnyElement) = match status {
                    ToolStatus::Running => ("Running", Spinner::new(c.accent).into_any_element()),
                    ToolStatus::Ok => ("Ran", icon(Icon::Terminal, 16.0, c.muted).into_any_element()),
                    ToolStatus::Failed => ("Failed", icon(Icon::Terminal, 16.0, red(&t)).into_any_element()),
                    ToolStatus::Cancelled => ("Cancelled", icon(Icon::Terminal, 16.0, c.muted).into_any_element()),
                };
                let label = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_none().text_color(if *status == ToolStatus::Failed { red(&t) } else { c.muted }).child(verb))
                    .child(
                        div()
                            .max_w(px(560.0))
                            .px(px(7.0))
                            .py(px(1.0))
                            .rounded(px(6.0))
                            .bg(c.surface_2)
                            .font_family(ty.mono_family)
                            .text_size(px(12.0))
                            .text_color(c.text)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(cmd.lines().next().unwrap_or("").to_string())),
                    )
                    .into_any_element();
                let meta = duration_ms.map(|d| div().flex_none().text_size(px(12.0)).text_color(c.outline).child(format_duration_ms(d)).into_any_element());
                let line = tool_line(("cmd", id), &t, lead, label, meta, Some(expanded), handler(cx));
                let thread = self.thread_id();
                let cwd = self.cwd();
                let cmd_full = cmd.clone();
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(line)
                    .when(expanded, |d| {
                        d.child(
                            div()
                                .my(px(4.0))
                                .ml(px(24.0))
                                .rounded(px(12.0))
                                .bg(c.surface_1)
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .id(("cmd-out", id))
                                        .max_h(px(220.0))
                                        .overflow_y_scroll()
                                        .px(px(14.0))
                                        .pt(px(10.0))
                                        .pb(px(6.0))
                                        .font_family(ty.mono_family)
                                        .text_size(px(12.0))
                                        .line_height(relative(1.45))
                                        .text_color(c.muted)
                                        .child(SharedString::from(
                                            output.clone().unwrap_or_else(|| if *status == ToolStatus::Running { "…".into() } else { "(sin salida)".into() }),
                                        )),
                                )
                                .child(
                                    div().px(px(8.0)).pb(px(8.0)).flex().flex_row().child(
                                        Button::ghost(format!("open-term-{id}"), &t, "Abrir en terminal")
                                            .icon(Icon::Terminal)
                                            .tooltip("Muestra el comando con su salida real en una pestaña de terminal")
                                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                                if let Some(thread) = thread {
                                                    let ev = crate::hub::ChatEvent::OpenInTerminal {
                                                        thread,
                                                        command: cmd_full.clone(),
                                                        cwd: cwd.clone(),
                                                    };
                                                    this.hub.update(cx, |h, cx| h.emit_event(ev, cx));
                                                }
                                            })),
                                    ),
                                ),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Edit { verb, path, title, adds, dels, status, summary } => {
                let shown = path.clone().unwrap_or_else(|| title.clone());
                let name = file_name(&shown).to_string();
                let lead: AnyElement = match status {
                    ToolStatus::Running => Spinner::new(c.accent).into_any_element(),
                    ToolStatus::Failed => icon(Icon::Alert, 16.0, red(&t)).into_any_element(),
                    _ => icon(Icon::Edit, 16.0, c.muted).into_any_element(),
                };
                let opens = path.clone();
                let thread = self.thread_id();
                let cwd = self.cwd();
                let chip = file_chip(&t, &name, c.text)
                    .id(("edit-chip", id))
                    .cursor_pointer()
                    .hover(move |s| s.bg(c.surface_4))
                    .tooltip(Tooltip::text(&t, format!("{shown}\nAbrir con su diff")))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let (Some(thread), Some(p)) = (thread, opens.clone()) {
                            let ev = crate::hub::ChatEvent::OpenDiff { thread, path: p, cwd: cwd.clone() };
                            this.hub.update(cx, |h, cx| h.emit_event(ev, cx));
                        }
                    }));
                let label = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_none().child(*verb))
                    .child(chip)
                    .when_some(*adds, |d, a| d.child(div().flex_none().text_size(px(12.0)).text_color(green(&t)).child(format!("+{a}"))))
                    .when_some(*dels, |d, x| d.child(div().flex_none().text_size(px(12.0)).text_color(red(&t)).child(format!("−{x}"))))
                    .into_any_element();
                let expandable = summary.is_some();
                let line = tool_line(("edit", id), &t, lead, label, None, expandable.then_some(expanded), if expandable { handler(cx) } else { None });
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(line)
                    .when(expanded && expandable, |d| {
                        d.child(
                            div()
                                .my(px(4.0))
                                .ml(px(24.0))
                                .px(px(14.0))
                                .py(px(10.0))
                                .rounded(px(12.0))
                                .bg(c.surface_1)
                                .font_family(ty.mono_family)
                                .text_size(px(12.0))
                                .text_color(c.muted)
                                .child(SharedString::from(summary.clone().unwrap_or_default())),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Tool { title, class, status, duration_ms, summary } => {
                let lead: AnyElement = match status {
                    ToolStatus::Running => Spinner::new(c.accent).into_any_element(),
                    ToolStatus::Failed => icon(Icon::Alert, 16.0, red(&t)).into_any_element(),
                    _ => icon(
                        match class {
                            ToolClass::Web => Icon::Search,
                            ToolClass::Task => Icon::Task,
                            _ => Icon::Snippet,
                        },
                        16.0,
                        c.muted,
                    )
                    .into_any_element(),
                };
                let meta = duration_ms.map(|d| div().flex_none().text_size(px(12.0)).text_color(c.outline).child(format_duration_ms(d)).into_any_element());
                let expandable = summary.is_some();
                let line = tool_line(
                    ("tool", id),
                    &t,
                    lead,
                    div().max_w(px(640.0)).overflow_hidden().text_ellipsis().child(SharedString::from(title.clone())).into_any_element(),
                    meta,
                    expandable.then_some(expanded),
                    if expandable { handler(cx) } else { None },
                );
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(line)
                    .when(expanded && expandable, |d| {
                        d.child(
                            div()
                                .my(px(4.0))
                                .ml(px(24.0))
                                .px(px(14.0))
                                .py(px(10.0))
                                .rounded(px(12.0))
                                .bg(c.surface_1)
                                .text_size(px(12.5))
                                .text_color(c.muted)
                                .child(SharedString::from(summary.clone().unwrap_or_default())),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Approval { request_id, tool, detail, state } => {
                if *state == ApprovalState::Pending {
                    let (a, d, s) = (request_id.clone(), request_id.clone(), request_id.clone());
                    div()
                        .w_full()
                        .my(px(8.0))
                        .p(px(14.0))
                        .rounded(px(Radius::MD))
                        .bg(c.surface_1)
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_size(px(13.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(c.text)
                                .child(icon(Icon::Alert, 16.0, amber(&t)))
                                .child(SharedString::from(format!("El agente pide permiso: {tool}"))),
                        )
                        .when(!detail.trim().is_empty(), |d| {
                            d.child(
                                div()
                                    .max_h(px(120.0))
                                    .overflow_hidden()
                                    .px(px(12.0))
                                    .py(px(8.0))
                                    .rounded(px(10.0))
                                    .bg(c.surface_2)
                                    .font_family(ty.mono_family)
                                    .text_size(px(12.0))
                                    .text_color(c.muted)
                                    .child(SharedString::from(detail.clone())),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(Button::action(format!("approve-{id}"), &t, "Permitir").icon(Icon::Check).on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| this.approve(&a, ApprovalDecision::Allow, cx),
                                )))
                                .child(Button::tonal(format!("deny-{id}"), &t, "Rechazar").on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| this.approve(&d, ApprovalDecision::Deny, cx),
                                )))
                                .child(
                                    Button::ghost(format!("always-{id}"), &t, "Siempre")
                                        .tooltip("Permitir y no volver a preguntar por esta herramienta en este hilo")
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.approve(&s, ApprovalDecision::Always, cx))),
                                ),
                        )
                        .into_any_element()
                } else {
                    let ok = matches!(state, ApprovalState::Allowed | ApprovalState::AlwaysAllowed);
                    div()
                        .w_full()
                        .h(px(28.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(13.0))
                        .text_color(c.muted)
                        .child(icon(if ok { Icon::Check } else { Icon::Close }, 16.0, if ok { green(&t) } else { c.muted }))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(SharedString::from(format!(
                                    "{} · {tool}{}",
                                    approval_outcome(*state),
                                    if detail.trim().is_empty() { String::new() } else { format!(" {}", detail.lines().next().unwrap_or("")) }
                                ))),
                        )
                        .into_any_element()
                }
            }
            RowKind::System { text } => div()
                .w_full()
                .py(px(4.0))
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.0))
                .text_size(px(12.5))
                .line_height(relative(1.5))
                .text_color(c.muted)
                .child(div().pt(px(1.0)).child(icon(Icon::Dots, 16.0, c.muted)))
                .child(div().flex_1().min_w_0().child(SharedString::from(text.clone())))
                .into_any_element(),
            RowKind::Footer { cost, tokens } => {
                let mut parts = Vec::new();
                let mut tip = Vec::new();
                if let Some(cost) = cost {
                    let src = match cost.source {
                        CostSource::Reported => "reportado",
                        CostSource::Estimated => "estimado",
                    };
                    parts.push(format_cost(cost.usd));
                    tip.push(format!("{} · {src}", format_cost(cost.usd)));
                }
                if *tokens > 0 {
                    parts.push(format!("{} tokens", format_tokens(*tokens)));
                    tip.push(format!("{} tokens en este mensaje", format_tokens(*tokens)));
                }
                div()
                    .id(("footer", id))
                    .w_full()
                    .h(px(22.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_size(px(11.5))
                    .text_color(c.outline)
                    .child(div().child(parts.join(" · ")))
                    .tooltip(Tooltip::text(&t, tip.join("\n")))
                    .into_any_element()
            }
            RowKind::Working => div()
                .w_full()
                .h(px(32.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_size(px(13.0))
                .text_color(c.muted)
                .child(working_bars(&t))
                .child("Working…")
                .into_any_element(),
            RowKind::Error { text } => div()
                .w_full()
                .my(px(6.0))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_1)
                .flex()
                .flex_row()
                .items_start()
                .gap(px(10.0))
                .text_size(px(13.0))
                .line_height(relative(1.5))
                .text_color(c.error)
                .child(div().pt(px(1.0)).child(icon(Icon::Alert, 16.0, c.error)))
                .child(div().flex_1().min_w_0().child(SharedString::from(text.clone())))
                .into_any_element(),
        }
    }
}
