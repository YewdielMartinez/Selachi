//! The attention queue (A11): the agents and commands that need you, across every tab and thread.
//!
//! The host feeds an [`AttentionModel`] with [`AttentionItem`]s (`upsert` / `remove`) from the
//! agent events it already receives; the model keeps them in a stable order, counts them for the
//! status bar, and cycles through them for `Ctrl+Shift+A` ([`AttentionModel::next_target`]; the
//! host binds the key). It never polls: every change comes from the host.
//!
//! Order: approvals first (an agent is blocked on a tool), then agents waiting for input, then
//! failed commands, then finished long commands; inside each group, oldest first.

use std::time::Instant;

use agents_driver::{AgentEvent, AgentKind, AgentStatus};
use gpui::{Context, EventEmitter, SharedString};

/// Identity of an item (a tab id, a thread id, "tab-3:approval"...). Upserting an id replaces the
/// item.
pub type AttentionId = SharedString;

/// What the host jumps to when the user picks an item. Opaque to this crate: a tab id, a chat
/// thread id...
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FocusTarget(pub SharedString);

impl FocusTarget {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self(id.into())
    }
}

impl From<&str> for FocusTarget {
    fn from(s: &str) -> Self {
        Self(SharedString::from(s.to_owned()))
    }
}

/// Why something is in the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttentionKind {
    /// The agent finished its turn or asked something and waits for you.
    WaitingInput,
    /// The agent wants to run a tool and needs your approval.
    Approval { tool: String, detail: String },
    /// A command failed.
    Failed { exit_code: i32 },
    /// A long command finished (A4).
    LongCommandDone,
}

impl AttentionKind {
    /// Sort rank: lower comes first.
    pub const fn rank(&self) -> u8 {
        match self {
            AttentionKind::Approval { .. } => 0,
            AttentionKind::WaitingInput => 1,
            AttentionKind::Failed { .. } => 2,
            AttentionKind::LongCommandDone => 3,
        }
    }

    /// Short text for the row ("Espera tu respuesta").
    pub fn text(&self) -> String {
        match self {
            AttentionKind::WaitingInput => "Espera tu respuesta".to_string(),
            AttentionKind::Approval { tool, .. } if tool.is_empty() => "Pide tu aprobación".to_string(),
            AttentionKind::Approval { tool, .. } => format!("Pide aprobación: {tool}"),
            AttentionKind::Failed { exit_code } => format!("Falló (código {exit_code})"),
            AttentionKind::LongCommandDone => "Terminó un comando largo".to_string(),
        }
    }

    /// Second line of the row, when there is one (the approval's detail).
    pub fn detail(&self) -> Option<&str> {
        match self {
            AttentionKind::Approval { detail, .. } if !detail.is_empty() => Some(detail),
            _ => None,
        }
    }

    /// What an agent event means for the queue: an approval request, "waiting for input", or a
    /// non-zero exit. `None` for everything else (the host removes the item on `Working` / `Done`).
    ///
    /// The ACP driver emits `ApprovalRequest` and then `Status(WaitingForInput)` for the same
    /// request: use the same item id for both ([`AttentionModel::upsert`] keeps the approval).
    pub fn from_agent_event(event: &AgentEvent) -> Option<Self> {
        match event {
            AgentEvent::ApprovalRequest { tool, detail, .. } => Some(Self::Approval { tool: tool.clone(), detail: detail.clone() }),
            AgentEvent::Status(AgentStatus::WaitingForInput) => Some(Self::WaitingInput),
            AgentEvent::Exited { code: Some(code) } if *code != 0 => Some(Self::Failed { exit_code: *code }),
            _ => None,
        }
    }
}

/// Name of an agent for the mark (`AgentMark::from_name` understands it).
pub fn agent_label(kind: &AgentKind) -> String {
    match kind {
        AgentKind::ClaudeCode => "claude".to_string(),
        AgentKind::Codex => "codex".to_string(),
        AgentKind::OpenCode => "opencode".to_string(),
        AgentKind::Acp { command } => {
            let c = command.to_lowercase();
            if c.contains("gemini") { "gemini".to_string() } else { "acp".to_string() }
        }
    }
}

/// One thing that needs attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionItem {
    pub id: AttentionId,
    /// Where it is: the tab or thread title ("selachi · claude").
    pub owner_label: SharedString,
    /// Agent name (`"claude"`, `"codex"`...), `None` for a plain command.
    pub agent: Option<String>,
    pub kind: AttentionKind,
    /// When it started waiting (the elapsed time counts from here).
    pub since: Instant,
    pub focus_target: FocusTarget,
}

