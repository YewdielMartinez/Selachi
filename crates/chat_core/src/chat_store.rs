//! `ChatStore`: the entry point of the chat model. Creates, lists, opens, archives and deletes
//! threads; owns `chat.db` and the shared services (driver factory, process hooks, prices).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agents_svc::PricingTable;
use futures::channel::mpsc::UnboundedReceiver;
use parking_lot::Mutex;

use crate::db::{default_db_path, ChatDb, NewThread};
use crate::error::{ChatError, Result};
use crate::hooks::{DefaultDriverFactory, DriverFactory, ProcessHooks};
use crate::model::{
    now_ms, path_key, Archived, ThreadFilter, ThreadGroups, ThreadId, ThreadRecord, ThreadSpec, ThreadStatus,
};
use crate::runtime::{Shared, ThreadRuntime, ThreadUpdate};
use crate::state::{Changed, CostPolicy};

/// Configure and open a [`ChatStore`].
#[derive(Default)]
pub struct ChatStoreBuilder {
    path: Option<PathBuf>,
    factory: Option<Arc<dyn DriverFactory>>,
    hooks: Option<Arc<dyn ProcessHooks>>,
    pricing: Option<PricingTable>,
    policy: CostPolicy,
}

impl ChatStoreBuilder {
    /// Database file (default: `%APPDATA%\Selachi\chat.db`, or `SELACHI_DATA_DIR`).
    pub fn path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// How agents are spawned (default: [`DefaultDriverFactory`], the real CLIs).
    pub fn factory(mut self, factory: Arc<dyn DriverFactory>) -> Self {
        self.factory = Some(factory);
        self
    }

    /// Process-ownership callbacks (`proc_svc` owners), see [`ProcessHooks`].
    pub fn hooks(mut self, hooks: Arc<dyn ProcessHooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Price table for estimated costs (default: the embedded `agents_svc` table).
    pub fn pricing(mut self, pricing: PricingTable) -> Self {
        self.pricing = Some(pricing);
        self
    }

    pub fn cost_policy(mut self, policy: CostPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn open(self) -> Result<ChatStore> {
        let path = match self.path {
            Some(p) => p,
            None => default_db_path()?,
        };
        let db = ChatDb::open(&path)?;
        let factory = self.factory.unwrap_or_else(|| Arc::new(DefaultDriverFactory));
        let pricing = Arc::new(self.pricing.unwrap_or_else(PricingTable::embedded));
        Ok(ChatStore {
            inner: Arc::new(StoreInner {
                shared: Arc::new(Shared::new(db, factory, self.hooks, pricing, self.policy)),
                runtimes: Mutex::new(HashMap::new()),
            }),
        })
    }
}

struct StoreInner {
    shared: Arc<Shared>,
    runtimes: Mutex<HashMap<ThreadId, ThreadRuntime>>,
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        let rts: Vec<ThreadRuntime> = self.runtimes.lock().drain().map(|(_, r)| r).collect();
        for rt in rts {
            rt.close();
        }
    }
}

/// Handle to the chat database and its live threads. Cheap to clone.
#[derive(Clone)]
pub struct ChatStore {
    inner: Arc<StoreInner>,
}

impl std::fmt::Debug for ChatStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatStore").field("db", &self.inner.shared.db).finish()
    }
}

/// What deleting a thread would do to its worktree (see [`ChatStore::delete_preview`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePreview {
    pub path: PathBuf,
    /// The directory is still on disk.
    pub exists: bool,
    pub branch: Option<String>,
    /// Uncommitted changes (`None`: could not be determined).
    pub dirty: Option<bool>,
    /// Human-readable warnings from git (uncommitted files, unpushed commits, lock...).
    pub warnings: Vec<String>,
    /// `git diff --stat` of what would be lost.
    pub diff_stat: Option<String>,
    /// Ask the user before removing it.
    pub requires_confirmation: bool,
}

/// Answer of [`ChatStore::delete_preview`]: show it, then call `delete_thread`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePreview {
    pub thread: ThreadRecord,
    pub message_count: usize,
    /// `None` when the thread has no worktree.
    pub worktree: Option<WorktreePreview>,
}

