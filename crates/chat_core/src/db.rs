//! `chat.db`: SQLite persistence of threads, messages, tool runs, approvals and plans.
//!
//! Same pattern as `store::StateDb`: WAL, versioned migrations (`PRAGMA user_version`), **one
//! writer thread** owning the only write connection (ops are queued and committed in batches of
//! up to 1024 / 20 ms), and a separate read-only connection for queries. Hot-path writes
//! (messages, status, approvals) return as soon as they are queued; management writes (create,
//! rename, delete) block until committed. Call [`ChatDb::flush`] for read-your-writes after a
//! fire-and-forget write.
//!
//! No secrets are stored: [`crate::state::message_write`] redacts everything before it gets
//! here, and tool inputs are capped at 4 KB.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{ChatError, Result};
use crate::model::{
    agent_from_db, agent_to_db, now_ms, permission_from_db, permission_to_db, Role, ThreadId,
    ThreadLink, ThreadRecord, ThreadSpec, ThreadStatus,
};
use crate::state::{ApprovalWrite, MessageWrite};

const BATCH_WINDOW: Duration = Duration::from_millis(20);
const MAX_BATCH: usize = 1024;

/// Append-only. Never edit a released entry; add a new one.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
CREATE TABLE threads(
    id                     INTEGER PRIMARY KEY,
    title                  TEXT    NOT NULL DEFAULT '',
    project_root           TEXT    NOT NULL,
    worktree_path          TEXT,
    agent_kind             TEXT    NOT NULL,      -- claude | codex | opencode | acp:<command>
    model                  TEXT,
    permission_mode        TEXT    NOT NULL DEFAULT 'ask',  -- ask | accept_edits | full | read_only
    status                 TEXT    NOT NULL DEFAULT 'idle', -- idle | working | waiting | done | error
    created                INTEGER NOT NULL,      -- unix ms
    updated                INTEGER NOT NULL,      -- unix ms
    agent_session_id       TEXT,
    archived               INTEGER NOT NULL DEFAULT 0,
    checkpoint_before_turn INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_threads_updated ON threads(archived, updated DESC);

CREATE TABLE messages(
    id           INTEGER PRIMARY KEY,
    thread_id    INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    role         TEXT    NOT NULL CHECK (role IN ('user','assistant','system')),
    created      INTEGER NOT NULL,
    content_json TEXT    NOT NULL,                -- {blocks:[...], attachments?, checkpoint?}
    usage_json   TEXT,                            -- {input,output,cache_create,cache_read}
    cost_usd     REAL,
    cost_source  TEXT,                            -- reported | estimated
    UNIQUE(thread_id, seq)
);

CREATE TABLE tool_runs(
    id             INTEGER PRIMARY KEY,
    thread_id      INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    message_id     INTEGER REFERENCES messages(id) ON DELETE CASCADE,
    tool_id        TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    title          TEXT    NOT NULL,
    input_json     TEXT    NOT NULL,              -- capped at 4 KB, redacted
    result_summary TEXT,
    ok             INTEGER,                       -- 1 ok, 0 failed, NULL running / cancelled
    started        INTEGER NOT NULL,
    finished       INTEGER
);
CREATE INDEX idx_tool_runs_thread ON tool_runs(thread_id);
CREATE INDEX idx_tool_runs_message ON tool_runs(message_id);

CREATE TABLE approvals(
    id         INTEGER PRIMARY KEY,
    thread_id  INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    request_id TEXT    NOT NULL,
    tool       TEXT    NOT NULL,
    detail     TEXT    NOT NULL,
    decision   TEXT,                              -- allow | deny | always | expired; NULL = pending
    decided_at INTEGER
);
CREATE INDEX idx_approvals_thread ON approvals(thread_id, request_id);

CREATE TABLE plans(
    thread_id INTEGER PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    json      TEXT NOT NULL
);
"#,
    // v2: reasoning effort per thread
    "ALTER TABLE threads ADD COLUMN effort TEXT;",
    // v3: delegation lineage (PLAN section 20). A delegated thread has `delegated_by` set; its
    // parent is a chat thread (`parent_thread_id`) and/or a terminal tab (`parent_tab`).
    r#"
ALTER TABLE threads ADD COLUMN parent_thread_id INTEGER REFERENCES threads(id) ON DELETE SET NULL;
ALTER TABLE threads ADD COLUMN parent_tab TEXT;
ALTER TABLE threads ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
ALTER TABLE threads ADD COLUMN delegated_by TEXT;
CREATE INDEX idx_threads_parent ON threads(parent_thread_id);
"#,
];

