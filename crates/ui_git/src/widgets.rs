//! Small, stateless elements of the panel: file rows, the diff card, the review / result cards.
//!
//! No dividers (DESIGN.md): groups are separated by tone and spacing. The only accent is the
//! focus ring, the dirty dot and the cursor; the single `accent_strong` button is "Commit".

use gpui::{
    AnyElement, App, ClickEvent, Entity, FontWeight, HighlightStyle, Hsla, IntoElement,
    ParentElement, Rgba, SharedString, StatefulInteractiveElement, Styled, StyledText, Window, div,
    prelude::*, px,
};
use theme::{Radius, Theme, rgb};
use ui_kit::{Button, GhostButton, Icon, Spinner, Tooltip, VirtualList, icon};
use viewer_svc::{HighlightKind, LineKind};

use crate::data::DiffTarget;
use crate::diffview::{DiffRow, PreviewLine};
use crate::fmt::path_label;
use crate::gate::{Gate, LineKind as ReviewKind, Review};
use crate::model::{DiffSlot, DiffState, GitModel};
use crate::sections::FileRow;

/// Height of a diff line.
pub const DIFF_ROW_H: f32 = 18.0;
/// Diff lines shown before the card scrolls.
const DIFF_MAX_VISIBLE: usize = 14;
/// Lines of one kind listed on a review card before "... y N más".
const REVIEW_MAX: usize = 6;

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub fn ansi(theme: &Theme, i: usize) -> Rgba {
    rgb(theme.terminal.ansi[i])
}

pub fn alpha(c: Rgba, a: f32) -> Rgba {
    Rgba { a, ..c }
}

/// Background of an added / removed diff line (a low-alpha tint of the terminal's green / red).
pub fn diff_tint(theme: &Theme, kind: LineKind) -> Option<Rgba> {
    let a = match theme.mode {
        theme::Mode::Light => 0.16,
        _ => 0.13,
    };
    match kind {
        LineKind::Added => Some(alpha(ansi(theme, 2), a)),
        LineKind::Removed => Some(alpha(ansi(theme, 1), a)),
        LineKind::Context => None,
    }
}

fn syntax_color(theme: &Theme, kind: HighlightKind) -> Option<Hsla> {
    let c = theme.colors;
    let rgba = match kind {
        HighlightKind::Keyword => ansi(theme, 5),
        HighlightKind::String => ansi(theme, 2),
        HighlightKind::Comment | HighlightKind::Operator | HighlightKind::Punctuation => c.muted,
        HighlightKind::Function | HighlightKind::Property => ansi(theme, 4),
        HighlightKind::Type => ansi(theme, 6),
        HighlightKind::Number | HighlightKind::Constant | HighlightKind::Attribute => ansi(theme, 3),
        HighlightKind::Tag => ansi(theme, 1),
        HighlightKind::Variable | HighlightKind::Plain => return None,
    };
    Some(rgba.into())
}

/// Color of the status letter of a file row.
pub fn letter_color(theme: &Theme, letter: char) -> Rgba {
    match letter {
        'M' => ansi(theme, 3),
        'A' | 'U' => ansi(theme, 2),
        'D' => ansi(theme, 1),
        'R' => ansi(theme, 4),
        'C' => ansi(theme, 6),
        'T' => ansi(theme, 5),
        '!' => theme.colors.error,
        _ => theme.colors.muted,
    }
}

pub fn letter_pill(theme: &Theme, letter: char) -> impl IntoElement + use<> {
    let color = letter_color(theme, letter);
    div()
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .bg(alpha(color, 0.16))
        .text_color(color)
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .child(letter.to_string())
}

/// Small text chip ("auto", "actual").
pub fn tag(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    let c = theme.colors;
    div()
        .h(px(18.0))
        .flex_none()
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(9.0))
        .bg(c.surface_3)
        .text_size(px(11.0))
        .text_color(c.muted)
        .child(text.into())
}