/// What the model tells the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttentionEvent {
    /// The user picked an item (or pressed the cycle key): bring that tab / thread to the front.
    Jump(FocusTarget),
    /// The user dismissed an item. The model already removed it; the host stops feeding it.
    Dismiss(AttentionId),
    /// The set of items changed (for the status-bar badge). Also available through `cx.observe`.
    Changed { count: usize },
}

/// The queue.
#[derive(Default)]
pub struct AttentionModel {
    items: Vec<AttentionItem>,
    /// Last item jumped to and the index it had: where `next_target` continues from.
    cursor: Option<(AttentionId, usize)>,
}

impl EventEmitter<AttentionEvent> for AttentionModel {}

impl AttentionModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the item with the same id. Replacing keeps `since` when the kind did not
    /// change (a repeated event must not reset the clock), and an `Approval` is not downgraded to
    /// `WaitingInput` (the ACP driver emits both for one request).
    pub fn upsert(&mut self, mut item: AttentionItem, cx: &mut Context<Self>) {
        if let Some(old) = self.items.iter_mut().find(|i| i.id == item.id) {
            if matches!(old.kind, AttentionKind::Approval { .. }) && item.kind == AttentionKind::WaitingInput {
                item.kind = old.kind.clone();
            }
            if old.kind == item.kind {
                item.since = old.since;
            }
            if *old == item {
                return;
            }
            *old = item;
        } else {
            self.items.push(item);
        }
        self.changed(cx);
    }

    /// Removes an item. Returns whether it existed.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.id.as_ref() != id);
        let removed = self.items.len() != before;
        if removed {
            self.changed(cx);
        }
        removed
    }

    /// Removes every item whose focus target is `target` (the tab was closed).
    pub fn remove_target(&mut self, target: &FocusTarget, cx: &mut Context<Self>) {
        let before = self.items.len();
        self.items.retain(|i| &i.focus_target != target);
        if self.items.len() != before {
            self.changed(cx);
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.items.is_empty() {
            self.items.clear();
            self.cursor = None;
            self.changed(cx);
        }
    }

    /// Items waiting for the user (for the status bar).
    pub fn count(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The items in queue order.
    pub fn items(&self) -> Vec<&AttentionItem> {
        let mut v: Vec<&AttentionItem> = self.items.iter().collect();
        v.sort_by(|a, b| (a.kind.rank(), a.since).cmp(&(b.kind.rank(), b.since)));
        v
    }

    pub fn get(&self, id: &str) -> Option<&AttentionItem> {
        self.items.iter().find(|i| i.id.as_ref() == id)
    }

    /// Where `Ctrl+Shift+A` goes next, cycling through the queue; `None` when it is empty. It
    /// continues after the last item jumped to, even if the host removed that item meanwhile (it
    /// then takes the item that moved into its place).
    pub fn next_target(&mut self) -> Option<FocusTarget> {
        let order: Vec<(AttentionId, FocusTarget)> = self.items().into_iter().map(|i| (i.id.clone(), i.focus_target.clone())).collect();
        if order.is_empty() {
            self.cursor = None;
            return None;
        }
        let ix = match &self.cursor {
            Some((id, _)) if order.iter().any(|(i, _)| i == id) => {
                let at = order.iter().position(|(i, _)| i == id).unwrap_or(0);
                (at + 1) % order.len()
            }
            Some((_, last)) => *last % order.len(),
            None => 0,
        };
        let (id, target) = order[ix].clone();
        self.cursor = Some((id, ix));
        Some(target)
    }

    /// [`AttentionModel::next_target`] plus emitting [`AttentionEvent::Jump`]: what the key does.
    pub fn jump_next(&mut self, cx: &mut Context<Self>) -> Option<FocusTarget> {
        let t = self.next_target();
        if let Some(t) = &t {
            cx.emit(AttentionEvent::Jump(t.clone()));
        }
        t
    }

    /// The user picked item `id`: emits `Jump` and remembers it for cycling.
    pub fn jump(&mut self, id: &str, cx: &mut Context<Self>) {
        let order = self.items();
        let Some(ix) = order.iter().position(|i| i.id.as_ref() == id) else { return };
        let (item_id, target) = (order[ix].id.clone(), order[ix].focus_target.clone());
        self.cursor = Some((item_id, ix));
        cx.emit(AttentionEvent::Jump(target));
    }

    /// The user dismissed item `id`: removes it and emits `Dismiss`.
    pub fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(item) = self.get(id) else { return };
        let id = item.id.clone();
        self.remove(&id, cx);
        cx.emit(AttentionEvent::Dismiss(id));
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(AttentionEvent::Changed { count: self.items.len() });
        cx.notify();
    }
}

