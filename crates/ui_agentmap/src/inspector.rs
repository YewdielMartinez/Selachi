//! The inspector: everything about one node (the exact prompt, the limits that applied, who
//! launched it, the transcript, the diff, the approvals) and the actions on it.
//!
//! The inspector **never runs anything**. Each action is an [`AgentMapEvent`]; the destructive ones
//! (cancel, retry) first ask for confirmation inline, and applying changes is confirmed by the
//! host's own preview.

use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use theme::{Radius, Theme};
use ui_kit::{Button, GhostButton, Icon, IconButton, Pill, SectionLabel, SegmentedControl};

use crate::events::AgentMapEvent;
use crate::fmt;
use crate::model::{InspectorTab, NodeDetail};
use crate::parts::{chip, external_badge, lead_mark, node_status_color, status_color, status_mark};
use crate::tree::{MapNode, NodeKey, NodeStatus, agent_display};
use crate::view::{AgentMapView, Pending};

/// Explanation shown in the inspector of an external (non-harness) agent process.
pub const EXTERNAL_EXPLANATION: &str = "Lanzado fuera de Selachi: sin prompt, límites ni diff. Usa run_agent del MCP selachi para verlos.";

/// Agents a retry can be sent to: `(id, label)`.
pub const RETRY_AGENTS: [(&str, &str); 5] = [
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("opencode", "OpenCode"),
    ("gemini", "Gemini"),
    ("agy", "Antigravity"),
];

/// Which actions make sense for a node (what the inspector offers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Available {
    pub open_thread: bool,
    pub diff: bool,
    pub worktree: bool,
    pub apply: bool,
    pub cancel: bool,
    pub retry: bool,
    pub duplicate: bool,
    /// External node only: stop the external process subtree (running only, behind inline confirm).
    pub stop_external: bool,
    /// External node only: jump to this process in the Processes panel.
    pub open_in_processes: bool,
    /// External node only: copy the redacted command line.
    pub copy_command: bool,
}

/// The actions available for `node`.
pub fn available(node: &MapNode) -> Available {
    if let Some(ext) = node.external() {
        return Available {
            stop_external: node.status.is_active() && ext.pid > 0,
            open_in_processes: ext.pid > 0,
            copy_command: !ext.cmdline_redacted.trim().is_empty(),
            ..Available::default()
        };
    }
    let is_thread = matches!(node.key, NodeKey::Thread(_));
    let has_changes = node.files > 0;
    let applied = node.run.as_ref().is_some_and(|r| r.applied_ms.is_some());
    Available {
        open_thread: is_thread && node.thread.is_some(),
        diff: is_thread && has_changes && node.worktree.is_some(),
        worktree: node.worktree.is_some(),
        apply: is_thread
            && has_changes
            && node.worktree.is_some()
            && node.status == NodeStatus::Done
            && !applied,
        cancel: is_thread && node.status.is_active(),
        retry: is_thread && !node.is_root() && !node.status.is_active() && !node.agent.is_empty(),
        duplicate: is_thread && !node.is_root() && !node.agent.is_empty(),
        stop_external: false,
        open_in_processes: false,
        copy_command: false,
    }
}

/// `(label, value)` summary rows for an external (non-harness) node in the inspector.
/// Returns an empty vec for non-external nodes.
pub fn external_facts(node: &MapNode, now_ms: i64) -> Vec<(String, String)> {
    let Some(ext) = node.external() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if !node.agent.is_empty() {
        rows.push(("Agente".to_string(), agent_display(&node.agent)));
    }
    if let Some(m) = fmt::model_effort(ext.model.as_deref(), ext.effort.as_deref()) {
        rows.push(("Modelo".to_string(), m));
    }
    rows.push(("PID".to_string(), ext.pid.to_string()));
    rows.push(("Estado".to_string(), ext.status_label()));
    if let Some(ms) = node.duration_ms(now_ms) {
        rows.push(("Duración".to_string(), fmt::duration_ms(ms)));
    }
    if let Some(pct) = ext.cpu_pct {
        rows.push(("CPU".to_string(), fmt::cpu(pct)));
    }
    if ext.rss_bytes > 0 {
        rows.push(("RAM".to_string(), fmt::bytes(ext.rss_bytes)));
    }
    if let Some(cwd) = &ext.cwd {
        rows.push(("Carpeta".to_string(), cwd.display().to_string()));
    }
    rows
}

