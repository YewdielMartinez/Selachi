//! `ThreadState`: the in-memory, UI-facing model of one thread, folded from the
//! [`AgentEvent`] stream. Pure (no I/O, no threads): [`ThreadState::apply`] takes an event and
//! returns what changed plus the records to persist, so it is trivially testable.
//!
//! Shape of a conversation:
//!
//! * A **user message** per `send` (text, attachments, checkpoint taken before the turn).
//! * One **assistant message per turn**. It opens with the first assistant-side event and is
//!   finalized (and persisted) on `Status(Done | Error)` or `Exited`. Its `blocks` are in
//!   arrival order: streamed `Text` (deltas appended to the open block), collapsed `Thinking`,
//!   tool rows and inline approval rows.
//! * Consecutive Read / Search / List tool calls are folded into one [`ExploreGroup`]
//!   ("Explored · 1 list · 2 searches · 1 file"). Thinking blocks between the calls do not break
//!   a group (they stay where they are, the group keeps growing above them); text, commands,
//!   edits and approvals do.
//! * **System messages** record local events (agent switched, errors that were not agent output).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agents_driver::{AgentEvent, AgentKind, AgentStatus, RateWindow, TaskState, ToolKind};
use agents_svc::{Agent as PriceAgent, PricingTable, TokenCounts};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::{ApprovalDecision, CostSource, Role, ThreadId, ThreadRecord, ThreadStatus};

/// Tool inputs are capped at this many bytes (serialized) before they enter the model.
pub const MAX_TOOL_INPUT_BYTES: usize = 4 * 1024;

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

/// Display class of a tool call, finer than [`ToolKind`] (it knows "list").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolClass {
    List,
    Search,
    /// A file read.
    File,
    Edit,
    Command,
    Web,
    Task,
    Other,
}

impl ToolClass {
    /// Part of an "Explored" group.
    pub fn is_explore(self) -> bool {
        matches!(self, Self::List | Self::Search | Self::File)
    }
}

const LIST_NAMES: &[&str] = &[
    "ls",
    "list",
    "list_dir",
    "listdir",
    "list_directory",
    "list-directory",
    "list_files",
    "directory_list",
    "readdir",
];

fn is_list_name(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    LIST_NAMES.contains(&l.as_str())
        // MCP style `server/list_dir`
        || l.rsplit('/').next().is_some_and(|t| LIST_NAMES.contains(&t))
}

/// Classify a tool call. `title` is only consulted for generic ACP `other` calls (OpenCode names
/// its list tool in the title).
pub fn classify(kind: &ToolKind, title: &str) -> ToolClass {
    match kind {
        ToolKind::Read => ToolClass::File,
        ToolKind::Search => ToolClass::Search,
        ToolKind::Edit | ToolKind::Write => ToolClass::Edit,
        ToolKind::Command => ToolClass::Command,
        ToolKind::Web => ToolClass::Web,
        ToolKind::Task => ToolClass::Task,
        ToolKind::Other(name) => {
            if is_list_name(name) {
                return ToolClass::List;
            }
            if name.eq_ignore_ascii_case("other") {
                let first = title.split_whitespace().next().unwrap_or("");
                if is_list_name(first) {
                    return ToolClass::List;
                }
            }
            ToolClass::Other
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
    /// The turn ended before a result arrived (interrupted, crashed).
    Cancelled,
}

/// One tool call (a row in the conversation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRun {
    /// The agent's tool call id (unique within the thread session).
    pub id: String,
    pub kind: ToolKind,
    pub class: ToolClass,
    pub title: String,
    /// Truncated to [`MAX_TOOL_INPUT_BYTES`].
    pub input: Value,
    pub status: ToolStatus,
    /// Result summary (already shortened by the driver).
    pub summary: Option<String>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
}

impl ToolRun {
    /// Wall-clock duration once finished.
    pub fn duration_ms(&self) -> Option<i64> {
        self.finished_ms.map(|f| (f - self.started_ms).max(0))
    }

    pub fn is_running(&self) -> bool {
        self.status == ToolStatus::Running
    }
}

/// Counts per class of an [`ExploreGroup`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExploreCounts {
    pub lists: usize,
    pub searches: usize,
    pub files: usize,
}

/// Consecutive Read/Search/List calls, collapsed into one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ExploreGroup {
    pub runs: Vec<ToolRun>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl ExploreGroup {
    pub fn counts(&self) -> ExploreCounts {
        let mut c = ExploreCounts::default();
        for r in &self.runs {
            match r.class {
                ToolClass::List => c.lists += 1,
                ToolClass::Search => c.searches += 1,
                ToolClass::File => c.files += 1,
                _ => {}
            }
        }
        c
    }

    /// `"Explored · 1 list · 2 searches · 1 file"` (zero counts are omitted; order: lists,
    /// searches, files).
    pub fn label(&self) -> String {
        let c = self.counts();
        let mut parts = Vec::new();
        if c.lists > 0 {
            parts.push(plural(c.lists, "list", "lists"));
        }
        if c.searches > 0 {
            parts.push(plural(c.searches, "search", "searches"));
        }
        if c.files > 0 {
            parts.push(plural(c.files, "file", "files"));
        }
        if parts.is_empty() {
            "Explored".to_string()
        } else {
            format!("Explored · {}", parts.join(" · "))
        }
    }

    /// Any call still waiting for its result.
    pub fn is_running(&self) -> bool {
        self.runs.iter().any(ToolRun::is_running)
    }
}

fn default_true() -> bool {
    true
}

/// Reasoning text. `collapsed` is the default view state (always `true` from the model; the UI
/// keeps its own expand toggles).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub text: String,
    #[serde(default = "default_true")]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApprovalState {
    Pending,
    Allowed,
    Denied,
    AlwaysAllowed,
    /// The turn ended (or the agent died) before the user answered.
    Expired,
}

/// An approval request shown inline in the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRow {
    pub request_id: String,
    pub tool: String,
    pub detail: String,
    pub state: ApprovalState,
    pub received_ms: i64,
    pub decided_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Markdown text (streamed deltas are appended to the last open text block).
    Text {
        text: String,
    },
    Thinking(ThinkingBlock),
    Explored(ExploreGroup),
    /// A command row (status, duration).
    Command(ToolRun),
    /// Edit / Write / Web / Task / other tool row.
    Tool(ToolRun),
    Approval(ApprovalRow),
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Token usage summed over the usage events of a message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageUsage {
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
}

impl MessageUsage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }
}

/// Cost of one message.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MessageCost {
    pub usd: f64,
    /// `Reported` only when every part came from the agent; a mix degrades to `Estimated`.
    pub source: CostSource,
}

