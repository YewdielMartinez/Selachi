//! "Que toco el agente": the files an agent session wrote (from the JSONL `Edit` / `Write` records
//! `agents_svc` indexes), each with its diff size against a base and the session's token cost.
//! Clicking a file asks the host to open it in diff mode.
//!
//! The model ([`AgentSession`], [`group_touches`]) is pure. The view runs one `git show` + diff per
//! file on the background executor, one after another, and updates as each finishes.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, prelude::*, px,
};
use panel_api::{TabEvent, TabIcon, TabModel};
use theme::{Radius, Theme};
use ui_kit::{Icon, Spinner, VirtualList, icon};
use viewer_svc::GitBase;

use crate::event::ViewerEvent;
use crate::file_icon::FileGlyph;
use crate::format::{display_path, find_repo_root, format_tokens};
use crate::rows::tint;

/// One file write recorded for the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touch {
    pub path: PathBuf,
    /// Tool that wrote it (`Edit`, `Write`, `apply_patch`...).
    pub tool: String,
    pub ts_ms: i64,
}

/// A session to show. Everything is host-fed so the view has no database dependency of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSession {
    /// `claude`, `codex` or `opencode`.
    pub agent: String,
    pub session_id: String,
    pub project_root: Option<PathBuf>,
    /// Checkpoint commit to diff against; `None` = HEAD.
    pub base: Option<String>,
    /// Total tokens of the session, when known.
    pub tokens: Option<u64>,
    /// Estimated cost in USD, when the model is priced.
    pub cost_usd: Option<f64>,
    pub touches: Vec<Touch>,
}

impl AgentSession {
    /// Builds the session from the records `agents_svc::Store::files_touched` returns.
    pub fn from_touches(
        agent: &str,
        session_id: &str,
        project_root: Option<PathBuf>,
        touches: &[agents_svc::FileTouched],
    ) -> Self {
        Self {
            agent: agent.to_owned(),
            session_id: session_id.to_owned(),
            project_root,
            base: None,
            tokens: None,
            cost_usd: None,
            touches: touches
                .iter()
                .map(|t| Touch { path: PathBuf::from(&t.path), tool: t.tool.clone(), ts_ms: t.ts_ms })
                .collect(),
        }
    }

    /// Reads the session from the agents database: its touched files and its token / cost totals.
    /// Cheap (two indexed queries); call it on the UI thread or hand it a `Store` on a worker.
    pub fn from_store(
        store: &agents_svc::Store,
        session_id: &str,
        project_root: Option<PathBuf>,
        base: Option<String>,
    ) -> Result<Self, String> {
        let touched = store.files_touched(session_id).map_err(|e| e.to_string())?;
        let agent = touched.first().map_or("claude", |t| t.agent.as_str()).to_owned();
        let mut session = Self::from_touches(&agent, session_id, project_root, &touched);
        session.base = base;
        let usage = store.usage_summary(&agents_svc::Range::Session(session_id.to_owned())).map_err(|e| e.to_string())?;
        if !usage.is_empty() {
            session.tokens = Some(usage.iter().map(agents_svc::AgentUsage::total_tokens).sum());
            let costs: Vec<f64> = usage.iter().filter_map(|u| u.est_cost_usd).collect();
            if costs.len() == usage.len() {
                session.cost_usd = Some(costs.iter().sum());
            }
            if let Some(first) = usage.first() {
                session.agent = first.agent.as_str().to_owned();
            }
        }
        Ok(session)
    }

    /// Label of the base chip.
    pub fn base_label(&self) -> String {
        match &self.base {
            Some(c) => format!("checkpoint {}", c.chars().take(7).collect::<String>()),
            None => "HEAD".to_owned(),
        }
    }

    /// The git revision to diff against.
    pub fn git_base(&self) -> GitBase {
        match &self.base {
            Some(c) => GitBase::Commit(c.clone()),
            None => GitBase::Head,
        }
    }
}

