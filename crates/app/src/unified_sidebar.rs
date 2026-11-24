//! Body of the unified sidebar (preset "Espacios unificados", docs/olas/ola-5.md 5.10): quick
//! actions, a row of panel buttons, the spaces with their tabs nested as rows, and a footer with
//! the usage meter and the settings button. `collapsed` is the icon-only column (Ctrl+B, S class).
//!
//! Only real data is painted: spaces come from the open tabs and the store's favorites, rows from
//! the tabs' own `TabInfo`. Nothing here ticks: the elapsed time is recomputed on render and the
//! only animation is the shark-bars spinner of a tab that is running a command.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use gpui::{
    App, ClickEvent, Div, Entity, Hsla, IntoElement, SharedString, Stateful, Transformation,
    Window, div, prelude::*, px, radians,
};
use panel_api::{TabId, TabInfo};
use store::ResolvedLayout;
use theme::Theme;
use ui_kit::{
    DitherMeter, Icon, IconButton, SectionLabel, StatusGlyph, glass_hover, glass_selected, icon,
};

use crate::app_shell::AppShell;
use crate::rail::RailItem;
use crate::spaces::{Space, row_subtitle, row_time};

/// What the unified sidebar needs to paint, prepared by the shell.
pub struct UnifiedProps<'a> {
    pub theme: &'a Theme,
    pub layout: &'a ResolvedLayout,
    pub tabs: &'a [TabInfo],
    pub active: Option<TabId>,
    pub shell: &'a Entity<AppShell>,
    /// The column is glass: pills are translucent.
    pub glass: bool,
    pub spaces: Vec<Space>,
    /// Keys of the spaces that are folded.
    pub folded: &'a HashSet<PathBuf>,
    /// The tab row under the pointer (state kept by the shell).
    pub hovered_row: Option<TabId>,
    /// The space header under the pointer.
    pub hovered_space: Option<usize>,
    pub usage: Usage,
}

/// The compact usage meter of the footer.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    /// `0.0..=1.0`; `None` = unknown (an empty meter, nothing invented).
    pub fraction: Option<f32>,
    /// `65 %`, or `estimado` when there is no official figure.
    pub label: SharedString,
}

fn pill_bg(theme: &Theme, glass: bool, selected: bool) -> Hsla {
    match (glass, selected) {
        (true, true) => glass_selected(theme),
        (true, false) => glass_hover(theme),
        (false, true) => theme.colors.surface_3.into(),
        (false, false) => theme.colors.surface_2.into(),
    }
}

/// The x of a tab row paints on the selected row and on the hovered one, nowhere else.
/// The x shows only on the row under the pointer, selected or not: at rest every row keeps
/// its status glyph and time visible (Ctrl+W still closes the selected tab).
pub fn close_visible(_selected: bool, hovered: Option<TabId>, id: TabId) -> bool {
    hovered == Some(id)
}

/// The new hovered key after `key` reported `hovered`: entering takes the slot, leaving clears it
/// only when the slot still holds `key` (a late "left" of the previous row must not wipe the next).
pub fn hover_transition<K: PartialEq + Copy>(
    current: Option<K>,
    key: K,
    hovered: bool,
) -> Option<K> {
    if hovered {
        Some(key)
    } else if current == Some(key) {
        None
    } else {
        current
    }
}

type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// "Nueva pestaña  Ctrl+N" / "Buscar  Ctrl+K".
fn action_row(
    p: &UnifiedProps,
    id: &'static str,
    ic: Icon,
    label: &'static str,
    chord: &'static str,
    on_click: Click,
) -> Stateful<Div> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let hover = pill_bg(t, p.glass, false);
    div()
        .id(id)
        .occlude()
        .w_full()
        .h(px(32.0))
        .px(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .rounded(px(10.0))
        .cursor_pointer()
        .text_size(px(ty.ui_size_md))
        .text_color(c.text)
        .hover(move |s| s.bg(hover))
        .on_click(move |e, w, cx| on_click(e, w, cx))
        .child(icon(ic, 16.0, c.muted))
        .child(div().flex_1().child(label))
        .child(
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.outline)
                .child(chord),
        )
}

/// The row of small panel buttons.
fn panel_icons(p: &UnifiedProps, items: Vec<RailItem>) -> Div {
    let mut row = div().flex().flex_row().flex_wrap().gap(px(1.0));
    for item in items {
        let RailItem {
            id,
            icon: ic,
            title,
            active,
            on_click,
        } = item;
        let mut b = IconButton::new(SharedString::from(format!("sb-{id}")), p.theme, ic)
            .size(28.0)
            .glass(p.glass)
            .active(active)
            .tooltip(title);
        if let Some(f) = on_click {
            b = b.on_click(move |_, window, cx| f(window, cx));
        }
        row = row.child(b);
    }
    row
}

