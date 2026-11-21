//! Root rendering: section list (or top menu when narrow), scrolling content column, and the
//! small control builders every section uses.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use ui_kit::{Icon, Placement, Popover, SegmentedControl, Toggle, icon};

use crate::ui::{self, MAX_CONTENT_WIDTH, NARROW_BELOW, NAV_WIDTH};
use crate::view::{CONTEXT, FocusNextField, FocusPrevField, Section, SettingsView};

impl SettingsView {
    /// A toggle wired to `f`.
    pub(crate) fn toggle(
        &self,
        cx: &mut Context<Self>,
        id: impl Into<SharedString>,
        on: bool,
        f: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
    ) -> Toggle {
        let this = cx.entity();
        Toggle::new(id, &self.theme).on(on).on_change(move |v, _, cx| {
            this.update(cx, |s, cx| f(s, v, cx));
        })
    }

    /// A segmented control wired to `f` (index of the picked segment).
    pub(crate) fn segmented<S: Into<SharedString>>(
        &self,
        cx: &mut Context<Self>,
        id: impl Into<SharedString>,
        labels: impl IntoIterator<Item = S>,
        selected: usize,
        f: impl Fn(&mut Self, usize, &mut Context<Self>) + 'static,
    ) -> SegmentedControl {
        let this = cx.entity();
        SegmentedControl::new(id, &self.theme, labels.into_iter().map(Into::into).collect::<Vec<SharedString>>())
            .selected(selected)
            .track(self.theme.colors.surface_2)
            .on_change(move |ix, _, cx| {
                this.update(cx, |s, cx| f(s, ix, cx));
            })
    }

