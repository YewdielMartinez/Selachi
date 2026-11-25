//! The "Actividad" tab of the inspector: the live feed of the selected run.
//!
//! * The rows are a gpui `list` (variable heights, only the visible ones are built) aligned to the
//!   bottom: while the user has not scrolled up, new rows keep the end in view; scrolling up pauses
//!   that and shows «Ir al final».
//! * The feed arrives as an `Arc<ActivityFeed>` from the model (already merged, capped to 500 rows
//!   and redacted). Each render diffs the row signatures against the previous ones and tells the
//!   list the one range that changed, so streaming text re-measures one row, not the list.
//! * Text of messages and the output of an expanded command is selectable and copyable
//!   (`ui_kit::selectable`, one selection group per row).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, list, prelude::*, px, relative,
};
use theme::{Radius, Rgba, Theme};
use ui_kit::{
    AgentMark, Button, EmptyState, Gap, Icon, RowStatus, SegmentedControl, Segments,
    SelectableText, SelectionGroup, StatusGlyph, agent_icon, icon,
};

use crate::activity::{
    ActivityEvent, ActivityFeed, ActivityFilter, ActivityKind, ApprovalOutcome, CommandEntry,
    CommandStatus, OUTPUT_COLLAPSED_LINES, diff_keys, last_lines, needs_collapse,
};
use crate::fmt;
use crate::inspector::approval_mode_label;
use crate::tree::{MapNode, NodeKey};
use crate::view::AgentMapView;

/// Note under the feed of an external agent.
pub const EXTERNAL_ACTIVITY_NOTE: &str = "Solo se ven los procesos que lanzó el agente. El texto de sus respuestas no es visible: corre fuera de Selachi.";

/// State of the feed in one view.
pub(crate) struct ActivityUi {
    node: Option<NodeKey>,
    pub(crate) filter: ActivityFilter,
    pub(crate) list: ListState,
    /// Signature of each shown row (event signature + expanded), to diff against the next render.
    keys: Vec<u64>,
    /// Indices into `feed.events` of the rows shown under the filter.
    shown: Vec<usize>,
    feed: Arc<ActivityFeed>,
    mark: Option<AgentMark>,
    /// Ids of the rows the user expanded.
    expanded: HashSet<String>,
    /// The user scrolled up: the list no longer follows the end.
    pub(crate) scrolled: bool,
    groups: HashMap<String, SelectionGroup>,
}

impl ActivityUi {
    pub(crate) fn new(cx: &mut Context<AgentMapView>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(600.0));
        let this = cx.entity().downgrade();
        list.set_scroll_handler(move |ev, _, cx| {
            let scrolled = ev.is_scrolled;
            this.update(cx, |v, cx| {
                if v.act.scrolled != scrolled {
                    v.act.scrolled = scrolled;
                    cx.notify();
                }
            })
            .ok();
        });
        Self {
            node: None,
            filter: ActivityFilter::All,
            list,
            keys: Vec::new(),
            shown: Vec::new(),
            feed: Arc::new(ActivityFeed::default()),
            mark: None,
            expanded: HashSet::new(),
            scrolled: false,
            groups: HashMap::new(),
        }
    }

    /// Rows shown right now (tests read it).
    #[cfg(test)]
    pub(crate) fn shown_len(&self) -> usize {
        self.shown.len()
    }

    /// Takes the new feed of `key`: starts the list over when the node changed, else splices only
    /// the rows that changed.
    pub(crate) fn sync(&mut self, key: &NodeKey, mark: AgentMark, feed: Arc<ActivityFeed>) {
        let node_changed = self.node.as_ref() != Some(key);
        if node_changed {
            self.node = Some(key.clone());
            self.expanded.clear();
            self.groups.clear();
            self.keys.clear();
            self.shown.clear();
            self.scrolled = false;
            self.list.reset(0);
        }
        self.mark = Some(mark);
        self.feed = feed;
        self.shown = self.feed.visible(self.filter);
        let new_keys: Vec<u64> = self
            .shown
            .iter()
            .map(|&i| {
                let e = &self.feed.events[i];
                e.sig ^ u64::from(self.expanded.contains(&e.id)).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            })
            .collect();
        if let Some((old, n)) = diff_keys(&self.keys, &new_keys) {
            self.list.splice(old, n);
        }
        self.keys = new_keys;
        let ids: HashSet<&str> = self.feed.events.iter().map(|e| e.id.as_str()).collect();
        self.groups.retain(|id, _| ids.contains(id.as_str()));
    }

    pub(crate) fn set_filter(&mut self, f: ActivityFilter) {
        if self.filter != f {
            self.filter = f;
            self.scrolled = false;
            self.keys.clear();
            self.list.reset(0);
        }
    }
}

