//! Sidebar content: the sections of `layout.sidebar.sections`, built from the open tabs.
//!
//! * `spaces` / `projects`: one row per project among the open tabs (git root, else cwd).
//! * `worktrees`: one row per distinct worktree (branch + project).
//! * `agents` / `threads`: the tab rows ([`TabInfo`] painted as [`ListRow`]: icon or status,
//!   title, branch, elapsed time), grouped by `group_by`. When the tabs live in the sidebar and
//!   neither section is enabled, the list is appended as "Pestañas" so tabs are never lost.
//!
//! Only real data is shown: nothing here comes from anywhere but the tabs and their git state.

use std::time::Instant;

use gpui::{
    App, ClickEvent, Div, Entity, IntoElement, SharedString, Stateful, Window, div, prelude::*, px,
};
use panel_api::{TabId, TabInfo, format_elapsed};
use store::{Density, ResolvedLayout, SidebarSection};
use theme::Theme;
use ui_kit::{GhostButton, Icon, IconButton, ListRow, SectionLabel};

use crate::app_shell::AppShell;
use crate::grouping::{self, aggregate_status, group_tabs};
use crate::paths;

pub struct SidebarProps<'a> {
    pub theme: &'a Theme,
    pub layout: &'a ResolvedLayout,
    pub tabs: &'a [TabInfo],
    pub active: Option<TabId>,
    pub shell: &'a Entity<AppShell>,
    /// The tab row under the pointer (state kept by the shell).
    pub hovered_row: Option<TabId>,
    /// The column is glass: rows use translucent pills so the backdrop shows through.
    pub glass: bool,
}

fn section_title(section: SidebarSection, forced_tabs: bool) -> &'static str {
    match section {
        SidebarSection::Spaces => "Espacios",
        SidebarSection::Projects => "Proyectos",
        SidebarSection::Worktrees => "Worktrees",
        SidebarSection::Agents if forced_tabs => "Pestañas",
        SidebarSection::Agents => "Agentes",
        SidebarSection::Threads => "Hilos",
    }
}

/// The icon-only sidebar of the S class: an expand button (opens the full sidebar as an overlay)
/// and one icon per tab. Clicks select the tab; hovering shows its title.
pub fn sidebar_collapsed(p: &SidebarProps) -> Stateful<Div> {
    let theme = p.theme;
    let shell = p.shell.clone();
    let mut col = div()
        .id("sidebar-collapsed")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .py(px(8.0))
        .child(
            IconButton::new("sb-expand", theme, Icon::ChevronRight)
                .glass(p.glass)
                .tooltip("Expandir la barra lateral")
                .on_click(move |_, _, cx| shell.update(cx, |s, cx| s.toggle_sidebar_overlay(cx))),
        );
    for info in p.tabs {
        let id = info.id;
        let shell = p.shell.clone();
        col = col.child(
            IconButton::new(
                SharedString::from(format!("sb-tab-{}", id.as_u64())),
                theme,
                info.icon.icon(),
            )
            .glass(p.glass)
            .active(Some(id) == p.active)
            .tooltip(info.title.clone())
            .on_click(move |_, window, cx| shell.update(cx, |s, cx| s.select_tab(id, window, cx))),
        );
    }
    col
}

/// The scrollable body of the sidebar card.
pub fn sidebar_content(p: &SidebarProps) -> Stateful<Div> {
    let compact = p.layout.density == Density::Compact;
    let mut sections = p.layout.sidebar.sections.clone();
    let has_list = sections
        .iter()
        .any(|s| matches!(s, SidebarSection::Agents | SidebarSection::Threads));
    let forced = p.layout.tabs_position == store::TabsPosition::Sidebar && !has_list;
    if forced {
        sections.push(SidebarSection::Agents);
    }
    let labels = sections.len() > 1;
    let mut body = div()
        .id("sidebar-body")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(if compact { 8.0 } else { 14.0 }))
        .p(px(if compact { 6.0 } else { 8.0 }));
    for section in sections {
        let mut block = div().flex().flex_col();
        if labels {
            block =
                block.child(div().px(px(10.0)).pt(px(2.0)).pb(px(4.0)).child(
                    SectionLabel::new(p.theme, section_title(section, forced)).uppercase(true),
                ));
        }
        block = match section {
            SidebarSection::Spaces => block.children(project_rows(p, true)),
            SidebarSection::Projects => block.children(project_rows(p, false)),
            SidebarSection::Worktrees => block.children(worktree_rows(p)),
            SidebarSection::Agents | SidebarSection::Threads => block.child(tab_list(p)),
        };
        body = body.child(block);
    }
    body
}

fn select(
    shell: &Entity<AppShell>,
    id: TabId,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let shell = shell.clone();
    move |_, window, cx| shell.update(cx, |s, cx| s.select_tab(id, window, cx))
}

