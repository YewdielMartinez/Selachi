//! Line diff with inline (word level) highlighting inside changed lines, plus a
//! helper that diffs a working file against a git revision.

use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use similar::{ChangeTag, TextDiff};

use crate::error::{Error, Result};

/// Lines of unchanged context kept around each change.
pub const CONTEXT_LINES: usize = 3;

/// Role of a line inside a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// 1-based line number in the old text (`None` for added lines).
    pub old_line: Option<usize>,
    /// 1-based line number in the new text (`None` for removed lines).
    pub new_line: Option<usize>,
    /// Line text without its line ending.
    pub text: String,
    /// Byte ranges within `text` that changed inside a modified line (word diff).
    /// Empty for pure additions/removals and context lines.
    pub inline: Vec<Range<usize>>,
}

/// A group of nearby changes with its context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based first line in the old text covered by the hunk.
    pub old_start: usize,
    pub old_len: usize,
    /// 1-based first line in the new text covered by the hunk.
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// Unified-diff style header, `@@ -1,3 +1,4 @@`.
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_len, self.new_start, self.new_len
        )
    }

    pub fn added(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.kind == LineKind::Added)
            .count()
    }

    pub fn removed(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.kind == LineKind::Removed)
            .count()
    }
}

/// Diff `old` against `new`. CRLF and LF line endings compare equal. Empty when
/// the texts are identical. The word diff is time-boxed (2 s) so pathological
/// inputs degrade to a coarser diff instead of hanging.
pub fn diff_text(old: &str, new: &str) -> Vec<Hunk> {
    let old = normalize_eol(old);
    let new = normalize_eol(new);
    let diff = TextDiff::configure()
        .timeout(Duration::from_secs(2))
        .diff_lines(old.as_ref(), new.as_ref());

    let mut hunks = Vec::new();
    for group in diff.grouped_ops(CONTEXT_LINES) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_inline_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Insert => LineKind::Added,
                    ChangeTag::Delete => LineKind::Removed,
                };
                let mut text = String::new();
                let mut inline = Vec::new();
                for (emphasized, value) in change.iter_strings_lossy() {
                    let start = text.len();
                    text.push_str(&value);
                    if emphasized && kind != LineKind::Context {
                        inline.push(start..text.len());
                    }
                }
                // Drop the line terminator and clip ranges accordingly.
                while text.ends_with('\n') || text.ends_with('\r') {
                    text.pop();
                }
                let max = text.len();
                inline.retain_mut(|r| {
                    r.end = r.end.min(max);
                    r.start < r.end
                });
                // A line that is entirely "changed" carries no extra information.
                if inline.len() == 1 && inline[0] == (0..max) {
                    inline.clear();
                }
                lines.push(DiffLine {
                    kind,
                    old_line: change.old_index().map(|i| i + 1),
                    new_line: change.new_index().map(|i| i + 1),
                    text,
                    inline,
                });
            }
        }
        hunks.push(Hunk {
            old_start: old_range.start + 1,
            old_len: old_range.len(),
            new_start: new_range.start + 1,
            new_len: new_range.len(),
            lines,
        });
    }
    hunks
}

fn normalize_eol(s: &str) -> Cow<'_, str> {
    if s.contains('\r') {
        Cow::Owned(s.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(s)
    }
}

/// Which revision to compare the working file against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitBase {
    Head,
    Commit(String),
}

impl GitBase {
    fn spec(&self) -> Result<String> {
        match self {
            GitBase::Head => Ok("HEAD".to_string()),
            GitBase::Commit(c) => {
                let c = c.trim();
                if c.is_empty()
                    || c.starts_with('-')
                    || c.contains(':')
                    || c.chars().any(|ch| ch.is_whitespace() || ch.is_control())
                {
                    Err(Error::InvalidInput(format!("bad git revision {c:?}")))
                } else {
                    Ok(c.to_string())
                }
            }
        }
    }
}

/// Diff the working-tree `path` against its content at `base`.
///
/// Runs `git show <base>:<relpath>` (no console window on Windows). A file missing at
/// `base` diffs as entirely added, a file missing on disk as entirely removed.
pub fn diff_against_git(repo_root: &Path, path: &Path, base: GitBase) -> Result<Vec<Hunk>> {
    let spec = base.spec()?;
    let rel = relative_to(repo_root, path)?;

    let old = git_show(repo_root, &spec, &rel)?;
    let new = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::io(path, e)),
    };
    if old.is_none() && new.is_none() {
        return Err(Error::InvalidInput(format!(
            "{} exists neither on disk nor in {spec}",
            rel
        )));
    }
    let old = old.unwrap_or_default();
    let new = new.unwrap_or_default();
    Ok(diff_text(
        &String::from_utf8_lossy(&old),
        &String::from_utf8_lossy(&new),
    ))
}

/// `path` relative to `repo_root`, with forward slashes (git's notation).
pub(crate) fn relative_to(repo_root: &Path, path: &Path) -> Result<String> {
    let rel = if path.is_relative() {
        path.to_path_buf()
    } else if let Ok(r) = path.strip_prefix(repo_root) {
        r.to_path_buf()
    } else {
        // Different spellings of the same location (`\\?\` prefix, 8.3 names, symlinks).
        let root = std::fs::canonicalize(repo_root).map_err(|e| Error::io(repo_root, e))?;
        let file = std::fs::canonicalize(path).map_err(|e| Error::io(path, e))?;
        file.strip_prefix(&root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                Error::InvalidInput(format!(
                    "{} is outside the repository {}",
                    path.display(),
                    repo_root.display()
                ))
            })?
    };
    let s = rel.to_string_lossy().replace('\\', "/");
    Ok(s.trim_start_matches("./").to_string())
}

/// Blob content at `rev:rel`, or `None` when the path does not exist at that revision.
fn git_show(repo_root: &Path, rev: &str, rel: &str) -> Result<Option<Vec<u8>>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .arg("show")
        .arg(format!("{rev}:{rel}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    no_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| Error::Git(format!("cannot run git: {e}")))?;
    if out.status.success() {
        return Ok(Some(out.stdout));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // "path 'x' does not exist in 'HEAD'" / "exists on disk, but not in 'HEAD'" /
    // "fatal: invalid object name" for a repo without commits.
    if stderr.contains("does not exist in")
        || stderr.contains("exists on disk, but not in")
        || (rev == "HEAD" && stderr.contains("unknown revision"))
        || (rev == "HEAD" && stderr.contains("invalid object name"))
        || (rev == "HEAD" && stderr.contains("bad revision"))
    {
        return Ok(None);
    }
    Err(Error::Git(stderr.trim().to_string()))
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}
