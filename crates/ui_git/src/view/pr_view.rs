//! The pull request pill, the "Pull request" section (PR, checks, "Enviar checks fallidos al
//! agente") and the "Crear PR" form with its confirmation card (backlog B1 + B3).
//!
//! Everything here is hidden while `gh` is missing or not logged in (`PrView::Unavailable`) or
//! the integration is off (`PrView::Idle`): the panel looks exactly as before.

use git_svc::{
    Check, CheckBucket, DraftKind, NewPr, PrInfo, PrState, ReviewDecision, split_message,
};
use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use theme::{Radius, Theme};
use ui_kit::{Button, GhostButton, Icon, Spinner, Tooltip, icon};

use super::GitPanel;
use crate::fmt::{plural, truncate_end};
use crate::pr::{
    PrCreate, PrSnapshot, PrSummary, PrTone, PrView, check_glyph, display_command, format_duration,
    summarize,
};
use crate::widgets::{card_shell, empty_state, mini_button, section_header, tag};

/// Checks listed before "Mostrar todos".
const CHECKS_SHOWN: usize = 10;

fn tone_color(theme: &Theme, tone: PrTone) -> gpui::Rgba {
    let c = theme.colors;
    match tone {
        PrTone::Good => c.success,
        PrTone::Bad => c.error,
        PrTone::Pending => c.warning,
        PrTone::Neutral => c.muted,
    }
}

/// The PR pill (`PR #12 · 2 checks fallan`): a tone dot, the summary and a click that opens the
/// PR in the browser. Public so the app's status bar can show the same element.
pub fn pr_pill(id: impl Into<SharedString>, theme: &Theme, summary: &PrSummary) -> AnyElement {
    let c = theme.colors;
    let url = summary.url.clone();
    div()
        .id(id.into())
        .occlude()
        .h(px(28.0))
        .min_w_0()
        .px(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(Radius::PILL))
        .bg(c.surface_2)
        .cursor_pointer()
        .hover(move |s| s.bg(c.surface_3))
        .tooltip(Tooltip::text(theme, "Abrir el pull request en GitHub"))
        .on_click(move |_, _, cx: &mut App| {
            if !url.is_empty() {
                cx.open_url(&url);
            }
        })
        .child(
            div()
                .size(px(7.0))
                .flex_none()
                .rounded_full()
                .bg(tone_color(theme, summary.tone)),
        )
        .child(
            div()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_size(px(theme.typography.ui_size_md))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c.text)
                .child(summary.text.clone()),
        )
        .into_any_element()
}

fn bucket_rank(b: CheckBucket) -> u8 {
    match b {
        CheckBucket::Fail => 0,
        CheckBucket::Pending => 1,
        CheckBucket::Pass => 2,
        CheckBucket::Cancel => 3,
        CheckBucket::Skipping => 4,
    }
}

fn state_label(pr: &PrInfo) -> &'static str {
    match pr.state {
        PrState::Open => "Abierto",
        PrState::Closed => "Cerrado",
        PrState::Merged => "Mergeado",
    }
}

fn review_label(d: ReviewDecision) -> &'static str {
    match d {
        ReviewDecision::Approved => "Aprobado",
        ReviewDecision::ChangesRequested => "Cambios solicitados",
        ReviewDecision::ReviewRequired => "Revisión pendiente",
    }
}

impl GitPanel {
    /// Opens the "Crear PR" form (the host's demo / tests drive it).
    pub fn open_pr_form(&mut self, cx: &mut Context<Self>) {
        if !self.pr_form {
            self.pr_form = true;
            cx.notify();
        }
    }

    /// The tab list: the PR tab only exists when there is something to show in it.
    pub(super) fn pr_available(&self, cx: &App) -> bool {
        self.model.read(cx).pr_view().is_visible()
    }

    /// The pill of the header, when the branch has a PR.
    pub(super) fn pr_header_pill(&self, cx: &App) -> Option<AnyElement> {
        let summary = self.model.read(cx).pr_summary()?;
        Some(pr_pill("git-pr-pill", &self.ctx.theme, &summary))
    }

