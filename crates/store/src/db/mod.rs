//! The state database (`%APPDATA%\Selachi\state.db`).
//!
//! * One **writer thread** owns the only write connection. Writes are queued over
//!   a channel and committed in batches (one transaction per batch).
//! * Reads use a **separate read-only connection** (WAL lets them run while the
//!   writer commits). Reads see committed data only; call [`StateDb::flush`] for
//!   read-your-writes after a fire-and-forget write.
//!
//! Write methods come in two flavours. *Hot-path* writes (`record_command`,
//! `finish_command`, `record_visit`, `save_session_layout`) return as soon as the
//! op is queued. *Management* writes (projects, favorites, `prune`, ...) block
//! until committed, so the UI can write then read back.

mod favorites;
mod frecency;
mod history;
mod layout;
mod meta;
mod projects;
mod retention;
mod schema;
mod snippets;

pub use favorites::Favorite;
pub use frecency::PathEntry;
pub use history::{looks_sensitive, HistoryEntry, NewCommand};
pub use layout::SessionLayout;
pub use meta::{ONBOARDING_DONE, UPDATE_FROM, UPDATE_NOTES, WHATS_NEW_SEEN};
pub use projects::{detect_project, discover_projects, DiscoveredProject, Project, ProjectKind};
pub use retention::PruneStats;
pub use schema::{latest_version, migrate};
pub use snippets::{NewSnippet, Snippet, SnippetScope};

use crate::error::{Result, StoreError};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long the writer waits for more ops before committing a batch.
const BATCH_WINDOW: Duration = Duration::from_millis(20);
/// Maximum ops per transaction.
const MAX_BATCH: usize = 1024;

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Called by the writer after the batch containing the op was committed
/// (`Ok`) or failed to commit (`Err(message)`).
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

/// Cheaply cloneable handle to the state database.
#[derive(Clone)]
pub struct StateDb {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for StateDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDb")
            .field("path", &self.inner.path)
            .finish()
    }
}

impl StateDb {
    /// Open (creating and migrating if needed) `%APPDATA%\Selachi\state.db`.
    pub fn open_default() -> Result<StateDb> {
        Self::open(&crate::paths::state_db_path()?)
    }

    /// Open (creating and migrating if needed) the database at `path`.
    /// In-memory databases are not supported (writer and reader are separate connections).
    pub fn open(path: &Path) -> Result<StateDb> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
            }
        }
        let mut writer = Connection::open(path)?;
        writer.busy_timeout(Duration::from_secs(5))?;
        writer.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        schema::migrate(&mut writer)?;

        let reader = Connection::open(path)?;
        reader.busy_timeout(Duration::from_secs(5))?;
        reader.pragma_update(None, "query_only", "ON")?;

        let (tx, rx) = channel::<Op>();
        let thread = std::thread::Builder::new()
            .name("selachi-state-writer".into())
            .spawn(move || writer_loop(writer, rx))
            .map_err(|e| StoreError::io(path, e))?;

        Ok(StateDb {
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

    /// Schema version of the open database.
    pub fn schema_version(&self) -> Result<u32> {
        self.read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
    }

    // ---- plumbing shared by the submodules --------------------------------

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
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
            .ok_or(StoreError::WriterClosed)?
            .send(op)
            .map_err(|_| StoreError::WriterClosed)
    }

    /// Queue a write and return immediately. Failures are logged by the writer.
    pub(crate) fn exec_async(
        &self,
        what: &'static str,
        f: impl FnOnce(&Connection) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.send(Op {
            urgent: false,
            run: Box::new(move |conn| {
                if let Err(e) = f(conn) {
                    log::warn!("state.db write `{what}` failed: {e}");
                }
                Box::new(|_| {})
            }),
        })
    }

    /// Queue a write and wait until it is committed; returns the closure's value.
    pub(crate) fn exec_sync<T: Send + 'static>(
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
                        Err(msg) => Err(StoreError::Invalid(format!("commit failed: {msg}"))),
                    };
                    let _ = rtx.send(out);
                })
            }),
        })?;
        rrx.recv().map_err(|_| StoreError::WriterClosed)?
    }
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
    // Manual BEGIN/COMMIT (instead of `Transaction`) so a failed BEGIN can still
    // fall back to running the ops in autocommit mode.
    let in_tx = match conn.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => true,
        Err(e) => {
            log::error!("state.db: cannot begin transaction, writing unbatched: {e}");
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
                log::error!("state.db: commit failed: {e}");
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
pub(crate) use tests::temp_db;

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn temp_db() -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let db = StateDb::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), latest_version());
        }
        {
            let db = StateDb::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), latest_version());
        }
        let mut conn = Connection::open(&path).unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), latest_version());
        assert_eq!(migrate(&mut conn).unwrap(), latest_version());
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('command_history','path_history','projects','favorites','session_layout')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 5);
    }

    #[test]
    fn newer_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA user_version = 999").unwrap();
        }
        assert!(StateDb::open(&path).is_err());
    }

    #[test]
    fn wal_enabled_and_reader_is_read_only() {
        let (_d, db) = temp_db();
        let mode: String = db
            .read(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let res = db.read(|c| Ok(c.execute("DELETE FROM favorites", [])?));
        assert!(res.is_err());
    }

    #[test]
    fn writes_survive_drop_without_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let db = StateDb::open(&path).unwrap();
            db.record_visit("C:\\x\\y").unwrap();
            // dropped without flush: the writer drains its queue on shutdown
        }
        let db = StateDb::open(&path).unwrap();
        assert_eq!(db.frecent("", 10).unwrap().len(), 1);
    }
}
