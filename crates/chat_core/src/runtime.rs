//! `ThreadRuntime`: the live side of a thread. Owns the agent process (spawned lazily on the
//! first `send`), folds its events into a [`ThreadState`], persists finished messages and
//! notifies the UI.
//!
//! Threads: none while the thread is only open. After the first command there is one **worker**
//! (executes commands in order: send, approve, switch... so none of them blocks the caller) and,
//! while an agent process is alive, one **event pump**. Both block in the kernel when idle: a
//! quiet thread costs no CPU. Updates reach the UI through [`ThreadUpdate`], at most one per
//! 16 ms per thread (bursts of events are folded into one).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use agents_driver::{AgentEvent, AgentKind, PermissionMode, ThreadConfig};
use agents_svc::PricingTable;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use parking_lot::{Condvar, Mutex, RwLock};

use crate::db::{ChatDb, StoredMessage};
use crate::error::{ChatError, Result};
use crate::handoff::{build_handoff, HandoffInput};
use crate::hooks::{
    driver_err_text, owner_agent_name, AgentControl, DriverFactory, JobHandle, OwnerInfo, ProcessHooks,
};
use crate::model::{agent_label, now_ms, should_checkpoint, ApprovalDecision, ThreadId, ThreadRecord, ThreadStatus};
use crate::state::{
    message_from_row, Changed, CheckpointRef, CostPolicy, Effects, Message, Persist, Plan, ThreadState,
};

/// Minimum spacing between two [`ThreadUpdate`]s of the same thread.
pub const MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(16);

/// "Something changed in thread `thread_id`": re-read what you display (`with_state`).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadUpdate {
    pub thread_id: ThreadId,
    /// `ThreadState::revision` after the change.
    pub revision: u64,
    /// Live status (so the sidebar needs no extra read).
    pub status: ThreadStatus,
    pub changed: Changed,
}

/// Everything runtimes share with the store.
pub(crate) struct Shared {
    pub db: ChatDb,
    pub factory: Arc<dyn DriverFactory>,
    pub hooks: Option<Arc<dyn ProcessHooks>>,
    pub pricing: Arc<PricingTable>,
    pub policy: CostPolicy,
    subs: Mutex<Vec<UnboundedSender<ThreadUpdate>>>,
}

impl Shared {
    pub fn new(
        db: ChatDb,
        factory: Arc<dyn DriverFactory>,
        hooks: Option<Arc<dyn ProcessHooks>>,
        pricing: Arc<PricingTable>,
        policy: CostPolicy,
    ) -> Self {
        Self { db, factory, hooks, pricing, policy, subs: Mutex::new(Vec::new()) }
    }

    pub fn subscribe(&self) -> UnboundedReceiver<ThreadUpdate> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        self.subs.lock().push(tx);
        rx
    }

    pub fn publish(&self, u: ThreadUpdate) {
        self.subs.lock().retain(|s| s.unbounded_send(u.clone()).is_ok());
    }
}

enum Cmd {
    Send { text: String, attachments: Vec<PathBuf> },
    Interrupt,
    Approve { request_id: String, decision: ApprovalDecision },
    Switch { agent: AgentKind, model: Option<String>, effort: Option<String> },
    SetPermission(PermissionMode),
    /// The pump saw the agent process exit.
    DriverExited { gen: u64 },
    Close { reply: Option<std::sync::mpsc::SyncSender<()>> },
}

enum Slot {
    NotStarted,
    Running { tx: Sender<Cmd>, handle: Option<JoinHandle<()>> },
    Closed,
}

pub(crate) struct Core {
    id: ThreadId,
    shared: Arc<Shared>,
    record: Mutex<ThreadRecord>,
    state: RwLock<ThreadState>,
    cv_lock: Mutex<()>,
    cv: Condvar,
    slot: Mutex<Slot>,
    /// Generation of the current agent process; events of older ones are ignored.
    gen: AtomicU64,
    /// Tools the user answered "Always" for (agents without a native grant).
    always: Mutex<HashSet<String>>,
    last_emit: Mutex<Option<Instant>>,
}

