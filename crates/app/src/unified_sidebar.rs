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
use theme::{Radius, Theme};
use ui_kit::{
    DitherMeter, Icon, IconButton, SectionLabel, StatusGlyph, glass_hover, glass_selected, icon,
};

use crate::rail::RailItem;
use crate::sidebar_view::{
    ChildRow, CloseFocusedRow, Lineage, RowFocus, SidebarEvent, SidebarView, focus_bg, send,
};
use crate::spaces::{Space, row_subtitle, row_time};

/// What the unified sidebar needs to paint, prepared by the shell.
pub struct UnifiedProps<'a> {
    pub theme: &'a Theme,
    pub layout: &'a ResolvedLayout,
    pub tabs: &'a [TabInfo],
    pub active: Option<TabId>,
    pub view: &'a Entity<SidebarView>,
    /// The column is glass: pills are translucent.
    pub glass: bool,
    pub spaces: Vec<Space>,
    /// Keys of the spaces that are folded.
    pub folded: &'a HashSet<PathBuf>,
    /// Delegations nested under their parent tab.
    pub lineage: &'a Lineage,
    /// The tab row under the pointer (state kept by the shell).
    pub hovered_row: Option<TabId>,
    /// The space header under the pointer (by its key, not its position).
    pub hovered_space: Option<PathBuf>,
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
        .rounded(px(Radius::PILL))
        .cursor_pointer()
        .text_size(px(ty.ui_size_md))
        .text_color(c.text)
        .hover(move |s| s.bg(hover))
        .on_click(move |e, w, cx| on_click(e, w, cx))
        .tab_stop_row(focus_bg(t, p.glass))
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
            action,
        } = item;
        let mut b = IconButton::new(SharedString::from(format!("sb-{id}")), p.theme, ic)
            .size(28.0)
            .glass(p.glass)
            .active(active)
            .tooltip(title);
        if let Some(a) = action {
            let v = p.view.clone();
            b = b.on_click(move |_, _, cx| send(&v, cx, SidebarEvent::Rail(a.clone())));
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
    let (view_sel, view_close, view_hover, view_key, view_map) = (
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
    );
    let has_children = p.lineage.children.contains_key(&id);
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
            .rounded(px(Radius::PILL))
            .cursor_pointer()
            .when(selected, move |d| d.bg(bg))
            .when(!selected, move |d| d.hover(move |s| s.bg(bg)))
            .on_hover(move |hovered, _, cx| {
                view_hover.update(cx, |v, cx| v.set_row_hover(id, *hovered, cx))
            })
            .on_click(move |_, _, cx| send(&view_sel, cx, SidebarEvent::Select(id)))
            // Keyboard: a tab stop; Delete closes it (the shell asks first when it is running).
            .key_context("SidebarRow")
            .on_action(move |_: &CloseFocusedRow, _, cx| {
                send(&view_key, cx, SidebarEvent::Close(id));
            })
            .tab_stop_row(focus_bg(t, p.glass))
            .debug_selector(move || format!("urow-{}", id.as_u64()))
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
            // A parent that delegated: the subagent map of its session (next to the close x).
            .children((show_close && has_children).then(|| {
                div()
                    .absolute()
                    .right(px(32.0))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        div().rounded(px(Radius::CHIP)).bg(close_bg).child(
                            IconButton::new(
                                SharedString::from(format!("urow-map-{}", id.as_u64())),
                                t,
                                ui_agentmap::MAP_ICON,
                            )
                            .glass(p.glass)
                            .passthrough(true)
                            .size(22.0)
                            .icon_size(12.0)
                            .tooltip("Mapa de subagentes")
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                send(&view_map, cx, SidebarEvent::OpenAgentMap(id));
                            }),
                        ),
                    )
            }))
            .children(show_close.then(|| {
                div()
                    .absolute()
                    .right(px(6.0))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        div().rounded(px(Radius::CHIP)).bg(close_bg).child(
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
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                send(&view_close, cx, SidebarEvent::Close(id));
                            }),
                        ),
                    )
            })),
    )
}

/// A delegated thread nested under its parent tab: agent mark, title, state line and status glyph.
/// It is a tab stop with the standard 2 px keyboard ring (`ui_kit::focus`).
#[derive(IntoElement)]
struct ChildRowEl {
    theme: Theme,
    glass: bool,
    view: Entity<SidebarView>,
    row: ChildRow,
    selected: bool,
}