/// A git checkpoint taken right before a user turn (A10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRef {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Index in the thread (`ThreadState::messages[seq]`), also the persisted `seq`.
    pub seq: u32,
    pub role: Role,
    pub created_ms: i64,
    pub blocks: Vec<Block>,
    pub usage: Option<MessageUsage>,
    pub cost: Option<MessageCost>,
    /// User messages: files attached to the turn.
    pub attachments: Vec<PathBuf>,
    /// User messages: checkpoint taken just before this turn.
    pub checkpoint: Option<CheckpointRef>,
    /// Assistant messages: still being produced (not persisted yet).
    pub streaming: bool,
}

impl Message {
    /// Concatenated text blocks.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            if let Block::Text { text } = b {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(text);
            }
        }
        out
    }

    /// All tool runs of the message, in order.
    pub fn tool_runs(&self) -> impl Iterator<Item = &ToolRun> {
        self.blocks
            .iter()
            .flat_map(|b| -> Box<dyn Iterator<Item = &ToolRun> + '_> {
                match b {
                    Block::Explored(g) => Box::new(g.runs.iter()),
                    Block::Command(r) | Block::Tool(r) => Box::new(std::iter::once(r)),
                    _ => Box::new(std::iter::empty()),
                }
            })
    }
}

/// What is stored in `messages.content_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageContent {
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointRef>,
}

// ---------------------------------------------------------------------------
// Plan, usage, context
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub text: String,
    pub state: TaskState,
}

/// The agent's checklist ("0/4").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

impl Plan {
    /// `(done, total)`.
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .items
            .iter()
            .filter(|i| i.state == TaskState::Done)
            .count();
        (done, self.items.len())
    }

    /// `"2/4"`.
    pub fn label(&self) -> String {
        let (d, t) = self.progress();
        format!("{d}/{t}")
    }

    /// Items that are not done yet.
    pub fn open_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|i| i.state != TaskState::Done)
    }
}

/// Thread-level totals.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    /// Sum of agent-reported costs (exact).
    pub cost_reported_usd: f64,
    /// Sum of price-table estimates.
    pub cost_estimated_usd: f64,
}

impl UsageTotals {
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }

    pub fn cost_usd(&self) -> f64 {
        self.cost_reported_usd + self.cost_estimated_usd
    }

    /// Every cent in the total came from the agent itself.
    pub fn cost_is_exact(&self) -> bool {
        self.cost_estimated_usd == 0.0
    }
}

/// Context window occupancy of the last turn (approximate: prompt + output tokens of the last
/// usage event over the window the agent reported).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub window_tokens: u64,
    /// 0.0 ..= 100.0
    pub percent: f32,
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

/// What changed in a state transition (OR-ed together when updates are coalesced).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changed {
    pub messages: bool,
    pub status: bool,
    pub plan: bool,
    pub approvals: bool,
    pub usage: bool,
    pub rate_limits: bool,
    /// Thread list metadata (title, archived, created, deleted).
    pub list: bool,
}

impl Changed {
    pub fn any(&self) -> bool {
        self.messages
            || self.status
            || self.plan
            || self.approvals
            || self.usage
            || self.rate_limits
            || self.list
    }

    pub fn merge(&mut self, o: Changed) {
        self.messages |= o.messages;
        self.status |= o.status;
        self.plan |= o.plan;
        self.approvals |= o.approvals;
        self.usage |= o.usage;
        self.rate_limits |= o.rate_limits;
        self.list |= o.list;
    }
}

/// One `tool_runs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRunWrite {
    pub tool_id: String,
    pub kind: String,
    pub title: String,
    pub input_json: String,
    pub result_summary: Option<String>,
    pub ok: Option<bool>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
}

/// One `messages` row (+ its `tool_runs`). Already redacted.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageWrite {
    pub seq: u32,
    pub role: Role,
    pub created_ms: i64,
    pub content_json: String,
    pub usage_json: Option<String>,
    pub cost: Option<MessageCost>,
    pub tool_runs: Vec<ToolRunWrite>,
}

/// One `approvals` row (insert when `decision` is `None`, update when it is set).
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalWrite {
    pub request_id: String,
    pub tool: String,
    pub detail: String,
    pub decision: Option<String>,
    pub decided_ms: Option<i64>,
}

/// A record the runtime must persist (batched by the writer thread).
#[derive(Debug, Clone, PartialEq)]
pub enum Persist {
    Message(MessageWrite),
    Plan(String),
    Approval(ApprovalWrite),
    Status(ThreadStatus),
    Session(String),
    Title(String),
}

/// Result of a transition.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    pub changed: Changed,
    pub persist: Vec<Persist>,
}

impl Effects {
    pub fn merge(&mut self, o: Effects) {
        self.changed.merge(o.changed);
        self.persist.extend(o.persist);
    }
}

// ---------------------------------------------------------------------------
// Cost policy
// ---------------------------------------------------------------------------

/// How agent-reported costs are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostPolicy {
    /// Claude Code's `total_cost_usd` is the running total of the **process**: each `Usage`
    /// carries the total so far, so the per-message cost is the difference with the previous
    /// total (the baseline resets whenever the process is restarted). ACP drivers already emit
    /// increments. Set to `false` if a CLI version turns out to report per-turn costs.
    pub claude_cumulative: bool,
}

impl Default for CostPolicy {
    fn default() -> Self {
        Self {
            claude_cumulative: true,
        }
    }
}

// ---------------------------------------------------------------------------
// ThreadState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Text,
    Thinking,
}

#[derive(Debug, Clone, Copy)]
struct ToolLoc {
    msg: usize,
    block: usize,
    /// Index inside an [`ExploreGroup`].
    run: Option<usize>,
}

/// The folded state of one thread. Read it through [`crate::ThreadRuntime::with_state`].
#[derive(Debug, Clone)]
pub struct ThreadState {
    pub thread_id: ThreadId,
    pub status: ThreadStatus,
    pub messages: Vec<Message>,
    pub plan: Option<Plan>,
    pub usage: UsageTotals,
    pub context: Option<ContextUsage>,
    /// Latest windows pushed by the CLI, merged by name.
    pub rate_limits: Vec<RateWindow>,
    pub session_id: Option<String>,
    /// Model reported by the agent (`SessionStarted`), else the thread's model.
    pub model: Option<String>,
    /// Message of the last `Status(Error)` (cleared when a new turn starts).
    pub error: Option<String>,
    /// Bumped on every transition that changed something.
    pub revision: u64,

    agent: AgentKind,
    pricing: Arc<PricingTable>,
    policy: CostPolicy,
    tool_index: HashMap<String, ToolLoc>,
    open: Option<usize>,
    stream: Option<Stream>,
    claude_last_total: f64,
    context_window: Option<u64>,
    title_set: bool,
}

