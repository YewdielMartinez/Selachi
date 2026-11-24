//! `ui_review`: diff review with line notes for the agent (A9) and the attention queue (A11).
//!
//! | Piece | What it is |
//! |---|---|
//! | [`ReviewTab`] / [`ReviewView`] | `TabModel` + content for the Main zone: file list, unified diff per file, line notes (click the gutter, shift-click for a range), "Enviar al agente" |
//! | [`ReviewSource`] | what is reviewed: the worktree, what changed since a checkpoint, or given files |
//! | [`Note`] / [`NoteSet`] | notes anchored by content hash (they follow their lines when the diff refreshes); states Borrador / Enviada / Resuelta |
//! | [`build_markdown`] | the Spanish Markdown message, grouped by file, with `path:line` and a +-3 line excerpt, secrets redacted |
//! | [`ReviewStore`] | in-memory drafts per `(root, source)` + JSON export / import for the host to persist |
//! | [`AttentionModel`] / [`AttentionView`] / [`AttentionPanelProvider`] | the queue of agents waiting for you: model fed by the host, compact list, `PanelProvider` `attention` |
//!
//! # Integration: review
//!
//! ```ignore
//! ui_review::init(cx);                                      // once: key bindings of the note field
//! let tab = ui_review::open_review_tab(ReviewSource::Worktree { root }, theme, cx);   // TabHandle
//! let review = tab.downcast::<ReviewTab>().unwrap();
//! cx.subscribe(&review, |review, ev: &ReviewEvent, cx| match ev {
//!     ReviewEvent::SendToAgent { markdown, notes } => {
//!         deliver_to_agent(markdown);                      // a terminal tab running the agent, or a chat thread
//!         let ids: Vec<_> = notes.iter().map(|n| n.id).collect();
//!         review.update(cx, |t, cx| t.mark_sent(&ids, cx)); // Borrador -> Enviada
//!     }
//!     ReviewEvent::NotesChanged { .. } => save(review.read(cx).export_notes(cx)),
//! }).detach();
//! review.update(cx, |t, cx| t.refresh(cx));                 // when the agent finishes a turn
//! ```
//!
//! # Integration: attention queue
//!
//! ```ignore
//! let model = cx.new(|_| AttentionModel::new());
//! registry.register(AttentionPanelProvider::new(model.clone()));        // PanelProvider "attention"
//! // From the agent events the host already receives (same id for the request and the status):
//! if let Some(kind) = AttentionKind::from_agent_event(&event) {
//!     model.update(cx, |m, cx| m.upsert(AttentionItem { id, owner_label, agent, kind, since: Instant::now(),
//!                                                        focus_target: FocusTarget::new(tab_id) }, cx));
//! } else { model.update(cx, |m, cx| { m.remove(&id, cx); }); }           // Working / Done
//! cx.subscribe(&model, |_, ev: &AttentionEvent, cx| match ev {
//!     AttentionEvent::Jump(target) => shell.focus_tab(target),
//!     AttentionEvent::Dismiss(_) | AttentionEvent::Changed { .. } => update_status_bar_badge(),
//! }).detach();
//! // Ctrl+Shift+A (the host owns the key): model.update(cx, |m, cx| m.jump_next(cx));
//! ```
//!
//! Zero idle cost: no watchers, no timers. The diff loads in the background executor on open and
//! on `refresh`; the attention list ticks (1 s) only while it is painted.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod attention;
mod attention_view;
mod diff;
mod markdown;
mod note;
mod review;
#[cfg(test)]
mod review_tests;
mod store;
mod tab;
mod tone;

pub use attention::{
    AttentionEvent, AttentionId, AttentionItem, AttentionKind, AttentionModel, FocusTarget,
    agent_label, format_wait,
};
pub use attention_view::{AttentionPanelProvider, AttentionView, PANEL_ID as ATTENTION_PANEL_ID};
pub use diff::{DiffRow, FileBody, FileDiff, FileStatus, ReviewSource, load_source};
pub use markdown::{ReviewMessage, build_markdown, fence_language};
pub use note::{
    EXCERPT_RADIUS, Note, NoteId, NoteRef, NoteSet, NoteState, Reanchor, SEARCH_WINDOW, Side,
    SideLines, excerpt, line_hash, lines_hash, range_hash,
};
pub use review::{ROW_PAGE, ReviewEvent, ReviewView};
pub use store::{DraftKey, ReviewStore, draft_key, root_key};
pub use tab::{ReviewTab, open_review_tab};
pub use viewer_svc::GitBase;

/// Registers the key bindings of the shared text field (`ui_kit::init`). Idempotent; call once at startup.
pub fn init(cx: &mut gpui::App) {
    ui_kit::init(cx);
}

/// SVGs this crate still wants. None: review, attention, send, note, trash and edit exist.
/// `(file under assets/icons, where it is used)`.
pub fn needed_svgs() -> &'static [(&'static str, &'static str)] {
    &[]
}
