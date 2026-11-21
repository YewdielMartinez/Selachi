//! Worktrees (PLAN A8): list, create, remove, prune, open in an editor.
//!
//! One worktree per agent/thread lets two agents work in the same repository without stepping on
//! each other. Everything goes through the real `git worktree` plumbing; this module adds the
//! validation that git does not do for us (Windows-hostile names, destination checks) and the
//! safety rules around removal.

use std::path::{Path, PathBuf};

use crate::action::Preflight;
use crate::error::{Error, Result};
use crate::exec::{run_git, run_git_ok, LOCAL_TIMEOUT};
use crate::pathutil::{absolutize, has_parent_dir, is_inside, same_path};
use crate::repo::Repo;

/// One entry of `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute path of the worktree directory (as printed by git).
    pub path: PathBuf,
    /// Short branch name (`feature/x`), `None` when detached (or bare).
    pub branch: Option<String>,
    /// HEAD commit id (empty for a bare entry).
    pub head: String,
    /// The main worktree (the first entry; it owns the `.git` directory).
    pub is_main: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    /// The directory is gone / the link is stale; [`Repo::prune_worktrees`] would drop it.
    pub prunable: bool,
    pub prunable_reason: Option<String>,
    pub detached: bool,
    /// Whether the worktree has uncommitted changes. `None` = not computed yet (it needs one
    /// `git status` per worktree, so it is lazy): see [`Repo::worktrees_with_dirty`] and
    /// [`Repo::worktree_is_dirty`].
    pub dirty: Option<bool>,
}

/// Where a new worktree goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorktreeLocation {
    /// `<repo_parent>/<repo_name>.worktrees/<name>` (the repo is the MAIN worktree).
    #[default]
    Sibling,
    /// Exactly this directory (it becomes the worktree root; it must not exist or be empty, and
    /// must not be inside the repository's working tree).
    Custom(PathBuf),
}

/// Options for [`Repo::create_worktree`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewWorktree {
    /// Short name: a single path component (see [`validate_worktree_name`]).
    pub name: String,
    /// Commit-ish to start from. Default: the current HEAD of this repo.
    pub base: Option<String>,
    /// New branch name. Default: `selachi/<name>`. It must not exist yet.
    pub branch: Option<String>,
    pub location: WorktreeLocation,
    /// OPT-IN, never defaulted. Untracked files (ignored ones included, e.g. `.env`) of the
    /// repo this is called on whose path matches one of these git pathspec globs are COPIED into
    /// the new worktree. A pattern without `/` matches at any depth (`.env` -> `**/.env`);
    /// `*` does not cross `/`, `**` does. Existing files are never overwritten, symlinks are
    /// skipped. Secrets get duplicated on disk: only list what you really need.
    pub copy_untracked: Vec<String>,
}

impl NewWorktree {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }
}

/// Result of [`Repo::create_worktree_detailed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub worktree: Worktree,
    /// Files copied because of `copy_untracked` (relative paths).
    pub copied: Vec<PathBuf>,
    /// Files matched by `copy_untracked` that were not copied, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Editors [`Repo::open_in_editor`] knows how to address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Editor {
    VsCode,
    /// A command line, e.g. `cursor` or `"C:\Program Files\Zed\zed.exe" --new`. The worktree
    /// path is appended as the last argument, or substituted for a `{path}` argument.
    Custom(String),
}

type CopyOutcome = (Vec<PathBuf>, Vec<(PathBuf, String)>);

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Windows MAX_PATH-ish limit (leaves room for files inside the worktree).
const MAX_PATH_LEN: usize = 200;

