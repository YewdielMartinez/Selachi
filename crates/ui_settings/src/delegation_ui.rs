//! Agents -> Delegación: what other agents may do when a session delegates to them through
//! `selachi agent` / the MCP server (PLAN section 20), plus "Registrar en Claude Code / Antigravity".
//!
//! Everything lives in `[delegation]` of `config.toml` (`store::Delegation`); `selachi-agent` reads
//! the same section, so a change here applies to the next delegation without restarting anything.
//! The register buttons show the exact command and run it only after the user confirms.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use ui_kit::{AgentMark, Button, GhostButton, Placement, Popover};

use crate::registration::{McpTarget, RegPhase};
use crate::ui::{self, PillTone, row, status_pill};
use crate::view::SettingsView;

/// Agents a delegation can target: `(config id, label)`.
pub const DELEGATE_AGENTS: [(&str, &str); 5] = [
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("opencode", "OpenCode"),
    ("gemini", "Gemini"),
    ("agy", "Antigravity"),
];

/// The agents currently allowed (an empty list means all of them).
pub fn effective_allowed(list: &[String]) -> Vec<&'static str> {
    if list.is_empty() {
        return DELEGATE_AGENTS.iter().map(|(id, _)| *id).collect();
    }
    DELEGATE_AGENTS
        .iter()
        .filter(|(id, _)| list.iter().any(|a| a.eq_ignore_ascii_case(id)))
        .map(|(id, _)| *id)
        .collect()
}

/// The list to store after toggling `agent`: all of them is stored as the empty list, and the last
/// allowed agent cannot be switched off (`None`).
pub fn toggled_allowed(list: &[String], agent: &str) -> Option<Vec<String>> {
    let mut set = effective_allowed(list);
    if let Some(pos) = set.iter().position(|a| *a == agent) {
        set.remove(pos);
    } else if let Some((id, _)) = DELEGATE_AGENTS.iter().find(|(id, _)| *id == agent) {
        set.push(id);
    }
    if set.is_empty() {
        return None;
    }
    if set.len() == DELEGATE_AGENTS.len() {
        return Some(Vec::new());
    }
    // keep the catalog order
    Some(
        DELEGATE_AGENTS
            .iter()
            .filter(|(id, _)| set.contains(id))
            .map(|(id, _)| (*id).to_string())
            .collect(),
    )
}

/// Minutes as the field shows them (`15`, `7,5`).
pub fn minutes_text(secs: u64) -> String {
    if secs.is_multiple_of(60) {
        (secs / 60).to_string()
    } else {
        format!("{:.1}", secs as f64 / 60.0).replace('.', ",")
    }
}

