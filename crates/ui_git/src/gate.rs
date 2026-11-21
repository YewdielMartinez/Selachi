//! Pure reducers: the confirmation gate, the single-in-flight refresh gate and the review card
//! shown before a sensitive operation.
//!
//! None of this knows gpui or git: the model feeds it events and does what it answers.

use git_svc::{Preflight, RepoStatus, RestorePlan};

use crate::cmd::GitOp;
use crate::fmt::{plural, slashes};

// ---------------------------------------------------------------------------------------------
// Review card
// ---------------------------------------------------------------------------------------------

/// Role of a line of a [`Review`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// A file that will be re-created.
    Create,
    /// A file that will be overwritten.
    Overwrite,
    /// A file that will be deleted.
    Delete,
    /// Left alone.
    Skip,
    Warn,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewLine {
    pub kind: LineKind,
    pub text: String,
}

/// What the confirmation card shows before a sensitive operation runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub title: String,
    pub confirm_label: String,
    pub lines: Vec<ReviewLine>,
    /// `git diff --stat` of what the action affects.
    pub stat: Option<String>,
    /// Why the operation cannot run (the confirm button is disabled).
    pub blocked: Option<String>,
    /// Destructive: the card is tinted with the error color.
    pub danger: bool,
}

impl Review {
    fn line(&mut self, kind: LineKind, text: impl Into<String>) {
        self.lines.push(ReviewLine { kind, text: text.into() });
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ReviewLine> {
        self.lines.iter().filter(|l| l.kind == LineKind::Warn)
    }
}

/// Spanish text for the (English) warnings of `git_svc`. Unknown ones pass through.
pub fn translate_warning(w: &str) -> String {
    let num = |s: &str| s.split_whitespace().find_map(|t| t.parse::<u32>().ok());
    if w == "nothing to commit" {
        "No hay nada que confirmar".into()
    } else if w.starts_with("nothing is staged") {
        "No hay nada preparado: usa + en un archivo".into()
    } else if w.starts_with("HEAD is detached; the commit") {
        "HEAD está separado: el commit no quedará en una rama".into()
    } else if w.starts_with("HEAD is detached; nothing to push") {
        "HEAD está separado: no hay nada que enviar".into()
    } else if w.starts_with("branch has no upstream; push") {
        "La rama no tiene upstream: usa «Publicar»".into()
    } else if w.contains("behind upstream; push will likely be rejected") {
        format!(
            "La rama va {} commit(s) por detrás del upstream: el push probablemente será rechazado",
            num(w).unwrap_or(0)
        )
    } else if w == "no local commits to push" {
        "No hay commits locales que enviar".into()
    } else if w.starts_with("branch has no upstream to pull") {
        "La rama no tiene upstream desde el que hacer pull".into()
    } else if w.starts_with("working tree has uncommitted changes; pull") {
        "Hay cambios sin confirmar: el pull puede generar conflictos".into()
    } else if w.starts_with("branch has diverged") {
        let mut n = w.split(|c: char| !c.is_ascii_digit()).filter_map(|t| t.parse::<u32>().ok());
        let (a, b) = (n.next().unwrap_or(0), n.next().unwrap_or(0));
        format!("La rama ha divergido ({a} por delante, {b} por detrás): el pull fusionará o rebasará")
    } else if w.contains("have unresolved conflicts") {
        format!("{} archivo(s) con conflictos sin resolver", num(w).unwrap_or(0))
    } else if w.starts_with("working tree has uncommitted changes that may be overwritten") {
        "Hay cambios sin confirmar que podrían sobrescribirse".into()
    } else if w.starts_with("worktree is locked") {
        format!("El worktree está bloqueado{}", w.trim_start_matches("worktree is locked"))
    } else if w.starts_with("directory is already missing") {
        "La carpeta ya no existe: solo se limpian los metadatos".into()
    } else if w.contains("file(s) with uncommitted changes will be lost") {
        format!("{} archivo(s) con cambios sin confirmar se perderán", num(w).unwrap_or(0))
    } else if let Some((head, rest)) = w.split_once(" commit(s) on ") {
        let n = head.trim();
        let what = rest.split(" are not pushed").next().unwrap_or(rest);
        format!("{n} commit(s) de {what} no están en ningún remoto ni en otra rama")
    } else {
        w.to_string()
    }
}

/// Builds the card for `op` from the service's [`Preflight`] (and the [`RestorePlan`] for a
/// restore), with the current `status` to decide when the operation is blocked.
pub fn build_review(
    op: &GitOp,
    pf: &Preflight,
    plan: Option<&RestorePlan>,
    status: &RepoStatus,
) -> Review {
    let mut r = Review {
        title: op.title().to_string(),
        confirm_label: op.title().to_string(),
        lines: Vec::new(),
        stat: pf.diff_stat.clone(),
        blocked: None,
        danger: false,
    };
    let commit_checks = |r: &mut Review| {
        if status.staged.is_empty() {
            r.blocked = Some("No hay nada preparado: usa + en un archivo".into());
        } else if !status.conflicted.is_empty() {
            r.blocked = Some(format!(
                "{} con conflictos sin resolver",
                plural(status.conflicted.len(), "archivo", "archivos")
            ));
        }
    };
    match op {
        GitOp::Action(git_svc::GitAction::Commit { message }) => {
            commit_checks(&mut r);
            r.line(LineKind::Info, format!("Mensaje: {}", crate::fmt::first_line(message)));
            r.line(
                LineKind::Info,
                format!("{} en staged", plural(status.staged.len(), "archivo", "archivos")),
            );
        }
        GitOp::CommitAndPush { message } => {
            commit_checks(&mut r);
            r.line(LineKind::Info, format!("Mensaje: {}", crate::fmt::first_line(message)));
            r.line(
                LineKind::Info,
                format!(
                    "{} en staged, y después git push",
                    plural(status.staged.len(), "archivo", "archivos")
                ),
            );
            if status.upstream.is_none() && !status.detached {
                r.line(LineKind::Warn, "La rama no tiene upstream: el push fallará. Usa «Publicar» después");
            }
        }
        GitOp::PublishBranch { branch } => {
            r.confirm_label = "Publicar".into();
            r.line(LineKind::Info, format!("Se creará origin/{branch} y quedará como upstream"));
        }
        GitOp::RemoveWorktree { .. } => {
            r.confirm_label = "Eliminar".into();
            r.danger = true;
            r.line(LineKind::Info, "La rama se conserva; solo se borra la carpeta");
        }
        GitOp::RestoreCheckpoint { .. } => {
            r.confirm_label = "Restaurar".into();
            r.danger = true;
            match plan {
                Some(p) => {
                    for f in &p.create {
                        r.line(LineKind::Create, slashes(f));
                    }
                    for f in &p.overwrite {
                        r.line(LineKind::Overwrite, slashes(f));
                    }
                    for f in &p.delete {
                        r.line(LineKind::Delete, slashes(f));
                    }
                    for (f, why) in &p.skipped {
                        r.line(LineKind::Skip, format!("{} ({why})", slashes(f)));
                    }
                    if p.is_empty() {
                        r.blocked = Some("Nada que restaurar: el árbol ya coincide con el checkpoint".into());
                    } else {
                        r.line(LineKind::Info, "Antes se crea un checkpoint «antes de restaurar» para poder deshacer");
                    }
                }
                None => r.blocked = Some("No se pudo calcular el plan de restauración".into()),
            }
            // The per-file warnings of the service are covered by the plan above.
            return r;
        }
        _ => {}
    }
    for w in &pf.warnings {
        // The checks above already state "nothing staged" and conflicts.
        let t = translate_warning(w);
        if !r.lines.iter().any(|l| l.text == t) && r.blocked.as_deref() != Some(t.as_str()) {
            r.line(LineKind::Warn, t);
        }
    }
    r
}

// ---------------------------------------------------------------------------------------------
// Confirmation gate
// ---------------------------------------------------------------------------------------------

/// Result of an operation that ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ok: bool,
    /// One line for the banner.
    pub summary: String,
    /// Extra text (stderr / a report), shown below the summary.
    pub detail: String,
    /// The command line to retry visibly in a terminal, when the failure may need credentials.
    pub retry: Option<Vec<String>>,
}

