//! Auto-update and "Novedades" inside the shell (wave 6b.2).
//!
//! * The update **card**: "Actualización 0.2.0 lista · Reiniciar y actualizar", small, above the
//!   status bar, dismissible ("Más tarde") and with "Al cerrar" to install silently on exit.
//! * **Installing**: the app writes `update.installed_from` (and the release notes) to `state.db`,
//!   saves the session, and starts the Inno Setup installer with the documented flags; Setup closes
//!   this window through the Restart Manager and starts Selachi again.
//! * **Novedades**: the first start after an update shows the release notes once. Shown when the
//!   version that wrote `update.installed_from` is older than the running one and
//!   `whatsnew.seen_version` is not the running version; closing it records the version.

use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, Hsla, IntoElement, KeyDownEvent, ParentElement,
    SharedString, Styled, Window, div, prelude::*, px,
};
use store::db::{UPDATE_FROM, UPDATE_NOTES, WHATS_NEW_SEEN};
use theme::Radius;
use ui_kit::{ActionButton, Button, Card, CardTone, Icon, IconButton};
use updater::Version;

use super::AppShell;
use crate::hint::Hint;
use crate::update_host::{UpdateChanged, UpdateState, ready_text};

/// Version of the running build.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The "Novedades" screen.
pub(super) struct WhatsNew {
    pub version: String,
    pub parsed: ui_chat::ParsedMd,
    pub focus: FocusHandle,
}

/// What `state.db` remembers across the update (`update.pending_notes`).
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
pub struct PendingNotes {
    pub version: String,
    pub notes: String,
}

/// Should "Novedades" show? `from` is `update.installed_from`, `seen` is `whatsnew.seen_version`.
pub fn should_show_whats_new(from: Option<&str>, seen: Option<&str>, current: &str) -> bool {
    let (Some(from), Some(cur)) = (from.and_then(Version::parse), Version::parse(current)) else {
        return false;
    };
    seen.and_then(Version::parse).is_none_or(|s| s != cur) && cur > from
}

/// Notes shown when the release carried none.
pub fn fallback_notes(version: &str) -> String {
    format!("Selachi se actualizó a la versión {version}.")
}