/// Handle to the live thread. Cheap to clone; all clones share the same runtime.
#[derive(Clone)]
pub struct ThreadRuntime {
    core: Arc<Core>,
}

/// Alias used by the UI-facing docs: "the handle of an open thread".
pub type ThreadHandle = ThreadRuntime;

impl std::fmt::Debug for ThreadRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadRuntime").field("id", &self.core.id).finish()
    }
}

/// Decode the persisted messages of a thread.
pub(crate) fn decode_messages(rows: Vec<StoredMessage>) -> Vec<Message> {
    rows.into_iter()
        .map(|r| {
            message_from_row(
                r.seq,
                r.role,
                r.created_ms,
                &r.content_json,
                r.usage_json.as_deref(),
                r.cost_usd,
                r.cost_source.as_deref(),
            )
        })
        .collect()
}

pub(crate) fn load_plan(db: &ChatDb, id: ThreadId) -> Option<Plan> {
    db.load_plan(id).ok().flatten().and_then(|j| serde_json::from_str(&j).ok())
}

impl ThreadRuntime {
    pub(crate) fn load(shared: Arc<Shared>, record: ThreadRecord) -> Result<ThreadRuntime> {
        let messages = decode_messages(shared.db.load_messages(record.id)?);
        let plan = load_plan(&shared.db, record.id);
        let state = ThreadState::from_stored(&record, messages, plan, shared.pricing.clone(), shared.policy);
        Ok(ThreadRuntime {
            core: Arc::new(Core {
                id: record.id,
                shared,
                state: RwLock::new(state),
                record: Mutex::new(record),
                cv_lock: Mutex::new(()),
                cv: Condvar::new(),
                slot: Mutex::new(Slot::NotStarted),
                gen: AtomicU64::new(0),
                always: Mutex::new(HashSet::new()),
                last_emit: Mutex::new(None),
            }),
        })
    }

    pub fn id(&self) -> ThreadId {
        self.core.id
    }

    /// Thread metadata with the live status.
    pub fn record(&self) -> ThreadRecord {
        let mut r = self.core.record.lock().clone();
        r.status = self.core.state.read().status;
        r
    }

    pub fn status(&self) -> ThreadStatus {
        self.core.state.read().status
    }

    /// Read the state under a shared lock (keep the closure short; do not call back into the
    /// runtime from it).
    pub fn with_state<R>(&self, f: impl FnOnce(&ThreadState) -> R) -> R {
        f(&self.core.state.read())
    }

    /// Clone of the whole state (cheap enough for tests; prefer `with_state` while rendering).
    pub fn snapshot(&self) -> ThreadState {
        self.core.state.read().clone()
    }

    pub fn revision(&self) -> u64 {
        self.core.state.read().revision
    }

