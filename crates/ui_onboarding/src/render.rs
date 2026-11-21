//! The frame around every step: breadcrumb header, scrolling body, footer buttons, and the
//! bottom-sheet variant for narrow windows (DESIGN.md "Responsive", class XS).

use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Render, Styled, Window, div, prelude::*, px,
};
use theme::Radius;
use ui_kit::{ActionButton, ActionSize, GhostButton, Icon, Logo, icon};

use crate::flow::Step;
use crate::view::OnboardingView;
use crate::widgets::{COLUMN_WIDTH, WIDE_COLUMN_WIDTH, XS_BELOW, ok_color};

impl OnboardingView {
    fn primary_label(&self) -> &'static str {
        match self.flow.step() {
            Step::Welcome => "Empezar",
            Step::Done => "Abrir Selachi",
            _ => "Continuar",
        }
    }

    /// "✓ Bienvenida · ✓ Agentes · Proyectos · …"; on XS just "Paso 3 de 6 · Proyectos".
    fn breadcrumb(&self, cx: &mut Context<Self>, xs: bool) -> Div {
        let t = self.theme;
        let c = t.colors;
        let current = self.flow.step();
        if xs {
            return div()
                .text_size(px(12.0))
                .text_color(c.muted)
                .child(format!("Paso {} de {} · {}", current.index() + 1, Step::ALL.len(), current.label()));
        }
        let mut row = div().flex().flex_row().flex_wrap().justify_center().items_center().gap(px(6.0)).text_size(px(12.0));
        for (ix, step) in Step::ALL.into_iter().enumerate() {
            if ix > 0 {
                row = row.child(div().text_color(c.outline_soft).child("·"));
            }
            let reachable = step <= self.flow.furthest() && step != current && !self.finishing;
            let is_current = step == current;
            let done = self.flow.is_done(step);
            let color = if is_current { c.text } else if step < current || self.flow.is_skipped(step) { c.muted } else { c.outline };
            let item = div()
                .id(("crumb", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .text_color(color)
                .when(is_current, |d| d.font_weight(FontWeight::MEDIUM))
                .when(done, |d| d.child(icon(Icon::Check, 12.0, ok_color(&t))))
                .child(step.label())
                .when(reachable, |d| {
                    d.cursor_pointer()
                        .hover(move |s| s.text_color(c.text))
                        .on_click(cx.listener(move |this, _, window, cx| this.go_to(step, window, cx)))
                });
            row = row.child(item);
        }
        row
    }

    fn header(&self, cx: &mut Context<Self>, xs: bool) -> Div {
        let mut col = div().w_full().flex().flex_col().items_center().gap(px(14.0)).pt(px(if xs { 18.0 } else { 28.0 })).px(px(16.0));
        if self.flow.step() != Step::Welcome {
            col = col.child(Logo::new(self.theme.mode).size(30.0));
        }
        col.child(self.breadcrumb(cx, xs))
    }

    /// Primary button plus the quiet navigation under it.
    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let step = self.flow.step();
        let primary = ActionButton::new("primary", &t, if self.finishing { "Guardando…" } else { self.primary_label() })
            .size(ActionSize::Lg)
            .no_icon()
            .disabled(self.finishing)
            .on_click(cx.listener(|this, _, window, cx| this.primary(window, cx)));
        let mut quiet = div().flex().flex_row().flex_wrap().justify_center().items_center().gap(px(4.0));
        if self.flow.can_go_back() {
            quiet = quiet.child(
                GhostButton::new("back", &t, "Atrás").on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
            );
        }
        if step == Step::Welcome {
            quiet = quiet.child(
                GhostButton::new("import", &t, "Importar configuración")
                    .on_click(cx.listener(|this, _, window, cx| this.import_config(window, cx))),
            );
        }
        if self.flow.can_skip() {
            quiet = quiet.child(
                GhostButton::new("skip", &t, "Omitir por ahora").on_click(cx.listener(|this, _, window, cx| this.skip(window, cx))),
            );
        }
        div().w_full().flex().flex_col().items_center().gap(px(10.0)).child(primary).child(quiet)
    }

    fn notice_line(&self) -> Option<AnyElement> {
        let c = self.theme.colors;
        self.notice.as_ref().map(|n| {
            div().w_full().px(px(16.0)).text_size(px(12.0)).text_color(c.muted).text_center().child(n.clone()).into_any_element()
        })
    }
}

impl Render for OnboardingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.init_window(window, cx);
        let t = self.theme;
        let c = t.colors;
        let size = window.viewport_size();
        let (w, h) = (f32::from(size.width), f32::from(size.height));
        let xs = w < XS_BELOW;
        let step = self.flow.step();
        let column = if step == Step::Appearance { WIDE_COLUMN_WIDTH } else { COLUMN_WIDTH };

        let body = self.step_body(window, cx, xs);
        let body_scroll = div()
            .id("onboarding-scroll")
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .max_w(px(column))
                    .px(px(if xs { 4.0 } else { 16.0 }))
                    .pt(px(if xs { 4.0 } else { 20.0 }))
                    .pb(px(16.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(18.0))
                    .child(body)
                    .children(self.notice_line()),
            );
        let footer = self.footer(cx);

        let root = div()
            .key_context("Onboarding")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "enter" && !ev.keystroke.modifiers.modified() {
                    this.primary(window, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, _| window.focus(&this.focus)))
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col();

        if xs {
            // Bottom sheet: the step rises from the bottom edge on a raised card.
            let hero = if step == Step::Welcome { Some(self.hero(&t)) } else { None };
            root.child(
                div()
                    .flex_1()
                    .min_h(px(96.0))
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.0))
                    .child(self.header(cx, true))
                    .children(hero),
            )
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .max_h(px(h * 0.86))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .pt(px(20.0))
                    .pb(px(16.0))
                    .px(px(12.0))
                    .rounded_tl(px(Radius::LG))
                    .rounded_tr(px(Radius::LG))
                    .bg(c.surface_1)
                    .child(div().flex_shrink().min_h_0().flex().flex_col().child(body_scroll))
                    .child(footer),
            )
        } else {
            root.child(self.header(cx, false))
                .child(div().flex_1().min_h_0().w_full().flex().flex_col().child(body_scroll))
                .child(div().flex_none().w_full().pt(px(8.0)).pb(px(24.0)).child(footer))
        }
    }
}
