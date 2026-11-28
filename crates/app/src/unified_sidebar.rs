//! Body of the unified sidebar (preset "Espacios unificados", docs/olas/ola-5.md 5.10): quick
//! actions, a row of panel buttons, the spaces with their tabs nested as rows, and a footer with
//! the usage meter and the settings button. `collapsed` is the icon-only column (Ctrl+B, S class).
//!
//! Only real data is painted: spaces come from the open tabs and the store's favorites, rows from
//! the tabs' own `TabInfo`. Nothing here ticks: the elapsed time is recomputed on render and the
//! only animation is the shark-bars spinner of a tab that is running a command.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, Bounds, ClickEvent, Div, Entity, Hsla, IntoElement, Pixels, Point, SharedString, Stateful,
    Transformation, Window, canvas, div, point, prelude::*, px, radians,
};
use panel_api::{TabId, TabInfo};
use store::ResolvedLayout;
use theme::{Radius, Theme};
use ui_kit::{
    Button, DitherMeter, EmptyState, Icon, IconButton, SectionLabel, StatusGlyph, glass_hover,
    glass_selected, icon,
};

use crate::paths::same_path;
use crate::projects::Dest;
use crate::rail::RailItem;
use crate::sidebar_view::{
    ChildRow, CloseFocusedRow, Lineage, OpenSpaceMenu, RowFocus, SidebarEvent, SidebarView,
    focus_bg, send,
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
    /// Parent rows whose subagents are folded behind the chip / that list every one of them.
    pub kids_folded: &'a HashSet<TabId>,
    pub kids_all: &'a HashSet<TabId>,
    /// The tab row under the pointer (state kept by the shell).
    pub hovered_row: Option<TabId>,
    /// The space header under the pointer (by its key, not its position).
    pub hovered_space: Option<PathBuf>,
    /// The project new terminals and chats go to (its header is tinted).
    pub selected: Option<&'a PathBuf>,
    /// What "Nueva pestaña" says about where a new tab goes.
    pub dest: &'a Dest,
    /// No project was added by hand: the list shows its empty state.
    pub known_empty: bool,
    /// Where each project header was painted (for the keyboard menu).
    pub header_bounds: &'a Rc<RefCell<HashMap<PathBuf, Bounds<Pixels>>>>,
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

/// "Nueva pestaña  Ctrl+N" / "Buscar  Ctrl+K". `detail` is a muted second line ("en TRON Project").
fn action_row(
    p: &UnifiedProps,
    id: &'static str,
    ic: Icon,
    label: &'static str,
    detail: Option<String>,
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
        .h(px(if detail.is_some() { 44.0 } else { 32.0 }))
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
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(label)
                .children(detail.map(|d| {
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .child(SharedString::from(d))
                })),
        )
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
    let has_children = p.lineage.children.contains_key(&id) || p.lineage.mapped.contains(&id);
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
    /// The tab it is listed under.
    parent: TabId,
}

impl ChildRowEl {
    fn new(p: &UnifiedProps, row: &ChildRow, parent: TabId) -> Self {
        Self {
            theme: *p.theme,
            glass: p.glass,
            view: p.view.clone(),
            selected: row.tab.is_some() && row.tab == p.active,
            row: row.clone(),
            parent,
        }
    }
}

/// What a line of the subagents list does when clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KidsAction {
    /// The chip: fold / unfold the list.
    Fold,
    /// «Ver todos» / «Ver menos».
    More,
}

/// The chip «3 subagentes · 1 en marcha» under a parent row, and the «Ver todos» line at the end of a
/// long list. A tab stop with the standard keyboard ring.
#[derive(IntoElement)]
struct KidsLine {
    theme: Theme,
    glass: bool,
    view: Entity<SidebarView>,
    parent: TabId,
    label: SharedString,
    /// The chevron of the chip (`None`: a plain line).
    open: Option<bool>,
    running: bool,
    action: KidsAction,
}