/// A 22 px quiet square with a text glyph (`+`, `−`, `×`). TODO-SVG: stage / unstage / remove
/// pixel icons (see the crate docs).
pub fn mini_button(
    id: impl Into<SharedString>,
    theme: &Theme,
    glyph: &'static str,
    tip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let c = theme.colors;
    div()
        .id(id.into())
        .occlude()
        .size(px(22.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.0))
        .text_size(px(15.0))
        .text_color(c.muted)
        .cursor_pointer()
        .hover(move |s| s.bg(c.surface_3).text_color(c.text))
        .tooltip(Tooltip::text(theme, tip))
        .on_click(move |e, w, cx| on_click(e, w, cx))
        .child(glyph)
}

/// Section header: collapse chevron, title, count and an optional action on the right.
pub fn section_header(
    id: &str,
    theme: &Theme,
    title: &str,
    count: Option<usize>,
    collapsed: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    action: Option<(&'static str, OnClick)>,
) -> gpui::Div {
    let c = theme.colors;
    let toggle = div()
        .id(SharedString::from(format!("{id}-toggle")))
        .occlude()
        .flex_1()
        .min_w_0()
        .h(px(26.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .child(icon(if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, 16.0, c.muted))
        .child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(c.muted).child(title.to_string()))
        .when_some(count, |d, n| {
            d.child(
                div()
                    .px(px(6.0))
                    .h(px(16.0))
                    .flex()
                    .items_center()
                    .rounded(px(8.0))
                    .bg(c.surface_3)
                    .text_size(px(10.0))
                    .text_color(c.muted)
                    .child(n.to_string()),
            )
        })
        .on_click(move |e, w, cx| on_toggle(e, w, cx));
    div().w_full().flex_none().flex().flex_row().items_center().gap(px(4.0)).child(toggle).when_some(action, |d, (label, f)| {
        d.child(
            GhostButton::new(SharedString::from(format!("{id}-action")), theme, label)
                .on_click(move |e, w, cx| f(e, w, cx)),
        )
    })
}

/// Truncated path, directory muted and file name strong.
fn path_text(theme: &Theme, text: &str, budget: usize) -> impl IntoElement + use<> {
    let c = theme.colors;
    let l = path_label(text, budget);
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(theme.typography.ui_size_md))
        .child(div().flex().flex_row().child(div().text_color(c.muted).child(l.dir)).child(div().text_color(c.text).child(l.name)))
}

fn counts_text(theme: &Theme, row: &FileRow) -> Option<AnyElement> {
    let mono = theme.typography.mono_family;
    match (row.counts, row.counted) {
        (Some((a, d)), _) => Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .gap(px(6.0))
                .font_family(mono)
                .text_size(px(11.0))
                .child(div().text_color(ansi(theme, 2)).child(format!("+{a}")))
                .child(div().text_color(ansi(theme, 1)).child(format!("−{d}")))
                .into_any_element(),
        ),
        (None, true) => Some(div().flex_none().text_size(px(11.0)).text_color(theme.colors.muted).child("bin").into_any_element()),
        (None, false) => None,
    }
}