    /// A PR draft arrived (or a commit draft): fill the fields. Editable before confirming.
    pub(super) fn apply_draft(&mut self, kind: DraftKind, cx: &mut Context<Self>) {
        let Some(text) = self.model.update(cx, |m, _| m.take_draft(kind)) else {
            return;
        };
        let (subject, body) = split_message(&text);
        match kind {
            DraftKind::Commit => self.set_message(&subject, &body, cx),
            DraftKind::PullRequest => {
                self.pr_title.update(cx, |f, cx| f.set_text(&subject, cx));
                self.pr_body.update(cx, |f, cx| f.set_text(&body, cx));
                self.pr_form = true;
                if self.is_compact() {
                    self.tab = super::Tab::Pr;
                }
            }
        }
        cx.notify();
    }

    fn review_pr(&mut self, cx: &mut Context<Self>) {
        let pr = NewPr {
            title: self.pr_title.read(cx).text().trim().to_string(),
            body: self.pr_body.read(cx).text().trim().to_string(),
            draft: self.pr_draft,
            base: None,
            head: None,
        };
        self.model.update(cx, |m, cx| m.request_create_pr(pr, cx));
    }

    /// "Redactar con agente" button (commit box and PR form).
    pub(super) fn draft_button(
        &self,
        id: &'static str,
        kind: DraftKind,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let drafting = self.model.read(cx).drafting();
        if drafting == Some(kind) {
            let model = self.model.clone();
            return div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(Spinner::new(c.accent))
                .child(
                    div()
                        .text_size(px(theme.typography.ui_size_sm))
                        .text_color(c.muted)
                        .child("Redactando con el agente…"),
                )
                .child(
                    GhostButton::new(
                        SharedString::from(format!("{id}-cancel")),
                        &theme,
                        "Cancelar",
                    )
                    .on_click(move |_, _, cx| model.update(cx, |m, cx| m.cancel_draft(cx))),
                )
                .into_any_element();
        }
        let model = self.model.clone();
        Button::tonal(id, &theme, "Redactar con agente")
            .icon(Icon::Edit)
            .disabled(!enabled || drafting.is_some())
            .tooltip(match kind {
                DraftKind::Commit => {
                    "El agente redacta el mensaje desde el diff en staged (gasta tokens solo al pulsar)"
                }
                DraftKind::PullRequest => {
                    "El agente redacta título y descripción desde los cambios de la rama (gasta tokens solo al pulsar)"
                }
            })
            .on_click(move |_, _, cx| model.update(cx, |m, cx| m.request_draft(kind, cx)))
            .into_any_element()
    }

    /// The "Pull request" section. Empty while the integration has nothing to show.
    pub(super) fn pr_block(
        &mut self,
        with_header: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let view = self.model.read(cx).pr_view().clone();
        if !view.is_visible() {
            return Vec::new();
        }
        let key = "pr";
        let collapsed = self.collapsed.contains(key) && with_header;
        let loading = self.model.read(cx).pr_loading();
        let refresh = mini_button(
            "git-pr-refresh",
            &theme,
            "↻",
            "Actualizar el estado del PR y del CI",
            {
                let model = self.model.clone();
                move |_, _, cx| model.update(cx, |m, cx| m.refresh_pr_now(cx))
            },
        );
        let count = match &view {
            PrView::Ready(s) => Some(s.checks.len()),
            _ => None,
        };
        let mut out: Vec<AnyElement> = Vec::new();
        let head = if with_header {
            div().flex_1().min_w_0().child(section_header(
                key,
                &theme,
                "Pull request",
                count,
                collapsed,
                cx.listener(move |this, _, _, cx| this.toggle(key, cx)),
                None,
            ))
        } else {
            div().flex_1().min_w_0()
        };
        out.push(
            div()
                .w_full()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(head)
                .when(loading, |d| d.child(Spinner::new(c.accent)))
                .child(refresh)
                .into_any_element(),
        );
        if collapsed {
            return out;
        }
        if let Some(e) = self.pr_create_card(cx) {
            out.push(e);
        }
        match &view {
            PrView::Loading => out.push(empty_state(&theme, "Consultando GitHub…", "")),
            PrView::NoPr { branch } => {
                out.extend(self.no_pr_block(branch, window, cx));
            }
            PrView::Ready(snap) => out.extend(self.pr_snapshot_block(snap, cx)),
            PrView::Idle | PrView::Unavailable | PrView::Detached => {}
        }
        out
    }