/// The confirmation flow of the model. A sensitive operation can only reach [`Gate::Running`]
/// through [`Gate::Confirm`], and [`Gate::confirm`] is the only way out of it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Gate {
    #[default]
    Idle,
    /// The preflight is being computed.
    Preflighting { op: GitOp, epoch: u64 },
    /// The card is shown, waiting for "Confirmar" / "Cancelar".
    Confirm { op: GitOp, review: Review },
    Running { op: GitOp },
    /// Banner with the result of the last operation.
    Done { op: GitOp, outcome: Outcome },
}

/// What the model must do after [`Gate::begin`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Not sensitive: run it now.
    Run(GitOp),
    /// Sensitive: compute the preflight for this epoch, then call [`Gate::preflight_ready`].
    Preflight { op: GitOp, epoch: u64 },
    /// Another confirmation or run is in progress.
    Busy,
}

impl Gate {
    /// Starts an operation. Sensitive ones go through the preflight and the card; the others run.
    pub fn begin(&mut self, op: GitOp, epoch: u64) -> Step {
        // One sensitive flow at a time. A quiet operation (stage, fetch...) never waits for it.
        if op.is_sensitive() && matches!(self, Gate::Preflighting { .. } | Gate::Running { .. }) {
            return Step::Busy;
        }
        if op.is_sensitive() {
            *self = Gate::Preflighting { op: op.clone(), epoch };
            Step::Preflight { op, epoch }
        } else {
            // A quiet operation does not dismiss a card the user is still reading, nor a flow in
            // progress; it only clears an old result banner.
            if matches!(self, Gate::Done { .. }) {
                *self = Gate::Idle;
            }
            Step::Run(op)
        }
    }