/// What the diff of a file looks like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffStat {
    /// Not computed yet.
    Pending,
    Changed { added: usize, removed: usize },
    /// The file equals the base (the agent's edit was reverted or committed).
    Unchanged,
    Failed(String),
}

/// A distinct file of the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    /// Directory relative to the project root, forward slashes.
    pub dir: String,
    /// Distinct tools in first-use order.
    pub tools: Vec<String>,
    pub touches: usize,
    pub last_ts_ms: i64,
    pub stat: DiffStat,
}

/// One entry per file, the most recently touched first. Relative paths resolve against `root`.
pub fn group_touches(root: Option<&Path>, touches: &[Touch]) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> = Vec::new();
    for t in touches {
        let path = if t.path.is_relative() { root.map_or_else(|| t.path.clone(), |r| r.join(&t.path)) } else { t.path.clone() };
        match entries.iter_mut().find(|e| e.path == path) {
            Some(e) => {
                e.touches += 1;
                e.last_ts_ms = e.last_ts_ms.max(t.ts_ms);
                if !e.tools.contains(&t.tool) {
                    e.tools.push(t.tool.clone());
                }
            }
            None => {
                let shown = display_path(root, &path);
                let (dir, name) = match shown.rsplit_once('/') {
                    Some((d, n)) => (d.to_owned(), n.to_owned()),
                    None => (String::new(), shown),
                };
                entries.push(FileEntry {
                    path,
                    name,
                    dir,
                    tools: vec![t.tool.clone()],
                    touches: 1,
                    last_ts_ms: t.ts_ms,
                    stat: DiffStat::Pending,
                });
            }
        }
    }
    entries.sort_by(|a, b| b.last_ts_ms.cmp(&a.last_ts_ms).then_with(|| a.path.cmp(&b.path)));
    entries
}

/// `+12 -3` style totals over the files whose diff is known.
pub fn totals(entries: &[FileEntry]) -> (usize, usize) {
    entries.iter().fold((0, 0), |(a, r), e| match e.stat {
        DiffStat::Changed { added, removed } => (a + added, r + removed),
        _ => (a, r),
    })
}

/// `~$1.23` (estimated), `<$0.01` for tiny amounts.
pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 { "<$0.01".to_owned() } else { format!("~${usd:.2}") }
}

/// The view entity.
pub struct AgentTouchesView {
    theme: Theme,
    session: AgentSession,
    root: Option<PathBuf>,
    entries: Vec<FileEntry>,
    task: Option<Task<()>>,
}

impl EventEmitter<ViewerEvent> for AgentTouchesView {}

const ROW_H: f32 = 52.0;

impl AgentTouchesView {
    pub fn new(session: AgentSession, theme: Theme, cx: &mut Context<Self>) -> Self {
        let root = session
            .project_root
            .clone()
            .or_else(|| session.touches.first().and_then(|t| find_repo_root(&t.path)));
        let entries = group_touches(root.as_deref(), &session.touches);
        let mut v = Self { theme, session, root, entries, task: None };
        v.start_stats(cx);
        v
    }

    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Replaces the session (a new checkpoint, more edits) and recomputes the diffs.
    pub fn set_session(&mut self, session: AgentSession, cx: &mut Context<Self>) {
        self.root = session
            .project_root
            .clone()
            .or_else(|| session.touches.first().and_then(|t| find_repo_root(&t.path)));
        self.entries = group_touches(self.root.as_deref(), &session.touches);
        self.session = session;
        self.start_stats(cx);
        cx.notify();
    }

