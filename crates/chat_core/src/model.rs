//! Plain data types of the chat model: ids, thread metadata, filters and grouping.

use std::fmt;
use std::path::{Path, PathBuf};

use agents_driver::{AgentKind, PermissionMode};
use serde::{Deserialize, Serialize};

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    store::now_ms()
}

/// Database id of a thread (also usable as the `proc_svc` owner id: `id.owner_id()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ThreadId(pub i64);

impl ThreadId {
    /// Owner id to hand to `proc_svc::Tracker` (`OwnerId = u64`).
    pub fn owner_id(self) -> u64 {
        self.0 as u64
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Lifecycle state of a thread (also persisted in `threads.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ThreadStatus {
    /// Never ran, or the app restarted while it was busy.
    #[default]
    Idle,
    /// A turn is running.
    Working,
    /// Blocked on the user (an approval).
    WaitingForInput,
    /// The last turn finished.
    Done,
    /// The last turn failed.
    Error,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::WaitingForInput => "waiting",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "working" => Self::Working,
            "waiting" => Self::WaitingForInput,
            "done" => Self::Done,
            "error" => Self::Error,
            _ => Self::Idle,
        }
    }

    /// Sidebar group.
    pub fn group(self) -> ThreadGroup {
        match self {
            Self::Working => ThreadGroup::Working,
            Self::WaitingForInput => ThreadGroup::Waiting,
            Self::Idle | Self::Done | Self::Error => ThreadGroup::Settled,
        }
    }

    /// A turn is in flight (Working or Waiting).
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Working | Self::WaitingForInput)
    }
}

/// Sidebar grouping: `Working` (spinner), `Waiting` (needs you), `Settled` (everything else).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThreadGroup {
    Working,
    Waiting,
    Settled,
}

/// Answer to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Allow,
    Deny,
    /// Allow now and for the rest of the session of this thread (see [`crate::ThreadRuntime::approve`]).
    Always,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "user" => Self::User,
            "system" => Self::System,
            _ => Self::Assistant,
        }
    }
}

/// Where a cost figure comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CostSource {
    /// The agent CLI reported it (`cost_usd`): exact.
    Reported,
    /// Computed from the local `agents_svc` price table (label it "estimated").
    Estimated,
}

impl CostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Estimated => "estimated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reported" => Some(Self::Reported),
            "estimated" => Some(Self::Estimated),
            _ => None,
        }
    }
}

/// What to create (see [`crate::ChatStore::create_thread`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSpec {
    /// `None`: untitled; the first user message becomes the title.
    pub title: Option<String>,
    pub project_root: PathBuf,
    pub agent: AgentKind,
    pub model: Option<String>,
    /// Reasoning effort (`low | medium | high | xhigh | max`); `None` = the agent's default.
    pub effort: Option<String>,
    pub permission: PermissionMode,
    /// Create a git worktree `selachi/<slug>` for the thread and run the agent there (A8).
    pub use_worktree: bool,
    /// Take a git checkpoint before every turn in Full / AcceptEdits mode (A10).
    pub checkpoint_before_turn: bool,
    /// `Some` when this thread is a delegation (a child of another thread / a CLI or MCP run).
    pub link: Option<ThreadLink>,
}

/// Delegation lineage of a thread (PLAN section 20): who started it and how deep the chain is.
/// Stored next to the thread (`threads.parent_thread_id`, `parent_tab`, `depth`, `delegated_by`);
/// threads that are not delegations have no link (`depth` 0, nothing else).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadLink {
    /// The thread that delegated this one, when it is a Selachi chat thread.
    pub parent_thread_id: Option<ThreadId>,
    /// Opaque id of the terminal tab that delegated this one (a tab of the shell, not a thread).
    pub parent_tab: Option<String>,
    /// Hops from the top-level session (1 = delegated by a top-level session).
    pub depth: u32,
    /// Who made the delegation: `cli`, `mcp`, ... A thread with this set is a delegation even
    /// when it has no parent thread (the delegating session was a terminal).
    pub delegated_by: Option<String>,
}

impl ThreadSpec {
    pub fn new(project_root: impl Into<PathBuf>, agent: AgentKind) -> Self {
        Self {
            title: None,
            project_root: project_root.into(),
            agent,
            model: None,
            effort: None,
            permission: PermissionMode::Ask,
            use_worktree: false,
            checkpoint_before_turn: false,
            link: None,
        }
    }
}

/// Thread metadata as stored in `threads`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRecord {
    pub id: ThreadId,
    /// Empty when untitled; see [`ThreadRecord::display_title`].
    pub title: String,
    pub project_root: PathBuf,
    pub worktree_path: Option<PathBuf>,
    pub agent: AgentKind,
    pub model: Option<String>,
    /// Reasoning effort chosen for this thread (see [`ThreadSpec::effort`]).
    pub effort: Option<String>,
    pub permission: PermissionMode,
    pub status: ThreadStatus,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Session id of the agent CLI (Claude `--resume`, Codex thread id, ACP session id).
    pub agent_session_id: Option<String>,
    pub archived: bool,
    pub checkpoint_before_turn: bool,
    /// Delegation lineage (`depth` 0 and nothing else when the thread is not a delegation).
    pub link: ThreadLink,
}

/// Title shown for untitled threads.
pub const UNTITLED: &str = "Nuevo hilo";

impl ThreadRecord {
    /// Was this thread started by another session through the harness (CLI or MCP)?
    pub fn is_delegation(&self) -> bool {
        self.link.delegated_by.is_some() || self.link.parent_thread_id.is_some()
    }

    /// Directory the agent runs in: the worktree when there is one, else the project root.
    pub fn cwd(&self) -> &Path {
        self.worktree_path.as_deref().unwrap_or(&self.project_root)
    }

    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            UNTITLED
        } else {
            &self.title
        }
    }

    pub fn group(&self) -> ThreadGroup {
        self.status.group()
    }
}

/// How [`ThreadFilter`] treats archived threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Archived {
    /// Hide archived threads (default).
    #[default]
    Exclude,
    Only,
    Include,
}

/// Filter for [`crate::ChatStore::list_threads`]. Status / group are matched against the live
/// status (a running thread reports its in-memory status, not the last persisted one).
#[derive(Debug, Clone, Default)]
pub struct ThreadFilter {
    pub project: Option<PathBuf>,
    pub status: Option<ThreadStatus>,
    pub group: Option<ThreadGroup>,
    pub archived: Archived,
}

/// Threads split for the sidebar. Each list is newest-first (`updated_ms` descending).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadGroups {
    pub working: Vec<ThreadRecord>,
    pub waiting: Vec<ThreadRecord>,
    pub settled: Vec<ThreadRecord>,
}

impl ThreadGroups {
    /// Split `threads` (any order) into the three groups, newest first.
    pub fn from_threads(threads: &[ThreadRecord]) -> Self {
        let mut g = ThreadGroups::default();
        for t in threads {
            match t.group() {
                ThreadGroup::Working => g.working.push(t.clone()),
                ThreadGroup::Waiting => g.waiting.push(t.clone()),
                ThreadGroup::Settled => g.settled.push(t.clone()),
            }
        }
        for v in [&mut g.working, &mut g.waiting, &mut g.settled] {
            v.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        }
        g
    }

    pub fn is_empty(&self) -> bool {
        self.working.is_empty() && self.waiting.is_empty() && self.settled.is_empty()
    }

    /// Threads needing attention right now (Working + Waiting), for badges.
    pub fn busy_count(&self) -> usize {
        self.working.len() + self.waiting.len()
    }
}

/// Group threads by project (most recently updated project first, threads newest first).
/// Comparison of project roots ignores case and slash style.
pub fn group_by_project(threads: &[ThreadRecord]) -> Vec<(PathBuf, Vec<ThreadRecord>)> {
    let mut sorted: Vec<&ThreadRecord> = threads.iter().collect();
    sorted.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
    let mut out: Vec<(String, PathBuf, Vec<ThreadRecord>)> = Vec::new();
    for t in sorted {
        let key = path_key(&t.project_root);
        match out.iter_mut().find(|(k, _, _)| *k == key) {
            Some((_, _, v)) => v.push(t.clone()),
            None => out.push((key, t.project_root.clone(), vec![t.clone()])),
        }
    }
    out.into_iter().map(|(_, p, v)| (p, v)).collect()
}

pub(crate) fn path_key(p: &Path) -> String {
    store::paths::normalize_key(&p.to_string_lossy())
}

// ---------------------------------------------------------------------------
// AgentKind / PermissionMode <-> text
// ---------------------------------------------------------------------------

pub(crate) fn agent_to_db(k: &AgentKind) -> String {
    match k {
        AgentKind::ClaudeCode => "claude".into(),
        AgentKind::Codex => "codex".into(),
        AgentKind::OpenCode => "opencode".into(),
        AgentKind::Acp { command } => format!("acp:{command}"),
    }
}

pub(crate) fn agent_from_db(s: &str) -> AgentKind {
    match s {
        "claude" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        "opencode" => AgentKind::OpenCode,
        other => match other.strip_prefix("acp:") {
            Some(cmd) => AgentKind::Acp {
                command: cmd.to_string(),
            },
            // Unknown future kind: keep it usable as a generic ACP command.
            None => AgentKind::Acp {
                command: other.to_string(),
            },
        },
    }
}

pub(crate) fn permission_to_db(p: PermissionMode) -> &'static str {
    match p {
        PermissionMode::Ask => "ask",
        PermissionMode::AcceptEdits => "accept_edits",
        PermissionMode::Full => "full",
        PermissionMode::ReadOnly => "read_only",
    }
}

pub(crate) fn permission_from_db(s: &str) -> PermissionMode {
    match s {
        "accept_edits" => PermissionMode::AcceptEdits,
        "full" => PermissionMode::Full,
        "read_only" => PermissionMode::ReadOnly,
        _ => PermissionMode::Ask,
    }
}

/// Human name of an agent (Spanish handoff text, UI fallbacks).
pub fn agent_label(k: &AgentKind) -> String {
    match k {
        AgentKind::ClaudeCode => "Claude Code".into(),
        AgentKind::Codex => "Codex".into(),
        AgentKind::OpenCode => "OpenCode".into(),
        AgentKind::Acp { command } => {
            let first = command.trim().trim_start_matches('"');
            let first = first.split('"').next().unwrap_or(first);
            let first = if command.trim_start().starts_with('"') {
                first
            } else {
                first.split(' ').next().unwrap_or(first)
            };
            let stem = Path::new(first)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("ACP");
            format!("{stem} (ACP)")
        }
    }
}

/// The permission a NEW thread starts with, from `[agents] permission_default`.
///
/// `Full` is never a starting point: «Acceso completo» skips every approval, so it has to be picked
/// in the thread itself (the permission menu of the composer). A configured `Full` starts new
/// threads in `AcceptEdits`, the most permissive mode that still asks before running commands.
/// Threads that already exist keep the permission they were created with.
pub fn permission_for_new_thread(default: store::config::PermissionDefault) -> PermissionMode {
    use store::config::PermissionDefault as D;
    match default {
        D::Ask => PermissionMode::Ask,
        D::AcceptEdits | D::Full => PermissionMode::AcceptEdits,
        D::ReadOnly => PermissionMode::ReadOnly,
    }
}

/// Whether a turn started in `mode` should be preceded by a git checkpoint (A10):
/// Full and AcceptEdits yes; ReadOnly (nothing can change) and Ask (every write is approved
/// one by one) no.
pub fn should_checkpoint(mode: PermissionMode) -> bool {
    matches!(mode, PermissionMode::Full | PermissionMode::AcceptEdits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_roundtrip() {
        for k in [
            AgentKind::ClaudeCode,
            AgentKind::Codex,
            AgentKind::OpenCode,
            AgentKind::Acp {
                command: "\"C:/x y/gemini.exe\" --acp".into(),
            },
        ] {
            assert_eq!(agent_from_db(&agent_to_db(&k)), k);
        }
        assert_eq!(
            agent_label(&AgentKind::Acp {
                command: "\"C:/x y/gemini.exe\" --acp".into()
            }),
            "gemini (ACP)"
        );
        assert_eq!(
            agent_label(&AgentKind::Acp {
                command: "gemini --acp".into()
            }),
            "gemini (ACP)"
        );
    }

    #[test]
    fn permission_roundtrip() {
        for p in [
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::Full,
            PermissionMode::ReadOnly,
        ] {
            assert_eq!(permission_from_db(permission_to_db(p)), p);
        }
    }

    #[test]
    fn status_groups() {
        assert_eq!(ThreadStatus::Working.group(), ThreadGroup::Working);
        assert_eq!(ThreadStatus::WaitingForInput.group(), ThreadGroup::Waiting);
        for s in [ThreadStatus::Idle, ThreadStatus::Done, ThreadStatus::Error] {
            assert_eq!(s.group(), ThreadGroup::Settled);
        }
        for s in [
            ThreadStatus::Idle,
            ThreadStatus::Working,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Done,
            ThreadStatus::Error,
        ] {
            assert_eq!(ThreadStatus::parse(s.as_str()), s);
        }
    }

    #[test]
    fn checkpoint_policy() {
        assert!(should_checkpoint(PermissionMode::Full));
        assert!(should_checkpoint(PermissionMode::AcceptEdits));
        assert!(!should_checkpoint(PermissionMode::ReadOnly));
        assert!(!should_checkpoint(PermissionMode::Ask));
    }

    #[test]
    fn a_new_thread_never_starts_in_full_access() {
        use store::config::PermissionDefault as D;
        assert_eq!(permission_for_new_thread(D::Ask), PermissionMode::Ask);
        assert_eq!(
            permission_for_new_thread(D::AcceptEdits),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            permission_for_new_thread(D::ReadOnly),
            PermissionMode::ReadOnly
        );
        assert_ne!(permission_for_new_thread(D::Full), PermissionMode::Full);
    }
}
