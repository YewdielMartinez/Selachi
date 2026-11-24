//! The built-in `context` right-panel provider: the active session's worktree, branch, upstream and
//! changes as a thread context card. The `git` panel itself lives in the `ui_git` crate; this one
//! stays because the context card is a different view of the same data. It proves the
//! [`PanelProvider`] contract end to end: the view adapts to [`WidthClass`], never looks at its
//! zone, and is re-targeted in place when the active tab changes directory
//! ([`PanelProvider::context_changed`]).
//!
//! Everything shown is read from `git status` (through [`GitMonitor`]); nothing is invented.

use std::path::Path;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Radius, Theme, rgb};
use ui_kit::SectionLabel;

use crate::git_monitor::{ChangedFile, GitInfo, GitMonitor};
use crate::paths;

/// Registers as `context`.
pub struct ContextPanelProvider;

impl PanelProvider for ContextPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(store::config::panel_ids::CONTEXT)
    }

    fn title(&self) -> SharedString {
        "Contexto".into()
    }

    fn icon(&self) -> Icon {
        // The session's context: its own rail button, not a second git icon.
        Icon::Agents
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Right, PanelZone::Sidebar, PanelZone::Main]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|cx| ContextPanel::new(ctx, cx)).into()
    }

    fn context_changed(
        &self,
        view: &AnyView,
        ctx: &PanelCtx,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Ok(panel) = view.clone().downcast::<ContextPanel>() else {
            return false;
        };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

pub struct ContextPanel {
    ctx: PanelCtx,
    git: Entity<GitMonitor>,
    _sub: Subscription,
}

impl ContextPanel {
    fn new(ctx: PanelCtx, cx: &mut Context<Self>) -> Self {
        let git = cx.new(|_| GitMonitor::new());
        // The monitor notifies when its data changes: repaint with it.
        let sub = cx.observe(&git, |_, _, cx| cx.notify());
        let mut panel = Self {
            ctx: ctx.clone(),
            git,
            _sub: sub,
        };
        panel.retarget(ctx, cx);
        panel
    }

    fn retarget(&mut self, ctx: PanelCtx, cx: &mut Context<Self>) {
        if let Some(cwd) = ctx.cwd.clone() {
            self.git.update(cx, |g, cx| g.set_cwd(cwd, cx));
        }
        self.ctx = ctx;
        cx.notify();
    }
}

/// Color of a status letter.
fn code_color(theme: &Theme, code: char) -> gpui::Rgba {
    let ansi = |i: usize| rgb(theme.terminal.ansi[i]);
    match code {
        'A' | 'C' => ansi(2),
        'M' | 'T' | 'R' => ansi(3),
        'D' => ansi(1),
        '!' => theme.colors.error,
        _ => theme.colors.muted,
    }
}

fn stat(theme: &Theme, label: &'static str, n: u32) -> impl IntoElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .h(px(22.0))
        .rounded(px(Radius::CONTROL))
        .bg(c.surface_2)
        .text_size(px(ty.ui_size_sm))
        .child(
            div()
                .text_color(if n > 0 { c.text } else { c.muted })
                .child(n.to_string()),
        )
        .child(div().text_color(c.muted).child(label))
}

fn file_row(theme: &Theme, f: &ChangedFile) -> impl IntoElement {
    let c = theme.colors;
    let ty = theme.typography;
    // Long paths keep their tail (the file name).
    let shown = paths::middle_truncate(&f.path.replace('/', "\\"), 44);
    div()
        .id(SharedString::from(format!("git-file:{}", f.path)))
        .h(px(22.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .rounded(px(Radius::CHIP))
        .hover(move |s| s.bg(c.surface_2))
        .text_size(px(ty.ui_size_sm))
        .child(
            div()
                .w(px(12.0))
                .flex_none()
                .text_color(code_color(theme, f.code))
                .child(f.code.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(if f.staged { c.text } else { c.muted })
                .child(shown),
        )
}

fn kv(theme: &Theme, key: &'static str, value: impl Into<SharedString>) -> impl IntoElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(ty.ui_size_xs))
                .text_color(c.muted)
                .child(key),
        )
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(ty.ui_size_md))
                .text_color(c.text)
                .child(value.into()),
        )
}

impl ContextPanel {
    fn render_info(&self, info: &GitInfo) -> impl IntoElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let compact = self.ctx.width_class == WidthClass::Compact;

        let stats = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.0))
            .child(stat(&theme, "staged", info.staged))
            .child(stat(&theme, "modificados", info.modified))
            .child(stat(&theme, "nuevos", info.untracked))
            .children((info.conflicted > 0).then(|| stat(&theme, "conflictos", info.conflicted)));

        let total = info.staged + info.modified + info.untracked + info.conflicted;
        let list = div()
            .id("git-files")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(info.files.iter().map(|f| file_row(&theme, f)))
            .children((total as usize > info.files.len()).then(|| {
                div()
                    .px(px(6.0))
                    .py(px(4.0))
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .child(format!("… y {} más", total as usize - info.files.len()))
            }))
            .children((total == 0).then(|| {
                div()
                    .px(px(6.0))
                    .py(px(4.0))
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .child("Árbol de trabajo limpio")
            }));

        let worktree: SharedString = info.root.display().to_string().into();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(if compact { 12.0 } else { 16.0 }))
            .child(SectionLabel::new(&theme, "Sesión").uppercase(true))
            .child(kv(&theme, "Worktree", worktree))
            .child(kv(&theme, "Rama", info.branch.clone()))
            .children(info.upstream.clone().map(|u| kv(&theme, "Upstream", u)))
            .child(SectionLabel::new(&theme, "Cambios").uppercase(true))
            .child(stats)
            .child(list)
    }
}

impl Render for ContextPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let info = self.git.read(cx).info().cloned();
        let body = match info {
            Some(info) => self.render_info(&info).into_any_element(),
            None => {
                let msg = match &self.ctx.cwd {
                    Some(p) => format!("{} no está en un repositorio git", dir_name(p)),
                    None => "Sin directorio de trabajo".to_string(),
                };
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(16.0))
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.muted)
                    .child(msg)
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .rounded(px(Radius::MD))
            .overflow_hidden()
            .font_family(ty.ui_family)
            .child(body)
    }
}

fn dir_name(p: &Path) -> String {
    paths::tab_label(p)
}