    /// One `git show` + diff per file, sequentially on the background executor; the view updates after
    /// each. Dropping the view (or replacing the session) cancels the rest.
    fn start_stats(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            for e in &mut self.entries {
                e.stat = DiffStat::Failed("sin repositorio git".into());
            }
            self.task = None;
            return;
        };
        let base = self.session.git_base();
        let paths: Vec<PathBuf> = self.entries.iter().map(|e| e.path.clone()).collect();
        self.task = Some(cx.spawn(async move |this, cx| {
            for path in paths {
                let (r, b, p) = (root.clone(), base.clone(), path.clone());
                let stat = cx
                    .background_executor()
                    .spawn(async move {
                        match viewer_svc::diff_against_git(&r, &p, b) {
                            Ok(h) if h.is_empty() => DiffStat::Unchanged,
                            Ok(h) => {
                                let (added, removed) = h.iter().fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()));
                                DiffStat::Changed { added, removed }
                            }
                            Err(e) => DiffStat::Failed(e.to_string()),
                        }
                    })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if let Some(e) = this.entries.iter_mut().find(|e| e.path == path) {
                        e.stat = stat;
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(e) = self.entries.get(ix) {
            cx.emit(ViewerEvent::OpenDiff { path: e.path.clone(), base: self.session.base.clone() });
        }
    }

    fn chip(&self, text: impl Into<SharedString>) -> gpui::Div {
        let c = self.theme.colors;
        div()
            .flex_none()
            .h(px(22.0))
            .px(px(9.0))
            .flex()
            .items_center()
            .rounded(px(8.0))
            .bg(c.surface_3)
            .text_size(px(self.theme.typography.ui_size_sm))
            .text_color(c.muted)
            .child(text.into())
    }

    fn row(&self, ix: usize, e: &FileEntry, this: &Entity<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let g = FileGlyph::of(&e.name);
        let light = theme.mode == theme::Mode::Light;
        let green = tint(&theme, if light { 2 } else { 10 }, 1.0);
        let red = tint(&theme, if light { 1 } else { 9 }, 1.0);
        let stat: AnyElement = match &e.stat {
            DiffStat::Pending => Spinner::new(c.accent).into_any_element(),
            DiffStat::Changed { added, removed } => div()
                .flex()
                .flex_row()
                .gap(px(8.0))
                .font_family(ty.mono_family)
                .text_size(px(12.0))
                .child(div().text_color(green).child(SharedString::from(format!("+{added}"))))
                .child(div().text_color(red).child(SharedString::from(format!("-{removed}"))))
                .into_any_element(),
            DiffStat::Unchanged => div().text_size(px(ty.ui_size_sm)).text_color(c.outline).child("sin cambios").into_any_element(),
            DiffStat::Failed(_) => icon(Icon::Alert, 14.0, c.outline).into_any_element(),
        };
        let tools = e.tools.join(" + ");
        let this = this.clone();
        div()
            .id(("touch-row", ix))
            .w_full()
            .h(px(ROW_H - 4.0))
            .my(px(2.0))
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .rounded(px(10.0))
            .cursor_pointer()
            .hover(move |s| s.bg(c.surface_2))
            .child(ui_kit::file_icon(ui_kit::FileIcon::for_name(&e.name), 16.0, g.tint(&theme)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.text)
                            .child(SharedString::from(e.name.clone())),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.muted)
                            .child(SharedString::from(if e.dir.is_empty() { tools } else { format!("{} \u{b7} {tools}", e.dir) })),
                    ),
            )
            .when(e.touches > 1, |d| d.child(self.chip(format!("x{}", e.touches))))
            .child(div().flex_none().min_w(px(64.0)).flex().justify_end().child(stat))
            .on_click(move |_, _, cx| this.update(cx, |v, cx| v.open(ix, cx)))
            .into_any_element()
    }
}

