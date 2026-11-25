//! Agents section: detected CLIs, default permission, Claude statusLine hook, usage options.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use store::config::{PermissionDefault, WeekStart};
use ui_kit::{GhostButton, Spinner, agent_icon};

use crate::agents::{AgentStatus, AgentsState, rows as agent_rows};
use crate::ui::{self, PillTone, group_with_action, plain_row, row, status_pill};
use crate::view::{AgentsTab, SettingsView};

const WEEK_DAYS: [(&str, &str, WeekStart); 7] = [
    ("monday", "Lunes", WeekStart::Monday),
    ("tuesday", "Martes", WeekStart::Tuesday),
    ("wednesday", "Miércoles", WeekStart::Wednesday),
    ("thursday", "Jueves", WeekStart::Thursday),
    ("friday", "Viernes", WeekStart::Friday),
    ("saturday", "Sábado", WeekStart::Saturday),
    ("sunday", "Domingo", WeekStart::Sunday),
];

impl SettingsView {
    /// Run `agents_driver::detect()` on the background executor (once per opening, or when
    /// forced by "Volver a comprobar").
    pub(crate) fn ensure_agents(&mut self, force: bool, cx: &mut Context<Self>) {
        if matches!(self.agents, AgentsState::Detecting)
            || (!force && matches!(self.agents, AgentsState::Done(_)))
        {
            return;
        }
        self.agents = AgentsState::Detecting;
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { agents_driver::detect() })
                .await;
            let rows = agent_rows(&found);
            let _ = this.update(cx, |s, cx| {
                s.agents = AgentsState::Done(rows);
                s.agents_task = None;
                cx.notify();
            });
        }));
    }

    pub(crate) fn agents_section(
        &mut self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;

        let tabs = self
            .segmented(
                cx,
                "agents-tab",
                ["Agentes y delegación", "Perfiles"],
                usize::from(self.agents_tab == AgentsTab::Profiles),
                |s, ix, cx| {
                    s.agents_tab = if ix == 1 {
                        AgentsTab::Profiles
                    } else {
                        AgentsTab::General
                    };
                    cx.notify();
                },
            )
            .into_any_element();
        if self.agents_tab == AgentsTab::Profiles {
            let editor = self.profile_editor(cx);
            let project = self.cwd.as_deref().map(crate::profiles::find_project_root);
            let limits = crate::profiles::Limits::from_delegation(&self.cfg.delegation);
            let theme = self.theme;
            editor.update(cx, |e, cx| {
                e.set_theme(theme, cx);
                e.set_narrow(narrow, cx);
                e.set_project(project, cx);
                e.set_limits(limits, cx);
            });
            return vec![tabs, editor.into_any_element()];
        }

        let recheck = GhostButton::new("agents-recheck", &t, "Volver a comprobar")
            .on_click(cx.listener(|s, _, _, cx| s.ensure_agents(true, cx)));
        let mut list: Vec<AnyElement> = Vec::new();
        match &self.agents {
            AgentsState::Unknown | AgentsState::Detecting => {
                list.push(
                    plain_row(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .text_color(c.muted)
                            .child(Spinner::new(c.accent))
                            .child("Buscando los CLI de agentes…"),
                    )
                    .into_any_element(),
                );
            }
            AgentsState::Done(rows) => {
                for r in rows {
                    let tone = match r.status {
                        AgentStatus::Ready => PillTone::Ok,
                        AgentStatus::Installed => PillTone::Neutral,
                        AgentStatus::LoginMissing => PillTone::Warn,
                        AgentStatus::NotInstalled => PillTone::Neutral,
                    };
                    let muted_out = r.status == AgentStatus::NotInstalled;
                    let detail = match (&r.version, &r.path) {
                        (Some(v), Some(p)) => format!("{v} · {}", p.display()),
                        (None, Some(p)) => p.display().to_string(),
                        _ => "No se encontró en el PATH.".to_string(),
                    };
                    list.push(
                        div()
                            .id(SharedString::from(format!("agent-{}", r.name)))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.0))
                            .w_full()
                            .min_w_0()
                            .px(px(16.0))
                            .py(px(10.0))
                            .child(agent_icon(
                                r.mark,
                                18.0,
                                if muted_out { c.outline } else { c.text },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .text_size(px(ty.ui_size_md))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if muted_out { c.muted } else { c.text })
                                            .child(SharedString::from(r.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(ty.ui_size_sm))
                                            .text_color(c.muted)
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .child(SharedString::from(detail)),
                                    ),
                            )
                            .child(status_pill(&t, r.status.label(), tone))
                            .into_any_element(),
                    );
                }
            }
        }
        let detected = group_with_action(&t, "CLI detectados", recheck, list);

        let perm_value = match self.cfg.agents.permission_default {
            PermissionDefault::Ask => "ask",
            PermissionDefault::AcceptEdits => "accept_edits",
            PermissionDefault::ReadOnly => "read_only",
            PermissionDefault::Full => "full",
        };
        let permission = self.select(
            cx,
            "permission",
            vec![
                ("ask".into(), "Preguntar".into()),
                ("accept_edits".into(), "Aceptar ediciones".into()),
                ("read_only".into(), "Solo lectura".into()),
                ("full".into(), "Acceso completo".into()),
            ],
            perm_value,
            |s, v, cx| {
                let p = match v {
                    "accept_edits" => PermissionDefault::AcceptEdits,
                    "read_only" => PermissionDefault::ReadOnly,
                    "full" => PermissionDefault::Full,
                    _ => PermissionDefault::Ask,
                };
                s.edit_cfg(cx, |c| c.agents.permission_default = p);
            },
        );
        let hook = self.toggle(
            cx,
            "statusline-hook",
            self.cfg.agents.claude_statusline_hook,
            |s, on, cx| {
                s.edit_cfg(cx, |c| c.agents.claude_statusline_hook = on);
            },
        );
        let week_value = WEEK_DAYS
            .iter()
            .find(|(_, _, w)| *w == self.cfg.agents.usage_week_start)
            .map_or("monday", |(id, _, _)| *id);
        let week = self.select(
            cx,
            "week-start",
            WEEK_DAYS
                .iter()
                .map(|(id, label, _)| ((*id).to_string(), (*label).to_string()))
                .collect(),
            week_value,
            |s, v, cx| {
                if let Some((_, _, w)) = WEEK_DAYS.iter().find(|(id, _, _)| *id == v) {
                    let w = *w;
                    s.edit_cfg(cx, |c| c.agents.usage_week_start = w);
                }
            },
        );
        let estimates = self.toggle(
            cx,
            "show-estimates",
            self.cfg.agents.show_estimates,
            |s, on, cx| {
                s.edit_cfg(cx, |c| c.agents.show_estimates = on);
            },
        );

        let delegation = self.delegation_groups(narrow, cx);
        let mut out = vec![
            tabs,
            detected.into_any_element(),
            ui::group(
                &t,
                Some("Permisos"),
                vec![
                    row(
                        &t,
                        narrow,
                        "Permiso por defecto",
                        Some("Para hilos nuevos. «Acceso completo» deja que el agente actúe sin pedir confirmación.".into()),
                        permission,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            ui::group(
                &t,
                Some("Uso"),
                vec![
                    row(
                        &t,
                        narrow,
                        "Hook statusLine de Claude",
                        Some(
                            "Opcional. Añade un hook statusLine a la configuración de Claude Code para leer el contexto y el coste exactos de cada sesión. Está apagado por defecto y se puede quitar apagándolo."
                                .into(),
                        ),
                        hook,
                        None,
                    )
                    .into_any_element(),
                    row(&t, narrow, "La semana empieza el", Some("Para el resumen semanal de uso.".into()), week, None)
                        .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Mostrar estimados",
                        Some("Cuando no hay datos oficiales, muestra una estimación marcada como tal. Nunca se inventa un porcentaje.".into()),
                        estimates,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ];
        out.extend(delegation);
        out
    }
}