impl ThreadState {
    pub fn new(
        thread_id: ThreadId,
        agent: AgentKind,
        model: Option<String>,
        pricing: Arc<PricingTable>,
        policy: CostPolicy,
    ) -> Self {
        Self {
            thread_id,
            status: ThreadStatus::Idle,
            messages: Vec::new(),
            plan: None,
            usage: UsageTotals::default(),
            context: None,
            rate_limits: Vec::new(),
            session_id: None,
            model,
            error: None,
            revision: 0,
            agent,
            pricing,
            policy,
            tool_index: HashMap::new(),
            open: None,
            stream: None,
            claude_last_total: 0.0,
            context_window: None,
            title_set: false,
        }
    }

    /// Rebuild from persisted rows (a thread reopened after a restart). Busy statuses become
    /// `Idle`: nothing is running any more.
    pub fn from_stored(
        record: &ThreadRecord,
        messages: Vec<Message>,
        plan: Option<Plan>,
        pricing: Arc<PricingTable>,
        policy: CostPolicy,
    ) -> Self {
        let status = record.status;
        let title_set =
            !record.title.trim().is_empty() || messages.iter().any(|m| m.role == Role::User);
        let mut st = Self::new(
            record.id,
            record.agent.clone(),
            record.model.clone(),
            pricing,
            policy,
        );
        st.status = if status.is_busy() {
            ThreadStatus::Idle
        } else {
            status
        };
        st.session_id = record.agent_session_id.clone();
        st.title_set = title_set;
        st.plan = plan;
        for m in &messages {
            if let Some(u) = m.usage {
                st.usage.input += u.input;
                st.usage.output += u.output;
                st.usage.cache_create += u.cache_create;
                st.usage.cache_read += u.cache_read;
            }
            if let Some(c) = m.cost {
                match c.source {
                    CostSource::Reported => st.usage.cost_reported_usd += c.usd,
                    CostSource::Estimated => st.usage.cost_estimated_usd += c.usd,
                }
            }
        }
        st.messages = messages;
        st
    }

    pub fn agent(&self) -> &AgentKind {
        &self.agent
    }

    /// The thread now talks to another agent / model: pricing follows, and the Claude running
    /// total restarts (a new process starts at zero).
    pub fn set_agent(&mut self, agent: AgentKind, model: Option<String>, keep_session: bool) {
        self.agent = agent;
        self.model = model;
        self.claude_last_total = 0.0;
        self.context = None;
        self.context_window = None;
        if !keep_session {
            self.session_id = None;
        }
    }

    /// Record the checkpoint taken before the user message `seq` (and persist the message again).
    pub fn attach_checkpoint(&mut self, seq: u32, cp: CheckpointRef) -> Effects {
        let mut fx = Effects::default();
        if let Some(m) = self.messages.get_mut(seq as usize) {
            m.checkpoint = Some(cp);
            fx.changed.messages = true;
            fx.persist
                .push(Persist::Message(self.write_of(seq as usize)));
        }
        self.bump(&fx);
        fx
    }

    /// A new agent process was started for this thread (resets cumulative cost baselines).
    pub fn driver_restarted(&mut self) {
        self.claude_last_total = 0.0;
    }

    /// The thread has a title now (do not derive one from the first message).
    pub fn mark_titled(&mut self) {
        self.title_set = true;
    }