impl ChildRowEl {
    fn new(p: &UnifiedProps, row: &ChildRow) -> Self {
        Self {
            theme: *p.theme,
            glass: p.glass,
            view: p.view.clone(),
            selected: row.tab.is_some() && row.tab == p.active,
            row: row.clone(),
        }
    }
}

impl RenderOnce for ChildRowEl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (thread, tab) = (self.row.thread, self.row.tab);
        let id = gpui::ElementId::from(SharedString::from(format!("uchild-{thread}")));
        let handle = ui_kit::focus::handle(window, cx, &id);
        let bg = pill_bg(&t, self.glass, self.selected);
        let view = self.view.clone();
        let mark = ui_kit::AgentMark::from_name(&self.row.agent);
        let el = div()
            .id(id.clone())
            .occlude()
            .w_full()
            .h(px(34.0))
            .my(px(1.0))
            .pl(px(38.0 + 14.0 * self.row.depth as f32))
            .pr(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::PILL))
            .cursor_pointer()
            .when(self.selected, move |d| d.bg(bg))
            .when(!self.selected, move |d| d.hover(move |s| s.bg(bg)))
            .on_click(move |_, _, cx| match tab {
                Some(tab) => send(&view, cx, SidebarEvent::Select(tab)),
                None => send(&view, cx, SidebarEvent::OpenThread(thread)),
            })
            .child(ui_kit::agent_icon(
                mark,
                14.0,
                if self.selected { c.text } else { c.muted },
            ))
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
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.text)
                            .child(self.row.title.clone()),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_xs))
                            .text_color(c.muted)
                            .child(self.row.subtitle.clone()),
                    ),
            )
            .children(self.row.status.map(|s| StatusGlyph::new(&t, s)));
        ui_kit::focus::wire(el, Some(handle), &id, Radius::PILL, c.accent)
    }
}

/// A space header: fold chevron, name, branch and dirty dot, and "+" (a new tab there).
fn space_header(p: &UnifiedProps, s: &Space, folded: bool) -> impl IntoElement + use<> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let key = s.key();
    let root = s.root.clone();
    let open = s.is_open();
    let hover = pill_bg(t, p.glass, false);
    let show_plus = p.hovered_space.as_ref() == Some(&key);
    let (view, view_plus, view_hover, view_drop) = (
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
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
    // Ids and hover are keyed by the space (its project path), never by its position: spaces
    // fold, reorder and appear while the pointer is over the list.
    let key_str = key.to_string_lossy().into_owned();
    let hover_key = key.clone();
    let focus = focus_bg(t, p.glass);
    div()
        .id(SharedString::from(format!("space:{key_str}")))
        .relative()
        .occlude()
        .w_full()
        .h(px(30.0))
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(Radius::CONTROL))
        .cursor_pointer()
        .hover(move |st| st.bg(hover))
        .on_hover(move |hovered, _, cx| {
            view_hover.update(cx, |v, cx| v.set_space_hover(&hover_key, *hovered, cx))
        })
        .on_click(move |_, _, cx| {
            let (key, root) = (key.clone(), root.clone());
            send(
                &view,
                cx,
                if open {
                    SidebarEvent::ToggleSpace(key)
                } else {
                    SidebarEvent::NewIn(root)
                },
            );
        })
        .tab_stop_row(focus)
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
                IconButton::new(
                    SharedString::from(format!("space-add:{key_str}")),
                    t,
                    Icon::Plus,
                )
                .glass(p.glass)
                .passthrough(true)
                .size(22.0)
                .icon_size(12.0)
                .tooltip("Nueva pestaña aquí")
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let root = root.clone();
                    send(&view_plus, cx, SidebarEvent::NewIn(root));
                }),
            )
        }))
        .child(
            ui_kit::DropZone::new(
                SharedString::from(format!("space-drop:{key_str}")),
                &theme,
                "Soltar para abrir una terminal aquí",
            )
            .radius(8.0)
            .compact(true)
            .on_drop(move |paths, _, cx| {
                let paths = paths.to_vec();
                send(&view_drop, cx, SidebarEvent::DropOnSpace(paths));
            }),
        )
}