impl RenderOnce for KidsLine {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let tag = match self.action {
            KidsAction::Fold => "fold",
            KidsAction::More => "more",
        };
        let id = gpui::ElementId::from(SharedString::from(format!(
            "ukids-{tag}-{}",
            self.parent.as_u64()
        )));
        let handle = ui_kit::focus::handle(window, cx, &id);
        let bg = pill_bg(&t, self.glass, false);
        let (view, parent, action) = (self.view.clone(), self.parent, self.action);
        let el = div()
            .id(id.clone())
            .occlude()
            .w_full()
            .h(px(24.0))
            .my(px(1.0))
            .pl(px(36.0))
            .pr(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(Radius::PILL))
            .cursor_pointer()
            .hover(move |s| s.bg(bg))
            .on_click(move |_, _, cx| {
                view.update(cx, |v, cx| match action {
                    KidsAction::Fold => v.toggle_kids(parent, cx),
                    KidsAction::More => v.toggle_kids_all(parent, cx),
                })
            })
            .children(self.open.map(|open| {
                icon(
                    if open { Icon::ChevronDown } else { Icon::ChevronRight },
                    12.0,
                    c.muted,
                )
            }))
            .child(
                div()
                    .text_size(px(ty.ui_size_xs))
                    .text_color(if self.running { c.accent } else { c.muted })
                    .child(self.label.clone()),
            );
        ui_kit::focus::wire(el, Some(handle), &id, Radius::PILL, c.accent)
    }
}

/// What is listed under a parent row that delegated: the chip, which subagents, and the extra line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KidsPlan {
    /// `3 subagentes · 1 en marcha`.
    pub summary: String,
    pub running: usize,
    /// Indices into the parent's rows; empty while the chip is folded.
    pub rows: Vec<usize>,
    /// `Ver todos (8)` / `Ver menos`.
    pub more: Option<String>,
}

/// Pure: the chip, the first five subagents (the running ones first) and «Ver todos». `None` when
/// the row delegated nothing.
pub(crate) fn kids_plan(rows: &[ChildRow], folded: bool, all: bool) -> Option<KidsPlan> {
    if rows.is_empty() {
        return None;
    }
    let running = rows.iter().filter(|r| r.running).count();
    let summary = ui_chat::subagents_summary(rows.len(), running);
    if folded {
        return Some(KidsPlan {
            summary,
            running,
            rows: Vec::new(),
            more: None,
        });
    }
    let depths: Vec<usize> = rows.iter().map(|r| r.depth).collect();
    let flags: Vec<bool> = rows.iter().map(|r| r.running).collect();
    let keep = ui_chat::visible_rows(&depths, &flags, all);
    let more = if keep.len() < rows.len() {
        Some(format!("Ver todos ({})", rows.len()))
    } else if all && depths.iter().filter(|d| **d == 0).count() > ui_chat::COLLAPSE_AFTER {
        Some("Ver menos".to_string())
    } else {
        None
    };
    Some(KidsPlan {
        summary,
        running,
        rows: keep,
        more,
    })
}

/// The lines under a parent row: the chip, up to five subagents (the running ones first) and
/// «Ver todos». Nothing when the row delegated nothing.
fn kids_block(p: &UnifiedProps, parent: TabId) -> Vec<gpui::AnyElement> {
    let Some(rows) = p.lineage.children.get(&parent) else {
        return Vec::new();
    };
    let folded = p.kids_folded.contains(&parent);
    let all = p.kids_all.contains(&parent);
    let Some(plan) = kids_plan(rows, folded, all) else {
        return Vec::new();
    };
    let running = plan.running;
    let line = |label: String, open: Option<bool>, running: bool, action: KidsAction| {
        KidsLine {
            theme: *p.theme,
            glass: p.glass,
            view: p.view.clone(),
            parent,
            label: label.into(),
            open,
            running,
            action,
        }
        .into_any_element()
    };
    let mut out = vec![line(
        plan.summary.clone(),
        Some(!folded),
        running > 0,
        KidsAction::Fold,
    )];
    out.extend(
        plan.rows
            .iter()
            .map(|i| ChildRowEl::new(p, &rows[*i], parent).into_any_element()),
    );
    if let Some(more) = plan.more {
        out.push(line(more, None, false, KidsAction::More));
    }
    out
}

