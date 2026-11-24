//! What the map tells the host. The map never runs anything itself: every action of the inspector
//! is one of these events and the host does the work (and shows its own preview / confirmation
//! where the action changes files).

use chat_core::ThreadId;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentMapEvent {
    /// Open the child's thread (its transcript) as a chat tab.
    OpenThread(ThreadId),
    /// "Ver diff": show the diff of the run's worktree (run id).
    OpenDiff(String),
    /// Stop the run (the user already confirmed in the inspector). The host writes the cancel marker.
    Cancel(String),
    /// Run it again with `agent` (and `model` when it is the same agent): the user confirmed.
    Retry {
        run_id: String,
        agent: String,
        model: Option<String>,
    },
    /// Open the run's worktree folder (in the viewer or the file manager).
    OpenWorktree(PathBuf),
    /// Bring the run's changes to the project. The host shows the preview (`apply` plan) and asks.
    ApplyChanges(String),
    /// "Duplicar como perfil": the host opens Ajustes -> Agentes -> Perfiles with a new profile
    /// seeded from this run (see `AgentMapModel::profile_seed`).
    DuplicateAsProfile(String),
}