    fn no_pr_block(
        &mut self,
        branch: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let mut out = Vec::new();
        let toggle = Button::tonal(
            "git-pr-new",
            &theme,
            if self.pr_form {
                "Cerrar formulario"
            } else {
                "Crear PR"
            },
        )
        .tooltip("Abre el formulario: revisas el comando antes de crearlo")
        .on_click(cx.listener(|this, _, _, cx| {
            this.pr_form = !this.pr_form;
            cx.notify();
        }));
        out.push(
            div()
                .w_full()
                .flex_none()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme.typography.ui_size_sm))
                        .text_color(c.muted)
                        .child(format!(
                            "{} no tiene pull request.",
                            truncate_end(branch, 30)
                        )),
                )
                .child(toggle)
                .into_any_element(),
        );
        if self.pr_form {
            out.push(self.pr_form_card(window, cx));
        }
        out
    }

    fn pr_form_card(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let title_ok = !self.pr_title.read(cx).is_blank();
        let busy = !matches!(self.model.read(cx).pr_create(), PrCreate::Idle);
        let draft_on = self.pr_draft;
        let toggle = Button::tonal(
            "git-pr-draft-flag",
            &theme,
            if draft_on {
                "Borrador: sí"
            } else {
                "Borrador: no"
            },
        )
        .tooltip("Crear el PR como borrador")
        .on_click(cx.listener(|this, _, _, cx| {
            this.pr_draft = !this.pr_draft;
            cx.notify();
        }));
        let review = Button::primary("git-pr-review", &theme, "Revisar y crear")
            .disabled(!title_ok || busy)
            .tooltip("Muestra el comando exacto; no crea nada hasta que confirmes")
            .on_click(cx.listener(|this, _, _, cx| this.review_pr(cx)));
        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(Radius::SM))
            .bg(c.surface_2)
            .child(self.input_box(&self.pr_title.clone(), false, window, cx))
            .child(self.input_box(&self.pr_body.clone(), true, window, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(review)
                    .child(toggle),
            )
            .child(div().flex().flex_row().child(self.draft_button(
                "git-pr-draft",
                DraftKind::PullRequest,
                !busy,
                cx,
            )))
            .into_any_element()
    }

    /// The confirmation / result card of "Crear PR".
    fn pr_create_card(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let state = self.model.read(cx).pr_create().clone();
        let model = self.model.clone();
        match state {
            PrCreate::Idle => None,
            PrCreate::Confirm(pr) => {
                let (m_ok, m_no) = (model.clone(), model);
                let body_preview: String = pr.body.lines().take(8).collect::<Vec<_>>().join("\n");
                let more = pr.body.lines().count().saturating_sub(8);
                let mut card = card_shell(&theme, false)
                    .child(
                        div()
                            .text_size(px(theme.typography.ui_size_md))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c.text)
                            .child("Crear pull request"),
                    )
                    .child(
                        div()
                            .text_size(px(theme.typography.ui_size_sm))
                            .text_color(c.muted)
                            .child("Se ejecutará exactamente este comando:"),
                    )
                    .child(
                        div()
                            .id("git-pr-command")
                            .w_full()
                            .p(px(8.0))
                            .rounded(px(Radius::CONTROL))
                            .bg(c.surface_0)
                            .font_family(theme.typography.mono_family)
                            .text_size(px(theme.typography.ui_size_xs))
                            .text_color(c.text)
                            .child(display_command(&pr.argv())),
                    );
                if !body_preview.is_empty() {
                    card = card.child(
                        div()
                            .w_full()
                            .p(px(8.0))
                            .rounded(px(Radius::CONTROL))
                            .bg(c.surface_0)
                            .font_family(theme.typography.mono_family)
                            .text_size(px(theme.typography.ui_size_xs))
                            .text_color(c.muted)
                            .child(if more > 0 {
                                format!("{body_preview}\n… y {more} líneas más")
                            } else {
                                body_preview
                            }),
                    );
                }
                card = card.child(
                    div()
                        .text_size(px(theme.typography.ui_size_xs))
                        .text_color(c.muted)
                        .child(
                            "La descripción se envía por la entrada estándar (--body-file -). \
                             La rama debe estar publicada en el remoto.",
                        ),
                );
                Some(
                    card.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                Button::primary("git-pr-confirm", &theme, "Confirmar").on_click(
                                    move |_, _, cx| {
                                        m_ok.update(cx, |m, cx| m.confirm_create_pr(cx))
                                    },
                                ),
                            )
                            .child(
                                GhostButton::new("git-pr-cancel", &theme, "Cancelar").on_click(
                                    move |_, _, cx| m_no.update(cx, |m, cx| m.cancel_create_pr(cx)),
                                ),
                            ),
                    )
                    .into_any_element(),
                )
            }
            PrCreate::Running(_) => Some(
                card_shell(&theme, false)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(Spinner::new(c.accent))
                            .child(
                                div()
                                    .text_size(px(theme.typography.ui_size_sm))
                                    .text_color(c.muted)
                                    .child("Creando el pull request…"),
                            ),
                    )
                    .into_any_element(),
            ),
            PrCreate::Done { url } => {
                let m = model;
                let open = url.clone();
                Some(
                    card_shell(&theme, false)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(icon(Icon::Check, 16.0, c.success))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(theme.typography.ui_size_sm))
                                        .text_color(c.text)
                                        .child(format!("PR creado: {url}")),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(6.0))
                                .child(
                                    Button::tonal("git-pr-open", &theme, "Abrir en GitHub")
                                        .on_click(move |_, _, cx| cx.open_url(&open)),
                                )
                                .child(
                                    GhostButton::new("git-pr-close", &theme, "Cerrar").on_click(
                                        move |_, _, cx| {
                                            m.update(cx, |m, cx| m.cancel_create_pr(cx))
                                        },
                                    ),
                                ),
                        )
                        .into_any_element(),
                )
            }
            PrCreate::Failed { error, .. } => {
                let m = model;
                Some(
                    card_shell(&theme, true)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_start()
                                .gap(px(8.0))
                                .child(icon(Icon::Alert, 16.0, c.error))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(theme.typography.ui_size_sm))
                                        .text_color(c.text)
                                        .child(format!("No se pudo crear el PR: {error}")),
                                ),
                        )
                        .child(
                            GhostButton::new("git-pr-close", &theme, "Cerrar").on_click(
                                move |_, _, cx| m.update(cx, |m, cx| m.cancel_create_pr(cx)),
                            ),
                        )
                        .into_any_element(),
                )
            }
        }
    }

    fn pr_snapshot_block(&mut self, snap: &PrSnapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let pr = &snap.pr;
        let summary = summarize(pr, &snap.checks);
        let mut out: Vec<AnyElement> = Vec::new();

        let url = pr.url.clone();
        let mut tags = div().flex().flex_row().flex_wrap().gap(px(4.0));
        tags = tags.child(tag(&theme, state_label(pr)));
        if pr.draft {
            tags = tags.child(tag(&theme, "Borrador"));
        }
        if let Some(d) = pr.review_decision {
            tags = tags.child(tag(&theme, review_label(d)));
        }
        if pr.mergeable == git_svc::Mergeable::Conflicting {
            tags = tags.child(tag(&theme, "Conflictos"));
        }
        out.push(
            div()
                .id("git-pr-info")
                .occlude()
                .w_full()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(10.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_2)
                .cursor_pointer()
                .hover(move |s| s.bg(c.surface_3))
                .tooltip(Tooltip::text(&theme, "Abrir el pull request en GitHub"))
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(theme.typography.ui_size_md))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(c.accent)
                                .child(format!("#{}", pr.number)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme.typography.ui_size_md))
                                .text_color(c.text)
                                .child(truncate_end(&pr.title, if compact { 34 } else { 64 })),
                        ),
                )
                .child(tags)
                .into_any_element(),
        );

        if snap.checks.is_empty() {
            out.push(
                div()
                    .text_size(px(theme.typography.ui_size_sm))
                    .text_color(c.muted)
                    .child(if snap.checks_known {
                        "Este PR no tiene checks."
                    } else {
                        "No se pudieron leer los checks."
                    })
                    .into_any_element(),
            );
            return out;
        }

        let totals = snap.totals();
        out.push(
            div()
                .text_size(px(theme.typography.ui_size_sm))
                .text_color(tone_color(&theme, summary.tone))
                .child(format!(
                    "{}{}{}",
                    plural(totals.pass, "correcto", "correctos"),
                    if totals.fail > 0 {
                        format!(" · {}", plural(totals.fail, "fallido", "fallidos"))
                    } else {
                        String::new()
                    },
                    if totals.pending > 0 {
                        format!(" · {}", plural(totals.pending, "en curso", "en curso"))
                    } else {
                        String::new()
                    }
                ))
                .into_any_element(),
        );

        let mut checks: Vec<&Check> = snap.checks.iter().collect();
        checks.sort_by_key(|c| bucket_rank(c.bucket));
        let total = checks.len();
        let limit = if self.pr_all_checks {
            total
        } else {
            CHECKS_SHOWN
        };
        for (i, check) in checks.into_iter().take(limit).enumerate() {
            out.push(self.check_row(i, check));
        }
        if total > CHECKS_SHOWN {
            out.push(
                GhostButton::new(
                    "git-pr-all",
                    &theme,
                    if self.pr_all_checks {
                        "Mostrar menos".to_string()
                    } else {
                        format!("Mostrar todos ({total})")
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.pr_all_checks = !this.pr_all_checks;
                    cx.notify();
                }))
                .into_any_element(),
            );
        }

        if totals.fail > 0 {
            let sending = self.model.read(cx).is_sending_failed_checks();
            let model = self.model.clone();
            out.push(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .pt(px(4.0))
                    .child(
                        Button::tonal(
                            "git-pr-send-failed",
                            &theme,
                            if sending {
                                "Obteniendo logs…"
                            } else {
                                "Enviar checks fallidos al agente"
                            },
                        )
                        .disabled(sending)
                        .tooltip("Envía los logs de los checks que fallan (con secretos ocultos)")
                        .on_click(move |_, _, cx| {
                            model.update(cx, |m, cx| m.send_failed_checks_to_agent(cx))
                        }),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn check_row(&self, ix: usize, check: &Check) -> AnyElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let compact = self.is_compact();
        let (glyph, tone) = check_glyph(check);
        let url = check.url.clone();
        let linked = !url.is_empty();
        let name = truncate_end(&check.name, if compact { 26 } else { 44 });
        let mut row = div()
            .id(SharedString::from(format!("git-check-{ix}")))
            .occlude()
            .w_full()
            .flex_none()
            .h(px(26.0))
            .px(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::CHIP))
            .when(linked, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(c.surface_2))
                    .tooltip(Tooltip::text(&theme, "Abrir el check en GitHub"))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
            .child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .text_size(px(theme.typography.ui_size_md))
                    .text_color(tone_color(&theme, tone))
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_size(px(theme.typography.ui_size_md))
                    .text_color(if check.is_failing() { c.text } else { c.muted })
                    .child(name),
            );
        if let Some(secs) = check.duration_secs() {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(px(theme.typography.ui_size_xs))
                    .text_color(c.muted)
                    .child(format_duration(secs)),
            );
        }
        row.into_any_element()
    }
}