    /// The preflight finished. Ignored when it is stale (the user cancelled or switched repo).
    pub fn preflight_ready(&mut self, epoch: u64, review: Result<Review, String>) {
        let Gate::Preflighting { op, epoch: e } = self else { return };
        if *e != epoch {
            return;
        }
        let op = op.clone();
        *self = match review {
            Ok(review) => Gate::Confirm { op, review },
            Err(e) => Gate::Done {
                outcome: Outcome { ok: false, summary: format!("{}: {e}", op.title()), detail: String::new(), retry: None },
                op,
            },
        };
    }

    /// "Confirmar": the operation to run, only when the card is showing and the operation is not
    /// blocked. This is the single place a sensitive operation is released.
    pub fn confirm(&mut self) -> Option<GitOp> {
        let Gate::Confirm { op, review } = self else { return None };
        if review.blocked.is_some() {
            return None;
        }
        let op = op.clone();
        *self = Gate::Running { op: op.clone() };
        Some(op)
    }

    /// "Cancelar" / dismiss the banner.
    pub fn cancel(&mut self) {
        match self {
            Gate::Preflighting { .. } | Gate::Confirm { .. } | Gate::Done { .. } => *self = Gate::Idle,
            Gate::Running { .. } | Gate::Idle => {}
        }
    }

    /// An operation finished. Quiet successes leave no banner. Returns `false` when the outcome
    /// could not be shown because a card is on screen (the caller keeps it as a plain error).
    pub fn finished(&mut self, op: GitOp, outcome: Outcome) -> bool {
        match self {
            Gate::Confirm { .. } | Gate::Preflighting { .. } => return false,
            // The result of a quiet operation must not close the banner of the running one.
            Gate::Running { op: current } if *current != op => return false,
            _ => {}
        }
        *self = if outcome.ok && !op.is_sensitive() && outcome.detail.is_empty() && !is_reported(&op) {
            Gate::Idle
        } else {
            Gate::Done { op, outcome }
        };
        true
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Gate::Preflighting { .. } | Gate::Running { .. })
    }
}

/// Quiet operations whose success is still worth a banner.
fn is_reported(op: &GitOp) -> bool {
    matches!(
        op,
        GitOp::Action(git_svc::GitAction::Fetch)
            | GitOp::CreateWorktree { .. }
            | GitOp::CreateCheckpoint { .. }
    )
}

// ---------------------------------------------------------------------------------------------
// Refresh gate
// ---------------------------------------------------------------------------------------------

/// One status read in flight at most, and one queued follow-up for everything that arrives
/// meanwhile (a burst of file events costs two reads, not twenty).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefreshGate {
    in_flight: bool,
    queued: bool,
    /// The queued follow-up must also re-read the commit log (an operation changed history).
    queued_full: bool,
}

impl RefreshGate {
    /// A refresh was requested. `true` = start it now; `false` = it was queued behind the one in
    /// flight.
    pub fn request(&mut self, full: bool) -> bool {
        if self.in_flight {
            self.queued = true;
            self.queued_full |= full;
            false
        } else {
            self.in_flight = true;
            true
        }
    }

    /// The read finished. `Some(full)` = run the queued follow-up now.
    pub fn finish(&mut self) -> Option<bool> {
        if self.queued {
            let full = self.queued_full;
            self.queued = false;
            self.queued_full = false;
            Some(full)
        } else {
            self.in_flight = false;
            None
        }
    }

    /// Forgets everything (the repo changed or the panel was hidden).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use git_svc::{FileChange, GitAction};

    use super::*;

    fn commit(msg: &str) -> GitOp {
        GitOp::Action(GitAction::Commit { message: msg.into() })
    }

