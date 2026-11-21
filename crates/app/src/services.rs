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
use ui_viewer::{FileTreeProvider, ViewerEvent};

/// One shared [`Tracker`]: every tab registers its job here (`ui_procs::open_owner`).
#[derive(Clone)]
pub struct Services {
    pub tracker: SharedTracker,
    pub procs: Entity<ProcsModel>,
    pub doctor: Entity<DoctorModel>,
    /// The git panel's model (one for the app; it follows the project root the shell passes).
    pub git: Entity<GitModel>,
    /// The "files" panel provider (shared: the shell feeds it git markers).
    pub files: FileTreeProvider,
    /// Events of the file tree, taken once by the shell.
    pub viewer_events: Rc<RefCell<Option<UnboundedReceiver<ViewerEvent>>>>,
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
        let (tx, rx) = unbounded();
        let files = FileTreeProvider::new().on_event(move |ev, _| {
            let _ = tx.unbounded_send(ev.clone());
        });
        Self { tracker, procs, doctor, git, files, viewer_events: Rc::new(RefCell::new(Some(rx))) }
    }
}