fn footer(p: &UnifiedProps) -> impl IntoElement + use<> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let view = p.view.clone();
    let usage_view = p.view.clone();
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
                .tab_stop_row(focus_bg(t, p.glass))
                .on_click(move |_, _, cx| {
                    send(&usage_view, cx, SidebarEvent::ToggleUsage);
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
                .on_click(move |_, _, cx| send(&view, cx, SidebarEvent::Settings)),
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
        .rounded(px(Radius::PILL))
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
        let (a, b) = (p.view.clone(), p.view.clone());
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
                    Box::new(move |_, _, cx| send(&a, cx, SidebarEvent::NewMenu)),
                ))
                .child(action_row(
                    p,
                    "sb-act-search",
                    Icon::Search,
                    "Buscar",
                    "Ctrl+K",
                    Box::new(move |_, _, cx| send(&b, cx, SidebarEvent::Palette)),
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
    for s in &p.spaces {
        let folded = p.folded.contains(&s.key());
        let mut block = div().flex().flex_col();
        if s.headless {
            for &ti in &s.tabs {
                if p.lineage.nested.contains(&p.tabs[ti].id) {
                    continue;
                }
                block = block.child(tab_row(p, &p.tabs[ti], false, now));
            }
            spaces = spaces.child(block);
            continue;
        }
        block = block.child(space_header(p, s, folded));
        if s.is_open() && !folded {
            for &ti in &s.tabs {
                let id = p.tabs[ti].id;
                if p.lineage.nested.contains(&id) {
                    continue;
                }
                block = block.child(tab_row(p, &p.tabs[ti], true, now));
                for child in p.lineage.children.get(&id).into_iter().flatten() {
                    block = block.child(ChildRowEl::new(p, child));
                }
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
    let view = p.view.clone();
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
        let s = view.clone();
        top = top.child(
            IconButton::new("sb-expand", t, Icon::ChevronRight)
                .glass(p.glass)
                .tooltip("Expandir la barra lateral (Ctrl+B)")
                .on_click(move |_, _, cx| send(&s, cx, SidebarEvent::ToggleCollapsed)),
        );
    }
    if l.show_actions {
        let (a, b) = (view.clone(), view.clone());
        top = top
            .child(
                IconButton::new("sb-c-new", t, Icon::Plus)
                    .glass(p.glass)
                    .tooltip("Nueva pestaña (Ctrl+N)")
                    .on_click(move |_, _, cx| send(&a, cx, SidebarEvent::NewMenu)),
            )
            .child(
                IconButton::new("sb-c-search", t, Icon::Search)
                    .glass(p.glass)
                    .tooltip("Buscar (Ctrl+K)")
                    .on_click(move |_, _, cx| send(&b, cx, SidebarEvent::Palette)),
            );
    }
    if l.panel_icons {
        for item in panel_items {
            let RailItem {
                id,
                icon: ic,
                title,
                active,
                action,
            } = item;
            let mut b = IconButton::new(SharedString::from(format!("sbc-{id}")), t, ic)
                .glass(p.glass)
                .active(active)
                .tooltip(title);
            if let Some(a) = action {
                let v = view.clone();
                b = b.on_click(move |_, _, cx| send(&v, cx, SidebarEvent::Rail(a.clone())));
            }
            top = top.child(b);
        }
    }
    top = top.child(div().h(px(6.0)));
    for info in p.tabs {
        let id = info.id;
        let s = view.clone();
        let mut b = IconButton::new(
            SharedString::from(format!("sb-tab-{}", id.as_u64())),
            t,
            info.icon.icon(),
        )
        .glass(p.glass)
        .active(Some(id) == p.active)
        .tooltip(info.title.clone());
        b = b.on_click(move |_, _, cx| send(&s, cx, SidebarEvent::Select(id)));
        top = top.child(b);
    }
    col = col.child(top);
    if l.footer_usage {
        col = col.child(
            IconButton::new("sb-c-settings", t, Icon::Settings)
                .glass(p.glass)
                .tooltip("Ajustes (Ctrl+,)")
                .on_click(move |_, _, cx| send(&view, cx, SidebarEvent::Settings)),
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