/// A tab as a nested row: mark, title and model / branch · state, elapsed time and status glyph.
fn tab_row(
    p: &UnifiedProps,
    info: &TabInfo,
    nested: bool,
    now: Instant,
) -> impl IntoElement + use<> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let id = info.id;
    let selected = Some(id) == p.active;
    let l = &p.layout.sidebar;
    let subtitle = row_subtitle(info, l.row_model);
    let time = row_time(info, l.row_time, now);
    let (shell_sel, shell_close, shell_hover) = (p.shell.clone(), p.shell.clone(), p.shell.clone());
    let show_close = close_visible(selected, p.hovered_row, id);
    let lead = crate::agents::lead_icon(&info.icon, 16.0, if selected { c.text } else { c.muted });
    let bg = pill_bg(t, p.glass, selected);
    let close_bg: Hsla = if p.glass {
        glass_selected(t)
    } else {
        c.surface_4.into()
    };
    div().when(nested, |d| d.pl(px(12.0))).child(
        div()
            .id(("urow", id))
            .relative()
            .occlude()
            .w_full()
            .h(px(if subtitle.is_some() { 40.0 } else { 32.0 }))
            .my(px(1.0))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(10.0))
            .rounded(px(10.0))
            .cursor_pointer()
            .when(selected, move |d| d.bg(bg))
            .when(!selected, move |d| d.hover(move |s| s.bg(bg)))
            .on_hover(move |hovered, _, cx| {
                shell_hover.update(cx, |s, cx| s.set_row_hover(id, *hovered, cx))
            })
            .on_click(move |_, window, cx| {
                shell_sel.update(cx, |s, cx| s.select_tab(id, window, cx))
            })
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.text)
                            .child(info.title.clone()),
                    )
                    .children(subtitle.map(|s| {
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child(SharedString::from(s))
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(1.0))
                    // Swap, not stack: the glyph and the time keep their space (no reflow of
                    // the title) but are not painted while the x takes their place.
                    .when(show_close, |d| d.invisible())
                    .children(info.status.map(|s| StatusGlyph::new(t, s)))
                    .children(time.map(|tm| {
                        div()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child(SharedString::from(tm))
                    })),
            )
            .children(show_close.then(|| {
                div()
                    .absolute()
                    .right(px(6.0))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        div().rounded(px(7.0)).bg(close_bg).child(
                            IconButton::new(
                                SharedString::from(format!("urow-close-{}", id.as_u64())),
                                t,
                                Icon::Close,
                            )
                            .glass(p.glass)
                            .passthrough(true)
                            .size(22.0)
                            .icon_size(10.0)
                            .tooltip("Cerrar (Ctrl+W)")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                shell_close.update(cx, |s, cx| s.request_close(id, window, cx));
                            }),
                        ),
                    )
            })),
    )
}

/// A space header: fold chevron, name, branch and dirty dot, and "+" (a new tab there).
fn space_header(p: &UnifiedProps, ix: usize, s: &Space, folded: bool) -> impl IntoElement + use<> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let key = s.key();
    let root = s.root.clone();
    let open = s.is_open();
    let hover = pill_bg(t, p.glass, false);
    let show_plus = p.hovered_space == Some(ix);
    let (shell, shell_plus, shell_hover, shell_drop) = (
        p.shell.clone(),
        p.shell.clone(),
        p.shell.clone(),
        p.shell.clone(),
    );
    let theme = *t;
    let chevron = {
        let g = icon(Icon::ChevronRight, 12.0, c.muted);
        if open && !folded {
            g.with_transformation(Transformation::rotate(radians(std::f32::consts::FRAC_PI_2)))
        } else {
            g
        }
    };
    let dirty_dot = s.dirty.then(|| {
        div()
            .size(px(6.0))
            .flex_none()
            .rounded_full()
            .bg(gpui::rgb(t.terminal.ansi[3]))
    });
    let can_add = !root.as_os_str().is_empty();
    div()
        .id(("space", ix))
        .relative()
        .occlude()
        .w_full()
        .h(px(30.0))
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(move |st| st.bg(hover))
        .on_hover(move |hovered, _, cx| {
            shell_hover.update(cx, |sh, cx| sh.set_space_hover(ix, *hovered, cx))
        })
        .on_click(move |_, window, cx| {
            let (key, root) = (key.clone(), root.clone());
            shell.update(cx, |sh, cx| {
                if open {
                    sh.toggle_space(key, cx);
                } else {
                    sh.new_tab_in(root, window, cx);
                }
            });
        })
        .child(chevron)
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_md))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if open { c.text } else { c.muted })
                .child(SharedString::from(s.name.clone())),
        )
        .children(s.branch.clone().map(|b| {
            div()
                .max_w(px(90.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(SharedString::from(b))
        }))
        .children(dirty_dot)
        .children(can_add.then(|| {
            let root = s.root.clone();
            // Reserved when hidden: the "+" appearing never reflows the name / branch.
            if !show_plus {
                return div().size(px(22.0)).flex_none();
            }
            div().child(
                IconButton::new(SharedString::from(format!("space-add-{ix}")), t, Icon::Plus)
                    .glass(p.glass)
                    .passthrough(true)
                    .size(22.0)
                    .icon_size(12.0)
                    .tooltip("Nueva pestaña aquí")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let root = root.clone();
                        shell_plus.update(cx, |sh, cx| sh.new_tab_in(root, window, cx));
                    }),
            )
        }))
        .child(
            ui_kit::DropZone::new(
                ("space-drop", ix),
                &theme,
                "Soltar para abrir una terminal aquí",
            )
            .radius(8.0)
            .compact(true)
            .on_drop(move |paths, window, cx| {
                let paths = paths.to_vec();
                shell_drop.update(cx, |sh, cx| sh.drop_on_space(&paths, window, cx));
            }),
        )
}