/// Latest schema version this build knows.
pub fn latest_version() -> u32 {
    MIGRATIONS.len() as u32
}

/// Apply pending migrations; returns the resulting version. Idempotent.
pub fn migrate(conn: &mut Connection) -> Result<u32> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > latest_version() {
        return Err(ChatError::NewerSchema {
            found: current,
            supported: latest_version(),
        });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(latest_version())
}

// ---------------------------------------------------------------------------
// Stored rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub seq: u32,
    pub role: Role,
    pub created_ms: i64,
    pub content_json: String,
    pub usage_json: Option<String>,
    pub cost_usd: Option<f64>,
    pub cost_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredToolRun {
    pub message_seq: Option<u32>,
    pub tool_id: String,
    pub kind: String,
    pub title: String,
    pub input_json: String,
    pub result_summary: Option<String>,
    pub ok: Option<bool>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredApproval {
    pub request_id: String,
    pub tool: String,
    pub detail: String,
    pub decision: Option<String>,
    pub decided_ms: Option<i64>,
}

/// Fields of a new thread row.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub spec: ThreadSpec,
    pub title: String,
    pub worktree_path: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// The database
// ---------------------------------------------------------------------------

type Reply = Box<dyn FnOnce(std::result::Result<(), String>) + Send>;

struct Op {
    run: Box<dyn FnOnce(&Connection) -> Reply + Send>,
    /// Someone is waiting: close the batch right after this op.
    urgent: bool,
}

struct Inner {
    tx: Option<Sender<Op>>,
    thread: Option<JoinHandle<()>>,
    reader: Mutex<Connection>,
    path: PathBuf,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Closing the channel lets the writer drain what is queued and exit.
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Cheaply cloneable handle to `chat.db`.
#[derive(Clone)]
pub struct ChatDb {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ChatDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatDb")
            .field("path", &self.inner.path)
            .finish()
    }
}

/// `%APPDATA%\Selachi\chat.db` (honours `SELACHI_DATA_DIR`), parent directory created.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(store::paths::app_data_dir()?.join("chat.db"))
}

impl ChatDb {
    /// Open (creating and migrating if needed) the database at `path`. Threads that were busy
    /// when the previous process died are reset to `idle`.
    pub fn open(path: &Path) -> Result<ChatDb> {
        Self::open_with(path, true)
    }

