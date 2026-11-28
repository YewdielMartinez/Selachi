//! Delegation inside the shell (wave 6a.4, PLAN section 20): the approvals of delegated agents, the
//! run registry that feeds the lineage rows, and the parent -> child nesting of the sidebar.
//!
//! Everything here is driven by events: the approvals pipe (a blocked thread + a channel), the OS
//! watcher of `agent-runs` (another process writes there) and the hub. No timers, no polling.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agent_harness::{Decision, Registry, RunRecord, RunStatus};
use chat_core::ThreadId;
use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::*, px,
};
use panel_api::{RowStatus, TabId};
use ui_chat::{ChatTab, DelegationRun, RunState};

use crate::terminal_tab::TerminalTab;
use ui_review::{AttentionItem, AttentionKind, FocusTarget};

use super::AppShell;
use crate::approval_queue::{self, ApprovalEvent};
use crate::approvals_ui::{self, CardCtx, DecideFn, OpenThreadFn};
use crate::notify;
use crate::services::Services;
use crate::sidebar_view::{ChildRow, Lineage};

/// The registry is reread this long after the last change (a run writes a few files in a burst).
pub const REGISTRY_DEBOUNCE: Duration = Duration::from_millis(250);
/// How long after «Permitir siempre en este perfil» the profile library is reread (the harness
/// writes the file right after it receives the answer).
const PROFILE_RELOAD_DELAY: Duration = Duration::from_millis(800);
/// A run that says "running" this long after its deadline has lost its process.
const STALE_RUN_GRACE_MS: i64 = 60_000;
/// Deepest delegation level listed in the sidebar.
const MAX_LINEAGE_DEPTH: usize = 2;

/// The data folder the harness uses (`SELACHI_DATA_DIR` overrides `%APPDATA%\Selachi`).
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var_os("SELACHI_DATA_DIR") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d)),
        _ => store::paths::app_data_dir_path().ok(),
    }
}

/// A registry record as the chat hub wants it.
pub fn run_of_record(rec: &RunRecord, now_ms: i64) -> Option<DelegationRun> {
    let thread = rec.run_id.parse::<i64>().ok()?;
    let stale = rec.status == RunStatus::Running
        && rec.finished_ms.is_none()
        && now_ms > rec.deadline_ms + STALE_RUN_GRACE_MS;
    let state = match rec.status {
        RunStatus::Running if stale => RunState::Failed,
        RunStatus::Running => RunState::Running,
        RunStatus::Done => RunState::Done,
        RunStatus::Cancelled => RunState::Cancelled,
        _ => RunState::Failed,
    };
    let (files, tokens, cost, summary) =
        rec.result.as_ref().map_or((0, 0, 0.0, String::new()), |r| {
            (
                r.files_changed.len(),
                r.usage.total,
                r.cost.usd,
                r.summary.chars().take(400).collect::<String>(),
            )
        });
    Some(DelegationRun {
        thread: ThreadId(thread),
        state,
        files_changed: files,
        started_ms: rec.started_ms,
        finished_ms: rec.finished_ms,
        cost_usd: cost,
        tokens,
        summary,
    })
}

/// Status glyph of a delegated child.
fn child_status(rec: &chat_core::ThreadRecord, run: Option<&DelegationRun>) -> Option<RowStatus> {
    match run.map(|r| r.state) {
        Some(RunState::Running) => Some(RowStatus::Working),
        Some(RunState::Done) => Some(RowStatus::Done),
        Some(RunState::Failed) => Some(RowStatus::Error),
        Some(RunState::Cancelled) => None,
        None => match rec.status {
            chat_core::ThreadStatus::Working => Some(RowStatus::Working),
            chat_core::ThreadStatus::WaitingForInput => Some(RowStatus::Waiting),
            chat_core::ThreadStatus::Error => Some(RowStatus::Error),
            chat_core::ThreadStatus::Done => Some(RowStatus::Done),
            chat_core::ThreadStatus::Idle => None,
        },
    }
}

impl AppShell {
    // ------------------------------------------------------------------------------------
    // Start-up
    // ------------------------------------------------------------------------------------