/// Spanish label of an approval mode as the harness spells it (`ask | allowlist | auto`).
pub fn approval_mode_label(m: &str) -> String {
    match m.trim().to_ascii_lowercase().as_str() {
        "ask" => "Preguntar".into(),
        "allowlist" => "Solo comandos permitidos".into(),
        "auto" => "No preguntar".into(),
        other => other.to_string(),
    }
}

/// Spanish word of a permission as the harness spells it.
pub fn permission_label(p: &str) -> String {
    match p.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "read-only" | "readonly" => "Solo lectura".into(),
        "ask" => "Preguntar".into(),
        "accept-edits" => "Aceptar ediciones".into(),
        "full" => "Acceso completo".into(),
        other => other.to_string(),
    }
}

/// `(label, value)` rows of "Límites aplicados": what the harness recorded, translated; without
/// that, what can be deduced from the run (permission, depth, time budget).
pub fn limit_rows(node: &MapNode) -> Vec<(String, String)> {
    let mut rows = limit_rows_base(node);
    // how the child's commands were approved (harness record, `limits.approvals`)
    if let Some(m) = node.run.as_ref().and_then(|r| r.approval_mode.as_deref()) {
        rows.push(("Aprobación de comandos".to_string(), approval_mode_label(m)));
    }
    rows
}

fn limit_rows_base(node: &MapNode) -> Vec<(String, String)> {
    let recorded = node
        .run
        .as_ref()
        .map(|r| r.effective_limits.as_slice())
        .unwrap_or_default();
    if !recorded.is_empty() {
        return recorded
            .iter()
            .map(|(k, v)| (limit_label(k), limit_value(k, v)))
            .collect();
    }
    let mut rows = Vec::new();
    if let Some(p) = &node.permission {
        rows.push(("Permiso".to_string(), permission_label(p)));
    }
    if node.depth > 0 {
        rows.push(("Profundidad".to_string(), node.depth.to_string()));
    }
    if let Some(run) = &node.run
        && let (Some(deadline), Some(start)) = (run.deadline_ms, run.started_ms)
        && deadline > start
    {
        rows.push((
            "Tiempo máximo".to_string(),
            fmt::duration_ms(deadline - start),
        ));
    }
    rows
}

fn limit_label(key: &str) -> String {
    match key {
        "permission" | "max_permission" => "Permiso".into(),
        "depth" | "max_depth" => "Profundidad".into(),
        "timeout" | "timeout_s" | "timeout_secs" => "Tiempo máximo".into(),
        "budget_tokens" => "Presupuesto de tokens".into(),
        "worktree" => "Worktree".into(),
        "max_concurrent" => "Ejecuciones a la vez".into(),
        "approvals" => "Aprobación de comandos".into(),
        other => {
            let spaced = other.replace('_', " ");
            let mut chars = spaced.chars();
            match chars.next() {
                Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

fn limit_value(key: &str, value: &str) -> String {
    match key {
        "permission" | "max_permission" => permission_label(value),
        "timeout" | "timeout_s" | "timeout_secs" => value
            .parse::<i64>()
            .map(|s| fmt::duration_ms(s * 1000))
            .unwrap_or_else(|_| value.to_string()),
        "budget_tokens" => match value.parse::<u64>() {
            Ok(0) => "sin límite".into(),
            Ok(n) => format!("{} tokens", fmt::tokens(n)),
            Err(_) => value.to_string(),
        },
        "worktree" => {
            if value == "true" {
                "Sí".into()
            } else {
                "No".into()
            }
        }
        "approvals" => match value {
            "auto" => "No preguntar".into(),
            "allowlist" => "Solo comandos permitidos".into(),
            "ask" => "Preguntar".into(),
            other => other.to_string(),
        },
        _ => value.to_string(),
    }
}

/// `allowed` / `denied`... as the history shows them.
pub fn decision_label(d: &str) -> &'static str {
    match d {
        "allowed" => "Permitida",
        "always" => "Permitida siempre",
        "denied" => "Rechazada",
        "expired" => "Expiró sin respuesta",
        _ => "Pendiente",
    }
}

fn fact(theme: &Theme, label: &str, value: impl Into<SharedString>) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(10.0))
        .child(
            div()
                .w(px(88.0))
                .flex_none()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(ty.ui_size_md))
                .text_color(c.text)
                .child(value.into()),
        )
        .into_any_element()
}