/// Options of [`ChatStore::delete_thread`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteOptions {
    /// Also remove the thread's worktree directory (the branch is always kept). Only with the
    /// user's confirmation.
    pub remove_worktree: bool,
    /// Remove a dirty or locked worktree anyway (discards uncommitted changes).
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReport {
    pub worktree_removed: bool,
    /// Branch left behind by a removed worktree.
    pub branch_kept: Option<String>,
}

impl ChatStore {
    pub fn builder() -> ChatStoreBuilder {
        ChatStoreBuilder::default()
    }

    /// Open `chat.db` at `path` with the default services (real agent CLIs, no process hooks).
    pub fn open_at(path: impl Into<PathBuf>) -> Result<ChatStore> {
        Self::builder().path(path).open()
    }

    /// Open `%APPDATA%\Selachi\chat.db` (`SELACHI_DATA_DIR` overrides the directory).
    pub fn open_default() -> Result<ChatStore> {
        Self::builder().open()
    }

    pub fn db_path(&self) -> &Path {
        self.inner.shared.db.path()
    }

    /// Stream of [`ThreadUpdate`]s for every thread (coalesced: at most one per 16 ms per
    /// thread). Each call returns an independent receiver.
    pub fn updates(&self) -> UnboundedReceiver<ThreadUpdate> {
        self.inner.shared.subscribe()
    }

    fn announce(&self, id: ThreadId, status: ThreadStatus) {
        self.inner.shared.publish(ThreadUpdate {
            thread_id: id,
            revision: 0,
            status,
            changed: Changed { list: true, ..Changed::default() },
        });
    }

    // ----- create ----------------------------------------------------------

    /// Create a thread. With `spec.use_worktree` a git worktree `selachi/<slug>` is created first
    /// (`NotAGitRepo` if the project is not a repository) and the agent will run inside it.
    pub fn create_thread(&self, spec: ThreadSpec) -> Result<ThreadRecord> {
        if !spec.project_root.is_dir() {
            return Err(ChatError::Invalid(format!("project folder not found: {}", spec.project_root.display())));
        }
        let title = spec.title.as_deref().map(str::trim).unwrap_or("").to_string();
        let worktree_path = if spec.use_worktree {
            Some(create_worktree(&spec.project_root, &title)?)
        } else {
            None
        };
        let id = self.inner.shared.db.insert_thread(NewThread { spec, title, worktree_path })?;
        let rec = self
            .inner
            .shared
            .db
            .get_thread(id)?
            .ok_or_else(|| ChatError::NotFound(format!("thread {id}")))?;
        self.announce(id, rec.status);
        Ok(rec)
    }

    // ----- read ------------------------------------------------------------

    pub fn get_thread(&self, id: ThreadId) -> Result<Option<ThreadRecord>> {
        if let Some(rt) = self.inner.runtimes.lock().get(&id) {
            return Ok(Some(rt.record()));
        }
        self.inner.shared.db.get_thread(id)
    }

    /// Threads matching `filter`, newest first. Open threads report their live status and
    /// metadata.
    pub fn list_threads(&self, filter: &ThreadFilter) -> Result<Vec<ThreadRecord>> {
        let mut all = self.inner.shared.db.list_threads()?;
        {
            let rts = self.inner.runtimes.lock();
            for t in &mut all {
                if let Some(rt) = rts.get(&t.id) {
                    *t = rt.record();
                }
            }
        }
        let want_project = filter.project.as_deref().map(path_key);
        all.retain(|t| {
            (match filter.archived {
                Archived::Exclude => !t.archived,
                Archived::Only => t.archived,
                Archived::Include => true,
            }) && want_project.as_ref().is_none_or(|p| *p == path_key(&t.project_root))
                && filter.status.is_none_or(|s| s == t.status)
                && filter.group.is_none_or(|g| g == t.group())
        });
        all.sort_by_key(|t| std::cmp::Reverse((t.updated_ms, t.id)));
        Ok(all)
    }

    /// [`Self::list_threads`] split into Working / Waiting / Settled for the sidebar.
    pub fn grouped_threads(&self, filter: &ThreadFilter) -> Result<ThreadGroups> {
        Ok(ThreadGroups::from_threads(&self.list_threads(filter)?))
    }