    fn status_with_staged(n: usize) -> RepoStatus {
        RepoStatus {
            staged: (0..n)
                .map(|i| FileChange {
                    path: PathBuf::from(format!("f{i}.rs")),
                    orig_path: None,
                    kind: git_svc::ChangeKind::Modified,
                })
                .collect(),
            ..RepoStatus::default()
        }
    }

    fn ok_outcome() -> Outcome {
        Outcome { ok: true, summary: "ok".into(), detail: String::new(), retry: None }
    }

    #[test]
    fn quiet_operations_run_without_a_card() {
        let mut g = Gate::default();
        assert_eq!(g.begin(GitOp::Stage("a".into()), 1), Step::Run(GitOp::Stage("a".into())));
        assert_eq!(g, Gate::Idle);
        assert_eq!(g.begin(GitOp::Action(GitAction::Fetch), 1), Step::Run(GitOp::Action(GitAction::Fetch)));
    }

    #[test]
    fn sensitive_operations_never_run_without_confirm() {
        for op in [
            commit("m"),
            GitOp::Action(GitAction::Push),
            GitOp::Action(GitAction::Pull),
            GitOp::CommitAndPush { message: "m".into() },
            GitOp::RemoveWorktree { path: "w".into(), force: true },
            GitOp::RestoreCheckpoint { id: "1".into() },
        ] {
            let mut g = Gate::default();
            // begin never returns Run for a sensitive op.
            let step = g.begin(op.clone(), 7);
            assert_eq!(step, Step::Preflight { op: op.clone(), epoch: 7 });
            // Confirming before the preflight arrived does nothing.
            assert_eq!(g.confirm(), None, "{op:?}");
            assert!(matches!(g, Gate::Preflighting { .. }));
            // The card shows; only confirm releases it.
            let review = build_review(&op, &Preflight::default(), Some(&RestorePlan { overwrite: vec!["a".into()], ..RestorePlan::default() }), &status_with_staged(1));
            g.preflight_ready(7, Ok(review));
            assert!(matches!(g, Gate::Confirm { .. }));
            assert_eq!(g.confirm(), Some(op.clone()));
            // A second confirm cannot release it again.
            assert_eq!(g.confirm(), None);
            assert!(matches!(g, Gate::Running { .. }));
        }
    }

    #[test]
    fn cancel_and_stale_preflights_release_nothing() {
        let mut g = Gate::default();
        let op = GitOp::Action(GitAction::Push);
        g.begin(op.clone(), 3);
        g.cancel();
        assert_eq!(g, Gate::Idle);
        // The late preflight of the cancelled request is ignored.
        g.preflight_ready(3, Ok(build_review(&op, &Preflight::default(), None, &RepoStatus::default())));
        assert_eq!(g, Gate::Idle);
        assert_eq!(g.confirm(), None);

        // A newer request supersedes an older epoch.
        g.begin(op.clone(), 4);
        g.preflight_ready(3, Ok(build_review(&op, &Preflight::default(), None, &RepoStatus::default())));
        assert!(matches!(g, Gate::Preflighting { .. }));
    }

    #[test]
    fn blocked_reviews_cannot_be_confirmed() {
        let mut g = Gate::default();
        let op = commit("m");
        g.begin(op.clone(), 1);
        let review = build_review(&op, &Preflight::default(), None, &status_with_staged(0));
        assert!(review.blocked.is_some());
        g.preflight_ready(1, Ok(review));
        assert_eq!(g.confirm(), None, "nothing staged: commit is blocked");
        assert!(matches!(g, Gate::Confirm { .. }), "the card stays so the user can read why");
    }

    #[test]
    fn busy_gate_refuses_new_sensitive_requests() {
        let mut g = Gate::default();
        g.begin(GitOp::Action(GitAction::Push), 1);
        assert_eq!(g.begin(GitOp::Action(GitAction::Pull), 2), Step::Busy);
        assert!(g.is_busy());
        // Staging is not blocked by a flow in progress.
        assert_eq!(g.begin(GitOp::StageAll, 3), Step::Run(GitOp::StageAll));
        assert!(g.is_busy());
    }

    #[test]
    fn quiet_success_leaves_no_banner_but_failures_and_reports_do() {
        let mut g = Gate::default();
        assert!(g.finished(GitOp::Stage("a".into()), ok_outcome()));
        assert_eq!(g, Gate::Idle);

        g.finished(GitOp::Stage("a".into()), Outcome { ok: false, summary: "x".into(), detail: String::new(), retry: None });
        assert!(matches!(g, Gate::Done { .. }));

        let mut g = Gate::default();
        g.finished(GitOp::CreateCheckpoint { label: "x".into() }, ok_outcome());
        assert!(matches!(g, Gate::Done { .. }));
        g.cancel();
        assert_eq!(g, Gate::Idle);
    }