fn check_component(c: &str, what: &str) -> Result<()> {
    let bad = |why: &str| Err(Error::InvalidArgument(format!("{what} {c:?}: {why}")));
    if c.is_empty() {
        return bad("empty");
    }
    if c.starts_with('-') {
        return bad("must not start with '-'");
    }
    if c.starts_with('.') {
        return bad("must not start with '.'");
    }
    if c.contains("..") {
        return bad("must not contain '..'");
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return bad("must not end with '.' or a space (Windows)");
    }
    if let Some(ch) = c.chars().find(|ch| {
        (*ch as u32) < 0x20
            || *ch == '\u{7f}'
            || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return bad(&format!("invalid character {ch:?}"));
    }
    let stem = c.split('.').next().unwrap_or(c).trim_end().to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return bad("reserved device name on Windows");
    }
    Ok(())
}

/// Validate a worktree short name: one path component, no `..`, no leading `-`/`.`, none of
/// Windows' invalid characters (`<>:"/\|?*`, control chars), no trailing dot/space, no reserved
/// device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9, also with an extension).
pub fn validate_worktree_name(name: &str) -> Result<()> {
    if name.chars().count() > 80 {
        return Err(Error::InvalidArgument("worktree name is longer than 80 characters".into()));
    }
    check_component(name, "worktree name")
}

fn validate_branch_name(root: &Path, branch: &str) -> Result<()> {
    if branch.starts_with('-') {
        return Err(Error::InvalidArgument(format!("branch {branch:?} must not start with '-'")));
    }
    for part in branch.split('/') {
        // Loose refs are files: each component must be a legal Windows file name too.
        check_component(part, "branch component")?;
    }
    let full = format!("refs/heads/{branch}");
    let raw = run_git(root, &["check-ref-format", &full], LOCAL_TIMEOUT)?;
    if !raw.success() {
        return Err(Error::InvalidArgument(format!("{branch:?} is not a valid branch name")));
    }
    Ok(())
}

/// Parse `git worktree list --porcelain -z`.
pub(crate) fn parse_worktree_list(out: &[u8]) -> Result<Vec<Worktree>> {
    let mut list: Vec<Worktree> = Vec::new();
    let mut cur: Option<Worktree> = None;
    for tok in out.split(|b| *b == 0) {
        if tok.is_empty() {
            if let Some(w) = cur.take() {
                list.push(w);
            }
            continue;
        }
        let tok = String::from_utf8_lossy(tok);
        let (key, val) = match tok.split_once(' ') {
            Some((k, v)) => (k, Some(v.to_owned())),
            None => (tok.as_ref(), None),
        };
        if key == "worktree" {
            if let Some(w) = cur.take() {
                list.push(w);
            }
            cur = Some(Worktree {
                path: PathBuf::from(val.unwrap_or_default()),
                branch: None,
                head: String::new(),
                is_main: list.is_empty(),
                locked: false,
                lock_reason: None,
                prunable: false,
                prunable_reason: None,
                detached: false,
                dirty: None,
            });
            continue;
        }
        let Some(w) = cur.as_mut() else {
            return Err(Error::Parse(format!("worktree attribute {key:?} before any `worktree`")));
        };
        match key {
            "HEAD" => w.head = val.unwrap_or_default(),
            "branch" => {
                let b = val.unwrap_or_default();
                w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(&b).to_owned());
            }
            "detached" => w.detached = true,
            "locked" => {
                w.locked = true;
                w.lock_reason = val.filter(|v| !v.is_empty());
            }
            "prunable" => {
                w.prunable = true;
                w.prunable_reason = val.filter(|v| !v.is_empty());
            }
            _ => {} // bare, future attributes
        }
    }
    if let Some(w) = cur.take() {
        list.push(w);
    }
    Ok(list)
}