    // ----- open / close ----------------------------------------------------

    /// The live runtime of a thread, loading its messages from the database the first time.
    /// Opening costs no thread and no process: the agent starts on the first `send`.
    pub fn open(&self, id: ThreadId) -> Result<ThreadRuntime> {
        let mut rts = self.inner.runtimes.lock();
        if let Some(rt) = rts.get(&id) {
            if !rt.is_closed() {
                return Ok(rt.clone());
            }
        }
        let rec = self
            .inner
            .shared
            .db
            .get_thread(id)?
            .ok_or_else(|| ChatError::NotFound(format!("thread {id}")))?;
        let rt = ThreadRuntime::load(self.inner.shared.clone(), rec)?;
        rts.insert(id, rt.clone());
        Ok(rt)
    }

    /// Close the runtime of a thread (stops its agent). The thread stays in the database.
    pub fn close_thread(&self, id: ThreadId) {
        let rt = self.inner.runtimes.lock().remove(&id);
        if let Some(rt) = rt {
            rt.close();
            self.announce(id, ThreadStatus::Idle);
        }
    }

    /// Rename a thread.
    pub fn rename_thread(&self, id: ThreadId, title: &str) -> Result<()> {
        let title = title.trim().to_string();
        self.inner.shared.db.set_title(id, title.clone())?;
        self.inner.shared.db.flush()?;
        if let Some(rt) = self.inner.runtimes.lock().get(&id) {
            rt.set_title(&title);
        }
        let status = self.get_thread(id)?.map_or(ThreadStatus::Idle, |t| t.status);
        self.announce(id, status);
        Ok(())
    }

    /// Archive (hide from the sidebar, stop the agent) or restore a thread.
    pub fn archive_thread(&self, id: ThreadId, archived: bool) -> Result<()> {
        if archived {
            self.close_thread(id);
        }
        self.inner.shared.db.set_archived(id, archived)?;
        self.inner.shared.db.flush()?;
        self.announce(id, ThreadStatus::Idle);
        Ok(())
    }

    // ----- delete ----------------------------------------------------------

    /// What deleting `id` involves: message count and, for a worktree thread, its path, branch,
    /// dirty state and git's warnings. Show this and ask before `delete_thread` with
    /// `remove_worktree`.
    pub fn delete_preview(&self, id: ThreadId) -> Result<DeletePreview> {
        let thread = self
            .get_thread(id)?
            .ok_or_else(|| ChatError::NotFound(format!("thread {id}")))?;
        let message_count = self.inner.shared.db.message_count(id)?;
        let worktree = thread.worktree_path.as_ref().map(|wt| preview_worktree(&thread.project_root, wt));
        Ok(DeletePreview { thread, message_count, worktree })
    }

    /// Delete a thread and its history (cascade). The agent is stopped first. The worktree is
    /// removed only with `opts.remove_worktree`; a dirty or locked one additionally needs
    /// `opts.force` (otherwise this fails and the thread is kept).
    pub fn delete_thread(&self, id: ThreadId, opts: DeleteOptions) -> Result<DeleteReport> {
        let thread = self
            .get_thread(id)?
            .ok_or_else(|| ChatError::NotFound(format!("thread {id}")))?;
        let rt = self.inner.runtimes.lock().remove(&id);
        if let Some(rt) = rt {
            rt.close();
        }
        let mut report = DeleteReport { worktree_removed: false, branch_kept: None };
        if let (true, Some(wt)) = (opts.remove_worktree, thread.worktree_path.as_ref()) {
            let repo = git_svc::Repo::discover(&thread.project_root)?
                .ok_or_else(|| ChatError::NotAGitRepo(thread.project_root.clone()))?;
            let branch = repo
                .worktrees()?
                .into_iter()
                .find(|w| path_key(&w.path) == path_key(wt))
                .and_then(|w| w.branch);
            repo.remove_worktree(wt, opts.force)?;
            report.worktree_removed = true;
            report.branch_kept = branch;
        }
        self.inner.shared.db.delete_thread(id)?;
        self.announce(id, ThreadStatus::Idle);
        Ok(report)
    }

    // ----- lifecycle -------------------------------------------------------

