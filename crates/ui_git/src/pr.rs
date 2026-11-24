//! Pull request and CI state of the current branch (backlog B1) and the "Crear PR" flow (B3):
//! plain data and pure rules, no gpui. The model that owns the state is [`crate::GitModel`].
//!
//! # When `gh` is asked (never on a timer)
//!
//! | Trigger | Where |
//! |---|---|
//! | the branch changed (also the first read) | `GitModel` after each repository read |
//! | a push finished (push, commit + push, publish) | `GitModel` after the operation |
//! | explicit refresh (the section's button) | [`crate::GitModel::refresh_pr_now`] |
//! | window focus, only when the last query is older than [`FOCUS_STALE`] | [`crate::GitModel::on_window_focus`] |
//! | after creating a PR | `GitModel` |
//!
//! At most one query is in flight; another one asked meanwhile is coalesced into one more.

use std::time::{Duration, Instant};

use git_svc::{
    Check, CheckBucket, CheckConclusion, CheckStatus, CheckTotals, GhClient, Mergeable, NewPr,
    PrInfo, PrState, Repo, tail_lines,
};

use crate::cmd::GitOp;

/// A window-focus refresh happens only when the last query is older than this.
pub const FOCUS_STALE: Duration = Duration::from_secs(5 * 60);
/// Failing checks whose log is sent to the agent.
pub const MAX_FAILED_SENT: usize = 3;
/// Log lines kept per failing check when sending them to the agent.
pub const LOG_EXCERPT_BYTES: usize = 8 * 1024;

/// Why a PR query runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrTrigger {
    BranchChanged,
    AfterPush,
    Manual,
    WindowFocus,
    AfterCreate,
}

/// What the panel knows about the PR of the current branch.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum PrView {
    /// Nothing asked yet (or the integration is off).
    #[default]
    Idle,
    /// `gh` is missing or not logged in: every PR element is hidden.
    Unavailable,
    /// The first query is running.
    Loading,
    /// Detached HEAD (or no commits yet): there is no branch to ask about.
    Detached,
    /// The branch has no pull request (a PR can be created).
    NoPr {
        branch: String,
    },
    Ready(std::sync::Arc<PrSnapshot>),
}

impl PrView {
    /// The PR section / pill should be shown at all.
    pub fn is_visible(&self) -> bool {
        !matches!(self, PrView::Idle | PrView::Unavailable | PrView::Detached)
    }
}

/// A PR and its checks, as of one query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrSnapshot {
    pub branch: String,
    pub pr: PrInfo,
    pub checks: Vec<Check>,
    /// The checks query worked (an empty list with `false` means "unknown", not "no CI").
    pub checks_known: bool,
}

impl PrSnapshot {
    pub fn totals(&self) -> CheckTotals {
        CheckTotals::of(&self.checks)
    }

    pub fn failing(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.is_failing())
    }
}

/// Tone of the pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrTone {
    Good,
    Bad,
    Pending,
    Neutral,
}

/// The one-line summary for the panel pill and the app status bar:
/// `PR #12 · 2 checks fallan` / `PR #12 · ✓ CI`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrSummary {
    pub number: u64,
    pub text: String,
    pub tone: PrTone,
    /// The PR page (the pill opens it).
    pub url: String,
}

/// Builds the summary of a PR and its checks. Pure.
pub fn summarize(pr: &PrInfo, checks: &[Check]) -> PrSummary {
    let t = CheckTotals::of(checks);
    let mut parts = vec![format!("PR #{}", pr.number)];
    let mut tone = PrTone::Neutral;
    match pr.state {
        PrState::Merged => parts.push("mergeado".into()),
        PrState::Closed => parts.push("cerrado".into()),
        PrState::Open => {
            if pr.draft {
                parts.push("borrador".into());
            }
            if t.fail > 0 {
                parts.push(if t.fail == 1 {
                    "1 check falla".into()
                } else {
                    format!("{} checks fallan", t.fail)
                });
                tone = PrTone::Bad;
            } else if t.pending > 0 {
                parts.push("CI en curso".into());
                tone = PrTone::Pending;
            } else if t.pass > 0 {
                parts.push("✓ CI".into());
                tone = PrTone::Good;
            }
            if pr.mergeable == Mergeable::Conflicting {
                parts.push("conflictos".into());
                tone = PrTone::Bad;
            }
        }
    }
    PrSummary {
        number: pr.number,
        text: parts.join(" · "),
        tone,
        url: pr.url.clone(),
    }
}

/// A window-focus refresh is due: never queried, or the last query is older than [`FOCUS_STALE`].
pub fn focus_refresh_due(last: Option<Instant>, now: Instant) -> bool {
    match last {
        None => true,
        Some(t) => now.saturating_duration_since(t) > FOCUS_STALE,
    }
}

/// A finished operation that publishes commits: the PR (and its CI) may have changed.
pub fn refresh_after(op: &GitOp, ok: bool) -> bool {
    ok && matches!(
        op,
        GitOp::Action(git_svc::GitAction::Push)
            | GitOp::CommitAndPush { .. }
            | GitOp::PublishBranch { .. }
    )
}