    #[test]
    fn quiet_operations_do_not_dismiss_a_card_being_read() {
        let mut g = Gate::default();
        let op = GitOp::Action(GitAction::Push);
        g.begin(op.clone(), 1);
        g.preflight_ready(1, Ok(build_review(&op, &Preflight::default(), None, &RepoStatus::default())));
        assert_eq!(g.begin(GitOp::Stage("a".into()), 2), Step::Run(GitOp::Stage("a".into())));
        assert!(!g.finished(GitOp::Stage("a".into()), ok_outcome()), "outcome not shown over the card");
        assert!(matches!(g, Gate::Confirm { .. }));

        // The result of a quiet op does not close the banner of the running sensitive one.
        assert_eq!(g.confirm(), Some(op.clone()));
        assert!(!g.finished(GitOp::StageAll, ok_outcome()));
        assert!(matches!(g, Gate::Running { .. }));
        assert!(g.finished(op, ok_outcome()));
        assert!(matches!(g, Gate::Done { .. }));
    }

    #[test]
    fn refresh_gate_keeps_one_in_flight_and_one_queued() {
        let mut r = RefreshGate::default();
        assert!(r.request(false));
        for _ in 0..10 {
            assert!(!r.request(false), "queued behind the in-flight read");
        }
        assert_eq!(r.finish(), Some(false), "exactly one follow-up");
        assert!(r.in_flight());
        assert_eq!(r.finish(), None);
        assert!(!r.in_flight());
        assert!(r.request(true));
    }

    #[test]
    fn queued_follow_up_inherits_a_full_request() {
        let mut r = RefreshGate::default();
        assert!(r.request(false));
        assert!(!r.request(true));
        assert!(!r.request(false));
        assert_eq!(r.finish(), Some(true));
        assert_eq!(r.finish(), None);
    }

    #[test]
    fn warnings_are_translated() {
        assert_eq!(translate_warning("nothing to commit"), "No hay nada que confirmar");
        assert_eq!(
            translate_warning("branch is 3 commit(s) behind upstream; push will likely be rejected"),
            "La rama va 3 commit(s) por detrás del upstream: el push probablemente será rechazado"
        );
        assert_eq!(
            translate_warning("branch has diverged (2 ahead, 5 behind); pull will merge or rebase"),
            "La rama ha divergido (2 por delante, 5 por detrás): el pull fusionará o rebasará"
        );
        assert_eq!(
            translate_warning("4 file(s) with uncommitted changes will be lost"),
            "4 archivo(s) con cambios sin confirmar se perderán"
        );
        assert_eq!(translate_warning("something new"), "something new");
    }

    #[test]
    fn restore_review_lists_every_file_by_kind() {
        let plan = RestorePlan {
            create: vec!["a.rs".into()],
            overwrite: vec!["b.rs".into(), "c.rs".into()],
            delete: vec!["d.rs".into()],
            skipped: vec![("sub".into(), "submodule".into())],
        };
        let op = GitOp::RestoreCheckpoint { id: "1".into() };
        let r = build_review(&op, &Preflight::default(), Some(&plan), &RepoStatus::default());
        let kinds: Vec<LineKind> = r.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Create,
                LineKind::Overwrite,
                LineKind::Overwrite,
                LineKind::Delete,
                LineKind::Skip,
                LineKind::Info
            ]
        );
        assert!(r.danger && r.blocked.is_none());
        let empty = build_review(&op, &Preflight::default(), Some(&RestorePlan::default()), &RepoStatus::default());
        assert!(empty.blocked.is_some());
    }

    #[test]
    fn commit_review_blocks_on_conflicts_and_translates_service_warnings() {
        let mut st = status_with_staged(2);
        st.conflicted.push("x.rs".into());
        let op = commit("feat: x\n\nbody");
        let pf = Preflight { warnings: vec!["HEAD is detached; the commit will not be on a branch".into()], ..Preflight::default() };
        let r = build_review(&op, &pf, None, &st);
        assert_eq!(r.blocked.as_deref(), Some("1 archivo con conflictos sin resolver"));
        assert!(r.warnings().any(|l| l.text.contains("HEAD está separado")));
        assert!(r.lines.iter().any(|l| l.text == "Mensaje: feat: x"));
    }
}
