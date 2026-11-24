//! GitHub CLI (`gh`) integration (backlog B1): the pull request of a branch, its CI checks, the
//! log of a failed check and the creation of a PR.
//!
//! * Everything is **on demand**: nothing here polls or keeps a process alive. The caller
//!   decides when to ask (see `ui_git::GitModel`: branch change, after a push, explicit refresh,
//!   window focus when the last query is older than five minutes).
//! * Every call runs `gh` without a console window, with prompts disabled and a timeout.
//!   Offline, unauthenticated, `gh` missing, no remote: the read functions answer `None`.
//! * Tokens are never read: availability is the exit code of `gh auth status`; `gh` itself keeps
//!   the credentials.
//! * Creating a PR is the only write. It is a separate, explicit call ([`GhClient::create_pr`])
//!   and the UI shows its exact argv ([`NewPr::argv`]) on a confirmation card first.
//!
//! The JSON schemas are those of `gh pr list|view --json` and `gh pr checks --json`
//! (`bucket, completedAt, link, name, startedAt, state, workflow`); parsing is pure
//! ([`parse_pr_list`], [`parse_checks`]) and tested against recorded fixtures.

use std::ffi::OsString;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::error::Error;
use crate::exec::run_command;
use crate::repo::Repo;

/// Timeout of `gh auth status`.
const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout of the PR and checks queries.
const QUERY_TIMEOUT: Duration = Duration::from_secs(25);
/// Timeout of a failed-log download.
const LOG_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout of `gh pr create` (it may push).
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest failed-check log returned (the tail is kept: failures are at the end).
pub const MAX_LOG_BYTES: usize = 200 * 1024;

const PR_FIELDS: &str = "number,title,url,state,isDraft,reviewDecision,mergeable,headRefName";
const CHECK_FIELDS: &str = "name,state,bucket,link,startedAt,completedAt,workflow";

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// The pull request of a branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub draft: bool,
    /// `None` when the repository does not require reviews or none happened yet.
    pub review_decision: Option<ReviewDecision>,
    pub mergeable: Mergeable,
    pub head_branch: String,
}

/// `gh`'s own classification of a check (`bucket`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckBucket {
    Pass,
    Fail,
    Pending,
    Skipping,
    Cancel,
}

/// Where a check is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    Queued,
    InProgress,
    Completed,
}

/// How a completed check ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckConclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    Neutral,
    TimedOut,
    ActionRequired,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// The workflow it belongs to (empty for third-party statuses).
    pub workflow: String,
    pub bucket: CheckBucket,
    pub status: CheckStatus,
    /// `None` while not completed.
    pub conclusion: Option<CheckConclusion>,
    /// Link to the run / job on GitHub (empty when none).
    pub url: String,
    /// Unix seconds.
    pub started: Option<i64>,
    /// Unix seconds.
    pub completed: Option<i64>,
}

impl Check {
    /// Seconds the check ran, when both ends are known and ordered.
    pub fn duration_secs(&self) -> Option<u64> {
        match (self.started, self.completed) {
            (Some(s), Some(c)) if c >= s => Some((c - s) as u64),
            _ => None,
        }
    }

    pub fn is_failing(&self) -> bool {
        self.bucket == CheckBucket::Fail
    }
}

/// Totals of a list of checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckTotals {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub pending: usize,
    pub skipped: usize,
}

impl CheckTotals {
    pub fn of(checks: &[Check]) -> Self {
        let mut t = CheckTotals {
            total: checks.len(),
            ..Default::default()
        };
        for c in checks {
            match c.bucket {
                CheckBucket::Pass => t.pass += 1,
                CheckBucket::Fail => t.fail += 1,
                CheckBucket::Pending => t.pending += 1,
                CheckBucket::Skipping | CheckBucket::Cancel => t.skipped += 1,
            }
        }
        t
    }
}

/// A pull request to create.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewPr {
    pub title: String,
    pub body: String,
    pub draft: bool,
    /// `--base`; `None` = the repository default.
    pub base: Option<String>,
    /// `--head`; `None` = the current branch.
    pub head: Option<String>,
}

