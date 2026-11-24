//! Host side of the chat mode (wave 5.8): what `chat_core` / `ui_chat` need from the app that they
//! cannot own themselves.
//!
//! * [`ProcHooks`]: `chat_core::ProcessHooks` over the shared `proc_svc` tracker. Every thread gets
//!   a process owner (a Job Object, so the agent and what it spawns show up in the procs panel and
//!   become orphans when the thread closes). The hooks run on `chat_core` worker threads, so the
//!   parts that need the UI thread (watching job events, pruning closed owners) travel as
//!   [`HookMsg`] over a channel the shell drains.
//! * [`open_store`]: the `ChatStore` (`chat.db`), with the scripted [`FakeFactory`] when
//!   `SELACHI_CHAT_FAKE=1` (no processes, no model calls: for screenshots and manual tests).
//! * [`attention_for`]: what a thread's state means for the attention queue.
//! * [`ThreadsProvider`]: the `threads` panel, registered at startup but resolved lazily, so the
//!   chat database is only opened when chat is used.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agents_driver::{AgentEvent, AgentKind, AgentStatus, DriverError, ThreadConfig, ToolKind};
use chat_core::{
    AgentControl, ApprovalDecision, ChatStore, DriverFactory, JobHandle, OwnerInfo, ProcessHooks,
    SpawnedAgent, ThreadId, ThreadStatus,
};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, IntoElement, Render,
    SharedString, Subscription, Task, Window, div,
};
use ui_chat::{ChatEvent, ChatHub};

use crate::services::Services;
use panel_api::{PanelCtx, PanelId, PanelProvider, PanelZone};
use ui_kit::Icon;
use ui_procs::SharedTracker;
use ui_review::AttentionKind;

/// Thread owners live above the tab owners (entity ids, small integers) in the shared tracker, so a
/// thread id can never collide with a terminal tab.
pub const OWNER_BASE: u64 = 1 << 40;

/// The tracker owner of a chat thread.
pub fn owner_of(thread: ThreadId) -> u64 {
    OWNER_BASE | thread.owner_id()
}

/// What the hooks hand to the UI thread.
pub enum HookMsg {
    /// A new owner's job events: give them to `ProcsModel::watch_job_events`.
    Events(UnboundedReceiver<proc_svc::JobEvent>),
    /// The thread runtime closed; the owner stays in the tracker until its job is empty.
    Closed(u64),
}

/// `ProcessHooks` over the shared tracker (see the module docs).
pub struct ProcHooks {
    tracker: SharedTracker,
    tx: Mutex<UnboundedSender<HookMsg>>,
}

impl ProcHooks {
    pub fn new(tracker: SharedTracker, tx: UnboundedSender<HookMsg>) -> Self {
        Self {
            tracker,
            tx: Mutex::new(tx),
        }
    }

    fn send(&self, msg: HookMsg) {
        let tx = self.tx.lock().unwrap_or_else(|p| p.into_inner());
        let _ = tx.unbounded_send(msg);
    }
}

impl ProcessHooks for ProcHooks {
    fn open_owner(&self, thread_id: ThreadId, info: &OwnerInfo) -> Option<JobHandle> {
        let label = if info.label.trim().is_empty() {
            format!("Hilo · {}", info.agent)
        } else {
            info.label.clone()
        };
        let meta = proc_svc::OwnerMeta {
            agent: Some(info.agent.clone()),
            label,
            cwd: Some(info.cwd.clone()),
        };
        match ui_procs::open_owner(
            &self.tracker,
            owner_of(thread_id),
            meta,
            proc_svc::JobOptions::default(),
        ) {
            Ok(job) => {
                self.send(HookMsg::Events(job.events));
                Some(job.job_handle as usize)
            }
            Err(e) => {
                log::warn!("could not create the process job of thread {thread_id}: {e}");
                None
            }
        }
    }

    fn register_root(&self, thread_id: ThreadId, pid: u32) {
        let mut t = self.tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = t.register_root(owner_of(thread_id), pid) {
            log::warn!("could not register pid {pid} of thread {thread_id}: {e}");
        }
    }

    fn close_owner(&self, thread_id: ThreadId) {
        self.send(HookMsg::Closed(owner_of(thread_id)));
    }
}

