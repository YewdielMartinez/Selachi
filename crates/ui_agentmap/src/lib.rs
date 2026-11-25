//! `ui_agentmap`: the subagent map (PLAN section 21.2), the view of the delegation harness.
//!
//! * [`AgentMapModel`]: the entity the app owns. Builds a forest (orchestrator -> delegations ->
//!   their delegations) from the chat threads and their delegation links (`chat_core`) and from the
//!   run registry the harness writes in `%APPDATA%\Selachi\agent-runs`. It is **event-driven**: a
//!   `store::DirWatcher` on that folder plus the chat store's update stream, both active only while
//!   a view is attached. With the map closed it holds nothing and does nothing.
//! * [`AgentMapView`] + [`AgentMapPanelProvider`] (id `agentmap`, zones Main and Right): a compact
//!   vertical tree by default, an optional graph (columns by depth, connectors painted with quads)
//!   and an optional timeline (Gantt bars that show the parallelism). Rows are virtualized.
//! * The inspector (click a node): the exact prompt the orchestrator sent (redacted), the limits that
//!   applied, who launched it, the transcript, the diff, the approvals and the actions. Actions are
//!   [`AgentMapEvent`]s; the map runs nothing.
//!
//! ```ignore
//! // once, at startup
//! let source = Arc::new(ui_agentmap::ChatStoreSource::new(chat_store.clone()));
//! let runs_dir = store::paths::app_data_dir()?.join("agent-runs");
//! let map = cx.new(|_| AgentMapModel::new(source, runs_dir));
//! registry.register(AgentMapPanelProvider::new(map.clone()));
//! cx.subscribe(&map, |_, ev: &AgentMapEvent, cx| { /* OpenThread, OpenDiff, Cancel, Retry... */ }).detach();
//!
//! // from a "Delegó en…" row, the sidebar icon or the palette:
//! map.update(cx, |m, cx| m.open_run(child_thread_id, cx));   // + show the "agentmap" panel
//! // from a thread / tab that has not delegated yet: m.set_root(Some(NodeKey::Thread(id)), cx)
//! ```
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod events;
mod fmt;
mod graph_ui;
mod inspector;
mod model;
mod parts;
mod registry;
mod source;
mod tree;
mod tree_ui;
mod view;

pub use events::AgentMapEvent;
pub use inspector::{
    Available, EXTERNAL_EXPLANATION, RETRY_AGENTS, available, decision_label, external_facts,
    limit_rows, permission_label,
};
pub use model::{
    AgentMapModel, CHAT_DEBOUNCE, MapChanged, NodeDetail, ProfileSeed, PromptInfo, PromptSource,
    WATCH_DEBOUNCE,
};
pub use registry::{ApprovalEntry, DiffStatInfo, ResultInfo, RunInfo, RunStatus, read_runs};
pub use source::{ChatStoreSource, SessionSource};
pub use tree::{
    Bar, ExternalProc, Forest, ForestInput, GraphLayout, MAX_EXTERNAL_PER_SESSION, MapNode,
    NodeKey, NodeKind, NodeStatus, Placed, Row, Timeline, Totals, agent_display, agent_id,
    build_forest, derive_status, graph_layout, timeline, visible_rows,
};
pub use view::{
    AgentMapPanelProvider, AgentMapView, INSPECTOR_WIDTH, MAP_ICON, MapMode, PANEL_ID, SPLIT_FROM,
    thumbnail,
};

#[cfg(test)]
mod tests;
