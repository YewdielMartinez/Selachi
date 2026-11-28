//! The GUI side of the approvals of delegated agents: what is waiting for the user, in order.
//!
//! [`ApprovalQueue`] receives [`ServerEvent`]s from the pipe server (see `approvals.rs`) through a
//! channel (a waker: nothing runs while nobody asks), keeps the pending requests and answers them.
//! Nothing is ever approved here on its own: only [`ApprovalQueue::resolve`], called by a button the
//! user pressed, allows anything; a request that expires is already denied by the server.
//!
//! The shell turns each pending request into an attention item, a toast (window not focused) and a
//! card (`approvals_ui.rs`).

use agent_harness::{ApprovalRequest, Decision};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Context, EventEmitter, Task};

use crate::approvals::{Incoming, ServerEvent};

/// `tool` of the confirmation of `apply_changes` (see `agent_harness::mcp`).
pub const APPLY_TOOL: &str = "selachi:apply_changes";
/// Most requests kept at once; more are denied right away ("cola llena").
pub const MAX_PENDING: usize = 20;

/// The plan an `apply_changes` confirmation carries (the harness renders it as text).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanView {
    pub source: String,
    pub target: String,
    /// `(change, path)`: `added | modified | deleted | type_changed`.
    pub files: Vec<(String, String)>,
    /// The `git diff --stat` lines.
    pub stat: Vec<String>,
    /// `WARNING:` lines (already applied, files that also have uncommitted changes).
    pub warnings: Vec<String>,
    /// `BLOCKED:` line: the patch does not apply cleanly.
    pub blocked: Option<String>,
    /// `RISK (full)` lines: such a plan is never offered by the harness; shown if one arrives.
    pub risks: Vec<String>,
}

impl PlanView {
    /// Reads `ApplyPlan::to_text` (`apply run 12: <source> -> <target>`, one line per file, the stat,
    /// then `WARNING:` / `BLOCKED:` / `RISK (full):` lines).
    pub fn parse(detail: &str) -> PlanView {
        let mut plan = PlanView::default();
        for line in detail.lines() {
            let t = line.trim_end();
            if let Some(rest) = t.strip_prefix("apply run ") {
                if let Some((_, route)) = rest.split_once(": ")
                    && let Some((s, d)) = route.split_once(" -> ")
                {
                    plan.source = s.trim().to_string();
                    plan.target = d.trim().to_string();
                }
            } else if let Some(w) = t.strip_prefix("WARNING: ") {
                plan.warnings.push(w.to_string());
            } else if let Some(b) = t.strip_prefix("BLOCKED: ") {
                plan.blocked = Some(b.to_string());
            } else if let Some(r) = t.strip_prefix("RISK (full): ") {
                plan.risks.push(r.to_string());
            } else if t.trim() == "nothing to apply" {
                // an empty plan: no files, no stat
            } else if t.starts_with("  ") && !t.trim().is_empty() {
                let mut it = t.split_whitespace();
                match (it.next(), it.next()) {
                    (Some(change), Some(_))
                        if matches!(change, "added" | "modified" | "deleted" | "type_changed") =>
                    {
                        // the path may contain spaces: everything after the change word
                        let path = t.trim_start()[change.len()..].trim();
                        plan.files.push((change.to_string(), path.to_string()));
                    }
                    _ => plan.stat.push(t.trim().to_string()),
                }
            } else if !t.trim().is_empty() {
                plan.stat.push(t.trim().to_string());
            }
        }
        plan
    }
}

/// One request waiting for the user.
pub struct PendingApproval {
    pub request: ApprovalRequest,
    /// `Some` for a real request; `None` for a demo card (nobody is waiting on the pipe).
    incoming: Option<Incoming>,
    /// Present when [`ApprovalRequest::tool`] is [`APPLY_TOOL`].
    pub plan: Option<PlanView>,
    /// Name of the profile the run uses, once known (read from the run's record). «Permitir siempre en
    /// este perfil» is only offered when there is one.
    pub profile: Option<String>,
}

impl PendingApproval {
    pub fn is_apply(&self) -> bool {
        self.request.tool == APPLY_TOOL
    }
}

/// What the queue tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalEvent {
    /// A request arrived (its `request_id`).
    Added(String),
    /// It was answered or it expired.
    Removed(String),
}

#[derive(Default)]
pub struct ApprovalQueue {
    pending: Vec<PendingApproval>,
    _task: Option<Task<()>>,
}

