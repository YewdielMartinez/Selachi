//! Keyboard section: the action list, chord capture, conflict prompt and reset.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use theme::Radius;
use ui_kit::{Button, GhostButton};

use crate::keys::{self, ChordError};
use crate::ui::{self, PillTone, group_with_action, status_pill};
use crate::view::{Capture, SettingsView};

impl SettingsView {
    /// Start capturing a new chord for `action`. The next keystroke becomes the shortcut (Esc
    /// cancels). A window-level interceptor is alive only while capturing, so the app's own
    /// shortcuts (Ctrl+Tab, F11...) can be captured instead of firing.
    pub(crate) fn start_capture(&mut self, action: &str, cx: &mut Context<Self>) {
        self.cancel_capture();
        self.capture = Some(Capture {
            action: action.to_string(),
            conflict: None,
            error: None,
            captured: false,
        });
        let this = cx.weak_entity();
        self.capture_sub = Some(cx.intercept_keystrokes(move |ev, _window, cx| {
            let ks = &ev.keystroke;
            let m = &ks.modifiers;
            let (ctrl, alt, shift, win, key) =
                (m.control, m.alt, m.shift, m.platform, ks.key.clone());
            let handled = this
                .update(cx, |s, cx| {
                    s.on_capture_key(ctrl, alt, shift, win, &key, cx)
                })
                .unwrap_or(false);
            if handled {
                cx.stop_propagation();
            }
        }));
        cx.notify();
    }

    /// Start capturing a new shortcut for `action` (the "Cambiar" button).
    pub fn capture_shortcut(&mut self, action: &str, cx: &mut Context<Self>) {
        self.set_section(crate::view::Section::Keyboard, cx);
        self.start_capture(action, cx);
    }

    pub(crate) fn cancel_capture(&mut self) {
        self.capture = None;
        self.capture_sub = None;
    }

