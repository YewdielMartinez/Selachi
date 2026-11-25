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

/// Key of the add field of "Otros ejecutables de agente" (its error and text live under it).
pub(crate) const EXTRA_AGENT_FIELD: &str = "extra_agent_add";

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

/// `[delegation] approvals`: `(value, label, one-line explanation)`. A profile or a single run can
/// only lower the mode, never raise it.
pub const APPROVAL_MODES: [(&str, &str, &str); 3] = [
    ("ask", "Preguntar", "todo va a la cola de atención."),
    (
        "allowlist",
        "Solo comandos permitidos",
        "se aprueba lo que case con la lista del perfil, lo demás se pregunta.",
    ),
    ("auto", "No preguntar", "no se pregunta nada."),
];

/// The select value for a stored mode (anything unknown is the safe one).
pub fn approvals_value(stored: &str) -> &'static str {
    APPROVAL_MODES
        .iter()
        .find(|(v, _, _)| v.eq_ignore_ascii_case(stored.trim()))
        .map_or("ask", |(v, _, _)| v)
}

/// The text under the select: what each mode does, and what never changes.
pub fn approvals_help() -> String {
    let mut s = String::new();
    for (_, label, help) in APPROVAL_MODES {
        s.push_str(label);
        s.push_str(": ");
        s.push_str(help);
        s.push(' ');
    }
    s.push_str("git push, git commit, rm -rf fuera del proyecto y los archivos de credenciales se rechazan siempre; un perfil o una ejecución solo pueden bajarlo.");
    s
}

/// What the user typed in "Otros ejecutables de agente", checked the way
/// `proc_svc::extra_agent_stems` (what the map uses) will read it: `Ok` is the executable name to
/// store, `Err` the Spanish reason it was refused (a built-in agent, a shell or runtime that would
/// flag every such process, a duplicate, a full list).
pub fn check_extra_agent(text: &str, existing: &[String]) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Escribe el nombre del ejecutable, p. ej. aider.".into());
    }
    if text.chars().any(char::is_control) {
        return Err("Una sola línea, sin saltos.".into());
    }
    if text.chars().count() > store::config::MAX_EXTRA_AGENT_CHARS {
        return Err("Demasiado largo.".into());
    }
    let have = proc_svc::extra_agent_stems(existing);
    let Some(stem) = proc_svc::extra_agent_stems(&[text]).into_iter().next() else {
        let name = typed_stem(text);
        return Err(if name.is_empty() {
            "Nombre de ejecutable no válido.".to_string()
        } else if proc_svc::KNOWN_AGENTS.contains(&name.as_str()) {
            format!("«{name}» ya se detecta sin configurar nada.")
        } else {
            format!(
                "«{name}» es un shell, un runtime o un proceso del sistema: marcaría todo lo que se ejecute con él."
            )
        });
    };
    if have.contains(&stem) {
        return Err("Ya está en la lista.".into());
    }
    if have.len() >= proc_svc::MAX_EXTRA_AGENTS {
        return Err(format!(
            "Máximo {} ejecutables.",
            proc_svc::MAX_EXTRA_AGENTS
        ));
    }
    Ok(stem)
}

