//! Checkpoints (PLAN A10): a safety net before an agent runs and "undo what the agent did".
//!
//! # Design
//!
//! A checkpoint is a stash-like commit stored under `refs/selachi/checkpoints/<id>`:
//!
//! * the commit's TREE is the snapshot of the whole working tree: tracked files as they are on
//!   disk plus untracked files that are not ignored (built with `git add -A` into a TEMPORARY
//!   index file, then `git write-tree`);
//! * its parents are `[HEAD (if born), index-commit]`, exactly like `git stash create`; the
//!   index commit's tree is the real index as it was (so the staged state is recoverable);
//! * the commit message carries `Selachi-*` trailers (label, worktree root, creation time,
//!   whether untracked files are included, HEAD, "auto" marker). The ref namespace is shared by
//!   all worktrees of a repository, so listing filters on `Selachi-Worktree`.
//!
//! Creating one never touches HEAD, the real index, the stash list or any file of the working
//! tree: the only writes are new objects, a temporary index inside the git dir (deleted after)
//! and the new ref. All snapshots are taken with `core.autocrlf=false` so bytes round-trip
//! exactly. Ignored files are never read, snapshotted, overwritten or deleted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::action::Preflight;
use crate::error::{Error, Result};
use crate::exec::{run_git_ok, run_git_ok_with, run_git_with, Opts, NETWORK_TIMEOUT};
use crate::pathutil::{fold_case, has_parent_dir, norm_key, same_path};
use crate::repo::Repo;

const REF_PREFIX: &str = "refs/selachi/checkpoints/";
/// Snapshots may hash many files; allow more than the 20 s used for reads.
const SNAP_TIMEOUT: Duration = NETWORK_TIMEOUT;
/// Config forced on every snapshot/restore command so bytes are stored and written verbatim.
const RAW_CFG: [&str; 6] = ["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", "-c", "commit.gpgsign=false"];

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A stored snapshot. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// `<unix-millis>-<7 hex of the commit>`; safe to use in a ref name.
    pub id: String,
    pub label: String,
    pub created: SystemTime,
    /// The stash-like commit object id (`refs/selachi/checkpoints/<id>` points at it).
    pub commit: String,
    /// Snapshot includes untracked (not ignored) files.
    pub includes_untracked: bool,
    /// Root of the worktree it was taken in.
    pub worktree: PathBuf,
    /// HEAD at creation (`None` on an unborn branch).
    pub head: Option<String>,
    /// Created automatically (e.g. before a restore) rather than by the user/agent flow.
    pub auto: bool,
}

/// What [`Repo::restore_checkpoint`] restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreMode {
    /// Only these files/directories (relative to the worktree root, or absolute inside it).
    /// A directory restores everything under it.
    Files(Vec<PathBuf>),
    /// Everything: tracked + untracked files go back to the snapshot, files created after it are
    /// deleted, and the index is reset to the staged state of the snapshot.
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    /// Exists now, did not at the checkpoint.
    Added,
    Modified,
    /// Existed at the checkpoint, gone now.
    Deleted,
    TypeChanged,
}

/// One file that differs between a checkpoint and the current working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointChange {
    pub path: PathBuf,
    pub kind: DeltaKind,
}

/// Exactly what a restore would do, relative to the worktree root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePlan {
    /// Files that existed at the checkpoint and are missing now: they will be re-created.
    pub create: Vec<PathBuf>,
    /// Files whose content/mode differs: they will be overwritten with the snapshot's version.
    pub overwrite: Vec<PathBuf>,
    /// Files created after the checkpoint: they will be deleted.
    pub delete: Vec<PathBuf>,
    /// Entries left alone, with the reason (submodules, ignored files in the way).
    pub skipped: Vec<(PathBuf, String)>,
}

impl RestorePlan {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty() && self.overwrite.is_empty() && self.delete.is_empty()
    }
}

/// Outcome of [`Repo::restore_checkpoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// The automatic "antes de restaurar" checkpoint taken first (`None` when nothing differed,
    /// so there was nothing to undo and no checkpoint was kept).
    pub auto_checkpoint: Option<Checkpoint>,
    /// Re-created or overwritten files.
    pub restored: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
}

struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    /// A private copy of the real index (keeps the stat cache, so `git add -A` stays fast).
    /// It lives in the git dir so split-index shared files resolve.
    fn copy_of(repo: &Repo) -> Result<Self> {
        let real = repo.git_dir.join("index");
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = repo.git_dir.join(format!("selachi-tmp-index-{}-{n}", std::process::id()));
        let tmp = Self { path };
        if real.is_file() {
            std::fs::copy(&real, &tmp.path)?;
        }
        Ok(tmp)
    }

    fn empty(repo: &Repo) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        Self { path: repo.git_dir.join(format!("selachi-tmp-index-{}-{n}", std::process::id())) }
    }

    fn opts(&self) -> Opts<'_> {
        Opts { envs: vec![("GIT_INDEX_FILE", self.path.clone().into_os_string())], stdin: None }
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
    }
}

struct Entry {
    status: char,
    old_mode: String,
    new_mode: String,
    path: String,
}

fn parse_raw_diff(out: &[u8]) -> Result<Vec<Entry>> {
    let mut toks = out.split(|b| *b == 0).filter(|t| !t.is_empty());
    let mut v = Vec::new();
    while let Some(meta) = toks.next() {
        let meta = String::from_utf8_lossy(meta);
        let meta = meta.strip_prefix(':').ok_or_else(|| Error::Parse(format!("bad raw diff line {meta:?}")))?;
        let f: Vec<&str> = meta.split(' ').collect();
        if f.len() < 5 {
            return Err(Error::Parse(format!("bad raw diff line {meta:?}")));
        }
        let path = toks.next().ok_or_else(|| Error::Parse("raw diff: missing path".into()))?;
        v.push(Entry {
            status: f[4].chars().next().unwrap_or('M'),
            old_mode: f[0].to_owned(),
            new_mode: f[1].to_owned(),
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
    Ok(v)
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(Error::InvalidArgument(format!("invalid checkpoint id {id:?}")));
    }
    Ok(())
}

fn one_line(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = t.trim();
    let t: String = t.chars().take(200).collect();
    if t.is_empty() {
        "checkpoint".to_owned()
    } else {
        t
    }
}

fn parse_checkpoints(out: &str) -> Vec<Checkpoint> {
    let mut v = Vec::new();
    for rec in out.split('\u{1e}') {
        let rec = rec.trim_start_matches(['\n', '\r']);
        let mut f = rec.splitn(3, '\u{1f}');
        let (Some(name), Some(oid), Some(body)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Some(id) = name.strip_prefix(REF_PREFIX) else { continue };
        let trailer = |key: &str| {
            body.lines().find_map(|l| l.strip_prefix(key).map(|r| r.trim().to_owned()))
        };
        if trailer("Selachi-Checkpoint:").is_none() {
            continue;
        }
        let ms: u64 = trailer("Selachi-Created-Ms:").and_then(|s| s.parse().ok()).unwrap_or(0);
        v.push(Checkpoint {
            id: id.to_owned(),
            label: trailer("Selachi-Label:").unwrap_or_default(),
            created: UNIX_EPOCH + Duration::from_millis(ms),
            commit: oid.to_owned(),
            includes_untracked: trailer("Selachi-Untracked:").as_deref() == Some("yes"),
            worktree: PathBuf::from(trailer("Selachi-Worktree:").unwrap_or_default()),
            head: trailer("Selachi-Head:").filter(|h| h != "none"),
            auto: trailer("Selachi-Auto:").is_some(),
        });
    }
    v.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.id.cmp(&a.id)));
    v
}

impl Repo {
    // ---------------------------------------------------------------- internals

    fn snap_git(&self, opts: &Opts<'_>, args: &[&str]) -> Result<crate::exec::Raw> {
        let mut full: Vec<&str> = RAW_CFG.to_vec();
        full.extend_from_slice(args);
        run_git_ok_with(&self.root, &full, opts, SNAP_TIMEOUT)
    }

    fn head_oid(&self) -> Option<String> {
        let raw = run_git_with(
            &self.root,
            &["rev-parse", "--verify", "-q", "HEAD"],
            &Opts::default(),
            SNAP_TIMEOUT,
        )
        .ok()?;
        raw.success().then(|| raw.stdout_string().trim().to_owned())
    }

    fn empty_tree(&self) -> Result<String> {
        let opts = Opts { envs: Vec::new(), stdin: Some(b"") };
        Ok(self.snap_git(&opts, &["mktree"])?.stdout_string().trim().to_owned())
    }

    /// `(index_tree, worktree_tree)` of the current state. Writes objects only.
    fn snapshot_trees(&self, include_untracked: bool) -> Result<(String, String)> {
        let mut last_err = None;
        for _ in 0..2 {
            // A second attempt re-copies the index in case git rewrote it mid-copy.
            let tmp = TempIndex::copy_of(self)?;
            let opts = tmp.opts();
            let attempt = (|| -> Result<(Option<String>, String)> {
                let it = run_git_with(&self.root, &["write-tree"], &opts, SNAP_TIMEOUT)?;
                let it = it.success().then(|| it.stdout_string().trim().to_owned());
                self.snap_git(&opts, &["add", if include_untracked { "-A" } else { "-u" }])?;
                let wt = self.snap_git(&opts, &["write-tree"])?.stdout_string().trim().to_owned();
                Ok((it, wt))
            })();
            match attempt {
                Ok((Some(it), wt)) => return Ok((it, wt)),
                Ok((None, wt)) => {
                    // Unmerged index: record HEAD's tree as the "index" side.
                    let it = match self.head_oid() {
                        Some(h) => self
                            .snap_git(&Opts::default(), &["rev-parse", &format!("{h}^{{tree}}")])?
                            .stdout_string()
                            .trim()
                            .to_owned(),
                        None => self.empty_tree()?,
                    };
                    return Ok((it, wt));
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Parse("snapshot failed".into())))
    }

    fn worktree_tree_now(&self, include_untracked: bool) -> Result<String> {
        Ok(self.snapshot_trees(include_untracked)?.1)
    }

    fn create_checkpoint_inner(
        &self,
        label: &str,
        include_untracked: bool,
        auto: Option<&str>,
    ) -> Result<Checkpoint> {
        let label = one_line(label);
        let (index_tree, wt_tree) = self.snapshot_trees(include_untracked)?;
        let head = self.head_oid();
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let when = format!("{} +0000", ms / 1000);
        let envs: Vec<(&str, std::ffi::OsString)> = vec![
            ("GIT_AUTHOR_NAME", "Selachi".into()),
            ("GIT_AUTHOR_EMAIL", "selachi@localhost".into()),
            ("GIT_COMMITTER_NAME", "Selachi".into()),
            ("GIT_COMMITTER_EMAIL", "selachi@localhost".into()),
            ("GIT_AUTHOR_DATE", when.clone().into()),
            ("GIT_COMMITTER_DATE", when.into()),
        ];
        let opts = Opts { envs, stdin: None };

        let mut idx_args = vec!["commit-tree", index_tree.as_str()];
        if let Some(h) = &head {
            idx_args.extend(["-p", h.as_str()]);
        }
        idx_args.extend(["-m", "index on selachi checkpoint"]);
        let idx_commit = self.snap_git(&opts, &idx_args)?.stdout_string().trim().to_owned();

        for nonce in 0..16u32 {
            let message = format!(
                "selachi checkpoint: {label}\n\nSelachi-Checkpoint: 1\nSelachi-Label: {label}\n\
Selachi-Worktree: {}\nSelachi-Created-Ms: {ms}\nSelachi-Head: {}\nSelachi-Untracked: {}\n{}Selachi-Nonce: {nonce}\n",
                self.root.display(),
                head.as_deref().unwrap_or("none"),
                if include_untracked { "yes" } else { "no" },
                auto.map(|a| format!("Selachi-Auto: {a}\n")).unwrap_or_default(),
            );
            let mut args = vec!["commit-tree", wt_tree.as_str()];
            if let Some(h) = &head {
                args.extend(["-p", h.as_str()]);
            }
            args.extend(["-p", idx_commit.as_str(), "-m", message.as_str()]);
            let commit = self.snap_git(&opts, &args)?.stdout_string().trim().to_owned();
            let id = format!("{ms}-{}", &commit[..commit.len().min(7)]);
            let refname = format!("{REF_PREFIX}{id}");
            // Empty old value: create only if it does not exist yet.
            let r = run_git_with(
                &self.root,
                &["update-ref", &refname, &commit, ""],
                &Opts::default(),
                SNAP_TIMEOUT,
            )?;
            if r.success() {
                return Ok(Checkpoint {
                    id,
                    label,
                    created: UNIX_EPOCH + Duration::from_millis(ms as u64),
                    commit,
                    includes_untracked: include_untracked,
                    worktree: self.root.clone(),
                    head,
                    auto: auto.is_some(),
                });
            }
        }
        Err(Error::Parse("could not allocate a unique checkpoint id".into()))
    }

    fn all_checkpoints_raw(&self) -> Result<Vec<Checkpoint>> {
        let raw = run_git_ok(
            &self.root,
            &[
                "for-each-ref",
                "--format=%(refname)%1f%(objectname)%1f%(contents)%1e",
                REF_PREFIX,
            ],
        )?;
        Ok(parse_checkpoints(&raw.stdout_string()))
    }

    /// Checkpoint of THIS worktree by id.
    fn own_checkpoint(&self, id: &str) -> Result<Checkpoint> {
        validate_id(id)?;
        let cp = self
            .all_checkpoints_raw()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error::InvalidArgument(format!("unknown checkpoint {id:?}")))?;
        Ok(cp)
    }

    fn require_same_worktree(&self, cp: &Checkpoint) -> Result<()> {
        if same_path(&cp.worktree, &self.root) {
            Ok(())
        } else {
            Err(Error::InvalidArgument(format!(
                "checkpoint {} belongs to another worktree ({})",
                cp.id,
                cp.worktree.display()
            )))
        }
    }

    fn diff_trees(&self, from: &str, to: &str) -> Result<Vec<Entry>> {
        let raw = run_git_ok(
            &self.root,
            &["diff-tree", "-r", "-z", "--no-renames", "--raw", from, to],
        )?;
        parse_raw_diff(&raw.stdout)
    }

    /// Normalise user-supplied restore paths to lowercase-on-Windows, `/`-separated, relative keys.
    fn restore_filter(&self, files: &[PathBuf]) -> Result<Vec<String>> {
        if files.is_empty() {
            return Err(Error::InvalidArgument("no files given to restore".into()));
        }
        let root_key = norm_key(&self.root);
        let mut keys = Vec::new();
        for f in files {
            if has_parent_dir(f) {
                return Err(Error::InvalidArgument(format!("{} contains '..'", f.display())));
            }
            let rel = if f.is_absolute() {
                let k = norm_key(f);
                match k.strip_prefix(&format!("{}/", root_key.trim_end_matches('/'))) {
                    Some(r) => r.to_owned(),
                    None => {
                        return Err(Error::InvalidArgument(format!(
                            "{} is outside the worktree",
                            f.display()
                        )))
                    }
                }
            } else {
                fold_case(&f.to_string_lossy().replace('\\', "/"))
            };
            let rel = rel.trim_start_matches("./").trim_matches('/').to_owned();
            if rel.is_empty() {
                return Err(Error::InvalidArgument("restore path is the worktree root; use RestoreMode::All".into()));
            }
            keys.push(rel);
        }
        Ok(keys)
    }

    fn plan_between(&self, now_tree: &str, cp_tree: &str, filter: Option<&[String]>) -> Result<RestorePlan> {
        let mut plan = RestorePlan::default();
        for e in self.diff_trees(now_tree, cp_tree)? {
            let rel = PathBuf::from(&e.path);
            if let Some(f) = filter {
                let key = fold_case(&e.path);
                if !f.iter().any(|k| key == *k || key.starts_with(&format!("{k}/"))) {
                    continue;
                }
            }
            if e.old_mode == "160000" || e.new_mode == "160000" {
                plan.skipped.push((rel, "submodule / nested repository is not restored".into()));
                continue;
            }
            match e.status {
                'A' => {
                    // In the snapshot, absent from the current (non-ignored) tree. If something is
                    // on disk there it can only be an ignored file: leave it alone.
                    if std::fs::symlink_metadata(self.root.join(&rel)).is_ok() {
                        plan.skipped.push((rel, "an ignored file is in the way; not touched".into()));
                    } else {
                        plan.create.push(rel);
                    }
                }
                'D' => plan.delete.push(rel),
                _ => plan.overwrite.push(rel),
            }
        }
        Ok(plan)
    }

    // ---------------------------------------------------------------- public API

    /// Snapshot index + working tree + untracked (not ignored) files WITHOUT touching the working
    /// tree, the index, HEAD or the stash list. Stored under `refs/selachi/checkpoints/<id>`.
    pub fn create_checkpoint(&self, label: &str) -> Result<Checkpoint> {
        self.create_checkpoint_inner(label, true, None)
    }

    /// Like [`Repo::create_checkpoint`] with a choice: `include_untracked = false` snapshots only
    /// tracked files (restoring such a checkpoint never deletes or recreates untracked files).
    pub fn create_checkpoint_with(&self, label: &str, include_untracked: bool) -> Result<Checkpoint> {
        self.create_checkpoint_inner(label, include_untracked, None)
    }

    /// Checkpoints taken in THIS worktree, newest first.
    pub fn checkpoints(&self) -> Result<Vec<Checkpoint>> {
        Ok(self
            .all_checkpoints_raw()?
            .into_iter()
            .filter(|c| same_path(&c.worktree, &self.root))
            .collect())
    }

    /// Checkpoints of every worktree of the repository, newest first.
    pub fn all_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        self.all_checkpoints_raw()
    }

    /// Unified diff from the checkpoint to the CURRENT working tree (untracked files included for
    /// checkpoints that include them): "what changed since".
    pub fn diff_since_checkpoint(&self, id: &str) -> Result<String> {
        let cp = self.own_checkpoint(id)?;
        let now = self.worktree_tree_now(cp.includes_untracked)?;
        let from = format!("{}^{{tree}}", cp.commit);
        let raw = run_git_ok(
            &self.root,
            &["diff", "--no-color", "--no-ext-diff", "--no-renames", &from, &now],
        )?;
        Ok(raw.stdout_string())
    }

    /// Files that differ between the checkpoint and the current working tree.
    pub fn changed_files_since(&self, id: &str) -> Result<Vec<CheckpointChange>> {
        let cp = self.own_checkpoint(id)?;
        let now = self.worktree_tree_now(cp.includes_untracked)?;
        let from = format!("{}^{{tree}}", cp.commit);
        Ok(self
            .diff_trees(&from, &now)?
            .into_iter()
            .map(|e| CheckpointChange {
                path: PathBuf::from(e.path),
                kind: match e.status {
                    'A' => DeltaKind::Added,
                    'D' => DeltaKind::Deleted,
                    'T' => DeltaKind::TypeChanged,
                    _ => DeltaKind::Modified,
                },
            })
            .collect())
    }

    /// Exactly what [`Repo::restore_checkpoint`] would do right now.
    pub fn restore_plan(&self, id: &str, mode: &RestoreMode) -> Result<RestorePlan> {
        let cp = self.own_checkpoint(id)?;
        self.require_same_worktree(&cp)?;
        let filter = match mode {
            RestoreMode::Files(f) => Some(self.restore_filter(f)?),
            RestoreMode::All => None,
        };
        let now = self.worktree_tree_now(cp.includes_untracked)?;
        self.plan_between(&now, &format!("{}^{{tree}}", cp.commit), filter.as_deref())
    }

    /// Confirmation data for a restore: one warning per file that will be created, overwritten
    /// or deleted, plus a `--stat`.
    pub fn preflight_restore(&self, id: &str, mode: &RestoreMode) -> Result<Preflight> {
        let cp = self.own_checkpoint(id)?;
        let plan = self.restore_plan(id, mode)?;
        let mut pf = Preflight { requires_confirmation: !plan.is_empty(), ..Preflight::default() };
        for p in &plan.create {
            pf.warnings.push(format!("will re-create: {}", p.display()));
        }
        for p in &plan.overwrite {
            pf.warnings.push(format!("will overwrite: {}", p.display()));
        }
        for p in &plan.delete {
            pf.warnings.push(format!("will delete (created after the checkpoint): {}", p.display()));
        }
        for (p, why) in &plan.skipped {
            pf.warnings.push(format!("will skip {}: {why}", p.display()));
        }
        if matches!(mode, RestoreMode::All) && self.head_oid() != cp.head {
            pf.warnings.push("HEAD moved since the checkpoint; the index will be reset to HEAD".into());
        }
        if !plan.is_empty() {
            pf.warnings.push("an automatic \"antes de restaurar\" checkpoint will be created first".into());
            let now = self.worktree_tree_now(cp.includes_untracked)?;
            let to = format!("{}^{{tree}}", cp.commit);
            pf.diff_stat = run_git_ok(&self.root, &["diff", "--stat", "--no-color", "--no-renames", &now, &to])
                .ok()
                .map(|r| r.stdout_string())
                .filter(|s| !s.trim().is_empty());
        }
        Ok(pf)
    }

    /// Restore files from a checkpoint. FIRST an automatic checkpoint labelled
    /// "antes de restaurar: ..." captures the current state, so the restore can be undone with
    /// another restore. Never touches ignored files or HEAD. See [`RestoreMode`].
    pub fn restore_checkpoint(&self, id: &str, mode: RestoreMode) -> Result<RestoreReport> {
        let cp = self.own_checkpoint(id)?;
        self.require_same_worktree(&cp)?;
        let filter = match &mode {
            RestoreMode::Files(f) => Some(self.restore_filter(f)?),
            RestoreMode::All => None,
        };
        let cp_tree = format!("{}^{{tree}}", cp.commit);

        let auto = self.create_checkpoint_inner(
            &format!("antes de restaurar: {}", cp.label),
            cp.includes_untracked,
            Some("restore"),
        )?;
        let plan = self.plan_between(&format!("{}^{{tree}}", auto.commit), &cp_tree, filter.as_deref())?;
        if plan.is_empty() {
            self.remove_checkpoint_ref(&auto.id)?;
            return Ok(RestoreReport {
                auto_checkpoint: None,
                restored: Vec::new(),
                deleted: Vec::new(),
                skipped: plan.skipped,
            });
        }

        // 1. Delete files created after the snapshot (never directories, never ignored files:
        //    the plan only contains paths that were in the non-ignored current tree).
        for rel in &plan.delete {
            let p = self.root.join(rel);
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => continue,
                Ok(_) => remove_file_force(&p)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            prune_empty_parents(&self.root, &p);
        }

        // 2. Write snapshot versions of the missing/changed files with checkout-index from a
        //    temporary index holding the snapshot tree.
        let write: Vec<&PathBuf> = plan.create.iter().chain(plan.overwrite.iter()).collect();
        if !write.is_empty() {
            let tmp = TempIndex::empty(self);
            let opts = tmp.opts();
            self.snap_git(&opts, &["read-tree", &cp_tree])?;
            let mut stdin = Vec::new();
            for p in &write {
                stdin.extend_from_slice(p.to_string_lossy().replace('\\', "/").as_bytes());
                stdin.push(0);
            }
            let opts = Opts { envs: opts.envs, stdin: Some(&stdin) };
            self.snap_git(&opts, &["checkout-index", "-f", "-q", "-z", "--stdin"])?;
        }

        // 3. Whole-tree restore also resets the real index to the snapshot's staged state (or to
        //    HEAD when HEAD moved), so `git status` matches the snapshot. HEAD itself is untouched.
        if matches!(mode, RestoreMode::All) {
            let head_now = self.head_oid();
            let o = Opts::default();
            if head_now == cp.head {
                // Parents are [HEAD, index commit], or just [index commit] on an unborn branch.
                let n = if cp.head.is_some() { 2 } else { 1 };
                self.snap_git(&o, &["read-tree", &format!("{}^{n}^{{tree}}", cp.commit)])?;
            } else if let Some(h) = head_now {
                self.snap_git(&o, &["read-tree", &h])?;
            } else {
                self.snap_git(&o, &["read-tree", "--empty"])?;
            }
            let _ = run_git_with(&self.root, &["update-index", "-q", "--refresh"], &o, SNAP_TIMEOUT);
        }

        let mut restored = plan.create;
        restored.extend(plan.overwrite);
        Ok(RestoreReport {
            auto_checkpoint: Some(auto),
            restored,
            deleted: plan.delete,
            skipped: plan.skipped,
        })
    }

    fn remove_checkpoint_ref(&self, id: &str) -> Result<()> {
        run_git_ok(&self.root, &["update-ref", "-d", &format!("{REF_PREFIX}{id}")])?;
        Ok(())
    }

    /// Delete a checkpoint of this worktree. Its objects stay in the object database until
    /// `git gc` expires them.
    pub fn delete_checkpoint(&self, id: &str) -> Result<()> {
        let cp = self.own_checkpoint(id)?;
        self.require_same_worktree(&cp)?;
        self.remove_checkpoint_ref(&cp.id)
    }

    /// Delete this worktree's checkpoints that are BOTH outside the `keep_last` newest AND older
    /// than `older_than`. Returns the deleted ids.
    pub fn prune_checkpoints(&self, keep_last: usize, older_than: Duration) -> Result<Vec<String>> {
        let now = SystemTime::now();
        let mut deleted = Vec::new();
        for cp in self.checkpoints()?.into_iter().skip(keep_last) {
            let age = now.duration_since(cp.created).unwrap_or_default();
            if age >= older_than {
                self.remove_checkpoint_ref(&cp.id)?;
                deleted.push(cp.id);
            }
        }
        Ok(deleted)
    }
}