fn dur(ms: i64) -> String {
    if ms < 1_000 {
        format!("{} ms", ms.max(0))
    } else if ms < 10_000 {
        format!("{:.1} s", ms as f64 / 1000.0).replace('.', ",")
    } else {
        fmt::duration_ms(ms)
    }
}

fn outcome_color(theme: &Theme, o: ApprovalOutcome) -> Rgba {
    let c = theme.colors;
    match o {
        ApprovalOutcome::Requested => c.warning,
        ApprovalOutcome::Allowed | ApprovalOutcome::Always | ApprovalOutcome::Auto => c.success,
        ApprovalOutcome::Blocked | ApprovalOutcome::Denied => c.error,
        ApprovalOutcome::Expired => c.muted,
    }
}

/// `ejecutando · 12 s`, `ok · 1,4 s`, `código 101 · 3 s`, `interrumpido`.
pub(crate) fn command_meta(c: &CommandEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(match c.status {
        CommandStatus::Running => "ejecutando".to_string(),
        CommandStatus::Ok => "ok".to_string(),
        CommandStatus::Failed => match c.exit_code {
            Some(code) => format!("código {code}"),
            None => "falló".to_string(),
        },
        CommandStatus::Cancelled => "interrumpido".to_string(),
    });
    if let Some(ms) = c.duration_ms {
        parts.push(dur(ms));
    }
    if let Some(pid) = c.pid {
        parts.push(format!("PID {pid}"));
    }
    if let Some(cpu) = c.cpu_pct {
        parts.push(format!("CPU {}", fmt::cpu(cpu)));
    }
    if let Some(rss) = c.rss_bytes {
        parts.push(format!("RAM {}", fmt::bytes(rss)));
    }
    parts.join(" · ")
}

impl AgentMapView {
    /// A selection group for the row `id`, created when the row is first painted.
    fn row_group(&mut self, id: &str, cx: &mut Context<Self>) -> SelectionGroup {
        if let Some(g) = self.act.groups.get(id) {
            return g.clone();
        }
        let g = SelectionGroup::with_tab_stop(false, cx);
        self.act.groups.insert(id.to_string(), g.clone());
        g
    }

    pub(crate) fn set_activity_filter(&mut self, f: ActivityFilter, cx: &mut Context<Self>) {
        self.act.set_filter(f);
        cx.notify();
    }