fn footer(p: &UnifiedProps) -> impl IntoElement + use<> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let shell = p.shell.clone();
    let usage_shell = p.shell.clone();
    div()
        .flex_none()
        .h(px(44.0))
        .px(px(12.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .id("sb-usage")
                .occlude()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    usage_shell.update(cx, |s, cx| {
                        s.toggle_right_panel(panel_api::PanelId::new(ui_usage::PANEL_ID), cx)
                    });
                })
                .child(DitherMeter::new(t).segments(6).value(p.usage.fraction))
                .child(
                    div()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .whitespace_nowrap()
                        .child(SharedString::from(format!("Uso  {}", p.usage.label))),
                ),
        )
        .child(
            IconButton::new("sb-settings", t, Icon::Settings)
                .glass(p.glass)
                .size(30.0)
                .tooltip("Ajustes (Ctrl+,)")
                .on_click(move |_, window, cx| {
                    shell.update(cx, |s, cx| s.open_settings(window, cx))
                }),
        )
}

/// The chevron at the top of the sidebar that folds it to icons (points left).
pub fn collapse_button(
    theme: &Theme,
    glass: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let c = theme.colors;
    let hover = pill_bg(theme, glass, false);
    div()
        .id("sb-collapse")
        .occlude()
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tooltip(ui_kit::Tooltip::text(
            theme,
            "Plegar la barra lateral (Ctrl+B)",
        ))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(
            icon(Icon::ChevronRight, 16.0, c.muted)
                .with_transformation(Transformation::rotate(radians(std::f32::consts::PI))),
        )
}