impl NewPr {
    /// Rejects what can never work, before any process runs.
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("el título del PR está vacío".into());
        }
        if self.title.contains('\n') {
            return Err("el título del PR debe ser una sola línea".into());
        }
        for (name, v) in [("base", &self.base), ("head", &self.head)] {
            let bad = v.as_ref().is_some_and(|v| {
                v.trim().is_empty() || v.starts_with('-') || v.contains(char::is_whitespace)
            });
            if bad {
                return Err(format!("rama {name} no válida"));
            }
        }
        Ok(())
    }

    /// The exact command (`argv[0]` is `gh`). The body goes through stdin (`--body-file -`):
    /// no quoting problems and nothing sensitive on a process listing.
    pub fn argv(&self) -> Vec<String> {
        let mut v: Vec<String> = ["gh", "pr", "create", "--title"].map(str::to_owned).into();
        v.push(self.title.trim().to_owned());
        v.extend(["--body-file".to_owned(), "-".to_owned()]);
        if self.draft {
            v.push("--draft".into());
        }
        if let Some(b) = &self.base {
            v.extend(["--base".to_owned(), b.clone()]);
        }
        if let Some(h) = &self.head {
            v.extend(["--head".to_owned(), h.clone()]);
        }
        v
    }
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

/// Runs `gh`. The default resolves `gh` from `PATH`; tests point it at a fake shim.
#[derive(Clone, Debug)]
pub struct GhClient {
    program: OsString,
}

impl Default for GhClient {
    fn default() -> Self {
        Self::system()
    }
}

struct GhOut {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl GhClient {
    /// `gh` from `PATH`.
    pub fn system() -> Self {
        Self {
            program: "gh".into(),
        }
    }

    /// An explicit executable (a `.cmd` shim in tests).
    pub fn with_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn run(
        &self,
        cwd: &std::path::Path,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Option<GhOut> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args)
            .current_dir(cwd)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("NO_COLOR", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        let raw = run_command(
            cmd,
            format!("gh {}", args.join(" ")),
            stdin,
            timeout,
            Error::Io,
        )
        .ok()?;
        Some(GhOut {
            ok: raw.success(),
            stdout: raw.stdout_string(),
            stderr: raw.stderr_string(),
        })
    }

    /// `gh` is installed and logged in (exit code of `gh auth status`; no token is read).
    pub fn available(&self) -> bool {
        let cwd = std::env::temp_dir();
        self.run(&cwd, &args(&["auth", "status"]), None, AUTH_TIMEOUT)
            .is_some_and(|o| o.ok)
    }

    /// The pull request whose head is `branch`: the open one when there is one, else the latest
    /// closed / merged one. `None` when there is none, `gh` is unavailable or the query failed.
    pub fn pr_for_branch(&self, repo: &Repo, branch: &str) -> Option<PrInfo> {
        if branch.trim().is_empty() || branch.starts_with('-') {
            return None;
        }
        let out = self.run(
            &repo.root,
            &args(&[
                "pr", "list", "--head", branch, "--state", "all", "--limit", "10", "--json",
                PR_FIELDS,
            ]),
            None,
            QUERY_TIMEOUT,
        )?;
        if !out.ok {
            return None;
        }
        pick_pr(parse_pr_list(&out.stdout), branch)
    }

    /// The checks of pull request `pr`. `None` when the query failed; an empty list when the PR
    /// has no checks.
    pub fn checks(&self, repo: &Repo, pr: u64) -> Option<Vec<Check>> {
        let out = self.run(
            &repo.root,
            &args(&["pr", "checks", &pr.to_string(), "--json", CHECK_FIELDS]),
            None,
            QUERY_TIMEOUT,
        )?;
        // `gh pr checks` exits 1 when a check failed and 8 while pending, still printing JSON.
        match parse_checks(&out.stdout) {
            Some(list) => Some(list),
            None if out.stderr.contains("no checks reported") => Some(Vec::new()),
            None => None,
        }
    }