    /// Number of user turns so far.
    pub fn turn_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count()
    }

    /// Approval rows waiting for an answer (only the open turn can have any).
    pub fn pending_approvals(&self) -> Vec<&ApprovalRow> {
        let Some(i) = self.open else {
            return Vec::new();
        };
        self.messages[i]
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Approval(a) if a.state == ApprovalState::Pending => Some(a),
                _ => None,
            })
            .collect()
    }

    /// The assistant message being produced, if any.
    pub fn open_message(&self) -> Option<&Message> {
        self.open.map(|i| &self.messages[i])
    }

    pub fn last_assistant(&self) -> Option<&Message> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
    }

    fn bump(&mut self, e: &Effects) {
        if e.changed.any() {
            self.revision += 1;
        }
    }

    // ----- user side -------------------------------------------------------

    /// Record a user message (a turn is about to start). Marks the thread `Working`.
    pub fn begin_user_message(
        &mut self,
        text: &str,
        attachments: Vec<PathBuf>,
        checkpoint: Option<CheckpointRef>,
        now_ms: i64,
    ) -> Effects {
        let mut fx = self.finalize_open(now_ms);
        self.error = None;
        let seq = self.messages.len() as u32;
        self.messages.push(Message {
            seq,
            role: Role::User,
            created_ms: now_ms,
            blocks: vec![Block::Text {
                text: text.to_string(),
            }],
            usage: None,
            cost: None,
            attachments,
            checkpoint,
            streaming: false,
        });
        fx.changed.messages = true;
        fx.persist
            .push(Persist::Message(self.write_of(self.messages.len() - 1)));
        if !self.title_set {
            self.title_set = true;
            let title = auto_title(text);
            if !title.is_empty() {
                fx.persist.push(Persist::Title(title));
                fx.changed.list = true;
            }
        }
        if self.status != ThreadStatus::Working {
            self.status = ThreadStatus::Working;
            fx.changed.status = true;
            fx.persist.push(Persist::Status(ThreadStatus::Working));
        }
        self.bump(&fx);
        fx
    }

    /// Record a local system note (agent switched, start failure...).
    pub fn add_system_message(&mut self, text: &str, now_ms: i64) -> Effects {
        let mut fx = self.finalize_open(now_ms);
        let seq = self.messages.len() as u32;
        self.messages.push(Message {
            seq,
            role: Role::System,
            created_ms: now_ms,
            blocks: vec![Block::Text {
                text: text.to_string(),
            }],
            usage: None,
            cost: None,
            attachments: Vec::new(),
            checkpoint: None,
            streaming: false,
        });
        fx.changed.messages = true;
        fx.persist
            .push(Persist::Message(self.write_of(self.messages.len() - 1)));
        self.bump(&fx);
        fx
    }

    /// Force the thread status (start failure, switch...). Persists when it changes.
    pub fn set_status(&mut self, status: ThreadStatus, error: Option<String>) -> Effects {
        let mut fx = Effects::default();
        if let Some(e) = error {
            self.error = Some(e);
        }
        if self.status != status {
            self.status = status;
            fx.changed.status = true;
            fx.persist.push(Persist::Status(status));
        }
        self.bump(&fx);
        fx
    }

    /// The user answered an approval. `None` when the request is unknown (already answered or
    /// expired).
    pub fn resolve_approval(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
        now_ms: i64,
    ) -> Option<Effects> {
        let i = self.open?;
        let row = self.messages[i].blocks.iter_mut().find_map(|b| match b {
            Block::Approval(a)
                if a.request_id == request_id && a.state == ApprovalState::Pending =>
            {
                Some(a)
            }
            _ => None,
        })?;
        row.state = match decision {
            ApprovalDecision::Allow => ApprovalState::Allowed,
            ApprovalDecision::Deny => ApprovalState::Denied,
            ApprovalDecision::Always => ApprovalState::AlwaysAllowed,
        };
        row.decided_ms = Some(now_ms);
        let write = ApprovalWrite {
            request_id: row.request_id.clone(),
            tool: row.tool.clone(),
            detail: redact_text(&row.detail),
            decision: Some(decision.as_str().to_string()),
            decided_ms: Some(now_ms),
        };
        let mut fx = Effects::default();
        fx.changed.approvals = true;
        fx.changed.messages = true;
        fx.persist.push(Persist::Approval(write));
        if self.status == ThreadStatus::WaitingForInput && self.pending_approvals().is_empty() {
            self.status = ThreadStatus::Working;
            fx.changed.status = true;
            fx.persist.push(Persist::Status(ThreadStatus::Working));
        }
        self.bump(&fx);
        Some(fx)
    }

    // ----- agent side ------------------------------------------------------

    /// Fold one agent event.
    pub fn apply(&mut self, ev: AgentEvent, now_ms: i64) -> Effects {
        let mut fx = Effects::default();
        match ev {
            AgentEvent::SessionStarted { session_id, model } => {
                if self.session_id.as_deref() != Some(&session_id) {
                    self.session_id = Some(session_id.clone());
                    fx.persist.push(Persist::Session(session_id));
                }
                if model.is_some() {
                    self.model = model;
                }
                fx.changed.status = true;
            }
            AgentEvent::Text { text, delta } => {
                let i = self.ensure_open(now_ms);
                let blocks = &mut self.messages[i].blocks;
                if delta && self.stream == Some(Stream::Text) {
                    if let Some(Block::Text { text: t }) = blocks.last_mut() {
                        t.push_str(&text);
                    } else {
                        blocks.push(Block::Text { text });
                    }
                } else {
                    blocks.push(Block::Text { text });
                }
                self.stream = if delta { Some(Stream::Text) } else { None };
                fx.changed.messages = true;
            }
            AgentEvent::Thinking { text, delta } => {
                let i = self.ensure_open(now_ms);
                let blocks = &mut self.messages[i].blocks;
                if delta && self.stream == Some(Stream::Thinking) {
                    if let Some(Block::Thinking(t)) = blocks.last_mut() {
                        t.text.push_str(&text);
                    } else {
                        blocks.push(Block::Thinking(ThinkingBlock {
                            text,
                            collapsed: true,
                        }));
                    }
                } else {
                    blocks.push(Block::Thinking(ThinkingBlock {
                        text,
                        collapsed: true,
                    }));
                }
                self.stream = if delta { Some(Stream::Thinking) } else { None };
                fx.changed.messages = true;
            }
            // The checklist is shown as the plan (`AgentEvent::Plan`), not as a tool row.
            AgentEvent::ToolCall {
                kind: ToolKind::Other(ref n),
                ..
            } if n == "TodoWrite" => {}
            AgentEvent::ToolCall {
                id,
                kind,
                title,
                input_json,
            } => {
                self.on_tool_call(id, kind, title, input_json, now_ms);
                fx.changed.messages = true;
            }
            AgentEvent::ToolResult { id, ok, summary } => {
                if let Some(run) = self.run_mut(&id) {
                    run.status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Failed
                    };
                    run.summary = Some(summary);
                    run.finished_ms = Some(now_ms);
                    fx.changed.messages = true;
                } else {
                    log::debug!("chat_core: result for unknown tool call {id}");
                }
            }
            AgentEvent::Plan { tasks } => {
                let plan = Plan {
                    items: tasks
                        .into_iter()
                        .map(|(text, state)| PlanItem { text, state })
                        .collect(),
                };
                if self.plan.as_ref() != Some(&plan) {
                    let json = serde_json::to_string(&Plan {
                        items: plan
                            .items
                            .iter()
                            .map(|i| PlanItem {
                                text: redact_text(&i.text),
                                state: i.state,
                            })
                            .collect(),
                    })
                    .unwrap_or_else(|_| "{\"items\":[]}".into());
                    self.plan = Some(plan);
                    fx.persist.push(Persist::Plan(json));
                    fx.changed.plan = true;
                }
            }
            AgentEvent::ApprovalRequest { id, tool, detail } => {
                let i = self.ensure_open(now_ms);
                self.stream = None;
                let detail = truncate_chars(&detail, 2000);
                fx.persist.push(Persist::Approval(ApprovalWrite {
                    request_id: id.clone(),
                    tool: tool.clone(),
                    detail: redact_text(&detail),
                    decision: None,
                    decided_ms: None,
                }));
                self.messages[i].blocks.push(Block::Approval(ApprovalRow {
                    request_id: id,
                    tool,
                    detail,
                    state: ApprovalState::Pending,
                    received_ms: now_ms,
                    decided_ms: None,
                }));
                fx.changed.messages = true;
                fx.changed.approvals = true;
            }
            AgentEvent::Usage {
                input,
                output,
                cache_create,
                cache_read,
                cost_usd,
                context_window,
            } => {
                self.on_usage(
                    MessageUsage {
                        input,
                        output,
                        cache_create,
                        cache_read,
                    },
                    cost_usd,
                    context_window,
                    now_ms,
                    &mut fx,
                );
            }
            AgentEvent::RateLimits { windows } => {
                for w in windows {
                    match self.rate_limits.iter_mut().find(|x| x.name == w.name) {
                        Some(slot) => *slot = w,
                        None => self.rate_limits.push(w),
                    }
                }
                fx.changed.rate_limits = true;
            }
            AgentEvent::Status(s) => match s {
                AgentStatus::Working => {
                    self.error = None;
                    fx.merge(self.set_status(ThreadStatus::Working, None));
                }
                AgentStatus::WaitingForInput => {
                    fx.merge(self.set_status(ThreadStatus::WaitingForInput, None))
                }
                AgentStatus::Done => {
                    fx.merge(self.finalize_open(now_ms));
                    fx.merge(self.set_status(ThreadStatus::Done, None));
                }
                AgentStatus::Error(msg) => {
                    fx.merge(self.finalize_open(now_ms));
                    fx.merge(self.set_status(ThreadStatus::Error, Some(msg)));
                }
            },
            // The model catalogue is for the picker, not for the transcript.
            AgentEvent::Models { .. } => {}
            AgentEvent::Exited { code } => {
                fx.merge(self.finalize_open(now_ms));
                if self.status.is_busy() {
                    let msg = match code {
                        Some(c) => format!("El agente terminó inesperadamente (código {c})"),
                        None => "El agente terminó inesperadamente".to_string(),
                    };
                    fx.merge(self.set_status(ThreadStatus::Error, Some(msg)));
                }
            }
        }
        self.bump(&fx);
        fx
    }

    /// Close the open assistant message (turn end / close / driver exit): running tools become
    /// `Cancelled`, pending approvals `Expired`, and the message is queued for persistence.
    pub fn finalize_open(&mut self, now_ms: i64) -> Effects {
        let mut fx = Effects::default();
        self.stream = None;
        let Some(i) = self.open.take() else { return fx };
        let m = &mut self.messages[i];
        m.streaming = false;
        for b in &mut m.blocks {
            match b {
                Block::Explored(g) => {
                    for r in &mut g.runs {
                        cancel_run(r, now_ms);
                    }
                }
                Block::Command(r) | Block::Tool(r) => cancel_run(r, now_ms),
                Block::Approval(a) if a.state == ApprovalState::Pending => {
                    a.state = ApprovalState::Expired;
                    a.decided_ms = Some(now_ms);
                    fx.persist.push(Persist::Approval(ApprovalWrite {
                        request_id: a.request_id.clone(),
                        tool: a.tool.clone(),
                        detail: redact_text(&a.detail),
                        decision: Some("expired".into()),
                        decided_ms: Some(now_ms),
                    }));
                    fx.changed.approvals = true;
                }
                _ => {}
            }
        }
        let empty = m.blocks.is_empty() && m.usage.is_none();
        if empty {
            // Nothing was produced: drop the placeholder instead of persisting an empty bubble.
            if i + 1 == self.messages.len() {
                self.messages.pop();
            }
        } else {
            fx.persist.push(Persist::Message(self.write_of(i)));
        }
        fx.changed.messages = true;
        self.bump(&fx);
        fx
    }

    // ----- internals -------------------------------------------------------

    fn ensure_open(&mut self, now_ms: i64) -> usize {
        if let Some(i) = self.open {
            return i;
        }
        let seq = self.messages.len() as u32;
        self.messages.push(Message {
            seq,
            role: Role::Assistant,
            created_ms: now_ms,
            blocks: Vec::new(),
            usage: None,
            cost: None,
            attachments: Vec::new(),
            checkpoint: None,
            streaming: true,
        });
        let i = self.messages.len() - 1;
        self.open = Some(i);
        i
    }

    fn run_mut(&mut self, id: &str) -> Option<&mut ToolRun> {
        let loc = *self.tool_index.get(id)?;
        match self.messages.get_mut(loc.msg)?.blocks.get_mut(loc.block)? {
            Block::Explored(g) => g.runs.get_mut(loc.run?),
            Block::Command(r) | Block::Tool(r) => Some(r),
            _ => None,
        }
    }

    fn on_tool_call(
        &mut self,
        id: String,
        kind: ToolKind,
        title: String,
        input: Value,
        now_ms: i64,
    ) {
        let input = truncate_input(&input);
        if let Some(run) = self.run_mut(&id) {
            // A repeated announcement (ACP updates): refresh instead of duplicating.
            run.title = title;
            if !input.is_null() {
                run.input = input;
            }
            return;
        }
        let class = classify(&kind, &title);
        let run = ToolRun {
            id: id.clone(),
            kind,
            class,
            title,
            input,
            status: ToolStatus::Running,
            summary: None,
            started_ms: now_ms,
            finished_ms: None,
        };
        let mi = self.ensure_open(now_ms);
        self.stream = None;
        let blocks = &mut self.messages[mi].blocks;
        let loc = if class.is_explore() {
            // Join the last Explored group, looking through collapsed thinking blocks.
            let target = blocks
                .iter_mut()
                .enumerate()
                .rev()
                .find(|(_, b)| !matches!(b, Block::Thinking(_)))
                .and_then(|(bi, b)| match b {
                    Block::Explored(g) => Some((bi, g)),
                    _ => None,
                });
            match target {
                Some((bi, g)) => {
                    g.runs.push(run);
                    ToolLoc {
                        msg: mi,
                        block: bi,
                        run: Some(g.runs.len() - 1),
                    }
                }
                None => {
                    blocks.push(Block::Explored(ExploreGroup { runs: vec![run] }));
                    ToolLoc {
                        msg: mi,
                        block: blocks.len() - 1,
                        run: Some(0),
                    }
                }
            }
        } else {
            blocks.push(if class == ToolClass::Command {
                Block::Command(run)
            } else {
                Block::Tool(run)
            });
            ToolLoc {
                msg: mi,
                block: blocks.len() - 1,
                run: None,
            }
        };
        self.tool_index.insert(id, loc);
    }

    fn on_usage(
        &mut self,
        u: MessageUsage,
        reported: Option<f64>,
        window: Option<u64>,
        now_ms: i64,
        fx: &mut Effects,
    ) {
        // Which message gets the usage: the open turn, else the last assistant message.
        let (idx, was_open) = match self.open {
            Some(i) => (i, true),
            None => match self
                .messages
                .iter()
                .rposition(|m| m.role == Role::Assistant)
            {
                Some(i) => (i, false),
                None => (self.ensure_open(now_ms), true),
            },
        };
        let cost = self.cost_of(u, reported);
        let m = &mut self.messages[idx];
        let mu = m.usage.get_or_insert_with(MessageUsage::default);
        mu.input += u.input;
        mu.output += u.output;
        mu.cache_create += u.cache_create;
        mu.cache_read += u.cache_read;
        if let Some(c) = cost {
            m.cost = Some(match m.cost {
                Some(prev) => MessageCost {
                    usd: prev.usd + c.usd,
                    source: if prev.source == CostSource::Reported
                        && c.source == CostSource::Reported
                    {
                        CostSource::Reported
                    } else {
                        CostSource::Estimated
                    },
                },
                None => c,
            });
        }
        self.usage.input += u.input;
        self.usage.output += u.output;
        self.usage.cache_create += u.cache_create;
        self.usage.cache_read += u.cache_read;
        if let Some(c) = cost {
            match c.source {
                CostSource::Reported => self.usage.cost_reported_usd += c.usd,
                CostSource::Estimated => self.usage.cost_estimated_usd += c.usd,
            }
        }
        if window.is_some() {
            self.context_window = window;
        }
        if let Some(w) = self.context_window.filter(|w| *w > 0) {
            let used = u.input + u.cache_create + u.cache_read + u.output;
            self.context = Some(ContextUsage {
                used_tokens: used,
                window_tokens: w,
                percent: ((used as f64 / w as f64) * 100.0).clamp(0.0, 100.0) as f32,
            });
        }
        fx.changed.usage = true;
        fx.changed.messages = true;
        if !was_open {
            fx.persist.push(Persist::Message(self.write_of(idx)));
        }
    }

    /// Cost of one usage event: the agent's figure when it gave one, else the price table.
    fn cost_of(&mut self, u: MessageUsage, reported: Option<f64>) -> Option<MessageCost> {
        if let Some(total) = reported {
            let usd =
                if matches!(self.agent, AgentKind::ClaudeCode) && self.policy.claude_cumulative {
                    let d = if total >= self.claude_last_total {
                        total - self.claude_last_total
                    } else {
                        total
                    };
                    self.claude_last_total = total;
                    d
                } else {
                    total
                };
            return Some(MessageCost {
                usd,
                source: CostSource::Reported,
            });
        }
        let agent = match self.agent {
            AgentKind::ClaudeCode => PriceAgent::Claude,
            AgentKind::Codex => PriceAgent::Codex,
            AgentKind::OpenCode => PriceAgent::OpenCode,
            AgentKind::Acp { .. } => return None,
        };
        let model = self.model.as_deref()?;
        let tokens = TokenCounts {
            input: u.input,
            output: u.output,
            cache_create: u.cache_create,
            cache_create_1h: 0,
            cache_read: u.cache_read,
        };
        let usd = self.pricing.cost(agent, model, &tokens)?;
        Some(MessageCost {
            usd,
            source: CostSource::Estimated,
        })
    }

    /// Build the (redacted) persistence record of `messages[i]`.
    pub(crate) fn write_of(&self, i: usize) -> MessageWrite {
        message_write(&self.messages[i])
    }
}