impl Render for AgentTouchesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let this = cx.entity();
        let (added, removed) = totals(&self.entries);

        let mut chips = div().flex().flex_row().flex_wrap().items_center().gap(px(6.0));
        chips = chips.child(self.chip(format!("{} archivos", self.entries.len())));
        chips = chips.child(self.chip(format!("vs {}", self.session.base_label())));
        if added + removed > 0 {
            let light = theme.mode == theme::Mode::Light;
            chips = chips
                .child(self.chip(format!("+{added}")).text_color(tint(&theme, if light { 2 } else { 10 }, 1.0)))
                .child(self.chip(format!("-{removed}")).text_color(tint(&theme, if light { 1 } else { 9 }, 1.0)));
        }
        if let Some(t) = self.session.tokens {
            chips = chips.child(self.chip(format!("{} tokens", format_tokens(t))));
        }
        if let Some(cost) = self.session.cost_usd {
            chips = chips.child(self.chip(format_cost(cost)));
        }

        let short: String = self.session.session_id.chars().take(8).collect();
        let header = div()
            .flex_none()
            .px(px(16.0))
            .pt(px(12.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .bg(c.surface_1)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(Icon::Agents, 16.0, c.muted))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_lg))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child(SharedString::from(format!("Que toco {}", self.session.agent))),
                    )
                    .child(div().text_size(px(ty.ui_size_sm)).text_color(c.outline).child(SharedString::from(short))),
            )
            .child(chips);

        let body: AnyElement = if self.entries.is_empty() {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .text_size(px(ty.ui_size_md))
                .text_color(c.muted)
                .child(icon(Icon::Check, 16.0, c.outline))
                .child("El agente no ha escrito archivos en esta sesion")
                .into_any_element()
        } else {
            let n = self.entries.len();
            VirtualList::new("touch-list", n, ROW_H, move |range, _, cx| {
                let me = this.read(cx);
                range.filter_map(|i| me.entries.get(i).map(|e| me.row(i, e, &this))).collect()
            })
            .size_full()
            .px(px(8.0))
            .py(px(6.0))
            .into_any_element()
        };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(Radius::MD))
            .bg(c.surface_0)
            .font_family(ty.ui_family)
            .text_color(c.text)
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// Tab wrapper so the shell can list the view next to terminals and files.
pub struct AgentTouchesTab {
    view: Entity<AgentTouchesView>,
    title: SharedString,
    agent: SharedString,
    _forward: Subscription,
}

impl EventEmitter<TabEvent> for AgentTouchesTab {}
impl EventEmitter<ViewerEvent> for AgentTouchesTab {}

impl AgentTouchesTab {
    pub fn new(session: AgentSession, theme: Theme, cx: &mut Context<Self>) -> Self {
        let title: SharedString = format!("Cambios de {}", session.agent).into();
        let agent: SharedString = session.agent.clone().into();
        let view = cx.new(|cx| AgentTouchesView::new(session, theme, cx));
        let forward = cx.subscribe(&view, |_, _, ev: &ViewerEvent, cx| cx.emit(ev.clone()));
        Self { view, title, agent, _forward: forward }
    }

    pub fn view(&self) -> &Entity<AgentTouchesView> {
        &self.view
    }
}

