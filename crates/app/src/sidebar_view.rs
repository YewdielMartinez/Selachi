//! `SidebarView`: the sidebar as an entity of its own (docs/auditoria-gpui.md H4/H8).
//!
//! It paints the whole card (header with the logo, collapse button and window controls, then the
//! unified or the classic body) from a [`SidebarModel`] the shell pushes whenever something it
//! shows changed, and it owns what only the sidebar cares about: which row / space header is under
//! the pointer and the keyboard focus of its zone. A hover therefore notifies *this* view, never
//! the shell. Everything the user does here leaves as a [`SidebarEvent`]; the shell subscribes.
//!
//! Rows are tab stops (`Delete` closes the focused tab through the shell's close flow, which asks
//! first when something is running) and the zone itself can be focused as a whole
//! ([`SidebarView::focus_first`]: Ctrl+Shift+E, F6).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Hsla, InteractiveElement,
    IntoElement, MouseButton, Render, Stateful, Subscription, Window, actions, div, prelude::*, px,
};
use panel_api::{TabId, TabInfo};
use store::{ResolvedLayout, SidebarSide, TabsPosition};
use theme::{Radius, Theme};
use ui_kit::{Logo, glass_selected};

use crate::rail::{RailAction, RailItem};
use crate::sidebar::{SidebarProps, sidebar_collapsed, sidebar_content};
use crate::spaces::{self, GitSummary};
use crate::titlebar::{TITLEBAR_H, new_tab_button, window_controls};
use crate::unified_sidebar::{
    UnifiedProps, Usage, collapse_button, hover_transition, unified_collapsed, unified_content,
};
use crate::usage_monitor::UsageMonitor;

actions!(
    sidebar,
    [
        /// `Delete` on a focused row: close its tab (asks first when it is running).
        CloseFocusedRow
    ]
);

/// How a sidebar card is drawn.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SidebarOpts {
    /// The window controls live in this zone (it is the outermost one on the right).
    pub with_controls: bool,
    /// Icon-only column (S).
    pub collapsed: bool,
    /// A drawer / expanded overlay over the main area.
    pub overlay: bool,
    /// Just the body in a rounded tinted box (zen mode's floating sidebar: no header).
    pub bare: bool,
}

/// A delegated thread listed under its parent chat tab (PLAN section 20).
#[derive(Clone, Debug, PartialEq)]
pub struct ChildRow {
    /// The child's chat thread id.
    pub thread: i64,
    /// Its tab when it is open (clicking selects it; otherwise the thread is opened).
    pub tab: Option<TabId>,
    pub title: gpui::SharedString,
    /// Agent name for the mark (`"codex"`).
    pub agent: String,
    pub status: Option<panel_api::RowStatus>,
    /// "Terminó · 3 archivos", "En curso".
    pub subtitle: gpui::SharedString,
    /// 0 = delegated by the parent tab itself, 1 = delegated by that child...
    pub depth: usize,
}

/// The parent -> child delegations of the open chat tabs.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Lineage {
    /// Children listed under a parent tab, oldest first.
    pub children: HashMap<TabId, Vec<ChildRow>>,
    /// Open tabs that are children of an open tab: shown nested, not as a top-level row too.
    pub nested: HashSet<TabId>,
}

/// Everything the sidebar paints that comes from the shell. Pushed with
/// [`SidebarView::set_model`], which repaints only when it differs.
#[derive(Clone, PartialEq)]
pub struct SidebarModel {
    pub theme: Theme,
    /// The layout this placement draws (the docked one, the drawer's, the zen strip's).
    pub layout: ResolvedLayout,
    pub infos: Vec<TabInfo>,
    pub active: Option<TabId>,
    /// Branch and dirty flag of the repositories the open terminals are in.
    pub git: HashMap<PathBuf, GitSummary>,
    /// Favorite and recent projects (listed as spaces without tabs).
    pub known: Vec<(PathBuf, String)>,
    /// Keys of the folded spaces.
    pub folded: HashSet<PathBuf>,
    pub panel_items: Vec<RailItem>,
    pub opts: SidebarOpts,
    /// Glass tint of the column (`None`: the opaque card).
    pub tint: Option<Hsla>,
    /// Outer margin of the card (density).
    pub margin: f32,
    /// Delegations nested under their parent tab (unified sidebar only).
    pub lineage: Lineage,
}