    fn toggle_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.act.expanded.remove(id) {
            self.act.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    fn jump_to_end(&mut self, cx: &mut Context<Self>) {
        self.act.scrolled = false;
        let n = self.act.shown.len();
        self.act.list.reset(n);
        cx.notify();
    }

    /// The whole tab: filters, the feed (or what explains why it is empty) and «Ir al final».
    pub(crate) fn activity_panel(
        &mut self,
        key: &NodeKey,
        node: &MapNode,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (feed, loaded) = {
            let m = self.model.read(cx);
            (m.activity_feed(key), m.activity_loaded(key))
        };
        let feed = feed.unwrap_or_default();
        self.act.sync(key, node.mark, feed.clone());

        let this = cx.entity();
        let counts = feed.counts();
        let filters = SegmentedControl::new(
            "agentmap-activity-filter",
            &t,
            ActivityFilter::ALL
                .iter()
                .map(|f| SharedString::from(f.label())),
        )
        .selected(
            ActivityFilter::ALL
                .iter()
                .position(|f| *f == self.act.filter)
                .unwrap_or(0),
        )
        .track(c.surface_1)
        .on_change({
            let this = this.clone();
            move |ix, _, cx| {
                let f = ActivityFilter::ALL.get(ix).copied().unwrap_or_default();
                this.update(cx, |v, cx| v.set_activity_filter(f, cx));
            }
        });

        let live = feed.active && loaded;
        let status_line = {
            let mut parts: Vec<String> = Vec::new();
            if feed.external {
                parts.push(format!(
                    "{} proceso{} hijo{}",
                    counts[2],
                    if counts[2] == 1 { "" } else { "s" },
                    if counts[2] == 1 { "" } else { "s" }
                ));
            } else {
                parts.push(format!("{} eventos", counts[0]));
            }
            if feed.dropped > 0 {
                parts.push(format!("se muestran los últimos {}", feed.events.len()));
            }
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_size(px(ty.ui_size_xs))
                .text_color(c.muted)
                .when(live, |d| {
                    d.child(StatusGlyph::new(&t, RowStatus::Working))
                        .child(div().text_color(c.accent).child("En vivo"))
                })
                .child(SharedString::from(parts.join(" · ")))
        };

        let body: AnyElement = if !loaded {
            self.empty_note("Leyendo la actividad…", "")
        } else if self.act.shown.is_empty() {
            if feed.is_empty() {
                if feed.external {
                    self.empty_note(
                        "Sin procesos hijos",
                        "El agente no tiene procesos en marcha ahora mismo.",
                    )
                } else if feed.active {
                    self.empty_note(
                        "Esperando al agente",
                        "Los mensajes, comandos y archivos aparecen aquí según ocurren.",
                    )
                } else {
                    self.empty_note(
                        "Sin actividad registrada",
                        "Esta ejecución no dejó mensajes ni comandos en el hilo ni en el registro de eventos.",
                    )
                }
            } else {
                self.empty_note("Nada con este filtro", "Prueba con «Todo».")
            }
        } else {
            list(
                self.act.list.clone(),
                cx.processor(|this, ix: usize, _w, cx| this.activity_row(ix, cx)),
            )
            .size_full()
            .into_any_element()
        };

        let to_end = self.act.scrolled.then(|| {
            div()
                .absolute()
                .bottom(px(10.0))
                .left(px(0.0))
                .right(px(0.0))
                .flex()
                .justify_center()
                .child(
                    Button::tonal("agentmap-activity-end", &t, "Ir al final").on_click({
                        let this = this.clone();
                        move |_, _, cx| this.update(cx, |v, cx| v.jump_to_end(cx))
                    }),
                )
        });

        div()
            .size_full()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex_none().px(px(14.0)).child(filters))
            .child(div().flex_none().px(px(14.0)).child(status_line))
            .when(feed.external, |d| {
                d.child(
                    div()
                        .flex_none()
                        .mx(px(14.0))
                        .p(px(8.0))
                        .rounded(px(Radius::CONTROL))
                        .bg(c.surface_1)
                        .text_size(px(ty.ui_size_xs))
                        .text_color(c.muted)
                        .child(EXTERNAL_ACTIVITY_NOTE),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(body)
                    .children(to_end),
            )
            .into_any_element()
    }

    fn empty_note(&self, title: &str, body: &str) -> AnyElement {
        let mut e = EmptyState::new(&self.theme, title.to_string())
            .icon(Icon::Agents)
            .fill(true);
        if !body.is_empty() {
            e = e.body(body.to_string());
        }
        div().size_full().child(e).into_any_element()
    }

    /// One row of the feed (`ix` counts the rows shown under the filter).
    fn activity_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(ev) = self
            .act
            .shown
            .get(ix)
            .and_then(|&i| self.act.feed.events.get(i))
            .cloned()
        else {
            return div().into_any_element();
        };
        let t = self.theme;
        let expanded = self.act.expanded.contains(&ev.id);
        let (mark, body) = match &ev.kind {
            ActivityKind::Message { text, streaming } => (
                self.message_mark(),
                self.message_body(&ev, text, *streaming, expanded, cx),
            ),
            ActivityKind::Command(c) => (
                command_mark(&t, c.status),
                self.command_body(&ev, c, expanded, cx),
            ),
            ActivityKind::File {
                path,
                added,
                removed,
            } => (
                icon(Icon::Edit, 14.0, t.colors.muted).into_any_element(),
                file_body(&t, path, *added, *removed),
            ),
            ActivityKind::Approval {
                subject,
                outcome,
                mode,
                reason,
            } => (
                icon(Icon::Attention, 14.0, outcome_color(&t, *outcome)).into_any_element(),
                approval_body(&t, subject, *outcome, mode.as_deref(), reason.as_deref()),
            ),
        };
        div()
            .w_full()
            .px(px(14.0))
            .py(px(6.0))
            .flex()
            .flex_row()
            .items_start()
            .gap(px(8.0))
            .child(div().flex_none().w(px(16.0)).pt(px(1.0)).child(mark))
            .child(div().flex_1().min_w(px(0.0)).child(body))
            .into_any_element()
    }

    fn message_mark(&self) -> AnyElement {
        let c = self.theme.colors;
        match self.act.mark {
            Some(m) => agent_icon(m, 14.0, c.muted).into_any_element(),
            None => icon(Icon::Agents, 14.0, c.muted).into_any_element(),
        }
    }

    fn message_body(
        &mut self,
        ev: &ActivityEvent,
        text: &str,
        streaming: bool,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let collapsible = needs_collapse(text);
        let (shown, cut) = if collapsible && !expanded {
            let (tail, cut) = last_lines(text, crate::activity::MESSAGE_COLLAPSED_LINES);
            (tail, cut)
        } else {
            (text.trim_end().to_string(), false)
        };
        let g = self.row_group(&ev.id, cx);
        let mut segs = Segments::new();
        let seg = segs.push(shown.clone(), Gap::Block);
        g.set_segments(segs);
        let id = ev.id.clone();
        let this = cx.entity();
        let toggle = collapsible.then(|| {
            let id2 = id.clone();
            div()
                .id(SharedString::from(format!("act-more-{id}")))
                .cursor_pointer()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.accent)
                .hover(|s| s.underline())
                .on_click(move |_, _, cx| {
                    let id3 = id2.clone();
                    this.update(cx, |v, cx| v.toggle_row(&id3, cx));
                })
                .child(if expanded { "Ver menos" } else { "Ver todo" })
        });
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                g.container(SharedString::from(format!("act-msg-{id}")), &t)
                    .w_full()
                    .min_w(px(0.0))
                    .text_size(px(ty.ui_size_md))
                    .line_height(relative(1.4))
                    .text_color(c.text)
                    .when(cut, |d| d.child(div().text_color(c.muted).child("…")))
                    .child(SelectableText::new(&g, seg, SharedString::from(shown))),
            )
            .when(streaming, |d| {
                d.child(
                    div()
                        .text_size(px(ty.ui_size_xs))
                        .text_color(c.accent)
                        .child("escribiendo…"),
                )
            })
            .children(toggle)
            .into_any_element()
    }

    fn command_body(
        &mut self,
        ev: &ActivityEvent,
        cmd: &CommandEntry,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (tail, hidden) = last_lines(&cmd.output, OUTPUT_COLLAPSED_LINES);
        let has_output = !cmd.output.trim().is_empty();
        let expandable =
            hidden || cmd.command.chars().count() > 60 || cmd.output.lines().count() > 1;
        let id = ev.id.clone();
        let this = cx.entity();
        let chevron = expandable.then(|| {
            let id2 = id.clone();
            let this = this.clone();
            div()
                .id(SharedString::from(format!("act-cmd-toggle-{id}")))
                .flex_none()
                .size(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::CHIP))
                .cursor_pointer()
                .hover(|s| s.bg(c.surface_3))
                .on_click(move |_, _, cx| {
                    let id3 = id2.clone();
                    this.update(cx, |v, cx| v.toggle_row(&id3, cx));
                })
                .child(icon(
                    if expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    14.0,
                    c.muted,
                ))
        });
        let meta_color = match cmd.status {
            CommandStatus::Failed => c.error,
            CommandStatus::Running => c.accent,
            _ => c.muted,
        };

        let command_line: AnyElement = if expanded {
            let g = self.row_group(&ev.id, cx);
            let mut segs = Segments::new();
            let s_cmd = segs.push(cmd.command.clone(), Gap::Block);
            let s_out = has_output.then(|| segs.push(cmd.output.clone(), Gap::Block));
            g.set_segments(segs);
            let out_el = s_out.map(|s| {
                div()
                    .id(SharedString::from(format!("act-out-{id}")))
                    .w_full()
                    .max_h(px(220.0))
                    .overflow_y_scroll()
                    .mt(px(4.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(Radius::CONTROL))
                    .bg(c.surface_1)
                    .text_color(c.muted)
                    .child(SelectableText::new(
                        &g,
                        s,
                        SharedString::from(cmd.output.clone()),
                    ))
            });
            g.container(SharedString::from(format!("act-cmd-sel-{id}")), &t)
                .w_full()
                .min_w(px(0.0))
                .font_family(ty.mono_family)
                .text_size(px(ty.ui_size_sm))
                .line_height(relative(1.4))
                .child(div().text_color(c.text).child(SelectableText::new(
                    &g,
                    s_cmd,
                    SharedString::from(cmd.command.clone()),
                )))
                .children(out_el)
                .into_any_element()
        } else {
            div()
                .w_full()
                .min_w(px(0.0))
                .font_family(ty.mono_family)
                .text_size(px(ty.ui_size_sm))
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(c.text)
                        .child(SharedString::from(cmd.command.clone())),
                )
                .when(has_output, |d| {
                    d.child(
                        // One line each, cut with an ellipsis: nothing is clipped mid-glyph.
                        div()
                            .mt(px(4.0))
                            .px(px(8.0))
                            .py(px(5.0))
                            .rounded(px(Radius::CONTROL))
                            .bg(c.surface_1)
                            .text_color(c.muted)
                            .flex()
                            .flex_col()
                            .children(tail.lines().map(|l| {
                                div()
                                    .w_full()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(l.to_string()))
                            })),
                    )
                })
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(4.0))
                    .child(div().flex_1().min_w(px(0.0)).child(command_line))
                    .children(chevron),
            )
            .child(
                div()
                    .text_size(px(ty.ui_size_xs))
                    .text_color(meta_color)
                    .child(SharedString::from(command_meta(cmd))),
            )
            .into_any_element()
    }
}