impl AppShell {
    /// Starts the update schedule, follows its state and looks for a pending "Novedades".
    pub(super) fn init_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subs.push(
            cx.subscribe(&self.updates.clone(), |this, _, _: &UpdateChanged, cx| {
                this.sync_settings_update_status(cx);
                this.refresh(cx)
            }),
        );
        self.updates.update(cx, |u, cx| {
            u.cleanup_old();
            u.start(cx)
        });
        self.check_whats_new(window, cx);
    }

    /// `[updates]` changed in `config.toml` (or from Settings).
    pub(super) fn on_updates_config_changed(&mut self, cx: &mut Context<Self>) {
        let updates = self.config.updates.clone();
        self.updates
            .update(cx, |u, cx| u.apply_config(&updates, cx));
    }

    /// What General > Actualizaciones shows.
    pub(super) fn update_status_for_settings(&self, cx: &gpui::App) -> ui_settings::UpdateStatus {
        let host = self.updates.read(cx);
        ui_settings::UpdateStatus {
            text: crate::update_host::status_text(host.state(), host.current()),
            busy: matches!(
                host.state(),
                UpdateState::Checking | UpdateState::Downloading(_)
            ),
            ready: host.ready().is_some(),
        }
    }

    /// Pushes the update state to the open settings screen.
    fn sync_settings_update_status(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.settings_view() {
            let status = self.update_status_for_settings(cx);
            view.update(cx, |s, cx| s.set_update_status(status, cx));
        }
    }

    /// "Buscar ahora" (Settings).
    pub(crate) fn check_updates_now(&mut self, cx: &mut Context<Self>) {
        self.updates.update(cx, |u, cx| u.check_now(cx));
    }

    // ------------------------------------------------------------------------------------
    // Installing
    // ------------------------------------------------------------------------------------

    /// Writes what "Novedades" needs after the restart. Best effort: an update never fails over it.
    fn remember_update(&self, cx: &gpui::App) {
        let Some(db) = self.session.read(cx).db().cloned() else {
            return;
        };
        let Some(ready) = self.updates.read(cx).ready().cloned() else {
            return;
        };
        let notes = PendingNotes {
            version: ready.version.to_string(),
            notes: ready.notes,
        };
        let _ = db.meta_set(UPDATE_FROM, CURRENT_VERSION);
        if let Ok(json) = serde_json::to_string(&notes) {
            let _ = db.meta_set(UPDATE_NOTES, &json);
        }
    }

    /// "Reiniciar y actualizar": session saved, installer launched; Setup closes and restarts us.
    pub(crate) fn install_update_now(&mut self, cx: &mut Context<Self>) {
        let Some(ready) = self.updates.read(cx).ready().cloned() else {
            return;
        };
        self.remember_update(cx);
        self.save_session_now(true, cx);
        match updater::launch(&ready.staged.path, true) {
            Ok(()) => self.show_hint(
                Hint::text(format!("Instalando Selachi {}…", ready.version)),
                cx,
            ),
            Err(e) => self.show_hint(
                Hint::text(format!("No se pudo iniciar el instalador: {e}")),
                cx,
            ),
        }
    }

    /// The window is closing and the user chose "al cerrar": run the installer now (no restart).
    pub(super) fn install_update_on_exit(&mut self, cx: &mut Context<Self>) {
        let host = self.updates.read(cx);
        if !host.installs_on_exit() {
            return;
        }
        let Some(ready) = host.ready().cloned() else {
            return;
        };
        self.remember_update(cx);
        if let Err(e) = updater::launch(&ready.staged.path, false) {
            log::warn!("could not start the update installer: {e}");
        }
    }

    // ------------------------------------------------------------------------------------
    // The card
    // ------------------------------------------------------------------------------------

    /// The "update ready" card, centered above the status bar.
    pub(super) fn update_card(&self, bottom: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let host = self.updates.read(cx);
        let ready = host.visible_update()?;
        let on_exit = host.installs_on_exit();
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let (now, later, close, dismiss) = (cx.entity(), cx.entity(), cx.entity(), cx.entity());
        let text = if on_exit {
            format!("Selachi {} se instalará al cerrar", ready.version)
        } else {
            ready_text(&ready.version)
        };
        let mut card = div()
            .id("update-card")
            .occlude()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .py(px(6.0))
            .pl(px(14.0))
            .pr(px(6.0))
            .rounded(px(Radius::MD))
            .bg(c.surface_2)
            .border_1()
            .border_color(c.outline_soft)
            .text_size(px(ty.ui_size_md))
            .text_color(c.text)
            .child(div().min_w_0().child(SharedString::from(text)))
            .child(
                Button::tonal("update-now", &theme, "Reiniciar y actualizar")
                    .on_click(move |_, _, cx| now.update(cx, |s, cx| s.install_update_now(cx))),
            );
        if !on_exit {
            card = card.child(
                Button::ghost("update-on-exit", &theme, "Al cerrar").on_click(move |_, _, cx| {
                    later.update(cx, |s, cx| {
                        s.updates.update(cx, |u, cx| u.install_when_closing(cx))
                    })
                }),
            );
        }
        let _ = close;
        card = card.child(
            IconButton::new("update-dismiss", &theme, Icon::Close)
                .size(24.0)
                .icon_size(10.0)
                .tooltip("Más tarde")
                .on_click(move |_, _, cx| {
                    dismiss.update(cx, |s, cx| s.updates.update(cx, |u, cx| u.dismiss(cx)))
                }),
        );
        Some(
            div()
                .absolute()
                .bottom(px(bottom))
                .left_0()
                .right_0()
                .flex()
                .flex_row()
                .justify_center()
                .px(px(16.0))
                .child(card)
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------------------------
    // Novedades
    // ------------------------------------------------------------------------------------

    /// Reads `state.db` off the UI thread and opens "Novedades" when this is the first start after
    /// an update.
    fn check_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db) = self.session.read(cx).db().cloned() else {
            return;
        };
        let job = cx.background_executor().spawn(async move {
            let from = db.meta_get(UPDATE_FROM).ok().flatten();
            let seen = db.meta_get(WHATS_NEW_SEEN).ok().flatten();
            let notes = db.meta_get(UPDATE_NOTES).ok().flatten();
            (from, seen, notes)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (from, seen, notes) = job.await;
            if !should_show_whats_new(from.as_deref(), seen.as_deref(), CURRENT_VERSION) {
                return;
            }
            let notes: Option<PendingNotes> = notes.and_then(|j| serde_json::from_str(&j).ok());
            let text = notes
                .as_ref()
                .filter(|n| n.version == CURRENT_VERSION && !n.notes.trim().is_empty())
                .map_or_else(|| fallback_notes(CURRENT_VERSION), |n| n.notes.clone());
            this.update_in(cx, |shell, window, cx| {
                shell.show_whats_new(CURRENT_VERSION, &text, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Opens the screen with `notes` (Markdown).
    pub(super) fn show_whats_new(
        &mut self,
        version: &str,
        notes: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.whats_new = Some(WhatsNew {
            version: version.to_string(),
            parsed: ui_chat::parse_markdown(notes),
            focus,
        });
        self.refresh(cx);
    }

    /// Closing it records the version: it is shown once.
    pub(super) fn dismiss_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = self.whats_new.take() else {
            return;
        };
        if let Some(db) = self.session.read(cx).db().cloned() {
            cx.background_executor()
                .spawn(async move {
                    let _ = db.meta_set(WHATS_NEW_SEEN, &w.version);
                    let _ = db.meta_remove(UPDATE_FROM);
                    let _ = db.meta_remove(UPDATE_NOTES);
                })
                .detach();
        }
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
        self.refresh(cx);
    }

    /// The screen: a scrim, a card with the release notes and one action.
    pub(super) fn whats_new_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let w = self.whats_new.as_ref()?;
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let (scrim, done, keys) = (cx.entity(), cx.entity(), cx.entity());
        let notes = ui_chat::render_md(&w.parsed, theme, 9_000);
        Some(
            div()
                .id("whats-new")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.0,
                    a: 0.4,
                })
                .track_focus(&w.focus)
                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    scrim.update(cx, |s, cx| s.dismiss_whats_new(window, cx))
                })
                .on_key_down(move |e: &KeyDownEvent, window, cx| {
                    if matches!(e.keystroke.key.as_str(), "escape" | "enter") {
                        keys.update(cx, |s, cx| s.dismiss_whats_new(window, cx));
                    }
                })
                .child(
                    div()
                        .id("whats-new-card")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Card::new(&theme)
                                .tone(CardTone::High, &theme)
                                .radius(Radius::MD)
                                .p(px(20.0))
                                .w(px(520.0))
                                .max_w_full()
                                .flex()
                                .flex_col()
                                .gap(px(14.0))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        .child(
                                            div()
                                                .text_size(px(ty.ui_size_xl))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(c.text)
                                                .child("Novedades"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(ty.ui_size_sm))
                                                .text_color(c.muted)
                                                .child(SharedString::from(format!(
                                                    "Selachi {}",
                                                    w.version
                                                ))),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("whats-new-notes")
                                        .max_h(px(340.0))
                                        .overflow_y_scroll()
                                        .text_size(px(ty.ui_size_md))
                                        .text_color(c.text)
                                        .child(notes),
                                )
                                .child(
                                    div().flex().flex_row().justify_end().child(
                                        ActionButton::new("whats-new-done", &theme, "Entendido")
                                            .icon(Icon::Check)
                                            .on_click(move |_, window, cx| {
                                                done.update(cx, |s, cx| {
                                                    s.dismiss_whats_new(window, cx)
                                                })
                                            }),
                                    ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn novedades_shows_once_after_an_update() {
        // updated from 0.1.0 to 0.2.0 and never seen
        assert!(should_show_whats_new(Some("0.1.0"), None, "0.2.0"));
        // seen already
        assert!(!should_show_whats_new(
            Some("0.1.0"),
            Some("0.2.0"),
            "0.2.0"
        ));
        // a stale mark from an older update does not block the new one
        assert!(should_show_whats_new(Some("0.2.0"), Some("0.2.0"), "0.3.0"));
        // not an update (the installer was run by hand, no mark) or the same / an older build
        assert!(!should_show_whats_new(None, None, "0.2.0"));
        assert!(!should_show_whats_new(Some("0.2.0"), None, "0.2.0"));
        assert!(!should_show_whats_new(Some("0.3.0"), None, "0.2.0"));
        assert!(!should_show_whats_new(Some("garbage"), None, "0.2.0"));
    }

    #[test]
    fn pending_notes_round_trip() {
        let n = PendingNotes {
            version: "0.2.0".into(),
            notes: "## Novedades\n- a".into(),
        };
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(serde_json::from_str::<PendingNotes>(&json).unwrap(), n);
        assert_eq!(
            fallback_notes("0.2.0"),
            "Selachi se actualizó a la versión 0.2.0."
        );
    }
}