impl SidebarModel {
    /// An empty sidebar (nothing to show yet): the shell pushes the real model on its first sync.
    pub fn blank(theme: Theme, layout: ResolvedLayout) -> Self {
        Self {
            theme,
            layout,
            infos: Vec::new(),
            active: None,
            git: HashMap::new(),
            known: Vec::new(),
            folded: HashSet::new(),
            panel_items: Vec::new(),
            opts: SidebarOpts::default(),
            tint: None,
            margin: 8.0,
            lineage: Lineage::default(),
        }
    }
}

/// What the sidebar asks of the shell.
#[derive(Clone, Debug, PartialEq)]
pub enum SidebarEvent {
    Select(TabId),
    Close(TabId),
    /// A new terminal in the active tab's folder.
    NewHere,
    /// A new terminal in this folder (the "+" of a space, or a project without tabs).
    NewIn(PathBuf),
    ToggleSpace(PathBuf),
    /// "Nueva pestaña": the Terminal / Chat menu.
    NewMenu,
    Palette,
    Settings,
    ToggleUsage,
    Rail(RailAction),
    /// Ctrl+B: fold / unfold to icons.
    ToggleCollapsed,
    /// The collapsed S column's expand button: the overlay.
    ExpandOverlay,
    DropOnSpace(Vec<PathBuf>),
    /// A delegated thread row that has no tab yet: open its thread.
    OpenThread(i64),
}

/// Emits `ev` from `view` (what the row callbacks of [`crate::sidebar`] and
/// [`crate::unified_sidebar`] call).
pub fn send(view: &Entity<SidebarView>, cx: &mut App, ev: SidebarEvent) {
    view.update(cx, |_, cx| cx.emit(ev));
}

/// Background of a keyboard-focused row.
pub fn focus_bg(theme: &Theme, glass: bool) -> Hsla {
    if glass {
        glass_selected(theme)
    } else {
        theme.colors.surface_4.into()
    }
}

/// Keyboard access for the sidebar's own rows (the `ui_kit` controls already have it).
pub trait RowFocus {
    /// A tab stop that shows `focused_bg` while it has the keyboard focus. A mouse press does
    /// not take the focus (a click must not steal it from the terminal), so the highlight only
    /// follows the keyboard.
    fn tab_stop_row(self, focused_bg: Hsla) -> Self;
}

impl RowFocus for Stateful<Div> {
    fn tab_stop_row(self, focused_bg: Hsla) -> Self {
        self.tab_index(0)
            .focus(move |s| s.bg(focused_bg))
            .on_action(|_: &ui_kit::FocusNext, window, _| window.focus_next())
            .on_action(|_: &ui_kit::FocusPrev, window, _| window.focus_prev())
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
    }
}

pub struct SidebarView {
    model: SidebarModel,
    /// The tab row under the pointer (drives the close button: state, not group-hover).
    hovered_row: Option<TabId>,
    /// The space header under the pointer, by its key (the project path).
    hovered_space: Option<PathBuf>,
    usage: Entity<UsageMonitor>,
    focus: FocusHandle,
    _usage_sub: Subscription,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    pub fn new(model: SidebarModel, usage: Entity<UsageMonitor>, cx: &mut Context<Self>) -> Self {
        // The footer meter: repaint this view (not the shell) when the numbers change.
        let usage_sub = cx.observe(&usage, |_, _, cx| cx.notify());
        Self {
            model,
            hovered_row: None,
            hovered_space: None,
            usage,
            focus: cx.focus_handle().tab_index(0).tab_stop(false),
            _usage_sub: usage_sub,
        }
    }

    pub fn set_model(&mut self, model: SidebarModel, cx: &mut Context<Self>) {
        if self.model != model {
            self.model = model;
            cx.notify();
        }
    }

    /// The pointer entered / left a tab row. Repaints only on a change.
    pub fn set_row_hover(&mut self, id: TabId, hovered: bool, cx: &mut Context<Self>) {
        let next = hover_transition(self.hovered_row, id, hovered);
        if next != self.hovered_row {
            self.hovered_row = next;
            cx.notify();
        }
    }

