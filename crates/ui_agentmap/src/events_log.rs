//! The per-run events log the harness appends while a child runs
//! (`<data dir>\agent-runs\<run id>.events.jsonl`, one JSON object per line) and a tolerant reader
//! for it.
//!
//! The harness owns the format and may add fields or event types at any time, so nothing here is a
//! strict `Deserialize`:
//!
//! * a line that is not JSON, not an object, or whose `type` is unknown is skipped;
//! * every field is read on its own (numbers may come as strings, ids as numbers) and a missing or
//!   ill-typed one stays `None`;
//! * the reader only consumes **complete** lines: a half-written last line is left for the next
//!   poll, and a last line without its newline is accepted only when it is already a whole JSON
//!   value;
//! * it is **incremental**: [`LogReader`] keeps the byte offset it has consumed and each
//!   [`poll`](LogReader::poll) reads only what was appended since. A file that shrank (rotation)
//!   restarts from the top and says so ([`LogBatch::reset`]);
//! * a first read of a big file starts at its last [`MAX_INITIAL_READ`] bytes.
//!
//! Lines (`t` is Unix milliseconds; a value below 1e11 is read as "ms since the run started"):
//!
//! ```text
//! {"t":1,"type":"message_delta","text":"Voy a revisar…"}
//! {"t":2,"type":"tool_start","id":"c1","kind":"command","command":"cargo test -p store"}
//! {"t":3,"type":"tool_end","id":"c1","exit_code":0,"duration_ms":4100,"output_tail":"ok"}
//! {"t":4,"type":"file_edit","path":"src/lib.rs","added":12,"removed":3}
//! {"t":5,"type":"approval","request":"r1","decision":"allowed","mode":"ask","reason":"…"}
//! {"t":6,"type":"status","status":"running"}
//! ```

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// A first read of a file bigger than this starts at its last `MAX_INITIAL_READ` bytes.
pub const MAX_INITIAL_READ: u64 = 1 << 20;
/// A line longer than this is skipped (it cannot be one of ours).
pub const MAX_LINE: usize = 256 * 1024;
/// Largest kept command / path / reason (head is kept).
const MAX_SHORT: usize = 2_000;
/// Largest kept message delta or command output (the tail is kept).
const MAX_LONG: usize = 16 * 1024;

/// What `<run id>.events.jsonl` can say. Every variant carries `t` (0 = unknown).
#[derive(Clone, Debug, PartialEq)]
pub enum LogEvent {
    /// A piece of the agent's answer.
    MessageDelta { t: i64, text: String },
    /// A tool began (`kind` is `command` for shell commands, `edit`, `read`...).
    ToolStart {
        t: i64,
        id: String,
        kind: String,
        command: Option<String>,
        path: Option<String>,
    },
    /// A tool ended.
    ToolEnd {
        t: i64,
        id: String,
        exit_code: Option<i64>,
        ok: Option<bool>,
        duration_ms: Option<i64>,
        output_tail: Option<String>,
    },
    /// A file was edited (`added` / `removed` lines).
    FileEdit {
        t: i64,
        path: String,
        added: Option<u32>,
        removed: Option<u32>,
    },
    /// An approval request or its decision (the same `request` appears more than once).
    Approval {
        t: i64,
        request: String,
        /// `pending | allowed | denied | auto | blocked | always`... as the harness spells it.
        decision: Option<String>,
        mode: Option<String>,
        reason: Option<String>,
        /// What was asked for (the command, the path), when the harness says.
        subject: Option<String>,
    },
    /// The harness' own status of the run (`running`, `waiting`, `done`...).
    Status {
        t: i64,
        status: String,
        detail: Option<String>,
    },
}

impl LogEvent {
    pub fn t(&self) -> i64 {
        match self {
            LogEvent::MessageDelta { t, .. }
            | LogEvent::ToolStart { t, .. }
            | LogEvent::ToolEnd { t, .. }
            | LogEvent::FileEdit { t, .. }
            | LogEvent::Approval { t, .. }
            | LogEvent::Status { t, .. } => *t,
        }
    }