impl SettingsView {
    /// The delegation groups of the Agents section.
    pub(crate) fn delegation_groups(
        &mut self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = self.theme;
        let d = self.cfg.delegation.clone();

        let enabled = self.toggle(cx, "deleg-enabled", d.enabled, |s, on, cx| {
            s.edit_cfg(cx, |c| c.delegation.enabled = on);
        });

        // Allowed agents: one chip each; all lit = all allowed.
        let allowed = effective_allowed(&d.allowed_agents);
        let mut chips = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for (id, label) in DELEGATE_AGENTS {
            let on = allowed.contains(&id);
            let this = cx.entity();
            chips = chips.child(
                ui_kit::Pill::new(SharedString::from(format!("deleg-agent-{id}")), &t)
                    .agent(AgentMark::from_name(id))
                    .label(label)
                    .active(on)
                    .on_click(move |_, _, cx| {
                        this.update(cx, |s, cx| {
                            if let Some(list) =
                                toggled_allowed(&s.cfg.delegation.allowed_agents, id)
                            {
                                s.edit_cfg(cx, |c| c.delegation.allowed_agents = list);
                            }
                        })
                    }),
            );
        }

        let perm_value = match d
            .max_permission
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .as_str()
        {
            "read_only" | "readonly" => "read_only",
            "ask" => "ask",
            _ => "accept_edits",
        };
        let max_permission = self.select(
            cx,
            "deleg-max-permission",
            vec![
                ("read_only".into(), "Solo lectura".into()),
                ("ask".into(), "Preguntar".into()),
                ("accept_edits".into(), "Aceptar ediciones".into()),
            ],
            perm_value,
            |s, v, cx| {
                let v = v.to_string();
                s.edit_cfg(cx, |c| c.delegation.max_permission = v);
            },
        );
        let depth = self.select(
            cx,
            "deleg-max-depth",
            vec![
                ("1".into(), "1 · solo el agente principal delega".into()),
                ("2".into(), "2 · el hijo puede delegar una vez más".into()),
            ],
            if d.max_depth >= 2 { "2" } else { "1" },
            |s, v, cx| {
                let n = if v == "2" { 2 } else { 1 };
                s.edit_cfg(cx, |c| c.delegation.max_depth = n);
            },
        );
        let concurrent = self.input(cx, "deleg_concurrent", "3", 80.0, true);
        let timeout = self.input(cx, "deleg_timeout", "15", 80.0, true);
        let budget = self.input(cx, "deleg_budget", "2000000", 140.0, true);
        let weekly = self.input(cx, "deleg_weekly", "90", 80.0, true);
        let approval = self.input(cx, "deleg_approval", "120", 80.0, true);

        let rules = ui::group(
            &t,
            Some("Delegación"),
            vec![
                row(
                    &t,
                    narrow,
                    "Permitir delegar",
                    Some("Un agente (Claude Code, Antigravity…) o `selachi agent run` puede encargar tareas a otros agentes a través de Selachi. Apagado: toda delegación se rechaza.".into()),
                    enabled,
                    None,
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Agentes permitidos",
                    Some("A quiénes se puede delegar. Todos encendidos = sin restricción.".into()),
                    chips,
                    None,
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Permiso máximo",
                    Some("Nunca más que el permiso de la sesión que delega. El acceso completo no se delega jamás.".into()),
                    max_permission,
                    self.error_for("delegation.max_permission"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Ejecuciones a la vez",
                    Some("Entre 1 y 16.".into()),
                    concurrent,
                    self.error_for("deleg_concurrent"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Profundidad máxima",
                    Some("Evita bucles: hasta dónde puede encadenarse una delegación.".into()),
                    depth,
                    self.error_for("delegation.max_depth"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Tiempo máximo (minutos)",
                    Some("Por ejecución; al agotarse se interrumpe al agente.".into()),
                    timeout,
                    self.error_for("deleg_timeout"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Presupuesto diario de tokens",
                    Some("Tokens facturables de todas las delegaciones juntas. 0 = sin límite.".into()),
                    budget,
                    self.error_for("deleg_budget"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Umbral de uso semanal (%)",
                    Some("No se delega en un agente cuyo límite semanal pasó de este porcentaje. 0 = no rechazar nunca.".into()),
                    weekly,
                    self.error_for("deleg_weekly"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Tiempo para responder una aprobación (s)",
                    Some("Si no contestas a lo que pide el agente hijo, se rechaza. Nunca se aprueba solo.".into()),
                    approval,
                    self.error_for("deleg_approval"),
                )
                .into_any_element(),
            ],
        )
        .into_any_element();

        let mut register_rows: Vec<AnyElement> = Vec::new();
        for target in McpTarget::ALL {
            register_rows.push(self.register_row(target, narrow, cx));
        }
        let register =
            ui::group(&t, Some("Registrar en otros agentes"), register_rows).into_any_element();
        vec![rules, register]
    }

    /// One "Registrar en …" row: the button opens a dialog with the exact command.
    fn register_row(
        &mut self,
        target: McpTarget,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let exe = self.agent_exe.clone();
        let phase = self
            .register
            .as_ref()
            .filter(|(tg, _)| *tg == target)
            .map(|(_, p)| p.clone());
        let last = self.register_results.get(&target).cloned();
        let id = target.id();

        let desc: SharedString = match (&exe, &phase, &last) {
            (None, _, _) => "No se encontró selachi-agent.exe junto a Selachi.".into(),
            (_, Some(RegPhase::Running), _) => "Registrando…".into(),
            (_, _, Some(Ok(msg))) => format!("Registrado. {msg}").into(),
            (_, _, Some(Err(e))) => format!("No se pudo registrar: {e}").into(),
            _ => format!(
                "Añade el servidor MCP `selachi` a {} para que pueda delegar tareas en otros agentes. Solo se ejecuta si confirmas.",
                target.label()
            )
            .into(),
        };

        let open = matches!(phase, Some(RegPhase::Confirm));
        let this = cx.entity();
        let trigger = {
            let this = this.clone();
            Button::tonal(
                SharedString::from(format!("register-{id}")),
                &t,
                format!("Registrar en {}", target.label()),
            )
            .on_click(move |_, _, cx| {
                this.update(cx, |s, cx| {
                    s.register = match &s.register {
                        Some((tg, RegPhase::Confirm)) if *tg == target => None,
                        _ => Some((target, RegPhase::Confirm)),
                    };
                    cx.notify();
                })
            })
        };

        let control: AnyElement = match exe {
            None => status_pill(&t, "No disponible", PillTone::Warn).into_any_element(),
            Some(exe) => {
                let command = target.display(&exe);
                let cancel = {
                    let this = this.clone();
                    GhostButton::new(
                        SharedString::from(format!("register-cancel-{id}")),
                        &t,
                        "Cancelar",
                    )
                    .on_click(move |_, _, cx| {
                        this.update(cx, |s, cx| {
                            s.register = None;
                            cx.notify();
                        })
                    })
                };
                let go = {
                    let this = this.clone();
                    Button::action(
                        SharedString::from(format!("register-go-{id}")),
                        &t,
                        "Registrar",
                    )
                    .no_icon()
                    .on_click(move |_, _, cx| {
                        this.update(cx, |s, cx| s.run_registration(target, cx))
                    })
                };
                let dismiss = {
                    move |_: &mut gpui::Window, cx: &mut gpui::App| {
                        this.update(cx, |s, cx| {
                            if matches!(s.register, Some((_, RegPhase::Confirm))) {
                                s.register = None;
                                cx.notify();
                            }
                        })
                    }
                };
                Popover::new(SharedString::from(format!("register-pop-{id}")), &t)
                    .open(open)
                    .placement(Placement::BottomEnd)
                    .width(420.0)
                    .padding(14.0)
                    .sheet_when_narrow(true)
                    .on_dismiss(dismiss)
                    .trigger(trigger)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(px(ty.ui_size_body))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(c.text)
                                    .child(SharedString::from(format!(
                                        "Registrar Selachi en {}",
                                        target.label()
                                    ))),
                            )
                            .child(
                                div()
                                    .text_size(px(ty.ui_size_sm))
                                    .text_color(c.muted)
                                    .child("Se ejecutará este comando:"),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("register-cmd-{id}")))
                                    .px(px(10.0))
                                    .py(px(8.0))
                                    .rounded(px(theme::Radius::CONTROL))
                                    .bg(c.surface_2)
                                    .font_family(ty.mono_family)
                                    .text_size(px(ty.ui_size_sm))
                                    .text_color(c.text)
                                    .child(SharedString::from(command)),
                            )
                            .child(
                                div()
                                    .text_size(px(ty.ui_size_sm))
                                    .text_color(c.muted)
                                    .child("Es un servidor MCP local por stdio (sin red). Las peticiones de los agentes hijos siempre te llegan a la cola de atención."),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_end()
                                    .gap(px(8.0))
                                    .child(cancel)
                                    .child(go),
                            ),
                    )
                    .into_any_element()
            }
        };

        row(
            &t,
            narrow,
            format!("Registrar en {}", target.label()),
            Some(desc),
            control,
            None,
        )
        .into_any_element()
    }

    /// The user confirmed: run it on the background executor and keep the result for the row.
    pub(crate) fn run_registration(&mut self, target: McpTarget, cx: &mut Context<Self>) {
        let Some(exe) = self.agent_exe.clone() else {
            return;
        };
        self.register = Some((target, RegPhase::Running));
        cx.notify();
        let runner = self.runner.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { runner.run(target, &exe) })
                .await;
            this.update(cx, |s, cx| {
                s.register = None;
                s.register_results.insert(target, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn an_empty_list_means_every_agent() {
        assert_eq!(effective_allowed(&[]).len(), 5);
        assert_eq!(
            effective_allowed(&v(&["codex", "AGY", "nope"])),
            vec!["codex", "agy"]
        );
    }

    #[test]
    fn toggling_keeps_the_list_meaningful() {
        // turning one off from "all" lists the other four
        let off = toggled_allowed(&[], "codex").unwrap();
        assert_eq!(off, v(&["claude", "opencode", "gemini", "agy"]));
        // turning it back on restores "all" as the empty list
        assert_eq!(
            toggled_allowed(&off, "codex").unwrap(),
            Vec::<String>::new()
        );
        // the last one cannot be switched off
        assert_eq!(toggled_allowed(&v(&["codex"]), "codex"), None);
        // order follows the catalog
        assert_eq!(
            toggled_allowed(&v(&["agy"]), "claude").unwrap(),
            v(&["claude", "agy"])
        );
    }

    #[test]
    fn minutes_read_naturally() {
        assert_eq!(minutes_text(900), "15");
        assert_eq!(minutes_text(450), "7,5");
        assert_eq!(minutes_text(60), "1");
    }
}
