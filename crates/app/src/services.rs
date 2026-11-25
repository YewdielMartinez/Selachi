//! App-wide services shared by the shell and the panels: the process tracker behind the
//! `procs` panel and the orphan pill, the `doctor` model, the one `GitModel` of the git panel and
//! the file tree provider whose events the shell drains.
//!
//! Nothing here reads a repository or a directory until a panel is shown (`GitModel` is lazy; the
//! tree builds its listing on first paint).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::{App, AppContext as _, Entity, Global};
use proc_svc::Tracker;
use ui_doctor::DoctorModel;
use ui_git::{ExecMode, GitModel};
use ui_procs::{ProcsModel, SharedTracker};
use ui_review::AttentionModel;
use ui_usage::UsageModel;
use ui_viewer::{FileTreeProvider, ViewerEvent};

use crate::terminal_view::JobCtx;

/// One shared [`Tracker`]: every tab registers its job here (`ui_procs::open_owner`).
#[derive(Clone)]
pub struct Services {
    pub tracker: SharedTracker,
    pub procs: Entity<ProcsModel>,
    pub doctor: Entity<DoctorModel>,
    /// The git panel's model (one for the app; it follows the project root the shell passes).
    pub git: Entity<GitModel>,
    /// The usage panel's model; the status-bar monitor tells it when a scan ended.
    pub usage: Entity<UsageModel>,
    /// The attention queue (failed / long commands now; agents and chat later).
    pub attention: Entity<AttentionModel>,
    /// The "files" panel provider (shared: the shell feeds it git markers).
    pub files: FileTreeProvider,
    /// The chat hub (`chat.db` + threads). Created by the shell the first time chat is used, so a
    /// session without chat never opens the database.
    pub chat: Rc<RefCell<Option<Entity<ui_chat::ChatHub>>>>,
    /// The subagent map's model (PLAN section 21.2). It holds no watcher and no task until a map view
    /// is shown. Its chat source is filled when the chat hub opens (`agentmap_source`).
    pub agentmap: Entity<ui_agentmap::AgentMapModel>,
    pub agentmap_source: crate::agentmap_host::LazySource,
    /// Events of the file tree, taken once by the shell.
    pub viewer_events: Rc<RefCell<Option<UnboundedReceiver<ViewerEvent>>>>,
    /// Requests of delegated agents from the approvals pipe, taken once by the shell. `None` in
    /// tests and when the pipe could not be opened.
    pub approvals_rx: Rc<RefCell<Option<UnboundedReceiver<crate::approvals::ServerEvent>>>>,
}

impl Global for Services {}

impl Services {
    pub fn new(cx: &mut App) -> Self {
        let tracker: SharedTracker = Arc::new(Mutex::new(Tracker::new()));
        let procs = cx.new(|_| ProcsModel::with_tracker(tracker.clone()));
        let doctor = cx.new(|_| DoctorModel::new());
        let git = cx.new(|_| {
            let mut m = GitModel::new();
            // Confirmed sensitive operations run visibly in a terminal (PLAN section 7).
            m.set_exec_mode(ExecMode::Terminal);
            m
        });
        let usage = cx.new(|_| UsageModel::new());
        if let Some(path) = crate::usage_monitor::db_path() {
            usage.update(cx, |m, cx| m.set_db_path(path, cx));
        }
        let attention = cx.new(|_| AttentionModel::new());
        let (tx, rx) = unbounded();
        let files = FileTreeProvider::new().on_event(move |ev, _| {
            let _ = tx.unbounded_send(ev.clone());
        });
        let agentmap_source = crate::agentmap_host::LazySource::default();
        let runs_dir = crate::app_shell::data_dir()
            .map(|d| d.join("agent-runs"))
            .unwrap_or_default();
        let source: std::sync::Arc<dyn ui_agentmap::SessionSource> =
            std::sync::Arc::new(agentmap_source.clone());
        let agentmap = cx.new(|_| ui_agentmap::AgentMapModel::new(source, runs_dir));
        Self {
            tracker,
            procs,
            doctor,
            git,
            usage,
            attention,
            files,
            agentmap,
            agentmap_source,
            chat: Rc::new(RefCell::new(None)),
            viewer_events: Rc::new(RefCell::new(Some(rx))),
            approvals_rx: Rc::new(RefCell::new(None)),
        }
    }
}

/// Gives a new tab its process job (the `procs` panel groups by it) **before** its shell is
/// spawned, so the shell is born inside it. `None` when the tracker is unavailable: the tab then
/// runs untracked, as before.
pub fn open_tab_job(owner: u64, cwd: Option<&std::path::Path>, cx: &mut App) -> Option<JobCtx> {
    let services = cx.try_global::<Services>().cloned()?;
    let label = cwd.map_or_else(|| "Terminal".to_string(), crate::paths::tab_label);
    let meta = proc_svc::OwnerMeta {
        agent: None,
        label,
        cwd: cwd.map(std::path::Path::to_path_buf),
    };
    match ui_procs::open_owner(
        &services.tracker,
        owner,
        meta,
        proc_svc::JobOptions::default(),
    ) {
        Ok(job) => {
            let handle = job.job_handle as isize;
            log::debug!("tab job: owner {owner} handle {handle:#x}");
            services
                .procs
                .update(cx, |m, cx| m.watch_job_events(job.events, cx));
            Some(JobCtx {
                tracker: services.tracker.clone(),
                owner,
                handle,
            })
        }
        Err(e) => {
            log::warn!("could not create the process job of a tab: {e}");
            None
        }
    }
}