    /// The log of the failed steps of `check`: the last [`MAX_LOG_BYTES`], ANSI stripped and
    /// **redacted**. Empty when the check has no run link, nothing failed or `gh` failed.
    pub fn failed_check_log(&self, repo: &Repo, check: &Check) -> String {
        let Some((run, job)) = parse_check_url(&check.url) else {
            return String::new();
        };
        let mut a = args(&["run", "view"]);
        if let Some(run) = run {
            a.push(run.to_string());
        }
        if let Some(job) = job {
            a.extend(["--job".to_owned(), job.to_string()]);
        }
        a.push("--log-failed".into());
        match self.run(&repo.root, &a, None, LOG_TIMEOUT) {
            Some(o) if o.ok => clean_log(&o.stdout),
            _ => String::new(),
        }
    }

    /// Creates the PR. **Only call this after an explicit confirmation** showing
    /// [`NewPr::argv`]. Returns the URL `gh` printed, or its error text.
    pub fn create_pr(&self, repo: &Repo, pr: &NewPr) -> Result<String, String> {
        pr.validate()?;
        let argv = pr.argv();
        let out = self
            .run(
                &repo.root,
                &argv[1..],
                Some(pr.body.as_bytes()),
                CREATE_TIMEOUT,
            )
            .ok_or_else(|| "no se pudo ejecutar gh".to_string())?;
        if !out.ok {
            let msg = if out.stderr.is_empty() {
                out.stdout.trim().to_owned()
            } else {
                out.stderr
            };
            return Err(if msg.is_empty() {
                "gh pr create falló".into()
            } else {
                msg
            });
        }
        Ok(out
            .stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_owned())
    }
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// `gh` is installed and authenticated (system `gh`).
pub fn gh_available() -> bool {
    GhClient::system().available()
}

/// See [`GhClient::pr_for_branch`] (system `gh`).
pub fn pr_for_branch(repo: &Repo, branch: &str) -> Option<PrInfo> {
    GhClient::system().pr_for_branch(repo, branch)
}

/// See [`GhClient::checks`] (system `gh`).
pub fn checks(repo: &Repo, pr: u64) -> Option<Vec<Check>> {
    GhClient::system().checks(repo, pr)
}

/// See [`GhClient::failed_check_log`] (system `gh`).
pub fn failed_check_log(repo: &Repo, check: &Check) -> String {
    GhClient::system().failed_check_log(repo, check)
}

// ---------------------------------------------------------------------------------------------
// Pure parsing
// ---------------------------------------------------------------------------------------------

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Parses the array of `gh pr list --json ...` (or one `gh pr view --json ...` object).
/// Entries without a number are skipped; garbage yields an empty list.
pub fn parse_pr_list(json: &str) -> Vec<PrInfo> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let items: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![&v],
        _ => Vec::new(),
    };
    items.into_iter().filter_map(parse_pr).collect()
}

