//! `ViewerTab`: the `panel_api::TabModel` wrapping a [`FileViewer`], so the shell lists a file as a
//! pill / sidebar row and hosts its body in a pane of the Main zone.
//!
//! The tab owns the viewer entity; closing the tab calls [`FileViewer::release`], which drops the
//! `Document` (mmap and line index) and cancels every background task.

use std::path::{Path, PathBuf};

use gpui::{AnyView, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, SharedString, Subscription, Window};
use panel_api::{CloseCheck, TabEvent, TabHandle, TabIcon, TabModel};
use theme::Theme;
use ui_kit::Icon;

use crate::event::ViewerEvent;
use crate::state::Body;
use crate::viewer::{FileViewer, ViewerConfig};

/// A file tab.
pub struct ViewerTab {
    viewer: Entity<FileViewer>,
    path: PathBuf,
    root: Option<PathBuf>,
    title: SharedString,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for ViewerTab {}
/// The viewer's events (`OpenInEditor`, `InstallGrammar`) are re-emitted so the host subscribes to
/// the tab it already holds.
impl EventEmitter<ViewerEvent> for ViewerTab {}

fn title_for(path: &Path, diff: bool) -> SharedString {
    let name = path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    if diff { format!("{name} (diff)").into() } else { name.into() }
}

impl ViewerTab {
    pub fn new(path: PathBuf, cfg: ViewerConfig, theme: Theme, cx: &mut Context<Self>) -> Self {
        let root = cfg.project_root.clone();
        let viewer = cx.new(|cx| FileViewer::new(path.clone(), cfg, theme, cx));
        let forward = cx.subscribe(&viewer, |_, _, ev: &ViewerEvent, cx| cx.emit(ev.clone()));
        // The title only changes when the diff toggle flips; observing the viewer is cheap
        // (a string compare) and keeps the pill honest.
        let observe = cx.observe(&viewer, |this, viewer, cx| {
            let diff = viewer.read(cx).body() == Body::Diff;
            let title = title_for(&this.path, diff);
            if title != this.title {
                this.title = title;
                cx.emit(TabEvent::TitleChanged);
            }
        });
        Self { title: title_for(&path, false), viewer, path, root, _subs: vec![forward, observe] }
    }

    /// The viewer entity (to drive it: `toggle_diff`, `goto_line`...).
    pub fn viewer(&self) -> &Entity<FileViewer> {
        &self.viewer
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TabModel for ViewerTab {
    fn title(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Kind(Icon::Files)
    }

    fn subtitle(&self, _cx: &App) -> Option<SharedString> {
        let dir = self.path.parent()?;
        let shown = crate::format::display_path(self.root.as_deref(), dir);
        (!shown.is_empty()).then(|| shown.into())
    }

    fn cwd(&self, _cx: &App) -> Option<PathBuf> {
        self.path.parent().map(Path::to_path_buf)
    }

    fn project_root(&self, _cx: &App) -> Option<PathBuf> {
        self.root.clone()
    }

    fn content(&self, _cx: &App) -> AnyView {
        self.viewer.clone().into()
    }

    fn can_close(&self, _cx: &App) -> CloseCheck {
        CloseCheck::Allow
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.viewer.update(cx, |v, _| v.release());
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        window.focus(&self.viewer.read(cx).focus_handle(cx));
    }

    fn theme_changed(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.viewer.update(cx, |v, cx| v.set_theme(theme, cx));
    }
}

/// Opens `path` in a new viewer tab, ready to hand to the shell.
pub fn open_tab(path: PathBuf, cfg: ViewerConfig, theme: Theme, cx: &mut App) -> TabHandle {
    TabHandle::new(cx.new(|cx| ViewerTab::new(path, cfg, theme, cx)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(title_for(Path::new("C:/p/src/main.rs"), false).as_ref(), "main.rs");
        assert_eq!(title_for(Path::new("C:/p/src/main.rs"), true).as_ref(), "main.rs (diff)");
    }
}
