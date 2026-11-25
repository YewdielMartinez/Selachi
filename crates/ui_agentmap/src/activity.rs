//! The pure part of the live activity feed of the inspector: what a run is doing, built from two
//! sources and merged by time. No gpui, no I/O: everything is unit-tested.
//!
//! * **Source A, `chat.db` / the chat runtime** ([`chat_events`]): the child thread's messages as
//!   `chat_core` folds them: assistant text, command rows, edit rows, approval rows.
//! * **Source B, the harness events log** ([`log_events`] over [`crate::events_log`]): streaming
//!   text, commands with exit code / duration / output tail, file edits with +/-, approvals with
//!   their decision and reason.
//! * [`build_feed`] merges them. Messages come from the chat when it has any (the log's deltas are
//!   the fallback, e.g. when the child runs in another process and the chat copy is behind);
//!   commands are joined by tool id (the log completes the chat row); files and approvals come from
//!   the log when it has any, else from the chat. Events are ordered by time and only the last
//!   [`MAX_EVENTS`] are kept. Every free text passes through `store::redact`.
//! * [`external_feed`]: a process launched outside the harness has no feed of its own; its child
//!   processes are shown as commands (redacted command line, duration, CPU / RAM).
//! * [`LiveTrack`] / [`live_line`]: the one-line state of a running node for the tree and graph
//!   rows (`ejecutando `cargo test`` / `escribiendo…` / `esperando aprobación`).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use chat_core::{ApprovalState, Block, Message, Role, ToolClass, ToolRun, ToolStatus};
use serde_json::Value;

use crate::events_log::LogEvent;
use crate::tree::NodeStatus;

/// Events kept per run in memory.
pub const MAX_EVENTS: usize = 500;
/// Largest message text kept (the tail is kept).
pub const MAX_MESSAGE_CHARS: usize = 20_000;
/// Output lines kept per command.
pub const OUTPUT_MAX_LINES: usize = 40;
/// Output lines shown while a command is collapsed.
pub const OUTPUT_COLLAPSED_LINES: usize = 3;
/// Message lines shown while a message is collapsed.
pub const MESSAGE_COLLAPSED_LINES: usize = 3;
/// Child processes kept per external node.
pub const MAX_EXTERNAL_CHILDREN: usize = 100;
/// A `t` below this is "ms since the run started", not a Unix time.
const RELATIVE_T_BELOW: i64 = 100_000_000_000;

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandStatus {
    Running,
    Ok,
    Failed,
    /// The run ended (or was interrupted) before the command reported back.
    Cancelled,
}

/// A command of the run (or a child process of an external agent).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandEntry {
    pub command: String,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Last lines of the output (redacted).
    pub output: String,
    /// External children only.
    pub pid: Option<u32>,
    pub cpu_pct: Option<f32>,
    pub rss_bytes: Option<u64>,
}

/// What happened with an approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApprovalOutcome {
    /// Asked, no answer yet.
    Requested,
    /// The user allowed it.
    Allowed,
    /// The user allowed it for the rest of the profile.
    Always,
    /// The harness allowed it without asking (allowlist or `auto`).
    Auto,
    /// The harness refused it without asking (fixed blocklist).
    Blocked,
    /// The user refused it.
    Denied,
    /// Nobody answered before the run ended.
    Expired,
}

impl ApprovalOutcome {
    /// Reads the harness' word (`allowed`, `auto`, `blocked`...). A missing decision is a request.
    pub fn parse(s: Option<&str>) -> Self {
        let Some(s) = s else {
            return ApprovalOutcome::Requested;
        };
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "allowed" | "allow" | "approved" | "approve" | "yes" | "ok" => ApprovalOutcome::Allowed,
            "always" | "always_allowed" | "always_profile" | "always_run" | "allow_always" => {
                ApprovalOutcome::Always
            }
            "auto" | "auto_approved" | "allowlist" | "allowlisted" | "automatic" => {
                ApprovalOutcome::Auto
            }
            "blocked" | "block" | "blocklist" | "blocklisted" => ApprovalOutcome::Blocked,
            "denied" | "deny" | "rejected" | "reject" | "refused" | "no" => ApprovalOutcome::Denied,
            "expired" | "timeout" | "timed_out" => ApprovalOutcome::Expired,
            _ => ApprovalOutcome::Requested,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ApprovalOutcome::Requested => "Pedida",
            ApprovalOutcome::Allowed => "Permitida",
            ApprovalOutcome::Always => "Permitida siempre",
            ApprovalOutcome::Auto => "Permitida sola",
            ApprovalOutcome::Blocked => "Bloqueada",
            ApprovalOutcome::Denied => "Rechazada",
            ApprovalOutcome::Expired => "Sin respuesta",
        }
    }

    pub fn is_pending(self) -> bool {
        self == ApprovalOutcome::Requested
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ActivityKind {
    /// What the agent says. `streaming`: it is still being written.
    Message {
        text: String,
        streaming: bool,
    },
    Command(CommandEntry),
    /// A file edit. `added` / `removed` are lines, when known.
    File {
        path: String,
        added: Option<u32>,
        removed: Option<u32>,
    },
    Approval {
        subject: String,
        outcome: ApprovalOutcome,
        mode: Option<String>,
        reason: Option<String>,
    },
}

/// One row of the feed.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityEvent {
    /// Stable across rebuilds (the list diffs on it).
    pub id: String,
    /// Unix milliseconds.
    pub t_ms: i64,
    pub kind: ActivityKind,
    /// Hash of everything that is displayed, set by the feed builder: a row repaints when it changes.
    pub sig: u64,
}

impl ActivityEvent {
    pub fn new(id: impl Into<String>, t_ms: i64, kind: ActivityKind) -> Self {
        Self {
            id: id.into(),
            t_ms,
            kind,
            sig: 0,
        }
    }
}

/// The filters of the feed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ActivityFilter {
    #[default]
    All,
    Messages,
    Commands,
    Files,
}

impl ActivityFilter {
    pub const ALL: [ActivityFilter; 4] = [
        ActivityFilter::All,
        ActivityFilter::Messages,
        ActivityFilter::Commands,
        ActivityFilter::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActivityFilter::All => "Todo",
            ActivityFilter::Messages => "Mensajes",
            ActivityFilter::Commands => "Comandos",
            ActivityFilter::Files => "Archivos",
        }
    }

    /// Approvals belong to "Comandos" (they are about commands) and to "Todo".
    pub fn matches(self, kind: &ActivityKind) -> bool {
        matches!(
            (self, kind),
            (ActivityFilter::All, _)
                | (ActivityFilter::Messages, ActivityKind::Message { .. })
                | (
                    ActivityFilter::Commands,
                    ActivityKind::Command(_) | ActivityKind::Approval { .. }
                )
                | (ActivityFilter::Files, ActivityKind::File { .. })
        )
    }
}