fn parse_pr(v: &Value) -> Option<PrInfo> {
    let number = v.get("number")?.as_u64()?;
    let state = match s(v, "state").to_ascii_uppercase().as_str() {
        "OPEN" => PrState::Open,
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => return None,
    };
    let review_decision = match s(v, "reviewDecision").to_ascii_uppercase().as_str() {
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => None,
    };
    let mergeable = match s(v, "mergeable").to_ascii_uppercase().as_str() {
        "MERGEABLE" => Mergeable::Mergeable,
        "CONFLICTING" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    Some(PrInfo {
        number,
        title: s(v, "title"),
        url: s(v, "url"),
        state,
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        review_decision,
        mergeable,
        head_branch: s(v, "headRefName"),
    })
}

/// The PR to show for `branch`: the first open one whose head is the branch, else the first
/// (they come newest first) whose head is the branch.
pub fn pick_pr(list: Vec<PrInfo>, branch: &str) -> Option<PrInfo> {
    let mine = |p: &PrInfo| p.head_branch.is_empty() || p.head_branch == branch;
    let mut candidates: Vec<PrInfo> = list.into_iter().filter(mine).collect();
    let open = candidates.iter().position(|p| p.state == PrState::Open);
    match open {
        Some(i) => Some(candidates.swap_remove(i)),
        None => candidates.into_iter().next(),
    }
}

/// Parses `gh pr checks --json ...`. `None` when the text is not a JSON array.
pub fn parse_checks(json: &str) -> Option<Vec<Check>> {
    let v: Value = serde_json::from_str(json).ok()?;
    let arr = v.as_array()?;
    Some(arr.iter().filter_map(parse_check).collect())
}

fn parse_check(v: &Value) -> Option<Check> {
    let name = s(v, "name");
    if name.is_empty() {
        return None;
    }
    let state = s(v, "state").to_ascii_uppercase();
    let bucket = match s(v, "bucket").to_ascii_lowercase().as_str() {
        "pass" => CheckBucket::Pass,
        "fail" => CheckBucket::Fail,
        "skipping" => CheckBucket::Skipping,
        "cancel" => CheckBucket::Cancel,
        "pending" => CheckBucket::Pending,
        // Older gh without `bucket`: derive it from the state.
        _ => match state.as_str() {
            "SUCCESS" => CheckBucket::Pass,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" => CheckBucket::Fail,
            "SKIPPED" | "NEUTRAL" => CheckBucket::Skipping,
            "CANCELLED" => CheckBucket::Cancel,
            _ => CheckBucket::Pending,
        },
    };
    let status = match state.as_str() {
        "QUEUED" | "PENDING" | "WAITING" | "REQUESTED" => CheckStatus::Queued,
        "IN_PROGRESS" => CheckStatus::InProgress,
        _ if bucket == CheckBucket::Pending => CheckStatus::InProgress,
        _ => CheckStatus::Completed,
    };
    let conclusion = (status == CheckStatus::Completed).then_some(match state.as_str() {
        "SUCCESS" => CheckConclusion::Success,
        "FAILURE" | "ERROR" | "STARTUP_FAILURE" => CheckConclusion::Failure,
        "CANCELLED" => CheckConclusion::Cancelled,
        "SKIPPED" => CheckConclusion::Skipped,
        "NEUTRAL" => CheckConclusion::Neutral,
        "TIMED_OUT" => CheckConclusion::TimedOut,
        "ACTION_REQUIRED" => CheckConclusion::ActionRequired,
        _ => match bucket {
            CheckBucket::Pass => CheckConclusion::Success,
            CheckBucket::Fail => CheckConclusion::Failure,
            CheckBucket::Cancel => CheckConclusion::Cancelled,
            CheckBucket::Skipping => CheckConclusion::Skipped,
            CheckBucket::Pending => CheckConclusion::Other,
        },
    });
    let started = parse_time(&s(v, "startedAt"));
    let completed = parse_time(&s(v, "completedAt"));
    Some(Check {
        name,
        workflow: s(v, "workflow"),
        bucket,
        status,
        conclusion,
        url: s(v, "link"),
        started,
        // A pending check has no real end even when GitHub reports a zero date.
        completed: if status == CheckStatus::Completed {
            completed
        } else {
            None
        },
    })
}

/// `2026-11-23T14:04:24Z` to unix seconds. Zero / pre-epoch dates (GitHub's `0001-01-01`) and
/// anything unparseable are `None`.
pub fn parse_time(text: &str) -> Option<i64> {
    let t = text.trim();
    let (date, time) = t.split_once('T')?;
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '.']).next()?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut hms = time.split(':');
    let (h, mi, se): (i64, i64, i64) = (
        hms.next()?.parse().ok()?,
        hms.next()?.parse().ok()?,
        hms.next().unwrap_or("0").parse().ok()?,
    );
    if y < 1970 || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

/// `(run id, job id)` from a check link: `.../actions/runs/<run>/job/<job>`,
/// `.../actions/runs/<run>` or `.../runs/<job>` (a check run). `None` for anything else
/// (third-party statuses).
pub fn parse_check_url(url: &str) -> Option<(Option<u64>, Option<u64>)> {
    let path = url.split(['?', '#']).next()?;
    let segs: Vec<&str> = path.split('/').collect();
    let at = segs.iter().position(|p| *p == "runs")?;
    let first: u64 = segs.get(at + 1)?.parse().ok()?;
    let in_actions = at > 0 && segs[at - 1] == "actions";
    if !in_actions {
        return Some((None, Some(first)));
    }
    let job = match (segs.get(at + 2), segs.get(at + 3)) {
        (Some(&"job"), Some(j)) => j.parse().ok(),
        _ => None,
    };
    Some((Some(first), job))
}