/// Projects among the open tabs. `rich` (spaces) adds the branch and the aggregated status.
fn project_rows(p: &SidebarProps, rich: bool) -> Vec<impl IntoElement + use<>> {
    let entries = grouping::projects(p.tabs);
    entries
        .into_iter()
        .map(|e| {
            let members: Vec<&TabInfo> = p
                .tabs
                .iter()
                .filter(|t| grouping::project_key(t).as_deref() == Some(e.root.as_path()))
                .collect();
            let contains_active = members.iter().any(|t| Some(t.id) == p.active);
            let first = &p.tabs[e.first_tab];
            let mut row = ListRow::new((if rich { "space" } else { "project" }, first.id), p.theme)
                .icon(Icon::Folder)
                .glass(p.glass)
                .selected(contains_active)
                .on_click(select(p.shell, first.id));
            if rich {
                row = row.title(e.name.clone());
                if let Some(branch) = members.iter().find_map(|t| t.subtitle.clone()) {
                    row = row.subtitle(branch);
                }
                if let Some(s) = aggregate_status(members.iter().map(|t| t.status)) {
                    row = row.status(s);
                }
            } else {
                row = row
                    .title(e.name.clone())
                    .subtitle(paths::breadcrumb(&e.root));
            }
            if e.tabs > 1 {
                row = row.meta(format!("{}", e.tabs));
            }
            row
        })
        .collect()
}

/// One row per distinct worktree that has a branch.
fn worktree_rows(p: &SidebarProps) -> Vec<impl IntoElement + use<>> {
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    let mut rows = Vec::new();
    for t in p.tabs {
        let (Some(root), Some(branch)) = (t.project_root.clone(), t.subtitle.clone()) else {
            continue;
        };
        if seen.contains(&root) {
            continue;
        }
        let selected = p
            .tabs
            .iter()
            .any(|o| o.project_root.as_ref() == Some(&root) && Some(o.id) == p.active);
        rows.push(
            ListRow::new(("worktree", t.id), p.theme)
                .icon(Icon::GitBranch)
                .glass(p.glass)
                .title(branch)
                .subtitle(grouping::project_name(&root))
                .selected(selected)
                .on_click(select(p.shell, t.id)),
        );
        seen.push(root);
    }
    rows
}

/// The tabs as rows, grouped, plus the "new tab" action.
fn tab_list(p: &SidebarProps) -> impl IntoElement + use<> {
    let groups = group_tabs(p.tabs, p.layout.sidebar.group_by);
    let shell = p.shell.clone();
    let mut col = div().flex().flex_col();
    for g in groups {
        if let Some(label) = g.label.clone() {
            col = col.child(
                div()
                    .px(px(10.0))
                    .pt(px(6.0))
                    .pb(px(2.0))
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(SectionLabel::new(p.theme, label))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(p.theme.colors.outline)
                            .child(g.tabs.len().to_string()),
                    ),
            );
        }
        for ix in g.tabs {
            col = col.child(tab_row(p, &p.tabs[ix]));
        }
    }
    col.child(
        div().mt(px(4.0)).child(
            GhostButton::new("sb-new-tab", p.theme, "Nueva pestaña")
                .icon(Icon::Plus)
                .on_click(move |_, window, cx| {
                    shell.update(cx, |s, cx| s.new_tab_here(window, cx));
                }),
        ),
    )
}

/// A tab as a sidebar row, with a close button that appears on hover.
fn tab_row(p: &SidebarProps, info: &TabInfo) -> impl IntoElement + use<> {
    let theme = p.theme;
    let c = theme.colors;
    let id = info.id;
    let show_close = crate::unified_sidebar::close_visible(Some(id) == p.active, p.hovered_row, id);
    let shell_hover = p.shell.clone();
    let mut row = ListRow::new(("srow", id), theme)
        .on_hover(move |h, _, cx| shell_hover.update(cx, |s, cx| s.set_row_hover(id, h, cx)))
        .icon(info.icon.icon())
        .when_some(crate::agents::agent_of(&info.icon), |r, m| r.agent(m))
        .glass(p.glass)
        .title(info.title.clone())
        .selected(Some(id) == p.active)
        .on_click(select(p.shell, id));
    if let Some(s) = info.subtitle.clone() {
        row = row.subtitle(s);
    }
    if let Some(s) = info.status {
        row = row.status(s);
        if let Some(t) = info.last_activity.filter(|_| !show_close) {
            row = row.meta(format_elapsed(
                Instant::now().saturating_duration_since(t).as_secs(),
            ));
        }
    }
    let shell = p.shell.clone();
    let close_id: SharedString = format!("srow-close-{}", id.as_u64()).into();
    div()
        .relative()
        .child(row)
        .children(show_close.then(|| {
            div().absolute().right(px(8.0)).top(px(10.0)).child(
                IconButton::new(close_id, theme, Icon::Close)
                    .glass(p.glass)
                    .passthrough(true)
                    .size(22.0)
                    .icon_size(10.0)
                    .tooltip("Cerrar (Ctrl+W)")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        shell.update(cx, |s, cx| s.request_close(id, window, cx));
                    }),
            )
        }))
        .text_color(c.text)
}
