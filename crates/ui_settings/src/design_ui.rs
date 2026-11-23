//! Diseño section (the embedded `ui_layout` panel + the unified-sidebar options) and the theme
//! preview cards / future palettes used by Apariencia.

use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use store::config::ThemeMode;
use theme::{Radius, Theme};
use ui_kit::{Icon, WirePreset, Wireframe, icon};

use crate::ui::{self, PillTone, note, plain_row, row, status_pill};
use crate::view::SettingsView;

/// Preview cell edge: a 32 x 20 wireframe is 96 x 60 px plus its 8 px padding.
const CELL: f32 = 3.0;
const TILE_W: f32 = 32.0 * CELL + 16.0;

/// The motion-kit palettes announced for a later release (docs/PLAN.md section 11), by their real
/// names. `black` is the base theme; none can be chosen yet.
pub const FUTURE_PALETTES: [&str; 19] = [
    "black",
    "blue",
    "purple",
    "neogreen",
    "green",
    "red",
    "orange",
    "pink",
    "pastel-pink",
    "brown",
    "super-blue",
    "cold-blue",
    "modern-1",
    "gray",
    "oled",
    "funny",
    "new",
    "new2",
    "black-green",
];

fn preview(theme: &Theme) -> Wireframe {
    Wireframe::new(theme).rects(WirePreset::Terminal.rects()).cell(CELL)
}

impl SettingsView {
    /// The four theme cards. Each preview is drawn in that theme's own colors; "Sistema" is split
    /// in half (dark | light) because it follows Windows.
    pub(crate) fn theme_cards(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
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
                    // A tab stop: Enter / Space run the click; `Tab` is moved by the view root.
                    .tab_index(0)
                    .focus(move |s| s.border_color(c.accent))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .p(px(8.0))
                    .rounded(px(Radius::SM))
                    .cursor_pointer()
                    .border_2()
                    .border_color(if selected { c.accent_strong } else { c.surface_1 })
                    .bg(c.surface_2)
                    .hover(move |s| s.bg(c.surface_3))
                    .child(div().rounded(px(Radius::PILL)).overflow_hidden().bg(backdrop).child(tile))
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
                                            .text_size(px(ty.ui_size_md))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(c.text)
                                            .child(label),
                                    )
                                    .when(!hint.is_empty(), |d| {
                                        d.child(div().text_size(px(ty.ui_size_xs)).text_color(c.muted).child(hint))
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
        let ty = t.typography;
        let chips = FUTURE_PALETTES.iter().enumerate().map(|(i, name)| {
            let base = i == 0;
            div()
                .h(px(28.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(Radius::CONTROL))
                .bg(c.surface_2)
                .text_size(px(ty.ui_size_sm))
                .text_color(if base { c.text } else { c.outline })
                .when(!base, |d| d.opacity(0.7))
                .child(if base { "black (base)".to_string() } else { (*name).to_string() })
        });
        ui::group(
            &t,
            Some("Paletas"),
            vec![
                row(
                    &t,
                    narrow,
                    "Paletas de motion-kit",
                    Some("Las 19 paletas (black, blue, purple, neogreen…) se importarán con el mismo script que generó el tema Oscuro. Todavía no se puede elegir ninguna.".into()),
                    status_pill(&t, "Próximamente", PillTone::Neutral),
                    None,
                )
                .into_any_element(),
                plain_row(div().flex().flex_row().flex_wrap().gap(px(8.0)).children(chips)).into_any_element(),
            ],
        )
        .into_any_element()
    }

    pub(crate) fn design(&mut self, _narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;

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
                // Full width, no card, no scroll of its own: this screen scrolls.
                p.update(cx, |p, cx| p.set_embedded(true, cx));
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
                    .text_size(px(ty.ui_size_xs))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.muted)
                    .child("Layout"),
            )
            .child(div().w_full().min_w_0().child(panel))
            .child(note(
                &t,
                "Es el mismo panel del botón de la barra de título (Ctrl+Shift+L): los cambios se aplican al instante.",
            ));

        vec![panel_block.into_any_element()]
    }
}