fn mono_block(theme: &Theme, id: &'static str, text: &str, max_h: f32) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .id(id)
        .w_full()
        .max_h(px(max_h))
        .overflow_y_scroll()
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(Radius::CONTROL))
        .bg(c.surface_1)
        .font_family(ty.mono_family)
        .text_size(px(ty.ui_size_sm))
        .text_color(c.text)
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

fn section(theme: &Theme, title: &str, body: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(SectionLabel::new(theme, title.to_string()))
        .children(body)
        .into_any_element()
}

fn muted(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(theme.typography.ui_size_sm))
        .text_color(theme.colors.muted)
        .child(text.into())
        .into_any_element()
}

type Act = Box<dyn Fn(&mut AgentMapView, &mut Context<AgentMapView>) + 'static>;

/// A tonal (or ghost) button that runs `f` on the view.
fn button(
    this: &Entity<AgentMapView>,
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    ghost: bool,
    f: Act,
) -> AnyElement {
    let this = this.clone();
    let on = move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
        this.update(cx, |v, cx| f(v, cx));
    };
    if ghost {
        GhostButton::new(id, theme, label)
            .on_click(on)
            .into_any_element()
    } else {
        Button::tonal(id, theme, label)
            .on_click(on)
            .into_any_element()
    }
}

/// The tabs an inspector offers: external agents have no prompt of their own.
pub(crate) fn tabs_for(external: bool) -> &'static [InspectorTab] {
    if external {
        &InspectorTab::ALL[..2]
    } else {
        &InspectorTab::ALL
    }
}

impl AgentMapView {
    /// `Resumen · Actividad · Prompt`.
    fn tab_bar(
        &self,
        tab: InspectorTab,
        external: bool,
        this: &Entity<AgentMapView>,
    ) -> AnyElement {
        let t = self.theme;
        let tabs = tabs_for(external);
        let this = this.clone();
        SegmentedControl::new(
            "agentmap-inspector-tabs",
            &t,
            tabs.iter().map(|x| SharedString::from(x.label())),
        )
        .selected(tabs.iter().position(|x| *x == tab).unwrap_or(0))
        .track(t.colors.surface_1)
        .on_change(move |ix, _, cx| {
            if let Some(next) = tabs.get(ix).copied() {
                this.update(cx, |v, cx| v.set_inspector_tab(next, cx));
            }
        })
        .into_any_element()
    }