    /// [`ChatDb::open`] that can leave the `working` / `waiting` status of threads alone
    /// (`reset_busy = false`). A process that shares `chat.db` with the running app (the agent
    /// harness CLI) must not reset threads the app is still running.
    pub fn open_with(path: &Path, reset_busy: bool) -> Result<ChatDb> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let mut writer = Connection::open(path)?;
        writer.busy_timeout(Duration::from_secs(5))?;
        writer.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        writer.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut writer)?;
        if reset_busy {
            writer.execute(
                "UPDATE threads SET status = 'idle' WHERE status IN ('working', 'waiting')",
                [],
            )?;
        }

        let reader = Connection::open(path)?;
        reader.busy_timeout(Duration::from_secs(5))?;
        reader.pragma_update(None, "query_only", "ON")?;

        let (tx, rx) = channel::<Op>();
        let thread = std::thread::Builder::new()
            .name("selachi-chat-writer".into())
            .spawn(move || writer_loop(writer, rx))?;
        Ok(ChatDb {
            inner: Arc::new(Inner {
                tx: Some(tx),
                thread: Some(thread),
                reader: Mutex::new(reader),
                path: path.to_path_buf(),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Block until everything queued before this call is committed.
    pub fn flush(&self) -> Result<()> {
        self.exec_sync(|_| Ok(()))
    }

    pub fn schema_version(&self) -> Result<u32> {
        self.read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard: MutexGuard<'_, Connection> = match self.inner.reader.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        f(&guard)
    }

    fn send(&self, op: Op) -> Result<()> {
        self.inner
            .tx
            .as_ref()
            .ok_or(ChatError::WriterClosed)?
            .send(op)
            .map_err(|_| ChatError::WriterClosed)
    }

    /// Queue a write and return immediately. Failures are logged by the writer.
    fn exec_async(
        &self,
        what: &'static str,
        f: impl FnOnce(&Connection) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.send(Op {
            urgent: false,
            run: Box::new(move |conn| {
                if let Err(e) = f(conn) {
                    log::warn!("chat.db write `{what}` failed: {e}");
                }
                Box::new(|_| {})
            }),
        })
    }

    /// Queue a write and wait until it is committed; returns the closure's value.
    fn exec_sync<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (rtx, rrx) = sync_channel::<Result<T>>(1);
        self.send(Op {
            urgent: true,
            run: Box::new(move |conn| {
                let res = f(conn);
                Box::new(move |commit| {
                    let out = match commit {
                        Ok(()) => res,
                        Err(msg) => Err(ChatError::Invalid(format!("commit failed: {msg}"))),
                    };
                    let _ = rtx.send(out);
                })
            }),
        })?;
        rrx.recv().map_err(|_| ChatError::WriterClosed)?
    }

    // ----- threads ---------------------------------------------------------

    pub fn insert_thread(&self, new: NewThread) -> Result<ThreadId> {
        self.exec_sync(move |c| {
            let now = now_ms();
            let link = new.spec.link.clone().unwrap_or_default();
            c.execute(
                "INSERT INTO threads(title, project_root, worktree_path, agent_kind, model, permission_mode, \
                 status, created, updated, archived, checkpoint_before_turn, effort, \
                 parent_thread_id, parent_tab, depth, delegated_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    new.title,
                    new.spec.project_root.to_string_lossy(),
                    new.worktree_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    agent_to_db(&new.spec.agent),
                    new.spec.model,
                    permission_to_db(new.spec.permission),
                    now,
                    new.spec.checkpoint_before_turn,
                    new.spec.effort,
                    link.parent_thread_id.map(|p| p.0),
                    link.parent_tab,
                    link.depth,
                    link.delegated_by,
                ],
            )?;
            Ok(ThreadId(c.last_insert_rowid()))
        })
    }

    /// Delegation lineage of a thread (`None`: no such thread). A thread that was not delegated
    /// returns the default link (`depth` 0, nothing else).
    pub fn get_link(&self, id: ThreadId) -> Result<Option<ThreadLink>> {
        self.read(|c| {
            Ok(c.query_row(
                &format!("SELECT {LINK_COLS} FROM threads WHERE id = ?1"),
                [id.0],
                link_from_row,
            )
            .optional()?)
        })
    }

    /// Direct children (threads whose `parent_thread_id` is `parent`), newest first.
    pub fn list_children(&self, parent: ThreadId) -> Result<Vec<ThreadId>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT id FROM threads WHERE parent_thread_id = ?1 ORDER BY updated DESC, id DESC",
            )?;
            let rows = st.query_map([parent.0], |r| Ok(ThreadId(r.get(0)?)))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Every delegated thread (`delegated_by` set) with its link, newest first.
    pub fn list_delegated(&self) -> Result<Vec<(ThreadId, ThreadLink)>> {
        self.read(|c| {
            let mut st = c.prepare(&format!(
                "SELECT id, {LINK_COLS} FROM threads WHERE delegated_by IS NOT NULL ORDER BY updated DESC, id DESC"
            ))?;
            let rows = st.query_map([], |r| {
                Ok((
                    ThreadId(r.get(0)?),
                    ThreadLink {
                        parent_thread_id: r.get::<_, Option<i64>>(1)?.map(ThreadId),
                        parent_tab: r.get(2)?,
                        depth: r.get(3)?,
                        delegated_by: r.get(4)?,
                    },
                ))
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_thread(&self, id: ThreadId) -> Result<Option<ThreadRecord>> {
        self.read(|c| {
            Ok(c.query_row(
                &format!("{THREAD_SELECT} WHERE id = ?1"),
                [id.0],
                thread_from_row,
            )
            .optional()?)
        })
    }

    /// All threads, newest first (archived included; the caller filters).
    pub fn list_threads(&self) -> Result<Vec<ThreadRecord>> {
        self.read(|c| {
            let mut st = c.prepare(&format!("{THREAD_SELECT} ORDER BY updated DESC, id DESC"))?;
            let rows = st.query_map([], thread_from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn set_status(&self, id: ThreadId, status: ThreadStatus) -> Result<()> {
        self.exec_async("set_status", move |c| {
            c.execute(
                "UPDATE threads SET status = ?2, updated = ?3 WHERE id = ?1",
                params![id.0, status.as_str(), now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn set_session(&self, id: ThreadId, session: Option<String>) -> Result<()> {
        self.exec_async("set_session", move |c| {
            c.execute(
                "UPDATE threads SET agent_session_id = ?2 WHERE id = ?1",
                params![id.0, session],
            )?;
            Ok(())
        })
    }

    pub fn set_title(&self, id: ThreadId, title: String) -> Result<()> {
        self.exec_sync(move |c| {
            c.execute(
                "UPDATE threads SET title = ?2 WHERE id = ?1",
                params![id.0, title],
            )?;
            Ok(())
        })
    }

    pub fn set_archived(&self, id: ThreadId, archived: bool) -> Result<()> {
        self.exec_sync(move |c| {
            c.execute(
                "UPDATE threads SET archived = ?2, updated = ?3 WHERE id = ?1",
                params![id.0, archived, now_ms()],
            )?;
            Ok(())
        })
    }

    /// Agent / model / effort switch: the agent session id is cleared unless `session` keeps one.
    pub fn set_agent(
        &self,
        id: ThreadId,
        agent: &agents_driver::AgentKind,
        model: Option<String>,
        effort: Option<String>,
        session: Option<String>,
    ) -> Result<()> {
        let agent = agent_to_db(agent);
        self.exec_sync(move |c| {
            c.execute(
                "UPDATE threads SET agent_kind = ?2, model = ?3, agent_session_id = ?4, updated = ?5, effort = ?6 WHERE id = ?1",
                params![id.0, agent, model, session, now_ms(), effort],
            )?;
            Ok(())
        })
    }

    pub fn set_permission(&self, id: ThreadId, mode: agents_driver::PermissionMode) -> Result<()> {
        self.exec_sync(move |c| {
            c.execute(
                "UPDATE threads SET permission_mode = ?2 WHERE id = ?1",
                params![id.0, permission_to_db(mode)],
            )?;
            Ok(())
        })
    }

    pub fn delete_thread(&self, id: ThreadId) -> Result<()> {
        self.exec_sync(move |c| {
            c.execute("DELETE FROM threads WHERE id = ?1", [id.0])?;
            Ok(())
        })
    }

    // ----- messages --------------------------------------------------------

    /// Insert or replace the message `(thread, seq)` and its tool runs (fire-and-forget).
    pub fn upsert_message(&self, id: ThreadId, w: MessageWrite) -> Result<()> {
        self.exec_async("upsert_message", move |c| {
            c.execute(
                "INSERT INTO messages(thread_id, seq, role, created, content_json, usage_json, cost_usd, cost_source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(thread_id, seq) DO UPDATE SET role = excluded.role, content_json = excluded.content_json, \
                 usage_json = excluded.usage_json, cost_usd = excluded.cost_usd, cost_source = excluded.cost_source",
                params![
                    id.0,
                    w.seq,
                    w.role.as_str(),
                    w.created_ms,
                    w.content_json,
                    w.usage_json,
                    w.cost.map(|c| c.usd),
                    w.cost.map(|c| c.source.as_str()),
                ],
            )?;
            let mid: i64 = c.query_row(
                "SELECT id FROM messages WHERE thread_id = ?1 AND seq = ?2",
                params![id.0, w.seq],
                |r| r.get(0),
            )?;
            c.execute("DELETE FROM tool_runs WHERE message_id = ?1", [mid])?;
            for t in &w.tool_runs {
                c.execute(
                    "INSERT INTO tool_runs(thread_id, message_id, tool_id, kind, title, input_json, result_summary, ok, \
                     started, finished) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id.0,
                        mid,
                        t.tool_id,
                        t.kind,
                        t.title,
                        t.input_json,
                        t.result_summary,
                        t.ok,
                        t.started_ms,
                        t.finished_ms,
                    ],
                )?;
            }
            c.execute("UPDATE threads SET updated = ?2 WHERE id = ?1", params![id.0, now_ms()])?;
            Ok(())
        })
    }

    pub fn load_messages(&self, id: ThreadId) -> Result<Vec<StoredMessage>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT seq, role, created, content_json, usage_json, cost_usd, cost_source \
                 FROM messages WHERE thread_id = ?1 ORDER BY seq",
            )?;
            let rows = st.query_map([id.0], |r| {
                Ok(StoredMessage {
                    seq: r.get(0)?,
                    role: Role::parse(&r.get::<_, String>(1)?),
                    created_ms: r.get(2)?,
                    content_json: r.get(3)?,
                    usage_json: r.get(4)?,
                    cost_usd: r.get(5)?,
                    cost_source: r.get(6)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn message_count(&self, id: ThreadId) -> Result<usize> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM messages WHERE thread_id = ?1",
                [id.0],
                |r| r.get::<_, i64>(0),
            )? as usize)
        })
    }

    pub fn load_tool_runs(&self, id: ThreadId) -> Result<Vec<StoredToolRun>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT m.seq, t.tool_id, t.kind, t.title, t.input_json, t.result_summary, t.ok, t.started, t.finished \
                 FROM tool_runs t LEFT JOIN messages m ON m.id = t.message_id \
                 WHERE t.thread_id = ?1 ORDER BY m.seq, t.id",
            )?;
            let rows = st.query_map([id.0], |r| {
                Ok(StoredToolRun {
                    message_seq: r.get(0)?,
                    tool_id: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    input_json: r.get(4)?,
                    result_summary: r.get(5)?,
                    ok: r.get(6)?,
                    started_ms: r.get(7)?,
                    finished_ms: r.get(8)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // ----- approvals -------------------------------------------------------

    /// `decision == None` records a new pending request; `Some` answers it (inserting the row if
    /// the request was never recorded).
    pub fn upsert_approval(&self, id: ThreadId, w: ApprovalWrite) -> Result<()> {
        self.exec_async("upsert_approval", move |c| {
            if w.decision.is_none() {
                c.execute(
                    "INSERT INTO approvals(thread_id, request_id, tool, detail) VALUES (?1, ?2, ?3, ?4)",
                    params![id.0, w.request_id, w.tool, w.detail],
                )?;
                return Ok(());
            }
            let n = c.execute(
                "UPDATE approvals SET decision = ?3, decided_at = ?4 \
                 WHERE thread_id = ?1 AND request_id = ?2 AND decision IS NULL",
                params![id.0, w.request_id, w.decision, w.decided_ms],
            )?;
            if n == 0 {
                c.execute(
                    "INSERT INTO approvals(thread_id, request_id, tool, detail, decision, decided_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id.0, w.request_id, w.tool, w.detail, w.decision, w.decided_ms],
                )?;
            }
            Ok(())
        })
    }

    pub fn load_approvals(&self, id: ThreadId) -> Result<Vec<StoredApproval>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT request_id, tool, detail, decision, decided_at FROM approvals WHERE thread_id = ?1 ORDER BY id",
            )?;
            let rows = st.query_map([id.0], |r| {
                Ok(StoredApproval {
                    request_id: r.get(0)?,
                    tool: r.get(1)?,
                    detail: r.get(2)?,
                    decision: r.get(3)?,
                    decided_ms: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // ----- plans -----------------------------------------------------------

    pub fn save_plan(&self, id: ThreadId, json: String) -> Result<()> {
        self.exec_async("save_plan", move |c| {
            c.execute(
                "INSERT INTO plans(thread_id, json) VALUES (?1, ?2) \
                 ON CONFLICT(thread_id) DO UPDATE SET json = excluded.json",
                params![id.0, json],
            )?;
            Ok(())
        })
    }

    pub fn load_plan(&self, id: ThreadId) -> Result<Option<String>> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT json FROM plans WHERE thread_id = ?1", [id.0], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?,
            )
        })
    }
}

const THREAD_SELECT: &str =
    "SELECT id, title, project_root, worktree_path, agent_kind, model, permission_mode, status, \
     created, updated, agent_session_id, archived, checkpoint_before_turn, effort, \n     parent_thread_id, parent_tab, depth, delegated_by FROM threads";

const LINK_COLS: &str = "parent_thread_id, parent_tab, depth, delegated_by";

fn link_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadLink> {
    Ok(ThreadLink {
        parent_thread_id: r.get::<_, Option<i64>>(0)?.map(ThreadId),
        parent_tab: r.get(1)?,
        depth: r.get(2)?,
        delegated_by: r.get(3)?,
    })
}

fn thread_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadRecord> {
    Ok(ThreadRecord {
        id: ThreadId(r.get(0)?),
        title: r.get(1)?,
        project_root: PathBuf::from(r.get::<_, String>(2)?),
        worktree_path: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
        agent: agent_from_db(&r.get::<_, String>(4)?),
        model: r.get(5)?,
        permission: permission_from_db(&r.get::<_, String>(6)?),
        status: ThreadStatus::parse(&r.get::<_, String>(7)?),
        created_ms: r.get(8)?,
        updated_ms: r.get(9)?,
        agent_session_id: r.get(10)?,
        archived: r.get::<_, i64>(11)? != 0,
        checkpoint_before_turn: r.get::<_, i64>(12)? != 0,
        effort: r.get(13)?,
        link: ThreadLink {
            parent_thread_id: r.get::<_, Option<i64>>(14)?.map(ThreadId),
            parent_tab: r.get(15)?,
            depth: r.get(16)?,
            delegated_by: r.get(17)?,
        },
    })
}

fn writer_loop(mut conn: Connection, rx: Receiver<Op>) {
    let mut closed = false;
    while !closed {
        let first = match rx.recv() {
            Ok(op) => op,
            Err(_) => return,
        };
        let mut batch = vec![first];
        let mut urgent = batch[0].urgent;
        let deadline = Instant::now() + BATCH_WINDOW;
        while !urgent && batch.len() < MAX_BATCH {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(op) => {
                    urgent = op.urgent;
                    batch.push(op);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        run_batch(&mut conn, batch);
    }
}

fn run_batch(conn: &mut Connection, batch: Vec<Op>) {
    // Manual BEGIN/COMMIT so a failed BEGIN can still fall back to autocommit.
    let in_tx = match conn.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => true,
        Err(e) => {
            log::error!("chat.db: cannot begin transaction, writing unbatched: {e}");
            false
        }
    };
    let conn: &Connection = conn;
    let replies: Vec<Reply> = batch.into_iter().map(|op| (op.run)(conn)).collect();
    let outcome = if in_tx {
        match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                log::error!("chat.db: commit failed: {e}");
                Err(e.to_string())
            }
        }
    } else {
        Ok(())
    };
    for reply in replies {
        reply(outcome.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents_driver::{AgentKind, PermissionMode};

    fn temp_db() -> (tempfile::TempDir, ChatDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = ChatDb::open(&dir.path().join("chat.db")).unwrap();
        (dir, db)
    }

    fn new_thread(db: &ChatDb, title: &str) -> ThreadId {
        let mut spec = ThreadSpec::new("C:/proj", AgentKind::ClaudeCode);
        spec.model = Some("claude-sonnet-5-5".into());
        spec.permission = PermissionMode::AcceptEdits;
        spec.checkpoint_before_turn = true;
        db.insert_thread(NewThread {
            spec,
            title: title.into(),
            worktree_path: None,
        })
        .unwrap()
    }

    #[test]
    fn the_record_carries_its_delegation_link() {
        let (_d, db) = temp_db();
        let parent = new_thread(&db, "padre");
        let mut spec = ThreadSpec::new("C:/proj", AgentKind::Codex);
        spec.link = Some(ThreadLink {
            parent_thread_id: Some(parent),
            parent_tab: None,
            depth: 1,
            delegated_by: Some("mcp".into()),
        });
        let child = db
            .insert_thread(NewThread { spec, title: "hijo".into(), worktree_path: None })
            .unwrap();
        let rec = db.get_thread(child).unwrap().unwrap();
        assert!(rec.is_delegation());
        assert_eq!(rec.link.parent_thread_id, Some(parent));
        assert_eq!((rec.link.depth, rec.link.delegated_by.as_deref()), (1, Some("mcp")));
        let top = db.get_thread(parent).unwrap().unwrap();
        assert!(!top.is_delegation());
        assert_eq!(top.link, ThreadLink::default());
    }

    #[test]
    fn migrations_are_idempotent_and_create_all_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.db");
        {
            let db = ChatDb::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), latest_version());
        }
        let db = ChatDb::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), latest_version());
        let mut conn = Connection::open(&path).unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), latest_version());
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('threads','messages','tool_runs','approvals','plans')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn newer_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 999")
            .unwrap();
        assert!(matches!(
            ChatDb::open(&path),
            Err(ChatError::NewerSchema { found: 999, .. })
        ));
    }

    #[test]
    fn wal_and_read_only_reader() {
        let (_d, db) = temp_db();
        let mode: String = db
            .read(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        assert!(db
            .read(|c| Ok(c.execute("DELETE FROM threads", [])?))
            .is_err());
    }

    #[test]
    fn thread_roundtrip_and_updates() {
        let (_d, db) = temp_db();
        let id = new_thread(&db, "Mi hilo");
        let t = db.get_thread(id).unwrap().unwrap();
        assert_eq!(t.title, "Mi hilo");
        assert_eq!(t.agent, AgentKind::ClaudeCode);
        assert_eq!(t.permission, PermissionMode::AcceptEdits);
        assert!(t.checkpoint_before_turn && !t.archived);
        db.set_status(id, ThreadStatus::Done).unwrap();
        db.set_session(id, Some("sess".into())).unwrap();
        db.set_archived(id, true).unwrap();
        db.set_agent(
            id,
            &AgentKind::Codex,
            Some("gpt".into()),
            Some("high".into()),
            None,
        )
        .unwrap();
        db.flush().unwrap();
        let t = db.get_thread(id).unwrap().unwrap();
        assert_eq!(
            (t.status, t.archived, t.agent_session_id.clone()),
            (ThreadStatus::Done, true, None)
        );
        assert_eq!(t.agent, AgentKind::Codex);
        assert_eq!(t.model.as_deref(), Some("gpt"));
        assert_eq!(t.effort.as_deref(), Some("high"));
    }

    #[test]
    fn busy_threads_reset_to_idle_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.db");
        let id = {
            let db = ChatDb::open(&path).unwrap();
            let id = new_thread(&db, "t");
            db.set_status(id, ThreadStatus::Working).unwrap();
            db.flush().unwrap();
            id
        };
        let db = ChatDb::open(&path).unwrap();
        assert_eq!(
            db.get_thread(id).unwrap().unwrap().status,
            ThreadStatus::Idle
        );
    }

    fn mw(seq: u32, role: Role, text: &str) -> MessageWrite {
        MessageWrite {
            seq,
            role,
            created_ms: 1,
            content_json: format!("{{\"blocks\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}"),
            usage_json: None,
            cost: None,
            tool_runs: vec![crate::state::ToolRunWrite {
                tool_id: "t1".into(),
                kind: "read".into(),
                title: "a.rs".into(),
                input_json: "{}".into(),
                result_summary: Some("ok".into()),
                ok: Some(true),
                started_ms: 1,
                finished_ms: Some(2),
            }],
        }
    }

    #[test]
    fn messages_upsert_replaces_and_tool_runs_follow() {
        let (_d, db) = temp_db();
        let id = new_thread(&db, "t");
        db.upsert_message(id, mw(0, Role::User, "hola")).unwrap();
        db.upsert_message(id, mw(1, Role::Assistant, "v1")).unwrap();
        let mut again = mw(1, Role::Assistant, "v2");
        again.cost = Some(crate::state::MessageCost {
            usd: 0.5,
            source: crate::model::CostSource::Reported,
        });
        again.tool_runs.clear();
        db.upsert_message(id, again).unwrap();
        db.flush().unwrap();
        let msgs = db.load_messages(id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[1].content_json.contains("v2"));
        assert_eq!(
            (msgs[1].cost_usd, msgs[1].cost_source.as_deref()),
            (Some(0.5), Some("reported"))
        );
        let runs = db.load_tool_runs(id).unwrap();
        assert_eq!(
            runs.len(),
            1,
            "the replaced message dropped its tool run, the first kept its own"
        );
        assert_eq!(runs[0].message_seq, Some(0));
    }

    #[test]
    fn approvals_and_plans_and_cascade_delete() {
        let (_d, db) = temp_db();
        let id = new_thread(&db, "t");
        let ap = |d: Option<&str>| ApprovalWrite {
            request_id: "r1".into(),
            tool: "Bash".into(),
            detail: "rm".into(),
            decision: d.map(str::to_string),
            decided_ms: d.map(|_| 9),
        };
        db.upsert_approval(id, ap(None)).unwrap();
        db.upsert_approval(id, ap(Some("allow"))).unwrap();
        db.save_plan(id, "{\"items\":[]}".into()).unwrap();
        db.save_plan(id, "{\"items\":[1]}".into()).unwrap();
        db.upsert_message(id, mw(0, Role::User, "x")).unwrap();
        db.flush().unwrap();
        let a = db.load_approvals(id).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(
            (a[0].decision.as_deref(), a[0].decided_ms),
            (Some("allow"), Some(9))
        );
        assert_eq!(db.load_plan(id).unwrap().unwrap(), "{\"items\":[1]}");
        db.delete_thread(id).unwrap();
        assert!(db.load_messages(id).unwrap().is_empty());
        assert!(db.load_tool_runs(id).unwrap().is_empty());
        assert!(db.load_approvals(id).unwrap().is_empty());
        assert!(db.load_plan(id).unwrap().is_none());
    }

    #[test]
    fn writes_survive_drop_without_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.db");
        let id = {
            let db = ChatDb::open(&path).unwrap();
            let id = new_thread(&db, "t");
            db.upsert_message(id, mw(0, Role::User, "hola")).unwrap();
            id
        };
        let db = ChatDb::open(&path).unwrap();
        assert_eq!(db.load_messages(id).unwrap().len(), 1);
    }
}
