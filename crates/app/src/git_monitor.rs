//! Git state of the terminal's working directory, for the status bar.
//!
//! Everything that touches git (discovery, `status`, creating the watcher) runs on gpui's
//! background executor; the UI thread only applies the result. The monitor re-discovers when the
//! cwd leaves the current repository, drops the previous watcher then, and refreshes on watcher
//! events and after every finished command. Overlapping refreshes are coalesced: at most one
//! `git status` is in flight, and a request that arrives meanwhile queues a single follow-up.
//! Dropping the monitor (tab closed) drops the watcher and cancels the watch loop.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use git_svc::{Repo, RepoStatus, WatchHandle};
use gpui::{Context, Task};

use crate::paths::is_within;

/// Quiet time after file-system events before the status is re-read.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// What the status bar shows about the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitInfo {
    /// Branch name, or the short commit id when HEAD is detached.
    pub branch: String,
    /// Staged, modified, conflicted or untracked files exist.
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
}

fn info_from(st: &RepoStatus) -> GitInfo {
    let branch = st.branch.clone().unwrap_or_else(|| {
        st.head_oid.as_deref().map_or_else(|| "HEAD".to_string(), |oid| oid.chars().take(7).collect())
    });
    GitInfo {
        branch,
        dirty: st.is_dirty() || !st.untracked.is_empty(),
        ahead: st.ahead,
        behind: st.behind,
    }
}

struct Found {
    repo: Repo,
    info: Option<GitInfo>,
    watch: Option<(WatchHandle, UnboundedReceiver<()>)>,
}

/// Blocking: discovery, first status and watcher setup. Only call it off the UI thread.
fn discover(cwd: &Path) -> Option<Found> {
    let repo = Repo::discover(cwd).ok().flatten()?;
    let info = repo.status().ok().map(|s| info_from(&s));
    let watch = match repo.watch(DEBOUNCE) {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("git watcher unavailable for {}: {e}", repo.root.display());
            None
        },
    };
    Some(Found { repo, info, watch })
}

pub struct GitMonitor {
    cwd: Option<PathBuf>,
    repo: Option<Repo>,
    info: Option<GitInfo>,
    /// Bumped whenever the repository changes; results of older jobs are discarded.
    epoch: u64,
    refreshing: bool,
    refresh_queued: bool,
    watch: Option<WatchHandle>,
    watch_task: Option<Task<()>>,
}

impl GitMonitor {
    pub fn new() -> Self {
        Self {
            cwd: None,
            repo: None,
            info: None,
            epoch: 0,
            refreshing: false,
            refresh_queued: false,
            watch: None,
            watch_task: None,
        }
    }

    /// `None` outside a repository (the pill is hidden).
    pub fn info(&self) -> Option<&GitInfo> {
        self.info.as_ref()
    }

    /// The terminal changed directory. Re-discovers only when it left the current repository.
    pub fn set_cwd(&mut self, cwd: PathBuf, cx: &mut Context<Self>) {
        if self.cwd.as_ref() == Some(&cwd) {
            return;
        }
        let same_repo = self.repo.as_ref().is_some_and(|r| is_within(&cwd, &r.root));
        self.cwd = Some(cwd.clone());
        if same_repo {
            return;
        }

        self.epoch += 1;
        let epoch = self.epoch;
        self.repo = None;
        self.watch = None; // stops the old watcher
        self.watch_task = None;
        self.refreshing = false;
        self.refresh_queued = false;
        if self.info.take().is_some() {
            cx.notify();
        }

        self.watch_task = Some(cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { discover(&cwd) }).await;
            let Some(Found { repo, info, watch }) = found else { return };
            let (handle, rx) = match watch {
                Some((h, rx)) => (Some(h), Some(rx)),
                None => (None, None),
            };
            let applied = this.update(cx, |m, cx| {
                if m.epoch != epoch {
                    return false;
                }
                m.repo = Some(repo);
                m.watch = handle;
                if m.info != info {
                    m.info = info;
                    cx.notify();
                }
                true
            });
            if !applied.unwrap_or(false) {
                return;
            }
            let Some(mut rx) = rx else { return };
            while rx.next().await.is_some() {
                if this.update(cx, |m, cx| m.refresh(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Re-reads the status (after a finished command or a watcher event). Never overlaps.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.refreshing {
            self.refresh_queued = true;
            return;
        }
        self.refreshing = true;
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async move { repo.status().ok().map(|s| info_from(&s)) })
                .await;
            let _ = this.update(cx, |m, cx| {
                if m.epoch != epoch {
                    return;
                }
                m.refreshing = false;
                if let Some(info) = info
                    && m.info.as_ref() != Some(&info)
                {
                    m.info = Some(info);
                    cx.notify();
                }
                if std::mem::take(&mut m.refresh_queued) {
                    m.refresh(cx);
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use git_svc::parse_status;

    use super::*;

    #[test]
    fn info_reflects_branch_dirty_and_ahead_behind() {
        let st = parse_status(
            b"# branch.oid abcdef0123456789\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0",
        )
        .unwrap();
        assert_eq!(
            info_from(&st),
            GitInfo { branch: "main".into(), dirty: false, ahead: 2, behind: 1 }
        );
        let st = parse_status(b"# branch.oid abcdef0123456789\0# branch.head main\0? new.txt\0").unwrap();
        assert!(info_from(&st).dirty, "untracked files count as dirty");
        let st = parse_status(b"# branch.oid abcdef0123456789\0# branch.head (detached)\0").unwrap();
        assert_eq!(info_from(&st).branch, "abcdef0");
    }
}
