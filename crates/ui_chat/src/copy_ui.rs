//! Copy and selection in the chat view (PLAN 21.1): selectable text per row, the hover action bar
//! of a message ("Copiar" and the `⋯` menu), "Copiar conversación" in the thread header, "Copiar
//! salida" for commands, the "Ocultar secretos al copiar" toggles and the "Copiado · incluye N
//! secretos ocultos" hint. Pure model in [`crate::copy`]; the selection engine is `ui_kit`'s.

use std::time::Duration;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, prelude::*, px,
};
use theme::Radius;
use ui_kit::{
    Button, Gap, Icon, IconButton, Placement, Popover, Segments, SelectableText, SelectionGroup,
    icon,
};

use crate::chat_view::ChatView;
use crate::copy::{
    CopyKind, CopySettings, conversation_markdown, copied_label, markdown_to_plain, prepare,
};
use crate::hub::ChatEvent;
use crate::rows::{ChatRow, RowKey, RowKind};
use crate::widgets::menu_row;

/// Which text a "Copiar" of a message produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMode {
    /// The original Markdown, fences included.
    Markdown,
    /// No syntax, no fences.
    Plain,
}

/// The line under the conversation after a copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Toast {
    pub text: String,
    /// Redaction removed something: shown in the warning tone.
    pub hidden: bool,
}

/// Which popover menu is open besides the composer's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CopyMenu {
    Message(RowKey),
    Thread,
}

impl ChatView {
    /// The selection group of `(row, sub)`, created on first use. Its copies go through
    /// [`Self::copy_out`] with the redaction policy of `kind`.
    pub(crate) fn group_for(
        &mut self,
        key: &RowKey,
        sub: u8,
        kind: CopyKind,
        cx: &mut Context<Self>,
    ) -> SelectionGroup {
        if let Some(g) = self.selections.get(&(key.clone(), sub)) {
            return g.clone();
        }
        // Not a tab stop: dozens of rows would make Tab useless. A click focuses the group, and
        // from there Ctrl+C / Ctrl+A work.
        let g = SelectionGroup::with_tab_stop(false, cx);
        let this = cx.entity().downgrade();
        g.on_copy(move |text, _, cx| {
            this.update(cx, |v, cx| v.copy_out(text, kind, cx)).ok();
        });
        self.selections.insert((key.clone(), sub), g.clone());
        g
    }

    /// `text` as one selectable block (a tool summary, a thought, a command output).
    pub(crate) fn selectable_block(
        &mut self,
        key: &RowKey,
        sub: u8,
        kind: CopyKind,
        text: &str,
        id: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let g = self.group_for(key, sub, kind, cx);
        let mut segs = Segments::new();
        let seg = segs.push(text.to_owned(), Gap::Block);
        g.set_segments(segs);
        g.container(
            gpui::ElementId::NamedChild(
                Box::new(gpui::ElementId::named_usize("sel-block", id)),
                SharedString::from(sub.to_string()),
            ),
            &self.theme,
        )
        .w_full()
        .min_w_0()
        .child(SelectableText::new(
            &g,
            seg,
            SharedString::from(text.to_owned()),
        ))
        .into_any_element()
    }

    fn settings(&self, cx: &App) -> CopySettings {
        self.hub.read(cx).copy_settings()
    }

    /// Writes `text` to the clipboard under the redaction policy of `kind` and tells the user.
    pub(crate) fn copy_out(&mut self, text: String, kind: CopyKind, cx: &mut Context<Self>) {
        let p = prepare(&text, kind, &self.settings(cx));
        cx.write_to_clipboard(ClipboardItem::new_string(p.text));
        self.show_toast(copied_label(p.hidden), p.hidden > 0, cx);
    }

