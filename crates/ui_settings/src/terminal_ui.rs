//! Terminal section: shell profiles, default shell, scrollback, cursor, clipboard and completion.

use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use store::config::{CursorStyle, PredictionSource, PredictionView, Profile, TabMode};
use ui_kit::{Button, GhostButton};

use crate::fields::commit_env;
use crate::ui::{self, note, plain_row, row};
use crate::validate::join_args;
use crate::view::{ProfileEditor, SettingsView};

impl SettingsView {
    pub(crate) fn terminal(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let term = self.cfg.terminal.clone();

        // Default shell: automatic + every profile.
        let mut shells = vec![(String::new(), "Automático".to_string())];
        shells.extend(self.cfg.profiles.iter().map(|p| (p.name.clone(), p.name.clone())));
        let default_shell = self.select(cx, "default-shell", shells, &term.default_profile, |s, v, cx| {
            let v = v.to_string();
            s.edit_cfg(cx, |c| c.terminal.default_profile = v);
        });

        let scrollback = self.input(cx, "scrollback", "10000", 120.0, true);
        let cursor_ix = match term.cursor_style {
            CursorStyle::Block => 0,
            CursorStyle::Bar => 1,
            CursorStyle::Underline => 2,
        };
        let cursor = self.segmented(cx, "cursor", ["Bloque", "Barra", "Subrayado"], cursor_ix, |s, ix, cx| {
            let v = [CursorStyle::Block, CursorStyle::Bar, CursorStyle::Underline][ix.min(2)];
            s.edit_cfg(cx, |c| c.terminal.cursor_style = v);
        });
        let copy_on_select = self.toggle(cx, "copy-on-select", term.copy_on_select, |s, on, cx| {
            s.edit_cfg(cx, |c| c.terminal.copy_on_select = on);
        });
        let paste = self.toggle(cx, "paste-ctrl-v", term.paste_ctrl_v, |s, on, cx| {
            s.edit_cfg(cx, |c| c.terminal.paste_ctrl_v = on);
        });

        let comp = term.completion.clone();
        let tab_ix = match comp.tab {
            TabMode::Smart => 0,
            TabMode::Complete => 1,
            TabMode::Menu => 2,
            TabMode::Native => 3,
        };
        let tab = self.segmented(cx, "tab-mode", ["Inteligente", "Completar", "Menú", "Nativo"], tab_ix, |s, ix, cx| {
            let v = [TabMode::Smart, TabMode::Complete, TabMode::Menu, TabMode::Native][ix.min(3)];
            s.edit_cfg(cx, |c| c.terminal.completion.tab = v);
        });
        let pred_value = match comp.prediction {
            PredictionSource::History => "history",
            PredictionSource::HistoryAndPlugin => "history_and_plugin",
            PredictionSource::Off => "off",
            PredictionSource::Native => "native",
        };
        let prediction = self.select(
            cx,
            "prediction",
            vec![
                ("history".into(), "Historial".into()),
                ("history_and_plugin".into(), "Historial y plugins".into()),
                ("off".into(), "Desactivada".into()),
                ("native".into(), "La del shell".into()),
            ],
            pred_value,
            |s, v, cx| {
                let p = match v {
                    "history_and_plugin" => PredictionSource::HistoryAndPlugin,
                    "off" => PredictionSource::Off,
                    "native" => PredictionSource::Native,
                    _ => PredictionSource::History,
                };
                s.edit_cfg(cx, |c| c.terminal.completion.prediction = p);
            },
        );
        let view_ix = match comp.prediction_view {
            PredictionView::Inline => 0,
            PredictionView::List => 1,
            PredictionView::Native => 2,
        };
        let pview = self.segmented(cx, "prediction-view", ["En línea", "Lista", "Nativa"], view_ix, |s, ix, cx| {
            let v = [PredictionView::Inline, PredictionView::List, PredictionView::Native][ix.min(2)];
            s.edit_cfg(cx, |c| c.terminal.completion.prediction_view = v);
        });
        let accept = self.input(cx, "accept_word_key", "Ctrl+RightArrow", 200.0, true);

        let profiles = self.profiles_group(narrow, cx);

        vec![
            profiles,
            ui::group(
                &t,
                Some("Shell"),
                vec![
                    row(
                        &t,
                        narrow,
                        "Shell por defecto",
                        Some("El que se abre con «Nueva pestaña». «Automático» detecta pwsh, PowerShell, cmd y WSL.".into()),
                        default_shell,
                        None,
                    )
                    .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Líneas de historial (scrollback)",
                        Some("Se guardan en memoria, nunca en disco. Máximo 1 000 000.".into()),
                        scrollback,
                        self.error_for("scrollback").or_else(|| self.error_for("terminal.scrollback")),
                    )
                    .into_any_element(),
                    row(&t, narrow, "Cursor", None, cursor, None).into_any_element(),
                    row(&t, narrow, "Copiar al seleccionar", Some("Copia el texto al soltar el ratón.".into()), copy_on_select, None)
                        .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Pegar con Ctrl+V",
                        Some("Si está apagado, Ctrl+V llega al programa (^V). Ctrl+Shift+V pega siempre.".into()),
                        paste,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            ui::group(
                &t,
                Some("Completado"),
                vec![
                    row(
                        &t,
                        narrow,
                        "Tecla Tab",
                        Some("«Inteligente» acepta la sugerencia en línea y, si no hay, abre el menú.".into()),
                        tab,
                        None,
                    )
                    .into_any_element(),
                    row(&t, narrow, "Predicción", Some("De dónde salen las sugerencias del shell.".into()), prediction, None)
                        .into_any_element(),
                    row(&t, narrow, "Vista de la predicción", None, pview, None).into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Aceptar una palabra",
                        Some("Tecla que acepta la siguiente palabra de la sugerencia. «native» deja la del shell.".into()),
                        accept,
                        self.error_for("accept_word_key")
                            .or_else(|| self.error_for("terminal.completion.accept_word_key")),
                    )
                    .into_any_element(),
                    ui::plain_row(note(&t, "Los cambios de completado se aplican a las pestañas nuevas.")).into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    fn profiles_group(&mut self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let add = GhostButton::new("add-profile", &t, "Añadir perfil").icon(ui_kit::Icon::Plus).on_click(
            cx.listener(|s, _, _, cx| s.add_profile(cx)),
        );
        let mut rows: Vec<AnyElement> = Vec::new();
        let count = self.cfg.profiles.len();
        if count == 0 {
            rows.push(
                plain_row(note(
                    &t,
                    "Sin perfiles propios: Selachi detecta pwsh, Windows PowerShell, cmd y WSL. Añade uno para fijar argumentos, carpeta o variables.",
                ))
                .into_any_element(),
            );
        }
        for i in 0..count {
            let p = self.cfg.profiles[i].clone();
            let editing = self.profile_editor.as_ref().is_some_and(|e| e.index == i);
            let summary = {
                let args = join_args(&p.args);
                if args.is_empty() { p.program.clone() } else { format!("{} {}", p.program, args) }
            };
            let buttons = div()
                .flex()
                .flex_row()
                .gap(px(4.0))
                .child(
                    GhostButton::new(SharedString::from(format!("edit-profile-{i}")), &t, if editing { "Hecho" } else { "Editar" })
                        .on_click(cx.listener(move |s, _, _, cx| s.toggle_profile_editor(i, cx))),
                )
                .child(
                    GhostButton::new(SharedString::from(format!("remove-profile-{i}")), &t, "Quitar")
                        .on_click(cx.listener(move |s, _, _, cx| s.remove_profile(i, cx))),
                );
            rows.push(row(&t, narrow, p.name.clone(), Some(SharedString::from(summary)), buttons, None).into_any_element());
            if editing {
                rows.push(self.profile_editor_card(i, narrow, cx));
            }
        }
        let _ = c;
        ui::group_with_action(&t, "Perfiles de shell", add, rows).into_any_element()
    }

    fn profile_editor_card(&mut self, i: usize, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let p: Profile = self.cfg.profiles[i].clone();
        let key = |k: &str| format!("profile.{i}.{k}");
        let name = self.profile_input(cx, &key("name"), &p.name, "Mi perfil", 260.0, false);
        let program = self.profile_input(cx, &key("program"), &p.program, "pwsh.exe", 260.0, true);
        let args = self.profile_input(cx, &key("args"), &join_args(&p.args), "-NoLogo", 260.0, true);
        let cwd = self.profile_input(cx, &key("cwd"), p.cwd.as_deref().unwrap_or(""), r"C:\Users\yo\code", 260.0, true);

        let env_rows: Vec<(String, String)> =
            self.profile_editor.as_ref().filter(|e| e.index == i).map(|e| e.env.clone()).unwrap_or_default();
        let mut env_elems: Vec<AnyElement> = Vec::new();
        for (j, (k, v)) in env_rows.iter().enumerate() {
            let kf = self.profile_input(cx, &format!("profile.{i}.env.{j}.k"), k, "NOMBRE", 150.0, true);
            let vf = self.profile_input(cx, &format!("profile.{i}.env.{j}.v"), v, "valor", 220.0, true);
            let err = self
                .error_for(&format!("profile.{i}.env.{j}.k"))
                .or_else(|| self.error_for(&format!("profile.{i}.env.{j}.v")));
            env_elems.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(kf)
                            .child(div().text_color(c.muted).child("="))
                            .child(vf)
                            .child(GhostButton::new(SharedString::from(format!("env-rm-{i}-{j}")), &t, "Quitar").on_click(
                                cx.listener(move |s, _, _, cx| s.remove_env(i, j, cx)),
                            )),
                    )
                    .when_some(err, |d, e| d.child(div().text_size(px(12.0)).text_color(c.error).child(e)))
                    .into_any_element(),
            );
        }
        let env_block = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(env_elems)
            .child(
                div().child(
                    Button::tonal(SharedString::from(format!("env-add-{i}")), &t, "Añadir variable")
                        .on_click(cx.listener(move |s, _, _, cx| s.add_env(i, cx))),
                ),
            );

        let field_row = |label: &'static str, control: AnyElement, err: Option<SharedString>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(c.muted).child(label))
                .child(control)
                .when_some(err, |d, e| d.child(div().text_size(px(12.0)).text_color(c.error).child(e)))
        };
        div()
            .mx(px(12.0))
            .mb(px(8.0))
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .rounded(px(12.0))
            .bg(c.surface_2)
            .child(field_row("Nombre", name, self.error_for(&key("name"))))
            .child(field_row("Programa", program, self.error_for(&key("program"))))
            .child(field_row("Argumentos", args, self.error_for(&key("args"))))
            .child(field_row("Carpeta inicial (opcional)", cwd, self.error_for(&key("cwd"))))
            .child(field_row("Variables de entorno", env_block.into_any_element(), self.error_for(&format!("profile.{i}"))))
            .when(narrow, |d| d)
            .into_any_element()
    }

    fn profile_input(
        &mut self,
        cx: &mut Context<Self>,
        key: &str,
        initial: &str,
        placeholder: &str,
        width: f32,
        mono: bool,
    ) -> AnyElement {
        let initial = initial.to_string();
        let field = self.field(key, move |_| initial, placeholder, mono, cx);
        let bad = self.errors.contains_key(key);
        field.update(cx, |f, cx| f.set_invalid(bad, cx));
        div().w(px(width)).max_w_full().flex().child(field).into_any_element()
    }

    fn add_profile(&mut self, cx: &mut Context<Self>) {
        let mut name = "Nuevo perfil".to_string();
        let mut n = 2;
        while self.cfg.profiles.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) {
            name = format!("Nuevo perfil {n}");
            n += 1;
        }
        let profile = Profile { name, program: "pwsh.exe".to_string(), ..Profile::default() };
        self.edit_cfg(cx, |c| c.profiles.push(profile));
        let i = self.cfg.profiles.len().saturating_sub(1);
        self.open_profile_editor(i, cx);
    }