/// Split a command line (double quotes group; no escapes), for [`Editor::Custom`].
fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for ch in cmd.chars() {
        match ch {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Repo {
    /// All worktrees of this repository (main first). `dirty` is `None`; see
    /// [`Repo::worktrees_with_dirty`].
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let raw = run_git_ok(&self.root, &["worktree", "list", "--porcelain", "-z"])?;
        parse_worktree_list(&raw.stdout)
    }

    /// [`Repo::worktrees`] with `dirty` filled in (one `git status` per existing worktree).
    pub fn worktrees_with_dirty(&self) -> Result<Vec<Worktree>> {
        let mut list = self.worktrees()?;
        for w in &mut list {
            if !w.prunable && w.path.is_dir() {
                w.dirty = self.worktree_is_dirty(&w.path).ok();
            }
        }
        Ok(list)
    }

    /// True when the worktree at `path` has staged, unstaged or untracked (not ignored) changes.
    pub fn worktree_is_dirty(&self, path: &Path) -> Result<bool> {
        let raw = run_git_ok(path, &["status", "--porcelain", "--untracked-files=normal"])?;
        Ok(!raw.stdout.iter().all(u8::is_ascii_whitespace))
    }

    /// Root of the main worktree (where `.git` lives).
    pub fn main_worktree(&self) -> Result<PathBuf> {
        let list = self.worktrees()?;
        list.into_iter()
            .next()
            .map(|w| w.path)
            .ok_or_else(|| Error::Parse("git worktree list returned nothing".into()))
    }

    fn find_worktree(&self, path: &Path) -> Result<Worktree> {
        let path = absolutize(path, &self.root);
        self.worktrees()?
            .into_iter()
            .find(|w| same_path(&w.path, &path))
            .ok_or_else(|| {
                Error::InvalidArgument(format!("{} is not a worktree of this repository", path.display()))
            })
    }

    /// Create a branch + worktree. See [`NewWorktree`]. Never touches the current worktree.
    pub fn create_worktree(&self, opts: &NewWorktree) -> Result<Worktree> {
        self.create_worktree_detailed(opts).map(|c| c.worktree)
    }

    /// [`Repo::create_worktree`] that also reports what `copy_untracked` copied.
    pub fn create_worktree_detailed(&self, opts: &NewWorktree) -> Result<CreatedWorktree> {
        validate_worktree_name(&opts.name)?;
        let branch = match &opts.branch {
            Some(b) => b.clone(),
            None => format!("selachi/{}", opts.name),
        };
        validate_branch_name(&self.root, &branch)?;
        // Validate patterns before creating anything.
        let specs = copy_pathspecs(&opts.copy_untracked)?;

        let base = opts.base.clone().unwrap_or_else(|| "HEAD".to_owned());
        if base.trim().is_empty() || base.starts_with('-') {
            return Err(Error::InvalidArgument(format!("invalid base {base:?}")));
        }
        let commit_ref = format!("{base}^{{commit}}");
        let rv = run_git(&self.root, &["rev-parse", "--verify", "-q", &commit_ref], LOCAL_TIMEOUT)?;
        if !rv.success() {
            return Err(Error::InvalidArgument(format!(
                "base {base:?} does not resolve to a commit (empty repository?)"
            )));
        }
        let exists = run_git(
            &self.root,
            &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")],
            LOCAL_TIMEOUT,
        )?;
        if exists.success() {
            return Err(Error::InvalidArgument(format!("branch {branch:?} already exists")));
        }

        let main = self.main_worktree()?;
        let target = match &opts.location {
            WorktreeLocation::Sibling => {
                let name = main
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .ok_or_else(|| Error::InvalidArgument("repository root has no name".into()))?;
                let parent = main
                    .parent()
                    .ok_or_else(|| Error::InvalidArgument("repository root has no parent".into()))?;
                parent.join(format!("{name}.worktrees")).join(&opts.name)
            }
            WorktreeLocation::Custom(p) => {
                if has_parent_dir(p) {
                    return Err(Error::InvalidArgument("custom location must not contain '..'".into()));
                }
                absolutize(p, &main)
            }
        };
        if cfg!(windows) && target.to_string_lossy().chars().count() > MAX_PATH_LEN {
            return Err(Error::InvalidArgument(format!(
                "destination path is too long for Windows ({} chars): {}",
                target.to_string_lossy().chars().count(),
                target.display()
            )));
        }
        for w in self.worktrees()? {
            if same_path(&w.path, &target) {
                return Err(Error::InvalidArgument(format!(
                    "{} is already a worktree",
                    target.display()
                )));
            }
        }
        if is_inside(&target, &main) {
            return Err(Error::InvalidArgument(format!(
                "{} is inside the repository's working tree",
                target.display()
            )));
        }
        if target.exists() {
            let empty = target.is_dir()
                && std::fs::read_dir(&target).map(|mut d| d.next().is_none()).unwrap_or(false);
            if !empty {
                return Err(Error::InvalidArgument(format!(
                    "{} already exists and is not an empty directory",
                    target.display()
                )));
            }
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let target_s = target.to_string_lossy().into_owned();
        run_git_ok(&self.root, &["worktree", "add", "-b", &branch, "--", &target_s, &base])?;

        let worktree = self
            .worktrees()?
            .into_iter()
            .find(|w| same_path(&w.path, &target))
            .ok_or_else(|| Error::Parse("created worktree is missing from `git worktree list`".into()))?;

        let (copied, skipped) = if specs.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            self.copy_untracked_into(&specs, &worktree.path)?
        };
        Ok(CreatedWorktree { worktree, copied, skipped })
    }

    fn copy_untracked_into(
        &self,
        specs: &[String],
        dest: &Path,
    ) -> Result<CopyOutcome> {
        let mut args: Vec<String> = ["ls-files", "-o", "-z", "--full-name", "--"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        args.extend(specs.iter().cloned());
        let raw = run_git_ok(&self.root, &args)?;
        let mut copied = Vec::new();
        let mut skipped = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for rel in raw.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let rel = String::from_utf8_lossy(rel).into_owned();
            if rel.ends_with('/') || !seen.insert(rel.clone()) {
                continue; // nested repository marker / duplicate
            }
            let relp = PathBuf::from(&rel);
            if has_parent_dir(&relp) || relp.is_absolute() || rel == ".git" || rel.starts_with(".git/") {
                skipped.push((relp, "unsafe path".into()));
                continue;
            }
            let src = self.root.join(&relp);
            let meta = match std::fs::symlink_metadata(&src) {
                Ok(m) => m,
                Err(e) => {
                    skipped.push((relp, e.to_string()));
                    continue;
                }
            };
            if !meta.is_file() {
                skipped.push((relp, "not a regular file".into()));
                continue;
            }
            let dst = dest.join(&relp);
            if dst.exists() {
                skipped.push((relp, "already exists in the worktree".into()));
                continue;
            }
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            match std::fs::copy(&src, &dst) {
                Ok(_) => copied.push(relp),
                Err(e) => skipped.push((relp, e.to_string())),
            }
        }
        Ok((copied, skipped))
    }

    /// Remove a worktree directory (the branch is kept). Refuses: the main worktree, the
    /// worktree this `Repo` is rooted in, locked worktrees and worktrees with uncommitted changes
    /// (staged, unstaged or untracked-not-ignored). `force` overrides the last two (a locked
    /// worktree is removed with git's double `--force`). Check [`Repo::preflight_remove_worktree`]
    /// first to show the user what would be lost.
    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let w = self.find_worktree(path)?;
        if w.is_main {
            return Err(Error::InvalidArgument("the main worktree cannot be removed".into()));
        }
        if same_path(&w.path, &self.root) {
            return Err(Error::InvalidArgument(
                "cannot remove the worktree this repository handle is rooted in".into(),
            ));
        }
        if w.locked && !force {
            return Err(Error::InvalidArgument(format!(
                "worktree is locked{}; use force to remove it",
                w.lock_reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default()
            )));
        }
        let main = self.main_worktree()?;
        let target = w.path.to_string_lossy().into_owned();
        if w.prunable || !w.path.exists() {
            // Directory already gone: only the administrative files remain.
            run_git_ok(&main, &["worktree", "prune"])?;
            return Ok(());
        }
        if !force && self.worktree_is_dirty(&w.path)? {
            return Err(Error::InvalidArgument(
                "worktree has uncommitted changes; use force to discard them".into(),
            ));
        }
        let mut args: Vec<&str> = vec!["worktree", "remove"];
        if force {
            args.push("--force");
            if w.locked {
                args.push("--force");
            }
        }
        args.push("--");
        args.push(&target);
        run_git_ok(&main, &args)?;
        Ok(())
    }

    /// Warnings to show before [`Repo::remove_worktree`]: uncommitted changes, commits that exist
    /// nowhere else (not on a remote or another branch), lock state. Errors for the main worktree.
    pub fn preflight_remove_worktree(&self, path: &Path) -> Result<Preflight> {
        let w = self.find_worktree(path)?;
        if w.is_main {
            return Err(Error::InvalidArgument("the main worktree cannot be removed".into()));
        }
        let mut pf = Preflight::default();
        if w.locked {
            pf.warnings.push(format!(
                "worktree is locked{}",
                w.lock_reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default()
            ));
        }
        if w.prunable || !w.path.is_dir() {
            pf.warnings.push("directory is already missing; only stale metadata will be pruned".into());
            pf.requires_confirmation = true;
            return Ok(pf);
        }
        if let Ok(out) = run_git_ok(&w.path, &["status", "--porcelain", "--untracked-files=normal"]) {
            let n = out.stdout_string().lines().filter(|l| !l.trim().is_empty()).count();
            if n > 0 {
                pf.warnings.push(format!("{n} file(s) with uncommitted changes will be lost"));
                pf.diff_stat = run_git(&w.path, &["diff", "--stat", "--no-color", "HEAD"], LOCAL_TIMEOUT)
                    .ok()
                    .filter(|r| r.success())
                    .map(|r| r.stdout_string())
                    .filter(|s| !s.trim().is_empty());
            }
        }
        if let Some(n) = self.unshared_commits(&w).filter(|n| *n > 0) {
            let note = if w.branch.is_some() {
                "the branch itself is kept"
            } else {
                "detached HEAD: they become unreachable"
            };
            let what = w.branch.as_deref().map_or("HEAD".to_owned(), |b| format!("branch {b}"));
            pf.warnings.push(format!(
                "{n} commit(s) on {what} are not pushed or merged anywhere else ({note})"
            ));
        }
        pf.requires_confirmation = !pf.warnings.is_empty();
        Ok(pf)
    }

    /// Commits reachable from the worktree's HEAD but from no remote ref and no other local branch.
    fn unshared_commits(&self, w: &Worktree) -> Option<usize> {
        let mut args: Vec<String> = vec!["rev-list".into(), "--count".into()];
        match &w.branch {
            Some(b) => {
                args.push(format!("refs/heads/{b}"));
                args.push("--not".into());
                args.push(format!("--exclude={b}"));
            }
            None => {
                args.push(w.head.clone());
                args.push("--not".into());
            }
        }
        args.push("--branches".into());
        args.push("--remotes".into());
        let raw = run_git(&self.root, &args, LOCAL_TIMEOUT).ok()?;
        if !raw.success() {
            return None;
        }
        raw.stdout_string().trim().parse().ok()
    }

    /// `git worktree prune`: drop administrative files of worktrees whose directory is gone.
    /// Returns git's report lines.
    pub fn prune_worktrees(&self) -> Result<Vec<String>> {
        let raw = run_git_ok(&self.root, &["worktree", "prune", "--verbose"])?;
        let mut lines: Vec<String> = raw
            .stdout_string()
            .lines()
            .chain(raw.stderr_string().lines())
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        lines.dedup();
        Ok(lines)
    }

    /// The argv that opens the worktree at `path` in `editor`. It does NOT spawn anything: the
    /// caller runs it (detached, no console window). `path` must be a worktree of this repo.
    /// On Windows VS Code's launcher is `code.cmd`.
    pub fn open_in_editor(&self, path: &Path, editor: &Editor) -> Result<Vec<String>> {
        let w = self.find_worktree(path)?;
        let p = w.path.to_string_lossy().into_owned();
        match editor {
            Editor::VsCode => {
                let exe = if cfg!(windows) { "code.cmd" } else { "code" };
                Ok(vec![exe.to_owned(), "-n".to_owned(), p])
            }
            Editor::Custom(cmd) => {
                let mut argv = split_command(cmd);
                if argv.is_empty() {
                    return Err(Error::InvalidArgument("empty editor command".into()));
                }
                if argv.iter().any(|a| a == "{path}") {
                    for a in &mut argv {
                        if a == "{path}" {
                            a.clone_from(&p);
                        }
                    }
                } else {
                    argv.push(p);
                }
                Ok(argv)
            }
        }
    }
}