fn remove_file_force(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Windows: read-only attribute.
            let mut perm = std::fs::metadata(p)?.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            std::fs::set_permissions(p, perm)?;
            Ok(std::fs::remove_file(p)?)
        }
        Err(e) => Err(e.into()),
    }
}

/// Remove now-empty ancestor directories of `file` up to (excluding) `root`.
fn prune_empty_parents(root: &Path, file: &Path) {
    let mut cur = file.parent();
    while let Some(dir) = cur {
        if dir == root || !dir.starts_with(root) || std::fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refs() {
        let out = "refs/selachi/checkpoints/100-abc1234\u{1f}deadbeef\u{1f}selachi checkpoint: x\n\nSelachi-Checkpoint: 1\nSelachi-Label: my label\nSelachi-Worktree: C:/r\nSelachi-Created-Ms: 100\nSelachi-Head: none\nSelachi-Untracked: yes\n\u{1e}\nrefs/selachi/checkpoints/200-fff\u{1f}cafe\u{1f}selachi checkpoint: y\n\nSelachi-Checkpoint: 1\nSelachi-Label: y\nSelachi-Worktree: C:/r\nSelachi-Created-Ms: 200\nSelachi-Head: abc\nSelachi-Untracked: no\nSelachi-Auto: restore\n\u{1e}\nrefs/selachi/checkpoints/x\u{1f}1\u{1f}not ours\u{1e}\n";
        let v = parse_checkpoints(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "200-fff");
        assert!(v[0].auto && !v[0].includes_untracked);
        assert_eq!(v[0].head.as_deref(), Some("abc"));
        assert_eq!(v[1].label, "my label");
        assert!(v[1].head.is_none() && v[1].includes_untracked);
    }

    #[test]
    fn ids() {
        assert!(validate_id("123-abc").is_ok());
        assert!(validate_id("").is_err());
        assert!(validate_id("../x").is_err());
        assert!(validate_id("a b").is_err());
    }

    #[test]
    fn labels() {
        assert_eq!(one_line("  a\nb\r\n "), "a b");
        assert_eq!(one_line("   "), "checkpoint");
    }
}
