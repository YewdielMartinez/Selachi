//! `TitleBarView`: the 40 px row at the top of the center column (or the zen top bar) as an entity
//! of its own (docs/auditoria-gpui.md H4).
//!
//! It owns what only the bar cares about: the `...` and "all tabs" menus (open / closed) and the
//! tab strip's scroll. Everything it paints comes from a [`TitleBarModel`] the shell pushes when
//! something it shows changed; what the user does leaves as a [`TitleBarEvent`].

use std::path::PathBuf;

use gpui::{
    AnyView, Context, EventEmitter, IntoElement, Render, ScrollHandle, SharedString, Window, div,
    prelude::*, px,
};
use panel_api::{TabId, TabInfo};
use theme::{Radius, Theme};
use ui_kit::{Icon, IconButton, ListRow, Placement, Popover, icon};

use crate::paths;
use crate::rail::{RailAction, RailItem};
use crate::titlebar::{breadcrumb, new_tab_button, tab_pill, title_row, window_controls};

/// Narrowest a tab pill gets in the strip.
const TAB_MIN_W: f32 = 96.0;

/// A button of the right side of the bar.
#[derive(Clone, Debug, PartialEq)]
pub enum TitleButton {
    /// The layout popover's trigger.
    Layout,
    /// A right panel or the review tab.
    Panel {
        id: SharedString,
        icon: Icon,
        title: SharedString,
        active: bool,
        action: RailAction,
    },
}

/// Everything the bar paints that comes from the shell.
#[derive(Clone, PartialEq)]
pub struct TitleBarModel {
    pub theme: Theme,
    pub infos: Vec<TabInfo>,
    pub active: Option<TabId>,
    pub active_cwd: Option<PathBuf>,
    /// Shell v2: the active tab's title replaces the breadcrumb.
    pub unified_heading: bool,
    pub breadcrumb: bool,
    pub breadcrumb_last_only: bool,
    /// The tab strip is on the bar (tabs on top, or always in XS).
    pub tab_strip: bool,
    pub xs: bool,
    /// The `...` menu with the rail's buttons (XS).
    pub rail_in_menu: bool,
    /// The sidebar is a drawer: the menu lists it.
    pub drawer_sidebar: bool,
    pub drawer_open: bool,
    pub buttons: Vec<TitleButton>,
    pub rail_items: Vec<RailItem>,
    /// The layout popover's content (open exactly when present).
    pub layout_panel: Option<AnyView>,
    /// The window controls close the bar.
    pub with_controls: bool,
    pub pad_left: f32,
    /// Opaque background (the zen bar floats over the terminal).
    pub opaque: bool,
}

/// What the bar asks of the shell.
#[derive(Clone, Debug, PartialEq)]
pub enum TitleBarEvent {
    Select(TabId),
    Close(TabId),
    NewTab,
    ToggleLayout,
    DismissLayout,
    Rail(RailAction),
    ToggleDrawer,
    /// A menu opened or closed (the zen bar stays while one is open).
    MenusChanged,
}

pub struct TitleBarView {
    model: TitleBarModel,
    menu_open: bool,
    tabs_menu_open: bool,
    tab_scroll: ScrollHandle,
    /// Scroll the strip to the active tab on the next paint (set when a tab opens or is selected).
    scroll_active: bool,
}

impl EventEmitter<TitleBarEvent> for TitleBarView {}

impl TitleBarView {
    pub fn new(model: TitleBarModel) -> Self {
        Self {
            model,
            menu_open: false,
            tabs_menu_open: false,
            tab_scroll: ScrollHandle::new(),
            scroll_active: false,
        }
    }

    pub fn set_model(&mut self, model: TitleBarModel, cx: &mut Context<Self>) {
        if self.model != model {
            self.model = model;
            cx.notify();
        }
    }

    pub fn menus_open(&self) -> bool {
        self.menu_open || self.tabs_menu_open
    }

    pub fn menu_open(&self) -> bool {
        self.menu_open
    }

    pub fn close_menus(&mut self, cx: &mut Context<Self>) {
        if self.menus_open() {
            self.menu_open = false;
            self.tabs_menu_open = false;
            cx.notify();
        }
    }

