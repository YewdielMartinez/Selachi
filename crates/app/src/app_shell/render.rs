//! Painting the shell. Everything here only *reads* the shell: no state changes, no tasks, no
//! database (those live in the event handlers; see the module docs of `app_shell`).

use super::*;
use crate::hint::hint_card;
use crate::rail::rail;
use crate::status_bar::bar_height;
use crate::titlebar::{title_row, window_controls};
use ui_kit::{Button, Card, icon};

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::repaint::bump(crate::repaint::View::Shell);
        if let Some(ob) = self.onboarding_view() {
            return self.onboarding_root(ob, window).into_any_element();
        }
        self.shell_root(window, cx)
    }
}

/// The title bar as a cached child: it repaints when it is notified, not on every shell render.
fn cached_titlebar(view: &Entity<TitleBarView>) -> AnyView {
    AnyView::from(view.clone()).cached(StyleRefinement::default().w_full().h(px(TITLEBAR_H)))
}

impl AppShell {
    /// The first-run wizard, with our own window controls (the system titlebar is hidden).
    fn onboarding_root(
        &self,
        ob: Entity<ui_onboarding::OnboardingView>,
        window: &Window,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let c = theme.colors;
        div()
            .id("onboarding-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(theme.typography.ui_family)
            .child(
                title_row(&theme)
                    .child(div().flex_1())
                    .child(window_controls(&theme, window.is_maximized())),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(ob),
            )
    }

    /// Wraps `content` in a zone of animated size. With no transition (or equal sizes) it is a
    /// plain fixed-size box; otherwise the box animates `from -> to` while the content keeps its
    /// final size and is clipped, so nothing reflows during the 200 ms.
    fn zone(
        &self,
        name: &'static str,
        horizontal: bool,
        from: f32,
        to: f32,
        content: impl IntoElement,
    ) -> AnyElement {
        let animate = self.anim.transition.is_some() && (from - to).abs() > 0.5;
        // The content is always laid out at its *target* size (the old size only when the zone is
        // going away): the new content shows immediately and never reflows or swaps at the end;
        // only the clipping box moves (SHEET easing, no overshoot).
        let full = if animate && to <= 0.5 { from } else { to };
        let inner = if horizontal {
            div().w(px(full)).h_full().flex_none().child(content)
        } else {
            div().h(px(full)).w_full().flex_none().child(content)
        };
        let outer = div().flex_none().overflow_hidden().child(inner);
        let outer = if horizontal {
            outer.h_full()
        } else {
            outer.w_full()
        };
        if !animate {
            return if horizontal {
                outer.w(px(to))
            } else {
                outer.h(px(to))
            }
            .into_any_element();
        }
        let epoch = self.anim.transition.as_ref().map_or(0, |t| t.epoch);
        let outer = if horizontal {
            outer.w(px(from))
        } else {
            outer.h(px(from))
        };
        outer
            .with_animation(
                ElementId::from((name, epoch as usize)),
                Animation::new(BASE).with_easing(|t| SHEET.solve(t).clamp(0.0, 1.0)),
                move |el, t| {
                    let v = from + (to - from) * t;
                    if horizontal { el.w(px(v)) } else { el.h(px(v)) }
                },
            )
            .into_any_element()
    }

    fn pane_header(&self, info: &TabInfo) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        div()
            .h(px(22.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(4.0))
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .child(crate::agents::lead_icon(&info.icon, 14.0, c.muted))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(info.title.clone()),
            )
            .children(
                info.subtitle
                    .clone()
                    .map(|s| div().text_color(c.outline).child(s)),
            )
            .child(div().flex_1())
            .children(
                info.status
                    .map(|s| ui_kit::StatusGlyph::new(&self.theme, s)),
            )
    }