/// The result of one background query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrQuery {
    /// `gh` missing or not authenticated.
    Unavailable,
    /// Detached HEAD or no commits: nothing to ask.
    NoBranch,
    NoPr {
        branch: String,
    },
    Ready(PrSnapshot),
}

/// Runs the queries (blocking: call it on the background executor). `known_available` skips the
/// auth probe when a previous query already proved `gh` works.
pub fn query_pr(client: &GhClient, repo: &Repo, known_available: bool) -> PrQuery {
    if !known_available && !client.available() {
        return PrQuery::Unavailable;
    }
    let Some(branch) = repo.status().ok().and_then(|s| s.branch) else {
        return PrQuery::NoBranch;
    };
    let Some(pr) = client.pr_for_branch(repo, &branch) else {
        return PrQuery::NoPr { branch };
    };
    let checks = client.checks(repo, pr.number);
    PrQuery::Ready(PrSnapshot {
        branch,
        checks_known: checks.is_some(),
        checks: checks.unwrap_or_default(),
        pr,
    })
}

/// Markdown handed to the agent for "Enviar checks fallidos al agente": each failing check with
/// the tail of its (already redacted) log. Pure.
pub fn failed_checks_markdown(snapshot: &PrSnapshot, logs: &[(Check, String)]) -> String {
    let failing = snapshot.failing().count();
    let mut out = format!(
        "## CI falla en el PR #{} «{}»\n\nRama `{}`. {} de {} checks fallan.\n",
        snapshot.pr.number,
        snapshot.pr.title,
        snapshot.branch,
        failing,
        snapshot.checks.len()
    );
    for (check, log) in logs {
        out.push_str(&format!("\n### {}", check.name));
        if !check.workflow.is_empty() {
            out.push_str(&format!(" ({})", check.workflow));
        }
        out.push('\n');
        if !check.url.is_empty() {
            out.push_str(&format!("Enlace: {}\n", check.url));
        }
        let excerpt = tail_lines(log.trim_end(), LOG_EXCERPT_BYTES);
        if excerpt.trim().is_empty() {
            out.push_str("(sin log disponible para este check)\n");
        } else {
            out.push_str("\n```text\n");
            out.push_str(&excerpt);
            out.push_str("\n```\n");
        }
    }
    if failing > logs.len() {
        out.push_str(&format!(
            "\n… y {} más (sin log incluido).\n",
            crate::fmt::plural(failing - logs.len(), "check fallido", "checks fallidos")
        ));
    }
    out.push_str("\nAnaliza por qué falla el CI y propón la corrección.\n");
    out
}

/// Where "Crear PR" is.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum PrCreate {
    #[default]
    Idle,
    /// The confirmation card shows the exact command; nothing ran yet.
    Confirm(NewPr),
    Running(NewPr),
    Done {
        url: String,
    },
    Failed {
        pr: NewPr,
        error: String,
    },
}

/// A PR was created (the host may open it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrCreated {
    pub url: String,
}

/// The command as the confirmation card shows it: arguments with spaces are double-quoted.
pub fn display_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Short status glyph and its role for a check row.
pub fn check_glyph(c: &Check) -> (&'static str, PrTone) {
    match (c.status, c.conclusion, c.bucket) {
        (CheckStatus::Completed, Some(CheckConclusion::Success), _) => ("✓", PrTone::Good),
        (CheckStatus::Completed, _, CheckBucket::Fail) => ("✗", PrTone::Bad),
        (CheckStatus::Completed, _, CheckBucket::Pass) => ("✓", PrTone::Good),
        (CheckStatus::Completed, _, _) => ("–", PrTone::Neutral),
        (CheckStatus::InProgress, _, _) => ("●", PrTone::Pending),
        (CheckStatus::Queued, _, _) => ("○", PrTone::Pending),
    }
}

