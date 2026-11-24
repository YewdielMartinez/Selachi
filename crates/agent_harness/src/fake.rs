//! A scripted agent for tests of the binaries (no process, no model, no network).
//!
//! `SELACHI_AGENT_FAKE=<script.json>` makes `selachi-agent` (and therefore `selachi-agent mcp`, which
//! starts one `selachi-agent run` per delegation) use this driver and a resolver that accepts the
//! known agent names without looking at the machine. It can only make a run *less* real, never more,
//! so it is safe to leave compiled in; it is not documented in `--help`.
//!
//! The script is a JSON object, every field optional:
//!
//! ```json
//! { "reply": "all good", "usage": [10, 20], "write": {"a.txt": "alpha\nbeta\n"},
//!   "delay_ms": 0, "hang": false, "fail": null, "echo_prompt_len": false }
//! ```
//!
//! * `write`: files (relative to the run directory) created before the reply;
//! * `delay_ms`: wait before replying (lets a test observe `running`);
//! * `hang`: never reply (the run ends only by cancel / timeout);
//! * `fail`: the run ends in an error with this message;
//! * `echo_prompt_len`: append `prompt_len=<chars>` to the reply (large-payload tests).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agents_driver::{AgentEvent, AgentKind, AgentStatus, DriverError, ThreadConfig};
use chat_core::{AgentControl, ApprovalDecision, DriverFactory, JobHandle, SpawnedAgent};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use serde::Deserialize;

use crate::agents::{canonical_name, AgentResolver};
use crate::policy::{Refusal, RefusalCode};
use crate::run::HarnessBuilder;

/// Environment variable with the path of the script file.
pub const ENV_FAKE: &str = "SELACHI_AGENT_FAKE";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Script {
    pub reply: String,
    pub usage: Option<[u64; 2]>,
    pub write: BTreeMap<String, String>,
    pub delay_ms: u64,
    pub hang: bool,
    pub fail: Option<String>,
    pub echo_prompt_len: bool,
}

/// Maps the known names to kinds without looking at the machine.
pub struct FakeResolver;

impl AgentResolver for FakeResolver {
    fn resolve(&self, name: &str) -> Result<AgentKind, Refusal> {
        match canonical_name(name) {
            Some("claude") => Ok(AgentKind::ClaudeCode),
            Some("codex") => Ok(AgentKind::Codex),
            Some("opencode") => Ok(AgentKind::OpenCode),
            Some(other) => Ok(AgentKind::Acp {
                command: format!("{other} --acp"),
            }),
            None => Err(Refusal::new(
                RefusalCode::UnknownAgent,
                format!("unknown agent `{}`", name.trim()),
            )),
        }
    }
}

struct FakeFactory(Script);

struct FakeControl {
    script: Script,
    cwd: std::path::PathBuf,
    tx: UnboundedSender<AgentEvent>,
    stopped: Arc<AtomicBool>,
}

impl DriverFactory for FakeFactory {
    fn spawn(
        &self,
        _kind: &AgentKind,
        cfg: &ThreadConfig,
        _job: Option<JobHandle>,
    ) -> agents_driver::Result<SpawnedAgent> {
        let (tx, events) = unbounded();
        let control = FakeControl {
            script: self.0.clone(),
            cwd: cfg.cwd.clone(),
            tx,
            stopped: Arc::new(AtomicBool::new(false)),
        };
        Ok(SpawnedAgent {
            control: Box::new(control),
            events,
            pid: None,
        })
    }
}

impl AgentControl for FakeControl {
    fn send(&mut self, text: &str) -> agents_driver::Result<()> {
        let (script, cwd, tx, stopped) = (
            self.script.clone(),
            self.cwd.clone(),
            self.tx.clone(),
            self.stopped.clone(),
        );
        let prompt_len = text.chars().count();
        let send = move |e: AgentEvent| tx.unbounded_send(e).map_err(|_| DriverError::Closed);
        send(AgentEvent::Status(AgentStatus::Working))?;
        if script.hang {
            return Ok(());
        }
        let tx2 = self.tx.clone();
        std::thread::spawn(move || {
            let emit = |e: AgentEvent| {
                let _ = tx2.unbounded_send(e);
            };
            let mut waited = 0;
            while waited < script.delay_ms && !stopped.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                waited += 20;
            }
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            if let Some(msg) = &script.fail {
                emit(AgentEvent::Status(AgentStatus::Error(msg.clone())));
                return;
            }
            for (path, body) in &script.write {
                let p = cwd.join(path);
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(p, body);
            }
            let mut reply = script.reply.clone();
            if script.echo_prompt_len {
                reply.push_str(&format!(" prompt_len={prompt_len}"));
            }
            emit(AgentEvent::Text {
                text: reply,
                delta: false,
            });
            if let Some([i, o]) = script.usage {
                emit(AgentEvent::Usage {
                    input: i,
                    output: o,
                    cache_create: 0,
                    cache_read: 0,
                    cost_usd: None,
                    context_window: None,
                });
            }
            emit(AgentEvent::Status(AgentStatus::Done));
        });
        Ok(())
    }

    fn interrupt(&mut self) -> agents_driver::Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn approve(&mut self, _id: &str, _decision: ApprovalDecision) -> agents_driver::Result<()> {
        Ok(())
    }

    fn shutdown(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.tx.unbounded_send(AgentEvent::Exited { code: Some(0) });
    }
}

/// Install the scripted driver when `SELACHI_AGENT_FAKE` names a readable script.
pub fn apply_env(builder: HarnessBuilder) -> Result<HarnessBuilder, String> {
    let Some(path) = std::env::var_os(ENV_FAKE).filter(|p| !p.is_empty()) else {
        return Ok(builder);
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{ENV_FAKE}: cannot read {}: {e}", path.to_string_lossy()))?;
    let script: Script =
        serde_json::from_str(&text).map_err(|e| format!("{ENV_FAKE}: bad script: {e}"))?;
    Ok(builder
        .factory(Arc::new(FakeFactory(script)))
        .resolver(Arc::new(FakeResolver)))
}
