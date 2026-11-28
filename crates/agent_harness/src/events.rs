//! Per-run events log: `<data dir>\agent-runs\<run_id>.events.jsonl` (ola 9B).
//!
//! The harness appends one JSON object per line while a delegated run is in flight, so a viewer
//! (the agent map's "Actividad" tab) can show what the child is doing without opening `chat.db`.
//! Nothing is written when no run is in flight (zero idle cost): the log is produced by the run's own
//! wait loop, which already wakes every 250 ms, and only when the thread state actually changed.
//!
//! # Format (version 1)
//!
//! UTF-8, `\n`-terminated lines, each one object. Common fields:
//!
//! * `"t"`: **Unix time in milliseconds** (wall clock) of the event. Tool events use the moments the
//!   chat runtime recorded, so `tool_end.t - tool_start.t` is about `duration_ms`.
//! * `"type"`: the event kind, snake_case.
//!
//! ```text
//! {"t":1760000000000,"type":"status","status":"working"}
//! {"t":1760000000300,"type":"message_delta","text":"I'll run the tests. "}
//! {"t":1760000000400,"type":"tool_start","id":"c1","kind":"command","command":"cargo test -p store","title":"cargo test -p store"}
//! {"t":1760000009400,"type":"tool_end","id":"c1","exit_code":0,"duration_ms":9000,"output_tail":"test result: ok","ok":true}
//! {"t":1760000009500,"type":"tool_start","id":"e1","kind":"edit","path":"src/lib.rs","title":"src/lib.rs"}
//! {"t":1760000009600,"type":"tool_end","id":"e1","duration_ms":100,"output_tail":"","ok":true}
//! {"t":1760000009600,"type":"file_edit","path":"src/lib.rs","added":3,"removed":1}
//! {"t":1760000009700,"type":"approval","request":"cargo build","decision":"allowed","mode":"ask"}
//! {"t":1760000010000,"type":"truncated","limit_bytes":5242880}
//! ```
//!
//! | `type` | fields |
//! |---|---|
//! | `message_delta` | `text`: new assistant text since the previous delta (coalesced: written at most every 250 ms, or at once before a tool / approval / status change; never cut inside a word except after 256 characters without a space) |
//! | `tool_start` | `id` (the agent's tool call id, unique per run), `kind` = `command` \| `edit` \| `read` \| `other`, `command` (kind `command`), `path` (kinds `edit` / `read`, project-relative with `/`), `title` (extra: the agent's label, up to 200 chars) |
//! | `tool_end` | `id`, `exit_code` (commands only; `0` when the agent reported success, parsed from `exit N` when it failed, absent when unknown), `duration_ms`, `output_tail` (redacted, last <= 4 KB; may be empty), `ok` (extra: `false` for a failed or interrupted tool) |
//! | `file_edit` | `path` (project-relative, `/`), `added`, `removed` (line counts, approximate when the agent's tool input was truncated); written after the `tool_end` of a successful edit, one per file |
//! | `approval` | `request` (redacted command, or `tool: detail`), `decision` = `allowed` \| `denied` \| `auto` \| `blocked` \| `always_run` \| `always_profile`, `mode` (`ask` \| `allowlist` \| `auto`, the mode in force when it was asked), `reason` (optional: why it was denied / blocked, or a note) |
//! | `status` | `status`: `working` \| `waiting` \| `done` \| `error` while the agent runs (the chat thread's status), then exactly one final run status: `done` \| `error` \| `cancelled` \| `timed_out` \| `budget_exceeded` |
//! | `follow_up` | `reason` = `background` \| `denial`, `text`: the automatic message the harness sent after a turn that ended early (a command still running, or a refused request the agent had not been told about) |
//! | `follow_up` | `reason` = `background` \| `denial`, `text`: the automatic message the harness sent after a turn that ended early (a command still running, or a refused request the agent had not been told about) |
//! | `truncated` | `limit_bytes`: written once when the file reached 5 MB |
//!
//! Every string goes through `store::redact` before it is written. Readers must ignore unknown
//! fields and unknown `type`s (the format grows by adding fields / kinds).
//!
//! # Size cap
//!
//! The file never grows past 5 MB (+ a small margin for the last events). When the next line would
//! cross 5 MB one `truncated` event is written and from then on only `status`, `tool_end` and
//! `approval` events are appended (`message_delta`, `tool_start` and `file_edit` are dropped); past
//! 6 MB only `status` events are kept. Each line is written with a single `write` call, so a reader
//! tailing the file never sees half a line from this writer.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chat_core::{Block, Role, ThreadState, ThreadStatus, ToolClass, ToolRun, ToolStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The file stops taking deltas at this size.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Past this size only `status` events are written.
pub const HARD_CAP_BYTES: u64 = 6 * 1024 * 1024;
/// Longest `output_tail` (bytes).
pub const MAX_TAIL_BYTES: usize = 4096;
/// Minimum spacing between two `message_delta` lines.
pub const DELTA_INTERVAL: Duration = Duration::from_millis(250);
/// Characters of a word held back by a non-forced flush before it is written anyway.
const MAX_HELD_WORD: usize = 256;
const MAX_COMMAND_CHARS: usize = 2000;
const MAX_TITLE_CHARS: usize = 200;