/// The executable name of what was typed (`acp:"C:\x\Foo.exe" --acp` -> `foo`), for the messages only.
fn typed_stem(text: &str) -> String {
    let t = text.trim();
    let t = t
        .strip_prefix("acp:")
        .or_else(|| t.strip_prefix("ACP:"))
        .unwrap_or(t)
        .trim();
    let first = if let Some(rest) = t.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        t.split_whitespace().next().unwrap_or("")
    };
    let seg = first.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com", ".ps1"] {
        if let Some(s) = seg.strip_suffix(ext) {
            return s.to_string();
        }
    }
    seg
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

        // Allowed agents: one chip each; all lit = all allowed. Placed below the label (stacked)
        // so the chip row has the full card width to wrap and never pushes past the card edge.
        let allowed = effective_allowed(&d.allowed_agents);
        let mut chips = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.0));
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
        let approvals = self.select(
            cx,
            "deleg-approvals",
            APPROVAL_MODES
                .iter()
                .map(|(v, l, _)| ((*v).to_string(), (*l).to_string()))
                .collect(),
            approvals_value(&d.approvals),
            |s, v, cx| {
                let v = v.to_string();
                s.edit_cfg(cx, |c| c.delegation.approvals = v);
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
        let extra_agents = self.extra_agents_control(cx);
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
                    true,
                    "Agentes permitidos",
                    Some("A quiénes se puede delegar. Todos encendidos = sin restricción.".into()),
                    chips,
                    None,
                )
                .into_any_element(),
                row(
                    &t,
                    true,
                    "Otros ejecutables de agente",
                    Some("Programas que el mapa debe marcar como «lanzado fuera de Selachi» además de claude, codex, opencode, gemini y agy (p. ej. aider). Los shells y runtimes como node o pwsh no se aceptan.".into()),
                    extra_agents,
                    self.error_for(EXTRA_AGENT_FIELD).or_else(|| self.error_for("delegation.extra_agents")),
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
                    "Aprobación de comandos",
                    Some(approvals_help().into()),
                    approvals,
                    self.error_for("delegation.approvals"),
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
                    Some("Si no contestas a lo que pide el agente hijo, se rechaza.".into()),
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

    /// "Otros ejecutables de agente": the configured names as removable chips and the add field
    /// (Enter adds). Validation is [`check_extra_agent`], the same reading the map applies.
    fn extra_agents_control(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let list = self.cfg.delegation.extra_agents.clone();
        let mut wrap = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.0));
        if !list.is_empty() {
            let mut chips = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0));
            for (i, item) in list.iter().enumerate() {
                let this = cx.entity();
                chips = chips.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .h(px(24.0))
                        .pl(px(9.0))
                        .pr(px(4.0))
                        .rounded(px(theme::Radius::CHIP))
                        .bg(c.surface_3)
                        .font_family(ty.mono_family)
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.text)
                        .child(SharedString::from(item.clone()))
                        .child(
                            div()
                                .id(SharedString::from(format!("extra-agent-x-{i}")))
                                .debug_selector({
                                    let id = format!("extra-agent-x-{i}");
                                    move || id.clone()
                                })
                                .size(px(16.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::Radius::CHIP))
                                .cursor_pointer()
                                .hover(|s| s.bg(c.surface_4))
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |s, cx| s.remove_extra_agent(i, cx))
                                })
                                .child(ui_kit::icon(ui_kit::Icon::Close, 12.0, c.muted)),
                        ),
                );
            }
            wrap = wrap.child(chips);
        }
        let add = self.input(
            cx,
            EXTRA_AGENT_FIELD,
            "Añadir ejecutable, p. ej. aider (Enter)",
            260.0,
            true,
        );
        wrap.child(add).into_any_element()
    }

    /// Enter in the add field.
    pub(crate) fn add_extra_agent(&mut self, cx: &mut Context<Self>) {
        let text = self.field_text(EXTRA_AGENT_FIELD, cx);
        if text.trim().is_empty() {
            return;
        }
        match check_extra_agent(&text, &self.cfg.delegation.extra_agents) {
            Ok(name) => {
                self.errors.remove(EXTRA_AGENT_FIELD);
                self.edit_cfg(cx, |c| c.delegation.extra_agents.push(name));
                if let Some(f) = self.fields.get(EXTRA_AGENT_FIELD).cloned() {
                    f.update(cx, |f, cx| f.set_text("", cx));
                }
                self.errors.remove(EXTRA_AGENT_FIELD);
            }
            Err(why) => {
                self.errors.insert(EXTRA_AGENT_FIELD.to_string(), why);
            }
        }
        self.sync_invalid(cx);
        cx.notify();
    }

    fn remove_extra_agent(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.cfg.delegation.extra_agents.len() {
            self.edit_cfg(cx, |c| {
                c.delegation.extra_agents.remove(i);
            });
        }
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
    fn approval_modes_have_spanish_labels_and_an_explanation_each() {
        let labels: Vec<_> = APPROVAL_MODES.iter().map(|(_, l, _)| *l).collect();
        assert_eq!(
            labels,
            ["Preguntar", "Solo comandos permitidos", "No preguntar"]
        );
        assert!(APPROVAL_MODES.iter().all(|(_, _, h)| h.ends_with('.')));
        // every value is one the store accepts
        for (v, _, _) in APPROVAL_MODES {
            assert!(store::config::APPROVAL_MODES.contains(&v));
        }
        assert_eq!(approvals_value("allowlist"), "allowlist");
        assert_eq!(approvals_value(" AUTO "), "auto");
        assert_eq!(approvals_value("???"), "ask", "unknown is the careful one");
        let help = approvals_help();
        assert!(help.contains("No preguntar: ") && help.contains("git push"));
    }


    #[test]
    fn extra_agents_follow_what_the_map_will_read() {
        // a plain name, a path, an acp command: all become the executable stem
        assert_eq!(check_extra_agent(" aider ", &[]).unwrap(), "aider");
        assert_eq!(
            check_extra_agent(r#"acp:"C:\tools\My-Agent.exe" --acp"#, &[]).unwrap(),
            "my-agent"
        );
        // shells and runtimes would flag every process of that kind
        for bad in ["node", "pwsh.exe", "python", "bash", "cmd"] {
            let e = check_extra_agent(bad, &[]).unwrap_err();
            assert!(e.contains("shell, un runtime"), "{bad}: {e}");
        }
        // the five built-in agents are already detected
        assert!(
            check_extra_agent("agy", &[])
                .unwrap_err()
                .contains("ya se detecta")
        );
        assert!(check_extra_agent("  ", &[]).is_err());
        assert!(check_extra_agent("a\nb", &[]).is_err());
        // duplicates (also when stored as an acp command) and the cap
        assert_eq!(
            check_extra_agent("aider", &v(&["Aider"])).unwrap_err(),
            "Ya está en la lista."
        );
        assert!(check_extra_agent("goose", &v(&["acp:goose --acp"])).is_err());
        let full: Vec<String> = (0..proc_svc::MAX_EXTRA_AGENTS)
            .map(|i| format!("tool{i}"))
            .collect();
        assert!(
            check_extra_agent("one-more", &full)
                .unwrap_err()
                .starts_with("Máximo")
        );
        // whatever is accepted is a valid `[delegation] extra_agents` entry
        let mut c = store::Config::default();
        c.delegation.extra_agents = vec![check_extra_agent("aider", &[]).unwrap()];
        assert!(c.validate().is_empty(), "{:?}", c.validate());
    }

    #[test]
    fn minutes_read_naturally() {
        assert_eq!(minutes_text(900), "15");
        assert_eq!(minutes_text(450), "7,5");
        assert_eq!(minutes_text(60), "1");
    }
}
