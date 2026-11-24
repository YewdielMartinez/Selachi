//! View-model of a conversation: `ThreadState` messages and blocks mapped to flat, owned rows
//! (one list item each), with per-row fingerprints so only rows that really changed are
//! re-measured by the list while the agent streams. Pure: no gpui.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use agents_driver::ToolKind;
use chat_core::state::touched_paths;
use chat_core::{
    ApprovalState, Block, Message, MessageCost, Role, ThreadState, ThreadStatus, ToolClass,
    ToolRun, ToolStatus,
};
use serde_json::Value;

use crate::md::MdBody;

/// Identity of a row, stable while the conversation grows (used for expand state and diffing).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowKey {
    User(u32),
    Checkpoint(u32),
    /// `(message seq, block index)`.
    Block(u32, u16),
    Footer(u32),
    System(u32),
    Working,
    Error,
}

/// One line of an "Explored" group.
#[derive(Clone, Debug, PartialEq)]
pub struct ExploreItem {
    pub class: ToolClass,
    pub label: String,
    pub running: bool,
}

#[derive(Clone, Debug)]
pub enum RowKind {
    User {
        text: String,
        attachments: Vec<PathBuf>,
    },
    Checkpoint {
        id: String,
        label: String,
    },
    Text {
        body: MdBody,
        streaming: bool,
    },
    Thinking {
        text: String,
        streaming: bool,
    },
    Explored {
        label: String,
        items: Vec<ExploreItem>,
        running: bool,
    },
    Command {
        cmd: String,
        status: ToolStatus,
        duration_ms: Option<i64>,
        output: Option<String>,
    },
    Edit {
        verb: &'static str,
        path: Option<String>,
        title: String,
        adds: Option<usize>,
        dels: Option<usize>,
        status: ToolStatus,
        summary: Option<String>,
    },
    Tool {
        title: String,
        class: ToolClass,
        status: ToolStatus,
        duration_ms: Option<i64>,
        summary: Option<String>,
    },
    Approval {
        request_id: String,
        tool: String,
        detail: String,
        state: ApprovalState,
    },
    System {
        text: String,
    },
    Footer {
        cost: Option<MessageCost>,
        tokens: u64,
    },
    Working,
    Error {
        text: String,
    },
}