    /// Send a user message. Returns immediately: the message appears in the state right away; a
    /// checkpoint (when configured), the agent spawn and the delivery happen on the worker.
    /// Start failures show up as a system message plus `status == Error`.
    ///
    /// `attachments` are appended to the prompt as a list of paths.
    pub fn send(&self, text: impl Into<String>, attachments: Vec<PathBuf>) -> Result<()> {
        let text = text.into();
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(ChatError::Invalid("empty message".into()));
        }
        self.core.dispatch(Cmd::Send { text, attachments })
    }

    /// Abort the current turn (the session stays alive). No-op when nothing is running.
    pub fn interrupt(&self) -> Result<()> {
        self.core.dispatch(Cmd::Interrupt)
    }

    /// Answer a pending approval. `Always` also remembers the tool for the rest of this runtime
    /// (agents with a native grant, ACP, get `allow_always` too).
    pub fn approve(&self, request_id: &str, decision: ApprovalDecision) -> Result<()> {
        let known = self
            .core
            .state
            .read()
            .pending_approvals()
            .iter()
            .any(|a| a.request_id == request_id);
        if !known {
            return Err(ChatError::NotFound(format!("approval {request_id}")));
        }
        self.core.dispatch(Cmd::Approve { request_id: request_id.to_string(), decision })
    }

    /// B7: move the thread to another agent and/or model. Runs on the worker: the old driver is
    /// shut down, the thread's agent/model are updated, and the new driver starts.
    ///
    /// * **Different agent** (or no session to resume): the new agent receives a Spanish handoff
    ///   message (last user goal, files touched, open plan items; see [`Self::handoff_preview`])
    ///   as its first message, and it is also recorded as a system message.
    /// * **Same agent, other model**: the session is resumed with the new model, no handoff.
    pub fn switch_agent_or_model(&self, agent: AgentKind, model: Option<String>) -> Result<()> {
        let effort = self.core.record.lock().effort.clone();
        self.core.dispatch(Cmd::Switch { agent, model, effort })
    }

    /// [`Self::switch_agent_or_model`] that also sets the reasoning effort (`None` = the agent's
    /// default). Effort is dropped when the agent has no such knob.
    pub fn switch_with_effort(&self, agent: AgentKind, model: Option<String>, effort: Option<String>) -> Result<()> {
        self.core.dispatch(Cmd::Switch { agent, model, effort })
    }

    /// The handoff text a switch to `to` would send now, built from what is stored (`None` when
    /// the thread has no user message yet). Blocks briefly (flushes the database).
    pub fn handoff_preview(&self, to: &AgentKind, to_model: Option<&str>) -> Result<Option<String>> {
        self.core.shared.db.flush()?;
        let from = self.core.record.lock().agent.clone();
        let messages = decode_messages(self.core.shared.db.load_messages(self.core.id)?);
        let plan = load_plan(&self.core.shared.db, self.core.id);
        Ok(build_handoff(&HandoffInput { from: &from, to, to_model, messages: &messages, plan: plan.as_ref() }))
    }

    /// Change the permission pill. Takes effect by restarting the agent (resuming the session) on
    /// the next message; refused while a turn is running.
    pub fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        if self.status().is_busy() {
            return Err(ChatError::Invalid("cannot change permissions during a turn".into()));
        }
        self.core.dispatch(Cmd::SetPermission(mode))
    }

    /// Stop the agent, persist what is pending and release the process owner. Blocks until done
    /// (up to a few seconds: the agent gets a graceful shutdown first). Idempotent. A closed
    /// runtime cannot be reused: `ChatStore::open` makes a fresh one.
    pub fn close(&self) {
        self.core.close();
    }

    /// Keep the live record in step with a rename done through the store.
    pub(crate) fn set_title(&self, title: &str) {
        self.core.record.lock().title = title.to_string();
        self.core.state.write().mark_titled();
    }

    pub fn is_closed(&self) -> bool {
        matches!(*self.core.slot.lock(), Slot::Closed)
    }

    /// Block until `pred(state)` holds or `timeout` elapses (returns whether it held). Woken by
    /// state changes, no polling. Meant for tests and non-UI callers.
    pub fn wait_for(&self, timeout: Duration, mut pred: impl FnMut(&ThreadState) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = self.core.cv_lock.lock();
        loop {
            if pred(&self.core.state.read()) {
                return true;
            }
            if self.core.cv.wait_until(&mut g, deadline).timed_out() {
                return pred(&self.core.state.read());
            }
        }
    }
}

impl Core {
    fn dispatch(self: &Arc<Self>, cmd: Cmd) -> Result<()> {
        let mut slot = self.slot.lock();
        if matches!(*slot, Slot::NotStarted) {
            let (tx, rx) = channel();
            let core = self.clone();
            let handle = std::thread::Builder::new()
                .name(format!("chat-worker-{}", self.id))
                .spawn(move || Worker { core, rx, driver: None, job: None, owner_open: false }.run())?;
            *slot = Slot::Running { tx, handle: Some(handle) };
        }
        match &*slot {
            Slot::Running { tx, .. } => tx.send(cmd).map_err(|_| ChatError::Closed),
            _ => Err(ChatError::Closed),
        }
    }