    /// Block until everything queued is committed to `chat.db`.
    pub fn flush(&self) -> Result<()> {
        self.inner.shared.db.flush()
    }

    /// Close every open thread (stopping all agents) and flush. Also happens when the last
    /// clone of the store is dropped.
    pub fn shutdown(&self) {
        let rts: Vec<ThreadRuntime> = self.inner.runtimes.lock().drain().map(|(_, r)| r).collect();
        for rt in rts {
            rt.close();
        }
        if let Err(e) = self.flush() {
            log::warn!("chat_core: flush on shutdown failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Worktrees
// ---------------------------------------------------------------------------

/// `"Arregla el parser (v2)"` -> `"arregla-el-parser-v2"`: ASCII, lowercase, hyphens, <= 40 chars.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in title.chars() {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' | 'Á' | 'À' | 'Ä' | 'Â' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'Ó' | 'Ò' | 'Ö' | 'Ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            other => other,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(40).collect();
    trimmed.trim_end_matches('-').to_string()
}

fn base36(mut n: u64) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut s = Vec::new();
    while n > 0 {
        s.push(D[(n % 36) as usize]);
        n /= 36;
    }
    s.reverse();
    String::from_utf8_lossy(&s).into_owned()
}

/// Worktree short name for a thread title: the slug, or `hilo-<time>` when untitled or the slug
/// is not a valid worktree name.
pub fn worktree_name(title: &str) -> String {
    let slug = slugify(title);
    let name = if slug.is_empty() {
        format!("hilo-{}", base36((now_ms() / 1000) as u64))
    } else {
        slug
    };
    if git_svc::validate_worktree_name(&name).is_ok() {
        name
    } else {
        format!("hilo-{name}")
    }
}

fn create_worktree(project_root: &Path, title: &str) -> Result<PathBuf> {
    let repo = git_svc::Repo::discover(project_root)?.ok_or_else(|| ChatError::NotAGitRepo(project_root.to_path_buf()))?;
    let base = worktree_name(title);
    let mut last_err = None;
    for n in 1..=20 {
        let name = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        match repo.create_worktree(&git_svc::NewWorktree::new(&name)) {
            Ok(wt) => return Ok(wt.path),
            Err(git_svc::Error::InvalidArgument(m)) if m.contains("already exists") || m.contains("already a worktree") => {
                last_err = Some(git_svc::Error::InvalidArgument(m));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(last_err
        .map(ChatError::from)
        .unwrap_or_else(|| ChatError::Invalid("could not find a free worktree name".into())))
}

fn preview_worktree(project_root: &Path, wt: &Path) -> WorktreePreview {
    let mut p = WorktreePreview {
        path: wt.to_path_buf(),
        exists: wt.is_dir(),
        branch: None,
        dirty: None,
        warnings: Vec::new(),
        diff_stat: None,
        requires_confirmation: false,
    };
    let repo = match git_svc::Repo::discover(project_root) {
        Ok(Some(r)) => r,
        _ => {
            p.warnings.push("no se pudo abrir el repositorio del proyecto".into());
            p.requires_confirmation = p.exists;
            return p;
        }
    };
    if let Ok(list) = repo.worktrees() {
        p.branch = list.into_iter().find(|w| path_key(&w.path) == path_key(wt)).and_then(|w| w.branch);
    }
    if p.exists {
        p.dirty = repo.worktree_is_dirty(wt).ok();
    }
    match repo.preflight_remove_worktree(wt) {
        Ok(pf) => {
            p.warnings = pf.warnings;
            p.diff_stat = pf.diff_stat;
            p.requires_confirmation = pf.requires_confirmation || p.dirty == Some(true);
        }
        Err(e) => {
            p.warnings.push(e.to_string());
            p.requires_confirmation = true;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Arregla el parser (v2)"), "arregla-el-parser-v2");
        assert_eq!(slugify("  ¡Añadir pestañas! "), "anadir-pestanas");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify(&"a".repeat(100)).len(), 40);
        assert!(worktree_name("").starts_with("hilo-"));
        assert_eq!(worktree_name("Fix: login"), "fix-login");
        // reserved device name on Windows -> prefixed
        assert_eq!(worktree_name("con"), "hilo-con");
    }
}