impl RowKind {
    /// Whether clicking the row expands something.
    pub fn is_expandable(&self) -> bool {
        match self {
            RowKind::Thinking { text, .. } => !text.trim().is_empty(),
            RowKind::Explored { items, .. } => !items.is_empty(),
            RowKind::Command { .. } => true,
            RowKind::Tool { summary, .. } => summary.as_ref().is_some_and(|s| !s.trim().is_empty()),
            RowKind::Edit { summary, .. } => summary.as_ref().is_some_and(|s| !s.trim().is_empty()),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChatRow {
    pub key: RowKey,
    /// Fingerprint of everything the row displays.
    pub fp: u64,
    pub kind: RowKind,
}

impl ChatRow {
    fn new(key: RowKey, fp: u64, kind: RowKind) -> Rc<Self> {
        Rc::new(Self { key, fp, kind })
    }
}

// ---------------------------------------------------------------------------------------------
// Tool input helpers
// ---------------------------------------------------------------------------------------------

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

/// The command line of a command run: `input.command` (string or argv array), else its title.
pub fn command_text(run: &ToolRun) -> String {
    if let Some(s) = str_of(&run.input, &["command", "cmd"]) {
        return s.to_string();
    }
    for k in ["command", "cmd"] {
        if let Some(Value::Array(a)) = run.input.get(k) {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            if !parts.is_empty() {
                return parts.join(" ");
            }
        }
    }
    run.title.clone()
}

fn line_count(s: &str) -> usize {
    if s.is_empty() { 0 } else { s.lines().count() }
}

/// `(added, removed)` lines between two texts, ignoring the unchanged head and tail.
fn replace_counts(old: &str, new: &str) -> (usize, usize) {
    let o: Vec<&str> = old.lines().collect();
    let n: Vec<&str> = new.lines().collect();
    let head = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let tail = o[head..]
        .iter()
        .rev()
        .zip(n[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (n.len() - head - tail, o.len() - head - tail)
}

fn unified_counts(diff: &str) -> (usize, usize) {
    let mut adds = 0;
    let mut dels = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            adds += 1;
        } else if l.starts_with('-') {
            dels += 1;
        }
    }
    (adds, dels)
}

fn counts_of(v: &Value, depth: usize) -> Option<(usize, usize)> {
    if depth > 3 {
        return None;
    }
    match v {
        Value::Object(o) => {
            if let (Some(Value::String(old)), Some(Value::String(new))) =
                (o.get("old_string"), o.get("new_string"))
            {
                return Some(replace_counts(old, new));
            }
            if let Some(Value::String(c)) = o.get("content") {
                return Some((line_count(c), 0));
            }
            for k in ["diff", "unified_diff"] {
                if let Some(Value::String(d)) = o.get(k) {
                    return Some(unified_counts(d));
                }
            }
            let mut total: Option<(usize, usize)> = None;
            for k in ["edits", "changes", "files"] {
                if let Some(x) = o.get(k)
                    && let Some((a, d)) = counts_of(x, depth + 1)
                {
                    let t = total.get_or_insert((0, 0));
                    t.0 += a;
                    t.1 += d;
                }
            }
            total
        }
        Value::Array(a) => {
            let mut total: Option<(usize, usize)> = None;
            for x in a {
                if let Some((ad, de)) = counts_of(x, depth + 1) {
                    let t = total.get_or_insert((0, 0));
                    t.0 += ad;
                    t.1 += de;
                }
            }
            total
        }
        _ => None,
    }
}

/// `(+added, -removed)` of an edit, when the tool input says enough to count.
pub fn diff_counts(run: &ToolRun) -> Option<(usize, usize)> {
    counts_of(&run.input, 0)
}

fn explore_item(run: &ToolRun) -> ExploreItem {
    let verb = match run.class {
        ToolClass::List => "Listed",
        ToolClass::Search => "Searched",
        _ => "Read",
    };
    let subject = if run.title.trim().is_empty() {
        str_of(
            &run.input,
            &["path", "file_path", "pattern", "query", "directory"],
        )
        .unwrap_or("")
        .to_string()
    } else {
        run.title.clone()
    };
    ExploreItem {
        class: run.class,
        label: format!("{verb} {subject}").trim().to_string(),
        running: run.is_running(),
    }
}

// ---------------------------------------------------------------------------------------------
// Fingerprints
// ---------------------------------------------------------------------------------------------

fn hash_text(h: &mut DefaultHasher, text: &str) {
    text.len().hash(h);
    let b = text.as_bytes();
    b[b.len().saturating_sub(64)..].hash(h);
}

fn hash_run(h: &mut DefaultHasher, r: &ToolRun) {
    r.id.hash(h);
    r.status.hash(h);
    r.finished_ms.hash(h);
    r.title.len().hash(h);
    r.summary.as_ref().map(String::len).hash(h);
}

fn block_fp(b: &Block, streaming_tail: bool) -> u64 {
    let mut h = DefaultHasher::new();
    streaming_tail.hash(&mut h);
    match b {
        Block::Text { text } => {
            0u8.hash(&mut h);
            hash_text(&mut h, text);
        }
        Block::Thinking(t) => {
            1u8.hash(&mut h);
            hash_text(&mut h, &t.text);
        }
        Block::Explored(g) => {
            2u8.hash(&mut h);
            g.runs.len().hash(&mut h);
            for r in &g.runs {
                hash_run(&mut h, r);
            }
        }
        Block::Command(r) => {
            3u8.hash(&mut h);
            hash_run(&mut h, r);
        }
        Block::Tool(r) => {
            4u8.hash(&mut h);
            hash_run(&mut h, r);
        }
        Block::Approval(a) => {
            5u8.hash(&mut h);
            a.request_id.hash(&mut h);
            a.state.hash(&mut h);
        }
    }
    h.finish()
}

fn message_fp(m: &Message) -> u64 {
    let mut h = DefaultHasher::new();
    (m.role as u8).hash(&mut h);
    m.streaming.hash(&mut h);
    m.blocks.len().hash(&mut h);
    m.attachments.len().hash(&mut h);
    m.checkpoint.as_ref().map(|c| c.id.clone()).hash(&mut h);
    m.cost
        .map(|c| (c.usd.to_bits(), c.source as u8))
        .hash(&mut h);
    m.usage.map(|u| u.total()).hash(&mut h);
    for (i, b) in m.blocks.iter().enumerate() {
        block_fp(b, m.streaming && i + 1 == m.blocks.len()).hash(&mut h);
    }
    h.finish()
}

// ---------------------------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------------------------

fn run_row(seq: u32, ix: u16, fp: u64, block: &Block) -> Option<Rc<ChatRow>> {
    let key = RowKey::Block(seq, ix);
    match block {
        Block::Command(r) => Some(ChatRow::new(
            key,
            fp,
            RowKind::Command {
                cmd: command_text(r),
                status: r.status,
                duration_ms: r.duration_ms(),
                output: r.summary.clone().filter(|s| !s.trim().is_empty()),
            },
        )),
        Block::Tool(r) if r.class == ToolClass::Edit => {
            let path = touched_paths(r).into_iter().next();
            let (adds, dels) = match diff_counts(r) {
                Some((a, d)) => (Some(a), Some(d)),
                None => (None, None),
            };
            Some(ChatRow::new(
                key,
                fp,
                RowKind::Edit {
                    verb: if r.kind == ToolKind::Write {
                        "Wrote"
                    } else {
                        "Edited"
                    },
                    path,
                    title: r.title.clone(),
                    adds,
                    dels,
                    status: r.status,
                    summary: r.summary.clone().filter(|s| !s.trim().is_empty()),
                },
            ))
        }
        Block::Tool(r) => Some(ChatRow::new(
            key,
            fp,
            RowKind::Tool {
                title: r.title.clone(),
                class: r.class,
                status: r.status,
                duration_ms: r.duration_ms(),
                summary: r.summary.clone().filter(|s| !s.trim().is_empty()),
            },
        )),
        _ => None,
    }
}

fn message_rows(m: &Message) -> Vec<Rc<ChatRow>> {
    let seq = m.seq;
    let mut out = Vec::new();
    match m.role {
        Role::User => {
            if let Some(cp) = &m.checkpoint {
                let mut h = DefaultHasher::new();
                cp.id.hash(&mut h);
                out.push(ChatRow::new(
                    RowKey::Checkpoint(seq),
                    h.finish(),
                    RowKind::Checkpoint {
                        id: cp.id.clone(),
                        label: cp.label.clone(),
                    },
                ));
            }
            let text = m.plain_text();
            let mut h = DefaultHasher::new();
            hash_text(&mut h, &text);
            m.attachments.len().hash(&mut h);
            out.push(ChatRow::new(
                RowKey::User(seq),
                h.finish(),
                RowKind::User {
                    text,
                    attachments: m.attachments.clone(),
                },
            ));
        }
        Role::System => {
            let text = m.plain_text();
            let mut h = DefaultHasher::new();
            hash_text(&mut h, &text);
            out.push(ChatRow::new(
                RowKey::System(seq),
                h.finish(),
                RowKind::System { text },
            ));
        }
        Role::Assistant => {
            let n = m.blocks.len();
            for (i, b) in m.blocks.iter().enumerate() {
                let ix = i as u16;
                let tail = m.streaming && i + 1 == n;
                let fp = block_fp(b, tail);
                let row = match b {
                    Block::Text { text } if !text.trim().is_empty() => Some(ChatRow::new(
                        RowKey::Block(seq, ix),
                        fp,
                        RowKind::Text {
                            body: MdBody::new(text.clone()),
                            streaming: tail,
                        },
                    )),
                    Block::Text { .. } => None,
                    Block::Thinking(t) => Some(ChatRow::new(
                        RowKey::Block(seq, ix),
                        fp,
                        RowKind::Thinking {
                            text: t.text.clone(),
                            streaming: tail,
                        },
                    )),
                    Block::Explored(g) => Some(ChatRow::new(
                        RowKey::Block(seq, ix),
                        fp,
                        RowKind::Explored {
                            label: g.label(),
                            items: g.runs.iter().map(explore_item).collect(),
                            running: g.is_running(),
                        },
                    )),
                    Block::Command(_) | Block::Tool(_) => run_row(seq, ix, fp, b),
                    Block::Approval(a) => Some(ChatRow::new(
                        RowKey::Block(seq, ix),
                        fp,
                        RowKind::Approval {
                            request_id: a.request_id.clone(),
                            tool: a.tool.clone(),
                            detail: a.detail.clone(),
                            state: a.state,
                        },
                    )),
                };
                out.extend(row);
            }
            if !m.streaming && (m.cost.is_some() || m.usage.is_some()) {
                let tokens = m.usage.map_or(0, |u| u.total());
                let mut h = DefaultHasher::new();
                m.cost
                    .map(|c| (c.usd.to_bits(), c.source as u8))
                    .hash(&mut h);
                tokens.hash(&mut h);
                out.push(ChatRow::new(
                    RowKey::Footer(seq),
                    h.finish(),
                    RowKind::Footer {
                        cost: m.cost,
                        tokens,
                    },
                ));
            }
        }
    }
    out
}

/// Rows already built per message, keyed by `seq` with the message fingerprint they came from.
#[derive(Default)]
pub struct RowCache {
    msgs: HashMap<u32, (u64, Vec<Rc<ChatRow>>)>,
}

/// The whole conversation as rows: one list item each. Unchanged messages come from `cache`.
pub fn build_rows(st: &ThreadState, cache: &mut RowCache) -> Vec<Rc<ChatRow>> {
    let mut out = Vec::new();
    for m in &st.messages {
        let fp = message_fp(m);
        match cache.msgs.get(&m.seq) {
            Some((cached, rows)) if *cached == fp => out.extend(rows.iter().cloned()),
            _ => {
                let rows = message_rows(m);
                out.extend(rows.iter().cloned());
                cache.msgs.insert(m.seq, (fp, rows));
            }
        }
    }
    let count = st.messages.len() as u32;
    cache.msgs.retain(|seq, _| *seq < count);
    match st.status {
        ThreadStatus::Error => {
            if let Some(e) = st.error.as_ref().filter(|e| !e.trim().is_empty()) {
                let mut h = DefaultHasher::new();
                e.hash(&mut h);
                out.push(ChatRow::new(
                    RowKey::Error,
                    h.finish(),
                    RowKind::Error { text: e.clone() },
                ));
            }
        }
        ThreadStatus::Working => out.push(ChatRow::new(RowKey::Working, 0, RowKind::Working)),
        _ => {}
    }
    out
}

/// What the list has to be told after the rows changed: replace `old` items by `new_len`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub old: Range<usize>,
    pub new_len: usize,
}

/// Smallest splice turning `old` into `new` (common head and tail are kept), `None` if equal.
/// Rows are compared by key and fingerprint; any `PartialEq` item works (the thread list uses it).
pub fn diff_rows<T: PartialEq>(old: &[T], new: &[T]) -> Option<Splice> {
    if old == new {
        return None;
    }
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    Some(Splice {
        old: head..old.len() - tail,
        new_len: new.len() - head - tail,
    })
}

/// `(key, fp)` pairs of rows, the input of [`diff_rows`].
pub fn keys_of(rows: &[Rc<ChatRow>]) -> Vec<(RowKey, u64)> {
    rows.iter().map(|r| (r.key.clone(), r.fp)).collect()
}

/// Whether a click on the "ask" row buttons still means something.
pub fn approval_is_pending(state: ApprovalState) -> bool {
    state == ApprovalState::Pending
}

/// Spanish word for an answered approval.
pub fn approval_outcome(state: ApprovalState) -> &'static str {
    match state {
        ApprovalState::Pending => "Esperando permiso",
        ApprovalState::Allowed => "Permitido",
        ApprovalState::Denied => "Rechazado",
        ApprovalState::AlwaysAllowed => "Permitido siempre",
        ApprovalState::Expired => "Sin respuesta",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents_driver::{AgentEvent, AgentKind, AgentStatus, ToolKind};
    use chat_core::{CostPolicy, ThreadId};
    use serde_json::json;
    use std::sync::Arc;

    fn state() -> ThreadState {
        ThreadState::new(
            ThreadId(1),
            AgentKind::ClaudeCode,
            Some("opus".into()),
            Arc::new(agents_svc::PricingTable::embedded()),
            CostPolicy::default(),
        )
    }

    fn call(id: &str, kind: ToolKind, title: &str, input: Value) -> AgentEvent {
        AgentEvent::ToolCall {
            id: id.into(),
            kind,
            title: title.into(),
            input_json: input,
        }
    }

    fn kinds(rows: &[Rc<ChatRow>]) -> Vec<&'static str> {
        rows.iter()
            .map(|r| match r.kind {
                RowKind::User { .. } => "user",
                RowKind::Checkpoint { .. } => "checkpoint",
                RowKind::Text { .. } => "text",
                RowKind::Thinking { .. } => "thinking",
                RowKind::Explored { .. } => "explored",
                RowKind::Command { .. } => "command",
                RowKind::Edit { .. } => "edit",
                RowKind::Tool { .. } => "tool",
                RowKind::Approval { .. } => "approval",
                RowKind::System { .. } => "system",
                RowKind::Footer { .. } => "footer",
                RowKind::Working => "working",
                RowKind::Error { .. } => "error",
            })
            .collect()
    }

    #[test]
    fn maps_a_whole_turn_to_rows() {
        let mut s = state();
        s.begin_user_message("arregla el parser", vec![], None, 1);
        s.apply(
            AgentEvent::Thinking {
                text: "hmm".into(),
                delta: false,
            },
            2,
        );
        s.apply(call("a", ToolKind::Other("LS".into()), "src", json!({})), 3);
        s.apply(call("b", ToolKind::Search, "fn parse", json!({})), 4);
        s.apply(
            AgentEvent::Text {
                text: "Voy a editar.".into(),
                delta: false,
            },
            5,
        );
        s.apply(
            call(
                "c",
                ToolKind::Command,
                "cargo test",
                json!({"command": "cargo test -p parser"}),
            ),
            6,
        );
        s.apply(
            AgentEvent::ToolResult {
                id: "c".into(),
                ok: true,
                summary: "ok".into(),
            },
            3006,
        );
        s.apply(
            call("d", ToolKind::Edit, "src/lib.rs", json!({"file_path": "src/lib.rs", "old_string": "a\nb\nc", "new_string": "a\nB\nC\nD"})),
            3007,
        );
        s.apply(
            AgentEvent::ApprovalRequest {
                id: "r1".into(),
                tool: "Bash".into(),
                detail: "rm x".into(),
            },
            3008,
        );
        let rows = build_rows(&s, &mut RowCache::default());
        assert_eq!(
            kinds(&rows),
            vec![
                "user", "thinking", "explored", "text", "command", "edit", "approval", "working"
            ]
        );
        let RowKind::Explored { label, items, .. } = &rows[2].kind else {
            panic!()
        };
        assert_eq!(label, "Explored · 1 list · 1 search");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].label, "Searched fn parse");
        let RowKind::Command {
            cmd,
            status,
            duration_ms,
            ..
        } = &rows[4].kind
        else {
            panic!()
        };
        assert_eq!(cmd, "cargo test -p parser");
        assert_eq!(*status, ToolStatus::Ok);
        assert_eq!(*duration_ms, Some(3000));
        let RowKind::Edit {
            path,
            adds,
            dels,
            verb,
            ..
        } = &rows[5].kind
        else {
            panic!()
        };
        assert_eq!(path.as_deref(), Some("src/lib.rs"));
        assert_eq!((*adds, *dels), (Some(3), Some(2)));
        assert_eq!(*verb, "Edited");
        let RowKind::Approval {
            request_id, state, ..
        } = &rows[6].kind
        else {
            panic!()
        };
        assert_eq!(request_id, "r1");
        assert_eq!(*state, ApprovalState::Pending);
    }

    #[test]
    fn finished_turn_gets_a_footer_with_cost_and_no_working_row() {
        let mut s = state();
        s.begin_user_message("hola", vec![], None, 1);
        s.apply(
            AgentEvent::Text {
                text: "hola!".into(),
                delta: false,
            },
            2,
        );
        s.apply(
            AgentEvent::Usage {
                input: 100,
                output: 50,
                cache_create: 0,
                cache_read: 0,
                cost_usd: Some(0.12),
                context_window: Some(1_000_000),
            },
            3,
        );
        s.apply(AgentEvent::Status(AgentStatus::Done), 4);
        let rows = build_rows(&s, &mut RowCache::default());
        assert_eq!(kinds(&rows), vec!["user", "text", "footer"]);
        let RowKind::Footer { cost, tokens } = &rows[2].kind else {
            panic!()
        };
        assert_eq!(*tokens, 150);
        let c = cost.expect("cost");
        assert!((c.usd - 0.12).abs() < 1e-9);
        assert_eq!(c.source, chat_core::CostSource::Reported);
    }

    #[test]
    fn checkpoint_row_precedes_its_user_message() {
        let mut s = state();
        s.begin_user_message(
            "hazlo",
            vec![PathBuf::from("a.txt")],
            Some(chat_core::CheckpointRef {
                id: "cp1".into(),
                label: "antes del turno 1".into(),
            }),
            1,
        );
        let rows = build_rows(&s, &mut RowCache::default());
        assert_eq!(&kinds(&rows)[..2], &["checkpoint", "user"]);
        let RowKind::Checkpoint { id, label } = &rows[0].kind else {
            panic!()
        };
        assert_eq!((id.as_str(), label.as_str()), ("cp1", "antes del turno 1"));
        let RowKind::User { attachments, .. } = &rows[1].kind else {
            panic!()
        };
        assert_eq!(attachments.len(), 1);
    }

    #[test]
    fn streaming_text_changes_only_the_last_row() {
        let mut s = state();
        let mut cache = RowCache::default();
        s.begin_user_message("hola", vec![], None, 1);
        s.apply(
            AgentEvent::Text {
                text: "Hola".into(),
                delta: true,
            },
            2,
        );
        let a = build_rows(&s, &mut cache);
        s.apply(
            AgentEvent::Text {
                text: " mundo".into(),
                delta: true,
            },
            3,
        );
        let b = build_rows(&s, &mut cache);
        let (ka, kb) = (keys_of(&a), keys_of(&b));
        let sp = diff_rows(&ka, &kb).expect("changed");
        // user row kept; the text row is replaced by one row; the working row stays
        assert_eq!(sp.old.start, 1);
        assert_eq!(sp.new_len, sp.old.len());
        assert!(sp.old.len() <= 1, "{sp:?}");
        let RowKind::Text { streaming, .. } = &b[1].kind else {
            panic!()
        };
        assert!(*streaming);
    }

    #[test]
    fn unchanged_messages_are_reused_from_the_cache() {
        let mut s = state();
        let mut cache = RowCache::default();
        s.begin_user_message("hola", vec![], None, 1);
        s.apply(
            AgentEvent::Text {
                text: "uno".into(),
                delta: false,
            },
            2,
        );
        s.apply(AgentEvent::Status(AgentStatus::Done), 3);
        let a = build_rows(&s, &mut cache);
        let b = build_rows(&s, &mut cache);
        assert!(Rc::ptr_eq(&a[0], &b[0]));
        assert!(Rc::ptr_eq(&a[1], &b[1]));
        assert_eq!(diff_rows(&keys_of(&a), &keys_of(&b)), None);
    }

    #[test]
    fn diff_rows_keeps_head_and_tail() {
        let k = |i: u32, fp: u64| (RowKey::User(i), fp);
        let old = vec![k(0, 1), k(1, 1), k(2, 1), k(3, 1)];
        let new = vec![k(0, 1), k(1, 9), k(1, 8), k(3, 1)];
        // k(2,1) differs from k(1,8) so the middle is replaced
        let sp = diff_rows(&old, &new).unwrap();
        assert_eq!(
            sp,
            Splice {
                old: 1..3,
                new_len: 2
            }
        );
        assert_eq!(diff_rows(&old, &old), None);
        let grown = vec![k(0, 1), k(1, 1), k(2, 1), k(3, 1), k(4, 1)];
        assert_eq!(
            diff_rows(&old, &grown),
            Some(Splice {
                old: 4..4,
                new_len: 1
            })
        );
        assert_eq!(
            diff_rows(&grown, &old),
            Some(Splice {
                old: 4..5,
                new_len: 0
            })
        );
    }

    #[test]
    fn diff_counts_understand_the_known_tools() {
        let run = |input: Value| ToolRun {
            id: "x".into(),
            kind: ToolKind::Edit,
            class: ToolClass::Edit,
            title: "t".into(),
            input,
            status: ToolStatus::Ok,
            summary: None,
            started_ms: 0,
            finished_ms: Some(1),
        };
        assert_eq!(
            diff_counts(&run(json!({"content": "a\nb\nc\n"}))),
            Some((3, 0))
        );
        assert_eq!(
            diff_counts(&run(json!({"old_string": "x", "new_string": "x"}))),
            Some((0, 0))
        );
        assert_eq!(
            diff_counts(&run(
                json!({"edits": [{"old_string": "a", "new_string": "b"}, {"old_string": "", "new_string": "c\nd"}]})
            )),
            Some((3, 1))
        );
        assert_eq!(
            diff_counts(&run(
                json!({"changes": [{"path": "a", "diff": "--- a\n+++ a\n@@\n-x\n+y\n+z"}]})
            )),
            Some((2, 1))
        );
        assert_eq!(diff_counts(&run(json!({"file_path": "a"}))), None);
    }

    #[test]
    fn error_state_shows_an_error_row() {
        let mut s = state();
        s.begin_user_message("x", vec![], None, 1);
        s.apply(AgentEvent::Status(AgentStatus::Error("sin red".into())), 2);
        let rows = build_rows(&s, &mut RowCache::default());
        assert_eq!(kinds(&rows).last(), Some(&"error"));
    }

    #[test]
    fn expandable_rows() {
        assert!(
            RowKind::Command {
                cmd: "x".into(),
                status: ToolStatus::Ok,
                duration_ms: None,
                output: None
            }
            .is_expandable()
        );
        assert!(!RowKind::Working.is_expandable());
        assert!(
            !RowKind::Thinking {
                text: " ".into(),
                streaming: true
            }
            .is_expandable()
        );
        assert!(
            RowKind::Thinking {
                text: "x".into(),
                streaming: false
            }
            .is_expandable()
        );
    }
}