/// Strips ANSI escapes, keeps the last [`MAX_LOG_BYTES`] cut at a line boundary and redacts
/// secrets. Pure.
pub fn clean_log(raw: &str) -> String {
    let plain = strip_ansi(raw);
    let tail = tail_lines(&plain, MAX_LOG_BYTES);
    store::redact(&tail).text
}

/// The last `max` bytes of `text`, starting at a line boundary (a marker line says it was cut).
pub fn tail_lines(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    let rest = match rest.find('\n') {
        Some(i) if start > 0 && text.as_bytes()[start - 1] != b'\n' => &rest[i + 1..],
        _ => rest,
    };
    format!(
        "[... log recortado, se muestran los últimos {} KB ...]\n{rest}",
        max / 1024
    )
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for n in it.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR_MIXED: &str = include_str!("../tests/fixtures/pr_list_mixed.json");
    const PR_REAL: &str = include_str!("../tests/fixtures/pr_list_merged_real.json");
    const CHECKS: &str = include_str!("../tests/fixtures/checks_mixed.json");
    const GREEN: &str = include_str!("../tests/fixtures/checks_green.json");
    const LOG: &str = include_str!("../tests/fixtures/failed_log.txt");

    #[test]
    fn parses_a_recorded_public_pr() {
        let l = parse_pr_list(PR_REAL);
        assert_eq!(l.len(), 1);
        let p = &l[0];
        assert_eq!(p.number, 15145);
        assert_eq!(p.state, PrState::Merged);
        assert_eq!(p.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(p.mergeable, Mergeable::Unknown);
        assert!(!p.draft);
        assert!(p.url.ends_with("/pull/15145"));
    }

    #[test]
    fn picks_the_open_pr_of_the_branch() {
        let list = parse_pr_list(PR_MIXED);
        assert_eq!(list.len(), 3);
        let p = pick_pr(list.clone(), "feat/ci").unwrap();
        assert_eq!(p.number, 12, "the open PR wins over the merged one");
        assert!(p.draft);
        assert_eq!(p.mergeable, Mergeable::Conflicting);
        assert_eq!(p.review_decision, Some(ReviewDecision::ChangesRequested));
        // A branch with no PR of its own gets none (13 belongs to another head).
        assert_eq!(pick_pr(list.clone(), "nope"), None);
        // Only a merged PR: it is still shown.
        let only_merged = vec![list[0].clone()];
        assert_eq!(pick_pr(only_merged, "feat/ci").unwrap().number, 9);
        // An empty review decision is None.
        assert_eq!(list[2].review_decision, None);
    }

    #[test]
    fn a_single_view_object_parses_too() {
        let one = r#"{"number":4,"state":"OPEN","title":"t","url":"u","isDraft":false,"mergeable":"MERGEABLE","reviewDecision":null,"headRefName":"b"}"#;
        let l = parse_pr_list(one);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].mergeable, Mergeable::Mergeable);
        assert_eq!(l[0].review_decision, None);
    }

    #[test]
    fn garbage_is_empty_not_a_panic() {
        for bad in [
            "",
            "null",
            "{",
            "[1,2]",
            r#"[{"number":1,"state":"WAT"}]"#,
            "<html>",
        ] {
            assert!(parse_pr_list(bad).is_empty(), "{bad}");
        }
        assert_eq!(parse_checks("not json"), None);
        assert_eq!(parse_checks("{}"), None);
        assert_eq!(parse_checks("[]").unwrap().len(), 0);
    }

    #[test]
    fn parses_checks_buckets_status_and_durations() {
        let c = parse_checks(CHECKS).unwrap();
        assert_eq!(c.len(), 7);
        assert_eq!(c[0].bucket, CheckBucket::Pass);
        assert_eq!(c[0].conclusion, Some(CheckConclusion::Success));
        assert_eq!(c[0].duration_secs(), Some(4));
        // `completedAt` before `startedAt` (as GitHub reports for skipped jobs): no duration.
        assert_eq!(c[1].bucket, CheckBucket::Skipping);
        assert_eq!(c[1].conclusion, Some(CheckConclusion::Skipped));
        assert_eq!(c[1].duration_secs(), None);
        assert!(c[2].is_failing());
        assert_eq!(c[2].conclusion, Some(CheckConclusion::Failure));
        assert_eq!(c[2].duration_secs(), Some(330));
        assert_eq!(c[2].workflow, "CI");
        // Running: no conclusion, the zero date is not a completion time.
        assert_eq!(c[4].status, CheckStatus::InProgress);
        assert_eq!(c[4].conclusion, None);
        assert_eq!(c[4].completed, None);
        // Queued: neither start nor end.
        assert_eq!(c[5].status, CheckStatus::Queued);
        assert_eq!(c[5].started, None);
        assert_eq!(c[6].bucket, CheckBucket::Cancel);
        assert_eq!(c[6].conclusion, Some(CheckConclusion::Cancelled));
        assert_eq!(
            CheckTotals::of(&c),
            CheckTotals {
                total: 7,
                pass: 1,
                fail: 2,
                pending: 2,
                skipped: 2
            }
        );
        let g = CheckTotals::of(&parse_checks(GREEN).unwrap());
        assert_eq!((g.fail, g.pending, g.pass), (0, 0, 1));
    }

    #[test]
    fn bucket_is_derived_when_gh_is_old() {
        let c = parse_checks(
            r#"[{"name":"a","state":"FAILURE"},{"name":"b","state":"SUCCESS"},{"name":"c","state":"IN_PROGRESS"}]"#,
        )
        .unwrap();
        assert_eq!(c[0].bucket, CheckBucket::Fail);
        assert_eq!(c[1].bucket, CheckBucket::Pass);
        assert_eq!(c[2].bucket, CheckBucket::Pending);
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_time("2026-11-23T14:04:24Z"), Some(1_795_442_664));
        assert_eq!(parse_time("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn check_urls() {
        assert_eq!(
            parse_check_url("https://github.com/o/r/actions/runs/777/job/888"),
            Some((Some(777), Some(888)))
        );
        assert_eq!(
            parse_check_url("https://github.com/o/r/actions/runs/777"),
            Some((Some(777), None))
        );
        assert_eq!(
            parse_check_url("https://github.com/cli/cli/runs/137746427837"),
            Some((None, Some(137_746_427_837)))
        );
        assert_eq!(parse_check_url("https://ci.example.com/build/5"), None);
        assert_eq!(parse_check_url(""), None);
    }

    #[test]
    fn logs_are_stripped_capped_and_redacted() {
        let out = clean_log(LOG);
        assert!(!out.contains('\u{1b}'), "ANSI escapes are removed");
        assert!(out.contains("unresolved import"));
        assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"));
        assert!(out.contains("oculto"), "the secret is replaced by a marker");

        // The tail is kept, cut at a line boundary, with a marker.
        let big: String = (0..40_000)
            .map(|i| format!("line {i} some log text\n"))
            .collect();
        assert!(big.len() > MAX_LOG_BYTES);
        let capped = clean_log(&big);
        assert!(capped.len() <= MAX_LOG_BYTES + 200);
        assert!(capped.starts_with("[... log recortado"));
        assert!(capped.trim_end().ends_with("line 39999 some log text"));
        assert!(
            capped.lines().nth(1).unwrap().starts_with("line "),
            "no half line"
        );
    }

    #[test]
    fn new_pr_argv_and_validation() {
        let pr = NewPr {
            title: "  feat: x ".into(),
            body: "cuerpo".into(),
            draft: true,
            base: Some("main".into()),
            head: Some("feat/x".into()),
        };
        assert_eq!(
            pr.argv(),
            [
                "gh",
                "pr",
                "create",
                "--title",
                "feat: x",
                "--body-file",
                "-",
                "--draft",
                "--base",
                "main",
                "--head",
                "feat/x"
            ]
        );
        assert!(pr.validate().is_ok());
        let plain = NewPr {
            title: "t".into(),
            ..Default::default()
        };
        assert_eq!(
            plain.argv(),
            ["gh", "pr", "create", "--title", "t", "--body-file", "-"]
        );
        for bad in [
            NewPr::default(),
            NewPr {
                title: "a\nb".into(),
                ..Default::default()
            },
            NewPr {
                title: "t".into(),
                base: Some("--force".into()),
                ..Default::default()
            },
            NewPr {
                title: "t".into(),
                head: Some("a b".into()),
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