    pub fn close_main_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_open {
            self.menu_open = false;
            cx.notify();
        }
    }

    pub fn close_tabs_menu(&mut self, cx: &mut Context<Self>) {
        if self.tabs_menu_open {
            self.tabs_menu_open = false;
            cx.notify();
        }
    }

    /// The strip scrolls to the active tab the next time it paints.
    pub fn reveal_active(&mut self, cx: &mut Context<Self>) {
        self.scroll_active = true;
        cx.notify();
    }

    fn set_menus(&mut self, menu: bool, tabs: bool, cx: &mut Context<Self>) {
        self.menu_open = menu;
        self.tabs_menu_open = tabs;
        cx.emit(TitleBarEvent::MenusChanged);
        cx.notify();
    }

    /// The layout popover's trigger.
    fn layout_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let m = &self.model;
        let me = cx.entity();
        let dismiss = me.clone();
        let open = m.layout_panel.is_some();
        // BottomEnd: the button sits at the right of the titlebar, so the panel grows leftwards
        // from the button's right edge (clamped to the window); in XS it is a bottom sheet.
        let popover = Popover::new("layout-pop", &m.theme)
            .open(open)
            .placement(Placement::BottomEnd)
            .padding(0.0)
            .width(ui_layout::PANEL_WIDTH)
            .sheet_when_narrow(true)
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, |_, cx| cx.emit(TitleBarEvent::DismissLayout))
            })
            .trigger(
                IconButton::new("layout-btn", &m.theme, Icon::Layout)
                    .size(28.0)
                    .active(open)
                    .emphasize_on_hover(true)
                    .tooltip("Layout (Ctrl+Shift+L)")
                    .on_click(move |_, _, cx| {
                        me.update(cx, |_, cx| cx.emit(TitleBarEvent::ToggleLayout))
                    }),
            )
            .children(m.layout_panel.clone());
        div()
            .h_full()
            .flex()
            .flex_col()
            .justify_center()
            .child(popover)
    }

    /// The `...` menu of XS: the rail's buttons and the sidebar drawer.
    fn rail_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let m = &self.model;
        let me = cx.entity();
        let dismiss = me.clone();
        let mut rows = div().flex().flex_col().w_full();
        if m.drawer_sidebar {
            let v = me.clone();
            rows = rows.child(
                ListRow::new("menu-sidebar", &m.theme)
                    .icon(Icon::Folder)
                    .title("Barra lateral")
                    .selected(m.drawer_open)
                    .on_click(move |_, _, cx| {
                        v.update(cx, |_, cx| cx.emit(TitleBarEvent::ToggleDrawer))
                    }),
            );
        }
        for item in m.rail_items.clone() {
            let RailItem {
                id,
                icon,
                title,
                active,
                action,
            } = item;
            let mut row = ListRow::new(id, &m.theme)
                .icon(icon)
                .title(title)
                .selected(active);
            if let Some(a) = action {
                let v = me.clone();
                row = row.on_click(move |_, _, cx| {
                    v.update(cx, |_, cx| cx.emit(TitleBarEvent::Rail(a.clone())))
                });
            }
            rows = rows.child(row);
        }
        let popover = Popover::new("rail-menu", &m.theme)
            .open(self.menu_open)
            .placement(Placement::BottomEnd)
            .padding(4.0)
            .width(240.0)
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, |t, cx| {
                    t.menu_open = false;
                    cx.emit(TitleBarEvent::MenusChanged);
                    cx.notify();
                });
            })
            .trigger(
                IconButton::new("menu-btn", &m.theme, Icon::Dots)
                    .size(28.0)
                    .active(self.menu_open)
                    .emphasize_on_hover(true)
                    .tooltip("Menú")
                    .on_click(move |_, _, cx| {
                        me.update(cx, |t, cx| {
                            let open = !t.menu_open;
                            t.set_menus(open, false, cx);
                        });
                    }),
            )
            .child(rows);
        div()
            .h_full()
            .flex()
            .flex_col()
            .justify_center()
            .child(popover)
    }

    /// The "all tabs" menu (the XS tab selector): every tab in a list.
    fn tabs_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let m = &self.model;
        let me = cx.entity();
        let dismiss = me.clone();
        let mut rows = div()
            .flex()
            .flex_col()
            .w_full()
            .max_h(px(360.0))
            .overflow_hidden();
        for info in &m.infos {
            let id = info.id;
            let v = me.clone();
            let mut row = ListRow::new(("alltab", id), &m.theme)
                .icon(info.icon.icon())
                .when_some(crate::agents::agent_of(&info.icon), |r, mark| r.agent(mark))
                .title(info.title.clone())
                .selected(Some(id) == m.active)
                .on_click(move |_, _, cx| {
                    v.update(cx, |t, cx| {
                        t.tabs_menu_open = false;
                        cx.emit(TitleBarEvent::Select(id));
                        cx.emit(TitleBarEvent::MenusChanged);
                    })
                });
            if let Some(sub) = info.subtitle.clone() {
                row = row.subtitle(sub);
            }
            if let Some(st) = info.status {
                row = row.status(st);
            }
            rows = rows.child(row);
        }
        let popover = Popover::new("tabs-menu", &m.theme)
            .open(self.tabs_menu_open)
            .placement(Placement::BottomEnd)
            .padding(4.0)
            .width(280.0)
            .sheet_when_narrow(true)
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, |t, cx| {
                    t.tabs_menu_open = false;
                    cx.emit(TitleBarEvent::MenusChanged);
                    cx.notify();
                });
            })
            .trigger(
                IconButton::new("tabs-menu-btn", &m.theme, Icon::ChevronDown)
                    .size(28.0)
                    .active(self.tabs_menu_open)
                    .emphasize_on_hover(true)
                    .tooltip("Todas las pestañas")
                    .on_click(move |_, _, cx| {
                        me.update(cx, |t, cx| {
                            let open = !t.tabs_menu_open;
                            t.set_menus(false, open, cx);
                        });
                    }),
            )
            .child(rows);
        div()
            .h_full()
            .flex()
            .flex_col()
            .justify_center()
            .child(popover)
    }

    /// The tab pills in a horizontally scrolling strip (never narrower than [`TAB_MIN_W`] each),
    /// with arrows that appear when the tabs overflow.
    fn tab_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let m = &self.model;
        let theme = m.theme;
        let c = theme.colors;
        let me = cx.entity();
        let active = m.active;
        let handle = self.tab_scroll.clone();
        if std::mem::take(&mut self.scroll_active)
            && let Some(ix) = m.infos.iter().position(|i| Some(i.id) == active)
        {
            handle.scroll_to_item(ix);
        }
        let max = f32::from(handle.max_offset().width);
        let offset = f32::from(handle.offset().x);
        let overflow = max > 0.5;
        let mut pills = div()
            .id("tab-strip")
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .overflow_x_scroll()
            .track_scroll(&handle);
        for info in &m.infos {
            let id = info.id;
            let (s1, s2) = (me.clone(), me.clone());
            pills = pills.child(div().flex_none().min_w(px(TAB_MIN_W)).child(tab_pill(
                &theme,
                info,
                Some(id) == active,
                move |_, _, cx| s1.update(cx, |_, cx| cx.emit(TitleBarEvent::Select(id))),
                move |_, _, cx| s2.update(cx, |_, cx| cx.emit(TitleBarEvent::Close(id))),
            )));
        }
        // The chevron-right icon, turned half a circle for "previous".
        let arrow = |id: &'static str, left: bool, enabled: bool| {
            let h = handle.clone();
            let me = me.clone();
            let mut glyph = icon(
                Icon::ChevronRight,
                12.0,
                if enabled { c.muted } else { c.outline },
            );
            if left {
                glyph = glyph.with_transformation(gpui::Transformation::rotate(gpui::radians(
                    std::f32::consts::PI,
                )));
            }
            div()
                .id(id)
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::CONTROL))
                .when(enabled, |d| {
                    d.cursor_pointer().hover(move |s| s.bg(c.surface_2))
                })
                .on_click(move |_, _, cx| {
                    if enabled {
                        let o = h.offset();
                        let step = if left { 160.0 } else { -160.0 };
                        let x =
                            (f32::from(o.x) + step).clamp(-f32::from(h.max_offset().width), 0.0);
                        h.set_offset(gpui::point(px(x), o.y));
                        me.update(cx, |_, cx| cx.notify());
                    }
                })
                .child(glyph)
        };
        div()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .children(overflow.then(|| arrow("tabs-left", true, offset < -0.5)))
            .child(pills)
            .children(overflow.then(|| arrow("tabs-right", false, offset > -max + 0.5)))
    }

    /// Left of the titlebar in the unified layout: the active tab's title and, muted, its project
    /// (or the branch when the tab is named after the project).
    fn heading(&self) -> impl IntoElement + use<> {
        let m = &self.model;
        let c = m.theme.colors;
        let ty = m.theme.typography;
        let active = m.active.and_then(|id| m.infos.iter().find(|i| i.id == id));
        let title: SharedString = active.map_or_else(|| "Selachi".into(), |i| i.title.clone());
        let project = active
            .and_then(crate::grouping::project_key)
            .map(|p| crate::grouping::project_name(&p));
        let muted: Option<SharedString> = match (project, active.and_then(|i| i.subtitle.clone())) {
            (Some(p), _) if p != title.as_ref() => Some(p.into()),
            (_, branch) => branch,
        };
        div()
            .min_w_0()
            .flex()
            .flex_row()
            .items_baseline()
            .gap(px(10.0))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_size(px(ty.ui_size_lg))
                    .text_color(c.text)
                    .child(title),
            )
            .children(muted.map(|text| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.muted)
                    .child(text)
            }))
    }

    /// The panel buttons at the right of the titlebar (`layout.titlebar.panel_buttons`).
    fn buttons(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let m = &self.model;
        let me = cx.entity();
        let mut group = div()
            .h_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0));
        for b in &m.buttons {
            match b {
                TitleButton::Layout => group = group.child(self.layout_button(cx)),
                TitleButton::Panel {
                    id,
                    icon: ic,
                    title,
                    active,
                    action,
                } => {
                    let v = me.clone();
                    let action = action.clone();
                    group = group.child(
                        div().h_full().flex().flex_col().justify_center().child(
                            IconButton::new(SharedString::from(format!("tb-{id}")), &m.theme, *ic)
                                .size(28.0)
                                .active(*active)
                                .emphasize_on_hover(true)
                                .tooltip(title.clone())
                                .on_click(move |_, _, cx| {
                                    let a = action.clone();
                                    v.update(cx, |_, cx| cx.emit(TitleBarEvent::Rail(a)));
                                }),
                        ),
                    );
                }
            }
        }
        group
    }
}

