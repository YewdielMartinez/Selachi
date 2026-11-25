//! Where the map reads chat threads from. A trait so tests (and the demo) can feed fixed data;
//! [`ChatStoreSource`] is the real one over `chat_core::ChatStore`.
//!
//! Every method may touch SQLite: the model calls them on the background executor, never on the UI
//! thread.

use chat_core::{
    ApprovalState, Archived, Block, ChatStore, Role, ThreadFilter, ThreadId, ThreadRecord,
    ThreadUpdate,
};
use futures::channel::mpsc::UnboundedReceiver;

use crate::activity::{ActivityEvent, LiveHint, chat_events, live_hint};
use crate::registry::ApprovalEntry;

/// Read access to the chat threads of the session.
pub trait SessionSource: Send + Sync + 'static {
    /// Every thread (archived ones included): the map picks the delegated ones and their parents.
    fn threads(&self) -> Vec<ThreadRecord>;

    /// First message of the user in the thread. For a delegated child this is the prompt the
    /// orchestrator sent (already redacted when it was stored).
    fn first_user_message(&self, id: ThreadId) -> Option<String>;

    /// The approvals the thread asked for, oldest first.
    fn approvals(&self, id: ThreadId) -> Vec<ApprovalEntry>;

    /// What the child thread did: its assistant text, commands, edits and approvals as feed
    /// events (source A of the activity feed). `thread_active`: the run is still in flight.
    /// Empty when the source has nothing (the events log may still have it).
    fn activity(&self, _id: ThreadId, _thread_active: bool) -> Vec<ActivityEvent> {
        Vec::new()
    }

    /// What a running thread is doing right now, for the line under its name in the tree. Cheap.
    fn live_hint(&self, _id: ThreadId) -> LiveHint {
        LiveHint::None
    }

    /// Stream of "something changed in a thread" (the store's own). `None` when the source has no
    /// live updates: the host then calls `AgentMapModel::refresh`.
    fn updates(&self) -> Option<UnboundedReceiver<ThreadUpdate>> {
        None
    }
}

/// [`SessionSource`] over the app's `ChatStore`.
#[derive(Clone)]
pub struct ChatStoreSource {
    store: ChatStore,
}

impl ChatStoreSource {
    pub fn new(store: ChatStore) -> Self {
        Self { store }
    }
}

fn decision_word(state: ApprovalState) -> &'static str {
    match state {
        ApprovalState::Pending => "pending",
        ApprovalState::Allowed => "allowed",
        ApprovalState::Denied => "denied",
        ApprovalState::AlwaysAllowed => "always",
        ApprovalState::Expired => "expired",
    }
}

impl SessionSource for ChatStoreSource {
    fn threads(&self) -> Vec<ThreadRecord> {
        let filter = ThreadFilter {
            archived: Archived::Include,
            ..ThreadFilter::default()
        };
        self.store.list_threads(&filter).unwrap_or_default()
    }

    fn first_user_message(&self, id: ThreadId) -> Option<String> {
        // Opening a runtime loads the messages and starts nothing: the agent only starts on `send`.
        let rt = self.store.open(id).ok()?;
        rt.with_state(|s| {
            s.messages
                .iter()
                .find(|m| m.role == Role::User)
                .map(|m| m.plain_text())
                .filter(|t| !t.trim().is_empty())
        })
    }

    fn approvals(&self, id: ThreadId) -> Vec<ApprovalEntry> {
        let Ok(rt) = self.store.open(id) else {
            return Vec::new();
        };
        rt.with_state(|s| {
            s.messages
                .iter()
                .flat_map(|m| m.blocks.iter())
                .filter_map(|b| match b {
                    Block::Approval(a) => Some(ApprovalEntry {
                        tool: a.tool.clone(),
                        detail: a.detail.clone(),
                        decision: decision_word(a.state).to_string(),
                        at_ms: a.decided_ms.or(Some(a.received_ms)),
                    }),
                    _ => None,
                })
                .collect()
        })
    }

    fn activity(&self, id: ThreadId, thread_active: bool) -> Vec<ActivityEvent> {
        let Ok(rt) = self.store.open(id) else {
            return Vec::new();
        };
        rt.with_state(|s| chat_events(&s.messages, thread_active))
    }

    fn live_hint(&self, id: ThreadId) -> LiveHint {
        let Ok(rt) = self.store.open(id) else {
            return LiveHint::None;
        };
        rt.with_state(|s| live_hint(&s.messages))
    }

    fn updates(&self) -> Option<UnboundedReceiver<ThreadUpdate>> {
        Some(self.store.updates())
    }
}