/// Elapsed time for a row: `42 s`, `3 min`, `1 h 05`, `2 d`.
pub fn format_wait(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3_599 => format!("{} min", secs / 60),
        3_600..=86_399 => format!("{} h {:02}", secs / 3_600, (secs % 3_600) / 60),
        _ => format!("{} d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use std::time::Duration;

    fn item(id: &str, kind: AttentionKind, age_s: u64) -> AttentionItem {
        AttentionItem {
            id: id.to_string().into(),
            owner_label: format!("tab {id}").into(),
            agent: Some("claude".into()),
            kind,
            since: Instant::now() - Duration::from_secs(age_s),
            focus_target: FocusTarget::new(format!("target-{id}")),
        }
    }

    fn approval() -> AttentionKind {
        AttentionKind::Approval { tool: "Bash".into(), detail: "rm -rf build".into() }
    }

    fn ids(m: &AttentionModel) -> Vec<String> {
        m.items().iter().map(|i| i.id.to_string()).collect()
    }

    #[gpui::test]
    fn ordering_is_kind_then_oldest_first(cx: &mut TestAppContext) {
        let m = cx.new(|_| AttentionModel::new());
        m.update(cx, |m, cx| {
            m.upsert(item("done", AttentionKind::LongCommandDone, 500), cx);
            m.upsert(item("wait-new", AttentionKind::WaitingInput, 10), cx);
            m.upsert(item("fail", AttentionKind::Failed { exit_code: 2 }, 5), cx);
            m.upsert(item("wait-old", AttentionKind::WaitingInput, 300), cx);
            m.upsert(item("appr", approval(), 1), cx);
        });
        m.read_with(cx, |m, _| {
            assert_eq!(ids(m), ["appr", "wait-old", "wait-new", "fail", "done"]);
            assert_eq!(m.count(), 5);
        });
    }

    #[gpui::test]
    fn upsert_replaces_and_keeps_the_clock(cx: &mut TestAppContext) {
        let m = cx.new(|_| AttentionModel::new());
        m.update(cx, |m, cx| {
            let first = item("a", AttentionKind::WaitingInput, 120);
            let since = first.since;
            m.upsert(first, cx);
            // Same kind again, fresh timestamp: the clock keeps running from the first one.
            m.upsert(item("a", AttentionKind::WaitingInput, 0), cx);
            assert_eq!(m.count(), 1);
            assert_eq!(m.get("a").map(|i| i.since), Some(since));
            // A different kind restarts it.
            m.upsert(item("a", AttentionKind::Failed { exit_code: 1 }, 0), cx);
            assert_ne!(m.get("a").map(|i| i.since), Some(since));
        });
    }

    #[gpui::test]
    fn an_approval_is_not_downgraded_by_waiting_input(cx: &mut TestAppContext) {
        let m = cx.new(|_| AttentionModel::new());
        m.update(cx, |m, cx| {
            m.upsert(item("a", approval(), 3), cx);
            m.upsert(item("a", AttentionKind::WaitingInput, 0), cx);
            assert!(matches!(m.get("a").map(|i| &i.kind), Some(AttentionKind::Approval { .. })));
            assert_eq!(m.count(), 1);
        });
    }

    #[gpui::test]
    fn cycling_wraps_and_survives_removals(cx: &mut TestAppContext) {
        let m = cx.new(|_| AttentionModel::new());
        m.update(cx, |m, cx| {
            assert_eq!(m.next_target(), None, "empty queue");
            m.upsert(item("a", AttentionKind::WaitingInput, 30), cx);
            m.upsert(item("b", AttentionKind::WaitingInput, 20), cx);
            m.upsert(item("c", AttentionKind::WaitingInput, 10), cx);
            let t = |m: &mut AttentionModel| m.next_target().map(|t| t.0.to_string());
            assert_eq!(t(m).as_deref(), Some("target-a"));
            assert_eq!(t(m).as_deref(), Some("target-b"));
            assert_eq!(t(m).as_deref(), Some("target-c"));
            assert_eq!(t(m).as_deref(), Some("target-a"), "wraps");
            // The host resolves `a` after jumping to it: the next press goes to `b`, not back to `a`.
            m.remove("a", cx);
            assert_eq!(t(m).as_deref(), Some("target-b"));
            // Removing the item we are on (b, now at index 0): the next one slides into its place.
            m.remove("b", cx);
            assert_eq!(t(m).as_deref(), Some("target-c"));
            m.remove("c", cx);
            assert_eq!(t(m), None);
        });
    }

    #[gpui::test]
    fn jump_and_dismiss_emit_events(cx: &mut TestAppContext) {
        let m = cx.new(|_| AttentionModel::new());
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = events.clone();
        let _sub = cx.update(|cx| cx.subscribe(&m, move |_, e: &AttentionEvent, _| log.borrow_mut().push(e.clone())));
        m.update(cx, |m, cx| {
            m.upsert(item("a", AttentionKind::WaitingInput, 5), cx);
            m.upsert(item("b", approval(), 5), cx);
            m.jump("a", cx);
            m.dismiss("b", cx);
            m.dismiss("zzz", cx); // unknown: nothing
            let t = m.jump_next(cx);
            assert_eq!(t, Some(FocusTarget::from("target-a")));
        });
        let log = events.borrow();
        assert_eq!(log[0], AttentionEvent::Changed { count: 1 });
        assert_eq!(log[1], AttentionEvent::Changed { count: 2 });
        assert!(log.contains(&AttentionEvent::Jump(FocusTarget::from("target-a"))));
        assert!(log.contains(&AttentionEvent::Dismiss("b".into())));
        assert!(!log.iter().any(|e| matches!(e, AttentionEvent::Dismiss(id) if id.as_ref() == "zzz")));
        m.read_with(cx, |m, _| assert_eq!(m.count(), 1));
    }

    #[gpui::test]
    fn removing_a_target_drops_all_its_items(cx: &mut TestAppContext) {
        let m = cx.new(|_| AttentionModel::new());
        m.update(cx, |m, cx| {
            let mut a = item("a", AttentionKind::WaitingInput, 5);
            let mut b = item("b", approval(), 5);
            a.focus_target = FocusTarget::from("tab-1");
            b.focus_target = FocusTarget::from("tab-1");
            m.upsert(a, cx);
            m.upsert(b, cx);
            m.upsert(item("c", AttentionKind::LongCommandDone, 5), cx);
            m.remove_target(&FocusTarget::from("tab-1"), cx);
            assert_eq!(m.count(), 1);
            m.clear(cx);
            assert!(m.is_empty());
        });
    }

    #[test]
    fn agent_events_map_to_kinds() {
        let req = AgentEvent::ApprovalRequest { id: "1".into(), tool: "Bash".into(), detail: "ls".into() };
        assert_eq!(AttentionKind::from_agent_event(&req), Some(AttentionKind::Approval { tool: "Bash".into(), detail: "ls".into() }));
        assert_eq!(AttentionKind::from_agent_event(&AgentEvent::Status(AgentStatus::WaitingForInput)), Some(AttentionKind::WaitingInput));
        assert_eq!(AttentionKind::from_agent_event(&AgentEvent::Exited { code: Some(3) }), Some(AttentionKind::Failed { exit_code: 3 }));
        assert_eq!(AttentionKind::from_agent_event(&AgentEvent::Exited { code: Some(0) }), None);
        assert_eq!(AttentionKind::from_agent_event(&AgentEvent::Exited { code: None }), None);
        assert_eq!(AttentionKind::from_agent_event(&AgentEvent::Status(AgentStatus::Working)), None);
        assert_eq!(AttentionKind::from_agent_event(&AgentEvent::Status(AgentStatus::Done)), None);
    }

    #[test]
    fn labels_and_times() {
        assert_eq!(agent_label(&AgentKind::ClaudeCode), "claude");
        assert_eq!(agent_label(&AgentKind::Acp { command: "C:/x/Gemini.exe --acp".into() }), "gemini");
        assert_eq!(format_wait(5), "5 s");
        assert_eq!(format_wait(60), "1 min");
        assert_eq!(format_wait(3_900), "1 h 05");
        assert_eq!(format_wait(2 * 86_400), "2 d");
        assert_eq!(AttentionKind::Failed { exit_code: 2 }.text(), "Falló (código 2)");
        assert_eq!(approval().detail(), Some("rm -rf build"));
        assert_eq!(AttentionKind::WaitingInput.detail(), None);
    }
}
