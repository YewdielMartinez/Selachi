//! The cards that ask the user about an agent's request (a delegated child asking to run a tool, or
//! `apply_changes` asking to bring a worktree's changes into the project).
//!
//! Pure view code: the shell passes the pending requests and closures; deciding is the buttons'
//! job. A card never decides anything by itself and has no timer (the server denies on its own when
//! nobody answers; the card goes away with the `Removed` event).
//!
//! * Tool request: **Permitir** / **Siempre** (the rest of that run) / **Rechazar**, and a quiet
//!   **No volver a preguntar en esta ejecución** (`Decision::AlwaysRun`: that run switches to the
//!   `auto` approval mode; the harness' hard blocklist still denies).
//! * `apply_changes`: the dry-run plan (files, stat, warnings) and **Aplicar**, the one
//!   `ActionButton`; plans that cannot apply cleanly or carry a full-risk reason offer only
//!   **Rechazar**.

use agent_harness::Decision;
use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::*, px,
};
use theme::{Radius, Theme};
use ui_kit::{ActionButton, AgentMark, Button, Card, CardTone, agent_icon};

use crate::approval_queue::{PendingApproval, PlanView};
use crate::notify::clip;

/// Width of a card.
pub const CARD_W: f32 = 392.0;
/// Cards painted at once; the rest wait in the attention queue.
pub const MAX_VISIBLE: usize = 3;
/// Files listed in an apply plan.
const MAX_PLAN_FILES: usize = 7;

/// "Codex pide aprobación" / "Codex quiere aplicar cambios".
pub fn card_title(agent: &str, apply: bool) -> String {
    let name = display_agent(agent);
    if apply {
        format!("{name}: aplicar cambios al proyecto")
    } else {
        format!("{name} pide tu aprobación")
    }
}

/// `codex` -> `Codex`, `claude` -> `Claude Code`.
pub fn display_agent(agent: &str) -> String {
    match agent.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "opencode" => "OpenCode".into(),
        "gemini" => "Gemini".into(),
        "agy" | "antigravity" => "Antigravity".into(),
        "" => "Un agente".into(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

/// "2 min" / "30 s" of the wait the agent granted.
pub fn wait_label(secs: u64) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else if secs >= 60 {
        format!("{} min {} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

/// What a card calls when the user decides (`request_id`, decision).
pub type DecideFn = std::rc::Rc<dyn Fn(String, Decision, &mut Window, &mut App)>;
/// What a card calls to open the child thread.
pub type OpenThreadFn = std::rc::Rc<dyn Fn(i64, &mut Window, &mut App)>;

/// What a card needs besides the request itself.
pub struct CardCtx {
    pub theme: Theme,
    /// Title of the child thread, when the hub knows it.
    pub thread_title: Option<String>,
    pub on_decide: DecideFn,
    pub on_open_thread: OpenThreadFn,
}

fn chip(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex_none()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(Radius::CHIP))
        .bg(c.surface_3)
        .text_size(px(ty.ui_size_xs))
        .text_color(c.muted)
        .child(text.into())
        .into_any_element()
}

fn plan_block(theme: &Theme, plan: &PlanView) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    let mut col = div().flex().flex_col().gap(px(3.0));
    for (change, path) in plan.files.iter().take(MAX_PLAN_FILES) {
        let word = match change.as_str() {
            "added" => "nuevo",
            "deleted" => "borrado",
            "type_changed" => "tipo",
            _ => "editado",
        };
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(chip(theme, word))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(ty.mono_family)
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.text)
                        .child(SharedString::from(path.clone())),
                ),
        );
    }
    let more = plan.files.len().saturating_sub(MAX_PLAN_FILES);
    if more > 0 {
        col = col.child(
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.outline)
                .child(SharedString::from(format!("… y {more} más"))),
        );
    }
    if let Some(summary) = plan.stat.last() {
        col = col.child(
            div()
                .pt(px(2.0))
                .font_family(ty.mono_family)
                .text_size(px(ty.ui_size_xs))
                .text_color(c.muted)
                .child(SharedString::from(summary.clone())),
        );
    }
    for w in &plan.warnings {
        col = col.child(
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.warning)
                .child(SharedString::from(clip(w, 140))),
        );
    }
    if let Some(b) = &plan.blocked {
        col = col.child(
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.error)
                .child(SharedString::from(format!(
                    "No se puede aplicar: {}",
                    clip(b, 120)
                ))),
        );
    }
    for r in &plan.risks {
        col = col.child(
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.error)
                .child(SharedString::from(format!("Riesgo alto: {}", clip(r, 120)))),
        );
    }
    col.into_any_element()
}