/// Is the scripted agent requested? (`SELACHI_CHAT_FAKE=1`.)
pub fn fake_requested() -> bool {
    std::env::var_os("SELACHI_CHAT_FAKE").is_some_and(|v| v == "1")
}

/// Opens `chat.db` with the real CLIs, or with the scripted agent when it was asked for.
pub fn open_store(hooks: Arc<ProcHooks>) -> chat_core::Result<ChatStore> {
    let mut b = ChatStore::builder().hooks(hooks);
    if fake_requested() {
        log::warn!(
            "SELACHI_CHAT_FAKE=1: chat threads use a scripted agent (no processes, no model calls)"
        );
        b = b.factory(Arc::new(FakeFactory));
    }
    b.open()
}

/// What a thread's state asks of the user. `approval` is the first pending approval's
/// `(tool, detail)`. `Done` and `Idle` ask nothing (the sidebar row shows the check).
pub fn attention_for(
    status: ThreadStatus,
    approval: Option<(String, String)>,
) -> Option<AttentionKind> {
    if let Some((tool, detail)) = approval {
        return Some(AttentionKind::Approval { tool, detail });
    }
    match status {
        ThreadStatus::WaitingForInput => Some(AttentionKind::WaitingInput),
        ThreadStatus::Error => Some(AttentionKind::AgentError),
        ThreadStatus::Idle | ThreadStatus::Working | ThreadStatus::Done => None,
    }
}