impl TabModel for AgentTouchesTab {
    fn title(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn icon(&self, _cx: &App) -> TabIcon {
        TabIcon::Agent(self.agent.clone())
    }

    fn subtitle(&self, cx: &App) -> Option<SharedString> {
        let n = self.view.read(cx).entries().len();
        Some(format!("{n} archivos").into())
    }

    fn agent(&self, _cx: &App) -> Option<SharedString> {
        Some(self.agent.clone())
    }

    fn content(&self, _cx: &App) -> gpui::AnyView {
        self.view.clone().into()
    }

    fn theme_changed(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.set_theme(theme, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &str, tool: &str, ts: i64) -> Touch {
        Touch { path: PathBuf::from(path), tool: tool.into(), ts_ms: ts }
    }

    #[test]
    fn groups_by_file_most_recent_first() {
        let root = Path::new("C:/proj");
        let entries = group_touches(
            Some(root),
            &[
                touch("C:/proj/src/a.rs", "Edit", 10),
                touch("C:/proj/src/b.rs", "Write", 20),
                touch("src/a.rs", "Edit", 30),
                touch("C:/proj/src/a.rs", "Write", 5),
            ],
        );
        assert_eq!(entries.len(), 2);
        // a.rs was touched last (ts 30)
        assert_eq!(entries[0].name, "a.rs");
        assert_eq!(entries[0].dir, "src");
        assert_eq!(entries[0].touches, 3);
        assert_eq!(entries[0].last_ts_ms, 30);
        assert_eq!(entries[0].tools, ["Edit", "Write"]);
        assert_eq!(entries[1].name, "b.rs");
        assert!(entries.iter().all(|e| e.stat == DiffStat::Pending));
    }

    #[test]
    fn files_outside_the_root_keep_their_full_path() {
        let entries = group_touches(Some(Path::new("C:/proj")), &[touch("D:/other/x.rs", "Edit", 1)]);
        assert_eq!(entries[0].name, "x.rs");
        assert_eq!(entries[0].dir, "D:/other");
    }

    #[test]
    fn empty_session() {
        assert!(group_touches(None, &[]).is_empty());
    }

    #[test]
    fn totals_only_count_known_stats() {
        let mut e = group_touches(None, &[touch("a.rs", "Edit", 1), touch("b.rs", "Edit", 2), touch("c.rs", "Edit", 3)]);
        e[0].stat = DiffStat::Changed { added: 5, removed: 2 };
        e[1].stat = DiffStat::Unchanged;
        assert_eq!(totals(&e), (5, 2));
    }

    #[test]
    fn base_label_and_revision() {
        let mut s = AgentSession {
            agent: "claude".into(),
            session_id: "abc".into(),
            project_root: None,
            base: None,
            tokens: None,
            cost_usd: None,
            touches: vec![],
        };
        assert_eq!(s.base_label(), "HEAD");
        assert_eq!(s.git_base(), GitBase::Head);
        s.base = Some("a1b2c3d4e5f6".into());
        assert_eq!(s.base_label(), "checkpoint a1b2c3d");
        assert_eq!(s.git_base(), GitBase::Commit("a1b2c3d4e5f6".into()));
    }

    #[test]
    fn cost_formatting() {
        assert_eq!(format_cost(0.0), "<$0.01");
        assert_eq!(format_cost(4.1), "~$4.10");
    }

    #[test]
    fn session_reads_from_the_agents_store() {
        use agents_svc::{Agent, FileTouch, Store, TokenCounts, UsageEvent};
        let store = Store::open_in_memory().unwrap();
        for (i, path) in ["C:/proj/a.rs", "C:/proj/b.rs"].into_iter().enumerate() {
            store
                .insert_touch(&FileTouch {
                    agent: Agent::Claude,
                    session_id: "s1".into(),
                    ts_ms: 1_000 + i as i64,
                    path: path.into(),
                    tool: "Edit".into(),
                    touch_key: format!("k{i}"),
                })
                .unwrap();
        }
        store
            .upsert_usage(&UsageEvent {
                agent: Agent::Claude,
                session_id: "s1".into(),
                ts_ms: 1_000,
                model: "claude-opus-4-5".into(),
                tokens: TokenCounts { input: 100, output: 50, cache_create: 0, cache_create_1h: 0, cache_read: 25 },
                cwd: None,
                git_branch: None,
                msg_key: "m1".into(),
            })
            .unwrap();
        let s = AgentSession::from_store(&store, "s1", Some("C:/proj".into()), Some("abc1234".into())).unwrap();
        assert_eq!(s.agent, "claude");
        assert_eq!(s.touches.len(), 2);
        assert_eq!(s.tokens, Some(175));
        assert_eq!(s.base_label(), "checkpoint abc1234");
        let entries = group_touches(s.project_root.as_deref(), &s.touches);
        assert_eq!(entries[0].name, "b.rs");

        let empty = AgentSession::from_store(&store, "nope", None, None).unwrap();
        assert!(empty.touches.is_empty());
        assert!(empty.tokens.is_none());
    }
}
