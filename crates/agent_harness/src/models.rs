//! Where the harness gets the models of an agent from: the catalog of `agents_driver::catalog`
//! (asked to the agent itself, cached with a TTL), behind a trait so tests never start a real CLI.
//!
//! A run that names a model or an effort is checked against the catalog **before** anything starts
//! ([`Harness::select_model`]): an unknown model or an effort the model does not offer is a
//! refusal that lists the valid options. A run that names neither never touches the catalog.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agents_driver::catalog::{self, AgentModels, DEFAULT_TTL, FETCH_TIMEOUT};

use crate::policy::{Refusal, RefusalCode};
use crate::run::Harness;

/// A cache older than this is refreshed once before a model is declared unknown (the agent may have
/// gained models since).
const RECHECK_AFTER: Duration = Duration::from_secs(300);

/// Source of the per-agent model lists.
pub trait ModelSource: Send + Sync {
    /// The list for `agent` (canonical name): the cache when it is younger than the TTL, else a
    /// refresh. Blocking only when it has to ask the agent. `None` = no list available.
    fn get(&self, agent: &str) -> Option<AgentModels>;
    /// Ask the agent again, whatever the cache says.
    fn refresh(&self, agent: &str) -> Option<AgentModels>;
    /// What is cached, without asking anything.
    fn cached(&self, agent: &str) -> Option<AgentModels>;
}

/// The real thing: `models-catalog.json` in the data folder plus the agent CLIs.
pub struct DiskModels {
    pub data_dir: PathBuf,
    pub ttl: Duration,
}

impl DiskModels {
    pub fn new(data_dir: impl Into<PathBuf>) -> DiskModels {
        DiskModels {
            data_dir: data_dir.into(),
            ttl: DEFAULT_TTL,
        }
    }
}

impl ModelSource for DiskModels {
    fn get(&self, agent: &str) -> Option<AgentModels> {
        Some(catalog::ensure(
            &self.data_dir,
            agent,
            self.ttl,
            FETCH_TIMEOUT,
        ))
    }
    fn refresh(&self, agent: &str) -> Option<AgentModels> {
        Some(catalog::refresh(&self.data_dir, agent, FETCH_TIMEOUT))
    }
    fn cached(&self, agent: &str) -> Option<AgentModels> {
        catalog::cached(&self.data_dir, agent)
    }
}

/// No catalog: models and efforts are passed through unchecked (the old behaviour).
pub struct NoModels;

impl ModelSource for NoModels {
    fn get(&self, _agent: &str) -> Option<AgentModels> {
        None
    }
    fn refresh(&self, _agent: &str) -> Option<AgentModels> {
        None
    }
    fn cached(&self, _agent: &str) -> Option<AgentModels> {
        None
    }
}

/// Fixed lists (tests, fakes). A refresh returns the same list and counts.
pub struct FixedModels {
    pub lists: Vec<AgentModels>,
    pub refreshes: std::sync::atomic::AtomicUsize,
}

impl FixedModels {
    pub fn new(lists: Vec<AgentModels>) -> Arc<FixedModels> {
        Arc::new(FixedModels {
            lists,
            refreshes: Default::default(),
        })
    }
}

impl ModelSource for FixedModels {
    fn get(&self, agent: &str) -> Option<AgentModels> {
        self.cached(agent)
    }
    fn refresh(&self, agent: &str) -> Option<AgentModels> {
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.cached(agent)
    }
    fn cached(&self, agent: &str) -> Option<AgentModels> {
        self.lists
            .iter()
            .find(|l| l.agent.eq_ignore_ascii_case(agent))
            .cloned()
    }
}

/// The checked pair a run starts with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChosenModel {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The agent's own id for the pair (`gemini-4-neon-medium`).
    pub real_id: Option<String>,
}

impl Harness {
    /// Check `model` / `effort` against the catalog of `agent` (a canonical name: `claude`, `codex`,
    /// `opencode`, `gemini`, `agy`). With neither given, nothing is looked up. Notes go to `notes`.
    pub fn select_model(
        &self,
        agent: &str,
        model: Option<&str>,
        effort: Option<&str>,
        notes: &mut Vec<String>,
    ) -> Result<ChosenModel, Refusal> {
        let model = model.map(str::trim).filter(|m| !m.is_empty());
        let effort = effort.map(str::trim).filter(|e| !e.is_empty());
        let passthrough = || ChosenModel {
            model: model.map(str::to_string),
            effort: effort.map(str::to_ascii_lowercase),
            real_id: model.map(str::to_string),
        };
        if model.is_none() && effort.is_none() {
            return Ok(ChosenModel::default());
        }
        let Some(list) = self.models.get(agent).filter(|l| !l.models.is_empty()) else {
            notes.push(format!(
                "the {agent} model list is not available, so {} was not checked against it (`selachi-agent list --models --refresh`)",
                model.or(effort).unwrap_or_default()
            ));
            return Ok(passthrough());
        };
        let mut result = list.resolve(model, effort);
        let old =
            catalog::now_ms().saturating_sub(list.fetched_ms) > RECHECK_AFTER.as_millis() as i64;
        let mut list = list;
        if result.is_err() && old {
            // maybe the agent gained models since the cache was written
            if let Some(fresh) = self.models.refresh(agent).filter(|l| !l.models.is_empty()) {
                result = fresh.resolve(model, effort);
                list = fresh;
            }
        }
        match result {
            Ok(sel) => {
                notes.extend(sel.notes);
                if let (Some(real), Some(m)) = (&sel.real_id, &sel.model) {
                    if real != m {
                        notes.push(format!(
                            "{agent} model {real} = {m} with effort {}",
                            sel.effort.as_deref().unwrap_or("default")
                        ));
                    }
                }
                Ok(ChosenModel {
                    model: sel.model,
                    effort: sel.effort,
                    real_id: sel.real_id,
                })
            }
            Err(e) => Err(Refusal::new(
                RefusalCode::UnknownModel,
                format!(
                    "{e} (catalog: {}, fetched {})",
                    list.source,
                    ago(list.fetched_ms)
                ),
            )),
        }
    }
}

fn ago(ms: i64) -> String {
    if ms <= 0 {
        return "never".into();
    }
    let s = catalog::now_ms().saturating_sub(ms) / 1000;
    match s {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", s / 60),
        5400..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}