impl Render for TitleBarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::repaint::bump(crate::repaint::View::TitleBar);
        let me = cx.entity();
        let (theme, pad_left) = (self.model.theme, self.model.pad_left);
        let mut row = title_row(&theme).pl(px(pad_left));
        if self.model.opaque {
            row = row.bg(theme.colors.bg);
        }
        if self.model.unified_heading {
            row = row.child(self.heading());
        } else if self.model.breadcrumb {
            let last_only = self.model.breadcrumb_last_only;
            let detail: SharedString = match &self.model.active_cwd {
                Some(cwd) if last_only => paths::tab_label(cwd).into(),
                Some(cwd) => paths::breadcrumb(cwd).into(),
                None => "~".into(),
            };
            row = row.child(breadcrumb(&theme, detail, last_only));
        }
        if self.model.tab_strip {
            row = row.child(self.tab_strip(cx));
            if self.model.xs || self.model.infos.len() > 1 {
                row = row.child(self.tabs_menu(cx));
            }
            let v = me;
            row = row.child(new_tab_button(&theme, move |_, _, cx| {
                v.update(cx, |_, cx| cx.emit(TitleBarEvent::NewTab))
            }));
        }
        row = row.child(div().flex_1()).child(self.buttons(cx));
        if self.model.rail_in_menu {
            row = row.child(self.rail_menu(cx));
        }
        if self.model.with_controls {
            row = row.child(window_controls(&theme, window.is_maximized()));
        } else {
            row = row.pr(px(8.0));
        }
        row
    }
}

impl TitleBarModel {
    /// An empty bar: the shell pushes the real model on its first sync.
    pub fn blank(theme: Theme) -> Self {
        Self {
            theme,
            infos: Vec::new(),
            active: None,
            active_cwd: None,
            unified_heading: false,
            breadcrumb: false,
            breadcrumb_last_only: false,
            tab_strip: false,
            xs: false,
            rail_in_menu: false,
            drawer_sidebar: false,
            drawer_open: false,
            buttons: Vec::new(),
            rail_items: Vec::new(),
            layout_panel: None,
            with_controls: false,
            pad_left: 0.0,
            opaque: false,
        }
    }
}