/// The merged feed of a run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityFeed {
    /// Oldest first, at most [`MAX_EVENTS`].
    pub events: Vec<ActivityEvent>,
    /// The run is still going (new events may arrive).
    pub active: bool,
    /// Older events dropped by the cap.
    pub dropped: usize,
    /// The feed of an external (non-harness) agent: its child processes.
    pub external: bool,
}

impl ActivityFeed {
    /// Indices of the events that pass `filter`.
    pub fn visible(&self, filter: ActivityFilter) -> Vec<usize> {
        self.events
            .iter()
            .enumerate()
            .filter(|(_, e)| filter.matches(&e.kind))
            .map(|(i, _)| i)
            .collect()
    }

    /// How many events each filter shows, in [`ActivityFilter::ALL`] order.
    pub fn counts(&self) -> [usize; 4] {
        let mut out = [0; 4];
        for (i, f) in ActivityFilter::ALL.iter().enumerate() {
            out[i] = self.events.iter().filter(|e| f.matches(&e.kind)).count();
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------------------------

/// The last `n` lines of `text` (blank lines at the end ignored) and whether lines were dropped.
pub fn last_lines(text: &str, n: usize) -> (String, bool) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    if lines.len() <= n {
        return (lines.join("\n"), false);
    }
    (lines[lines.len() - n..].join("\n"), true)
}

/// A message is long enough to collapse (more lines than shown, or a lot of text).
pub fn needs_collapse(text: &str) -> bool {
    text.trim_end().lines().count() > MESSAGE_COLLAPSED_LINES || text.chars().count() > 360
}

/// First line, shortened to `max` chars with an ellipsis.
pub fn short(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

fn tail_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        s.chars().skip(n - max).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Source A: the chat thread
// ---------------------------------------------------------------------------------------------

fn input_str(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        input
            .get(*k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    })
}

fn line_count(s: &str) -> u32 {
    s.lines().count().min(u32::MAX as usize) as u32
}

/// `(added, removed)` lines of an edit tool call, estimated from its input.
fn edit_counts(input: &Value) -> (Option<u32>, Option<u32>) {
    let old = input_str(input, &["old_string", "old_str", "old_text", "oldText"]);
    let new = input_str(
        input,
        &[
            "new_string",
            "new_str",
            "new_text",
            "newText",
            "content",
            "text",
        ],
    );
    if old.is_some() || new.is_some() {
        return (
            new.as_deref().map(line_count),
            old.as_deref().map(line_count),
        );
    }
    if let Some(patch) = input_str(input, &["diff", "patch"]) {
        let (mut a, mut r) = (0u32, 0u32);
        for l in patch.lines() {
            if l.starts_with("+++") || l.starts_with("---") {
                continue;
            }
            if l.starts_with('+') {
                a += 1;
            } else if l.starts_with('-') {
                r += 1;
            }
        }
        return (Some(a), Some(r));
    }
    (None, None)
}

fn command_of(run: &ToolRun) -> String {
    input_str(&run.input, &["command", "cmd"]).unwrap_or_else(|| run.title.clone())
}

fn command_entry(run: &ToolRun, active: bool) -> CommandEntry {
    let status = match run.status {
        ToolStatus::Running if active => CommandStatus::Running,
        ToolStatus::Running => CommandStatus::Cancelled,
        ToolStatus::Ok => CommandStatus::Ok,
        ToolStatus::Failed => CommandStatus::Failed,
        ToolStatus::Cancelled => CommandStatus::Cancelled,
    };
    CommandEntry {
        command: command_of(run),
        status,
        exit_code: None,
        duration_ms: run.duration_ms(),
        output: run.summary.clone().unwrap_or_default(),
        pid: None,
        cpu_pct: None,
        rss_bytes: None,
    }
}

/// Turns the messages of a chat thread into feed events: the assistant's text, its commands and
/// edits, and the approvals. `thread_active` says whether a command that never reported back is
/// still running or was interrupted. User messages, thinking and read / search rows are not part of
/// the feed.
pub fn chat_events(messages: &[Message], thread_active: bool) -> Vec<ActivityEvent> {
    let mut out = Vec::new();
    let mut last_t = 0i64;
    let n_msgs = messages.len();
    for (mi, m) in messages.iter().enumerate() {
        if m.role != Role::Assistant {
            continue;
        }
        last_t = last_t.max(m.created_ms);
        let live_msg = m.streaming && mi + 1 == n_msgs;
        for (bi, b) in m.blocks.iter().enumerate() {
            let is_last_block = bi + 1 == m.blocks.len();
            match b {
                Block::Text { text } if !text.trim().is_empty() => {
                    out.push(ActivityEvent::new(
                        format!("m:{}.{bi}", m.seq),
                        last_t,
                        ActivityKind::Message {
                            text: text.clone(),
                            streaming: live_msg && is_last_block,
                        },
                    ));
                }
                Block::Command(run) => {
                    last_t = last_t.max(run.started_ms);
                    out.push(ActivityEvent::new(
                        format!("c:{}", run.id),
                        run.started_ms,
                        ActivityKind::Command(command_entry(run, thread_active)),
                    ));
                }
                Block::Tool(run) if run.class == ToolClass::Edit => {
                    last_t = last_t.max(run.started_ms);
                    let path = input_str(
                        &run.input,
                        &["file_path", "path", "filePath", "target_file", "file"],
                    )
                    .unwrap_or_else(|| run.title.clone());
                    let (added, removed) = edit_counts(&run.input);
                    out.push(ActivityEvent::new(
                        format!("f:{}", run.id),
                        run.started_ms,
                        ActivityKind::File {
                            path,
                            added,
                            removed,
                        },
                    ));
                }
                Block::Approval(a) => {
                    last_t = last_t.max(a.received_ms);
                    let outcome = match a.state {
                        ApprovalState::Pending => ApprovalOutcome::Requested,
                        ApprovalState::Allowed => ApprovalOutcome::Allowed,
                        ApprovalState::AlwaysAllowed => ApprovalOutcome::Always,
                        ApprovalState::Denied => ApprovalOutcome::Denied,
                        ApprovalState::Expired => ApprovalOutcome::Expired,
                    };
                    let subject = if a.detail.trim().is_empty() {
                        a.tool.clone()
                    } else {
                        format!("{} · {}", a.tool, a.detail)
                    };
                    out.push(ActivityEvent::new(
                        format!("a:{}", a.request_id),
                        a.received_ms,
                        ActivityKind::Approval {
                            subject,
                            outcome,
                            mode: None,
                            reason: None,
                        },
                    ));
                }
                _ => {}
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Source B: the events log
// ---------------------------------------------------------------------------------------------

/// Unix ms of a log `t`: values below 1e11 are relative to the run's start; 0 / unknown reuses
/// the previous event's time so the order is kept.
fn norm_t(t: i64, started_ms: Option<i64>, prev: i64) -> i64 {
    if t <= 0 {
        return prev;
    }
    if t < RELATIVE_T_BELOW {
        return started_ms.unwrap_or(0) + t;
    }
    t
}

fn kind_is_command(kind: &str, has_command: bool) -> bool {
    match kind {
        "command" | "bash" | "shell" | "exec" | "execute" | "terminal" | "run" => true,
        "" => has_command,
        _ => false,
    }
}

/// Folds the log into feed events. `active`: the run is still going (the last message may be
/// streaming; a command that never ended is interrupted otherwise).
pub fn log_events(log: &[LogEvent], started_ms: Option<i64>, active: bool) -> Vec<ActivityEvent> {
    let mut out: Vec<ActivityEvent> = Vec::new();
    let mut cmds: HashMap<String, usize> = HashMap::new();
    let mut apprs: HashMap<String, usize> = HashMap::new();
    let mut open_msg: Option<usize> = None;
    let mut last_t = started_ms.unwrap_or(0);
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut unique = |base: String| -> String {
        let n = used.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 { base } else { format!("{base}#{n}") }
    };
    for ev in log {
        let t = norm_t(ev.t(), started_ms, last_t);
        last_t = last_t.max(t);
        match ev {
            LogEvent::MessageDelta { text, .. } => {
                if let Some(ix) = open_msg
                    && let ActivityKind::Message { text: acc, .. } = &mut out[ix].kind
                {
                    acc.push_str(text);
                    if acc.chars().count() > MAX_MESSAGE_CHARS {
                        *acc = tail_chars(acc, MAX_MESSAGE_CHARS);
                    }
                } else {
                    out.push(ActivityEvent::new(
                        unique(format!("d:{t}")),
                        t,
                        ActivityKind::Message {
                            text: text.clone(),
                            streaming: false,
                        },
                    ));
                    open_msg = Some(out.len() - 1);
                }
            }
            LogEvent::ToolStart {
                id, kind, command, ..
            } => {
                open_msg = None;
                if kind_is_command(kind, command.is_some()) {
                    let entry = CommandEntry {
                        command: command.clone().unwrap_or_default(),
                        status: CommandStatus::Running,
                        exit_code: None,
                        duration_ms: None,
                        output: String::new(),
                        pid: None,
                        cpu_pct: None,
                        rss_bytes: None,
                    };
                    out.push(ActivityEvent::new(
                        format!("c:{id}"),
                        t,
                        ActivityKind::Command(entry),
                    ));
                    cmds.insert(id.clone(), out.len() - 1);
                }
            }
            LogEvent::ToolEnd {
                id,
                exit_code,
                ok,
                duration_ms,
                output_tail,
                ..
            } => {
                open_msg = None;
                if let Some(&ix) = cmds.get(id) {
                    let started = out[ix].t_ms;
                    if let ActivityKind::Command(c) = &mut out[ix].kind {
                        c.exit_code = *exit_code;
                        c.status = match (exit_code, ok) {
                            (Some(0), _) => CommandStatus::Ok,
                            (Some(_), _) => CommandStatus::Failed,
                            (None, Some(false)) => CommandStatus::Failed,
                            (None, _) => CommandStatus::Ok,
                        };
                        c.duration_ms = duration_ms.or_else(|| (t > started).then(|| t - started));
                        if let Some(o) = output_tail {
                            c.output = o.clone();
                        }
                    }
                }
            }
            LogEvent::FileEdit {
                path,
                added,
                removed,
                ..
            } => {
                open_msg = None;
                out.push(ActivityEvent::new(
                    unique(format!("f:{t}:{path}")),
                    t,
                    ActivityKind::File {
                        path: path.clone(),
                        added: *added,
                        removed: *removed,
                    },
                ));
            }
            LogEvent::Approval {
                request,
                decision,
                mode,
                reason,
                subject,
                ..
            } => {
                open_msg = None;
                let outcome = ApprovalOutcome::parse(decision.as_deref());
                let existing = (!request.is_empty())
                    .then(|| apprs.get(request).copied())
                    .flatten();
                if let Some(ix) = existing {
                    if let ActivityKind::Approval {
                        subject: s,
                        outcome: o,
                        mode: m,
                        reason: r,
                    } = &mut out[ix].kind
                    {
                        *o = outcome;
                        if let Some(x) = subject
                            && s.is_empty()
                        {
                            *s = x.clone();
                        }
                        if mode.is_some() {
                            *m = mode.clone();
                        }
                        if reason.is_some() {
                            *r = reason.clone();
                        }
                    }
                } else {
                    out.push(ActivityEvent::new(
                        unique(format!("a:{request}")),
                        t,
                        ActivityKind::Approval {
                            // The harness writes the command asked for as the `request` itself.
                            subject: subject.clone().unwrap_or_else(|| request.clone()),
                            outcome,
                            mode: mode.clone(),
                            reason: reason.clone(),
                        },
                    ));
                    if !request.is_empty() {
                        apprs.insert(request.clone(), out.len() - 1);
                    }
                }
            }
            LogEvent::Status { .. } => {}
        }
    }
    if active {
        if let Some(ix) = open_msg
            && let ActivityKind::Message { streaming, .. } = &mut out[ix].kind
        {
            *streaming = true;
        }
    } else {
        for e in &mut out {
            if let ActivityKind::Command(c) = &mut e.kind
                && c.status == CommandStatus::Running
            {
                c.status = CommandStatus::Cancelled;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------------------------

fn merge_command(chat: &mut ActivityEvent, log: ActivityEvent) {
    let ActivityKind::Command(l) = log.kind else {
        return;
    };
    chat.t_ms = chat.t_ms.min(log.t_ms);
    if let ActivityKind::Command(c) = &mut chat.kind {
        if !l.command.is_empty() {
            c.command = l.command;
        }
        // The log knows the exit code and the output; the chat may be a step behind.
        if l.status != CommandStatus::Running || c.status == CommandStatus::Running {
            c.status = l.status;
        }
        if l.exit_code.is_some() {
            c.exit_code = l.exit_code;
        }
        if l.duration_ms.is_some() {
            c.duration_ms = l.duration_ms;
        }
        if !l.output.is_empty() {
            c.output = l.output;
        }
    }
}

fn has(events: &[ActivityEvent], f: impl Fn(&ActivityKind) -> bool) -> bool {
    events.iter().any(|e| f(&e.kind))
}

/// Merges the chat events with the log events (see the module docs for the rules), orders them by
/// time, caps them and redacts them.
pub fn build_feed(chat: Vec<ActivityEvent>, log: Vec<ActivityEvent>, active: bool) -> ActivityFeed {
    let chat_has_msg = has(&chat, |k| matches!(k, ActivityKind::Message { .. }));
    let log_has_file = has(&log, |k| matches!(k, ActivityKind::File { .. }));
    let log_has_appr = has(&log, |k| matches!(k, ActivityKind::Approval { .. }));

    let mut merged: Vec<ActivityEvent> = Vec::with_capacity(chat.len() + log.len());
    let mut cmd_at: HashMap<String, usize> = HashMap::new();
    for e in chat {
        let skip = match &e.kind {
            ActivityKind::File { .. } => log_has_file,
            ActivityKind::Approval { .. } => log_has_appr,
            _ => false,
        };
        if skip {
            continue;
        }
        if matches!(e.kind, ActivityKind::Command(_)) {
            cmd_at.insert(e.id.clone(), merged.len());
        }
        merged.push(e);
    }
    for e in log {
        match &e.kind {
            ActivityKind::Message { .. } if chat_has_msg => {}
            ActivityKind::Command(_) => {
                if let Some(&ix) = cmd_at.get(&e.id) {
                    merge_command(&mut merged[ix], e);
                } else {
                    merged.push(e);
                }
            }
            _ => merged.push(e),
        }
    }
    finish(merged, active, false)
}

/// Orders, caps, redacts and signs the events.
fn finish(mut events: Vec<ActivityEvent>, active: bool, external: bool) -> ActivityFeed {
    events.sort_by_key(|e| e.t_ms);
    let dropped = events.len().saturating_sub(MAX_EVENTS);
    if dropped > 0 {
        events.drain(..dropped);
    }
    for e in &mut events {
        match &mut e.kind {
            ActivityKind::Message { text, .. } => {
                *text = store::redact(&tail_chars(text, MAX_MESSAGE_CHARS)).text;
            }
            ActivityKind::Command(c) => {
                c.command = store::redact(&c.command).text;
                let (lines, _) = last_lines(&c.output, OUTPUT_MAX_LINES);
                c.output = store::redact(&lines).text;
            }
            ActivityKind::Approval {
                subject, reason, ..
            } => {
                *subject = store::redact(subject).text;
                if let Some(r) = reason {
                    *r = store::redact(r).text;
                }
            }
            ActivityKind::File { .. } => {}
        }
        let mut h = DefaultHasher::new();
        e.id.hash(&mut h);
        e.t_ms.hash(&mut h);
        format!("{:?}", e.kind).hash(&mut h);
        e.sig = h.finish();
    }
    ActivityFeed {
        events,
        active,
        dropped,
        external,
    }
}

// ---------------------------------------------------------------------------------------------
// External agents: their child processes
// ---------------------------------------------------------------------------------------------

/// A child process of an external agent (what `agy` spawned: `cargo`, `git`...), as the host
/// samples it. The command line must be redacted by the host; the map redacts again.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalChild {
    pub pid: u32,
    pub cmdline_redacted: String,
    /// Process start, Unix milliseconds (with the pid it identifies the process).
    pub started_ms: i64,
    /// End, Unix milliseconds (`None` while it runs). Filled by the map when the process vanishes.
    pub ended_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub cpu_pct: Option<f32>,
    pub rss_bytes: u64,
}

impl ExternalChild {
    pub fn is_running(&self) -> bool {
        self.ended_ms.is_none() && self.exit_code.is_none()
    }
}

/// The feed of an external node: each child process is a command (running while it lives).
pub fn external_feed(children: &[ExternalChild], now_ms: i64, active: bool) -> ActivityFeed {
    let events = children
        .iter()
        .map(|c| {
            let status = match (c.ended_ms, c.exit_code) {
                (None, None) => CommandStatus::Running,
                (_, Some(0)) | (Some(_), None) => CommandStatus::Ok,
                (_, Some(_)) => CommandStatus::Failed,
            };
            let end = c.ended_ms.unwrap_or(now_ms);
            ActivityEvent::new(
                format!("x:{}:{}", c.pid, c.started_ms),
                c.started_ms,
                ActivityKind::Command(CommandEntry {
                    command: c.cmdline_redacted.clone(),
                    status,
                    exit_code: c.exit_code.map(i64::from),
                    duration_ms: Some((end - c.started_ms).max(0)),
                    output: String::new(),
                    pid: Some(c.pid),
                    cpu_pct: c.is_running().then_some(c.cpu_pct).flatten(),
                    rss_bytes: (c.is_running() && c.rss_bytes > 0).then_some(c.rss_bytes),
                }),
            )
        })
        .collect();
    finish(events, active, true)
}

/// The line of an external node in the tree: its newest running child process.
pub fn external_live_line(children: &[ExternalChild]) -> Option<String> {
    children
        .iter()
        .filter(|c| c.is_running())
        .max_by_key(|c| c.started_ms)
        .map(|c| {
            format!(
                "ejecutando `{}`",
                short(&store::redact(&c.cmdline_redacted).text, 48)
            )
        })
}

// ---------------------------------------------------------------------------------------------
// Live line of a running node
// ---------------------------------------------------------------------------------------------

/// What the chat runtime says about a running thread (the fallback when the log is silent).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LiveHint {
    #[default]
    None,
    Writing,
    Thinking,
    Command(String),
}

/// What kind of event the log said last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LastKind {
    #[default]
    Unknown,
    Message,
    Tool,
    Approval,
}

/// A small fold of the log: open commands and pending approvals. Cheap to keep per running node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveTrack {
    /// `(tool id, command)` of the commands that started and have not ended.
    pub open: Vec<(String, String)>,
    /// `(request, subject)` of the approvals asked and not decided.
    pub pending: Vec<(String, String)>,
    pub last: LastKind,
}

impl LiveTrack {
    pub fn apply(&mut self, ev: &LogEvent) {
        match ev {
            LogEvent::MessageDelta { .. } => self.last = LastKind::Message,
            LogEvent::ToolStart {
                id, kind, command, ..
            } => {
                self.last = LastKind::Tool;
                if kind_is_command(kind, command.is_some()) {
                    self.open
                        .push((id.clone(), command.clone().unwrap_or_default()));
                    if self.open.len() > 32 {
                        self.open.remove(0);
                    }
                }
            }
            LogEvent::ToolEnd { id, .. } => {
                self.last = LastKind::Tool;
                self.open.retain(|(i, _)| i != id);
            }
            LogEvent::FileEdit { .. } => self.last = LastKind::Tool,
            LogEvent::Approval {
                request,
                decision,
                subject,
                ..
            } => {
                self.last = LastKind::Approval;
                self.pending.retain(|(r, _)| r != request);
                if ApprovalOutcome::parse(decision.as_deref()).is_pending() {
                    self.pending
                        .push((request.clone(), subject.clone().unwrap_or_default()));
                    if self.pending.len() > 16 {
                        self.pending.remove(0);
                    }
                }
            }
            LogEvent::Status { .. } => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.last == LastKind::Unknown && self.open.is_empty() && self.pending.is_empty()
    }
}

/// The status line under a running node's name. `None` for nodes that are not in flight.
pub fn live_line(status: NodeStatus, track: &LiveTrack, hint: &LiveHint) -> Option<String> {
    let running = |cmd: &str| format!("ejecutando `{}`", short(cmd, 48));
    match status {
        NodeStatus::Waiting => {
            let subject = track
                .pending
                .last()
                .map(|(_, s)| short(s, 40))
                .filter(|s| !s.is_empty());
            Some(match subject {
                Some(s) => format!("esperando aprobación · `{s}`"),
                None => "esperando aprobación".to_string(),
            })
        }
        NodeStatus::Running => {
            if let Some((_, cmd)) = track.open.last()
                && !cmd.trim().is_empty()
            {
                return Some(running(cmd));
            }
            if let LiveHint::Command(cmd) = hint
                && !cmd.trim().is_empty()
            {
                return Some(running(cmd));
            }
            match (track.last, hint) {
                (LastKind::Message, _) | (_, LiveHint::Writing) => Some("escribiendo…".into()),
                (_, LiveHint::Thinking) => Some("pensando…".into()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// What the chat runtime says about the newest assistant message of a running thread.
pub fn live_hint(messages: &[Message]) -> LiveHint {
    let Some(m) = messages.iter().rev().find(|m| m.role == Role::Assistant) else {
        return LiveHint::None;
    };
    if !m.streaming {
        return LiveHint::None;
    }
    if let Some(run) = m.tool_runs().filter(|r| r.is_running()).last()
        && run.class == ToolClass::Command
    {
        return LiveHint::Command(command_of(run));
    }
    match m.blocks.last() {
        Some(Block::Text { .. }) => LiveHint::Writing,
        Some(Block::Thinking(_)) => LiveHint::Thinking,
        _ => LiveHint::None,
    }
}

// ---------------------------------------------------------------------------------------------
// Row diffing for the virtualized list
// ---------------------------------------------------------------------------------------------

/// The one splice that turns the list of keys `old` into `new`: `(range of old replaced, count of
/// new items)`. `None` when they are equal. Appending is `old.len()..old.len()` + n.
pub fn diff_keys(old: &[u64], new: &[u64]) -> Option<(Range<usize>, usize)> {
    let mut p = 0;
    while p < old.len() && p < new.len() && old[p] == new[p] {
        p += 1;
    }
    if p == old.len() && p == new.len() {
        return None;
    }
    let mut s = 0;
    while s < old.len() - p && s < new.len() - p && old[old.len() - 1 - s] == new[new.len() - 1 - s]
    {
        s += 1;
    }
    Some((p..old.len() - s, new.len() - p - s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_core::{ApprovalRow, ThinkingBlock};

    fn run(id: &str, class: ToolClass, input: Value, status: ToolStatus, t: i64) -> ToolRun {
        ToolRun {
            id: id.into(),
            kind: match class {
                ToolClass::Command => agents_driver::ToolKind::Command,
                _ => agents_driver::ToolKind::Edit,
            },
            class,
            title: format!("tool {id}"),
            input,
            status,
            summary: None,
            started_ms: t,
            finished_ms: (status != ToolStatus::Running).then_some(t + 1500),
        }
    }

    fn msg(seq: u32, t: i64, blocks: Vec<Block>, streaming: bool) -> Message {
        Message {
            seq,
            role: Role::Assistant,
            created_ms: t,
            blocks,
            usage: None,
            cost: None,
            attachments: vec![],
            checkpoint: None,
            streaming,
        }
    }

    fn user(seq: u32) -> Message {
        Message {
            role: Role::User,
            ..msg(
                seq,
                0,
                vec![Block::Text {
                    text: "haz algo".into(),
                }],
                false,
            )
        }
    }

    const T0: i64 = 1_800_000_000_000;

    fn log(lines: &str) -> Vec<LogEvent> {
        crate::events_log::parse_text(lines)
    }

    #[test]
    fn chat_rows_become_events_in_order() {
        let mut cmd = run(
            "c1",
            ToolClass::Command,
            serde_json::json!({"command": "cargo test -p store"}),
            ToolStatus::Ok,
            T0 + 1_000,
        );
        cmd.summary = Some("test result: ok".into());
        let edit = run(
            "e1",
            ToolClass::Edit,
            serde_json::json!({"file_path": "src/lib.rs", "old_string": "a\nb", "new_string": "a\nb\nc\nd"}),
            ToolStatus::Ok,
            T0 + 2_000,
        );
        let messages = vec![
            user(0),
            msg(
                1,
                T0,
                vec![
                    Block::Thinking(ThinkingBlock {
                        text: "hmm".into(),
                        collapsed: true,
                    }),
                    Block::Text {
                        text: "Voy a probar.".into(),
                    },
                    Block::Command(cmd),
                    Block::Tool(edit),
                    Block::Approval(ApprovalRow {
                        request_id: "r1".into(),
                        tool: "Bash".into(),
                        detail: "git push".into(),
                        state: ApprovalState::Denied,
                        received_ms: T0 + 3_000,
                        decided_ms: Some(T0 + 4_000),
                    }),
                    Block::Text {
                        text: "Listo.".into(),
                    },
                ],
                true,
            ),
        ];
        let ev = chat_events(&messages, true);
        let kinds: Vec<&str> = ev
            .iter()
            .map(|e| match &e.kind {
                ActivityKind::Message { .. } => "msg",
                ActivityKind::Command(_) => "cmd",
                ActivityKind::File { .. } => "file",
                ActivityKind::Approval { .. } => "appr",
            })
            .collect();
        assert_eq!(kinds, ["msg", "cmd", "file", "appr", "msg"]);
        assert!(matches!(
            &ev[0].kind,
            ActivityKind::Message {
                streaming: false,
                ..
            }
        ));
        assert!(
            matches!(&ev[4].kind, ActivityKind::Message { text, streaming: true } if text == "Listo.")
        );
        let ActivityKind::Command(c) = &ev[1].kind else {
            panic!()
        };
        assert_eq!(c.command, "cargo test -p store");
        assert_eq!(c.status, CommandStatus::Ok);
        assert_eq!(c.duration_ms, Some(1_500));
        assert_eq!(c.output, "test result: ok");
        assert!(
            matches!(&ev[2].kind, ActivityKind::File { path, added: Some(4), removed: Some(2) } if path == "src/lib.rs")
        );
        assert!(
            matches!(&ev[3].kind, ActivityKind::Approval { outcome: ApprovalOutcome::Denied, subject, .. } if subject == "Bash · git push")
        );
    }

    #[test]
    fn a_command_that_never_reported_is_interrupted_when_the_thread_is_idle() {
        let r = run(
            "c",
            ToolClass::Command,
            serde_json::json!({"command":"sleep 9"}),
            ToolStatus::Running,
            T0,
        );
        let m = vec![msg(1, T0, vec![Block::Command(r)], false)];
        let running = chat_events(&m, true);
        let stopped = chat_events(&m, false);
        assert!(
            matches!(&running[0].kind, ActivityKind::Command(c) if c.status == CommandStatus::Running)
        );
        assert!(
            matches!(&stopped[0].kind, ActivityKind::Command(c) if c.status == CommandStatus::Cancelled)
        );
    }

    #[test]
    fn the_log_folds_into_messages_commands_files_and_approvals() {
        let l = log(&format!(
            r#"{{"t":{a},"type":"message_delta","text":"Voy "}}
{{"t":{b},"type":"message_delta","text":"a probar."}}
{{"t":{c},"type":"tool_start","id":"c1","kind":"command","command":"cargo test"}}
{{"t":{d},"type":"approval","request":"r1","decision":"pending","command":"cargo test"}}
{{"t":{e},"type":"approval","request":"r1","decision":"auto","mode":"allowlist","reason":"en la lista"}}
{{"t":{f},"type":"tool_end","id":"c1","exit_code":101,"duration_ms":3000,"output_tail":"error[E0308]"}}
{{"t":{g},"type":"file_edit","path":"src/a.rs","added":3,"removed":1}}
{{"t":{h},"type":"message_delta","text":"Arreglado"}}
"#,
            a = T0,
            b = T0 + 100,
            c = T0 + 200,
            d = T0 + 300,
            e = T0 + 400,
            f = T0 + 3_200,
            g = T0 + 3_300,
            h = T0 + 3_400
        ));
        let ev = log_events(&l, Some(T0), true);
        assert_eq!(ev.len(), 5, "{ev:#?}");
        assert!(
            matches!(&ev[0].kind, ActivityKind::Message { text, streaming: false } if text == "Voy a probar.")
        );
        let ActivityKind::Command(c) = &ev[1].kind else {
            panic!()
        };
        assert_eq!(
            (c.status, c.exit_code, c.duration_ms),
            (CommandStatus::Failed, Some(101), Some(3000))
        );
        assert_eq!(c.output, "error[E0308]");
        // the approval is one row: asked, then decided
        assert!(
            matches!(&ev[2].kind, ActivityKind::Approval { outcome: ApprovalOutcome::Auto, mode: Some(m), reason: Some(r), subject }
            if m == "allowlist" && r == "en la lista" && subject == "cargo test")
        );
        assert!(matches!(
            &ev[3].kind,
            ActivityKind::File {
                added: Some(3),
                removed: Some(1),
                ..
            }
        ));
        assert!(
            matches!(
                &ev[4].kind,
                ActivityKind::Message {
                    streaming: true,
                    ..
                }
            ),
            "the last message is streaming"
        );
        // not active: nothing streams
        let done = log_events(&l, Some(T0), false);
        assert!(matches!(
            &done[4].kind,
            ActivityKind::Message {
                streaming: false,
                ..
            }
        ));
    }

    #[test]
    fn relative_times_are_anchored_to_the_run_start_and_unknown_ones_keep_order() {
        let l = log(r#"{"t":500,"type":"message_delta","text":"a"}
{"type":"tool_start","id":"x","kind":"command","command":"ls"}
"#);
        let ev = log_events(&l, Some(T0), true);
        assert_eq!(ev[0].t_ms, T0 + 500);
        assert_eq!(ev[1].t_ms, T0 + 500, "no t: the previous one");
    }

    #[test]
    fn a_command_open_when_the_run_ended_is_interrupted() {
        let l =
            log(r#"{"t":1,"type":"tool_start","id":"x","kind":"command","command":"sleep 99"}"#);
        let ev = log_events(&l, Some(T0), false);
        assert!(
            matches!(&ev[0].kind, ActivityKind::Command(c) if c.status == CommandStatus::Cancelled)
        );
    }

    fn chat_cmd(id: &str, t: i64, status: CommandStatus) -> ActivityEvent {
        ActivityEvent::new(
            format!("c:{id}"),
            t,
            ActivityKind::Command(CommandEntry {
                command: "cargo test".into(),
                status,
                exit_code: None,
                duration_ms: None,
                output: String::new(),
                pid: None,
                cpu_pct: None,
                rss_bytes: None,
            }),
        )
    }

    #[test]
    fn the_merge_prefers_chat_messages_and_completes_commands_from_the_log() {
        let chat = vec![
            ActivityEvent::new(
                "m:1.0",
                T0,
                ActivityKind::Message {
                    text: "del chat".into(),
                    streaming: false,
                },
            ),
            chat_cmd("c1", T0 + 10, CommandStatus::Running),
        ];
        let l = log_events(
            &log(&format!(
                r#"{{"t":{a},"type":"message_delta","text":"del log"}}
{{"t":{b},"type":"tool_start","id":"c1","kind":"command","command":"cargo test -p store"}}
{{"t":{c},"type":"tool_end","id":"c1","exit_code":0,"duration_ms":900,"output_tail":"ok"}}
{{"t":{d},"type":"tool_start","id":"c2","kind":"command","command":"git status"}}
"#,
                a = T0 + 5,
                b = T0 + 10,
                c = T0 + 20,
                d = T0 + 30
            )),
            Some(T0),
            true,
        );
        let feed = build_feed(chat, l, true);
        assert_eq!(feed.events.len(), 3, "{:#?}", feed.events);
        assert!(
            matches!(&feed.events[0].kind, ActivityKind::Message { text, .. } if text == "del chat")
        );
        let ActivityKind::Command(c1) = &feed.events[1].kind else {
            panic!()
        };
        assert_eq!(c1.status, CommandStatus::Ok);
        assert_eq!(c1.command, "cargo test -p store");
        assert_eq!(c1.exit_code, Some(0));
        assert_eq!(c1.output, "ok");
        assert!(
            matches!(&feed.events[2].kind, ActivityKind::Command(c) if c.status == CommandStatus::Running && c.command == "git status")
        );
    }

    #[test]
    fn without_chat_messages_the_log_ones_are_used_and_files_prefer_the_log() {
        let chat = vec![
            ActivityEvent::new(
                "f:e1",
                T0,
                ActivityKind::File {
                    path: "a.rs".into(),
                    added: None,
                    removed: None,
                },
            ),
            ActivityEvent::new(
                "a:r0",
                T0,
                ActivityKind::Approval {
                    subject: "x".into(),
                    outcome: ApprovalOutcome::Requested,
                    mode: None,
                    reason: None,
                },
            ),
        ];
        let l = log_events(
            &log(&format!(
                r#"{{"t":{a},"type":"message_delta","text":"hola"}}
{{"t":{b},"type":"file_edit","path":"a.rs","added":2,"removed":0}}
"#,
                a = T0 + 1,
                b = T0 + 2
            )),
            Some(T0),
            false,
        );
        let feed = build_feed(chat, l, false);
        let files: Vec<_> = feed
            .events
            .iter()
            .filter(|e| matches!(e.kind, ActivityKind::File { .. }))
            .collect();
        assert_eq!(files.len(), 1);
        assert!(matches!(
            &files[0].kind,
            ActivityKind::File { added: Some(2), .. }
        ));
        assert!(
            feed.events
                .iter()
                .any(|e| matches!(&e.kind, ActivityKind::Message { text, .. } if text == "hola"))
        );
        // no approvals in the log: the chat's stay
        assert!(
            feed.events
                .iter()
                .any(|e| matches!(e.kind, ActivityKind::Approval { .. }))
        );
    }

    #[test]
    fn the_feed_is_capped_to_the_newest_events_and_secrets_are_redacted() {
        let mut chat: Vec<ActivityEvent> = (0..MAX_EVENTS as i64 + 40)
            .map(|i| chat_cmd(&format!("c{i}"), T0 + i, CommandStatus::Ok))
            .collect();
        if let ActivityKind::Command(c) = &mut chat[MAX_EVENTS + 39].kind {
            c.output = "token=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789\nlinea".into();
        }
        let feed = build_feed(chat, vec![], false);
        assert_eq!(feed.events.len(), MAX_EVENTS);
        assert_eq!(feed.dropped, 40);
        assert_eq!(
            feed.events[0].id, "c:c40",
            "the oldest ones are the ones dropped"
        );
        let ActivityKind::Command(c) = &feed.events.last().unwrap().kind else {
            panic!()
        };
        assert!(
            !c.output.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "{}",
            c.output
        );
        assert!(feed.events.iter().all(|e| e.sig != 0));
    }

    #[test]
    fn command_output_keeps_the_last_lines_only() {
        let mut e = chat_cmd("c", T0, CommandStatus::Ok);
        if let ActivityKind::Command(c) = &mut e.kind {
            c.output = (0..100)
                .map(|i| format!("l{i}"))
                .collect::<Vec<_>>()
                .join("\n");
        }
        let feed = build_feed(vec![e], vec![], false);
        let ActivityKind::Command(c) = &feed.events[0].kind else {
            panic!()
        };
        assert_eq!(c.output.lines().count(), OUTPUT_MAX_LINES);
        assert!(c.output.ends_with("l99"));
    }

    #[test]
    fn filters_show_the_right_kinds() {
        let feed = build_feed(
            vec![
                ActivityEvent::new(
                    "m",
                    T0,
                    ActivityKind::Message {
                        text: "x".into(),
                        streaming: false,
                    },
                ),
                chat_cmd("c", T0 + 1, CommandStatus::Ok),
                ActivityEvent::new(
                    "f",
                    T0 + 2,
                    ActivityKind::File {
                        path: "a".into(),
                        added: None,
                        removed: None,
                    },
                ),
                ActivityEvent::new(
                    "a",
                    T0 + 3,
                    ActivityKind::Approval {
                        subject: "s".into(),
                        outcome: ApprovalOutcome::Allowed,
                        mode: None,
                        reason: None,
                    },
                ),
            ],
            vec![],
            false,
        );
        assert_eq!(feed.visible(ActivityFilter::All), vec![0, 1, 2, 3]);
        assert_eq!(feed.visible(ActivityFilter::Messages), vec![0]);
        assert_eq!(
            feed.visible(ActivityFilter::Commands),
            vec![1, 3],
            "approvals ride with commands"
        );
        assert_eq!(feed.visible(ActivityFilter::Files), vec![2]);
        assert_eq!(feed.counts(), [4, 1, 2, 1]);
    }

    #[test]
    fn approval_words_map_to_outcomes() {
        use ApprovalOutcome::*;
        for (w, o) in [
            ("allowed", Allowed),
            ("Approved", Allowed),
            ("always_profile", Always),
            ("always_run", Always),
            ("auto", Auto),
            ("auto-approved", Auto),
            ("blocked", Blocked),
            ("denied", Denied),
            ("expired", Expired),
            ("pending", Requested),
            ("???", Requested),
        ] {
            assert_eq!(ApprovalOutcome::parse(Some(w)), o, "{w}");
        }
        assert_eq!(ApprovalOutcome::parse(None), Requested);
    }

    #[test]
    fn the_live_line_follows_the_log_and_the_hint() {
        let mut t = LiveTrack::default();
        assert_eq!(live_line(NodeStatus::Running, &t, &LiveHint::None), None);
        assert_eq!(
            live_line(NodeStatus::Done, &t, &LiveHint::Writing),
            None,
            "only running nodes"
        );
        for ev in log(r#"{"type":"message_delta","text":"a"}
{"type":"tool_start","id":"c1","kind":"command","command":"cargo test -p store"}
"#)
        {
            t.apply(&ev);
        }
        assert_eq!(
            live_line(NodeStatus::Running, &t, &LiveHint::None).as_deref(),
            Some("ejecutando `cargo test -p store`")
        );
        t.apply(&LogEvent::ToolEnd {
            t: 1,
            id: "c1".into(),
            exit_code: Some(0),
            ok: None,
            duration_ms: None,
            output_tail: None,
        });
        assert_eq!(
            live_line(NodeStatus::Running, &t, &LiveHint::None),
            None,
            "thinking between tools says nothing"
        );
        t.apply(&LogEvent::MessageDelta {
            t: 2,
            text: "x".into(),
        });
        assert_eq!(
            live_line(NodeStatus::Running, &t, &LiveHint::None).as_deref(),
            Some("escribiendo…")
        );
        assert_eq!(
            live_line(
                NodeStatus::Running,
                &LiveTrack::default(),
                &LiveHint::Thinking
            )
            .as_deref(),
            Some("pensando…")
        );
        assert_eq!(
            live_line(
                NodeStatus::Running,
                &LiveTrack::default(),
                &LiveHint::Command("ls -la".into())
            )
            .as_deref(),
            Some("ejecutando `ls -la`")
        );
        // waiting
        t.apply(&LogEvent::Approval {
            t: 3,
            request: "r".into(),
            decision: None,
            mode: None,
            reason: None,
            subject: Some("git push".into()),
        });
        assert_eq!(
            live_line(NodeStatus::Waiting, &t, &LiveHint::None).as_deref(),
            Some("esperando aprobación · `git push`")
        );
        t.apply(&LogEvent::Approval {
            t: 4,
            request: "r".into(),
            decision: Some("allowed".into()),
            mode: None,
            reason: None,
            subject: None,
        });
        assert_eq!(
            live_line(NodeStatus::Waiting, &t, &LiveHint::None).as_deref(),
            Some("esperando aprobación")
        );
    }

    #[test]
    fn the_hint_comes_from_the_newest_streaming_message() {
        let cmd = run(
            "c",
            ToolClass::Command,
            serde_json::json!({"command":"npm test"}),
            ToolStatus::Running,
            T0,
        );
        assert_eq!(
            live_hint(&[msg(1, T0, vec![Block::Command(cmd)], true)]),
            LiveHint::Command("npm test".into())
        );
        assert_eq!(
            live_hint(&[msg(
                1,
                T0,
                vec![Block::Text {
                    text: "hola".into()
                }],
                true
            )]),
            LiveHint::Writing
        );
        assert_eq!(
            live_hint(&[msg(
                1,
                T0,
                vec![Block::Text {
                    text: "hola".into()
                }],
                false
            )]),
            LiveHint::None,
            "a finished message says nothing"
        );
        assert_eq!(live_hint(&[user(0)]), LiveHint::None);
    }

    fn child(pid: u32, start: i64, end: Option<i64>, code: Option<i32>) -> ExternalChild {
        ExternalChild {
            pid,
            cmdline_redacted: format!(
                "cargo test --token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 {pid}"
            ),
            started_ms: start,
            ended_ms: end,
            exit_code: code,
            cpu_pct: Some(12.0),
            rss_bytes: 50 << 20,
        }
    }

    #[test]
    fn external_children_are_commands_in_progress() {
        let kids = vec![
            child(10, T0, Some(T0 + 4_000), Some(0)),
            child(11, T0 + 5_000, None, None),
            child(12, T0 + 1, Some(T0 + 2), Some(2)),
        ];
        let feed = external_feed(&kids, T0 + 9_000, true);
        assert!(feed.external && feed.active);
        assert_eq!(feed.events.len(), 3);
        let by_pid = |pid: u32| {
            feed.events
                .iter()
                .find_map(|e| match &e.kind {
                    ActivityKind::Command(c) if c.pid == Some(pid) => Some(c.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(by_pid(10).status, CommandStatus::Ok);
        assert_eq!(by_pid(10).duration_ms, Some(4_000));
        let live = by_pid(11);
        assert_eq!(live.status, CommandStatus::Running);
        assert_eq!(live.duration_ms, Some(4_000), "runs until now");
        assert_eq!(live.rss_bytes, Some(50 << 20));
        assert_eq!(live.cpu_pct, Some(12.0));
        assert!(
            !live
                .command
                .contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "redacted: {}",
            live.command
        );
        assert_eq!(by_pid(12).status, CommandStatus::Failed);
        assert_eq!(by_pid(12).cpu_pct, None, "finished processes show no CPU");
        let line = external_live_line(&kids).unwrap();
        assert!(line.starts_with("ejecutando `cargo test"), "{line}");
        assert_eq!(external_live_line(&kids[..1]), None);
    }

    #[test]
    fn diff_keys_splices_the_smallest_range() {
        assert_eq!(diff_keys(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(
            diff_keys(&[1, 2, 3], &[1, 2, 3, 4, 5]),
            Some((3..3, 2)),
            "append"
        );
        assert_eq!(
            diff_keys(&[1, 2, 3], &[1, 9, 3]),
            Some((1..2, 1)),
            "one row changed"
        );
        assert_eq!(diff_keys(&[], &[1, 2]), Some((0..0, 2)));
        assert_eq!(diff_keys(&[1, 2], &[]), Some((0..2, 0)));
        assert_eq!(
            diff_keys(&[1, 2, 3, 4], &[1, 4]),
            Some((1..3, 0)),
            "removed in the middle"
        );
    }

    #[test]
    fn text_helpers() {
        assert_eq!(last_lines("a\nb\nc\nd\n", 2), ("c\nd".to_string(), true));
        assert_eq!(last_lines("a\nb", 3), ("a\nb".to_string(), false));
        assert!(needs_collapse("1\n2\n3\n4"));
        assert!(!needs_collapse("corto"));
        assert!(needs_collapse(&"x".repeat(400)));
        assert_eq!(short("  \nprimera línea\nsegunda", 40), "primera línea");
        assert_eq!(short("abcdefghij", 5), "abcd…");
    }
}