    fn close(self: &Arc<Self>) {
        let taken = {
            let mut slot = self.slot.lock();
            std::mem::replace(&mut *slot, Slot::Closed)
        };
        match taken {
            Slot::Running { tx, handle } => {
                let (rtx, rrx) = sync_channel(1);
                if tx.send(Cmd::Close { reply: Some(rtx) }).is_ok() {
                    let _ = rrx.recv_timeout(Duration::from_secs(15));
                }
                if let Some(h) = handle {
                    let _ = h.join();
                }
            }
            Slot::NotStarted => {
                let (fx, ()) = self.mutate(|st| (st.finalize_open(now_ms()), ()));
                self.commit(fx);
            }
            Slot::Closed => {}
        }
    }

    fn notify_waiters(&self) {
        let _g = self.cv_lock.lock();
        self.cv.notify_all();
    }

    /// Mutate the state; the caller commits the returned effects.
    fn mutate<R>(&self, f: impl FnOnce(&mut ThreadState) -> (Effects, R)) -> (Effects, R) {
        let mut st = self.state.write();
        f(&mut st)
    }

    /// Persist the effects, wake waiters and notify the UI.
    fn commit(&self, fx: Effects) {
        self.persist(&fx.persist);
        self.notify_waiters();
        self.emit(fx.changed);
    }

    fn run_state<R>(&self, f: impl FnOnce(&mut ThreadState) -> (Effects, R)) -> R {
        let (fx, r) = self.mutate(f);
        self.commit(fx);
        r
    }

    fn persist(&self, items: &[Persist]) {
        let db = &self.shared.db;
        let log = |what: &str, r: Result<()>| {
            if let Err(e) = r {
                log::warn!("chat_core: persisting {what} of thread {} failed: {e}", self.id);
            }
        };
        for p in items {
            match p {
                Persist::Message(w) => log("message", db.upsert_message(self.id, w.clone())),
                Persist::Plan(j) => log("plan", db.save_plan(self.id, j.clone())),
                Persist::Approval(a) => log("approval", db.upsert_approval(self.id, a.clone())),
                Persist::Status(s) => {
                    self.record.lock().status = *s;
                    log("status", db.set_status(self.id, *s));
                }
                Persist::Session(s) => {
                    self.record.lock().agent_session_id = Some(s.clone());
                    log("session", db.set_session(self.id, Some(s.clone())));
                }
                Persist::Title(t) => {
                    self.record.lock().title = t.clone();
                    log("title", db.set_title(self.id, t.clone()));
                }
            }
        }
    }

    /// Time until the next update may be published.
    fn until_next_emit(&self) -> Duration {
        match *self.last_emit.lock() {
            Some(t) => MIN_UPDATE_INTERVAL.saturating_sub(t.elapsed()),
            None => Duration::ZERO,
        }
    }