/// The full sidebar body: scrolling middle plus the pinned footer.
pub fn unified_content(p: &UnifiedProps, panel_items: Vec<RailItem>) -> Stateful<Div> {
    let t = p.theme;
    let l = &p.layout.sidebar;
    let now = Instant::now();
    let mut body = div()
        .id("usb-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .p(px(8.0));
    if l.show_actions {
        let (a, b) = (p.shell.clone(), p.shell.clone());
        body = body.child(
            div()
                .flex()
                .flex_col()
                .child(action_row(
                    p,
                    "sb-act-new",
                    Icon::Plus,
                    "Nueva pestaña",
                    "Ctrl+N",
                    Box::new(move |_, window, cx| {
                        a.update(cx, |s, cx| s.open_new_tab_menu(window, cx))
                    }),
                ))
                .child(action_row(
                    p,
                    "sb-act-search",
                    Icon::Search,
                    "Buscar",
                    "Ctrl+K",
                    Box::new(move |_, window, cx| b.update(cx, |s, cx| s.open_palette(window, cx))),
                )),
        );
    }
    if l.panel_icons && !panel_items.is_empty() {
        body = body.child(panel_icons(p, panel_items));
    }
    let mut spaces = div().flex().flex_col().gap(px(4.0));
    spaces = spaces.child(
        div()
            .px(px(10.0))
            .pt(px(2.0))
            .pb(px(2.0))
            .child(SectionLabel::new(t, "Espacios").uppercase(true)),
    );
    for (ix, s) in p.spaces.iter().enumerate() {
        let folded = p.folded.contains(&s.key());
        let mut block = div().flex().flex_col();
        if s.headless {
            for &ti in &s.tabs {
                block = block.child(tab_row(p, &p.tabs[ti], false, now));
            }
            spaces = spaces.child(block);
            continue;
        }
        block = block.child(space_header(p, ix, s, folded));
        if s.is_open() && !folded {
            for &ti in &s.tabs {
                block = block.child(tab_row(p, &p.tabs[ti], true, now));
            }
        }
        spaces = spaces.child(block);
    }
    body = body.child(spaces);
    let mut col = div().id("usb").size_full().flex().flex_col().child(body);
    if l.footer_usage {
        col = col.child(footer(p));
    }
    col
}

/// The icon-only column: expand, new tab, search, panel buttons, one icon per tab and settings.
pub fn unified_collapsed(p: &UnifiedProps, panel_items: Vec<RailItem>) -> Stateful<Div> {
    let t = p.theme;
    let l = &p.layout.sidebar;
    let shell = p.shell.clone();
    let mut col = div()
        .id("usb-collapsed")
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .py(px(8.0));
    let mut top = div()
        .id("usb-collapsed-scroll")
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0));
    {
        let s = shell.clone();
        top = top.child(
            IconButton::new("sb-expand", t, Icon::ChevronRight)
                .glass(p.glass)
                .tooltip("Expandir la barra lateral (Ctrl+B)")
                .on_click(move |_, _, cx| s.update(cx, |s, cx| s.toggle_sidebar_collapsed(cx))),
        );
    }
    if l.show_actions {
        let (a, b) = (shell.clone(), shell.clone());
        top = top
            .child(
                IconButton::new("sb-c-new", t, Icon::Plus)
                    .glass(p.glass)
                    .tooltip("Nueva pestaña (Ctrl+N)")
                    .on_click(move |_, window, cx| {
                        a.update(cx, |s, cx| s.open_new_tab_menu(window, cx))
                    }),
            )
            .child(
                IconButton::new("sb-c-search", t, Icon::Search)
                    .glass(p.glass)
                    .tooltip("Buscar (Ctrl+K)")
                    .on_click(move |_, window, cx| {
                        b.update(cx, |s, cx| s.open_palette(window, cx))
                    }),
            );
    }
    if l.panel_icons {
        for item in panel_items {
            let RailItem {
                id,
                icon: ic,
                title,
                active,
                on_click,
            } = item;
            let mut b = IconButton::new(SharedString::from(format!("sbc-{id}")), t, ic)
                .glass(p.glass)
                .active(active)
                .tooltip(title);
            if let Some(f) = on_click {
                b = b.on_click(move |_, window, cx| f(window, cx));
            }
            top = top.child(b);
        }
    }
    top = top.child(div().h(px(6.0)));
    for info in p.tabs {
        let id = info.id;
        let s = shell.clone();
        let mut b = IconButton::new(
            SharedString::from(format!("sb-tab-{}", id.as_u64())),
            t,
            info.icon.icon(),
        )
        .glass(p.glass)
        .active(Some(id) == p.active)
        .tooltip(info.title.clone());
        b = b.on_click(move |_, window, cx| s.update(cx, |s, cx| s.select_tab(id, window, cx)));
        top = top.child(b);
    }
    col = col.child(top);
    if l.footer_usage {
        col = col.child(
            IconButton::new("sb-c-settings", t, Icon::Settings)
                .glass(p.glass)
                .tooltip("Ajustes (Ctrl+,)")
                .on_click(move |_, window, cx| {
                    shell.update(cx, |s, cx| s.open_settings(window, cx))
                }),
        );
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> TabId {
        gpui::EntityId::from(n)
    }

    #[test]
    fn close_shows_on_selected_and_hovered_rows_only() {
        assert!(
            !close_visible(true, None, id(1)),
            "selected row keeps its status at rest"
        );
        assert!(
            !close_visible(true, Some(id(2)), id(1)),
            "another hovered row doesn't affect it"
        );
        assert!(
            close_visible(true, Some(id(1)), id(1)),
            "hovered selected row shows the x"
        );
        assert!(
            close_visible(false, Some(id(1)), id(1)),
            "hovered row shows the x"
        );
        assert!(!close_visible(false, None, id(1)));
        assert!(!close_visible(false, Some(id(2)), id(1)));
    }

    #[test]
    fn hover_slot_follows_the_pointer_without_flicker() {
        assert_eq!(hover_transition(None, 1, true), Some(1));
        assert_eq!(hover_transition(Some(1), 1, false), None);
        // The next row's "entered" arrives before the previous row's "left": the late one is ignored.
        let s = hover_transition(Some(1), 2, true);
        assert_eq!(s, Some(2));
        assert_eq!(hover_transition(s, 1, false), Some(2));
    }
}