    /// One pane: a floating card hosting a tab (or an empty-pane prompt).
    fn pane(
        &self,
        ix: usize,
        infos: &[TabInfo],
        m: Metrics,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let multi = pane_count(self.eff.layout.splits) > 1;
        let focused = self.splits.panes.focused() == ix;
        let shell = cx.entity();
        let tab = self.splits.panes.panes()[ix];
        let card = div()
            .id(("pane", ix))
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .px(px(m.pad_x))
            .py(px(m.pad_y))
            .rounded(px(Radius::MD))
            .bg(c.surface_0)
            .border_1()
            .border_color(if multi && focused {
                hsla(c.accent).opacity(0.55)
            } else {
                hsla(c.surface_0)
            })
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, {
                let shell = shell.clone();
                move |_, window, cx| shell.update(cx, |s, cx| s.focus_pane(ix, window, cx))
            });
        match tab.and_then(|id| self.slot(id).map(|s| (id, s))) {
            Some((id, slot)) => {
                let info = infos.iter().find(|i| i.id == id);
                let is_viewer = slot.handle.downcast::<ViewerTab>().is_some();
                let drop = is_viewer.then(|| self.viewer_drop_zone("viewer-drop", Radius::MD, cx));
                card.relative()
                    .children(multi.then(|| info.map(|i| self.pane_header(i))).flatten())
                    .child(slot.handle.content(cx))
                    .children(drop)
            }
            // While a session waits for the chat hub the panes stay blank (no "empty pane" prompt).
            None if self.session.read(cx).restore_pending() => card,
            None => card.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.muted)
                    .child("Panel vacío")
                    .child(
                        Button::tonal(format!("pane-new-{ix}"), &self.theme, "Nueva terminal")
                            .icon(ui_kit::Icon::Plus)
                            .on_click(move |_, window, cx| {
                                shell.update(cx, |s, cx| {
                                    s.focus_pane(ix, window, cx);
                                    s.new_tab_here(window, cx);
                                });
                            }),
                    ),
            ),
        }
    }

    /// A resize handle between two panes.
    fn gutter(
        &self,
        vertical_bar: bool,
        drag: Drag,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shell = cx.entity();
        let d = div().flex_none();
        let d = if vertical_bar {
            d.w(px(GUTTER)).h_full().cursor_col_resize()
        } else {
            d.h(px(GUTTER)).w_full().cursor_row_resize()
        };
        d.on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
            shell.update(cx, |s, _| s.start_drag(drag));
        })
    }

    /// A flex child that takes `share` of its parent's main axis.
    fn share(share: f32) -> Div {
        let mut d = div().min_w_0().min_h_0().flex().flex_col();
        let s = d.style();
        s.flex_grow = Some(share);
        s.flex_shrink = Some(1.0);
        s.flex_basis = Some(relative(0.0).into());
        d
    }

    /// The split area, laid out as `layout.splits`.
    fn pane_area(
        &self,
        infos: &[TabInfo],
        m: Metrics,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (rx, ry) = (
            clamp_ratio(self.splits.ratio_x),
            clamp_ratio(self.splits.ratio_y),
        );
        let bounds = self.splits.bounds.clone();
        let tracker = canvas(move |b, _, _| bounds.set(b), |_, (), _, _| {})
            .absolute()
            .size_full();
        let row = || div().size_full().min_w_0().min_h_0().flex().flex_row();
        let col = || div().size_full().min_w_0().min_h_0().flex().flex_col();
        let body: AnyElement = match self.eff.layout.splits {
            Splits::Single => self.pane(0, infos, m, cx).into_any_element(),
            Splits::TwoVertical => row()
                .child(Self::share(rx).child(self.pane(0, infos, m, cx)))
                .child(self.gutter(true, Drag::SplitX, cx))
                .child(Self::share(1.0 - rx).child(self.pane(1, infos, m, cx)))
                .into_any_element(),
            Splits::TwoHorizontal => col()
                .child(Self::share(ry).child(self.pane(0, infos, m, cx)))
                .child(self.gutter(false, Drag::SplitY, cx))
                .child(Self::share(1.0 - ry).child(self.pane(1, infos, m, cx)))
                .into_any_element(),
            Splits::OneAndTwoStacked => row()
                .child(Self::share(rx).child(self.pane(0, infos, m, cx)))
                .child(self.gutter(true, Drag::SplitX, cx))
                .child(
                    Self::share(1.0 - rx)
                        .child(Self::share(ry).child(self.pane(1, infos, m, cx)))
                        .child(self.gutter(false, Drag::SplitY, cx))
                        .child(Self::share(1.0 - ry).child(self.pane(2, infos, m, cx))),
                )
                .into_any_element(),
            Splits::Grid2x2 => col()
                .child(
                    Self::share(ry).child(
                        row()
                            .child(Self::share(rx).child(self.pane(0, infos, m, cx)))
                            .child(self.gutter(true, Drag::SplitX, cx))
                            .child(Self::share(1.0 - rx).child(self.pane(1, infos, m, cx))),
                    ),
                )
                .child(self.gutter(false, Drag::SplitY, cx))
                .child(
                    Self::share(1.0 - ry).child(
                        row()
                            .child(Self::share(rx).child(self.pane(2, infos, m, cx)))
                            .child(self.gutter(true, Drag::SplitX, cx))
                            .child(Self::share(1.0 - rx).child(self.pane(3, infos, m, cx))),
                    ),
                )
                .into_any_element(),
        };
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(body)
            .child(tracker)
    }

    /// The right panel: header (title, window controls) and the provider's view. `overlay`: it is
    /// the S overlay / XS drawer (no resize handle, near-opaque glass).
    fn right_card(
        &self,
        window: &Window,
        with_controls: bool,
        m: Metrics,
        overlay: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let tint = if overlay {
            Some(self.overlay_bg())
        } else {
            self.glass(&self.eff.layout)
        };
        let registry_info = self.right.as_ref().and_then(|r| {
            cx.try_global::<PanelRegistry>()
                .and_then(|reg| reg.get(&r.id))
                .map(|p| (p.title(), p.icon()))
        });
        let mut header = div()
            .h(px(TITLEBAR_H))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .pl(px(GUTTER + 6.0))
            .window_control_area(gpui::WindowControlArea::Drag)
            .text_size(px(ty.ui_size_md))
            .text_color(c.muted);
        if let Some((title, ic)) = registry_info {
            header = header.child(icon(ic, 16.0, c.muted)).child(title);
        }
        header = header.child(div().flex_1());
        if with_controls {
            header = header.child(window_controls(&self.theme, window.is_maximized()));
        }
        let is_tree = self
            .right
            .as_ref()
            .is_some_and(|r| r.id.as_str() == ui_viewer::FILES_PANEL_ID);
        let body: AnyElement = match &self.right {
            Some(RightPanel { view: Some(v), .. }) if is_tree => div()
                .relative()
                .size_full()
                .child(v.clone())
                .child(self.viewer_drop_zone("tree-drop", Radius::MD, cx))
                .into_any_element(),
            Some(RightPanel { view: Some(v), .. }) => v.clone().into_any_element(),
            Some(RightPanel { id, view: None, .. }) => Card::new(&self.theme)
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(c.text)
                        .text_size(px(ty.ui_size_md))
                        .child("Panel no disponible"),
                )
                .child(
                    div()
                        .text_color(c.muted)
                        .text_size(px(ty.ui_size_sm))
                        .child(format!("«{}» no está registrado.", id.as_str())),
                )
                .into_any_element(),
            None => div().into_any_element(),
        };
        let shell = cx.entity();
        let w = self.right_mounted_width();
        // The drag handle: the 8 px gutter on the card's left edge.
        let handle = div()
            .w(px(GUTTER))
            .h_full()
            .flex_none()
            .when(!overlay, |d| {
                d.cursor_col_resize().on_mouse_down(
                    MouseButton::Left,
                    move |e: &MouseDownEvent, _, cx| {
                        let x = f32::from(e.position.x);
                        shell.update(cx, |s, _| {
                            s.start_drag(Drag::Right {
                                start_x: x,
                                start_w: w,
                            })
                        });
                    },
                )
            });
        let mut col = div().size_full().flex().flex_col().child(header);
        if let Some(t) = tint {
            col = col.bg(t);
        }
        col.child(
            div()
                .flex_1()
                .min_h_0()
                .mb(px(m.margin))
                .mr(px(m.margin))
                .flex()
                .flex_row()
                .child(handle)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .rounded(px(Radius::MD))
                        .when(tint.is_none(), |d| d.bg(c.surface_1))
                        .overflow_hidden()
                        // The zone's keyboard focus (F6): a container, never a tab stop itself.
                        .track_focus(&self.chrome.right_focus)
                        .child(body),
                ),
        )
    }

    /// The "close this tab?" prompt.
    fn confirm_overlay(
        &self,
        why: &SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let shell = cx.entity();
        let cancel = shell.clone();
        let id = self.prompts.confirm_close.as_ref().map(|(id, _)| *id);
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.0,
                a: 0.4,
            })
            .child(
                Card::new(&self.theme)
                    .radius(Radius::MD)
                    .p_4()
                    .w(px(380.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_lg))
                            .text_color(c.text)
                            .child("¿Cerrar la pestaña?"),
                    )
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.muted)
                            .child(why.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                Button::ghost("confirm-cancel", &self.theme, "Cancelar").on_click(
                                    move |_, _, cx| {
                                        cancel.update(cx, |s, cx| {
                                            s.prompts.confirm_close = None;
                                            s.refresh(cx);
                                        });
                                    },
                                ),
                            )
                            .child(
                                Button::primary("confirm-close", &self.theme, "Cerrar").on_click(
                                    move |_, window, cx| {
                                        if let Some(id) = id {
                                            shell.update(cx, |s, cx| s.close_tab(id, window, cx));
                                        }
                                    },
                                ),
                            ),
                    ),
            )
    }

    fn shell_root(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let z = self.zones();
        let l = z.l.clone();
        let m = Metrics::of(l.density);
        let infos = self.infos(cx);
        let from = z.from.clone();
        let shell = cx.entity();

        // ---- rail (its buttons come from the panel registry)
        let rail_shell = shell.clone();
        let rail_el = self.zone(
            "zone-rail",
            true,
            Self::rail_w(&from),
            Self::rail_w(&l),
            rail(&theme, self.rail_items(cx), move |action, window, cx| {
                rail_shell.update(cx, |s, cx| s.run_rail_action(action, window, cx));
            }),
        );

        // ---- sidebars (left / right): entities that paint themselves
        let collapsed = |sl: &ResolvedLayout| sl.sidebar.width_px == COLLAPSED_SIDEBAR_W;
        let left_el = (z.left_w > 0.0 || z.left_from > 0.0).then(|| {
            self.zone(
                "zone-left",
                true,
                z.left_from,
                z.left_w,
                cached(&self.chrome.left),
            )
        });
        let right_sb_el = (z.rsb_w > 0.0 || z.rsb_from > 0.0).then(|| {
            self.zone(
                "zone-rsb",
                true,
                z.rsb_from,
                z.rsb_w,
                cached(&self.chrome.right),
            )
        });
        let left_visible = z.left_w > 0.0;
        let right_visible = z.right_w > 0.0 || z.rsb_w > 0.0;
        let sidebar_gutter = |side: SidebarSide| {
            let shell2 = shell.clone();
            let w = l.sidebar.width_px;
            let resizable = !collapsed(&l);
            div()
                .w(px(GUTTER))
                .h_full()
                .flex_none()
                .when(resizable, |d| {
                    d.cursor_col_resize().on_mouse_down(
                        MouseButton::Left,
                        move |e: &MouseDownEvent, _, cx| {
                            let x = f32::from(e.position.x);
                            shell2.update(cx, |s, _| {
                                s.start_drag(Drag::Sidebar {
                                    side,
                                    start_x: x,
                                    start_w: w,
                                })
                            });
                        },
                    )
                })
        };
        let gutter_left = left_visible.then(|| sidebar_gutter(l.sidebar.side));
        let gutter_right = (z.rsb_w > 0.0).then(|| sidebar_gutter(SidebarSide::Right));

        // ---- right panel (docked; the overlay / drawer is drawn over the body below)
        let right_el = (z.right_w > 0.0 || z.right_from > 0.0).then(|| {
            self.zone(
                "zone-right",
                true,
                z.right_from,
                z.right_w,
                self.right_card(window, z.controls_in_right, m, false, cx),
            )
        });

        // ---- center: title row + panes
        let title_row_el = (!l.zen).then(|| cached_titlebar(&self.chrome.titlebar));
        let panes = self.pane_area(&infos, m, cx);
        // Zen: while the top bar is revealed the terminal is pushed down by its height, so the bar
        // never covers the first line (the push is instant and only lasts while it is shown).
        let zen_push = if l.zen
            && (self.drawer.reveal.top
                || self.layout_panel.is_some()
                || self.chrome.titlebar.read(cx).menu_open())
        {
            TITLEBAR_H
        } else {
            0.0
        };
        let center = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(c.bg)
            .children(title_row_el)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .mb(px(m.margin))
                    .ml(px(if left_visible { 0.0 } else { m.margin }))
                    .mr(px(if right_visible { 0.0 } else { m.margin }))
                    .mt(px(if l.zen { m.margin + zen_push } else { 0.0 }))
                    .child(panes),
            );

        // ---- bottom bar (its mode follows the layout: see `sync_chrome`)
        let status_el =
            (bar_height(l.bottom_bar) > 0.0 || bar_height(from.bottom_bar) > 0.0).then(|| {
                self.zone(
                    "zone-status",
                    false,
                    bar_height(from.bottom_bar),
                    bar_height(l.bottom_bar),
                    self.status.clone(),
                )
            });

        // ---- zen reveal overlays, drawers
        let zen_overlays = l.zen.then(|| self.zen_overlays(cx));
        let drawers = self.drawers(window, m, cx);

        let translucent = self.host.blurred;
        let mut body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_row();
        body = body.child(div().h_full().bg(c.bg).child(rail_el));
        if l.sidebar.side != SidebarSide::Right || left_el.is_some() {
            body = body.children(left_el.map(|e| {
                // Transparent only here: the blurred backdrop shows through the glass column.
                div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)
            }));
            body = body.children(gutter_left.map(|g| div().h_full().bg(c.bg).child(g)));
        }
        body = body.child(center);
        body = body
            .children(right_el.map(|e| div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)));
        if z.rsb_w > 0.0 || z.rsb_from > 0.0 {
            body = body.children(gutter_right.map(|g| div().h_full().bg(c.bg).child(g)));
            body = body.children(
                right_sb_el.map(|e| div().h_full().when(!translucent, |d| d.bg(c.bg)).child(e)),
            );
        }
        body = body.children(drawers);

        let mut key_context = String::from("AppShell");
        if self.prompts.confirm_close.is_some()
            || self.prompts.restore.is_some()
            || self.prompts.apply.is_some()
        {
            key_context.push_str(" CloseConfirm");
        }
        if self.drawer.open.is_some() {
            key_context.push_str(" Drawer");
        }
        let confirm = self
            .prompts
            .confirm_close
            .as_ref()
            .map(|(_, why)| why.clone());
        let hint_el = self.prompts.hint.clone().map(|h| {
            let (a, b) = (shell.clone(), shell.clone());
            hint_card(
                &theme,
                &h,
                bar_height(l.bottom_bar) + m.margin + 8.0,
                move |action, _, cx| a.update(cx, |s, cx| s.run_hint_action(action, cx)),
                move |_, cx| b.update(cx, |s, cx| s.dismiss_hint(cx)),
            )
        });
        let overlay_bottom = bar_height(l.bottom_bar) + m.margin + 8.0;
        let update_el = self.update_card(
            // above the hint when both show
            overlay_bottom
                + if self.prompts.hint.is_some() {
                    52.0
                } else {
                    0.0
                },
            cx,
        );
        let approvals_el = self.approval_overlay(overlay_bottom, cx);
        let whats_new_el = self.whats_new_overlay(cx);
        let frame_stats = cx
            .try_global::<Services>()
            .map(|s| s.doctor.read(cx).frame_stats());
        div()
            .id("app-shell")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .when(!translucent, |d| d.bg(c.bg))
            .text_color(c.text)
            .font_family(ty.ui_family)
            .key_context(key_context.as_str())
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(
                cx.listener(|this, _: &NewTab, window, cx| this.open_new_tab_menu(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NewTerminalTab, window, cx| this.new_tab_here(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(id) = this.active_id() {
                    this.request_close(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(-1, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleLayoutPanel, window, cx| {
                this.toggle_layout_panel(window, cx)
            }))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|this, _: &NextAttention, _, cx| this.next_attention(cx)))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar_collapsed(cx)),
            )
            .on_action(cx.listener(|this, _: &FocusSidebar, window, cx| {
                this.focus_zone(Zone::Sidebar, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusTerminal, window, cx| {
                this.focus_zone(Zone::Main, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextZone, window, cx| this.cycle_zone(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PrevZone, window, cx| this.cycle_zone(-1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ui_palette::TogglePalette, window, cx| {
                    this.toggle_palette(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CancelOverlay, _, cx| this.close_overlay(cx)))
            .on_action(cx.listener(|this, _: &ConfirmClose, window, cx| {
                if let Some((id, _)) = this.prompts.confirm_close.clone() {
                    this.close_tab(id, window, cx);
                } else if this.prompts.restore.is_some() {
                    this.confirm_restore(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CancelClose, _, cx| {
                this.prompts.confirm_close = None;
                this.prompts.restore = None;
                this.prompts.apply = None;
                this.refresh(cx);
            }))
            .on_action(cx.listener(|this, _: &Jump1, w, cx| this.jump_tab(0, w, cx)))
            .on_action(cx.listener(|this, _: &Jump2, w, cx| this.jump_tab(1, w, cx)))
            .on_action(cx.listener(|this, _: &Jump3, w, cx| this.jump_tab(2, w, cx)))
            .on_action(cx.listener(|this, _: &Jump4, w, cx| this.jump_tab(3, w, cx)))
            .on_action(cx.listener(|this, _: &Jump5, w, cx| this.jump_tab(4, w, cx)))
            .on_action(cx.listener(|this, _: &Jump6, w, cx| this.jump_tab(5, w, cx)))
            .on_action(cx.listener(|this, _: &Jump7, w, cx| this.jump_tab(6, w, cx)))
            .on_action(cx.listener(|this, _: &Jump8, w, cx| this.jump_tab(7, w, cx)))
            .on_action(cx.listener(|this, _: &Jump9, w, cx| this.jump_tab(8, w, cx)))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| this.on_mouse_move(e, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_drag(cx)),
            )
            .child(ui_doctor::frame_begin())
            .child(body)
            .children(status_el.map(|s| div().w_full().bg(c.bg).child(s)))
            .children(zen_overlays)
            .children(hint_el)
            .children(update_el)
            .children(approvals_el)
            .children(confirm.map(|why| self.confirm_overlay(&why, cx)))
            .children(
                self.prompts
                    .restore
                    .as_ref()
                    .map(|r| self.restore_overlay(r, cx)),
            )
            .children(
                self.prompts
                    .apply
                    .as_ref()
                    .map(|p| self.apply_overlay(p, cx)),
            )
            .children(
                self.prompts
                    .new_tab_menu
                    .then(|| self.new_tab_menu_el(window, cx)),
            )
            .children(whats_new_el)
            // The palette is the last child: an absolute full-window overlay (nothing while closed).
            .child(self.palette.clone())
            .children(frame_stats.map(ui_doctor::frame_end))
            .into_any_element()
    }

    /// The sidebar / right panel as drawers (XS) or overlays (S) over the main area: a subtle scrim
    /// below the titlebar (so the window controls stay reachable) and the panel sliding in from its
    /// edge. Click on the scrim or Esc closes it.
    fn drawers(&self, window: &Window, m: Metrics, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(which) = self.drawer.open else {
            return Vec::new();
        };
        let epoch = self.drawer.epoch as usize;
        let w = drawer_width(self.host.width);
        let shell = cx.entity();
        let scrim = div()
            .id("drawer-scrim")
            .absolute()
            .top(px(TITLEBAR_H))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .bg(Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.0,
                a: 0.28,
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                shell.update(cx, |s, cx| s.close_overlay(cx))
            })
            .into_any_element();
        let animation = || Animation::new(BASE).with_easing(|t| SHEET.solve(t).clamp(0.0, 1.0));
        let panel = match which {
            Overlay::Left => {
                let x = if self.eff.layout.rail == RailVisibility::Visible {
                    RAIL_WIDTH
                } else {
                    0.0
                };
                let total = w + m.margin;
                div()
                    .id("drawer-left")
                    .bg(self.theme.colors.bg)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(x))
                    .w(px(total))
                    .occlude()
                    .child(cached(&self.chrome.drawer))
                    .with_animation(
                        ElementId::from(("drawer-in-left", epoch)),
                        animation(),
                        move |el, t| el.left(px(x - total * (1.0 - t))),
                    )
                    .into_any_element()
            }
            Overlay::Right => {
                let total = w + GUTTER + m.margin;
                div()
                    .id("drawer-right")
                    .bg(self.theme.colors.bg)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(total))
                    .occlude()
                    .child(self.right_card(window, true, m, true, cx))
                    .with_animation(
                        ElementId::from(("drawer-in-right", epoch)),
                        animation(),
                        move |el, t| el.right(px(-total * (1.0 - t))),
                    )
                    .into_any_element()
            }
        };
        vec![scrim, panel]
    }

    /// Zen mode: invisible hover strips at the top and left edges reveal the tab bar (with the
    /// window controls) and a floating sidebar. They hide again when the pointer leaves them, and
    /// stay while the layout popover is open. The top bar pushes the terminal down while shown
    /// (see `shell_root`), the floating sidebar is glass over it.
    fn zen_overlays(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.entity();
        let reveal = self.drawer.reveal;
        let keep = self.layout_panel.is_some() || self.chrome.titlebar.read(cx).menus_open();
        let strip = |edge: &'static str, top: bool| {
            let shell = shell.clone();
            let d = div().id(edge).absolute();
            let d = if top {
                d.top_0().left_0().right_0().h(px(EDGE_STRIP))
            } else {
                d.top(px(TITLEBAR_H)).left_0().bottom_0().w(px(EDGE_STRIP))
            };
            d.on_hover(move |hovered, _, cx| {
                if *hovered {
                    shell.update(cx, |s, cx| {
                        if top {
                            s.drawer.reveal.top = true;
                        } else {
                            s.drawer.reveal.left = true;
                        }
                        s.refresh(cx);
                    });
                }
            })
        };
        let hide = |top: bool| {
            let shell = shell.clone();
            move |hovered: &bool, _: &mut Window, cx: &mut App| {
                if !*hovered {
                    shell.update(cx, |s, cx| {
                        let keep =
                            s.layout_panel.is_some() || s.chrome.titlebar.read(cx).menus_open();
                        if top && !keep {
                            s.drawer.reveal.top = false;
                        } else if !top {
                            s.drawer.reveal.left = false;
                        }
                        s.refresh(cx);
                    });
                }
            }
        };

        let top_bar = if reveal.top || keep {
            div()
                .id("zen-top")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .occlude()
                .on_hover(hide(true))
                .child(cached_titlebar(&self.chrome.titlebar))
                .into_any_element()
        } else {
            strip("zen-top-strip", true).into_any_element()
        };

        let left_panel = if reveal.left {
            div()
                .id("zen-left")
                .absolute()
                .top(px(TITLEBAR_H))
                .left_0()
                .bottom_0()
                .w(px(ZEN_SIDEBAR_W + 8.0))
                .occlude()
                .on_hover(hide(false))
                .py(px(8.0))
                .pl(px(8.0))
                .child(cached(&self.chrome.zen))
                .into_any_element()
        } else {
            strip("zen-left-strip", false).into_any_element()
        };

        div()
            .absolute()
            .size_full()
            .child(left_panel)
            .child(top_bar)
    }
}