fn cancel_run(r: &mut ToolRun, now_ms: i64) {
    if r.status == ToolStatus::Running {
        r.status = ToolStatus::Cancelled;
        r.finished_ms = Some(now_ms);
    }
}

/// Build the (redacted) persistence record of a message.
pub fn message_write(m: &Message) -> MessageWrite {
    let blocks: Vec<Block> = m.blocks.iter().map(redacted_block).collect();
    let mut tool_runs = Vec::new();
    for b in &blocks {
        let runs: Vec<&ToolRun> = match b {
            Block::Explored(g) => g.runs.iter().collect(),
            Block::Command(r) | Block::Tool(r) => vec![r],
            _ => continue,
        };
        for r in runs {
            tool_runs.push(ToolRunWrite {
                tool_id: r.id.clone(),
                kind: tool_kind_str(&r.kind),
                title: r.title.clone(),
                input_json: r.input.to_string(),
                result_summary: r.summary.clone(),
                ok: match r.status {
                    ToolStatus::Ok => Some(true),
                    ToolStatus::Failed => Some(false),
                    ToolStatus::Running | ToolStatus::Cancelled => None,
                },
                started_ms: r.started_ms,
                finished_ms: r.finished_ms,
            });
        }
    }
    let content = MessageContent {
        blocks,
        attachments: m.attachments.clone(),
        checkpoint: m.checkpoint.clone(),
    };
    MessageWrite {
        seq: m.seq,
        role: m.role,
        created_ms: m.created_ms,
        content_json: serde_json::to_string(&content).unwrap_or_else(|_| "{\"blocks\":[]}".into()),
        usage_json: m.usage.and_then(|u| serde_json::to_string(&u).ok()),
        cost: m.cost,
        tool_runs,
    }
}

