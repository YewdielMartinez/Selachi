//! Diseño section (the embedded `ui_layout` panel + the unified-sidebar options) and the theme
//! preview cards / future palettes used by Apariencia.

use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use store::GroupBy;
use store::config::ThemeMode;
use theme::Theme;
use ui_kit::{Icon, WirePreset, Wireframe, icon};

use crate::extras::TITLEBAR_PANELS;
use crate::ui::{self, PillTone, note, plain_row, row, status_pill};
use crate::view::SettingsView;

/// Preview cell edge: a 32 x 20 wireframe is 96 x 60 px plus its 8 px padding.
const CELL: f32 = 3.0;
const TILE_W: f32 = 32.0 * CELL + 16.0;

/// Number of motion-kit palettes announced for a later release (docs/PLAN.md section 11).
pub const FUTURE_PALETTES: usize = 19;

fn preview(theme: &Theme) -> Wireframe {
    Wireframe::new(theme).rects(WirePreset::Terminal.rects()).cell(CELL)
}

impl SettingsView {
    /// The four theme cards. Each preview is drawn in that theme's own colors; "Sistema" is split
    /// in half (dark | light) because it follows Windows.
    pub(crate) fn theme_cards(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let current = self.cfg.appearance.mode;
        let cards: [(ThemeMode, &'static str, &'static str); 4] = [
            (ThemeMode::System, "Sistema", "Sigue a Windows"),
            (ThemeMode::Dark, "Oscuro", "Por defecto"),
            (ThemeMode::Light, "Claro", ""),
            (ThemeMode::Oled, "OLED", "Negro puro"),
        ];
        let this = cx.entity();
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(12.0))
            .children(cards.into_iter().map(|(mode, label, hint)| {
                let selected = mode == current;
                let tile: AnyElement = match mode {
                    ThemeMode::System => {
                        let half = TILE_W / 2.0;
                        div()
                            .flex()
                            .flex_row()
                            .w(px(TILE_W))
                            .overflow_hidden()
                            .child(div().w(px(half)).flex_none().overflow_hidden().child(preview(&Theme::dark())))
                            .child(
                                div()
                                    .w(px(half))
                                    .flex_none()
                                    .overflow_hidden()
                                    .child(div().ml(px(-half)).child(preview(&Theme::light()))),
                            )
                            .into_any_element()
                    }
                    ThemeMode::Dark => preview(&Theme::dark()).into_any_element(),
                    ThemeMode::Light => preview(&Theme::light()).into_any_element(),
                    ThemeMode::Oled => preview(&Theme::oled()).into_any_element(),
                };
                let backdrop = match mode {
                    ThemeMode::Light => Theme::light().colors.bg,
                    ThemeMode::Oled => Theme::oled().colors.bg,
                    _ => Theme::dark().colors.bg,
                };
                let this = this.clone();
                div()
                    .id(ElementId::Name(SharedString::from(format!("theme-card-{label}"))))
                    .occlude()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .p(px(8.0))
                    .rounded(px(14.0))
                    .cursor_pointer()
                    .border_2()
                    .border_color(if selected { c.accent_strong } else { c.surface_1 })
                    .bg(c.surface_2)
                    .hover(move |s| s.bg(c.surface_3))
                    .child(div().rounded(px(10.0)).overflow_hidden().bg(backdrop).child(tile))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .px(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(c.text)
                                            .child(label),
                                    )
                                    .when(!hint.is_empty(), |d| {
                                        d.child(div().text_size(px(11.0)).text_color(c.muted).child(hint))
                                    }),
                            )
                            .when(selected, |d| d.child(icon(Icon::Check, 14.0, c.text))),
                    )
                    .on_click(move |_, _, cx| {
                        this.update(cx, |s, cx| s.edit_cfg(cx, |c| c.appearance.mode = mode));
                    })
            }))
            .into_any_element()
    }

    /// "Paletas": the 19 motion-kit palettes as an honest placeholder (nothing is selectable).
    pub(crate) fn palettes_group(&self, narrow: bool) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let chips = (1..=FUTURE_PALETTES).map(|i| {
            let base = i == 1;
            div()
                .h(px(28.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(9.0))
                .bg(c.surface_2)
                .text_size(px(12.0))
                .text_color(if base { c.text } else { c.outline })
                .when(!base, |d| d.opacity(0.7))
                .child(if base { "black (base)".to_string() } else { format!("Paleta {i}") })
        });
        ui::group(
            &t,
            Some("Paletas"),
            vec![
                row(
                    &t,
                    narrow,
                    "Paletas de motion-kit",
                    Some("Las 19 paletas se importarán con el mismo script que generó el tema Oscuro. Todavía no se puede elegir ninguna.".into()),
                    status_pill(&t, "Próximamente", PillTone::Neutral),
                    None,
                )
                .into_any_element(),
                plain_row(div().flex().flex_row().flex_wrap().gap(px(8.0)).children(chips)).into_any_element(),
            ],
        )
        .into_any_element()
    }

    pub(crate) fn design(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;

        // ----- the embedded layout panel -----
        let panel = match &self.layout_panel {
            Some(p) => p.clone(),
            None => {
                let this = cx.entity();
                let p = ui_layout::layout_panel(
                    self.cfg.layout.clone(),
                    self.cwd.clone(),
                    t,
                    move |layout, _window, cx| {
                        this.update(cx, |s, cx| s.edit_cfg(cx, |c| c.layout = layout));
                    },
                    cx,
                );
                self.layout_panel = Some(p.clone());
                p
            }
        };
        let panel_block = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(
                div()
                    .px(px(4.0))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.muted)
                    .child("Layout"),
            )
            .child(
                // The panel is 560 px wide by itself (see the crate docs, "embedded mode").
                div().w_full().flex_none().overflow_hidden().rounded(px(theme::Radius::SM)).child(panel),
            )
            .child(note(
                &t,
                "Es el mismo panel del botón de la barra de título (Ctrl+Shift+L): los cambios se aplican al instante.",
            ));

        // ----- unified sidebar (docs/olas/ola-5.md 5.10) -----
        let sb = self.extras.sidebar.clone();
        let toggle = |this: &mut Self, cx: &mut Context<Self>, id: &'static str, on: bool, f: fn(&mut crate::extras::SidebarPrefs, bool)| {
            this.toggle(cx, id, on, move |s, v, cx| s.edit_extras("sidebar", cx, |e| f(&mut e.sidebar, v)))
        };
        let actions = toggle(self, cx, "sb-actions", sb.show_actions, |p, v| p.show_actions = v);
        let icons = toggle(self, cx, "sb-icons", sb.panel_icons, |p, v| p.panel_icons = v);
        let nested = toggle(self, cx, "sb-nested", sb.nested_tabs, |p, v| p.nested_tabs = v);
        let footer = toggle(self, cx, "sb-footer", sb.footer_usage, |p, v| p.footer_usage = v);
        let row_model = toggle(self, cx, "sb-model", sb.row_model, |p, v| p.row_model = v);
        let row_time = toggle(self, cx, "sb-time", sb.row_time, |p, v| p.row_time = v);

        let group_by = self.cfg.layout.resolve().sidebar.group_by;
        let group_ix = match group_by {
            GroupBy::Project => 0,
            GroupBy::Status => 1,
            GroupBy::Agent => 2,
            GroupBy::None => 3,
        };
        let group_control = self.segmented(cx, "sb-group", ["Proyecto", "Estado", "Agente", "Ninguno"], group_ix, |s, ix, cx| {
            let g = [GroupBy::Project, GroupBy::Status, GroupBy::Agent, GroupBy::None][ix.min(3)];
            s.edit_cfg(cx, |c| {
                let mut r = c.layout.resolve();
                r.sidebar.group_by = g;
                c.layout.set_resolved(&r);
            });
        });

        let this = cx.entity();
        let chips = div().flex().flex_row().flex_wrap().gap(px(8.0)).children(TITLEBAR_PANELS.iter().map(|(id, label)| {
            let on = sb.titlebar_has(id);
            let this = this.clone();
            let id = *id;
            div()
                .id(ElementId::Name(SharedString::from(format!("tb-panel-{id}"))))
                .occlude()
                .h(px(28.0))
                .px(px(12.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .rounded(px(9.0))
                .cursor_pointer()
                .bg(if on { c.surface_4 } else { c.surface_2 })
                .text_size(px(12.0))
                .text_color(if on { c.text } else { c.muted })
                .hover(move |s| s.bg(c.surface_3).text_color(c.text))
                .when(on, |d| d.child(icon(Icon::Check, 12.0, c.text)))
                .child(*label)
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| s.edit_extras("sidebar", cx, |e| e.sidebar.set_titlebar(id, !on)));
                })
        }));

        let sidebar = ui::group(
            &t,
            Some("Barra lateral unificada"),
            vec![
                row(
                    &t,
                    narrow,
                    "Acciones del encabezado",
                    Some("«Nueva pestaña» y «Buscar» arriba, con su atajo.".into()),
                    actions,
                    None,
                )
                .into_any_element(),
                row(&t, narrow, "Fila de iconos de paneles", Some("Terminal, git, archivos, agentes, historial…".into()), icons, None)
                    .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Pestañas e hilos dentro de su espacio",
                    Some("Anidados y plegables bajo el proyecto o el worktree. Apagado = lista plana.".into()),
                    nested,
                    None,
                )
                .into_any_element(),
                row(&t, narrow, "Uso al pie", Some("Medidor compacto y acceso a Ajustes.".into()), footer, None)
                    .into_any_element(),
                row(&t, narrow, "Agrupar por", Some("Cómo se ordenan los espacios.".into()), group_control, None)
                    .into_any_element(),
                row(&t, narrow, "Mostrar el modelo en cada fila", Some("Modelo y rama junto al título.".into()), row_model, None)
                    .into_any_element(),
                row(&t, narrow, "Mostrar el tiempo en cada fila", Some("«ahora», «5 min», «2 h»…".into()), row_time, None)
                    .into_any_element(),
                row(
                    &t,
                    true, // stacked: the chips need the full width
                    "Botones de paneles en la barra de título",
                    Some("Los paneles que se abren a la derecha, junto a los controles de la ventana.".into()),
                    chips,
                    None,
                )
                .into_any_element(),
            ],
        );

        vec![
            panel_block.into_any_element(),
            sidebar.into_any_element(),
            note(
                &t,
                "Estas opciones se guardan ya en config.toml ([layout.sidebar] y [layout.titlebar]); la barra lateral unificada las aplicará cuando esté disponible.",
            )
            .into_any_element(),
        ]
    }
}