    /// Open the inline editor of profile `i` (the "Editar" button).
    pub fn edit_profile(&mut self, i: usize, cx: &mut Context<Self>) {
        self.set_section(crate::view::Section::Terminal, cx);
        self.open_profile_editor(i, cx);
    }

    fn open_profile_editor(&mut self, i: usize, cx: &mut Context<Self>) {
        self.drop_profile_fields();
        let env = self.cfg.profiles.get(i).map(|p| p.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        self.profile_editor = Some(ProfileEditor { index: i, env });
        cx.notify();
    }

    fn toggle_profile_editor(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.profile_editor.as_ref().is_some_and(|e| e.index == i) {
            self.drop_profile_fields();
            self.profile_editor = None;
            // Errors of a half-typed field are dropped with it: the config never took the bad value.
            self.errors.retain(|k, _| !k.starts_with("profile"));
            cx.notify();
        } else {
            self.open_profile_editor(i, cx);
        }
    }

    fn remove_profile(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.cfg.profiles.len() {
            return;
        }
        let name = self.cfg.profiles[i].name.clone();
        self.edit_cfg(cx, move |c| {
            c.profiles.remove(i);
            if c.terminal.default_profile == name {
                c.terminal.default_profile.clear();
            }
        });
        self.drop_profile_fields();
        self.profile_editor = None;
        self.errors.retain(|k, _| !k.starts_with("profile"));
        cx.notify();
    }

    fn add_env(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(e) = self.profile_editor.as_mut().filter(|e| e.index == i) {
            e.env.push((String::new(), String::new()));
        }
        cx.notify();
    }

    fn remove_env(&mut self, i: usize, j: usize, cx: &mut Context<Self>) {
        if let Some(e) = self.profile_editor.as_mut().filter(|e| e.index == i)
            && j < e.env.len()
        {
            e.env.remove(j);
        }
        // Env field keys are positional: rebuild them from the draft.
        self.drop_env_fields(i);
        self.errors.retain(|k, _| !k.starts_with(&format!("profile.{i}.env")));
        commit_env(self, i, &format!("profile.{i}"), cx);
        cx.notify();
    }

    /// Forget every profile text field (they are rebuilt from the config on the next render).
    fn drop_profile_fields(&mut self) {
        self.fields.retain(|k, _| !k.starts_with("profile."));
        self.field_subs_retain(|k| !k.starts_with("profile."));
    }

    fn drop_env_fields(&mut self, i: usize) {
        let prefix = format!("profile.{i}.env.");
        self.fields.retain(|k, _| !k.starts_with(&prefix));
        self.field_subs_retain(|k| !k.starts_with(&prefix));
    }
}
