//! Plugins section: language grammars from the verified index (install / update / remove).

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use theme::Radius;
use ui_kit::{Button, DitherMeter, GhostButton, Icon, Placement, Popover, Spinner};

use crate::fmt;
use crate::plugins::{Group, IndexState, Op, PluginRow, PluginState};
use crate::ui::{self, PillTone, banner, group, plain_row, row, status_pill};
use crate::view::{ConfirmKind, SettingsView};

impl SettingsView {
    pub(crate) fn plugins_section(
        &mut self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (rows, state, needs_restart) = {
            let m = self.plugins.read(cx);
            (m.rows(), m.index_state().clone(), m.needs_restart())
        };
        let mut blocks: Vec<AnyElement> = Vec::new();

        if needs_restart {
            blocks.push(
                banner(
                    &t,
                    Icon::Alert,
                    "Reinicia Selachi para completar",
                    "Hay plugins instalados, actualizados o quitados que se aplicarán al volver a abrir la aplicación.",
                    None,
                )
                .into_any_element(),
            );
        }

        let retry = |cx: &mut Context<Self>| {
            Button::tonal("plugins-retry", &t, "Reintentar").on_click(cx.listener(|s, _, _, cx| {
                s.plugins.update(cx, |m, cx| m.refresh(cx));
            }))
        };
        match &state {
            IndexState::Failed(e) => {
                blocks.push(
                    banner(
                        &t,
                        Icon::Alert,
                        "No se pudo cargar el índice de plugins",
                        &format!("{e} Los plugins ya instalados siguen funcionando."),
                        Some(retry(cx).into_any_element()),
                    )
                    .into_any_element(),
                );
            }
            IndexState::Loading | IndexState::Idle => {
                blocks.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(4.0))
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .child(Spinner::new(c.accent))
                        .child("Consultando el índice de plugins…")
                        .into_any_element(),
                );
            }
            IndexState::Ready => {}
        }

        blocks.push(
            ui::note(
                &t,
                "Un plugin es una gramática tree-sitter compilada (código nativo). Solo se instalan las del índice verificado por sha256 y siempre con tu confirmación.",
            )
            .into_any_element(),
        );

        for (grp, title) in [
            (Group::Installed, "Instalados"),
            (Group::Available, "Disponibles"),
            (Group::Builtin, "Incluidos"),
        ] {
            let part: Vec<&PluginRow> = rows.iter().filter(|r| r.group() == grp).collect();
            if part.is_empty() {
                continue;
            }
            let elems: Vec<AnyElement> = part
                .into_iter()
                .map(|r| self.plugin_row(r, narrow, cx))
                .collect();
            blocks.push(group(&t, Some(title), elems).into_any_element());
        }
        if rows.iter().all(|r| r.group() == Group::Builtin) && matches!(state, IndexState::Ready) {
            blocks.push(
                group(
                    &t,
                    Some("Disponibles"),
                    vec![
                        plain_row(ui::note(&t, "El índice no lista plugins todavía."))
                            .into_any_element(),
                    ],
                )
                .into_any_element(),
            );
        }

        let refresh = GhostButton::new("plugins-refresh", &t, "Actualizar lista").on_click(
            cx.listener(|s, _, _, cx| {
                s.plugins.update(cx, |m, cx| m.refresh(cx));
            }),
        );
        let url = self.input(
            cx,
            "index_url",
            store::config::DEFAULT_PLUGIN_INDEX_URL,
            360.0,
            true,
        );
        blocks.push(
            ui::group_with_action(
                &t,
                "Avanzado",
                refresh,
                vec![
                    row(
                        &t,
                        narrow,
                        "URL del índice",
                        Some("Solo https://. Vacío = el índice oficial de Selachi.".into()),
                        url,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        );
        blocks
    }

    fn plugin_row(&mut self, r: &PluginRow, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let highlighted = self.plugin_focus.as_deref() == Some(r.id.as_str());
        let failed =
            matches!(r.op, Some(Op::Failed(_))) || matches!(r.state, PluginState::Broken(_));
        let id = r.id.clone();

        let version = r
            .version
            .clone()
            .or_else(|| r.index_version.clone())
            .map(|v| {
                status_pill(
                    &t,
                    format!("v{}", v.trim_start_matches('v')),
                    PillTone::Neutral,
                )
            });
        let exts = if r.extensions.is_empty() {
            String::new()
        } else {
            r.extensions
                .iter()
                .take(4)
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let label_color = if failed { c.error } else { c.muted };

        // Progress meter while downloading.
        let progress: Option<AnyElement> = match &r.op {
            Some(Op::Downloading { received, total }) => {
                let v = total
                    .filter(|t| *t > 0)
                    .map(|t| (*received as f32 / t as f32).clamp(0.0, 1.0));
                Some(
                    DitherMeter::new(&t)
                        .segments(10)
                        .value(v)
                        .into_any_element(),
                )
            }
            Some(Op::Installing | Op::Removing) => Some(
                DitherMeter::new(&t)
                    .segments(10)
                    .value(Some(1.0))
                    .into_any_element(),
            ),
            _ => None,
        };

        let actions = self.plugin_actions(r, cx);

        let info = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.text)
                            .child(SharedString::from(r.name.clone())),
                    )
                    .children(version)
                    .when(!exts.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(ty.ui_size_xs))
                                .text_color(c.outline)
                                .child(SharedString::from(exts)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(ty.ui_size_sm))
                    .text_color(label_color)
                    .child(SharedString::from(r.label()))
                    .when(r.size > 0 && r.group() != Group::Builtin, |d| {
                        d.child(
                            div()
                                .text_color(c.outline)
                                .child(SharedString::from(fmt::bytes(r.size))),
                        )
                    })
                    .children(progress),
            );

        div()
            .id(SharedString::from(format!("plugin-{id}")))
            .flex()
            .mx(px(6.0))
            .px(px(10.0))
            .py(px(10.0))
            .gap(px(12.0))
            .rounded(px(Radius::SM))
            .when(narrow, |d| d.flex_col())
            .when(!narrow, |d| d.flex_row().items_center())
            .when(highlighted, |d| d.bg(c.surface_3))
            .child(info)
            .child(div().flex_none().child(actions))
            .into_any_element()
    }

    /// Buttons of a row, each with its confirmation popover.
    fn plugin_actions(&mut self, r: &PluginRow, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let mut buttons: Vec<AnyElement> = Vec::new();

        if matches!(r.op, Some(Op::Failed(_))) {
            let id = r.id.clone();
            buttons.push(
                GhostButton::new(
                    SharedString::from(format!("plugin-dismiss-{id}")),
                    &t,
                    "Descartar",
                )
                .on_click(cx.listener(move |s, _, _, cx| {
                    s.plugins.update(cx, |m, cx| m.dismiss_failure(&id, cx));
                }))
                .into_any_element(),
            );
        }
        if r.can_install() {
            let label = if matches!(r.state, PluginState::Broken(_))
                || matches!(r.op, Some(Op::Failed(_)))
            {
                "Reintentar"
            } else {
                "Instalar"
            };
            buttons.push(self.confirm_button(r, ConfirmKind::Install, label, cx));
        }
        if r.can_update() {
            buttons.push(self.confirm_button(r, ConfirmKind::Update, "Actualizar", cx));
        }
        if r.can_remove() {
            buttons.push(self.confirm_button(r, ConfirmKind::Remove, "Quitar", cx));
        }
        let _ = c;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .children(buttons)
            .into_any_element()
    }

    fn confirm_button(
        &mut self,
        r: &PluginRow,
        kind: ConfirmKind,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let id = r.id.clone();
        let open = self
            .confirm
            .as_ref()
            .is_some_and(|(cid, k)| *cid == r.id && *k == kind);
        let this = cx.entity();

        let trigger = {
            let (this, id) = (this.clone(), id.clone());
            let button = Button::tonal(
                SharedString::from(format!("plugin-{kind:?}-{id}")),
                &t,
                label,
            );
            button.on_click(move |_, _, cx| {
                let (this, id) = (this.clone(), id.clone());
                this.update(cx, |s, cx| {
                    let same = s
                        .confirm
                        .as_ref()
                        .is_some_and(|(cid, k)| *cid == id && *k == kind);
                    s.confirm = (!same).then_some((id, kind));
                    cx.notify();
                });
            })
        };

        let version = r
            .index_version
            .clone()
            .map(|v| format!("v{}", v.trim_start_matches('v')))
            .unwrap_or_default();
        let size = if r.size > 0 {
            fmt::bytes(r.size)
        } else {
            "tamaño desconocido".to_string()
        };
        let (title, body, confirm_label) = match kind {
            ConfirmKind::Install => (
                format!("Instalar {} {version}", r.name),
                format!(
                    "{size}. Es código nativo (una DLL) de un índice verificado (sha256): solo se instala si coincide con el hash fijado en el índice."
                ),
                "Instalar",
            ),
            ConfirmKind::Update => (
                format!("Actualizar {} a {version}", r.name),
                format!(
                    "{size}. Es código nativo de un índice verificado (sha256). Si el plugin ya está en uso, la versión nueva se activa al reiniciar."
                ),
                "Actualizar",
            ),
            ConfirmKind::Remove => (
                format!("Quitar {}", r.name),
                format!(
                    "Se borra el plugin ({}). El resaltado de sus archivos vuelve a texto plano; si está en uso, se quita al reiniciar.",
                    fmt::bytes(r.size)
                ),
                "Quitar",
            ),
        };

        let cancel = {
            let this = this.clone();
            GhostButton::new(
                SharedString::from(format!("plugin-cancel-{}", r.id)),
                &t,
                "Cancelar",
            )
            .on_click(move |_, _, cx| {
                this.update(cx, |s, cx| {
                    s.confirm = None;
                    cx.notify();
                });
            })
        };
        let go: AnyElement = {
            let this = this.clone();
            let on_click = move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                let id = id.clone();
                this.update(cx, |s, cx| {
                    s.confirm = None;
                    s.plugin_focus = None;
                    s.plugins.update(cx, |m, cx| match kind {
                        ConfirmKind::Install | ConfirmKind::Update => m.install(&id, cx),
                        ConfirmKind::Remove => m.uninstall(&id, cx),
                    });
                    cx.notify();
                });
            };
            match kind {
                // The one vivid action of the view: only one confirmation is open at a time.
                ConfirmKind::Install | ConfirmKind::Update => Button::action(
                    SharedString::from(format!("plugin-go-{}", r.id)),
                    &t,
                    confirm_label,
                )
                .no_icon()
                .on_click(on_click)
                .into_any_element(),
                ConfirmKind::Remove => Button::primary(
                    SharedString::from(format!("plugin-go-{}", r.id)),
                    &t,
                    confirm_label,
                )
                .on_click(on_click)
                .into_any_element(),
            }
        };

        let dismiss = {
            move |_: &mut gpui::Window, cx: &mut gpui::App| {
                this.update(cx, |s, cx| {
                    if s.confirm.is_some() {
                        s.confirm = None;
                        cx.notify();
                    }
                });
            }
        };
        Popover::new(
            SharedString::from(format!("plugin-pop-{kind:?}-{}", r.id)),
            &t,
        )
        .open(open)
        .placement(Placement::BottomEnd)
        .width(340.0)
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
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .child(SharedString::from(body)),
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
}
