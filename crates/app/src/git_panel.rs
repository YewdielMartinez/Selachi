//! Built-in right-panel providers backed by the git monitor: `git` (branch, ahead / behind,
//! changed files) and `context` (the same data as a thread context card: worktree, branch,
//! upstream, changes). They prove the [`PanelProvider`] contract end to end: the views adapt to
//! [`WidthClass`], never look at their zone, and are re-targeted in place when the active tab
//! changes directory ([`PanelProvider::context_changed`]).
//!
//! Everything shown is read from `git status` (through [`GitMonitor`]); nothing is invented.

use std::path::Path;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Window, div, prelude::*, px,
};
use panel_api::{Icon, PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Radius, Theme, rgb};
use ui_kit::{SectionLabel, icon};

use crate::git_monitor::{ChangedFile, GitInfo, GitMonitor};
use crate::paths;

/// Which flavour of the panel to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `git`: status of the repository.
    Git,
    /// `context`: the active session's worktree and changes.
    Context,
}

/// Registers as `git` or `context`.
pub struct GitPanelProvider(pub Variant);

impl PanelProvider for GitPanelProvider {
    fn id(&self) -> PanelId {
        match self.0 {
            Variant::Git => PanelId::new(store::config::panel_ids::GIT),
            Variant::Context => PanelId::new(store::config::panel_ids::CONTEXT),
        }
    }

    fn title(&self) -> SharedString {
        match self.0 {
            Variant::Git => "Git".into(),
            Variant::Context => "Contexto".into(),
        }
    }

    fn icon(&self) -> Icon {
        match self.0 {
            Variant::Git => Icon::GitBranch,
            // The session's context: its own rail button, not a second git icon.
            Variant::Context => Icon::Agents,
        }
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Right, PanelZone::Sidebar, PanelZone::Main]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let variant = self.0;
        cx.new(|cx| GitPanel::new(ctx, variant, cx)).into()
    }

    fn context_changed(&self, view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        let Ok(panel) = view.clone().downcast::<GitPanel>() else { return false };
        panel.update(cx, |p, cx| p.retarget(ctx.clone(), cx));
        true
    }
}

pub struct GitPanel {
    ctx: PanelCtx,
    variant: Variant,
    git: Entity<GitMonitor>,
    _sub: Subscription,
}

impl GitPanel {
    fn new(ctx: PanelCtx, variant: Variant, cx: &mut Context<Self>) -> Self {
        let git = cx.new(|_| GitMonitor::new());
        // The monitor notifies when its data changes: repaint with it.
        let sub = cx.observe(&git, |_, _, cx| cx.notify());
        let mut panel = Self { ctx: ctx.clone(), variant, git, _sub: sub };
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
        .rounded(px(8.0))
        .bg(c.surface_2)
        .text_size(px(ty.ui_size_sm))
        .child(div().text_color(if n > 0 { c.text } else { c.muted }).child(n.to_string()))
        .child(div().text_color(c.muted).child(label))
}

fn file_row(theme: &Theme, i: usize, f: &ChangedFile) -> impl IntoElement {
    let c = theme.colors;
    let ty = theme.typography;
    // Long paths keep their tail (the file name).
    let shown = paths::middle_truncate(&f.path.replace('/', "\\"), 44);
    div()
        .id(("git-file", i))
        .h(px(22.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .rounded(px(6.0))
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
        .child(div().text_size(px(11.0)).text_color(c.muted).child(key))
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

impl GitPanel {
    fn render_info(&self, info: &GitInfo) -> impl IntoElement {
        let theme = self.ctx.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let compact = self.ctx.width_class == WidthClass::Compact;

        let mut head = div().flex().flex_row().items_center().gap(px(8.0));
        head = head
            .child(icon(Icon::GitBranch, 16.0, c.muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(ty.ui_size_lg))
                    .text_color(c.text)
                    .child(info.branch.clone()),
            );
        if info.ahead > 0 {
            head = head.child(div().text_color(c.muted).text_size(px(ty.ui_size_sm)).child(format!("↑{}", info.ahead)));
        }
        if info.behind > 0 {
            head = head.child(div().text_color(c.muted).text_size(px(ty.ui_size_sm)).child(format!("↓{}", info.behind)));
        }

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
            .children(info.files.iter().enumerate().map(|(i, f)| file_row(&theme, i, f)))
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

        let mut col = div().size_full().flex().flex_col().gap(px(12.0)).p(px(if compact { 12.0 } else { 16.0 }));
        if self.variant == Variant::Context {
            let worktree: SharedString = info.root.display().to_string().into();
            col = col
                .child(SectionLabel::new(&theme, "Sesión").uppercase(true))
                .child(kv(&theme, "Worktree", worktree))
                .child(kv(&theme, "Rama", info.branch.clone()))
                .children(info.upstream.clone().map(|u| kv(&theme, "Upstream", u)))
                .child(SectionLabel::new(&theme, "Cambios").uppercase(true))
                .child(stats)
                .child(list);
        } else {
            col = col
                .child(head)
                .children(info.upstream.clone().map(|u| {
                    div().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(format!("sigue a {u}"))
                }))
                .child(stats)
                .child(list);
        }
        col
    }
}

impl Render for GitPanel {
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
            },
        };
        div().size_full().rounded(px(Radius::MD)).overflow_hidden().font_family(ty.ui_family).child(body)
    }
}

fn dir_name(p: &Path) -> String {
    paths::tab_label(p)
}
