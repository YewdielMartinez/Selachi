//! Job Objects for delegated agents: every run's agent process tree lives in its own job with
//! `KILL_ON_JOB_CLOSE`, so nothing a delegated agent spawned can outlive the harness process
//! (crash, Ctrl+C, timeout, cancel).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use chat_core::{JobHandle, OwnerInfo, ProcessHooks, ThreadId};
use parking_lot::Mutex;
use proc_svc::{JobGroup, JobOptions};

/// `chat_core::ProcessHooks` backed by `proc_svc::JobGroup`.
#[derive(Default)]
pub struct JobHooks {
    jobs: Mutex<HashMap<ThreadId, JobGroup>>,
    opened: AtomicUsize,
    assigned: AtomicUsize,
}

impl JobHooks {
    /// Job Objects created so far.
    pub fn jobs_opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    /// Agent processes that had to be assigned to the job after the fact (the spawner normally
    /// creates them inside it).
    pub fn late_assignments(&self) -> usize {
        self.assigned.load(Ordering::SeqCst)
    }

    /// Pids currently inside the job of `thread` (empty when there is none).
    pub fn members(&self, thread: ThreadId) -> Vec<u32> {
        self.jobs
            .lock()
            .get(&thread)
            .map(JobGroup::members)
            .unwrap_or_default()
    }
}

impl ProcessHooks for JobHooks {
    fn open_owner(&self, thread_id: ThreadId, info: &OwnerInfo) -> Option<JobHandle> {
        let label = format!("delegation-{}-{}", info.agent, thread_id);
        match JobGroup::with_options(
            label,
            JobOptions {
                kill_on_close: true,
                breakaway_ok: false,
            },
        ) {
            Ok(job) => {
                let handle = job.raw_handle() as usize;
                self.jobs.lock().insert(thread_id, job);
                self.opened.fetch_add(1, Ordering::SeqCst);
                Some(handle)
            }
            Err(e) => {
                log::warn!(
                    "agent_harness: cannot create the Job Object of thread {thread_id}: {e}"
                );
                None
            }
        }
    }

    fn register_root(&self, thread_id: ThreadId, pid: u32) {
        let jobs = self.jobs.lock();
        let Some(job) = jobs.get(&thread_id) else {
            return;
        };
        if job.contains(pid) {
            return;
        }
        match job.assign(pid) {
            Ok(()) => {
                self.assigned.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => log::warn!(
                "agent_harness: could not put pid {pid} into the job of thread {thread_id}: {e}"
            ),
        }
    }

    fn close_owner(&self, thread_id: ThreadId) {
        // Dropping the group closes the last handle: KILL_ON_JOB_CLOSE ends whatever is left.
        self.jobs.lock().remove(&thread_id);
    }
}