/// What a tool call is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventToolKind {
    Command,
    Edit,
    Read,
    Other,
}

/// How an approval request was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    /// The user allowed it (also "always for this run's tool").
    Allowed,
    Denied,
    /// Approved by the harness without asking (mode `auto`, or `allowlist` and it matched).
    Auto,
    /// On the hard blocklist: denied without asking.
    Blocked,
    /// The user allowed it and the rest of the run is automatic.
    AlwaysRun,
    /// The user allowed it and its pattern was saved to the profile (when it could be).
    AlwaysProfile,
}

/// The body of one event (the `type` tag and its fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    MessageDelta {
        text: String,
    },
    ToolStart {
        id: String,
        kind: EventToolKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    ToolEnd {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        duration_ms: i64,
        #[serde(default)]
        output_tail: String,
        #[serde(default = "yes")]
        ok: bool,
    },
    FileEdit {
        path: String,
        added: u32,
        removed: u32,
    },
    Approval {
        request: String,
        decision: ApprovalOutcome,
        mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Status {
        status: String,
    },
    Truncated {
        limit_bytes: u64,
    },
    /// The harness sent the agent another message after a turn that ended early (`background`: a
    /// command was still running; `denial`: a request was refused and the agent was told why).
    FollowUp {
        reason: String,
        text: String,
    },
}

fn yes() -> bool {
    true
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    /// Unix ms.
    pub t: i64,
    #[serde(flatten)]
    pub body: EventBody,
}

impl RunEvent {
    pub fn new(t: i64, body: EventBody) -> RunEvent {
        RunEvent { t, body }
    }

    /// The line (without the `\n`).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse one line; `None` for anything that is not a known event.
    pub fn parse_line(line: &str) -> Option<RunEvent> {
        serde_json::from_str(line.trim()).ok()
    }

    /// Every readable event of a log's text, in order.
    pub fn parse_all(text: &str) -> Vec<RunEvent> {
        text.lines().filter_map(RunEvent::parse_line).collect()
    }

    fn tier(&self) -> u8 {
        match self.body {
            EventBody::Status { .. } | EventBody::Truncated { .. } => 0,
            EventBody::ToolEnd { .. } | EventBody::Approval { .. } => 1,
            _ => 2,
        }
    }
}

/// `<agent-runs dir>\<run_id>.events.jsonl`.
pub fn events_path(runs_dir: &Path, run_id: &str) -> PathBuf {
    runs_dir.join(format!("{run_id}.events.jsonl"))
}

/// The append-only writer (one `write` per line, size cap).
#[derive(Debug)]
pub struct EventLog {
    file: Option<File>,
    path: PathBuf,
    bytes: u64,
    truncated: bool,
    max_bytes: u64,
}

impl EventLog {
    /// Create (or reopen for append) the log of `run_id`. A file that cannot be opened disables the
    /// log (a warning is logged once): the run goes on.
    pub fn open(runs_dir: &Path, run_id: &str) -> EventLog {
        EventLog::open_at(events_path(runs_dir, run_id), MAX_FILE_BYTES)
    }

    pub(crate) fn open_at(path: PathBuf, max_bytes: u64) -> EventLog {
        let _ = path.parent().map(std::fs::create_dir_all);
        let file = OpenOptions::new().create(true).append(true).open(&path);
        let (file, bytes) = match file {
            Ok(f) => {
                let n = f.metadata().map(|m| m.len()).unwrap_or(0);
                (Some(f), n)
            }
            Err(e) => {
                log::warn!(
                    "agent_harness: cannot open the events log {}: {e}",
                    path.display()
                );
                (None, 0)
            }
        };
        EventLog {
            file,
            path,
            bytes,
            truncated: bytes >= max_bytes,
            max_bytes,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Append one event; `false` when it was dropped (log disabled or over the cap).
    pub fn append(&mut self, ev: &RunEvent) -> bool {
        if self.file.is_none() {
            return false;
        }
        let mut line = ev.to_line();
        line.push('\n');
        let size = line.len() as u64;
        let tier = ev.tier();
        let hard = self
            .max_bytes
            .saturating_add(HARD_CAP_BYTES - MAX_FILE_BYTES);
        if !self.truncated && tier > 0 && self.bytes + size > self.max_bytes {
            self.truncated = true;
            let mark = RunEvent::new(
                ev.t,
                EventBody::Truncated {
                    limit_bytes: self.max_bytes,
                },
            );
            self.write_line(&mark);
        }
        if self.truncated && (tier == 2 || (tier == 1 && self.bytes + size > hard)) {
            return false;
        }
        self.write_line(ev)
    }

    fn write_line(&mut self, ev: &RunEvent) -> bool {
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        let mut line = ev.to_line();
        line.push('\n');
        match file.write_all(line.as_bytes()) {
            Ok(()) => {
                self.bytes += line.len() as u64;
                true
            }
            Err(e) => {
                log::warn!("agent_harness: events log write failed: {e}");
                self.file = None;
                false
            }
        }
    }
}

fn red(text: &str) -> String {
    store::redact(text).text
}

fn cap(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max_chars).collect();
    s.push('…');
    s
}

/// The last `max` bytes of `s`, cut at a character boundary.
pub fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

// ---------------------------------------------------------------------------
// ThreadState -> events
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct ToolMark {
    started: bool,
    ended: bool,
}

/// Turns what happens in a run's chat thread into events. Feed it the thread state with
/// [`Recorder::observe`]; nothing is read or written between two observations.
#[derive(Debug)]
pub struct Recorder {
    log: EventLog,
    roots: Vec<PathBuf>,
    out: Vec<RunEvent>,
    last_revision: Option<u64>,
    last_status: Option<String>,
    /// Bytes of each text block already moved into `pending`, keyed by (message seq, block index).
    text_marks: HashMap<(u32, usize), usize>,
    tools: HashMap<String, ToolMark>,
    pending: String,
    last_delta: Option<Instant>,
    delta_interval: Duration,
}

impl Recorder {
    /// `roots`: the project folder and the run's own folder (worktree); paths inside them are logged
    /// relative.
    pub fn new(log: EventLog, roots: &[&Path]) -> Recorder {
        Recorder {
            log,
            roots: roots.iter().map(|p| p.to_path_buf()).collect(),
            out: Vec::new(),
            last_revision: None,
            last_status: None,
            text_marks: HashMap::new(),
            tools: HashMap::new(),
            pending: String::new(),
            last_delta: None,
            delta_interval: DELTA_INTERVAL,
        }
    }

    pub fn log(&self) -> &EventLog {
        &self.log
    }

    #[cfg(test)]
    pub(crate) fn set_delta_interval(&mut self, d: Duration) {
        self.delta_interval = d;
    }

    /// Collect the events the state implies since the last call (no I/O: call [`Recorder::commit`]
    /// after releasing any lock on the state).
    pub fn observe(&mut self, state: &ThreadState) {
        let status = state.status.as_str().to_string();
        let changed_status = self.last_status.as_deref() != Some(&status);
        let changed = self.last_revision != Some(state.revision);
        // "working" opens the turn, so it goes before what the turn did; the other statuses
        // (waiting, done, error) follow it
        let opens = changed_status && state.status == ThreadStatus::Working;
        if opens {
            self.push(EventBody::Status {
                status: status.clone(),
            });
        }
        if changed || changed_status {
            self.last_revision = Some(state.revision);
            self.scan(state);
        }
        if changed_status {
            // a status that is only "idle" before the first turn is not worth a line
            if !opens && state.status != ThreadStatus::Idle {
                self.flush_text(true);
                self.push(EventBody::Status {
                    status: status.clone(),
                });
            }
            self.last_status = Some(status);
        }
        self.flush_text(false);
    }

    /// Write what was collected.
    pub fn commit(&mut self) {
        for ev in std::mem::take(&mut self.out) {
            self.log.append(&ev);
        }
    }

    /// Observe a live runtime and commit.
    pub fn sync(&mut self, rt: &chat_core::ThreadRuntime) {
        rt.with_state(|s| self.observe(s));
        self.commit();
    }

    /// An approval was settled by the harness (the pending text is written first, to keep order).
    pub fn approval(
        &mut self,
        request: &str,
        decision: ApprovalOutcome,
        mode: &str,
        reason: Option<&str>,
    ) {
        self.flush_text(true);
        self.push(EventBody::Approval {
            request: cap(&red(request), 2000),
            decision,
            mode: mode.to_string(),
            reason: reason.map(|r| cap(&red(r), 500)),
        });
        self.commit();
    }

    /// The harness is about to send an automatic follow-up (`reason` = `background` | `denial`).
    pub fn follow_up(&mut self, reason: &str, text: &str) {
        self.flush_text(true);
        self.push(EventBody::FollowUp {
            reason: reason.to_string(),
            text: cap(&red(text), 1000),
        });
        self.commit();
    }

    /// The final run status (`done`, `error`, `cancelled`, `timed_out`, `budget_exceeded`).
    pub fn finish(&mut self, status: &str) {
        self.flush_text(true);
        if self.last_status.as_deref() != Some(status) {
            self.push(EventBody::Status {
                status: status.to_string(),
            });
            self.last_status = Some(status.to_string());
        }
        self.commit();
    }

    fn push(&mut self, body: EventBody) {
        self.out.push(RunEvent::new(chat_core::now_ms(), body));
    }

    fn push_at(&mut self, t: i64, body: EventBody) {
        self.out.push(RunEvent::new(t, body));
    }

    fn scan(&mut self, state: &ThreadState) {
        for msg in state.messages.iter().filter(|m| m.role == Role::Assistant) {
            for (bi, block) in msg.blocks.iter().enumerate() {
                match block {
                    Block::Text { text } => {
                        let mark = self.text_marks.entry((msg.seq, bi)).or_insert(0);
                        if text.len() > *mark {
                            let mut from = *mark;
                            while !text.is_char_boundary(from) {
                                from += 1;
                            }
                            self.pending.push_str(&text[from..]);
                            *mark = text.len();
                        }
                    }
                    Block::Command(run) | Block::Tool(run) => self.scan_run(run),
                    Block::Explored(g) => {
                        for run in &g.runs {
                            self.scan_run(run);
                        }
                    }
                    Block::Thinking(_) | Block::Approval(_) => {}
                }
            }
        }
    }

    fn scan_run(&mut self, run: &ToolRun) {
        let mark = self.tools.get(&run.id).copied().unwrap_or_default();
        if !mark.started {
            self.flush_text(true);
            let (kind, command, path) = describe(run, &self.roots);
            self.push_at(
                run.started_ms,
                EventBody::ToolStart {
                    id: run.id.clone(),
                    kind,
                    command,
                    path,
                    title: Some(cap(&red(&run.title), MAX_TITLE_CHARS)).filter(|t| !t.is_empty()),
                },
            );
            self.tools.entry(run.id.clone()).or_default().started = true;
        }
        if !mark.ended && run.status != ToolStatus::Running {
            self.flush_text(true);
            let ok = run.status == ToolStatus::Ok;
            let summary = run.summary.as_deref().unwrap_or("");
            let end = run.finished_ms.unwrap_or_else(chat_core::now_ms);
            let exit_code = if run.class == ToolClass::Command {
                parse_exit_code(summary).or(ok.then_some(0))
            } else {
                None
            };
            self.push_at(
                end,
                EventBody::ToolEnd {
                    id: run.id.clone(),
                    exit_code,
                    duration_ms: run.duration_ms().unwrap_or(0),
                    output_tail: tail_bytes(&red(summary), MAX_TAIL_BYTES).to_string(),
                    ok,
                },
            );
            if ok && run.class == ToolClass::Edit {
                for e in edit_stats(&run.input, &run.title) {
                    let path = relative_path(&e.path, &self.roots);
                    self.push_at(
                        end,
                        EventBody::FileEdit {
                            path: red(&path),
                            added: e.added,
                            removed: e.removed,
                        },
                    );
                }
            }
            self.tools.entry(run.id.clone()).or_default().ended = true;
        }
    }

    /// Move `pending` to the out queue as one `message_delta`. Not forced: at most every
    /// `delta_interval`, and a trailing partial word stays for the next flush (a secret split across two
    /// lines would escape redaction).
    fn flush_text(&mut self, force: bool) {
        if self.pending.is_empty() {
            return;
        }
        let now = Instant::now();
        if !force {
            if let Some(last) = self.last_delta {
                if now.duration_since(last) < self.delta_interval {
                    return;
                }
            }
        }
        let cut = if force {
            self.pending.len()
        } else {
            match self.pending.rfind(char::is_whitespace) {
                Some(i) => i + self.pending[i..].chars().next().map_or(1, char::len_utf8),
                None if self.pending.chars().count() > MAX_HELD_WORD => self.pending.len(),
                None => return,
            }
        };
        let rest = self.pending.split_off(cut);
        let text = std::mem::replace(&mut self.pending, rest);
        if text.is_empty() {
            return;
        }
        self.last_delta = Some(now);
        self.push(EventBody::MessageDelta { text: red(&text) });
    }
}

/// `kind`, `command`, `path` of a tool call.
fn describe(run: &ToolRun, roots: &[PathBuf]) -> (EventToolKind, Option<String>, Option<String>) {
    match run.class {
        ToolClass::Command => {
            let cmd = run
                .input
                .get("command")
                .or_else(|| run.input.get("cmd"))
                .and_then(command_text)
                .unwrap_or_else(|| run.title.clone());
            (
                EventToolKind::Command,
                Some(cap(&red(&cmd), MAX_COMMAND_CHARS)),
                None,
            )
        }
        ToolClass::Edit => {
            let path = edit_stats(&run.input, &run.title)
                .first()
                .map(|e| relative_path(&e.path, roots));
            (EventToolKind::Edit, None, path.map(|p| red(&p)))
        }
        ToolClass::File => {
            let path = path_of(&run.input).or_else(|| path_like(&run.title));
            (
                EventToolKind::Read,
                None,
                path.map(|p| red(&relative_path(&p, roots))),
            )
        }
        _ => (EventToolKind::Other, None, None),
    }
}

fn command_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

const PATH_KEYS: &[&str] = &["file_path", "path", "filePath", "notebook_path", "file"];

fn path_of(input: &Value) -> Option<String> {
    PATH_KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// A title that is just a path (`src/lib.rs`).
fn path_like(title: &str) -> Option<String> {
    let t = title.trim();
    (!t.is_empty() && !t.contains(char::is_whitespace) && t.contains(['/', '\\', '.']))
        .then(|| t.to_string())
}

/// `exit 1`, `exit code: 2`, `exited with code 3` at the start of a summary.
pub fn parse_exit_code(summary: &str) -> Option<i64> {
    let s = summary.trim().to_ascii_lowercase();
    let rest = s
        .strip_prefix("exit code")
        .or_else(|| s.strip_prefix("exited with code"))
        .or_else(|| s.strip_prefix("exit"))?;
    let rest = rest.trim_start_matches([' ', ':', '=']);
    let digits: String = rest
        .chars()
        .enumerate()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
        .map(|(_, c)| c)
        .collect();
    digits.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditStat {
    path: String,
    added: u32,
    removed: u32,
}

fn count_lines(s: &str) -> u32 {
    if s.is_empty() {
        0
    } else {
        s.lines().count().max(1) as u32
    }
}

/// Added / removed lines between two texts, ignoring the lines they share at both ends.
fn replace_stats(old: &str, new: &str) -> (u32, u32) {
    let o: Vec<&str> = old.lines().collect();
    let n: Vec<&str> = new.lines().collect();
    let mut head = 0;
    while head < o.len() && head < n.len() && o[head] == n[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < o.len() - head
        && tail < n.len() - head
        && o[o.len() - 1 - tail] == n[n.len() - 1 - tail]
    {
        tail += 1;
    }
    (
        (n.len() - head - tail) as u32,
        (o.len() - head - tail) as u32,
    )
}

fn diff_stats(diff: &str) -> (u32, u32) {
    let (mut a, mut r) = (0, 0);
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            a += 1;
        } else if l.starts_with('-') {
            r += 1;
        }
    }
    (a, r)
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

/// Lines added / removed by one edit-shaped object (Claude `Edit` / `Write` / `MultiEdit`, Codex
/// `fileChange` entries, ACP diffs).
fn stats_of(v: &Value) -> (u32, u32) {
    if let Some(diff) = str_of(v, &["diff", "unified_diff", "patch"]) {
        return diff_stats(diff);
    }
    if let Some(edits) = v.get("edits").and_then(Value::as_array) {
        return edits.iter().fold((0, 0), |(a, r), e| {
            let (ea, er) = stats_of(e);
            (a + ea, r + er)
        });
    }
    let old = str_of(v, &["old_string", "old_str", "oldText", "old_text"]);
    let new = str_of(v, &["new_string", "new_str", "newText", "new_text"]);
    match (old, new) {
        (Some(o), Some(n)) => replace_stats(o, n),
        (None, Some(n)) => (count_lines(n), 0),
        (Some(o), None) => (0, count_lines(o)),
        (None, None) => match str_of(v, &["content", "file_text", "new_content"]) {
            Some(c) => (count_lines(c), 0),
            None => (0, 0),
        },
    }
}

fn edit_stats(input: &Value, title: &str) -> Vec<EditStat> {
    let list = input
        .as_array()
        .or_else(|| input.get("changes").and_then(Value::as_array));
    if let Some(items) = list {
        return items
            .iter()
            .filter_map(|c| {
                let path = path_of(c)?;
                let (added, removed) = stats_of(c);
                Some(EditStat {
                    path,
                    added,
                    removed,
                })
            })
            .collect();
    }
    let Some(path) = path_of(input).or_else(|| path_like(title)) else {
        return Vec::new();
    };
    let (added, removed) = stats_of(input);
    vec![EditStat {
        path,
        added,
        removed,
    }]
}

/// `path` relative to the first root that contains it, with `/`.
pub fn relative_path(path: &str, roots: &[PathBuf]) -> String {
    let p = path.replace('\\', "/");
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    for r in roots {
        let root = r.to_string_lossy().replace('\\', "/");
        let root = root
            .strip_prefix("//?/")
            .unwrap_or(&root)
            .trim_end_matches('/');
        if root.is_empty() {
            continue;
        }
        if p.len() > root.len()
            && p.is_char_boundary(root.len())
            && p[..root.len()].eq_ignore_ascii_case(root)
            && p.as_bytes()[root.len()] == b'/'
        {
            return p[root.len() + 1..].to_string();
        }
    }
    p.strip_prefix("./").unwrap_or(p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_round_trip_with_the_documented_field_names() {
        let evs = vec![
            RunEvent::new(
                1,
                EventBody::ToolStart {
                    id: "c1".into(),
                    kind: EventToolKind::Command,
                    command: Some("cargo test".into()),
                    path: None,
                    title: None,
                },
            ),
            RunEvent::new(
                2,
                EventBody::ToolEnd {
                    id: "c1".into(),
                    exit_code: Some(0),
                    duration_ms: 5,
                    output_tail: "ok".into(),
                    ok: true,
                },
            ),
            RunEvent::new(
                3,
                EventBody::Approval {
                    request: "ls".into(),
                    decision: ApprovalOutcome::AlwaysProfile,
                    mode: "ask".into(),
                    reason: None,
                },
            ),
        ];
        for e in &evs {
            assert_eq!(RunEvent::parse_line(&e.to_line()).as_ref(), Some(e));
        }
        let v: Value = serde_json::from_str(&evs[0].to_line()).unwrap();
        assert_eq!(v["type"], "tool_start");
        assert_eq!(v["kind"], "command");
        assert_eq!(v["t"], 1);
        assert!(v.get("path").is_none());
        let v: Value = serde_json::from_str(&evs[2].to_line()).unwrap();
        assert_eq!(v["decision"], "always_profile");
        // a reader ignores unknown fields and unknown kinds
        assert!(
            RunEvent::parse_line(r#"{"t":1,"type":"status","status":"done","extra":1}"#).is_some()
        );
        assert!(RunEvent::parse_line(r#"{"t":1,"type":"future"}"#).is_none());
    }

    #[test]
    fn exit_codes_are_parsed_from_summaries() {
        assert_eq!(parse_exit_code("exit 1"), Some(1));
        assert_eq!(parse_exit_code("Exit code: 101"), Some(101));
        assert_eq!(parse_exit_code("exit -1"), Some(-1));
        assert_eq!(parse_exit_code("exited with code 3 (killed)"), Some(3));
        assert_eq!(parse_exit_code("test result: ok"), None);
        assert_eq!(parse_exit_code("exit"), None);
    }

    #[test]
    fn edit_stats_cover_the_agent_shapes() {
        let s = edit_stats(
            &json!({"file_path":"C:/p/src/a.rs","old_string":"a\nb\nc","new_string":"a\nB\nB2\nc"}),
            "",
        );
        assert_eq!(
            s,
            [EditStat {
                path: "C:/p/src/a.rs".into(),
                added: 2,
                removed: 1
            }]
        );
        let w = edit_stats(&json!({"file_path":"x.txt","content":"1\n2\n3\n"}), "");
        assert_eq!((w[0].added, w[0].removed), (3, 0));
        let m = edit_stats(
            &json!({"file_path":"m.rs","edits":[{"old_string":"a","new_string":"b"},{"old_string":"c\nd","new_string":"c"}]}),
            "",
        );
        assert_eq!((m[0].added, m[0].removed), (1, 2));
        let codex = edit_stats(
            &json!([{"path":"a.rs","diff":"--- a\n+++ b\n@@\n-x\n+y\n+z\n"},{"path":"b.rs"}]),
            "a.rs, b.rs",
        );
        assert_eq!(codex.len(), 2);
        assert_eq!((codex[0].added, codex[0].removed), (2, 1));
        assert_eq!((codex[1].added, codex[1].removed), (0, 0));
        assert!(edit_stats(&json!(null), "Edit file").is_empty());
        assert_eq!(edit_stats(&json!(null), "src/x.rs")[0].path, "src/x.rs");
    }

    #[test]
    fn paths_become_project_relative_with_forward_slashes() {
        let roots = vec![
            PathBuf::from(r"C:\Users\me\proj"),
            PathBuf::from(r"C:\wt\proj-1"),
        ];
        assert_eq!(
            relative_path(r"C:\Users\me\proj\src\lib.rs", &roots),
            "src/lib.rs"
        );
        assert_eq!(relative_path("c:/users/ME/proj/a.rs", &roots), "a.rs");
        assert_eq!(relative_path(r"C:\wt\proj-1\x\y.rs", &roots), "x/y.rs");
        assert_eq!(relative_path(r".\src\a.rs", &roots), "src/a.rs");
        assert_eq!(
            relative_path(r"C:\Users\me\proj2\a.rs", &roots),
            "C:/Users/me/proj2/a.rs"
        );
    }

    #[test]
    fn tail_respects_char_boundaries() {
        assert_eq!(tail_bytes("hello", 10), "hello");
        let s = "ñ".repeat(10); // 20 bytes
        let t = tail_bytes(&s, 5);
        assert!(t.len() <= 5 && t.chars().all(|c| c == 'ñ'));
    }

    #[test]
    fn the_file_is_capped_and_keeps_status_and_tool_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1.events.jsonl");
        let mut log = EventLog::open_at(path.clone(), 2000);
        let big = "x".repeat(300);
        let mut dropped = 0;
        for i in 0..20 {
            if !log.append(&RunEvent::new(
                i,
                EventBody::MessageDelta { text: big.clone() },
            )) {
                dropped += 1;
            }
        }
        assert!(dropped > 0 && log.is_truncated());
        assert!(!log.append(&RunEvent::new(
            99,
            EventBody::ToolStart {
                id: "a".into(),
                kind: EventToolKind::Other,
                command: None,
                path: None,
                title: None
            }
        )));
        assert!(log.append(&RunEvent::new(
            100,
            EventBody::ToolEnd {
                id: "a".into(),
                exit_code: None,
                duration_ms: 1,
                output_tail: String::new(),
                ok: true
            }
        )));
        assert!(log.append(&RunEvent::new(
            101,
            EventBody::Status {
                status: "done".into()
            }
        )));
        let text = std::fs::read_to_string(&path).unwrap();
        let evs = RunEvent::parse_all(&text);
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e.body, EventBody::Truncated { .. }))
                .count(),
            1
        );
        assert!(matches!(evs.last().unwrap().body, EventBody::Status { .. }));
        assert!(text.lines().all(|l| RunEvent::parse_line(l).is_some()));
        // the cap held (one line of margin)
        assert!(log.bytes() < 2000 + 600, "{}", log.bytes());
    }

    fn state() -> ThreadState {
        ThreadState::new(
            chat_core::ThreadId(1),
            agents_driver::AgentKind::Codex,
            None,
            std::sync::Arc::new(agents_svc::PricingTable::embedded()),
            chat_core::CostPolicy::default(),
        )
    }

    fn delta(text: &str) -> agents_driver::AgentEvent {
        agents_driver::AgentEvent::Text {
            text: text.into(),
            delta: true,
        }
    }

    fn read(path: &Path) -> Vec<RunEvent> {
        RunEvent::parse_all(&std::fs::read_to_string(path).unwrap())
    }

    #[test]
    fn deltas_are_coalesced_cut_at_words_and_flushed_before_tools() {
        use agents_driver::{AgentEvent, AgentStatus, ToolKind};
        let dir = tempfile::tempdir().unwrap();
        let path = events_path(dir.path(), "7");
        let mut rec = Recorder::new(EventLog::open(dir.path(), "7"), &[Path::new("C:/p")]);
        let mut st = state();
        st.apply(AgentEvent::Status(AgentStatus::Working), 1);
        st.apply(delta("hello wor"), 2);
        rec.observe(&st);
        rec.commit();
        // first flush: the partial word waits
        assert_eq!(
            read(&path)
                .iter()
                .filter_map(|e| match &e.body {
                    EventBody::MessageDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["hello "]
        );
        // more text inside the interval: nothing written yet
        st.apply(delta("ld, more"), 3);
        rec.observe(&st);
        rec.commit();
        assert_eq!(read(&path).len(), 2); // status + the first delta
                                          // a tool call forces the pending text out, in order
        st.apply(
            AgentEvent::ToolCall {
                id: "t".into(),
                kind: ToolKind::Command,
                title: "ls".into(),
                input_json: serde_json::json!({"command":"ls"}),
            },
            4,
        );
        rec.observe(&st);
        rec.commit();
        let evs = read(&path);
        let texts: String = evs
            .iter()
            .filter_map(|e| match &e.body {
                EventBody::MessageDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, "hello world, more");
        assert!(matches!(
            evs.last().unwrap().body,
            EventBody::ToolStart { .. }
        ));
        // observing an unchanged state writes nothing
        let n = read(&path).len();
        rec.observe(&st);
        rec.commit();
        assert_eq!(read(&path).len(), n);
    }

    #[test]
    fn a_secret_split_between_flushes_is_still_hidden() {
        use agents_driver::{AgentEvent, AgentStatus};
        let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
        let dir = tempfile::tempdir().unwrap();
        let path = events_path(dir.path(), "8");
        let mut rec = Recorder::new(EventLog::open(dir.path(), "8"), &[]);
        rec.set_delta_interval(Duration::ZERO);
        let mut st = state();
        st.apply(AgentEvent::Status(AgentStatus::Working), 1);
        st.apply(delta(&format!("key {}", &secret[..20])), 2);
        rec.observe(&st);
        rec.commit();
        st.apply(delta(&format!("{} end", &secret[20..])), 3);
        rec.observe(&st);
        rec.commit();
        rec.finish("done");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("abcdefghijklmnop"), "{raw}");
    }
}
