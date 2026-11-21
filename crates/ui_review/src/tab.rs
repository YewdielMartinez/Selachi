//! `ReviewTab`: the `panel_api::TabModel` wrapping a [`ReviewView`], so the shell lists a review as
//! a pill / sidebar row and hosts its body in a pane of the Main zone.
//!
//! The tab owns the view; closing the tab stops its background load. The notes live in the
//! [`crate::ReviewStore`], so reopening the same review brings them back.

use std::path::PathBuf;

use gpui::{AnyView, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, SharedString, Subscription, Window};
use panel_api::{CloseCheck, TabEvent, TabHandle, TabIcon, TabModel};
use theme::Theme;
use ui_kit::{Icon, RowStatus};

use crate::diff::ReviewSource;
use crate::note::NoteId;
use crate::review::{ReviewEvent, ReviewView};

/// A diff review tab.
pub struct ReviewTab {
    view: Entity<ReviewView>,
    source: ReviewSource,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for ReviewTab {}
/// The view's events are re-emitted so the host subscribes to the tab it already holds.
impl EventEmitter<ReviewEvent> for ReviewTab {}

impl ReviewTab {
    pub fn new(source: ReviewSource, theme: Theme, cx: &mut Context<Self>) -> Self {
        let view = cx.new(|cx| ReviewView::new(source.clone(), theme, cx));
        let forward = cx.subscribe(&view, |_, _, ev: &ReviewEvent, cx| {
            cx.emit(ev.clone());
            if matches!(ev, ReviewEvent::NotesChanged { .. }) {
                cx.emit(TabEvent::SubtitleChanged);
                cx.emit(TabEvent::StatusChanged);
            }
        });
        Self { view, source, _subs: vec![forward] }
    }

    /// The view (to drive it: `refresh`, `select_path`, `begin_note`...).
    pub fn view(&self) -> &Entity<ReviewView> {
        &self.view
    }

    pub fn source(&self) -> &ReviewSource {
        &self.source
    }

    /// The host delivered the message of `ReviewEvent::SendToAgent`: those notes become "Enviada".
    pub fn mark_sent(&self, ids: &[NoteId], cx: &mut App) {
        self.view.update(cx, |v, cx| v.mark_sent(ids, cx));
    }

    pub fn mark_resolved(&self, ids: &[NoteId], cx: &mut App) {
        self.view.update(cx, |v, cx| v.mark_resolved(ids, cx));
    }

    /// Re-reads the diff (the agent finished a turn); notes follow their lines.
    pub fn refresh(&self, cx: &mut App) {
        self.view.update(cx, |v, cx| v.refresh(cx));
    }

    /// The notes of this review as JSON.
    pub fn export_notes(&self, cx: &App) -> String {
        self.view.read(cx).export_notes()
    }

    /// Merges notes exported earlier. Returns how many were added.
    pub fn import_notes(&self, json: &str, cx: &mut App) -> Result<usize, String> {
        self.view.update(cx, |v, cx| v.import_notes(json, cx))
    }
}

impl TabModel for ReviewTab {
    fn title(&self, _cx: &App) -> SharedString {
        "Revisión".into()
    }

    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Kind(Icon::Review)
    }

    fn subtitle(&self, cx: &App) -> Option<SharedString> {
        let (total, _) = self.view.read(cx).counts();
        let title = self.source.title();
        Some(if total == 0 { title } else { format!("{title} · {total} {}", if total == 1 { "nota" } else { "notas" }) }.into())
    }

    fn status(&self, cx: &App) -> Option<RowStatus> {
        // Drafts are waiting for you to send them.
        (self.view.read(cx).counts().1 > 0).then_some(RowStatus::Waiting)
    }

    fn cwd(&self, _cx: &App) -> Option<PathBuf> {
        Some(self.source.root().to_path_buf())
    }

    fn project_root(&self, _cx: &App) -> Option<PathBuf> {
        Some(self.source.root().to_path_buf())
    }

    fn content(&self, _cx: &App) -> AnyView {
        self.view.clone().into()
    }

    fn can_close(&self, cx: &App) -> CloseCheck {
        let drafts = self.view.read(cx).counts().1;
        if drafts == 0 {
            CloseCheck::Allow
        } else {
            CloseCheck::Confirm(
                format!(
                    "Hay {drafts} {} en borrador sin enviar. Se conservan mientras Selachi siga abierto.",
                    if drafts == 1 { "nota" } else { "notas" }
                )
                .into(),
            )
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.view.update(cx, |v, _| v.release());
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        window.focus(&self.view.read(cx).focus_handle(cx));
    }

    fn theme_changed(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.set_theme(theme, cx));
    }
}

/// Opens a review tab, ready to hand to the shell.
pub fn open_review_tab(source: ReviewSource, theme: Theme, cx: &mut App) -> TabHandle {
    TabHandle::new(cx.new(|cx| ReviewTab::new(source, theme, cx)))
}