pub(crate) fn tool_kind_str(k: &ToolKind) -> String {
    match k {
        ToolKind::Read => "read".into(),
        ToolKind::Search => "search".into(),
        ToolKind::Edit => "edit".into(),
        ToolKind::Write => "write".into(),
        ToolKind::Command => "command".into(),
        ToolKind::Web => "web".into(),
        ToolKind::Task => "task".into(),
        ToolKind::Other(n) => format!("other:{n}"),
    }
}

// ---------------------------------------------------------------------------
// Redaction / truncation helpers
// ---------------------------------------------------------------------------

pub(crate) fn redact_text(s: &str) -> String {
    store::redact(s).text
}

fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact_text(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), redact_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn redacted_run(r: &ToolRun) -> ToolRun {
    ToolRun {
        title: redact_text(&r.title),
        input: redact_value(&r.input),
        summary: r.summary.as_deref().map(redact_text),
        ..r.clone()
    }
}

fn redacted_block(b: &Block) -> Block {
    match b {
        Block::Text { text } => Block::Text {
            text: redact_text(text),
        },
        Block::Thinking(t) => Block::Thinking(ThinkingBlock {
            text: redact_text(&t.text),
            collapsed: t.collapsed,
        }),
        Block::Explored(g) => Block::Explored(ExploreGroup {
            runs: g.runs.iter().map(redacted_run).collect(),
        }),
        Block::Command(r) => Block::Command(redacted_run(r)),
        Block::Tool(r) => Block::Tool(redacted_run(r)),
        Block::Approval(a) => Block::Approval(ApprovalRow {
            detail: redact_text(&a.detail),
            ..a.clone()
        }),
    }
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn shrink(v: &Value, cap: usize) -> Value {
    match v {
        Value::String(s) if s.chars().count() > cap => Value::String(truncate_chars(s, cap)),
        Value::Array(a) => Value::Array(a.iter().map(|x| shrink(x, cap)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), shrink(x, cap))).collect())
        }
        other => other.clone(),
    }
}

/// Cap a tool input at [`MAX_TOOL_INPUT_BYTES`] serialized: long string leaves are shortened
/// first (keeping the structure, e.g. `file_path` intact); if that is not enough the value is
/// replaced by `{"_truncated":true,"preview":...}`.
pub fn truncate_input(v: &Value) -> Value {
    if v.to_string().len() <= MAX_TOOL_INPUT_BYTES {
        return v.clone();
    }
    for cap in [1024, 512, 256, 128] {
        let s = shrink(v, cap);
        if s.to_string().len() <= MAX_TOOL_INPUT_BYTES {
            return s;
        }
    }
    let raw = v.to_string();
    let mut end = (MAX_TOOL_INPUT_BYTES - 128).min(raw.len());
    loop {
        while end > 0 && !raw.is_char_boundary(end) {
            end -= 1;
        }
        // JSON escaping (quotes) can grow the preview: shrink until the whole value fits.
        let out = json!({ "_truncated": true, "bytes": raw.len(), "preview": &raw[..end] });
        if end == 0 || out.to_string().len() <= MAX_TOOL_INPUT_BYTES {
            return out;
        }
        end = end * 3 / 4;
    }
}

/// Title from the first user message: first non-empty line, 60 chars.
pub(crate) fn auto_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    truncate_chars(line, 60)
}

// ---------------------------------------------------------------------------
// Decoding stored rows
// ---------------------------------------------------------------------------

/// Rebuild a [`Message`] from a stored row. Unparseable content degrades to a text block.
pub(crate) fn message_from_row(
    seq: u32,
    role: Role,
    created_ms: i64,
    content_json: &str,
    usage_json: Option<&str>,
    cost_usd: Option<f64>,
    cost_source: Option<&str>,
) -> Message {
    let content: MessageContent =
        serde_json::from_str(content_json).unwrap_or_else(|_| MessageContent {
            blocks: vec![Block::Text {
                text: content_json.to_string(),
            }],
            ..Default::default()
        });
    Message {
        seq,
        role,
        created_ms,
        blocks: content.blocks,
        usage: usage_json.and_then(|j| serde_json::from_str(j).ok()),
        cost: cost_usd.map(|usd| MessageCost {
            usd,
            source: cost_source
                .and_then(CostSource::parse)
                .unwrap_or(CostSource::Estimated),
        }),
        attachments: content.attachments,
        checkpoint: content.checkpoint,
        streaming: false,
    }
}