fn command_mark(theme: &Theme, status: CommandStatus) -> AnyElement {
    match status {
        CommandStatus::Running => StatusGlyph::new(theme, RowStatus::Working).into_any_element(),
        CommandStatus::Ok => StatusGlyph::new(theme, RowStatus::Done).into_any_element(),
        CommandStatus::Failed => StatusGlyph::new(theme, RowStatus::Error).into_any_element(),
        CommandStatus::Cancelled => icon(Icon::Close, 14.0, theme.colors.muted).into_any_element(),
    }
}

fn file_body(theme: &Theme, path: &str, added: Option<u32>, removed: Option<u32>) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .font_family(ty.mono_family)
                .text_size(px(ty.ui_size_sm))
                .text_color(c.text)
                .child(SharedString::from(path.to_string())),
        )
        .when_some(added, |d, n| {
            d.child(
                div()
                    .flex_none()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.success)
                    .child(SharedString::from(format!("+{n}"))),
            )
        })
        .when_some(removed, |d, n| {
            d.child(
                div()
                    .flex_none()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.error)
                    .child(SharedString::from(format!("−{n}"))),
            )
        })
        .into_any_element()
}

fn approval_body(
    theme: &Theme,
    subject: &str,
    outcome: ApprovalOutcome,
    mode: Option<&str>,
    reason: Option<&str>,
) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    let mut notes: Vec<String> = Vec::new();
    if let Some(m) = mode {
        notes.push(approval_mode_label(m));
    }
    if let Some(r) = reason {
        notes.push(r.to_string());
    }
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_none()
                        .h(px(20.0))
                        .px(px(7.0))
                        .flex()
                        .items_center()
                        .rounded(px(Radius::CHIP))
                        .bg(c.surface_3)
                        .text_size(px(ty.ui_size_xs))
                        .text_color(outcome_color(theme, outcome))
                        .child(outcome.label()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(ty.mono_family)
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.text)
                        .child(SharedString::from(subject.to_string())),
                ),
        )
        .when(!notes.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(ty.ui_size_xs))
                    .text_color(c.muted)
                    .child(SharedString::from(notes.join(" · "))),
            )
        })
        .into_any_element()
}