    /// A scrolling column that fills the tab area (`Resumen`, `Prompt`).
    fn scroll_tab(&self, id: &'static str, body: Vec<AnyElement>) -> AnyElement {
        div()
            .id(id)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.inspector_scroll)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .px(px(14.0))
                    .pb(px(14.0))
                    .children(body),
            )
            .into_any_element()
    }

    /// The inspector of `key` (node `idx` of the forest).
    pub(crate) fn inspector(
        &mut self,
        key: NodeKey,
        idx: usize,
        split: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (forest, detail, tab) = {
            let m = self.model.read(cx);
            (m.forest().clone(), m.detail(&key), m.inspector_tab())
        };
        let node = forest.nodes[idx].clone();
        let av = available(&node);
        let this = cx.entity();
        let now = store::now_ms();
        let detail: NodeDetail = detail.map(|d| (*d).clone()).unwrap_or_default();

        // ---- top bar -------------------------------------------------------------------------
        let close = {
            let this = this.clone();
            IconButton::new("agentmap-close", &t, Icon::Close)
                .tooltip("Cerrar")
                .on_click(move |_, _, cx| this.update(cx, |v, cx| v.select(None, cx)))
        };
        let top = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .when(!split, |d| {
                d.child(button(
                    &this,
                    &t,
                    "agentmap-back",
                    "Volver al mapa",
                    true,
                    Box::new(|v, cx| v.select(None, cx)),
                ))
            })
            .when(split, |d| d.child(SectionLabel::new(&t, "Inspector")))
            .child(close);

        if let Some(ext) = node.external() {
            return self.external_inspector(
                &node,
                ext,
                top.into_any_element(),
                av,
                now,
                tab,
                &this,
                cx,
            );
        }

        // ---- title and status ----------------------------------------------------------------
        let title = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .child(div().pt(px(2.0)).child(lead_mark(&t, &node, 16.0)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(ty.ui_size_lg))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child(SharedString::from(node.title.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .when_some(status_mark(&t, node.status), |d, s| d.child(s))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .text_color(status_color(&t, node.status))
                            .child(node.status.label()),
                    )
                    .when_some(node.duration_ms(now), |d, ms| {
                        d.child(
                            div()
                                .text_size(px(ty.ui_size_sm))
                                .text_color(c.muted)
                                .child(SharedString::from(fmt::duration_ms(ms))),
                        )
                    })
                    .when_some(node.launcher.clone(), |d, l| {
                        d.child(chip(&t, format!("Lanzado por {}", l.to_uppercase())))
                    })
                    .when_some(node.profile.clone(), |d, p| {
                        d.child(chip(&t, format!("Perfil {p}")))
                    }),
            );

        // ---- facts ---------------------------------------------------------------------------
        let mut facts: Vec<AnyElement> = Vec::new();
        if !node.agent.is_empty() {
            facts.push(fact(&t, "Agente", agent_display(&node.agent)));
        }
        if let Some(m) = fmt::model_effort(node.model.as_deref(), node.effort.as_deref()) {
            facts.push(fact(&t, "Modelo", m));
        }
        if let Some(tok) = node.tokens.filter(|n| *n > 0) {
            facts.push(fact(
                &t,
                "Tokens",
                format!("{} facturables", fmt::tokens(tok)),
            ));
        }
        if let Some(cost) = node.cost_usd.filter(|n| *n > 0.0) {
            let exact = node
                .run
                .as_ref()
                .and_then(|r| r.result.as_ref())
                .and_then(|r| r.cost_exact);
            let suffix = match exact {
                Some(true) => "",
                Some(false) => " (estimado)",
                // an external agent has no run record: its cost always comes from the price table
                None if node.is_external() => " (estimado)",
                None => "",
            };
            facts.push(fact(&t, "Costo", format!("{}{suffix}", fmt::cost(cost))));
        }
        if let Some(w) = &node.worktree {
            facts.push(fact(&t, "Worktree", w.display().to_string()));
        }
        if let Some(n) = node
            .run
            .as_ref()
            .and_then(|r| r.auto_approved)
            .filter(|n| *n > 0)
        {
            facts.push(fact(&t, "Aprobaciones", fmt::auto_approved(n)));
        }
        let facts = section(&t, "Resumen", facts);

        // ---- limits --------------------------------------------------------------------------
        let limits = limit_rows(&node);
        let limits = (!limits.is_empty()).then(|| {
            section(
                &t,
                "Límites aplicados",
                limits.into_iter().map(|(l, v)| fact(&t, &l, v)).collect(),
            )
        });

        // ---- prompt --------------------------------------------------------------------------
        let prompt_body: Vec<AnyElement> = match &detail.prompt {
            Some(p) => {
                let mut v = vec![mono_block(&t, "agentmap-prompt", &p.text, 220.0)];
                let mut note = p.source.label().to_string();
                if p.redacted > 0 {
                    note.push_str(&format!(
                        " · incluye {} {} oculto{}",
                        p.redacted,
                        if p.redacted == 1 {
                            "secreto"
                        } else {
                            "secretos"
                        },
                        if p.redacted == 1 { "" } else { "s" }
                    ));
                }
                v.push(muted(&t, note));
                v
            }
            None => vec![muted(
                &t,
                if node.is_root() {
                    "Es el orquestador: no recibió este prompt de nadie."
                } else {
                    "Sin prompt registrado."
                },
            )],
        };
        let prompt = section(&t, "Prompt enviado por el orquestador", prompt_body);

        // ---- result and changes --------------------------------------------------------------
        let result = node.run.as_ref().and_then(|r| r.result.as_ref());
        let summary = result.and_then(|r| r.summary.clone().or_else(|| r.reason.clone()));
        let summary = summary.map(|s| {
            let mut body = vec![
                div()
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.text)
                    .child(SharedString::from(store::redact(&s).text))
                    .into_any_element(),
            ];
            for n in result.map(|r| r.notes.clone()).unwrap_or_default() {
                body.push(muted(&t, store::redact(&n).text));
            }
            section(&t, "Resultado", body)
        });

        let changes =
            (node.files > 0 || result.is_some_and(|r| r.diff_stat.is_some())).then(|| {
                let mut body: Vec<AnyElement> = Vec::new();
                if let Some(d) = result.and_then(|r| r.diff_stat.as_ref()) {
                    body.push(muted(
                        &t,
                        format!(
                            "{} · +{} −{}",
                            fmt::files(d.files.max(node.files)),
                            d.insertions,
                            d.deletions
                        ),
                    ));
                    if !d.text.trim().is_empty() {
                        body.push(mono_block(
                            &t,
                            "agentmap-diffstat",
                            d.text.trim_end(),
                            140.0,
                        ));
                    }
                } else {
                    body.push(muted(&t, fmt::files(node.files)));
                }
                if node.run.as_ref().is_some_and(|r| r.applied_ms.is_some()) {
                    body.push(muted(&t, "Los cambios ya se aplicaron al proyecto."));
                }
                section(&t, "Cambios", body)
            });

        // ---- approvals -----------------------------------------------------------------------
        let approvals = {
            let rows: Vec<AnyElement> = if detail.approvals.is_empty() {
                vec![muted(&t, "No pidió ninguna aprobación.")]
            } else {
                detail
                    .approvals
                    .iter()
                    .map(|a| {
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
                                    .text_size(px(ty.ui_size_sm))
                                    .text_color(c.text)
                                    .child(SharedString::from(if a.detail.is_empty() {
                                        a.tool.clone()
                                    } else {
                                        format!("{} · {}", a.tool, a.detail)
                                    })),
                            )
                            .child(chip(&t, decision_label(&a.decision)))
                            .into_any_element()
                    })
                    .collect()
            };
            section(&t, "Aprobaciones", rows)
        };

        // ---- confirmation strip --------------------------------------------------------------
        let confirm = self.confirm_strip(&node, &this);

        // ---- actions -------------------------------------------------------------------------
        let mut actions = div().flex().flex_row().flex_wrap().gap(px(8.0));
        let run_id = node.run_id();
        if av.open_thread
            && let NodeKey::Thread(id) = node.key.clone()
        {
            actions = actions.child(button(
                &this,
                &t,
                "agentmap-act-thread",
                "Abrir transcripción",
                false,
                Box::new(move |v, cx| v.emit_action(AgentMapEvent::OpenThread(id), cx)),
            ));
        }
        if av.diff
            && let Some(id) = run_id.clone()
        {
            actions = actions.child(button(
                &this,
                &t,
                "agentmap-act-diff",
                "Ver diff",
                false,
                Box::new(move |v, cx| v.emit_action(AgentMapEvent::OpenDiff(id.clone()), cx)),
            ));
        }
        if av.worktree
            && let Some(path) = node.worktree.clone()
        {
            actions = actions.child(button(
                &this,
                &t,
                "agentmap-act-worktree",
                "Abrir worktree",
                false,
                Box::new(move |v, cx| v.emit_action(AgentMapEvent::OpenWorktree(path.clone()), cx)),
            ));
        }
        if av.apply
            && let Some(id) = run_id.clone()
        {
            let this2 = this.clone();
            actions = actions.child(
                Button::action("agentmap-act-apply", &t, "Aplicar cambios")
                    .no_icon()
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        this2.update(cx, |v, cx| {
                            v.emit_action(AgentMapEvent::ApplyChanges(id), cx)
                        });
                    }),
            );
        }
        if av.cancel {
            let k = node.key.clone();
            actions = actions.child(button(
                &this,
                &t,
                "agentmap-act-cancel",
                "Cancelar",
                false,
                Box::new(move |v, cx| {
                    v.pending = Some(Pending::Cancel(k.clone()));
                    cx.notify();
                }),
            ));
        }
        if av.retry {
            let k = node.key.clone();
            let agent = node.agent.clone();
            actions = actions.child(button(
                &this,
                &t,
                "agentmap-act-retry",
                "Reintentar",
                false,
                Box::new(move |v, cx| {
                    v.pending = Some(Pending::Retry {
                        key: k.clone(),
                        agent: agent.clone(),
                    });
                    cx.notify();
                }),
            ));
        }
        if av.duplicate
            && let Some(id) = run_id
        {
            actions = actions.child(button(
                &this,
                &t,
                "agentmap-act-profile",
                "Duplicar como perfil",
                true,
                Box::new(move |v, cx| {
                    v.emit_action(AgentMapEvent::DuplicateAsProfile(id.clone()), cx)
                }),
            ));
        }

        let tabs = self.tab_bar(tab, false, &this);
        let body: AnyElement = match tab {
            InspectorTab::Summary => {
                let mut rows = vec![facts];
                rows.extend(limits);
                rows.extend(summary);
                rows.extend(changes);
                rows.push(approvals);
                rows.push(actions.into_any_element());
                self.scroll_tab("agentmap-inspector-summary", rows)
            }
            InspectorTab::Prompt => self.scroll_tab("agentmap-inspector-prompt", vec![prompt]),
            InspectorTab::Activity => self.activity_panel(&key, &node, cx),
        };
        self.inspector_shell(
            top.into_any_element(),
            title.into_any_element(),
            confirm,
            tabs,
            body,
        )
    }

    /// The frame of every inspector: header (close, title, confirmation, tabs) and the tab body,
    /// which takes the rest of the height.
    fn inspector_shell(
        &self,
        top: AnyElement,
        title: AnyElement,
        confirm: Option<AnyElement>,
        tabs: AnyElement,
        body: AnyElement,
    ) -> AnyElement {
        div()
            .id("agentmap-inspector")
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .px(px(14.0))
                    .pt(px(14.0))
                    .pb(px(10.0))
                    .child(top)
                    .child(title)
                    .children(confirm)
                    .child(tabs),
            )
            .child(div().flex_1().min_h(px(0.0)).child(body))
            .into_any_element()
    }

    /// Inspector for an agent CLI launched directly outside the Selachi harness.
    #[allow(clippy::too_many_arguments)]
    fn external_inspector(
        &mut self,
        node: &MapNode,
        ext: &crate::tree::ExternalProc,
        top: AnyElement,
        av: Available,
        now: i64,
        tab: InspectorTab,
        this: &Entity<AgentMapView>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;

        let title = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .child(div().pt(px(2.0)).child(lead_mark(&t, node, 16.0)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(ty.ui_size_lg))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child(SharedString::from(node.title.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .when_some(status_mark(&t, node.status), |d, s| d.child(s))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .text_color(node_status_color(&t, node))
                            .child(SharedString::from(ext.status_label())),
                    )
                    .when_some(node.duration_ms(now), |d, ms| {
                        d.child(
                            div()
                                .text_size(px(ty.ui_size_sm))
                                .text_color(c.muted)
                                .child(SharedString::from(fmt::duration_ms(ms))),
                        )
                    })
                    .child(external_badge(&t)),
            );

        let confirm = self.confirm_strip(node, this);

        let notice = div()
            .p(px(10.0))
            .rounded(px(Radius::CONTROL))
            .bg(c.surface_1)
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .child(EXTERNAL_EXPLANATION);

        let facts = section(
            &t,
            "Resumen",
            external_facts(node, now)
                .into_iter()
                .map(|(l, v)| fact(&t, &l, v))
                .collect(),
        );

        let cmd_body = if ext.cmdline_redacted.trim().is_empty() {
            vec![muted(&t, "Línea de comandos no disponible.")]
        } else {
            vec![
                mono_block(&t, "agentmap-ext-cmd", &ext.cmdline_redacted, 160.0),
                muted(&t, "Línea de comandos redactada."),
            ]
        };
        let command = section(&t, "Comando", cmd_body);

        let mut actions = div().flex().flex_row().flex_wrap().gap(px(8.0));
        if av.stop_external {
            let k = node.key.clone();
            let pid = ext.pid;
            actions = actions.child(button(
                this,
                &t,
                "agentmap-act-stop-external",
                "Detener",
                false,
                Box::new(move |v, cx| {
                    v.pending = Some(Pending::StopExternal {
                        key: k.clone(),
                        pid,
                    });
                    cx.notify();
                }),
            ));
        }
        if av.open_in_processes {
            let pid = ext.pid;
            actions = actions.child(button(
                this,
                &t,
                "agentmap-act-processes",
                "Abrir en Procesos",
                false,
                Box::new(move |v, cx| v.emit_action(AgentMapEvent::OpenInProcesses(pid), cx)),
            ));
        }
        if av.copy_command {
            let cmd = ext.cmdline_redacted.clone();
            actions = actions.child(button(
                this,
                &t,
                "agentmap-act-copy-cmd",
                "Copiar comando",
                true,
                Box::new(move |v, cx| v.emit_action(AgentMapEvent::CopyCommand(cmd.clone()), cx)),
            ));
        }

        let tabs = self.tab_bar(tab, true, this);
        let body: AnyElement = match tab {
            InspectorTab::Activity => self.activity_panel(&node.key, node, cx),
            _ => self.scroll_tab(
                "agentmap-inspector-summary",
                vec![
                    notice.into_any_element(),
                    facts,
                    command,
                    actions.into_any_element(),
                ],
            ),
        };
        self.inspector_shell(top, title.into_any_element(), confirm, tabs, body)
    }

    /// The inline confirmation of a pending cancel / retry (only when it is about `node`).
    fn confirm_strip(&self, node: &MapNode, this: &Entity<AgentMapView>) -> Option<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let strip = |body: Vec<AnyElement>| {
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .p(px(12.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_3)
                .children(body)
                .into_any_element()
        };
        let text = |s: &'static str| {
            div()
                .text_size(px(ty.ui_size_md))
                .text_color(c.text)
                .child(s)
                .into_any_element()
        };
        match self.pending.as_ref()? {
            Pending::Cancel(k) if *k == node.key => {
                let id = node.run_id()?;
                let back = button(
                    this,
                    &t,
                    "agentmap-confirm-cancel-back",
                    "Volver",
                    true,
                    Box::new(|v, cx| {
                        v.pending = None;
                        cx.notify();
                    }),
                );
                let go = button(
                    this,
                    &t,
                    "agentmap-confirm-cancel",
                    "Cancelar la ejecución",
                    false,
                    Box::new(move |v, cx| {
                        v.pending = None;
                        v.emit_action(AgentMapEvent::Cancel(id.clone()), cx);
                    }),
                );
                Some(strip(vec![
                    text(
                        "¿Cancelar esta ejecución? El agente se detiene y los cambios parciales quedan en su worktree.",
                    ),
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .child(go)
                        .child(back)
                        .into_any_element(),
                ]))
            }
            Pending::Retry { key, agent } if *key == node.key => {
                let id = node.run_id()?;
                let mut picks = div().flex().flex_row().flex_wrap().gap(px(6.0));
                for (aid, label) in RETRY_AGENTS {
                    let this = this.clone();
                    let k = key.clone();
                    picks = picks.child(
                        Pill::new(format!("agentmap-retry-pick-{aid}"), &t)
                            .agent(ui_kit::AgentMark::from_name(aid))
                            .label(label)
                            .active(aid == agent)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |v, cx| {
                                    v.pending = Some(Pending::Retry {
                                        key: k.clone(),
                                        agent: aid.to_string(),
                                    });
                                    cx.notify();
                                })
                            }),
                    );
                }
                let model = (*agent == node.agent).then(|| node.model.clone()).flatten();
                let chosen = agent.clone();
                let back = button(
                    this,
                    &t,
                    "agentmap-confirm-retry-back",
                    "Volver",
                    true,
                    Box::new(|v, cx| {
                        v.pending = None;
                        cx.notify();
                    }),
                );
                let go = button(
                    this,
                    &t,
                    "agentmap-confirm-retry",
                    "Reintentar",
                    false,
                    Box::new(move |v, cx| {
                        v.pending = None;
                        v.emit_action(
                            AgentMapEvent::Retry {
                                run_id: id.clone(),
                                agent: chosen.clone(),
                                model: model.clone(),
                            },
                            cx,
                        );
                    }),
                );
                Some(strip(vec![
                    text(
                        "Reintentar con el mismo prompt. El agente que elijas lo ejecuta en un worktree nuevo.",
                    ),
                    picks.into_any_element(),
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .child(go)
                        .child(back)
                        .into_any_element(),
                ]))
            }
            Pending::StopExternal { key, pid } if *key == node.key => {
                let pid = *pid;
                let back = button(
                    this,
                    &t,
                    "agentmap-confirm-stop-external-back",
                    "Volver",
                    true,
                    Box::new(|v, cx| {
                        v.pending = None;
                        cx.notify();
                    }),
                );
                let go = button(
                    this,
                    &t,
                    "agentmap-confirm-stop-external",
                    "Detener subárbol",
                    false,
                    Box::new(move |v, cx| {
                        v.pending = None;
                        v.emit_action(AgentMapEvent::StopExternal(pid), cx);
                    }),
                );
                Some(strip(vec![
                    div()
                        .text_size(px(ty.ui_size_md))
                        .text_color(c.text)
                        .child(SharedString::from(format!(
                            "¿Detener el proceso PID {pid} y sus hijos? Solo se detiene este subárbol; la sesión padre sigue abierta."
                        )))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .child(go)
                        .child(back)
                        .into_any_element(),
                ]))
            }
            _ => None,
        }
    }
}