/// Turn user globs into git `:(glob)` pathspecs; rejects magic, absolute paths and `..`.
fn copy_pathspecs(globs: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for g in globs {
        let g = g.trim().replace('\\', "/");
        if g.is_empty() {
            return Err(Error::InvalidArgument("empty copy_untracked pattern".into()));
        }
        if g.starts_with('/') || g.contains(':') || g.starts_with('!') {
            return Err(Error::InvalidArgument(format!(
                "copy_untracked pattern {g:?} must be relative and plain (no ':' / leading '/')"
            )));
        }
        if g.split('/').any(|c| c == "..") {
            return Err(Error::InvalidArgument(format!("copy_untracked pattern {g:?} contains '..'")));
        }
        let g = g.trim_start_matches("./");
        let anywhere = if g.contains('/') { g.to_owned() } else { format!("**/{g}") };
        out.push(format!(":(glob){anywhere}"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["feat-x", "fix_1", "añadir", "my feature", "a.b", "v1.2", "console"] {
            assert!(validate_worktree_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "-x", "..", "a..b", ".hidden", "a/b", "a\\b", "c:x", "a*b", "a?b", "a<b", "a|b",
            "a\"b", "end.", "end ", "CON", "con", "Nul.txt", "COM1", "lpt9", "aux.log", "a\tb",
        ] {
            assert!(validate_worktree_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_worktree_name(&"x".repeat(81)).is_err());
    }

    #[test]
    fn parse_list() {
        let out = b"worktree C:/repo\0HEAD abc\0branch refs/heads/main\0\0\
worktree C:/repo.worktrees/a b\0HEAD def\0branch refs/heads/selachi/a\0locked because\0\0\
worktree C:/gone\0HEAD 123\0detached\0prunable gitdir file points to non-existent location\0\0";
        let l = parse_worktree_list(out).unwrap();
        assert_eq!(l.len(), 3);
        assert!(l[0].is_main && !l[1].is_main);
        assert_eq!(l[0].branch.as_deref(), Some("main"));
        assert_eq!(l[1].path, PathBuf::from("C:/repo.worktrees/a b"));
        assert!(l[1].locked);
        assert_eq!(l[1].lock_reason.as_deref(), Some("because"));
        assert!(l[2].detached && l[2].prunable && l[2].branch.is_none());
        assert!(l.iter().all(|w| w.dirty.is_none()));
    }

    #[test]
    fn command_split() {
        assert_eq!(
            split_command(r#""C:\Program Files\Z\z.exe" --new  x"#),
            ["C:\\Program Files\\Z\\z.exe", "--new", "x"]
        );
        assert_eq!(split_command("  "), Vec::<String>::new());
    }

    #[test]
    fn pathspecs() {
        assert_eq!(copy_pathspecs(&[".env".into()]).unwrap(), [":(glob)**/.env"]);
        assert_eq!(copy_pathspecs(&["config/*.local".into()]).unwrap(), [":(glob)config/*.local"]);
        assert!(copy_pathspecs(&["../x".into()]).is_err());
        assert!(copy_pathspecs(&["/abs".into()]).is_err());
        assert!(copy_pathspecs(&[":(top)x".into()]).is_err());
        assert!(copy_pathspecs(&["C:/x".into()]).is_err());
    }
}
