#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! A scripted agent driver for tests and the demo: no processes, no models. Derived from
//! `chat_core/tests/common` (std mutexes only, optional delays between events so a session can
//! be streamed in real time).
//!
//! Self-contained on purpose: `examples/chat_demo.rs` includes this very file with `#[path]`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agents_driver::{AgentEvent, AgentKind, DriverError, ThreadConfig};
use chat_core::{AgentControl, ApprovalDecision, DriverFactory, JobHandle, SpawnedAgent};
use futures::channel::mpsc::{UnboundedSender, unbounded};

/// One scripted event, optionally after a pause.
#[derive(Clone, Debug)]
pub struct Step {
    pub delay_ms: u64,
    pub event: AgentEvent,
}

impl Step {
    pub fn now(event: AgentEvent) -> Self {
        Self { delay_ms: 0, event }
    }

    pub fn after(delay_ms: u64, event: AgentEvent) -> Self {
        Self { delay_ms, event }
    }
}

#[derive(Default)]
pub struct Rec {
    pub spawns: Vec<(AgentKind, ThreadConfig, Option<JobHandle>)>,
    pub sent: Vec<String>,
    pub approvals: Vec<(String, ApprovalDecision)>,
    pub interrupts: usize,
    pub shutdowns: usize,
    txs: Vec<UnboundedSender<AgentEvent>>,
}

pub type Script = Box<dyn Fn(usize, &str) -> Vec<Step> + Send + Sync>;

pub struct FakeFactory {
    pub rec: Arc<Mutex<Rec>>,
    script: Arc<Script>,
    pub fail_spawn: AtomicBool,
    /// Emitted when `interrupt` is called.
    pub on_interrupt: Mutex<Vec<AgentEvent>>,
    /// Emitted when an approval is answered (the demo continues its session).
    pub on_approve: Mutex<Vec<Step>>,
}

impl FakeFactory {
    pub fn new(script: impl Fn(usize, &str) -> Vec<Step> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            rec: Arc::new(Mutex::new(Rec::default())),
            script: Arc::new(Box::new(script)),
            fail_spawn: AtomicBool::new(false),
            on_interrupt: Mutex::new(Vec::new()),
            on_approve: Mutex::new(Vec::new()),
        })
    }

    /// A factory whose agent never answers by itself (tests push events with [`FakeFactory::push`]).
    pub fn silent() -> Arc<Self> {
        Self::new(|_, _| Vec::new())
    }

    /// Pushes events into the most recently started agent, as if it produced them now.
    pub fn push(&self, events: Vec<AgentEvent>) {
        let rec = self.rec.lock().unwrap();
        let tx = rec.txs.last().expect("no agent running");
        for e in events {
            tx.unbounded_send(e).unwrap();
        }
    }

    pub fn sent(&self) -> Vec<String> {
        self.rec.lock().unwrap().sent.clone()
    }

    pub fn approvals(&self) -> Vec<(String, ApprovalDecision)> {
        self.rec.lock().unwrap().approvals.clone()
    }

    pub fn interrupts(&self) -> usize {
        self.rec.lock().unwrap().interrupts
    }

    pub fn spawn_count(&self) -> usize {
        self.rec.lock().unwrap().spawns.len()
    }
}

struct FakeControl {
    owner: Arc<FakeFactory>,
    tx: UnboundedSender<AgentEvent>,
}

impl AgentControl for FakeControl {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        let n = {
            let mut r = self.owner.rec.lock().unwrap();
            r.sent.push(text.to_string());
            r.sent.len() - 1
        };
        let steps = (self.owner.script)(n, text);
        if steps.iter().all(|s| s.delay_ms == 0) {
            for s in steps {
                self.tx.unbounded_send(s.event).map_err(|_| DriverError::Closed)?;
            }
        } else {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                for s in steps {
                    if s.delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(s.delay_ms));
                    }
                    if tx.unbounded_send(s.event).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(())
    }

    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.owner.rec.lock().unwrap().interrupts += 1;
        let events = self.owner.on_interrupt.lock().unwrap().clone();
        for e in events {
            let _ = self.tx.unbounded_send(e);
        }
        Ok(())
    }

    fn approve(&mut self, id: &str, decision: ApprovalDecision) -> agents_driver::Result<()> {
        self.owner.rec.lock().unwrap().approvals.push((id.to_string(), decision));
        let steps = self.owner.on_approve.lock().unwrap().clone();
        if !steps.is_empty() {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                for s in steps {
                    if s.delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(s.delay_ms));
                    }
                    if tx.unbounded_send(s.event).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        self.owner.rec.lock().unwrap().shutdowns += 1;
        let _ = self.tx.unbounded_send(AgentEvent::Exited { code: Some(0) });
    }
}

/// The `DriverFactory` to hand to `ChatStore::builder().factory(..)`.
pub struct FactoryHandle(pub Arc<FakeFactory>);

impl DriverFactory for FactoryHandle {
    fn spawn(&self, kind: &AgentKind, cfg: &ThreadConfig, job: Option<JobHandle>) -> agents_driver::Result<SpawnedAgent> {
        let f = &self.0;
        if f.fail_spawn.load(Ordering::SeqCst) {
            return Err(DriverError::NotFound("claude".into()));
        }
        let (tx, events) = unbounded();
        {
            let mut r = f.rec.lock().unwrap();
            r.spawns.push((kind.clone(), cfg.clone(), job));
            r.txs.push(tx.clone());
        }
        Ok(SpawnedAgent { control: Box::new(FakeControl { owner: f.clone(), tx }), events, pid: Some(4242) })
    }
}