    /// A key was pressed while capturing. Returns whether it was consumed.
    pub(crate) fn on_capture_key(
        &mut self,
        ctrl: bool,
        alt: bool,
        shift: bool,
        win: bool,
        key: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cap) = self.capture.clone() else {
            return false;
        };
        if cap.captured {
            return false;
        }
        if key == "escape" && !(ctrl || alt || shift || win) {
            self.cancel_capture();
            cx.notify();
            return true;
        }
        match keys::chord_from_keystroke(ctrl, alt, shift, win, key) {
            Err(ChordError::ModifierOnly) => true, // keep waiting for the real key
            Err(e) => {
                if let Some(c) = self.capture.as_mut() {
                    c.error = Some(e.message().to_string());
                }
                cx.notify();
                true
            }
            Ok(chord) => {
                self.apply_chord(&cap.action, &chord, cx);
                true
            }
        }
    }

    fn apply_chord(&mut self, action: &str, chord: &str, cx: &mut Context<Self>) {
        let overrides = &self.cfg.keybindings;
        if let Some(other) = keys::find_conflict(&self.catalog, overrides, action, chord) {
            let other_id = other.id.clone();
            // Stop capturing (the chord is known); wait for Intercambiar / Cancelar.
            self.capture_sub = None;
            self.capture = Some(Capture {
                action: action.to_string(),
                conflict: Some((chord.to_string(), other_id)),
                error: None,
                captured: true,
            });
            cx.notify();
            return;
        }
        let (catalog, id, chord) = (self.catalog.clone(), action.to_string(), chord.to_string());
        self.edit_cfg(cx, move |c| {
            keys::set_binding(&catalog, &mut c.keybindings, &id, &chord)
        });
        self.cancel_capture();
        cx.notify();
    }

    fn resolve_conflict(&mut self, cx: &mut Context<Self>) {
        let Some(cap) = self.capture.clone() else {
            return;
        };
        if let Some((chord, other)) = cap.conflict {
            let catalog = self.catalog.clone();
            self.edit_cfg(cx, move |c| {
                keys::swap_bindings(&catalog, &mut c.keybindings, &cap.action, &other, &chord)
            });
        }
        self.cancel_capture();
        cx.notify();
    }

    fn reset_binding(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.edit_cfg(cx, move |c| {
            c.keybindings.remove(&id);
        });
        cx.notify();
    }

    fn reset_all_bindings(&mut self, cx: &mut Context<Self>) {
        self.edit_cfg(cx, |c| c.keybindings.clear());
        self.cancel_capture();
        cx.notify();
    }

    pub(crate) fn keyboard(&mut self, _narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let reset_all = GhostButton::new("keys-reset-all", &t, "Restablecer todo")
            .on_click(cx.listener(|s, _, _, cx| s.reset_all_bindings(cx)));
        let mut rows: Vec<AnyElement> = Vec::new();
        let catalog = self.catalog.clone();
        let (_, clashes) = keys::plan(&catalog, &self.cfg.keybindings);
        for a in &catalog {
            let chord = keys::effective(&catalog, &self.cfg.keybindings, &a.id);
            let overridden = keys::is_overridden(&self.cfg.keybindings, &a.id);
            let capturing = self
                .capture
                .as_ref()
                .filter(|cap| cap.action == a.id)
                .cloned();
            let id = a.id.clone();

            let chord_el: AnyElement = match &capturing {
                Some(cap) if !cap.captured => div()
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(Radius::CONTROL))
                    .border_2()
                    .border_color(c.accent)
                    .bg(c.surface_2)
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.text)
                    .child("Pulsa el atajo…  (Esc cancela)")
                    .into_any_element(),
                _ => status_pill(
                    &t,
                    if chord.is_empty() {
                        "Sin asignar".to_string()
                    } else {
                        keys::display_chord(&chord)
                    },
                    PillTone::Neutral,
                )
                .into_any_element(),
            };
            let change = {
                let id = id.clone();
                GhostButton::new(
                    SharedString::from(format!("key-change-{}", a.id)),
                    &t,
                    "Cambiar",
                )
                .on_click(cx.listener(move |s, _, _, cx| s.start_capture(&id, cx)))
            };
            let reset = overridden.then(|| {
                let id = id.clone();
                GhostButton::new(
                    SharedString::from(format!("key-reset-{}", a.id)),
                    &t,
                    "Restablecer",
                )
                .on_click(cx.listener(move |s, _, _, cx| s.reset_binding(&id, cx)))
            });

            let mut message: Option<AnyElement> =
                keys::clash_note(&catalog, &clashes, &a.id).map(|note| {
                    div()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.error)
                        .child(SharedString::from(note))
                        .into_any_element()
                });
            if let Some(cap) = &capturing {
                if let Some(e) = &cap.error {
                    message = Some(
                        div()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.error)
                            .child(SharedString::from(e.clone()))
                            .into_any_element(),
                    );
                }
                if let Some((chord, other)) = &cap.conflict {
                    let other_label = catalog
                        .iter()
                        .find(|x| &x.id == other)
                        .map_or(other.clone(), |x| x.label.clone());
                    message = Some(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(px(ty.ui_size_sm))
                                    .text_color(c.error)
                                    .child(SharedString::from(format!(
                                        "{} ya es el atajo de «{other_label}».",
                                        keys::display_chord(chord)
                                    ))),
                            )
                            .child(
                                Button::tonal("key-swap", &t, "Intercambiar")
                                    .on_click(cx.listener(|s, _, _, cx| s.resolve_conflict(cx))),
                            )
                            .child(GhostButton::new("key-cancel", &t, "Cancelar").on_click(
                                cx.listener(|s, _, _, cx| {
                                    s.cancel_capture();
                                    cx.notify();
                                }),
                            ))
                            .into_any_element(),
                    );
                }
            }

            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .w_full()
                    .px(px(16.0))
                    .py(px(10.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_size(px(ty.ui_size_md))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(a.label.clone())),
                            )
                            .child(chord_el)
                            .child(change)
                            .children(reset),
                    )
                    .children(message)
                    .into_any_element(),
            );
        }
        vec![
            group_with_action(&t, "Atajos", reset_all, rows).into_any_element(),
            ui::note(
                &t,
                "Solo se guardan los atajos que cambias, en [keybindings] de config.toml. Un atajo igual al original no se guarda.",
            )
            .into_any_element(),
        ]
    }
}