/// One card.
pub fn approval_card(p: &PendingApproval, ctx: &CardCtx) -> AnyElement {
    let theme = &ctx.theme;
    let c = theme.colors;
    let ty = theme.typography;
    let req = &p.request;
    let id = req.request_id.clone();
    let apply = p.is_apply();
    let mark = AgentMark::from_name(&req.agent);
    let title = card_title(&req.agent, apply);
    let thread_line = match (&ctx.thread_title, req.depth) {
        (Some(t), _) => format!("Hilo delegado: {t}"),
        (None, _) => format!("Ejecución {}", req.run_id),
    };

    let body: AnyElement = if let Some(plan) = p.plan.as_ref().filter(|_| apply) {
        let mut col = div().flex().flex_col().gap(px(6.0)).child(
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(SharedString::from(format!(
                    "{} archivo{} → {}",
                    plan.files.len(),
                    if plan.files.len() == 1 { "" } else { "s" },
                    clip(&plan.target, 44)
                ))),
        );
        col = col.child(plan_block(theme, plan));
        col.into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(div().flex().flex_row().child(chip(theme, req.tool.clone())))
            .when(!req.detail.trim().is_empty(), |d| {
                d.child(
                    div()
                        .max_h(px(92.0))
                        .overflow_hidden()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(Radius::CONTROL))
                        .bg(c.surface_2)
                        .font_family(ty.mono_family)
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.text)
                        .child(SharedString::from(req.detail.clone())),
                )
            })
            .into_any_element()
    };

    let (decide_deny, decide_allow, decide_always, decide_apply, decide_run, open) = (
        ctx.on_decide.clone(),
        ctx.on_decide.clone(),
        ctx.on_decide.clone(),
        ctx.on_decide.clone(),
        ctx.on_decide.clone(),
        ctx.on_open_thread.clone(),
    );
    let (id_deny, id_allow, id_always, id_apply, id_run) = (
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
    );
    let thread = req.thread_id;
    let can_apply = p
        .plan
        .as_ref()
        .is_none_or(|pl| pl.blocked.is_none() && pl.risks.is_empty());
    let mut buttons = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .gap(px(8.0))
        .child(
            Button::ghost(format!("appr-open-{id}"), theme, "Ver hilo")
                .on_click(move |_, w, cx| open(thread, w, cx)),
        )
        .child(div().flex_1())
        .child(
            Button::ghost(format!("appr-deny-{id}"), theme, "Rechazar")
                .on_click(move |_, w, cx| decide_deny(id_deny.clone(), Decision::Deny, w, cx)),
        );
    if apply {
        if can_apply {
            buttons = buttons.child(
                ActionButton::new(format!("appr-apply-{id}"), theme, "Aplicar")
                    .icon(ui_kit::Icon::Check)
                    .on_click(move |_, w, cx| {
                        decide_apply(id_apply.clone(), Decision::Allow, w, cx)
                    }),
            );
        }
    } else {
        buttons = buttons
            .child(
                Button::ghost(format!("appr-always-{id}"), theme, "Siempre").on_click(
                    move |_, w, cx| decide_always(id_always.clone(), Decision::Always, w, cx),
                ),
            )
            .child(
                Button::tonal(format!("appr-allow-{id}"), theme, "Permitir").on_click(
                    move |_, w, cx| decide_allow(id_allow.clone(), Decision::Allow, w, cx),
                ),
            );
    }

    Card::new(theme)
        .tone(CardTone::High, theme)
        .radius(Radius::MD)
        .p(px(14.0))
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(agent_icon(mark, 16.0, c.muted))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(ty.ui_size_body))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c.text)
                        .child(SharedString::from(title)),
                ),
        )
        .child(
            div()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(SharedString::from(thread_line)),
        )
        .child(body)
        .child(buttons)
        .when(!apply, |card| {
            card.child(
                div().flex().flex_row().child(
                    Button::ghost(
                        format!("appr-always-run-{id}"),
                        theme,
                        "No volver a preguntar en esta ejecución",
                    )
                    .on_click(move |_, w, cx| {
                        decide_run(id_run.clone(), Decision::AlwaysRun, w, cx)
                    }),
                ),
            )
        })
        .child(
            div()
                .text_size(px(ty.ui_size_xs))
                .text_color(c.outline)
                .child(SharedString::from(format!(
                    "Si no respondes en {} se rechaza",
                    wait_label(req.timeout_secs)
                ))),
        )
        .into_any_element()
}

/// The stack of cards (newest on top), anchored bottom-right by the caller.
pub fn approval_stack(
    pending: &[PendingApproval],
    ctx_for: impl Fn(&PendingApproval) -> CardCtx,
    theme: &Theme,
) -> Option<AnyElement> {
    if pending.is_empty() {
        return None;
    }
    let c = theme.colors;
    let ty = theme.typography;
    let mut col = div().flex().flex_col().gap(px(8.0)).items_end();
    let hidden = pending.len().saturating_sub(MAX_VISIBLE);
    for p in pending.iter().rev().take(MAX_VISIBLE) {
        col = col.child(approval_card(p, &ctx_for(p)));
    }
    if hidden > 0 {
        col = col.child(
            div()
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(Radius::PILL))
                .bg(c.surface_2)
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(SharedString::from(format!(
                    "+{hidden} más en la cola de atención"
                ))),
        );
    }
    Some(col.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_waits_read_naturally() {
        assert_eq!(card_title("codex", false), "Codex pide tu aprobación");
        assert_eq!(
            card_title("claude", true),
            "Claude Code: aplicar cambios al proyecto"
        );
        assert_eq!(display_agent("agy"), "Antigravity");
        assert_eq!(display_agent("foo"), "Foo");
        assert_eq!(display_agent(""), "Un agente");
        assert_eq!(wait_label(120), "2 min");
        assert_eq!(wait_label(90), "1 min 30 s");
        assert_eq!(wait_label(30), "30 s");
    }
}
