//! `ui_chat`: the chat mode UI (wave 5.6, PLAN section 14). A pure view of [`chat_core`].
//!
//! | Piece | What it is |
//! |---|---|
//! | [`ChatHub`] | one entity per app: owns the `ChatStore`, pumps its `updates()`, caches the thread list, routes [`ChatEvent`]s |
//! | [`ChatTab`] / [`ChatView`] | `TabModel` for the Main zone + its body: header, virtualized conversation, plan bar, composer, new-thread screen |
//! | [`ThreadListView`] / [`ThreadsPanelProvider`] | grouped thread list (sidebar section or `PanelProvider` `"threads"`) with rename / archive / delete |
//! | [`Composer`] | field (`Enter` sends, `Shift+Enter` newline, `@` mentions), `+`, context meter, permission / model / agent pills, send-stop |
//! | [`rows`] | the pure view-model: `ThreadState` blocks to list rows, fingerprints, splices |
//!
//! # Integration
//!
//! ```ignore
//! // 1. Once at startup (after gpui::init): key bindings of the text field.
//! ui_chat::init(cx);
//!
//! // 2. The store. ProcessHooks bridges chat_core to proc_svc (owner per thread, see chat_core::ProcessHooks).
//! let store = chat_core::ChatStore::builder().hooks(Arc::new(MyProcessHooks::new(tracker))).open()?;
//! let hub = ui_chat::ChatHub::create(store, theme, cx);        // Entity<ChatHub>
//! hub.update(cx, |h, cx| { h.detect_agents(cx); h.set_projects(project_roots, cx); });
//!
//! // 3. Panels and tabs.
//! registry.register(ui_chat::ThreadsPanelProvider::new(hub.clone()));   // PanelProvider "threads"
//! let tab = hub.update(cx, |h, cx| h.new_thread_tab(Some(project), cx)); // TabHandle (Main zone)
//! let tab = hub.update(cx, |h, cx| h.open_thread_tab(thread_id, cx));    // existing thread
//!
//! // 4. Route what the chat asks of the host.
//! cx.subscribe(&hub, |_, ev: &ui_chat::ChatEvent, cx| match ev {
//!     ChatEvent::OpenThread(id) => shell.focus_or_add(hub.update(cx, |h, cx| h.open_thread_tab(*id, cx))),
//!     ChatEvent::NewThread { project } => shell.add_tab(hub.update(cx, |h, cx| h.new_thread_tab(project.clone(), cx))),
//!     ChatEvent::OpenInTerminal { command, cwd, .. } => shell.open_terminal_tab_running(command, cwd),
//!     ChatEvent::OpenDiff { path, cwd, .. } => viewer.open_with_diff(cwd.join(path)),
//!     ChatEvent::RestoreCheckpoint { id, .. } => confirm_then_restore(id),
//!     ChatEvent::ThreadRemoved(id) => shell.close_tab_of_thread(*id),
//! }).detach();
//!
//! // 5. Keep the list's highlight in step with the front tab.
//! hub.update(cx, |h, cx| h.set_active_thread(Some(thread_id), cx));
//! ```
//!
//! In the unified sidebar (wave 5.10) the thread rows are the *tab rows* of each space (the shell
//! paints `ChatTab`'s `TabInfo`); [`ThreadListView`] is the standalone version of the same list.
//!
//! Zero idle cost: no timers. A `ThreadUpdate` only notifies the views of that thread; a hidden
//! thread reads nothing (rows are built when painted).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![allow(clippy::too_many_arguments)]

mod chat_view;
mod composer;
mod field;
mod hub;
mod md;
mod mentions;
mod options;
mod row_view;
pub mod rows;
mod tab;
mod thread_list;
mod util;
mod widgets;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use chat_view::{ChatView, ChatViewEvent, DraftSpec, READING_WIDTH_XL, ViewMode, XL_FROM};
pub use composer::{ButtonMode, Composer, ComposerEvent, ComposerState, Popup, Preview, button_mode};
pub use field::{ChatField, FieldEvent, Nav, TextBuf};
pub use hub::{ChatEvent, ChatHub, FilesReady, ModelsReady};
pub use md::{MdBody, ParsedMd, parse_markdown};
pub use mentions::{MentionQuery, filter_files, mention_at, walk_files};
pub use options::{
    AgentOption, EFFORTS, ModelOption, PERMISSION_ORDER, agent_mark, default_agents, effort_label, efforts_for, model_effort_label,
    model_label, models_for, permission_hint, permission_label, pretty_model,
};
pub use tab::ChatTab;
pub use thread_list::{
    DeleteDialog, GroupKey, GroupMode, ListItem, PANEL_ID as THREADS_PANEL_ID, ThreadListView, ThreadsPanelProvider, build_items,
    subtitle,
};
pub use util::{format_cost, format_duration_ms, format_tokens, read_branch};

/// Registers the key bindings of the chat text field. Call once at startup.
pub fn init(cx: &mut gpui::App) {
    field::bind_keys(cx);
}

/// SVGs this crate wants from the user (`assets/icons/<file>`, 16x16 grid, white fill or
/// `currentColor`) and where each is used. Placeholders (marked `TODO-SVG` in the code) stand in
/// until they exist.
pub fn needed_svgs() -> &'static [(&'static str, &'static str)] {
    &[
        ("stop.svg", "round stop button of the composer while a turn runs (placeholder: a rounded square drawn with a div)"),
        ("copy.svg", "copy button of code blocks (placeholder: the word \"Copiar\")"),
        ("archive.svg", "archive action of a thread row (placeholder: the folder icon)"),
        ("attach.svg", "attach files: the + menu entry (placeholder: files.svg)"),
        ("chevron-up.svg", "plan bar and popovers that open upwards (placeholder: chevron-right / chevron-down)"),
    ]
}