impl EventEmitter<ApprovalEvent> for ApprovalQueue {}

impl ApprovalQueue {
    /// A queue fed by the pipe server's channel (`None` = no server: tests, or the pipe was taken).
    pub fn new(events: Option<UnboundedReceiver<ServerEvent>>, cx: &mut Context<Self>) -> Self {
        let task = events.map(|mut rx| {
            cx.spawn(async move |this, cx| {
                while let Some(ev) = rx.next().await {
                    if this.update(cx, |q, cx| q.on_server_event(ev, cx)).is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            pending: Vec::new(),
            _task: task,
        }
    }

    pub fn pending(&self) -> &[PendingApproval] {
        &self.pending
    }

    fn on_server_event(&mut self, ev: ServerEvent, cx: &mut Context<Self>) {
        match ev {
            ServerEvent::Request(incoming) => {
                self.add(incoming.request.clone(), Some(incoming), cx)
            }
            ServerEvent::Expired(id) => self.remove(&id, cx),
        }
    }

    /// A card without a pipe behind it (demo flags and screenshots): answering only removes it.
    pub fn add_demo(&mut self, request: ApprovalRequest, cx: &mut Context<Self>) {
        self.add(request, None, cx);
    }

    fn add(
        &mut self,
        request: ApprovalRequest,
        incoming: Option<Incoming>,
        cx: &mut Context<Self>,
    ) {
        if self.pending.len() >= MAX_PENDING {
            if let Some(i) = incoming {
                i.answer(
                    Decision::Deny,
                    Some("Selachi has too many approvals waiting; denied".into()),
                );
            }
            return;
        }
        // The same request id twice (a client retry) replaces the old card: the old connection is
        // denied so only one answer is ever pending per id.
        if let Some(pos) = self.pending.iter().position(|p| {
            // A retry is the same request of the same run; ids are only unique per run.
            p.request.request_id == request.request_id && p.request.run_id == request.run_id
        }) {
            let old = self.pending.remove(pos);
            if let Some(i) = old.incoming {
                i.answer(Decision::Deny, Some("superseded by a newer request".into()));
            }
        }
        let id = request.request_id.clone();
        let plan = (request.tool == APPLY_TOOL).then(|| PlanView::parse(&request.detail));
        self.pending.push(PendingApproval {
            request,
            incoming,
            plan,
            profile: None,
        });
        cx.emit(ApprovalEvent::Added(id));
        cx.notify();
    }

    /// The user decided. `Always` on an apply confirmation counts as a single allow (the harness
    /// does the same).
    pub fn resolve(&mut self, id: &str, decision: Decision, cx: &mut Context<Self>) {
        let Some(pos) = self.pending.iter().position(|p| p.request.request_id == id) else {
            return;
        };
        let p = self.pending.remove(pos);
        if let Some(i) = p.incoming {
            let reason =
                (decision == Decision::Deny).then(|| "rechazado por el usuario".to_string());
            i.answer(decision, reason);
        }
        cx.emit(ApprovalEvent::Removed(id.to_string()));
        cx.notify();
    }

    /// The run's profile became known (the shell reads the run record off the UI thread).
    pub fn set_profile(&mut self, id: &str, profile: Option<String>, cx: &mut Context<Self>) {
        if let Some(p) = self.pending.iter_mut().find(|p| p.request.request_id == id)
            && p.profile != profile
        {
            p.profile = profile;
            cx.notify();
        }
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        let before = self.pending.len();
        self.pending.retain(|p| p.request.request_id != id);
        if self.pending.len() != before {
            cx.emit(ApprovalEvent::Removed(id.to_string()));
            cx.notify();
        }
    }
}

/// The attention id of a request.
pub fn attention_id(request_id: &str) -> String {
    format!("approval-{request_id}")
}

/// The focus target of a request's attention item.
pub fn focus_target(request_id: &str) -> String {
    format!("approval:{request_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apply_plan_text_is_read_back() {
        let text = "apply run 12: C:/wt/selachi-codex -> C:/proj\n  added        src/new.rs\n  modified     src/lib.rs\n  deleted      docs/old file.md\n src/new.rs | 10 ++++\n 3 files changed, 12 insertions(+), 2 deletions(-)\nWARNING: these files also have uncommitted changes in the project: src/lib.rs\n";
        let p = PlanView::parse(text);
        assert_eq!(p.source, "C:/wt/selachi-codex");
        assert_eq!(p.target, "C:/proj");
        assert_eq!(
            p.files,
            vec![
                ("added".to_string(), "src/new.rs".to_string()),
                ("modified".to_string(), "src/lib.rs".to_string()),
                ("deleted".to_string(), "docs/old file.md".to_string()),
            ]
        );
        assert_eq!(p.stat.len(), 2, "{:?}", p.stat);
        assert!(p.stat[1].contains("3 files changed"));
        assert_eq!(p.warnings.len(), 1);
        assert!(p.blocked.is_none() && p.risks.is_empty());
    }

    #[test]
    fn blocked_and_risky_plans_are_flagged() {
        let p = PlanView::parse(
            "apply run 1: a -> b\nBLOCKED: the patch does not apply cleanly: x\nRISK (full): deletes .git\n",
        );
        assert_eq!(
            p.blocked.as_deref(),
            Some("the patch does not apply cleanly: x")
        );
        assert_eq!(p.risks, vec!["deletes .git".to_string()]);
        let empty = PlanView::parse("apply run 1: a -> b\n  nothing to apply\n");
        assert!(empty.files.is_empty() && empty.stat.is_empty());
    }

    #[test]
    fn ids_are_stable() {
        assert_eq!(attention_id("r1"), "approval-r1");
        assert_eq!(focus_target("r1"), "approval:r1");
    }
}

#[cfg(test)]
mod queue_tests {
    use std::time::Duration;

    use agent_harness::{ClientMessage, PROTOCOL_VERSION, ServerMessage};
    use gpui::{AppContext as _, TestAppContext};

    use super::*;
    use crate::approvals::{handle_message, wait_for};

    fn req(id: &str, tool: &str) -> ApprovalRequest {
        ApprovalRequest {
            v: PROTOCOL_VERSION,
            request_id: id.into(),
            run_id: "5".into(),
            thread_id: 5,
            parent_thread_id: Some(1),
            agent: "codex".into(),
            tool: tool.into(),
            detail: "rm -rf build".into(),
            permission: "ask".into(),
            depth: 1,
            timeout_secs: 60,
        }
    }

    /// The server thread asks, the queue holds the request, only `resolve` answers it.
    #[gpui::test]
    fn a_request_waits_for_the_user_and_the_decision_reaches_the_agent(cx: &mut TestAppContext) {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let queue = cx.new(|cx| ApprovalQueue::new(Some(rx), cx));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        cx.update(|cx| {
            cx.subscribe(&queue, move |_, ev: &ApprovalEvent, _| {
                seen.borrow_mut().push(ev.clone())
            })
            .detach();
        });
        let asker = std::thread::spawn(move || {
            handle_message(
                ClientMessage::ApprovalRequest(req("r1", "Bash")),
                &tx,
                wait_for,
            )
        });
        for _ in 0..200 {
            cx.run_until_parked();
            if queue.read_with(cx, |q, _| q.pending().len()) == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), 1);
        assert!(!asker.is_finished(), "nothing is approved by itself");
        assert_eq!(*events.borrow(), vec![ApprovalEvent::Added("r1".into())]);

        queue.update(cx, |q, cx| q.resolve("r1", Decision::Always, cx));
        let reply = asker.join().unwrap();
        assert!(matches!(
            reply,
            ServerMessage::ApprovalResponse { decision: Decision::Always, ref request_id, .. } if request_id == "r1"
        ));
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), 0);
        assert_eq!(
            events.borrow().last(),
            Some(&ApprovalEvent::Removed("r1".into()))
        );
    }

    /// Two delegated runs ask at the same moment and both call their request `req_1`. The harness
    /// scopes the wire id by run (`run<id>:req_1`), so both reach the queue, neither is treated as a
    /// retry of the other, and each decision reaches the run that asked.
    #[gpui::test]
    fn two_runs_asking_at_once_are_both_queued_and_answered_independently(cx: &mut TestAppContext) {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let queue = cx.new(|cx| ApprovalQueue::new(Some(rx), cx));
        let ask = |run: i64| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut r = req(&format!("run{run}:req_1"), "Bash");
                r.run_id = run.to_string();
                r.thread_id = run;
                handle_message(ClientMessage::ApprovalRequest(r), &tx, wait_for)
            })
        };
        let (a, b) = (ask(5), ask(6));
        for _ in 0..200 {
            cx.run_until_parked();
            if queue.read_with(cx, |q, _| q.pending().len()) == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), 2);
        assert!(
            !a.is_finished() && !b.is_finished(),
            "nothing is superseded"
        );

        // answer the second one first, and differently
        queue.update(cx, |q, cx| q.resolve("run6:req_1", Decision::Deny, cx));
        let reply_b = b.join().unwrap();
        assert!(matches!(
            reply_b,
            ServerMessage::ApprovalResponse { decision: Decision::Deny, ref request_id, .. } if request_id == "run6:req_1"
        ));
        assert!(
            !a.is_finished(),
            "the other run is still waiting for its own answer"
        );
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), 1);

        queue.update(cx, |q, cx| q.resolve("run5:req_1", Decision::AlwaysRun, cx));
        let reply_a = a.join().unwrap();
        assert!(matches!(
            reply_a,
            ServerMessage::ApprovalResponse { decision: Decision::AlwaysRun, ref request_id, .. } if request_id == "run5:req_1"
        ));
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), 0);
    }

    /// A retry is the same id of the same run; the same id of another run is a different request.
    #[gpui::test]
    fn only_the_same_id_of_the_same_run_replaces_a_card(cx: &mut TestAppContext) {
        let queue = cx.new(|cx| ApprovalQueue::new(None, cx));
        let mut other = req("req_1", "Bash");
        other.run_id = "6".into();
        queue.update(cx, |q, cx| {
            q.add_demo(req("req_1", "Bash"), cx);
            q.add_demo(other, cx);
        });
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), 2);
        queue.update(cx, |q, cx| q.add_demo(req("req_1", "Bash"), cx));
        assert_eq!(
            queue.read_with(cx, |q, _| q.pending().len()),
            2,
            "a retry of run 5 replaced its own card"
        );
    }

    #[gpui::test]
    fn an_expired_request_disappears_and_a_full_queue_denies(cx: &mut TestAppContext) {
        let queue = cx.new(|cx| ApprovalQueue::new(None, cx));
        queue.update(cx, |q, cx| {
            for i in 0..MAX_PENDING {
                q.add_demo(req(&format!("r{i}"), "Bash"), cx);
            }
            q.add_demo(req("overflow", "Bash"), cx);
        });
        assert_eq!(queue.read_with(cx, |q, _| q.pending().len()), MAX_PENDING);
        queue.update(cx, |q, cx| q.remove("r3", cx));
        assert!(queue.read_with(cx, |q, _| {
            q.pending().iter().all(|p| p.request.request_id != "r3")
        }));
    }

    /// The run's profile arrives after the card (it is read off the UI thread); a card without one
    /// stays without «Permitir siempre en este perfil».
    #[gpui::test]
    fn the_profile_of_a_run_is_attached_to_its_card(cx: &mut TestAppContext) {
        let queue = cx.new(|cx| ApprovalQueue::new(None, cx));
        queue.update(cx, |q, cx| q.add_demo(req("r1", "Bash"), cx));
        assert_eq!(
            queue.read_with(cx, |q, _| q.pending()[0].profile.clone()),
            None
        );
        queue.update(cx, |q, cx| {
            q.set_profile("other", Some("X".into()), cx);
            q.set_profile("r1", Some("Tests".into()), cx);
        });
        assert_eq!(
            queue.read_with(cx, |q, _| q.pending()[0].profile.clone()),
            Some("Tests".to_string())
        );
        queue.update(cx, |q, cx| q.set_profile("r1", None, cx));
        assert_eq!(
            queue.read_with(cx, |q, _| q.pending()[0].profile.clone()),
            None
        );
    }

    #[gpui::test]
    fn an_apply_confirmation_carries_its_plan(cx: &mut TestAppContext) {
        let queue = cx.new(|cx| ApprovalQueue::new(None, cx));
        let mut r = req("apply-12-1", APPLY_TOOL);
        r.detail =
            "apply run 12: wt -> proj\n  added        a.rs\n 1 file changed, 3 insertions(+)\n"
                .into();
        queue.update(cx, |q, cx| q.add_demo(r, cx));
        let plan = queue
            .read_with(cx, |q, _| q.pending()[0].plan.clone())
            .unwrap();
        assert_eq!(plan.files, vec![("added".to_string(), "a.rs".to_string())]);
        assert!(queue.read_with(cx, |q, _| q.pending()[0].is_apply()));
    }
}