    /// Subscribes to the approvals queue and starts watching the run registry. Called once, after
    /// the first tab exists.
    pub(super) fn init_delegation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let queue = self.deleg.approvals.clone();
        self._subs.push(
            cx.subscribe_in(&queue, window, |this, _, ev: &ApprovalEvent, _, cx| {
                this.on_approval_event(ev, cx)
            }),
        );
        let Some(dir) = data_dir().map(|d| d.join("agent-runs")) else {
            return;
        };
        match store::DirWatcher::start(&dir, REGISTRY_DEBOUNCE) {
            Ok((watcher, mut rx)) => {
                self.deleg._watcher = Some(watcher);
                self.deleg.runs_task = Some(cx.spawn(async move |this, cx| {
                    use futures::StreamExt as _;
                    while rx.next().await.is_some() {
                        if this.update(cx, |shell, cx| shell.reload_runs(cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(e) => log::warn!("cannot watch the delegation run registry: {e}"),
        }
    }

    /// Rereads the run registry off the UI thread, hands it to the hub and refreshes the thread
    /// list (a delegated child is a new thread written by another process).
    pub(super) fn reload_runs(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self.chat.read(cx).hub() else {
            return;
        };
        let Some(dir) = data_dir() else { return };
        let job = cx.background_executor().spawn(async move {
            let now = chat_core::now_ms();
            Registry::new(&dir)
                .list()
                .iter()
                .filter_map(|r| run_of_record(r, now))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let runs = job.await;
            this.update(cx, |shell, cx| {
                hub.update(cx, |h, cx| {
                    h.set_delegation_runs(runs, cx);
                    h.refresh(cx);
                });
                shell.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------------------------------
    // Approvals
    // ------------------------------------------------------------------------------------

    fn on_approval_event(&mut self, ev: &ApprovalEvent, cx: &mut Context<Self>) {
        let Some(services) = cx.try_global::<Services>().cloned() else {
            return;
        };
        match ev {
            ApprovalEvent::Added(id) => {
                let Some((request, plan)) = self
                    .deleg
                    .approvals
                    .read(cx)
                    .pending()
                    .iter()
                    .find(|p| &p.request.request_id == id)
                    .map(|p| (p.request.clone(), p.plan.clone()))
                else {
                    return;
                };
                // The child is a new thread: show it in the sidebar and the chat list.
                self.reload_runs(cx);
                if plan.is_none() {
                    self.read_run_profile(id, &request.run_id, cx);
                }
                let title = self.child_title(request.thread_id, cx);
                let agent = approvals_ui::display_agent(&request.agent);
                // What the agent wants to run may carry secrets: through the redactor like the rest.
                let (detail, apply) = match &plan {
                    Some(p) => (
                        format!(
                            "{} archivo{} · {}",
                            p.files.len(),
                            if p.files.len() == 1 { "" } else { "s" },
                            p.target
                        ),
                        true,
                    ),
                    None => (term_core::redact(&request.detail).0, false),
                };
                let tool = if apply {
                    "aplicar cambios".to_string()
                } else {
                    term_core::redact(&request.tool).0
                };
                services.attention.update(cx, |m, cx| {
                    m.upsert(
                        AttentionItem {
                            id: approval_queue::attention_id(id).into(),
                            owner_label: format!("{title} · {agent}").into(),
                            agent: Some(request.agent.to_ascii_lowercase()),
                            kind: AttentionKind::Approval {
                                tool,
                                detail: detail.clone(),
                            },
                            since: Instant::now(),
                            focus_target: FocusTarget::new(approval_queue::focus_target(id)),
                        },
                        cx,
                    )
                });
                if !notify::window_active() {
                    notify::attention_toast(
                        APPROVAL_TOAST,
                        &approvals_ui::card_title(&request.agent, apply),
                        &title,
                        &format!("{} {}", request.tool, detail),
                    );
                }
                self.refresh(cx);
            }
            ApprovalEvent::Removed(id) => {
                services
                    .attention
                    .update(cx, |m, cx| m.remove(&approval_queue::attention_id(id), cx));
                self.refresh(cx);
            }
        }
    }

    /// The user pressed a button of a card.
    pub(super) fn decide_approval(&mut self, id: &str, decision: Decision, cx: &mut Context<Self>) {
        self.deleg
            .approvals
            .update(cx, |q, cx| q.resolve(id, decision, cx));
        if decision == Decision::AlwaysProfile {
            self.reload_profile_library_soon(cx);
        }
    }

    /// Reads the profile of a run off the UI thread (one small JSON file, once per request) so the card
    /// can offer «Permitir siempre en este perfil» only when there is a profile to save into.
    fn read_run_profile(&mut self, request_id: &str, run_id: &str, cx: &mut Context<Self>) {
        let Some(dir) = data_dir() else { return };
        if !Registry::valid_id(run_id) {
            return;
        }
        let (request_id, run_id) = (request_id.to_string(), run_id.to_string());
        let job = cx.background_executor().spawn(async move {
            Registry::new(&dir)
                .load(&run_id)
                .ok()
                .and_then(|r| r.profile)
                .filter(|p| !p.trim().is_empty())
        });
        cx.spawn(async move |this, cx| {
            let profile = job.await;
            this.update(cx, |shell, cx| {
                shell
                    .deleg
                    .approvals
                    .update(cx, |q, cx| q.set_profile(&request_id, profile, cx));
            })
            .ok();
        })
        .detach();
    }

    /// After «Permitir siempre en este perfil» the harness rewrites the profile file a moment later;
    /// if the settings tab is open its profile library is reread once, then (a one-shot timer started
    /// by the user's click: nothing polls).
    fn reload_profile_library_soon(&mut self, cx: &mut Context<Self>) {
        if self.settings_view().is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROFILE_RELOAD_DELAY).await;
            this.update(cx, |shell, cx| {
                if let Some(v) = shell.settings_view() {
                    v.update(cx, |s, cx| s.reload_profiles(cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn child_title(&self, thread: i64, cx: &App) -> String {
        self.chat
            .read(cx)
            .hub()
            .and_then(|h| {
                h.read(cx)
                    .thread(ThreadId(thread))
                    .map(|t| t.display_title().to_string())
            })
            .unwrap_or_else(|| format!("Delegación {thread}"))
    }

    /// The stack of approval cards, bottom-right above the status bar.
    pub(super) fn approval_overlay(
        &self,
        bottom: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let queue = self.deleg.approvals.read(cx);
        if queue.pending().is_empty() {
            return None;
        }
        let theme = self.theme;
        let shell = cx.entity();
        let decide: DecideFn = {
            let shell = shell.clone();
            Rc::new(move |id, d, _, cx| shell.update(cx, |s, cx| s.decide_approval(&id, d, cx)))
        };
        let open: OpenThreadFn = {
            Rc::new(move |thread, window, cx| {
                shell.update(cx, |s, cx| s.open_chat_thread(ThreadId(thread), window, cx))
            })
        };
        let titles: HashMap<i64, String> = queue
            .pending()
            .iter()
            .map(|p| {
                (
                    p.request.thread_id,
                    self.child_title(p.request.thread_id, cx),
                )
            })
            .collect();
        let stack = approvals_ui::approval_stack(
            queue.pending(),
            |p| CardCtx {
                theme,
                thread_title: titles.get(&p.request.thread_id).cloned(),
                on_decide: decide.clone(),
                on_open_thread: open.clone(),
            },
            &theme,
        )?;
        Some(
            div()
                .id("approval-stack")
                .absolute()
                .right(px(16.0))
                .bottom(px(bottom))
                .occlude()
                .child(stack)
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------------------------
    // Lineage
    // ------------------------------------------------------------------------------------

    /// The menu of a delegated row (right click): open it, see it on the map, cancel it, archive the
    /// finished siblings.
    pub(super) fn open_child_menu(
        &mut self,
        thread: i64,
        parent: TabId,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lineage = self.compute_lineage(cx);
        let rows = lineage.children.get(&parent).map(Vec::as_slice).unwrap_or(&[]);
        let running = rows.iter().any(|r| r.thread == thread && r.running);
        let finished: Vec<i64> = rows.iter().filter(|r| r.finished).map(|r| r.thread).collect();
        let items = super::projects::child_items(thread, running, finished);
        self.open_menu(items, Some(at), window, cx);
    }

    /// «Archivar terminados»: the finished delegations leave the lists (the threads are kept, archived).
    pub(super) fn archive_threads(&mut self, ids: &[i64], cx: &mut Context<Self>) {
        let Some(hub) = self.chat.read(cx).hub() else {
            return;
        };
        hub.update(cx, |h, cx| {
            for id in ids {
                h.archive_thread(ThreadId(*id), cx);
            }
        });
        self.refresh(cx);
    }

    /// Delegated threads nested under the open chat tab that delegated them.
    pub(super) fn compute_lineage(&self, cx: &App) -> Lineage {
        let Some(hub) = self.chat.read(cx).hub() else {
            return Lineage {
                mapped: self.tabs_with_external_agents(cx),
                ..Lineage::default()
            };
        };
        let hub = hub.read(cx);
        let mut tab_of: HashMap<ThreadId, TabId> = HashMap::new();
        for slot in &self.tabs {
            if let Some(tab) = slot.handle.downcast::<ChatTab>()
                && let Some(thread) = tab.read(cx).thread_id()
            {
                tab_of.insert(thread, slot.handle.id());
            }
        }
        let now = chat_core::now_ms();
        let mut out = Lineage::default();
        for (&thread, &tab) in &tab_of {
            let mut rows = Vec::new();
            let mut seen = HashSet::from([thread]);
            collect_children(hub, thread, 0, &tab_of, &mut seen, now, &mut rows);
            if rows.is_empty() {
                continue;
            }
            out.nested.extend(rows.iter().filter_map(|r| r.tab));
            out.children.insert(tab, rows);
        }
        // Terminal tabs that delegated (`selachi-agent run` started in their shell carries the tab's
        // id in `SELACHI_PARENT_TAB`): their delegations nest under the terminal row too, and the row
        // gets the map icon.
        for slot in &self.tabs {
            if slot.handle.downcast::<TerminalTab>().is_none() {
                continue;
            }
            let tab = slot.handle.id();
            let key = tab.as_u64().to_string();
            let mut rows = Vec::new();
            let mut seen = HashSet::new();
            for child in terminal_children(hub.threads(), &key) {
                if seen.insert(child.id) {
                    push_child(hub, child, 0, &tab_of, &mut seen, now, &mut rows);
                }
            }
            if rows.is_empty() {
                continue;
            }
            out.nested.extend(rows.iter().filter_map(|r| r.tab));
            out.children.insert(tab, rows);
        }
        // Sessions that only launched agents outside Selachi (seen by the map while it was open):
        // no rows to nest, but the map icon.
        out.mapped = self.tabs_with_external_agents(cx);
        out
    }
}

/// Toast "tab" id of an approval (the click only brings the window forward).
pub const APPROVAL_TOAST: u64 = u64::MAX;

/// The delegations started from the terminal tab `tab` (its id as the harness recorded it in
/// `parent_tab`), oldest first. A thread that also names a parent thread belongs to that thread.
pub(crate) fn terminal_children<'a>(
    threads: &'a [chat_core::ThreadRecord],
    tab: &str,
) -> Vec<&'a chat_core::ThreadRecord> {
    let mut out: Vec<&chat_core::ThreadRecord> = threads
        .iter()
        .filter(|t| t.link.parent_thread_id.is_none() && t.link.parent_tab.as_deref() == Some(tab))
        .collect();
    out.sort_by_key(|t| (t.created_ms, t.id.0));
    out
}

fn collect_children(
    hub: &ui_chat::ChatHub,
    parent: ThreadId,
    depth: usize,
    tab_of: &HashMap<ThreadId, TabId>,
    seen: &mut HashSet<ThreadId>,
    now: i64,
    out: &mut Vec<ChildRow>,
) {
    for child in hub.children_of(parent) {
        if !seen.insert(child.id) {
            continue;
        }
        push_child(hub, child, depth, tab_of, seen, now, out);
    }
}

/// One delegated thread as a row, then its own delegations.
fn push_child(
    hub: &ui_chat::ChatHub,
    child: &chat_core::ThreadRecord,
    depth: usize,
    tab_of: &HashMap<ThreadId, TabId>,
    seen: &mut HashSet<ThreadId>,
    now: i64,
    out: &mut Vec<ChildRow>,
) {
    let run = hub.delegation_run(child.id);
    let mut subtitle = ui_chat::run_state_label(child, run).to_string();
    if let Some(r) = run {
        if r.state != RunState::Running {
            subtitle.push_str(" · ");
            subtitle.push_str(&ui_chat::files_label(r.files_changed));
        }
        subtitle.push_str(" · ");
        subtitle.push_str(&ui_chat::format_duration_ms(r.duration_ms(now)));
    }
    let status = child_status(child, run);
    let running = matches!(status, Some(RowStatus::Working | RowStatus::Waiting));
    let finished = !running
        && match run.map(|r| r.state) {
            Some(state) => state != RunState::Running,
            None => matches!(
                child.status,
                chat_core::ThreadStatus::Done | chat_core::ThreadStatus::Error
            ),
        };
    out.push(ChildRow {
        thread: child.id.0,
        tab: tab_of.get(&child.id).copied(),
        title: SharedString::from(child.display_title().to_string()),
        agent: chat_core::agent_label(&child.agent),
        status,
        subtitle: SharedString::from(subtitle),
        depth,
        running,
        finished,
    });
    if depth < MAX_LINEAGE_DEPTH {
        collect_children(hub, child.id, depth + 1, tab_of, seen, now, out);
    }
}

#[cfg(test)]
mod tests {
    use agent_harness::{RunRecord, RunResult};

    use super::*;

    fn record(status: RunStatus, finished: Option<i64>) -> RunRecord {
        let mut result = RunResult::empty(status);
        result.files_changed = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        result.summary = "hecho".into();
        RunRecord {
            run_id: "7".into(),
            thread_id: 7,
            agent: "codex".into(),
            model: None,
            permission: "read-only".into(),
            cwd: "C:/p".into(),
            project_root: "C:/p".into(),
            worktree_path: None,
            checkpoint_id: None,
            prompt: "p".into(),
            parent_thread_id: Some(1),
            depth: 1,
            started_ms: 1_000,
            finished_ms: finished,
            status,
            runner_pid: 1,
            runner_start_ms: None,
            deadline_ms: 1_000 + 900_000,
            result: Some(result),
            applied_ms: None,
            profile: None,
            final_prompt: None,
            limits: None,
            launcher: None,
            effort: None,
            output_format: None,
            context_files: Vec::new(),
        }
    }

    fn thread(
        id: i64,
        parent_thread: Option<i64>,
        parent_tab: Option<&str>,
    ) -> chat_core::ThreadRecord {
        chat_core::ThreadRecord {
            id: ThreadId(id),
            title: format!("hilo {id}"),
            project_root: "C:/p".into(),
            worktree_path: None,
            agent: agents_driver::AgentKind::Codex,
            model: None,
            effort: None,
            permission: agents_driver::PermissionMode::ReadOnly,
            status: chat_core::ThreadStatus::Done,
            created_ms: 1_000 * id,
            updated_ms: 1_000 * id,
            agent_session_id: None,
            archived: false,
            checkpoint_before_turn: false,
            link: chat_core::ThreadLink {
                parent_thread_id: parent_thread.map(ThreadId),
                parent_tab: parent_tab.map(str::to_string),
                depth: 1,
                delegated_by: Some("cli".into()),
            },
        }
    }

    /// 8B/8C: a `selachi-agent run` started in a terminal carries the tab's id; those threads nest
    /// under that terminal row (which gets the map icon), oldest first.
    #[test]
    fn delegations_started_from_a_terminal_belong_to_its_tab() {
        let threads = vec![
            thread(3, None, Some("77")),
            thread(1, None, Some("77")),
            thread(2, None, Some("88")),
            // names a parent thread too: it belongs to that thread, not to the tab
            thread(4, Some(1), Some("77")),
            // not a delegation from a terminal
            thread(5, None, None),
        ];
        let ids: Vec<i64> = terminal_children(&threads, "77")
            .iter()
            .map(|t| t.id.0)
            .collect();
        assert_eq!(ids, [1, 3]);
        assert_eq!(terminal_children(&threads, "88").len(), 1);
        assert!(terminal_children(&threads, "99").is_empty());
        // ids are the tab's entity id as text, never a prefix match
        assert!(terminal_children(&threads, "7").is_empty());
    }

    #[test]
    fn registry_records_become_delegation_runs() {
        let done = run_of_record(&record(RunStatus::Done, Some(121_000)), 200_000).unwrap();
        assert_eq!(done.thread, ThreadId(7));
        assert_eq!(done.state, RunState::Done);
        assert_eq!(done.files_changed, 3);
        assert_eq!(done.duration_ms(0), 120_000);
        let running = run_of_record(&record(RunStatus::Running, None), 5_000).unwrap();
        assert_eq!(running.state, RunState::Running);
        for (status, want) in [
            (RunStatus::Error, RunState::Failed),
            (RunStatus::TimedOut, RunState::Failed),
            (RunStatus::BudgetExceeded, RunState::Failed),
            (RunStatus::Cancelled, RunState::Cancelled),
        ] {
            assert_eq!(
                run_of_record(&record(status, Some(2_000)), 5_000)
                    .unwrap()
                    .state,
                want
            );
        }
    }

    #[test]
    fn a_run_stuck_past_its_deadline_is_not_shown_as_running() {
        let late = 1_000 + 900_000 + STALE_RUN_GRACE_MS + 1;
        let r = run_of_record(&record(RunStatus::Running, None), late).unwrap();
        assert_eq!(r.state, RunState::Failed);
    }

    /// What Settings writes into `[delegation]` is exactly what `selachi-agent` reads back.
    #[test]
    fn the_harness_reads_what_the_settings_write() {
        let dir = std::env::temp_dir().join(format!("selachi-deleg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = store::Config::default();
        cfg.delegation.enabled = false;
        cfg.delegation.allowed_agents = vec!["codex".into(), "agy".into()];
        cfg.delegation.max_permission = "read_only".into();
        cfg.delegation.max_concurrent = 2;
        cfg.delegation.max_depth = 2;
        cfg.delegation.timeout_secs = 450;
        cfg.delegation.daily_token_budget = 123_456;
        cfg.delegation.weekly_usage_threshold_percent = 75.0;
        cfg.delegation.approval_timeout_secs = 45;
        cfg.delegation.extra_agents = vec!["aider".into(), "acp:goose --acp".into()];
        cfg.save_to(&store::paths::config_path_in(&dir)).unwrap();
        let (h, warning) = agent_harness::DelegationConfig::load(&dir);
        assert!(warning.is_none(), "{warning:?}");
        assert!(!h.enabled);
        assert_eq!(h.allowed_agents, ["codex", "agy"]);
        assert_eq!(h.max_permission, agent_harness::Permission::ReadOnly);
        assert_eq!((h.max_concurrent, h.max_depth, h.timeout_secs), (2, 2, 450));
        assert_eq!(h.daily_token_budget, 123_456);
        assert_eq!(h.weekly_usage_threshold_percent, 75.0);
        assert_eq!(h.approval_timeout_secs, 45);
        assert_eq!(h.extra_agents, ["aider", "acp:goose --acp"]);
        // and the defaults of both sides agree
        let d = agent_harness::DelegationConfig::default();
        let s = store::Delegation::default();
        assert_eq!(
            (
                d.enabled,
                d.max_concurrent as u32,
                d.max_depth,
                d.timeout_secs,
                d.daily_token_budget,
                d.approval_timeout_secs
            ),
            (
                s.enabled,
                s.max_concurrent,
                s.max_depth,
                s.timeout_secs,
                s.daily_token_budget,
                s.approval_timeout_secs
            )
        );
        assert_eq!(
            d.weekly_usage_threshold_percent,
            s.weekly_usage_threshold_percent
        );
        assert_eq!(d.extra_agents, s.extra_agents);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_run_id_that_is_not_a_thread_is_ignored() {
        let mut rec = record(RunStatus::Done, Some(2));
        rec.run_id = "abc".into();
        assert!(run_of_record(&rec, 0).is_none());
    }
}