/// `45 s`, `3 min 5 s`, `1 h 02 min`.
pub fn format_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 if secs.is_multiple_of(60) => format!("{} min", secs / 60),
        60..=3599 => format!("{} min {} s", secs / 60, secs % 60),
        _ => format!("{} h {:02} min", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_svc::{parse_checks, parse_pr_list};

    const PRS: &str = include_str!("../../git_svc/tests/fixtures/pr_list_mixed.json");
    const CHECKS: &str = include_str!("../../git_svc/tests/fixtures/checks_mixed.json");
    const GREEN: &str = include_str!("../../git_svc/tests/fixtures/checks_green.json");

    fn open_pr() -> PrInfo {
        parse_pr_list(PRS).remove(1)
    }

    #[test]
    fn summary_wording_and_tone() {
        let mut pr = open_pr();
        pr.draft = false;
        pr.mergeable = Mergeable::Mergeable;
        let failing = parse_checks(CHECKS).unwrap();
        let s = summarize(&pr, &failing);
        assert_eq!(s.text, "PR #12 · 2 checks fallan");
        assert_eq!(s.tone, PrTone::Bad);

        let green = parse_checks(GREEN).unwrap();
        let s = summarize(&pr, &green);
        assert_eq!(s.text, "PR #12 · ✓ CI");
        assert_eq!(s.tone, PrTone::Good);
        assert_eq!(s.url, pr.url);

        let one_fail: Vec<Check> = failing
            .iter()
            .filter(|c| c.name == "clippy")
            .cloned()
            .collect();
        assert_eq!(summarize(&pr, &one_fail).text, "PR #12 · 1 check falla");

        let running: Vec<Check> = failing
            .iter()
            .filter(|c| c.bucket == CheckBucket::Pending)
            .cloned()
            .collect();
        let s = summarize(&pr, &running);
        assert_eq!(s.text, "PR #12 · CI en curso");
        assert_eq!(s.tone, PrTone::Pending);

        assert_eq!(summarize(&pr, &[]).text, "PR #12");
        pr.draft = true;
        pr.mergeable = Mergeable::Conflicting;
        assert_eq!(
            summarize(&pr, &green).text,
            "PR #12 · borrador · ✓ CI · conflictos"
        );
        assert_eq!(summarize(&pr, &green).tone, PrTone::Bad);

        pr.state = PrState::Merged;
        assert_eq!(summarize(&pr, &failing).text, "PR #12 · mergeado");
        pr.state = PrState::Closed;
        assert_eq!(summarize(&pr, &failing).tone, PrTone::Neutral);
    }

    #[test]
    fn focus_refresh_needs_a_stale_query() {
        let t0 = Instant::now();
        assert!(focus_refresh_due(None, t0), "never queried");
        assert!(!focus_refresh_due(Some(t0), t0 + Duration::from_secs(60)));
        assert!(!focus_refresh_due(Some(t0), t0 + FOCUS_STALE));
        assert!(focus_refresh_due(
            Some(t0),
            t0 + FOCUS_STALE + Duration::from_secs(1)
        ));
        // A clock that went backwards is not "stale".
        assert!(!focus_refresh_due(Some(t0 + Duration::from_secs(9)), t0));
    }

    #[test]
    fn only_publishing_operations_refresh_after() {
        use git_svc::GitAction;
        assert!(refresh_after(&GitOp::Action(GitAction::Push), true));
        assert!(refresh_after(
            &GitOp::CommitAndPush {
                message: "m".into()
            },
            true
        ));
        assert!(refresh_after(
            &GitOp::PublishBranch { branch: "b".into() },
            true
        ));
        assert!(!refresh_after(&GitOp::Action(GitAction::Push), false));
        assert!(!refresh_after(&GitOp::Action(GitAction::Fetch), true));
        assert!(!refresh_after(&GitOp::Action(GitAction::Pull), true));
        assert!(!refresh_after(&GitOp::StageAll, true));
    }

    #[test]
    fn failed_checks_markdown_carries_names_links_and_log_tails() {
        let checks = parse_checks(CHECKS).unwrap();
        let snap = PrSnapshot {
            branch: "feat/ci".into(),
            pr: open_pr(),
            checks,
            checks_known: true,
        };
        let failing: Vec<Check> = snap.failing().cloned().collect();
        let logs = vec![
            (
                failing[0].clone(),
                "error[E0432]: unresolved import".to_string(),
            ),
            (failing[1].clone(), String::new()),
        ];
        let md = failed_checks_markdown(&snap, &logs);
        assert!(md.contains("PR #12"));
        assert!(md.contains("2 de 7 checks fallan"));
        assert!(md.contains("### test (windows-latest) (CI)"));
        assert!(md.contains("actions/runs/777/job/888"));
        assert!(md.contains("unresolved import"));
        assert!(md.contains("sin log disponible"));
        assert!(!md.contains("sin log incluido"));
        // Fewer logs than failing checks says so.
        let md = failed_checks_markdown(&snap, &logs[..1]);
        assert!(md.contains("1 check fallido más (sin log incluido)"));
    }

    #[test]
    fn command_display_quotes_what_needs_it() {
        let a = NewPr {
            title: "feat: algo bonito".into(),
            draft: true,
            ..Default::default()
        }
        .argv();
        assert_eq!(
            display_command(&a),
            "gh pr create --title \"feat: algo bonito\" --body-file - --draft"
        );
    }

    #[test]
    fn glyphs_and_durations() {
        let c = parse_checks(CHECKS).unwrap();
        assert_eq!(check_glyph(&c[0]).0, "✓");
        assert_eq!(check_glyph(&c[1]), ("–", PrTone::Neutral));
        assert_eq!(check_glyph(&c[2]), ("✗", PrTone::Bad));
        assert_eq!(check_glyph(&c[4]), ("●", PrTone::Pending));
        assert_eq!(check_glyph(&c[5]), ("○", PrTone::Pending));
        assert_eq!(format_duration(45), "45 s");
        assert_eq!(format_duration(185), "3 min 5 s");
        assert_eq!(format_duration(120), "2 min");
        assert_eq!(format_duration(3720), "1 h 02 min");
    }
}