impl RenderOnce for ChildRowEl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (thread, tab, parent) = (self.row.thread, self.row.tab, self.parent);
        let id = gpui::ElementId::from(SharedString::from(format!("uchild-{thread}")));
        let view_menu = self.view.clone();
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
            .on_mouse_down(gpui::MouseButton::Right, move |e, _, cx| {
                send(
                    &view_menu,
                    cx,
                    SidebarEvent::ChildMenu {
                        thread,
                        parent,
                        at: e.position,
                    },
                );
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

/// A space header. A project: its folder name, branch and dirty dot; a click selects it (it is
/// where new terminals and chats go: tinted with the accent, which the active tab's grey pill
/// never is), the chevron folds it, and on hover a "+" (new terminal / chat here) and a "..." (the
/// menu, also on right click and Shift+F10) appear over its right edge. A status / agent group
/// (no folder) just folds.
fn space_header(p: &UnifiedProps, s: &Space, folded: bool) -> impl IntoElement + use<> {
    let t = p.theme;
    let c = t.colors;
    let ty = t.typography;
    let key = s.key();
    let root = s.root.clone();
    let open = s.is_open();
    let project = !root.as_os_str().is_empty();
    let selected = project && p.selected.is_some_and(|sel| same_path(sel, &root));
    let missing = s.missing;
    let hover = pill_bg(t, p.glass, false);
    let tint: Hsla = Hsla {
        a: 0.17,
        ..Hsla::from(c.accent)
    };
    let show_buttons = project && p.hovered_space.as_ref() == Some(&key);
    let (view, view_chev, view_plus, view_more, view_hover, view_right, view_menu) = (
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
        p.view.clone(),
    );
    let chevron = {
        let tone = if selected { c.accent } else { c.muted };
        let g = icon(Icon::ChevronRight, 12.0, tone);
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
    // Ids and hover are keyed by the space (its project path), never by its position: spaces
    // fold, reorder and appear while the pointer is over the list.
    let key_str = key.to_string_lossy().into_owned();
    let hover_key = key.clone();
    let focus = focus_bg(t, p.glass);
    let name_color = if missing {
        c.outline
    } else if selected {
        c.accent
    } else if open || project {
        c.text
    } else {
        c.muted
    };
    let bounds = p.header_bounds.clone();
    let bounds_key = root.clone();
    let (root_click, root_key, root_menu) = (root.clone(), root.clone(), root.clone());
    let (root_plus, root_more) = (root.clone(), root.clone());
    let click_key = key.clone();
    let chev_key = key;
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
        .gap(px(4.0))
        .rounded(px(Radius::CONTROL))
        .cursor_pointer()
        .when(selected, move |d| d.bg(tint))
        .when(!selected, move |d| d.hover(move |st| st.bg(hover)))
        .on_hover(move |hovered, _, cx| {
            view_hover.update(cx, |v, cx| v.set_space_hover(&hover_key, *hovered, cx))
        })
        .on_click(move |_, _, cx| {
            send(
                &view,
                cx,
                if project {
                    SidebarEvent::SelectSpace(root_click.clone())
                } else {
                    SidebarEvent::ToggleSpace(click_key.clone())
                },
            );
        })
        .on_mouse_down(gpui::MouseButton::Right, move |e, _, cx| {
            if project {
                send(
                    &view_right,
                    cx,
                    SidebarEvent::SpaceMenu {
                        root: root_menu.clone(),
                        at: e.position,
                        full: true,
                    },
                );
            }
        })
        .tab_stop_row(focus)
        .key_context("SidebarSpace")
        .on_action(move |_: &OpenSpaceMenu, _, cx| {
            if !project {
                return;
            }
            let at = bounds
                .borrow()
                .get(&bounds_key)
                .map_or_else(Point::default, |b| {
                    b.bottom_left() + point(px(16.0), px(2.0))
                });
            send(
                &view_menu,
                cx,
                SidebarEvent::SpaceMenu {
                    root: root_key.clone(),
                    at,
                    full: true,
                },
            );
        })
        .debug_selector({
            let k = key_str.clone();
            move || format!("space-{k}")
        })
        // Where the header was painted: the keyboard menu opens right under it.
        .child({
            let bounds = p.header_bounds.clone();
            let k = root;
            canvas(
                move |b, _, _| {
                    if project {
                        bounds.borrow_mut().insert(k.clone(), b);
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        })
        .child(if open {
            // The chevron folds; the rest of the header selects.
            div()
                .id(SharedString::from(format!("space-fold:{key_str}")))
                .debug_selector({
                    let k = key_str.clone();
                    move || format!("space-fold-{k}")
                })
                .occlude()
                .size(px(18.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::CHIP))
                .when(project, |d| d.hover(move |st| st.bg(hover)))
                .tooltip(ui_kit::Tooltip::text(
                    t,
                    if folded { "Desplegar" } else { "Plegar" },
                ))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    send(&view_chev, cx, SidebarEvent::ToggleSpace(chev_key.clone()));
                })
                .child(chevron)
                .into_any_element()
        } else {
            div().size(px(18.0)).flex_none().into_any_element()
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(ty.ui_size_md))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(name_color)
                .child(SharedString::from(s.name.clone())),
        )
        .children(missing.then(|| {
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.outline)
                .whitespace_nowrap()
                .child("no encontrada")
        }))
        .children(s.branch.clone().filter(|_| !missing).map(|b| {
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
        .children(show_buttons.then(|| {
            let bg: Hsla = if p.glass {
                glass_selected(t)
            } else {
                c.surface_3.into()
            };
            div()
                .absolute()
                .right(px(4.0))
                .top_0()
                .h_full()
                .flex()
                .items_center()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(1.0))
                        .rounded(px(Radius::CHIP))
                        .bg(bg)
                        .when(!missing, |d| {
                            d.child(
                                IconButton::new(
                                    SharedString::from(format!("space-add:{key_str}")),
                                    t,
                                    Icon::Plus,
                                )
                                .glass(p.glass)
                                .passthrough(true)
                                .size(22.0)
                                .icon_size(12.0)
                                .tooltip("Nueva terminal o chat aquí")
                                .on_click(move |e, _, cx| {
                                    cx.stop_propagation();
                                    send(
                                        &view_plus,
                                        cx,
                                        SidebarEvent::SpaceMenu {
                                            root: root_plus.clone(),
                                            at: e.position(),
                                            full: false,
                                        },
                                    );
                                }),
                            )
                        })
                        .child(
                            IconButton::new(
                                SharedString::from(format!("space-more:{key_str}")),
                                t,
                                Icon::Dots,
                            )
                            .glass(p.glass)
                            .passthrough(true)
                            .size(22.0)
                            .icon_size(12.0)
                            .tooltip("Más opciones del proyecto")
                            .on_click(move |e, _, cx| {
                                cx.stop_propagation();
                                send(
                                    &view_more,
                                    cx,
                                    SidebarEvent::SpaceMenu {
                                        root: root_more.clone(),
                                        at: e.position(),
                                        full: true,
                                    },
                                );
                            }),
                        ),
                )
        }))
}

/// The "+" of the "Espacios" header: add a project (the menu opens under it).
fn add_project_button(p: &UnifiedProps) -> impl IntoElement + use<> {
    let view = p.view.clone();
    IconButton::new("sb-add-project", p.theme, Icon::FolderPlus)
        .glass(p.glass)
        .size(22.0)
        .icon_size(12.0)
        .tooltip("Agregar proyecto")
        .on_click(move |e, _, cx| send(&view, cx, SidebarEvent::AddProjectMenu(e.position())))
}

/// No project was ever added or opened: a short explanation and the one thing to do about it.
fn empty_projects(p: &UnifiedProps) -> impl IntoElement + use<> {
    let view = p.view.clone();
    div().id("sb-empty-projects").px(px(4.0)).child(
        EmptyState::new(p.theme, "Sin proyectos agregados")
            .icon(Icon::Folder)
            .icon_size(24.0)
            .body("Agrega una carpeta: las nuevas terminales y chats se abrirán dentro de ella.")
            .padding_y(12.0)
            .action(
                Button::tonal("sb-empty-add", p.theme, "Agregar proyecto")
                    .icon(Icon::Plus)
                    .on_click(move |e, _, cx| {
                        send(&view, cx, SidebarEvent::AddProjectMenu(e.position()))
                    }),
            ),
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
                    Some(p.dest.line()),
                    "Ctrl+N",
                    Box::new(move |_, _, cx| send(&a, cx, SidebarEvent::NewMenu)),
                ))
                .child(action_row(
                    p,
                    "sb-act-search",
                    Icon::Search,
                    "Buscar",
                    None,
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
            .pl(px(10.0))
            .pr(px(2.0))
            .pt(px(2.0))
            .pb(px(2.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(SectionLabel::new(t, "Espacios").uppercase(true))
            .child(add_project_button(p)),
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
                for el in kids_block(p, id) {
                    block = block.child(el);
                }
            }
        }
        spaces = spaces.child(block);
    }
    if p.known_empty {
        spaces = spaces.child(empty_projects(p));
    }
    // A folder dropped anywhere on the list is added as a project.
    let view_drop = p.view.clone();
    spaces = spaces.relative().child(
        ui_kit::DropZone::new("spaces-drop", t, "Soltar para agregar como proyecto")
            .radius(10.0)
            .on_drop(move |paths, _, cx| {
                let paths = paths.to_vec();
                send(&view_drop, cx, SidebarEvent::DropOnSpace(paths));
            }),
    );
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
        let c_view = view.clone();
        top = top.child(
            IconButton::new("sb-c-add-project", t, Icon::FolderPlus)
                .glass(p.glass)
                .tooltip("Agregar proyecto")
                .on_click(move |e, _, cx| {
                    send(&c_view, cx, SidebarEvent::AddProjectMenu(e.position()))
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

    fn kid(thread: i64, depth: usize, running: bool, finished: bool) -> ChildRow {
        ChildRow {
            thread,
            tab: None,
            title: format!("hijo {thread}").into(),
            agent: "codex".into(),
            status: None,
            subtitle: "".into(),
            depth,
            running,
            finished,
        }
    }

    #[test]
    fn subagents_nest_under_their_parent_behind_a_counting_chip() {
        assert_eq!(kids_plan(&[], false, false), None);
        let rows = vec![kid(1, 0, false, true), kid(2, 0, true, false), kid(3, 1, false, true)];
        let plan = kids_plan(&rows, false, false).unwrap();
        assert_eq!(plan.summary, "3 subagentes · 1 en marcha");
        assert_eq!(plan.rows, vec![0, 1, 2]);
        assert_eq!(plan.more, None);
        // the chip folds the list but keeps counting
        let folded = kids_plan(&rows, true, false).unwrap();
        assert!(folded.rows.is_empty() && folded.running == 1);
    }

    #[test]
    fn long_lists_fold_after_five_and_ver_todos_shows_the_rest() {
        let rows: Vec<ChildRow> = (1..=8).map(|i| kid(i, 0, i == 2, i != 2)).collect();
        let plan = kids_plan(&rows, false, false).unwrap();
        assert_eq!(plan.rows.len(), 5);
        assert!(plan.rows.contains(&1), "the running one is never hidden");
        assert_eq!(plan.more.as_deref(), Some("Ver todos (8)"));
        let all = kids_plan(&rows, false, true).unwrap();
        assert_eq!(all.rows.len(), 8);
        assert_eq!(all.more.as_deref(), Some("Ver menos"));
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