/// Paths an edit/write tool call touched, from its input (`file_path`, `path`, Codex
/// `changes[].path`...).
pub fn touched_paths(run: &ToolRun) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > 3 {
            return;
        }
        match v {
            Value::Object(o) => {
                for key in ["file_path", "filePath", "path", "notebook_path", "filename"] {
                    if let Some(Value::String(s)) = o.get(key) {
                        if !s.is_empty() {
                            out.push(s.clone());
                        }
                    }
                }
                for key in ["changes", "edits", "files"] {
                    if let Some(x) = o.get(key) {
                        walk(x, out, depth + 1);
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, out, depth + 1);
                }
            }
            _ => {}
        }
    }
    if !matches!(run.class, ToolClass::Edit) {
        return Vec::new();
    }
    let mut out = Vec::new();
    walk(&run.input, &mut out, 0);
    if out.is_empty() && !run.title.is_empty() && run.title != "file changes" {
        // Codex titles a file change with its comma-joined paths.
        out.extend(run.title.split(", ").map(str::to_string));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents_driver::TaskState;

    fn st(agent: AgentKind, model: Option<&str>) -> ThreadState {
        ThreadState::new(
            ThreadId(1),
            agent,
            model.map(str::to_string),
            Arc::new(PricingTable::embedded()),
            CostPolicy::default(),
        )
    }

    fn call(id: &str, kind: ToolKind, title: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            id: id.into(),
            kind,
            title: title.into(),
            input_json: json!({}),
        }
    }

    #[test]
    fn explore_group_label_and_thinking_does_not_break_it() {
        let mut s = st(AgentKind::ClaudeCode, None);
        s.apply(call("a", ToolKind::Other("LS".into()), "src"), 1);
        s.apply(
            AgentEvent::Thinking {
                text: "hm".into(),
                delta: false,
            },
            2,
        );
        s.apply(call("b", ToolKind::Search, "foo"), 3);
        s.apply(call("c", ToolKind::Search, "bar"), 4);
        s.apply(call("d", ToolKind::Read, "a.rs"), 5);
        let m = &s.messages[0];
        // one group + one thinking block
        assert_eq!(m.blocks.len(), 2);
        let Block::Explored(g) = &m.blocks[0] else {
            panic!()
        };
        assert_eq!(g.label(), "Explored · 1 list · 2 searches · 1 file");
        assert_eq!(g.runs.len(), 4);
    }

    #[test]
    fn text_breaks_groups_and_commands_are_rows() {
        let mut s = st(AgentKind::ClaudeCode, None);
        s.apply(call("a", ToolKind::Read, "a"), 1);
        s.apply(
            AgentEvent::Text {
                text: "x".into(),
                delta: false,
            },
            2,
        );
        s.apply(call("b", ToolKind::Read, "b"), 3);
        s.apply(call("c", ToolKind::Command, "ls"), 4);
        s.apply(
            AgentEvent::ToolResult {
                id: "c".into(),
                ok: false,
                summary: "boom".into(),
            },
            1004,
        );
        let kinds: Vec<&str> = s.messages[0]
            .blocks
            .iter()
            .map(|b| match b {
                Block::Explored(_) => "explored",
                Block::Text { .. } => "text",
                Block::Command(_) => "command",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["explored", "text", "explored", "command"]);
        let Block::Command(r) = &s.messages[0].blocks[3] else {
            panic!()
        };
        assert_eq!(
            (r.status, r.duration_ms()),
            (ToolStatus::Failed, Some(1000))
        );
    }

    #[test]
    fn list_detection_via_acp_other_title() {
        assert_eq!(
            classify(&ToolKind::Other("other".into()), "list /src"),
            ToolClass::List
        );
        assert_eq!(
            classify(&ToolKind::Other("other".into()), "frobnicate"),
            ToolClass::Other
        );
        assert_eq!(
            classify(&ToolKind::Other("fs/list_dir".into()), ""),
            ToolClass::List
        );
    }

    #[test]
    fn deltas_append_and_complete_text_is_a_new_block() {
        let mut s = st(AgentKind::ClaudeCode, None);
        for t in ["Hel", "lo"] {
            s.apply(
                AgentEvent::Text {
                    text: t.into(),
                    delta: true,
                },
                1,
            );
        }
        s.apply(
            AgentEvent::Text {
                text: "Done.".into(),
                delta: false,
            },
            2,
        );
        s.apply(
            AgentEvent::Text {
                text: "more".into(),
                delta: true,
            },
            3,
        );
        let texts: Vec<_> = s.messages[0]
            .blocks
            .iter()
            .map(|b| match b {
                Block::Text { text } => text.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(texts, ["Hello", "Done.", "more"]);
        assert!(s.messages[0].streaming);
    }

    #[test]
    fn finalize_persists_once_and_cancels_running_tools() {
        let mut s = st(AgentKind::ClaudeCode, None);
        let fx = s.apply(call("a", ToolKind::Command, "sleep 9"), 1);
        assert!(fx.persist.is_empty(), "nothing persisted while streaming");
        s.apply(
            AgentEvent::ApprovalRequest {
                id: "r".into(),
                tool: "Bash".into(),
                detail: "rm".into(),
            },
            2,
        );
        assert_eq!(s.pending_approvals().len(), 1);
        let fx = s.apply(AgentEvent::Status(AgentStatus::Done), 3);
        let writes = fx
            .persist
            .iter()
            .filter(|p| matches!(p, Persist::Message(_)))
            .count();
        assert_eq!(writes, 1);
        let Block::Command(r) = &s.messages[0].blocks[0] else {
            panic!()
        };
        assert_eq!(r.status, ToolStatus::Cancelled);
        assert!(s.pending_approvals().is_empty());
        let Block::Approval(a) = &s.messages[0].blocks[1] else {
            panic!()
        };
        assert_eq!(a.state, ApprovalState::Expired);
    }

    #[test]
    fn plan_progress() {
        let mut s = st(AgentKind::ClaudeCode, None);
        s.apply(
            AgentEvent::Plan {
                tasks: vec![
                    ("a".into(), TaskState::Pending),
                    ("b".into(), TaskState::Pending),
                ],
            },
            1,
        );
        assert_eq!(s.plan.as_ref().unwrap().label(), "0/2");
        s.apply(
            AgentEvent::Plan {
                tasks: vec![
                    ("a".into(), TaskState::Done),
                    ("b".into(), TaskState::InProgress),
                ],
            },
            2,
        );
        assert_eq!(s.plan.as_ref().unwrap().progress(), (1, 2));
        assert_eq!(s.plan.as_ref().unwrap().open_items().count(), 1);
    }

    #[test]
    fn claude_cost_is_cumulative_per_process() {
        let mut s = st(AgentKind::ClaudeCode, Some("claude-sonnet-5-5"));
        let usage = |c| AgentEvent::Usage {
            input: 1,
            output: 1,
            cache_create: 0,
            cache_read: 0,
            cost_usd: Some(c),
            context_window: Some(1000),
        };
        let text = || AgentEvent::Text {
            text: "x".into(),
            delta: false,
        };
        s.apply(text(), 0);
        s.apply(usage(0.2), 1);
        s.apply(AgentEvent::Status(AgentStatus::Done), 2);
        s.apply(text(), 2);
        s.apply(usage(0.5), 3);
        s.apply(AgentEvent::Status(AgentStatus::Done), 4);
        let costs: Vec<f64> = s
            .messages
            .iter()
            .filter_map(|m| m.cost)
            .map(|c| c.usd)
            .collect();
        assert_eq!(costs.len(), 2);
        assert!((costs[0] - 0.2).abs() < 1e-9 && (costs[1] - 0.3).abs() < 1e-9);
        assert!((s.usage.cost_usd() - 0.5).abs() < 1e-9 && s.usage.cost_is_exact());
        // process restart: baseline back to zero
        s.driver_restarted();
        s.apply(text(), 4);
        s.apply(usage(0.1), 5);
        assert!((s.usage.cost_usd() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn estimated_cost_when_not_reported_and_none_for_unknown_models() {
        let mut s = st(AgentKind::ClaudeCode, Some("claude-opus-5-5"));
        s.apply(
            AgentEvent::Usage {
                input: 1_000_000,
                output: 0,
                cache_create: 0,
                cache_read: 0,
                cost_usd: None,
                context_window: None,
            },
            1,
        );
        let c = s.messages[0].cost.unwrap();
        assert_eq!(c.source, CostSource::Estimated);
        assert!(c.usd > 0.0);
        let mut s = st(
            AgentKind::Acp {
                command: "x".into(),
            },
            Some("whatever"),
        );
        s.apply(
            AgentEvent::Usage {
                input: 10,
                output: 0,
                cache_create: 0,
                cache_read: 0,
                cost_usd: None,
                context_window: None,
            },
            1,
        );
        assert!(s.messages[0].cost.is_none());
        assert_eq!(s.usage.tokens(), 10);
    }

    #[test]
    fn context_percent_needs_a_window() {
        let mut s = st(AgentKind::Codex, None);
        s.apply(
            AgentEvent::Usage {
                input: 100,
                output: 0,
                cache_create: 0,
                cache_read: 0,
                cost_usd: None,
                context_window: None,
            },
            1,
        );
        assert!(s.context.is_none());
        s.apply(
            AgentEvent::Usage {
                input: 200,
                output: 50,
                cache_create: 0,
                cache_read: 50,
                cost_usd: None,
                context_window: Some(1000),
            },
            2,
        );
        let c = s.context.unwrap();
        assert_eq!(c.used_tokens, 300);
        assert!((c.percent - 30.0).abs() < 1e-3);
    }

    #[test]
    fn tool_input_is_capped_at_4kb() {
        let big = json!({ "file_path": "a.rs", "content": "x".repeat(50_000) });
        let t = truncate_input(&big);
        assert!(t.to_string().len() <= MAX_TOOL_INPUT_BYTES);
        assert_eq!(t["file_path"], "a.rs");
        let many: Vec<String> = (0..2000).map(|i| format!("item-{i}")).collect();
        let t = truncate_input(&json!(many));
        assert!(t.to_string().len() <= MAX_TOOL_INPUT_BYTES);
        assert_eq!(t["_truncated"], true);
    }

    #[test]
    fn exited_mid_turn_is_an_error_and_persists_partial_message() {
        let mut s = st(AgentKind::ClaudeCode, None);
        s.apply(AgentEvent::Status(AgentStatus::Working), 1);
        s.apply(
            AgentEvent::Text {
                text: "partial".into(),
                delta: true,
            },
            2,
        );
        let fx = s.apply(AgentEvent::Exited { code: Some(1) }, 3);
        assert_eq!(s.status, ThreadStatus::Error);
        assert!(s.error.as_deref().unwrap().contains("código 1"));
        assert!(fx.persist.iter().any(|p| matches!(p, Persist::Message(_))));
    }

    #[test]
    fn rate_limits_merge_by_name() {
        let mut s = st(AgentKind::ClaudeCode, None);
        let w = |n: &str, p: f64| RateWindow {
            name: n.into(),
            used_percent: p,
            window_mins: None,
            resets_at: None,
        };
        s.apply(
            AgentEvent::RateLimits {
                windows: vec![w("five_hour", 10.0), w("seven_day", 5.0)],
            },
            1,
        );
        s.apply(
            AgentEvent::RateLimits {
                windows: vec![w("five_hour", 20.0)],
            },
            2,
        );
        assert_eq!(s.rate_limits.len(), 2);
        assert_eq!(s.rate_limits[0].used_percent, 20.0);
    }

    #[test]
    fn secrets_are_redacted_in_persisted_records_but_not_in_memory() {
        let mut s = st(AgentKind::ClaudeCode, None);
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        s.apply(
            AgentEvent::ToolCall {
                id: "a".into(),
                kind: ToolKind::Command,
                title: format!("curl -H 'Authorization: Bearer {secret}'"),
                input_json: json!({ "command": format!("echo {secret}") }),
            },
            1,
        );
        let w = message_write(&s.messages[0]);
        assert!(!w.content_json.contains(secret));
        assert!(!w.tool_runs[0].input_json.contains(secret));
        let Block::Command(r) = &s.messages[0].blocks[0] else {
            panic!()
        };
        assert!(r.title.contains(secret));
    }

    #[test]
    fn touched_paths_from_claude_and_codex_shapes() {
        let mut r = ToolRun {
            id: "a".into(),
            kind: ToolKind::Edit,
            class: ToolClass::Edit,
            title: "a.rs".into(),
            input: json!({ "file_path": "src/a.rs", "old_string": "x" }),
            status: ToolStatus::Ok,
            summary: None,
            started_ms: 0,
            finished_ms: None,
        };
        assert_eq!(touched_paths(&r), ["src/a.rs"]);
        r.input = json!([{ "path": "b.rs", "kind": "update" }, { "path": "c.rs" }]);
        assert_eq!(touched_paths(&r), ["b.rs", "c.rs"]);
        r.input = Value::Null;
        r.title = "d.rs, e.rs".into();
        assert_eq!(touched_paths(&r), ["d.rs", "e.rs"]);
    }
}