    /// The same event with every free text passed through `store::redact` (the harness redacts
    /// before writing; this is the defense in depth the map adds).
    pub fn redacted(self) -> LogEvent {
        let r = |s: String| store::redact(&s).text;
        let ro = |s: Option<String>| s.map(|s| store::redact(&s).text);
        match self {
            LogEvent::MessageDelta { t, text } => LogEvent::MessageDelta { t, text: r(text) },
            LogEvent::ToolStart {
                t,
                id,
                kind,
                command,
                path,
            } => LogEvent::ToolStart {
                t,
                id,
                kind,
                command: ro(command),
                path,
            },
            LogEvent::ToolEnd {
                t,
                id,
                exit_code,
                ok,
                duration_ms,
                output_tail,
            } => LogEvent::ToolEnd {
                t,
                id,
                exit_code,
                ok,
                duration_ms,
                output_tail: ro(output_tail),
            },
            LogEvent::Approval {
                t,
                request,
                decision,
                mode,
                reason,
                subject,
            } => LogEvent::Approval {
                t,
                request,
                decision,
                mode,
                reason: ro(reason),
                subject: ro(subject),
            },
            other => other,
        }
    }
}

// ---- field readers ----------------------------------------------------------------------------

fn text_of(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        v.get(*k).and_then(|x| match x {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

fn int_of(v: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| {
        v.get(*k).and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
                .or_else(|| x.as_str().and_then(|t| t.trim().parse().ok()))
        })
    })
}

fn bool_of(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(Value::as_bool)
}

/// Keeps the head (`max` chars).
fn head(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    s.chars().take(max).collect()
}

/// Keeps the tail (`max` chars).
fn tail(s: String, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s;
    }
    s.chars().skip(n - max).collect()
}

fn nonblank(s: Option<String>) -> Option<String> {
    s.filter(|t| !t.trim().is_empty())
}

fn small(v: &Value, keys: &[&str]) -> Option<String> {
    nonblank(text_of(v, keys)).map(|s| head(s.trim().to_string(), MAX_SHORT))
}

fn count(v: &Value, keys: &[&str]) -> Option<u32> {
    int_of(v, keys).map(|n| n.clamp(0, i64::from(u32::MAX)) as u32)
}

/// Reads one event from a JSON value. `None` for a non-object, an unknown `type` or a line without
/// what the type needs (`id` of a tool, `path` of an edit).
pub fn parse_value(v: &Value) -> Option<LogEvent> {
    if !v.is_object() {
        return None;
    }
    let ty = v.get("type")?.as_str()?.trim().to_ascii_lowercase();
    let t = int_of(v, &["t", "ts", "time_ms"]).unwrap_or(0).max(0);
    match ty.as_str() {
        "message_delta" => {
            let text = text_of(v, &["text", "delta", "content"])?;
            if text.is_empty() {
                return None;
            }
            Some(LogEvent::MessageDelta {
                t,
                text: tail(text, MAX_LONG),
            })
        }
        "tool_start" => Some(LogEvent::ToolStart {
            t,
            id: nonblank(text_of(v, &["id", "tool_id"]))?,
            kind: small(v, &["kind"]).unwrap_or_default().to_ascii_lowercase(),
            command: small(v, &["command", "cmd"]),
            path: small(v, &["path", "file"]),
        }),
        "tool_end" => Some(LogEvent::ToolEnd {
            t,
            id: nonblank(text_of(v, &["id", "tool_id"]))?,
            exit_code: int_of(v, &["exit_code", "code"]),
            ok: bool_of(v, "ok"),
            duration_ms: int_of(v, &["duration_ms"]).map(|d| d.max(0)),
            output_tail: nonblank(text_of(v, &["output_tail", "output"]))
                .map(|s| tail(s, MAX_LONG)),
        }),
        "file_edit" => Some(LogEvent::FileEdit {
            t,
            path: small(v, &["path", "file"])?,
            added: count(v, &["added", "additions"]),
            removed: count(v, &["removed", "deletions"]),
        }),
        "approval" => Some(LogEvent::Approval {
            t,
            request: text_of(v, &["request", "request_id", "id"]).unwrap_or_default(),
            decision: small(v, &["decision", "state"]),
            mode: small(v, &["mode"]),
            reason: small(v, &["reason"]),
            subject: small(v, &["command", "detail", "path", "tool"]),
        }),
        "status" => Some(LogEvent::Status {
            t,
            status: small(v, &["status", "state"])?,
            detail: small(v, &["detail", "reason"]),
        }),
        _ => None,
    }
}