/// One file of the Changes list. `on_open` toggles its diff; `action` is the `+` / `−` button.
#[allow(clippy::too_many_arguments)] // every argument is a distinct paint input of one row
pub fn file_row(
    theme: &Theme,
    id: SharedString,
    row: &FileRow,
    selected: bool,
    compact: bool,
    budget: usize,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    action: Option<AnyElement>,
) -> gpui::Div {
    let c = theme.colors;
    let main = div()
        .id(id)
        .occlude()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .child(letter_pill(theme, row.letter))
        .child(path_text(theme, &row.text, budget))
        .when(!compact, |d| d.when_some(counts_text(theme, row), |d, t| d.child(t)))
        .on_click(move |e, w, cx| on_open(e, w, cx));
    div()
        .w_full()
        .h(px(28.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .pl(px(6.0))
        .pr(px(2.0))
        .rounded(px(8.0))
        .when(selected, |d| d.bg(c.surface_3))
        .when(!selected, |d| d.hover(move |s| s.bg(c.surface_2)))
        .child(main)
        .when_some(action, |d, a| d.child(a))
}

// ---------------------------------------------------------------------------------------------
// Diff card
// ---------------------------------------------------------------------------------------------

fn diff_line(theme: &Theme, row: &DiffRow) -> AnyElement {
    let c = theme.colors;
    let mono = theme.typography.mono_family;
    let base = div().w_full().h(px(DIFF_ROW_H)).flex().flex_row().items_center().font_family(mono).text_size(px(12.0));
    match row {
        DiffRow::Hunk(h) => base.px(px(8.0)).text_size(px(11.0)).text_color(c.muted).child(h.clone()).into_any_element(),
        DiffRow::More(n) => base
            .px(px(8.0))
            .text_size(px(11.0))
            .text_color(c.muted)
            .child(format!("… {n} líneas más (usa «Ampliar»)"))
            .into_any_element(),
        DiffRow::Line(PreviewLine { kind, num, text, spans }) => {
            let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = spans
                .iter()
                .filter_map(|(r, k)| {
                    syntax_color(theme, *k).map(|color| (r.clone(), HighlightStyle { color: Some(color), ..HighlightStyle::default() }))
                })
                .collect();
            let tint = diff_tint(theme, *kind);
            base.when_some(tint, |d, t| d.bg(t))
                .child(
                    div()
                        .w(px(38.0))
                        .flex_none()
                        .pr(px(8.0))
                        .flex()
                        .justify_end()
                        .text_size(px(10.0))
                        .text_color(alpha(c.muted, 0.7))
                        .child(num.map_or_else(String::new, |n| n.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_color(c.text)
                        .child(StyledText::new(if text.is_empty() { " ".to_string() } else { text.clone() }).with_highlights(highlights)),
                )
                .into_any_element()
        }
    }
}

/// The inline diff of the selected file, with "Abrir", "Ampliar" and "Agente".
pub fn diff_card(theme: &Theme, slot: &DiffSlot, compact: bool, model: &Entity<GitModel>) -> AnyElement {
    let c = theme.colors;
    let shell = div().w_full().flex_none().flex().flex_col().gap(px(6.0)).p(px(8.0)).rounded(px(10.0)).bg(c.surface_0);
    let note = |text: String| div().px(px(6.0)).py(px(4.0)).text_size(px(12.0)).text_color(c.muted).child(text);
    let preview = match &slot.state {
        DiffState::Loading => return shell.child(note("Cargando diff…".into())).into_any_element(),
        DiffState::Failed(e) => return shell.child(note(format!("No se pudo leer el diff: {e}"))).into_any_element(),
        DiffState::Ready(p) => p.clone(),
    };
    let path = slot.target.path().to_path_buf();
    let header = {
        let (m1, m2, m3) = (model.clone(), model.clone(), model.clone());
        let (p1, p2) = (path.clone(), path);
        let line = preview.first_line;
        div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(2.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(60.0))
                    .px(px(4.0))
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .font_family(theme.typography.mono_family)
                    .text_size(px(11.0))
                    .child(div().text_color(ansi(theme, 2)).child(format!("+{}", preview.added)))
                    .child(div().text_color(ansi(theme, 1)).child(format!("−{}", preview.removed))),
            )
            .child(GhostButton::new("diff-open", theme, "Abrir").on_click(move |_, _, cx| {
                m1.update(cx, |m, cx| m.open_file(&p1, line, cx));
            }))
            .when(!compact, |d| {
                d.child(GhostButton::new("diff-expand", theme, "Ampliar").on_click(move |_, _, cx| {
                    m2.update(cx, |m, cx| m.open_diff(&p2, cx));
                }))
            })
            .child(
                GhostButton::new("diff-agent", theme, "Agente")
                    .tooltip("Enviar este diff a un agente")
                    .on_click(move |_, _, cx| m3.update(cx, |m, cx| m.ask_agent(cx))),
            )
    };
    if preview.binary {
        return shell.child(header).child(note("Archivo binario: no hay vista previa".into())).into_any_element();
    }
    if preview.is_empty() {
        return shell.child(header).child(note("Sin diferencias".into())).into_any_element();
    }
    let rows = preview.rows.clone();
    let visible = rows.len().min(DIFF_MAX_VISIBLE);
    let t = *theme;
    let list_id: SharedString = format!("diff-{}", slot.target.path().display()).into();
    let list = VirtualList::new(list_id, rows.len(), DIFF_ROW_H, move |range, _, _| {
        range.filter_map(|i| rows.get(i)).map(|r| diff_line(&t, r)).collect()
    })
    .w_full()
    .h(px(visible as f32 * DIFF_ROW_H))
    .rounded(px(6.0))
    .overflow_hidden();
    shell.child(header).child(list).into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Review and result cards
// ---------------------------------------------------------------------------------------------

fn kind_label(theme: &Theme, kind: ReviewKind) -> (&'static str, Rgba) {
    match kind {
        ReviewKind::Create => ("recrea", ansi(theme, 2)),
        ReviewKind::Overwrite => ("sobrescribe", ansi(theme, 3)),
        ReviewKind::Delete => ("elimina", ansi(theme, 1)),
        ReviewKind::Skip => ("omite", theme.colors.muted),
        ReviewKind::Warn => ("", ansi(theme, 3)),
        ReviewKind::Info => ("", theme.colors.muted),
    }
}

fn review_lines(theme: &Theme, review: &Review) -> Vec<AnyElement> {
    let c = theme.colors;
    let mut out: Vec<AnyElement> = Vec::new();
    // Group by kind so a long restore shows 6 of each and "y N más".
    for kind in [
        ReviewKind::Warn,
        ReviewKind::Create,
        ReviewKind::Overwrite,
        ReviewKind::Delete,
        ReviewKind::Skip,
        ReviewKind::Info,
    ] {
        let of_kind: Vec<_> = review.lines.iter().filter(|l| l.kind == kind).collect();
        let (label, color) = kind_label(theme, kind);
        for l in of_kind.iter().take(REVIEW_MAX) {
            let row = div().w_full().flex().flex_row().items_start().gap(px(8.0)).text_size(px(12.0));
            let row = match kind {
                ReviewKind::Warn => row
                    .text_color(c.text)
                    .child(div().pt(px(1.0)).flex_none().child(icon(Icon::Alert, 16.0, color)))
                    .child(div().flex_1().min_w_0().child(l.text.clone())),
                ReviewKind::Info => row.text_color(c.muted).child(div().flex_1().min_w_0().child(l.text.clone())),
                _ => row
                    .child(div().w(px(70.0)).flex_none().text_color(color).child(label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family(theme.typography.mono_family)
                            .text_size(px(11.0))
                            .text_color(c.text)
                            .child(l.text.clone()),
                    ),
            };
            out.push(row.into_any_element());
        }
        if of_kind.len() > REVIEW_MAX {
            out.push(
                div()
                    .pl(px(if matches!(kind, ReviewKind::Warn | ReviewKind::Info) { 24.0 } else { 78.0 }))
                    .text_size(px(11.0))
                    .text_color(c.muted)
                    .child(format!("… y {} más", of_kind.len() - REVIEW_MAX))
                    .into_any_element(),
            );
        }
    }
    out
}

fn card_shell(theme: &Theme, danger: bool) -> gpui::Div {
    let c = theme.colors;
    let bg = if danger { alpha(c.error, 0.10) } else { c.surface_2 };
    div().w_full().flex_none().flex().flex_col().gap(px(8.0)).p(px(12.0)).rounded(px(Radius::SM)).bg(bg)
}

/// The confirmation card, the "comprobando" / "ejecutando" states and the result banner of the
/// operation in `gate`, when it belongs in `slot`.
pub fn gate_card(theme: &Theme, gate: &Gate, slot: crate::cmd::Slot, model: &Entity<GitModel>) -> Option<AnyElement> {
    let c = theme.colors;
    let title = |text: String| div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(c.text).child(text);
    match gate {
        Gate::Idle => None,
        Gate::Preflighting { op, .. } if op.slot() == slot => Some(
            card_shell(theme, false)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(Spinner::new(c.accent))
                        .child(div().text_size(px(12.0)).text_color(c.muted).child(format!("Comprobando {}…", op.title().to_lowercase()))),
                )
                .into_any_element(),
        ),
        Gate::Running { op } if op.slot() == slot => Some(
            card_shell(theme, false)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(Spinner::new(c.accent))
                        .child(div().text_size(px(12.0)).text_color(c.muted).child(format!("{}…", op.title()))),
                )
                .into_any_element(),
        ),
        Gate::Confirm { op, review } if op.slot() == slot => {
            let (m_ok, m_no) = (model.clone(), model.clone());
            let blocked = review.blocked.clone();
            let mut card = card_shell(theme, review.danger).child(title(review.title.clone())).children(review_lines(theme, review));
            if let Some(stat) = &review.stat {
                let lines: Vec<&str> = stat.lines().collect();
                let shown = lines.iter().take(6).copied().collect::<Vec<_>>().join("\n");
                let more = lines.len().saturating_sub(6);
                card = card.child(
                    div()
                        .w_full()
                        .p(px(8.0))
                        .rounded(px(8.0))
                        .bg(c.surface_0)
                        .overflow_hidden()
                        .font_family(theme.typography.mono_family)
                        .text_size(px(11.0))
                        .text_color(c.muted)
                        .child(if more > 0 { format!("{shown}\n… y {more} líneas más") } else { shown }),
                );
            }
            if let Some(why) = blocked.clone() {
                card = card.child(div().text_size(px(12.0)).text_color(c.error).child(why));
            }
            let confirm = Button::primary("gate-confirm", theme, review.confirm_label.clone())
                .disabled(blocked.is_some())
                .on_click(move |_, _, cx| m_ok.update(cx, |m, cx| m.confirm(cx)));
            let cancel = GhostButton::new("gate-cancel", theme, "Cancelar").on_click(move |_, _, cx| m_no.update(cx, |m, cx| m.dismiss(cx)));
            Some(card.child(div().flex().flex_row().items_center().gap(px(6.0)).child(confirm).child(cancel)).into_any_element())
        }
        Gate::Done { op, outcome } if op.slot() == slot => {
            let (m_close, m_retry) = (model.clone(), model.clone());
            let color = if outcome.ok { ansi(theme, 2) } else { c.error };
            let mut card = card_shell(theme, false).child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .child(div().pt(px(1.0)).flex_none().child(icon(if outcome.ok { Icon::Check } else { Icon::Alert }, 16.0, color)))
                    .child(div().flex_1().min_w_0().text_size(px(12.0)).text_color(c.text).child(outcome.summary.clone())),
            );
            if !outcome.detail.is_empty() {
                card = card.child(
                    div()
                        .w_full()
                        .max_h(px(110.0))
                        .overflow_hidden()
                        .font_family(theme.typography.mono_family)
                        .text_size(px(11.0))
                        .text_color(c.muted)
                        .child(outcome.detail.clone()),
                );
            }
            let retry = outcome.retry.is_some();
            Some(
                card.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .when(retry, |d| {
                            d.child(
                                Button::tonal("gate-retry", theme, "Reintentar en terminal")
                                    .tooltip("Lo ejecuta visible en la terminal (útil si pide credenciales)")
                                    .on_click(move |_, _, cx| m_retry.update(cx, |m, cx| m.retry_in_terminal(cx))),
                            )
                        })
                        .child(GhostButton::new("gate-close", theme, "Cerrar").on_click(move |_, _, cx| m_close.update(cx, |m, cx| m.dismiss(cx)))),
                )
                .into_any_element(),
            )
        }
        _ => None,
    }
}

/// Plain error line of a quiet operation.
pub fn quiet_error_card(theme: &Theme, text: &str, model: &Entity<GitModel>) -> AnyElement {
    let c = theme.colors;
    let m = model.clone();
    card_shell(theme, true)
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.0))
                .child(div().pt(px(1.0)).flex_none().child(icon(Icon::Alert, 16.0, c.error)))
                .child(div().flex_1().min_w_0().text_size(px(12.0)).text_color(c.text).child(text.to_string()))
                .child(mini_button("quiet-close", theme, "×", "Cerrar", move |_, _, cx| m.update(cx, |m, cx| m.dismiss(cx)))),
        )
        .into_any_element()
}

/// Muted centered message ("Sin cambios", "Esto no es un repositorio").
pub fn empty_state(theme: &Theme, title: &str, hint: &str) -> AnyElement {
    let c = theme.colors;
    div()
        .w_full()
        .flex_none()
        .py(px(20.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .child(div().text_size(px(theme.typography.ui_size_md)).text_color(c.text).child(title.to_string()))
        .child(div().px(px(12.0)).text_size(px(12.0)).text_color(c.muted).child(hint.to_string()))
        .into_any_element()
}

/// Target a file row opens.
pub fn working_target(row: &FileRow) -> DiffTarget {
    DiffTarget::Working { section: row.section, path: row.path.clone() }
}