    /// The hint line; it clears itself (one timer, no polling).
    pub(crate) fn show_toast(&mut self, text: String, hidden: bool, cx: &mut Context<Self>) {
        self.toast = Some(Toast { text, hidden });
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(3200))
                .await;
            this.update(cx, |v, cx| {
                v.toast = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn row_text(&self, key: &RowKey) -> Option<(String, bool)> {
        self.rows
            .iter()
            .find(|r| &r.key == key)
            .and_then(|r| match &r.kind {
                RowKind::Text { body, .. } => Some((body.text().to_string(), true)),
                RowKind::User { text, .. } => Some((text.clone(), false)),
                _ => None,
            })
    }

    /// "Copiar" / "Copiar como texto plano" / "Copiar como Markdown" of one message block.
    pub(crate) fn copy_message(&mut self, key: &RowKey, mode: CopyMode, cx: &mut Context<Self>) {
        let Some((text, markdown)) = self.row_text(key) else {
            return;
        };
        let text = match mode {
            CopyMode::Plain if markdown => markdown_to_plain(&text),
            _ => text,
        };
        self.copy_out(text, CopyKind::Text, cx);
        self.menu = None;
    }

    /// "Copiar salida" of a command row.
    pub(crate) fn copy_command_output(&mut self, key: &RowKey, cx: &mut Context<Self>) {
        let output = self
            .rows
            .iter()
            .find(|r| &r.key == key)
            .and_then(|r| match &r.kind {
                RowKind::Command { output, .. } => output.clone(),
                _ => None,
            });
        match output {
            Some(o) => self.copy_out(o, CopyKind::Command, cx),
            None => self.show_toast("Este comando no tiene salida".into(), false, cx),
        }
    }

    /// "Copiar conversación": the whole thread as Markdown with a heading per role.
    pub(crate) fn copy_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(rt) = self.runtime().cloned() else {
            return;
        };
        let settings = self.settings(cx);
        let p = rt.with_state(|st| conversation_markdown(&st.messages, &settings));
        self.menu = None;
        cx.write_to_clipboard(ClipboardItem::new_string(p.text));
        self.show_toast(copied_label(p.hidden), p.hidden > 0, cx);
    }

    pub(crate) fn toggle_redaction(&mut self, kind: CopyKind, cx: &mut Context<Self>) {
        let next = self.settings(cx).toggled(kind);
        self.hub.update(cx, |h, cx| {
            h.set_copy_settings(next, cx);
            // The host writes it back to `[chat]` in config.toml.
            h.emit_event(ChatEvent::CopySettingsChanged(next), cx);
        });
    }

    /// Hover tracking of the rows that have an action bar. Both the row and the bar report, so
    /// the bar survives the pointer moving onto it (a button hides its parent's hover).
    pub(crate) fn set_hover(&mut self, key: &RowKey, bar: bool, on: bool, cx: &mut Context<Self>) {
        let slot = if bar {
            &mut self.hover_bar
        } else {
            &mut self.hover_row
        };
        let want = on.then(|| key.clone());
        let changed = if on {
            *slot != want
        } else {
            slot.as_ref() == Some(key)
        };
        if changed {
            *slot = want;
            cx.notify();
        }
    }

    fn bar_visible(&self, key: &RowKey) -> bool {
        self.hover_row.as_ref() == Some(key)
            || self.hover_bar.as_ref() == Some(key)
            || self.menu == Some(CopyMenu::Message(key.clone()))
    }

    /// The floating "Copiar" / `⋯` bar of a message, only while the row is hovered or its menu
    /// is open (nothing is built otherwise).
    pub(crate) fn message_bar(
        &mut self,
        row: &ChatRow,
        id: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !matches!(row.kind, RowKind::Text { .. } | RowKind::User { .. }) {
            return None;
        }
        let key = row.key.clone();
        if !self.bar_visible(&key) {
            return None;
        }
        let t = self.theme;
        let c = t.colors;
        let settings = self.settings(cx);
        let open = self.menu == Some(CopyMenu::Message(key.clone()));
        let this = cx.entity();
        let k_copy = key.clone();
        let k_plain = key.clone();
        let k_md = key.clone();
        let k_open = key.clone();
        let k_hover = key;
        let menu_row_id = |name: &'static str| (name, id);
        let menu = Popover::new(format!("msg-pop-{id}"), &t)
            .open(open)
            .placement(Placement::BottomEnd)
            .width(260.0)
            .padding(6.0)
            .on_dismiss({
                move |_, cx| {
                    this.update(cx, |v, cx| {
                        v.menu = None;
                        cx.notify();
                    })
                }
            })
            .trigger(
                IconButton::new(format!("msg-more-{id}"), &t, Icon::Dots)
                    .size(24.0)
                    .tooltip("Más opciones")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let me = CopyMenu::Message(k_open.clone());
                        this.menu = if this.menu.as_ref() == Some(&me) {
                            None
                        } else {
                            Some(me)
                        };
                        cx.notify();
                    })),
            )
            .child(menu_row(
                menu_row_id("msg-plain"),
                &t,
                None,
                "Copiar como texto plano",
                None,
                false,
                true,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.copy_message(&k_plain, CopyMode::Plain, cx)
                }),
            ))
            .child(menu_row(
                menu_row_id("msg-md"),
                &t,
                None,
                "Copiar como Markdown",
                None,
                false,
                true,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.copy_message(&k_md, CopyMode::Markdown, cx)
                }),
            ))
            .child(menu_row(
                menu_row_id("msg-redact"),
                &t,
                None,
                "Ocultar secretos al copiar",
                Some(
                    "En el texto del chat; las salidas de comandos tienen su propio ajuste".into(),
                ),
                settings.redact_text,
                true,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_redaction(CopyKind::Text, cx)
                }),
            ));
        Some(
            div()
                .id(("msg-bar", id))
                .absolute()
                .top(px(-10.0))
                .right(px(4.0))
                .p(px(2.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_2)
                .border_1()
                .border_color(c.outline_soft)
                .occlude()
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    this.set_hover(&k_hover, true, *on, cx)
                }))
                .child(
                    Button::ghost(format!("msg-copy-{id}"), &t, "Copiar")
                        .tooltip("Copiar el mensaje (Markdown original)")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.copy_message(&k_copy, CopyMode::Markdown, cx)
                        })),
                )
                .child(menu)
                .into_any_element(),
        )
    }

    /// `⋯` of the thread header: "Copiar conversación" and the redaction toggles.
    pub(crate) fn thread_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let settings = self.settings(cx);
        let open = self.menu == Some(CopyMenu::Thread);
        let this = cx.entity();
        Popover::new("thread-pop", &t)
            .open(open)
            .placement(Placement::BottomEnd)
            .width(280.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| {
                this.update(cx, |v, cx| {
                    v.menu = None;
                    cx.notify();
                })
            })
            .trigger(
                IconButton::new("thread-more", &t, Icon::Dots)
                    .size(28.0)
                    .tooltip("Más opciones")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.menu = if this.menu == Some(CopyMenu::Thread) {
                            None
                        } else {
                            Some(CopyMenu::Thread)
                        };
                        cx.notify();
                    })),
            )
            .child(menu_row(
                "thread-copy",
                &t,
                None,
                "Copiar conversación",
                Some("Markdown, con un encabezado por rol".into()),
                false,
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| this.copy_conversation(cx)),
            ))
            .child(menu_row(
                "thread-redact-text",
                &t,
                None,
                "Ocultar secretos en el texto",
                None,
                settings.redact_text,
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.toggle_redaction(CopyKind::Text, cx)
                }),
            ))
            .child(menu_row(
                "thread-redact-cmd",
                &t,
                None,
                "Ocultar secretos en salidas de comandos",
                None,
                settings.redact_commands,
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.toggle_redaction(CopyKind::Command, cx)
                }),
            ))
            .into_any_element()
    }

    /// The "Copiar salida" / "Ocultar secretos" buttons of an expanded command.
    pub(crate) fn command_copy_buttons(
        &self,
        key: &RowKey,
        id: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = self.theme;
        let on = self.settings(cx).redact_commands;
        let k = key.clone();
        let copy = Button::ghost(format!("copy-out-{id}"), &t, "Copiar salida")
            .tooltip("Copia la salida del comando")
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.copy_command_output(&k, cx)),
            );
        let redact = Button::ghost(format!("redact-out-{id}"), &t, "Ocultar secretos")
            .when_icon(on)
            .tooltip(if on {
                "Al copiar, los secretos (tokens, claves) se ocultan. Clic para desactivar"
            } else {
                "Al copiar, la salida va tal cual. Clic para ocultar secretos"
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_redaction(CopyKind::Command, cx)
            }));
        vec![copy.into_any_element(), redact.into_any_element()]
    }

    /// The hint under the conversation after a copy.
    pub(crate) fn toast_line(&self) -> Option<AnyElement> {
        let toast = self.toast.clone()?;
        let t = self.theme;
        let c = t.colors;
        let tone = if toast.hidden { c.warning } else { c.muted };
        Some(
            div()
                .id("chat-toast")
                .w_full()
                .mb(px(6.0))
                .px(px(14.0))
                .py(px(7.0))
                .rounded(px(Radius::XS))
                .bg(c.surface_1)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.5))
                .text_color(tone)
                .child(icon(
                    if toast.hidden {
                        Icon::Alert
                    } else {
                        Icon::Check
                    },
                    16.0,
                    tone,
                ))
                .child(SharedString::from(toast.text))
                .into_any_element(),
        )
    }
}

/// `Button` has `.icon(Icon)`; this keeps the call site short: the check only while `on`.
trait WhenIcon {
    fn when_icon(self, on: bool) -> Self;
}

impl WhenIcon for Button {
    fn when_icon(self, on: bool) -> Self {
        if on { self.icon(Icon::Check) } else { self }
    }
}