    /// The pointer entered / left a space header (keyed by the space's path).
    pub fn set_space_hover(&mut self, key: &PathBuf, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(key.clone())
        } else if self.hovered_space.as_ref() == Some(key) {
            None
        } else {
            self.hovered_space.clone()
        };
        if next != self.hovered_space {
            self.hovered_space = next;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn hovered_row(&self) -> Option<TabId> {
        self.hovered_row
    }

    #[cfg(test)]
    pub fn hovered_space(&self) -> Option<&PathBuf> {
        self.hovered_space.as_ref()
    }

    /// Does the keyboard focus sit anywhere inside this sidebar?
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Moves the keyboard focus to the first tab stop of the sidebar (its container when it
    /// has none).
    pub fn focus_first(&self, window: &mut Window, cx: &App) {
        window.focus(&self.focus);
        window.focus_next();
        if !self.focus.contains_focused(window, cx) {
            // Nothing focusable inside (or the next stop is elsewhere): keep the container.
            window.focus(&self.focus);
        }
    }

    fn usage_summary(&self, cx: &App) -> Usage {
        match self.usage.read(cx).info() {
            Some(u) => match u.fresh_five_hour().or_else(|| u.fresh_official()) {
                Some(o) => Usage {
                    fraction: Some((o.percent / 100.0).clamp(0.0, 1.0) as f32),
                    label: format!("{:.0} %", o.percent).into(),
                },
                None => Usage {
                    fraction: None,
                    label: "estimado".into(),
                },
            },
            None => Usage {
                fraction: None,
                label: "…".into(),
            },
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::repaint::bump(crate::repaint::View::Sidebar);
        let me = cx.entity();
        let m = &self.model;
        let theme = &m.theme;
        let c = theme.colors;
        let l = &m.layout;
        let opts = m.opts;
        let tint = m.tint;
        let right_side = l.sidebar.side == SidebarSide::Right;
        let unified = l.sidebar.nested_tabs;

        let mut header = div()
            .h(px(TITLEBAR_H))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .window_control_area(gpui::WindowControlArea::Drag);
        if opts.collapsed {
            header = header.justify_center();
            if l.rail == store::RailVisibility::Hidden {
                header = header.child(Logo::new(theme.mode).size(28.0));
            }
        } else if l.rail == store::RailVisibility::Hidden && !right_side {
            header = header
                .pl(px(m.margin + 8.0))
                .child(Logo::new(theme.mode).size(28.0));
        } else {
            header = header.pl(px(m.margin));
        }
        if !opts.collapsed {
            header = header.child(div().flex_1());
            if unified {
                let v = me.clone();
                header = header.child(div().pr(px(6.0)).child(collapse_button(
                    theme,
                    tint.is_some(),
                    move |_, cx| send(&v, cx, SidebarEvent::ToggleCollapsed),
                )));
            } else if l.tabs_position == TabsPosition::Sidebar {
                let v = me.clone();
                header = header.child(new_tab_button(theme, move |_, _, cx| {
                    send(&v, cx, SidebarEvent::NewHere)
                }));
            }
        }
        if opts.with_controls {
            header = header.child(window_controls(theme, window.is_maximized()));
        }

        let body: AnyElement = if unified {
            let up = UnifiedProps {
                theme,
                layout: l,
                tabs: &m.infos,
                active: m.active,
                view: &me,
                glass: tint.is_some(),
                spaces: spaces::build_spaces(&m.infos, l.sidebar.group_by, &m.git, &m.known),
                folded: &m.folded,
                lineage: &m.lineage,
                hovered_row: self.hovered_row,
                hovered_space: self.hovered_space.clone(),
                usage: self.usage_summary(cx),
            };
            let panel_items: Vec<RailItem> = m.panel_items.clone();
            if opts.collapsed {
                unified_collapsed(&up, panel_items).into_any_element()
            } else {
                unified_content(&up, panel_items).into_any_element()
            }
        } else {
            let props = SidebarProps {
                theme,
                layout: l,
                tabs: &m.infos,
                active: m.active,
                view: &me,
                hovered_row: self.hovered_row,
                glass: tint.is_some(),
            };
            if opts.collapsed {
                sidebar_collapsed(&props).into_any_element()
            } else {
                sidebar_content(&props).into_any_element()
            }
        };
        if opts.bare {
            return div()
                .size_full()
                .rounded(px(Radius::MD))
                .when_some(tint, |d, t| d.bg(t))
                .overflow_hidden()
                .track_focus(&self.focus)
                .child(body)
                .into_any_element();
        }
        let card = div()
            .flex_1()
            .min_h_0()
            .mb(px(m.margin))
            .rounded(px(Radius::MD))
            .overflow_hidden()
            .when(tint.is_none(), |d| d.bg(Hsla::from(c.surface_1)))
            .child(body);
        let mut col = div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .child(header);
        if let Some(t) = tint {
            col = col.bg(t);
        }
        let card = if opts.collapsed {
            card.mx(px(4.0))
        } else if right_side {
            card.mr(px(m.margin))
        } else {
            card.ml(px(m.margin))
        };
        col.child(card).into_any_element()
    }
}