    /// A drop-down: a tonal button with the current label that opens a list of `(value, label)`.
    pub(crate) fn select(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        options: Vec<(String, String)>,
        selected: &str,
        f: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let c = self.theme.colors;
        let this = cx.entity();
        let open = self.open_select == Some(id);
        let label = options
            .iter()
            .find(|(v, _)| v == selected)
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| selected.to_string());
        let f = Rc::new(f);
        let trigger = {
            let this = this.clone();
            div()
                .id(SharedString::from(format!("{id}-trigger")))
                .occlude()
                .h(px(32.0))
                .min_w(px(170.0))
                .px(px(12.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .rounded(px(10.0))
                .bg(if open { c.surface_4 } else { c.surface_3 })
                .text_color(c.text)
                .text_size(px(13.0))
                .cursor_pointer()
                .hover(move |s| s.bg(c.surface_4))
                .child(SharedString::from(label))
                .child(icon(Icon::ChevronDown, 14.0, c.muted))
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| {
                        s.open_select = if s.open_select == Some(id) { None } else { Some(id) };
                        cx.notify();
                    });
                })
        };
        let dismiss = {
            let this = this.clone();
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |s, cx| {
                    if s.open_select == Some(id) {
                        s.open_select = None;
                        cx.notify();
                    }
                });
            }
        };
        let items = options.into_iter().enumerate().map(|(ix, (value, text))| {
            let is_sel = value == selected;
            let this = this.clone();
            let f = f.clone();
            div()
                .id(SharedString::from(format!("{id}-opt-{ix}")))
                .h(px(32.0))
                .px(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(14.0))
                .rounded(px(8.0))
                .text_size(px(13.0))
                .text_color(if is_sel { c.text } else { c.muted })
                .cursor_pointer()
                .when(is_sel, |d| d.bg(c.surface_3))
                .hover(move |s| s.bg(c.surface_3).text_color(c.text))
                .child(SharedString::from(text))
                .when(is_sel, |d| d.child(icon(Icon::Check, 14.0, c.text)))
                .on_click(move |_, _, cx| {
                    let (this, f, value) = (this.clone(), f.clone(), value.clone());
                    this.update(cx, |s, cx| {
                        s.open_select = None;
                        f(s, &value, cx);
                        cx.notify();
                    });
                })
        });
        Popover::new(SharedString::from(format!("{id}-pop")), &self.theme)
            .open(open)
            .placement(Placement::BottomEnd)
            .padding(6.0)
            .sheet_when_narrow(false)
            .on_dismiss(dismiss)
            .trigger(trigger)
            .children(items)
            .into_any_element()
    }

    /// The text field `key` laid out at `width`.
    pub(crate) fn input(
        &mut self,
        cx: &mut Context<Self>,
        key: &str,
        placeholder: &str,
        width: f32,
        mono: bool,
    ) -> AnyElement {
        let field = self.field(key, |s| s.field_value(key).unwrap_or_default(), placeholder, mono, cx);
        let bad = self.errors.contains_key(key);
        field.update(cx, |f, cx| f.set_invalid(bad, cx));
        div().w(px(width)).max_w_full().flex().child(field).into_any_element()
    }

    pub(crate) fn error_for(&self, key: &str) -> Option<SharedString> {
        self.errors.get(key).map(|e| SharedString::from(e.clone()))
    }

    fn is_narrow(&self, window: &Window) -> bool {
        self.width_hint.unwrap_or(f32::from(window.viewport_size().width)) < NARROW_BELOW
    }

    fn render_section(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.section {
            Section::General => self.general(narrow, cx),
            Section::Appearance => self.appearance(narrow, cx),
            Section::Terminal => self.terminal(narrow, cx),
            Section::Agents => self.agents_section(narrow, cx),
            Section::Git => self.git(narrow, cx),
            Section::Processes => self.processes(narrow, cx),
            Section::Plugins => self.plugins_section(narrow, cx),
            Section::Keyboard => self.keyboard(narrow, cx),
            Section::About => self.about(narrow, cx),
        }
    }

    fn save_status(&self) -> (String, bool) {
        if let Some(e) = &self.save_error_text() {
            return (format!("No se pudo guardar: {e}"), true);
        }
        (self.save_label().to_string(), false)
    }

    fn nav(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.theme.colors;
        let this = cx.entity();
        let (status, is_err) = self.save_status();
        div()
            .w(px(NAV_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(12.0))
            .py(px(16.0))
            .child(
                div()
                    .px(px(10.0))
                    .pb(px(10.0))
                    .text_size(px(15.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.text)
                    .child("Ajustes"),
            )
            .children(Section::ALL.into_iter().map(|s| {
                let active = s == self.section;
                let this = this.clone();
                div()
                    .id(SharedString::from(format!("nav-{}", s.label())))
                    .occlude()
                    .h(px(34.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(10.0))
                    .text_size(px(13.0))
                    .text_color(if active { c.text } else { c.muted })
                    .cursor_pointer()
                    .when(active, |d| d.bg(c.surface_3))
                    .when(!active, |d| d.hover(move |s| s.bg(c.surface_2).text_color(c.text)))
                    .child(icon(s.icon(), 16.0, if active { c.text } else { c.muted }))
                    .child(s.label())
                    .on_click(move |_, _, cx| {
                        this.update(cx, |v, cx| v.set_section(s, cx));
                    })
            }))
            .child(div().flex_1())
            .child(
                div()
                    .px(px(10.0))
                    .text_size(px(11.0))
                    .text_color(if is_err { c.error } else { c.muted })
                    .child(SharedString::from(status)),
            )
            .into_any_element()
    }

    /// Top bar of the narrow layout: a menu with the current section and the save state.
    fn top_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.theme.colors;
        let this = cx.entity();
        let open = self.nav_open_flag();
        let (status, is_err) = self.save_status();
        let trigger = {
            let this = this.clone();
            div()
                .id("settings-menu")
                .occlude()
                .h(px(34.0))
                .px(px(12.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .rounded(px(10.0))
                .bg(if open { c.surface_4 } else { c.surface_3 })
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c.text)
                .cursor_pointer()
                .child(icon(self.section.icon(), 16.0, c.text))
                .child(self.section.label())
                .child(icon(Icon::ChevronDown, 14.0, c.muted))
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| {
                        s.toggle_nav();
                        cx.notify();
                    });
                })
        };
        let items = Section::ALL.into_iter().map(|s| {
            let active = s == self.section;
            let this = this.clone();
            div()
                .id(SharedString::from(format!("menu-{}", s.label())))
                .h(px(34.0))
                .min_w(px(200.0))
                .px(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .rounded(px(8.0))
                .text_size(px(13.0))
                .text_color(if active { c.text } else { c.muted })
                .cursor_pointer()
                .when(active, |d| d.bg(c.surface_3))
                .hover(move |st| st.bg(c.surface_3).text_color(c.text))
                .child(icon(s.icon(), 16.0, if active { c.text } else { c.muted }))
                .child(s.label())
                .on_click(move |_, _, cx| {
                    this.update(cx, |v, cx| v.set_section(s, cx));
                })
        });
        let dismiss = {
            let this = this.clone();
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |s, cx| {
                    if s.nav_open_flag() {
                        s.close_nav();
                        cx.notify();
                    }
                });
            }
        };
        div()
            .w_full()
            .flex_none()
            .px(px(16.0))
            .pt(px(12.0))
            .pb(px(4.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                Popover::new("settings-menu-pop", &self.theme)
                    .open(open)
                    .placement(Placement::BottomStart)
                    .padding(6.0)
                    .on_dismiss(dismiss)
                    .trigger(trigger)
                    .children(items),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(if is_err { c.error } else { c.muted })
                    .child(SharedString::from(status)),
            )
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let narrow = self.is_narrow(window);
        let blocks = self.render_section(narrow, cx);
        let title = ui::page_header(&t, self.section.label(), self.section.subtitle());
        let content = div()
            .id("settings-scroll")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div().w_full().flex().flex_col().items_center().child(
                    div()
                        .w_full()
                        .max_w(px(MAX_CONTENT_WIDTH))
                        .flex()
                        .flex_col()
                        .gap(px(20.0))
                        .px(px(if narrow { 16.0 } else { 32.0 }))
                        .pt(px(if narrow { 12.0 } else { 28.0 }))
                        .pb(px(40.0))
                        .child(title)
                        .children(blocks),
                ),
            );
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle_ref())
            .on_action(cx.listener(|_, _: &FocusNextField, window, _| window.focus_next()))
            .on_action(cx.listener(|_, _: &FocusPrevField, window, _| window.focus_prev()))
            .size_full()
            .flex()
            .when(narrow, |d| d.flex_col())
            .when(!narrow, |d| d.flex_row())
            .font_family(t.typography.ui_family)
            .text_size(px(t.typography.ui_size_md))
            .text_color(c.text)
            .child(if narrow { self.top_menu(cx) } else { self.nav(cx) })
            .child(content)
    }
}