/// Reads one line. `None` for junk, unknown types and lines of other shapes.
pub fn parse_line(line: &str) -> Option<LogEvent> {
    let line = line.trim();
    if line.is_empty() || line.len() > MAX_LINE {
        return None;
    }
    parse_value(&serde_json::from_str::<Value>(line).ok()?)
}

/// Every complete event of `text` (used by tests and by the reader).
#[cfg(test)]
pub fn parse_text(text: &str) -> Vec<LogEvent> {
    text.lines().filter_map(parse_line).collect()
}

// ---- incremental reader -----------------------------------------------------------------------

/// What one [`LogReader::poll`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogBatch {
    pub events: Vec<LogEvent>,
    /// The file shrank (rotation): what was read before is gone, start the feed over.
    pub reset: bool,
}

/// Reads a log from the last offset it consumed.
#[derive(Clone, Debug)]
pub struct LogReader {
    path: PathBuf,
    offset: u64,
}

impl LogReader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes consumed so far (complete lines only).
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads what was appended since the last call. A missing file is an empty batch. Never blocks
    /// on anything but the read itself: call it off the UI thread.
    pub fn poll(&mut self) -> LogBatch {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return LogBatch::default();
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return LogBatch::default();
        };
        let mut batch = LogBatch::default();
        if len < self.offset {
            self.offset = 0;
            batch.reset = true;
        }
        if len == self.offset {
            return batch;
        }
        let mut start = self.offset;
        let mut skip_partial = false;
        if start == 0 && len > MAX_INITIAL_READ {
            start = len - MAX_INITIAL_READ;
            skip_partial = true;
        }
        if file.seek(SeekFrom::Start(start)).is_err() {
            return batch;
        }
        let mut data = Vec::with_capacity((len - start).min(MAX_INITIAL_READ) as usize);
        if file.take(len - start).read_to_end(&mut data).is_err() {
            return batch;
        }
        let mut base = start;
        if skip_partial {
            // We landed in the middle of a line: drop up to the first newline.
            match data.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    data.drain(..=i);
                    base += i as u64 + 1;
                }
                None => {
                    self.offset = base + data.len() as u64;
                    return batch;
                }
            }
        }
        let cut = data.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let mut consumed = cut;
        for line in data[..cut].split(|b| *b == b'\n') {
            if let Ok(s) = std::str::from_utf8(line) {
                batch.events.extend(parse_line(s));
            } else {
                batch
                    .events
                    .extend(parse_line(&String::from_utf8_lossy(line)));
            }
        }
        // A last line without its newline counts only once it is a whole JSON value.
        let rest = &data[cut..];
        if !rest.is_empty()
            && let Ok(s) = std::str::from_utf8(rest)
            && let Ok(v) = serde_json::from_str::<Value>(s.trim())
        {
            batch.events.extend(parse_value(&v));
            consumed = data.len();
        }
        self.offset = base + consumed as u64;
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(path: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn every_documented_type_parses() {
        let ev = parse_text(
            r#"{"t":1700000000000,"type":"message_delta","text":"hola"}
{"t":2,"type":"tool_start","id":"c1","kind":"Command","command":"cargo test -p store"}
{"t":3,"type":"tool_end","id":"c1","exit_code":101,"duration_ms":4100,"output_tail":"boom"}
{"t":4,"type":"file_edit","path":"src/lib.rs","added":12,"removed":3}
{"t":5,"type":"approval","request":"r1","decision":"allowed","mode":"ask","reason":"ok","command":"git status"}
{"t":6,"type":"status","status":"running"}
"#,
        );
        assert_eq!(ev.len(), 6);
        assert_eq!(
            ev[0],
            LogEvent::MessageDelta {
                t: 1_700_000_000_000,
                text: "hola".into()
            }
        );
        assert!(
            matches!(&ev[1], LogEvent::ToolStart { id, kind, command: Some(c), .. }
            if id == "c1" && kind == "command" && c == "cargo test -p store")
        );
        assert!(
            matches!(&ev[2], LogEvent::ToolEnd { exit_code: Some(101), duration_ms: Some(4100), output_tail: Some(o), .. } if o == "boom")
        );
        assert!(matches!(
            &ev[3],
            LogEvent::FileEdit {
                added: Some(12),
                removed: Some(3),
                ..
            }
        ));
        assert!(
            matches!(&ev[4], LogEvent::Approval { request, decision: Some(d), subject: Some(s), .. }
            if request == "r1" && d == "allowed" && s == "git status")
        );
        assert!(matches!(&ev[5], LogEvent::Status { status, .. } if status == "running"));
    }

    #[test]
    fn unknown_types_fields_and_junk_are_ignored() {
        let ev = parse_text(
            r#"{"t":1,"type":"future_thing","x":1}
not json at all
[1,2,3]
{"type":"message_delta"}
{"type":"tool_start"}
{"type":"file_edit"}
{"t":"7","type":"file_edit","path":"a.rs","added":"5","surprise":{"a":1},"removed":null}
{"type":"tool_end","id":9,"exit_code":"0","extra":true}

"#,
        );
        assert_eq!(ev.len(), 2);
        assert!(matches!(
            &ev[0],
            LogEvent::FileEdit {
                t: 7,
                added: Some(5),
                removed: None,
                ..
            }
        ));
        assert!(matches!(&ev[1], LogEvent::ToolEnd { id, exit_code: Some(0), .. } if id == "9"));
    }

    #[test]
    fn the_reader_is_incremental_and_ignores_a_partial_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("7.events.jsonl");
        let mut r = LogReader::new(&path);
        // missing file: nothing, no error
        assert_eq!(r.poll(), LogBatch::default());

        append(
            &path,
            "{\"t\":1,\"type\":\"message_delta\",\"text\":\"a\"}\n{\"t\":2,\"type\":\"mess",
        );
        let b = r.poll();
        assert_eq!(b.events.len(), 1, "the half line waits");
        let after_first = r.offset();
        assert!(after_first > 0);
        // nothing new: nothing read
        assert!(r.poll().events.is_empty());
        assert_eq!(r.offset(), after_first);

        append(
            &path,
            "age_delta\",\"text\":\"b\"}\n{\"t\":3,\"type\":\"status\",\"status\":\"done\"}\n",
        );
        let b = r.poll();
        assert_eq!(b.events.len(), 2);
        assert!(matches!(&b.events[0], LogEvent::MessageDelta { text, .. } if text == "b"));
        assert!(r.poll().events.is_empty());
    }

    #[test]
    fn a_last_line_without_newline_counts_only_when_it_is_whole_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1.events.jsonl");
        let mut r = LogReader::new(&path);
        append(
            &path,
            "{\"t\":1,\"type\":\"status\",\"status\":\"running\"}",
        );
        assert_eq!(r.poll().events.len(), 1);
        append(&path, "\n{\"t\":2,\"type\":\"status\",\"stat");
        assert!(r.poll().events.is_empty());
        append(&path, "us\":\"done\"}\n");
        assert_eq!(r.poll().events.len(), 1);
    }

    #[test]
    fn a_shrunk_file_restarts_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1.events.jsonl");
        let mut r = LogReader::new(&path);
        append(
            &path,
            "{\"t\":1,\"type\":\"status\",\"status\":\"running\"}\n{\"t\":2,\"type\":\"status\",\"status\":\"waiting\"}\n",
        );
        assert_eq!(r.poll().events.len(), 2);
        std::fs::write(&path, "{\"t\":9,\"type\":\"status\",\"status\":\"done\"}\n").unwrap();
        let b = r.poll();
        assert!(b.reset);
        assert_eq!(b.events.len(), 1);
    }

    #[test]
    fn a_huge_file_is_read_from_its_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1.events.jsonl");
        let line = "{\"t\":1,\"type\":\"message_delta\",\"text\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}\n";
        let n = (MAX_INITIAL_READ as usize / line.len()) * 2;
        append(&path, &line.repeat(n));
        let mut r = LogReader::new(&path);
        let b = r.poll();
        assert!(!b.events.is_empty());
        assert!(b.events.len() < n, "only the tail was parsed");
        assert!(r.poll().events.is_empty());
    }

    #[test]
    fn redaction_hides_secrets_in_free_text() {
        let ev = parse_line(
            r#"{"type":"tool_end","id":"1","output_tail":"token=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"}"#,
        )
        .unwrap()
        .redacted();
        let LogEvent::ToolEnd {
            output_tail: Some(o),
            ..
        } = ev
        else {
            panic!()
        };
        assert!(!o.contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{o}");
    }
}