/// Title and first line of the toast of an attention kind (`agent`: "Claude Code").
pub fn toast_title(kind: &AttentionKind, agent: &str) -> String {
    match kind {
        AttentionKind::Approval { .. } => format!("{agent} pide tu aprobación"),
        AttentionKind::WaitingInput => format!("{agent} espera tu respuesta"),
        AttentionKind::AgentError => format!("{agent} terminó con un error"),
        AttentionKind::Failed { .. } | AttentionKind::LongCommandDone => {
            format!("{agent} necesita tu atención")
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The `threads` panel, resolved lazily
// ---------------------------------------------------------------------------------------------

/// Stand-in view until the chat hub exists (the shell creates it before it builds the panel).
struct Pending;

impl Render for Pending {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
    }
}

/// `PanelProvider` `"threads"`. The real provider needs the `ChatHub`, which opens `chat.db`;
/// that is deferred to the first use so a session without chat pays nothing at startup.
pub struct ThreadsProvider;

impl ThreadsProvider {
    fn inner(cx: &App) -> Option<ui_chat::ThreadsPanelProvider> {
        let services = cx.try_global::<crate::services::Services>()?;
        let hub = services.chat.borrow().clone()?;
        Some(ui_chat::ThreadsPanelProvider::new(hub))
    }
}

impl PanelProvider for ThreadsProvider {
    fn id(&self) -> PanelId {
        PanelId::new(ui_chat::THREADS_PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Hilos".into()
    }

    fn icon(&self) -> Icon {
        Icon::Agents
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Right, PanelZone::Sidebar]
    }

    fn build(&self, ctx: PanelCtx, window: &mut Window, cx: &mut App) -> AnyView {
        match Self::inner(cx) {
            Some(p) => p.build(ctx, window, cx),
            None => cx.new(|_| Pending).into(),
        }
    }

    fn context_changed(
        &self,
        view: &AnyView,
        ctx: &PanelCtx,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        Self::inner(cx).is_some_and(|p| p.context_changed(view, ctx, window, cx))
    }
}

// ---------------------------------------------------------------------------------------------
// The hub's lifecycle: `ChatHost`
// ---------------------------------------------------------------------------------------------

/// Chat work that needs the hub and was asked before it was ready.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatIntent {
    /// A thread row was clicked: its tab, opened or brought forward.
    OpenThread(ThreadId),
    /// "Nuevo chat" (the project to start in, if any).
    NewThread(Option<PathBuf>),
    /// `SELACHI_DEMO=chat`.
    Demo,
}

/// Where the hub is: `chat.db` is opened off the UI thread, so there is a loading state.
#[derive(Clone)]
enum ChatPhase {
    /// Nobody used chat yet: nothing was opened.
    Idle,
    Loading,
    Ready(Entity<ChatHub>),
    /// `chat.db` could not be opened (never retried).
    Failed,
}

/// What the host tells the shell.
pub enum ChatHostEvent {
    Ready,
    Failed(String),
    /// An event of the hub (open a thread, new thread, restore a checkpoint...).
    Chat(ChatEvent),
    /// A thread changed (status, approvals...).
    Thread(chat_core::ThreadUpdate),
    /// A thread runtime closed: its process owner can be pruned by the shell.
    OwnerClosed(u64),
}

/// Owns the chat hub's lifecycle and what travels with it: the hooks bridge, the projects last
/// given to the hub, what each thread last asked of the user, and the work queued while the
/// database loads. Created with the shell, but the hub (and `chat.db`) only the first time chat is
/// used, so a terminal-only session pays nothing.
pub struct ChatHost {
    phase: ChatPhase,
    theme: theme::Theme,
    pending: Vec<ChatIntent>,
    /// Projects last given to the hub (it only hears about changes).
    projects: Vec<PathBuf>,
    /// What each chat thread last asked of the user (a toast fires when it changes).
    attn: HashMap<ThreadId, AttentionKind>,
    subs: Vec<Subscription>,
    _open: Option<Task<()>>,
    _hooks: Option<Task<()>>,
}

impl EventEmitter<ChatHostEvent> for ChatHost {}

impl ChatHost {
    pub fn new(theme: theme::Theme) -> Self {
        Self {
            phase: ChatPhase::Idle,
            theme,
            pending: Vec::new(),
            projects: Vec::new(),
            attn: HashMap::new(),
            subs: Vec::new(),
            _open: None,
            _hooks: None,
        }
    }

    /// The hub, once it is ready.
    pub fn hub(&self) -> Option<Entity<ChatHub>> {
        match &self.phase {
            ChatPhase::Ready(h) => Some(h.clone()),
            _ => None,
        }
    }

    /// Is the database opening right now?
    #[cfg(test)]
    pub fn loading(&self) -> bool {
        matches!(self.phase, ChatPhase::Loading)
    }

    pub fn failed(&self) -> bool {
        matches!(self.phase, ChatPhase::Failed)
    }

    pub fn set_theme(&mut self, theme: theme::Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        if let Some(hub) = self.hub() {
            hub.update(cx, |h, cx| h.set_theme(theme, cx));
        }
    }

    /// Queues `intent` and starts loading when nobody did. `false`: chat is unavailable.
    pub fn queue(&mut self, intent: ChatIntent, cx: &mut Context<Self>) -> bool {
        if self.failed() {
            return false;
        }
        self.pending.push(intent);
        self.ensure(cx);
        true
    }

    pub fn take_pending(&mut self) -> Vec<ChatIntent> {
        std::mem::take(&mut self.pending)
    }

    /// Starts opening `chat.db` on the background executor (once). The hub appears through
    /// [`ChatHostEvent::Ready`]; a failure through [`ChatHostEvent::Failed`].
    pub fn ensure(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, ChatPhase::Idle) {
            return;
        }
        let Some(services) = cx.try_global::<Services>().cloned() else {
            self.phase = ChatPhase::Failed;
            self.pending.clear();
            cx.emit(ChatHostEvent::Failed("sin servicios".into()));
            return;
        };
        self.phase = ChatPhase::Loading;
        let (tx, mut rx) = unbounded::<HookMsg>();
        let hooks = Arc::new(ProcHooks::new(services.tracker.clone(), tx));
        // The hooks run on chat worker threads; what needs the UI thread arrives here.
        let procs = services.procs;
        self._hooks = Some(cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let done = this.update(cx, |_, cx| match msg {
                    HookMsg::Events(events) => {
                        procs.update(cx, |m, cx| m.watch_job_events(events, cx))
                    }
                    HookMsg::Closed(owner) => cx.emit(ChatHostEvent::OwnerClosed(owner)),
                });
                if done.is_err() {
                    break;
                }
            }
        }));
        self._open = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { open_store(hooks) })
                .await;
            this.update(cx, |host, cx| host.finish(res, cx)).ok();
        }));
    }

    /// The store was opened (or not): create the hub on the UI thread and wire its events.
    fn finish(&mut self, res: chat_core::Result<ChatStore>, cx: &mut Context<Self>) {
        let store = match res {
            Ok(s) => s,
            Err(e) => {
                log::warn!("chat is unavailable: {e}");
                self.phase = ChatPhase::Failed;
                self.pending.clear();
                cx.emit(ChatHostEvent::Failed(e.to_string()));
                cx.notify();
                return;
            }
        };
        let Some(services) = cx.try_global::<Services>().cloned() else {
            self.phase = ChatPhase::Failed;
            self.pending.clear();
            cx.emit(ChatHostEvent::Failed("sin servicios".into()));
            return;
        };
        let hub = ChatHub::create(store, self.theme, cx);
        *services.chat.borrow_mut() = Some(hub.clone());
        hub.update(cx, |h, cx| h.detect_agents(cx));
        // Scripted agents (`SELACHI_CHAT_FAKE`) start no processes, not even to list models.
        hub.update(cx, |h, _| h.set_probe_models(!fake_requested()));
        self.subs
            .push(cx.subscribe(&hub, |_, _, ev: &ChatEvent, cx| {
                cx.emit(ChatHostEvent::Chat(ev.clone()))
            }));
        self.subs
            .push(cx.subscribe(&hub, |_, _, u: &chat_core::ThreadUpdate, cx| {
                cx.emit(ChatHostEvent::Thread(u.clone()))
            }));
        self.phase = ChatPhase::Ready(hub.clone());
        self.projects.clear();
        cx.emit(ChatHostEvent::Ready);
        cx.notify();
    }

    /// The projects the new-thread picker offers and the default one. Told to the hub only when
    /// they changed.
    pub fn sync_projects(
        &mut self,
        projects: Vec<PathBuf>,
        default: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(hub) = self.hub() else { return };
        hub.update(cx, |h, _| h.set_default_project(default));
        if projects != self.projects {
            self.projects.clone_from(&projects);
            hub.update(cx, |h, cx| h.set_projects(projects, cx));
        }
    }

    /// What `thread` last asked of the user.
    pub fn attention_of(&self, thread: ThreadId) -> Option<&AttentionKind> {
        self.attn.get(&thread)
    }

    pub fn set_attention(&mut self, thread: ThreadId, kind: Option<AttentionKind>) {
        match kind {
            Some(k) => {
                self.attn.insert(thread, k);
            }
            None => {
                self.attn.remove(&thread);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The scripted agent (SELACHI_CHAT_FAKE=1)
// ---------------------------------------------------------------------------------------------

/// A `DriverFactory` whose agents are a script: no process, no model. Every message gets the same
/// short session (a thinking block, an explored group, streamed Markdown with inline code, usage).
pub struct FakeFactory;

struct FakeControl {
    tx: UnboundedSender<AgentEvent>,
}

const FAKE_TEXT: &str = "Revisé el módulo: el problema está en `skip_comment`, que cierra el comentario en el primer `*/`.\n\nEl arreglo es contar la profundidad:\n\n```rust\nfn skip_comment(&mut self) {\n    let mut depth = 1;\n    while depth > 0 {\n        match (self.bump(), self.peek()) {\n            ('/', '*') => depth += 1,\n            ('*', '/') => depth -= 1,\n            _ => {}\n        }\n    }\n}\n```\n\nNo ejecuté nada: esto es el agente de pruebas de Selachi (`SELACHI_CHAT_FAKE=1`), sin modelo ni procesos.";

fn fake_session() -> Vec<(u64, AgentEvent)> {
    let call =
        |id: &str, kind: ToolKind, title: &str, input: serde_json::Value| AgentEvent::ToolCall {
            id: id.into(),
            kind,
            title: title.into(),
            input_json: input,
        };
    let res = |id: &str, summary: &str| AgentEvent::ToolResult {
        id: id.into(),
        ok: true,
        summary: summary.into(),
    };
    let mut s = vec![
        (
            0,
            AgentEvent::SessionStarted {
                session_id: "fake-session".into(),
                model: Some("claude-opus-5-5".into()),
            },
        ),
        (0, AgentEvent::Status(AgentStatus::Working)),
        (
            250,
            AgentEvent::Thinking {
                text: "Miro cómo se cierran hoy los comentarios del lexer.".into(),
                delta: false,
            },
        ),
        (
            250,
            call(
                "f1",
                ToolKind::Search,
                "fn skip_comment",
                serde_json::json!({"pattern": "fn skip_comment"}),
            ),
        ),
        (150, res("f1", "crates/parser/src/lexer.rs:88")),
        (
            200,
            call(
                "f2",
                ToolKind::Read,
                "crates/parser/src/lexer.rs",
                serde_json::json!({"file_path": "crates/parser/src/lexer.rs"}),
            ),
        ),
        (150, res("f2", "212 líneas")),
    ];
    let chars: Vec<char> = FAKE_TEXT.chars().collect();
    s.extend(chars.chunks(12).map(|c| {
        (
            14,
            AgentEvent::Text {
                text: c.iter().collect(),
                delta: true,
            },
        )
    }));
    s.push((
        100,
        AgentEvent::Usage {
            input: 4200,
            output: 610,
            cache_create: 0,
            cache_read: 18_000,
            cost_usd: None,
            context_window: Some(1_000_000),
        },
    ));
    s.push((0, AgentEvent::Status(AgentStatus::Done)));
    s
}

impl AgentControl for FakeControl {
    fn send(&mut self, _text: &str) -> agents_driver::Result<()> {
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("selachi-fake-agent".into())
            .spawn(move || {
                for (delay, ev) in fake_session() {
                    if delay > 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                    if tx.unbounded_send(ev).is_err() {
                        break;
                    }
                }
            })
            .map(|_| ())
            .map_err(DriverError::Io)
    }

    fn interrupt(&mut self) -> agents_driver::Result<()> {
        let _ = self
            .tx
            .unbounded_send(AgentEvent::Status(AgentStatus::Done));
        Ok(())
    }

    fn approve(&mut self, _id: &str, _decision: ApprovalDecision) -> agents_driver::Result<()> {
        Ok(())
    }

    fn shutdown(&mut self) {
        let _ = self.tx.unbounded_send(AgentEvent::Exited { code: Some(0) });
    }
}

impl DriverFactory for FakeFactory {
    fn spawn(
        &self,
        _kind: &AgentKind,
        _cfg: &ThreadConfig,
        _job: Option<JobHandle>,
    ) -> agents_driver::Result<SpawnedAgent> {
        let (tx, events) = unbounded();
        Ok(SpawnedAgent {
            control: Box::new(FakeControl { tx }),
            events,
            pid: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_owners_never_collide_with_tab_owners() {
        // Tab owners are entity ids: small integers.
        assert!(owner_of(ThreadId(1)) > u64::from(u32::MAX));
        assert_ne!(owner_of(ThreadId(1)), owner_of(ThreadId(2)));
        assert_eq!(owner_of(ThreadId(5)) & (OWNER_BASE - 1), 5);
    }

    #[test]
    fn attention_follows_the_thread_state() {
        use ThreadStatus as S;
        assert_eq!(attention_for(S::Working, None), None);
        assert_eq!(attention_for(S::Done, None), None);
        assert_eq!(attention_for(S::Idle, None), None);
        assert_eq!(
            attention_for(S::WaitingForInput, None),
            Some(AttentionKind::WaitingInput)
        );
        assert_eq!(
            attention_for(S::Error, None),
            Some(AttentionKind::AgentError)
        );
        // A pending approval wins, whatever the status says.
        let k = attention_for(S::WaitingForInput, Some(("Bash".into(), "ls".into())));
        assert_eq!(
            k,
            Some(AttentionKind::Approval {
                tool: "Bash".into(),
                detail: "ls".into()
            })
        );
    }

    #[test]
    fn toasts_name_the_agent_and_the_need() {
        assert_eq!(
            toast_title(&AttentionKind::WaitingInput, "Claude Code"),
            "Claude Code espera tu respuesta"
        );
        assert!(
            toast_title(
                &AttentionKind::Approval {
                    tool: String::new(),
                    detail: String::new()
                },
                "Codex"
            )
            .contains("aprobación")
        );
    }

    #[test]
    fn the_scripted_session_ends_done_and_streams_text() {
        let s = fake_session();
        assert!(matches!(
            s.last(),
            Some((_, AgentEvent::Status(AgentStatus::Done)))
        ));
        let text: String = s
            .iter()
            .filter_map(|(_, e)| match e {
                AgentEvent::Text { text, delta: true } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, FAKE_TEXT);
    }
}