    /// Publish an update, never closer than [`MIN_UPDATE_INTERVAL`] to the previous one.
    fn emit(&self, changed: Changed) {
        if !changed.any() {
            return;
        }
        let mut last = self.last_emit.lock();
        if let Some(t) = *last {
            let wait = MIN_UPDATE_INTERVAL.saturating_sub(t.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
        *last = Some(Instant::now());
        let (revision, status) = {
            let st = self.state.read();
            (st.revision, st.status)
        };
        self.shared.publish(ThreadUpdate { thread_id: self.id, revision, status, changed });
    }

    /// Fold one event from the pump. `false`: the event belongs to a replaced process or the
    /// runtime is closed; the pump must stop.
    fn handle_event(self: &Arc<Self>, gen: u64, ev: AgentEvent, changed: &mut Changed) -> bool {
        if self.gen.load(Ordering::SeqCst) != gen || matches!(*self.slot.lock(), Slot::Closed) {
            return false;
        }
        let auto = match &ev {
            AgentEvent::ApprovalRequest { id, tool, .. } if self.always.lock().contains(tool) => Some(id.clone()),
            _ => None,
        };
        let exited = matches!(ev, AgentEvent::Exited { .. });
        let (fx, ()) = self.mutate(|st| (st.apply(ev, now_ms()), ()));
        self.persist(&fx.persist);
        self.notify_waiters();
        changed.merge(fx.changed);
        if let Some(request_id) = auto {
            let _ = self.dispatch(Cmd::Approve { request_id, decision: ApprovalDecision::Always });
        }
        if exited {
            let _ = self.dispatch(Cmd::DriverExited { gen });
        }
        true
    }
}

fn pump(core: Arc<Core>, gen: u64, mut events: UnboundedReceiver<AgentEvent>) {
    let mut done = false;
    while !done {
        let Some(first) = futures::executor::block_on(events.next()) else { break };
        let mut changed = Changed::default();
        if !core.handle_event(gen, first, &mut changed) {
            break;
        }
        // Hold back until the 16 ms slot opens, folding whatever arrives meanwhile.
        let wait = core.until_next_emit();
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        loop {
            match events.try_recv() {
                Ok(ev) => {
                    if !core.handle_event(gen, ev, &mut changed) {
                        done = true;
                        break;
                    }
                }
                Err(futures::channel::mpsc::TryRecvError::Closed) => {
                    done = true;
                    break;
                }
                Err(futures::channel::mpsc::TryRecvError::Empty) => break,
            }
        }
        core.emit(changed);
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

struct Active {
    control: Box<dyn AgentControl>,
    gen: u64,
}

struct Worker {
    core: Arc<Core>,
    rx: Receiver<Cmd>,
    driver: Option<Active>,
    job: Option<JobHandle>,
    owner_open: bool,
}

/// Prompt text plus the attachment paths.
fn compose_prompt(text: &str, attachments: &[PathBuf]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("Archivos adjuntos (léelos con tus herramientas):\n");
    for p in attachments {
        out.push_str(&format!("- {}\n", p.display()));
    }
    out
}

impl Worker {
    fn run(mut self) {
        while let Ok(cmd) = self.rx.recv() {
            match cmd {
                Cmd::Send { text, attachments } => self.on_send(text, attachments),
                Cmd::Interrupt => self.on_interrupt(),
                Cmd::Approve { request_id, decision } => self.on_approve(&request_id, decision),
                Cmd::Switch { agent, model, effort } => self.on_switch(agent, model, effort),
                Cmd::SetPermission(mode) => self.on_set_permission(mode),
                Cmd::DriverExited { gen } => {
                    if self.driver.as_ref().is_some_and(|d| d.gen == gen) {
                        if let Some(mut d) = self.driver.take() {
                            d.control.shutdown();
                        }
                    }
                }
                Cmd::Close { reply } => {
                    self.on_close();
                    if let Some(r) = reply {
                        let _ = r.send(());
                    }
                    return;
                }
            }
        }
        // The sender side vanished without a Close (runtime dropped): clean up anyway.
        self.on_close();
    }

    fn fail(&self, msg: String) {
        self.core.run_state(|st| {
            let mut fx = st.add_system_message(&msg, now_ms());
            fx.merge(st.set_status(ThreadStatus::Error, Some(msg.clone())));
            (fx, ())
        });
    }

    fn stop_driver(&mut self) {
        if let Some(mut d) = self.driver.take() {
            // Bump first so the old pump drops whatever the dying process still says.
            self.core.gen.fetch_add(1, Ordering::SeqCst);
            d.control.shutdown();
        }
    }

    fn ensure_driver(&mut self) -> std::result::Result<(), String> {
        if self.driver.is_some() {
            return Ok(());
        }
        let rec = self.core.record.lock().clone();
        let shared = self.core.shared.clone();
        if !self.owner_open {
            self.owner_open = true;
            if let Some(h) = &shared.hooks {
                self.job = h.open_owner(
                    self.core.id,
                    &OwnerInfo {
                        agent: owner_agent_name(&rec.agent),
                        label: rec.display_title().to_string(),
                        cwd: rec.cwd().to_path_buf(),
                    },
                );
            }
        }
        let mut cfg = ThreadConfig::new(rec.cwd());
        cfg.model = rec.model.clone();
        cfg.effort = rec.effort.clone();
        cfg.permission_mode = rec.permission;
        cfg.resume_session = rec.agent_session_id.clone();
        let spawned = shared.factory.spawn(&rec.agent, &cfg, self.job).map_err(|e| driver_err_text(&e))?;
        let gen = self.core.gen.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut st = self.core.state.write();
            st.driver_restarted();
        }
        if let (Some(pid), Some(h)) = (spawned.pid, &shared.hooks) {
            h.register_root(self.core.id, pid);
        }
        let core = self.core.clone();
        let events = spawned.events;
        std::thread::Builder::new()
            .name(format!("chat-events-{}", self.core.id))
            .spawn(move || pump(core, gen, events))
            .map_err(|e| format!("No se pudo iniciar el hilo de eventos: {e}"))?;
        self.driver = Some(Active { control: spawned.control, gen });
        Ok(())
    }

    /// Deliver `prompt` to the agent, starting (or restarting a dead) process as needed.
    fn deliver(&mut self, prompt: &str) -> std::result::Result<(), String> {
        for attempt in 0..2 {
            self.ensure_driver()?;
            let Some(d) = self.driver.as_mut() else { return Err("agente no disponible".into()) };
            match d.control.send(prompt) {
                Ok(()) => return Ok(()),
                Err(agents_driver::DriverError::Closed) if attempt == 0 => {
                    // The process died between turns: drop it and resume the session once.
                    self.stop_driver();
                }
                Err(e) => return Err(format!("No se pudo enviar el mensaje al agente: {e}")),
            }
        }
        Err("El agente no responde".into())
    }

    fn on_send(&mut self, text: String, attachments: Vec<PathBuf>) {
        let core = self.core.clone();
        // 1. The user message shows up immediately.
        let seq = core.run_state(|st| {
            let fx = st.begin_user_message(&text, attachments.clone(), None, now_ms());
            let seq = (st.messages.len() - 1) as u32;
            (fx, seq)
        });
        // 2. Safety net (A10): checkpoint before the turn.
        let rec = core.record.lock().clone();
        if rec.checkpoint_before_turn && should_checkpoint(rec.permission) {
            let turn = core.state.read().turn_count();
            match git_svc::Repo::discover(rec.cwd()) {
                Ok(Some(repo)) => {
                    let label = format!("antes del turno {turn}");
                    match repo.create_checkpoint(&label) {
                        Ok(cp) => {
                            let cp = CheckpointRef { id: cp.id, label: cp.label };
                            core.run_state(|st| (st.attach_checkpoint(seq, cp), ()));
                        }
                        Err(e) => {
                            log::warn!("chat_core: checkpoint failed: {e}");
                            core.run_state(|st| {
                                (st.add_system_message(&format!("No se pudo crear el checkpoint: {e}"), now_ms()), ())
                            });
                        }
                    }
                }
                Ok(None) => log::debug!("chat_core: {} is not a git repo, no checkpoint", rec.cwd().display()),
                Err(e) => log::warn!("chat_core: checkpoint discovery failed: {e}"),
            }
        }
        // 3. Deliver.
        if let Err(msg) = self.deliver(&compose_prompt(&text, &attachments)) {
            self.fail(msg);
        }
    }

    fn on_interrupt(&mut self) {
        if let Some(d) = self.driver.as_mut() {
            if let Err(e) = d.control.interrupt() {
                log::debug!("chat_core: interrupt: {e}");
            }
        }
    }

    fn on_approve(&mut self, request_id: &str, decision: ApprovalDecision) {
        let core = self.core.clone();
        let tool = core
            .state
            .read()
            .pending_approvals()
            .iter()
            .find(|a| a.request_id == request_id)
            .map(|a| a.tool.clone());
        let Some(tool) = tool else { return };
        core.run_state(|st| match st.resolve_approval(request_id, decision, now_ms()) {
            Some(fx) => (fx, ()),
            None => (Effects::default(), ()),
        });
        if decision == ApprovalDecision::Always {
            core.always.lock().insert(tool);
        }
        let Some(d) = self.driver.as_mut() else { return };
        if let Err(e) = d.control.approve(request_id, decision) {
            log::warn!("chat_core: approve failed: {e}");
            self.fail(format!("No se pudo responder a la aprobación: {e}"));
        }
    }

    fn on_set_permission(&mut self, mode: PermissionMode) {
        let core = self.core.clone();
        {
            let mut rec = core.record.lock();
            if rec.permission == mode {
                return;
            }
            rec.permission = mode;
        }
        if let Err(e) = core.shared.db.set_permission(core.id, mode) {
            log::warn!("chat_core: persisting permission failed: {e}");
        }
        // The CLI fixed its permission flags at spawn: restart on the next message (resume).
        self.stop_driver();
        core.always.lock().clear();
        core.notify_waiters();
        core.emit(Changed { list: true, status: true, ..Changed::default() });
    }

    fn on_switch(&mut self, agent: AgentKind, model: Option<String>, effort: Option<String>) {
        let core = self.core.clone();
        let old = core.record.lock().clone();
        let effort = effort.filter(|_| agents_driver::supports_effort(&agent));
        if agent == old.agent && model == old.model && effort == old.effort {
            return;
        }
        let agent_changed = agent != old.agent;
        // 1. Close the open turn and make the stored copy current.
        core.run_state(|st| (st.finalize_open(now_ms()), ()));
        if let Err(e) = core.shared.db.flush() {
            log::warn!("chat_core: flush before switch failed: {e}");
        }
        // 2. Handoff text from what is stored (not for a plain model change that can resume).
        let handoff = if agent_changed || old.agent_session_id.is_none() {
            let messages = core.shared.db.load_messages(core.id).map(decode_messages).unwrap_or_default();
            let plan = load_plan(&core.shared.db, core.id);
            build_handoff(&HandoffInput {
                from: &old.agent,
                to: &agent,
                to_model: model.as_deref(),
                messages: &messages,
                plan: plan.as_ref(),
            })
        } else {
            None
        };
        // 3. Stop the old agent; nothing from it may reach the state any more.
        self.stop_driver();
        core.always.lock().clear();
        // 4. Re-point the thread.
        let keep_session = !agent_changed;
        let session = if keep_session { old.agent_session_id } else { None };
        {
            let mut rec = core.record.lock();
            rec.agent = agent.clone();
            rec.model = model.clone();
            rec.effort = effort.clone();
            rec.agent_session_id = session.clone();
        }
        if let Err(e) = core.shared.db.set_agent(core.id, &agent, model.clone(), effort, session) {
            log::warn!("chat_core: persisting agent switch failed: {e}");
        }
        let note = match &model {
            Some(m) if !m.is_empty() => format!("Cambiado a {} · {m}", agent_label(&agent)),
            _ => format!("Cambiado a {}", agent_label(&agent)),
        };
        core.run_state(|st| {
            st.set_agent(agent.clone(), model.clone(), keep_session);
            let mut fx = st.add_system_message(&note, now_ms());
            fx.merge(st.set_status(ThreadStatus::Idle, None));
            fx.changed.list = true;
            (fx, ())
        });
        // 5. The new agent starts with the handoff.
        if let Some(text) = handoff {
            core.run_state(|st| {
                let mut fx = st.add_system_message(&format!("Traspaso enviado:\n{text}"), now_ms());
                fx.merge(st.set_status(ThreadStatus::Working, None));
                (fx, ())
            });
            if let Err(msg) = self.deliver(&text) {
                self.fail(msg);
            }
        }
    }

    fn on_close(&mut self) {
        let core = self.core.clone();
        core.run_state(|st| {
            let mut fx = st.finalize_open(now_ms());
            if st.status.is_busy() {
                fx.merge(st.set_status(ThreadStatus::Idle, None));
            }
            (fx, ())
        });
        self.stop_driver();
        if self.owner_open {
            if let Some(h) = &core.shared.hooks {
                h.close_owner(core.id);
            }
            self.owner_open = false;
        }
        if let Err(e) = core.shared.db.flush() {
            log::warn!("chat_core: flush on close failed: {e}");
        }
        core.notify_waiters();
    }
}
