//! The header and the toolbar of the map: title and placement button, view switch, search box,
//! status chips with their counts and the organization panel (grouping, order, hide old).

use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use panel_api::PanelZone;
use theme::{Radius, Theme};
use ui_kit::{Button, Icon, IconButton, SegmentedControl, icon};

use crate::events::AgentMapEvent;
use crate::fmt;
use crate::organize::{filter_counts, scope_totals};
use crate::prefs::{
    GroupBy, HIDE_OLDER_CHOICES, SortBy, StatusFilter, hide_older_label,
};
use crate::view::{AgentMapView, MAP_ICON, MapMode, Shown};

/// A small pill that toggles (status filters, «Todas las sesiones», fold all).
pub(crate) fn pill_toggle(
    id: impl Into<SharedString>,
    theme: &Theme,
    label: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .id(id.into())
        .flex_none()
        .h(px(24.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(Radius::PILL))
        .cursor_pointer()
        .text_size(px(ty.ui_size_sm))
        .when(active, |d| d.bg(c.surface_3).text_color(c.text))
        .when(!active, |d| {
            d.bg(c.surface_2)
                .text_color(c.muted)
                .hover(|s| s.bg(c.surface_3))
        })
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(label.into())
}

fn row_label(theme: &Theme, text: &'static str) -> impl IntoElement + use<> {
    let c = theme.colors;
    div()
        .w(px(92.0))
        .flex_none()
        .text_size(px(theme.typography.ui_size_sm))
        .text_color(c.muted)
        .child(text)
}

impl AgentMapView {
    pub(crate) fn header(
        &mut self,
        shown: &Shown,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let loaded = self.model.read(cx).is_loaded();
        let forest = shown.forest.clone();
        let many = forest.roots.len() > 1;
        let prefs = &shown.prefs;
        let scope = shown.scope;
        let totals = scope.map(|s| scope_totals(&forest, s));
        let summary: Option<String> = totals.filter(|_| loaded).map(|t| {
            let mut parts: Vec<String> = Vec::new();
            if t.runs == 0 && t.external == 0 {
                parts.push("Sin delegaciones".to_string());
            } else {
                if t.runs > 0 {
                    parts.push(match t.runs {
                        1 => "1 delegación".to_string(),
                        n => format!("{n} delegaciones"),
                    });
                }
                if t.external > 0 {
                    parts.push(format!("{} fuera de Selachi", t.external));
                }
            }
            if t.running > 0 {
                parts.push(format!("{} en marcha", t.running));
            }
            if t.waiting > 0 {
                parts.push(format!("{} esperan aprobación", t.waiting));
            }
            if t.failed > 0 {
                parts.push(format!("{} fallaron", t.failed));
            }
            if t.tokens > 0 {
                parts.push(format!("{} tokens", fmt::tokens(t.tokens)));
            }
            if t.cost_usd > 0.0 {
                parts.push(fmt::cost(t.cost_usd));
            }
            parts.join(" · ")
        });
        let this = cx.entity();

        // ---- title row with the placement button ----
        let in_tab = self.ctx.zone == PanelZone::Main;
        let place_ev = if in_tab {
            AgentMapEvent::DockRight
        } else {
            AgentMapEvent::OpenInTab
        };
        let place_label = if in_tab {
            "Acoplar a la derecha"
        } else {
            "Abrir en pestaña"
        };
        let place_btn: AnyElement = {
            let this = this.clone();
            let ev = place_ev.clone();
            if narrow {
                IconButton::new(
                    "agentmap-place",
                    &t,
                    if in_tab { Icon::Restore } else { Icon::Maximize },
                )
                .tooltip(place_label)
                .on_click(move |_, _, cx| {
                    let ev = ev.clone();
                    this.update(cx, |v, cx| v.emit_action(ev, cx));
                })
                .into_any_element()
            } else {
                Button::ghost("agentmap-place", &t, place_label)
                    .icon(if in_tab { Icon::Restore } else { Icon::Maximize })
                    .on_click(move |_, _, cx| {
                        let ev = ev.clone();
                        this.update(cx, |v, cx| v.emit_action(ev, cx));
                    })
                    .into_any_element()
            }
        };
        let title_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(icon(MAP_ICON, 16.0, c.muted))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(ty.ui_size_lg))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.text)
                    .child("Mapa de subagentes"),
            )
            .when(many && !narrow && !prefs.all_sessions, |d| {
                d.child(self.root_select.clone())
            })
            .child(place_btn);

        // ---- view switch + summary ----
        let modes = {
            let this = this.clone();
            SegmentedControl::new(
                "agentmap-mode",
                &t,
                MapMode::ALL
                    .iter()
                    .map(|m| SharedString::from(if narrow { m.short_label() } else { m.label() })),
            )
            .selected(
                MapMode::ALL
                    .iter()
                    .position(|m| *m == prefs.mode)
                    .unwrap_or(0),
            )
            .track(c.surface_2)
            .on_change(move |ix, _, cx| {
                let mode = MapMode::ALL.get(ix).copied().unwrap_or(MapMode::Tree);
                this.update(cx, |v, cx| v.set_mode(mode, cx));
            })
        };

        // ---- search + «Organizar» ----
        let organize = {
            let this = this.clone();
            pill_toggle(
                "agentmap-organize",
                &t,
                "Organizar",
                self.options_open,
                move |_, cx| {
                    this.update(cx, |v, cx| {
                        v.options_open = !v.options_open;
                        cx.notify();
                    })
                },
            )
        };
        let search_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(div().flex_1().min_w(px(0.0)).child(self.search.clone()))
            .child(organize);

        // ---- status chips with counts ----
        let counts = scope.map(|s| filter_counts(&forest, s)).unwrap_or([0; 5]);
        let mut chips = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for (ix, f) in StatusFilter::ALL.into_iter().enumerate() {
            let this = this.clone();
            let label = if f == StatusFilter::All {
                f.label().to_string()
            } else {
                format!("{} {}", f.label(), counts[ix])
            };
            chips = chips.child(pill_toggle(
                format!("agentmap-filter-{ix}"),
                &t,
                label,
                prefs.filter == f,
                move |_, cx| {
                    this.update(cx, |v, cx| {
                        v.model.update(cx, |m, cx| m.update_prefs(|p| p.filter = f, cx))
                    })
                },
            ));
        }

        let panel = self.options_open.then(|| self.options_panel(shown, narrow, cx));

        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(title_row)
            .when(many && narrow && !prefs.all_sessions, |d| {
                d.child(self.root_select.clone())
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(modes)
                    .when_some(summary, |d, s| {
                        d.child(
                            div()
                                .min_w(px(0.0))
                                .text_size(px(ty.ui_size_sm))
                                .text_color(c.muted)
                                .child(SharedString::from(s)),
                        )
                    }),
            )
            .child(search_row)
            .child(chips)
            .children(panel)
            .into_any_element()
    }

    /// Grouping, order, hide old, all sessions and fold all.
    fn options_panel(&mut self, shown: &Shown, _narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let prefs = &shown.prefs;
        let this = cx.entity();
        let seg = |id: &'static str, labels: Vec<&'static str>, sel: usize| {
            SegmentedControl::new(
                id,
                &t,
                labels.into_iter().map(SharedString::from),
            )
            .selected(sel)
            .track(c.surface_3)
        };
        let group = {
            let this = this.clone();
            seg(
                "agentmap-group",
                GroupBy::ALL.iter().map(|g| g.label()).collect(),
                GroupBy::ALL.iter().position(|g| *g == prefs.group).unwrap_or(0),
            )
            .on_change(move |ix, _, cx| {
                let g = GroupBy::ALL.get(ix).copied().unwrap_or_default();
                this.update(cx, |v, cx| {
                    v.model.update(cx, |m, cx| m.update_prefs(|p| p.group = g, cx))
                });
            })
        };
        let sort = {
            let this = this.clone();
            seg(
                "agentmap-sort",
                SortBy::ALL.iter().map(|s| s.label()).collect(),
                SortBy::ALL.iter().position(|s| *s == prefs.sort).unwrap_or(0),
            )
            .on_change(move |ix, _, cx| {
                let s = SortBy::ALL.get(ix).copied().unwrap_or_default();
                this.update(cx, |v, cx| {
                    v.model.update(cx, |m, cx| m.update_prefs(|p| p.sort = s, cx))
                });
            })
        };
        let hide = {
            let this = this.clone();
            seg(
                "agentmap-hide",
                HIDE_OLDER_CHOICES.iter().map(|m| hide_older_label(*m)).collect(),
                HIDE_OLDER_CHOICES
                    .iter()
                    .position(|m| *m == shown.session.hide_older_min)
                    .unwrap_or(0),
            )
            .on_change(move |ix, _, cx| {
                let m = HIDE_OLDER_CHOICES.get(ix).copied().flatten();
                this.update(cx, |v, cx| {
                    v.model
                        .update(cx, |md, cx| md.update_session(|s| s.hide_older_min = m, cx))
                });
            })
        };
        let line = |label: &'static str, body: AnyElement| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .child(row_label(&t, label))
                .child(body)
        };
        let many = shown.forest.roots.len() > 1;
        let mut actions = div().flex().flex_row().flex_wrap().gap(px(6.0));
        if many {
            let this = this.clone();
            actions = actions.child(pill_toggle(
                "agentmap-all",
                &t,
                "Todas las sesiones",
                prefs.all_sessions,
                move |_, cx| {
                    this.update(cx, |v, cx| {
                        v.model.update(cx, |m, cx| {
                            m.update_prefs(|p| p.all_sessions = !p.all_sessions, cx)
                        })
                    })
                },
            ));
        }
        if prefs.mode == MapMode::Tree {
            for (id, label, fold) in [
                ("agentmap-fold", "Plegar todo", true),
                ("agentmap-unfold", "Desplegar todo", false),
            ] {
                let this = this.clone();
                actions = actions.child(pill_toggle(id, &t, label, false, move |_, cx| {
                    this.update(cx, |v, cx| v.collapse_all(fold, cx))
                }));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(Radius::MD))
            .bg(c.surface_2)
            .when(prefs.mode == MapMode::Tree, |d| {
                d.child(line("Agrupar por", group.into_any_element()))
            })
            .child(line("Ordenar por", sort.into_any_element()))
            .child(line("Ocultar terminados", hide.into_any_element()))
            .child(
                div()
                    .text_size(px(t.typography.ui_size_xs))
                    .text_color(c.muted)
                    .child("«Ocultar terminados» quita los que acabaron hace más de ese tiempo; vale solo hasta cerrar Selachi."),
            )
            .child(actions)
            .into_any_element()
    }
}

/// `Entity` helper so the compiler sees the types used in closures above.
#[allow(dead_code)]
fn _assert_types(_: Entity<AgentMapView>) {}
