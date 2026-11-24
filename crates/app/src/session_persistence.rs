//! Who writes the session (docs/PLAN.md A5): the debounce and the "only if it changed" guard.
//!
//! The shell knows the *shape* of the window (tabs, panes, ratios) and this entity owns the
//! *persistence*: the state database, the last JSON written and the 2 s debounce timer. When a
//! save is due it emits [`SessionEvent::SaveDue`]; the shell answers with a snapshot through
//! [`SessionPersistence::save`]. Closing the window flushes synchronously.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};
use store::StateDb;

use crate::session::SessionState;

/// Something that belongs to the session changed: it is written within this long (once).
pub const SESSION_DEBOUNCE: Duration = Duration::from_secs(2);

/// What the persistence asks of the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The debounce ended: capture the window and call [`SessionPersistence::save`].
    SaveDue,
}

pub struct SessionPersistence {
    db: Option<StateDb>,
    /// JSON of the last session written (or restored): an unchanged session is not written again.
    last: String,
    /// A session to restore once the chat hub is ready (its tabs need it).
    pending_restore: Option<(SessionState, Option<PathBuf>)>,
    timer: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for SessionPersistence {}

impl SessionPersistence {
    pub fn new(db: Option<StateDb>) -> Self {
        Self {
            db,
            last: String::new(),
            pending_restore: None,
            timer: None,
        }
    }

    /// The state database (also used for favorites, recents and the first-run flag).
    pub fn db(&self) -> Option<&StateDb> {
        self.db.as_ref()
    }

    pub fn enabled(&self) -> bool {
        self.db.is_some()
    }

    /// The session that was just restored counts as written.
    pub fn set_baseline(&mut self, json: String) {
        self.last = json;
    }

    /// Parks a session that waits for the chat hub (and the folder to open afterwards, if any).
    pub fn defer_restore(&mut self, state: SessionState, then_open: Option<PathBuf>) {
        self.pending_restore = Some((state, then_open));
    }

    pub fn take_pending_restore(&mut self) -> Option<(SessionState, Option<PathBuf>)> {
        self.pending_restore.take()
    }

    pub fn restore_pending(&self) -> bool {
        self.pending_restore.is_some()
    }

    /// Asks for a save within [`SESSION_DEBOUNCE`] (nothing happens while one is already armed).
    pub fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        if self.db.is_none() || self.timer.is_some() {
            return;
        }
        self.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SESSION_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.timer = None;
                cx.emit(SessionEvent::SaveDue);
            })
            .ok();
        }));
    }

    /// Writes `json` if it differs from the last one written; `flush` waits for the writer.
    pub fn save(&mut self, json: String, flush: bool) {
        let Some(db) = &self.db else { return };
        if json != self.last {
            if let Err(e) = db.save_session_layout(&json) {
                log::warn!("could not save the session: {e}");
            }
            self.last = json;
        }
        if flush && let Err(e) = db.flush() {
            log::warn!("could not flush the session: {e}");
        }
    }
}

impl SessionPersistence {
    /// A folder to open once the parked session is restored (a second launch asked for it).
    pub fn set_open_after_restore(&mut self, dir: PathBuf) {
        if let Some((_, then)) = &mut self.pending_restore {
            *then = Some(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use std::cell::Cell;
    use std::rc::Rc;

    fn db(tag: &str) -> (StateDb, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("selachi-session-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir.join("state.db")).unwrap();
        (db, dir)
    }

    #[gpui::test]
    fn changes_are_debounced_into_one_save(cx: &mut TestAppContext) {
        let (db, dir) = db("debounce");
        let persistence = cx.new(|_| SessionPersistence::new(Some(db)));
        let due = Rc::new(Cell::new(0u32));
        let _sub = cx.update(|cx| {
            let due = due.clone();
            cx.subscribe(&persistence, move |_, _: &SessionEvent, _| {
                due.set(due.get() + 1)
            })
        });
        for _ in 0..5 {
            persistence.update(cx, |p, cx| p.mark_dirty(cx));
        }
        cx.executor().advance_clock(SESSION_DEBOUNCE / 2);
        assert_eq!(due.get(), 0, "nothing before the debounce ends");
        cx.executor().advance_clock(SESSION_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(due.get(), 1, "five changes, one save request");
        // The timer re-arms for the next change.
        persistence.update(cx, |p, cx| p.mark_dirty(cx));
        cx.executor().advance_clock(SESSION_DEBOUNCE * 2);
        cx.run_until_parked();
        assert_eq!(due.get(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[gpui::test]
    fn an_unchanged_session_is_not_written_again(cx: &mut TestAppContext) {
        let (db, dir) = db("same");
        let reader = db.clone();
        let persistence = cx.new(|_| SessionPersistence::new(Some(db)));
        persistence.update(cx, |p, _| p.save("{\"a\":1}".into(), true));
        let first = reader.load_session_layout().unwrap().unwrap();
        persistence.update(cx, |p, _| p.save("{\"a\":1}".into(), true));
        let again = reader.load_session_layout().unwrap().unwrap();
        assert_eq!(first.json, again.json);
        persistence.update(cx, |p, _| p.save("{\"a\":2}".into(), true));
        assert_eq!(
            reader.load_session_layout().unwrap().unwrap().json,
            "{\"a\":2}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[gpui::test]
    fn without_a_database_nothing_is_armed(cx: &mut TestAppContext) {
        let persistence = cx.new(|_| SessionPersistence::new(None));
        persistence.update(cx, |p, cx| p.mark_dirty(cx));
        assert!(!persistence.read_with(cx, |p, _| p.enabled()));
    }
}
