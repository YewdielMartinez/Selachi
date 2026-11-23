//! Root rendering: section list (or top menu when narrow), scrolling content column, and the
//! small control builders every section uses.

use std::rc::Rc;

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use ui_kit::{Pill, SegmentedControl, Select, SelectEvent, SelectOption, Toggle};

use crate::ui::{self, MAX_CONTENT_WIDTH, NARROW_BELOW, NAV_WIDTH};
use crate::view::{CONTEXT, FocusNextField, FocusPrevField, Section, SelectSlot, SettingsView};

/// Width of a drop-down trigger in a setting row.
const SELECT_WIDTH: f32 = 220.0;

fn select_options(options: &[(String, String)]) -> Vec<SelectOption> {
    options.iter().map(|(v, l)| SelectOption::new(v.clone(), l.clone())).collect()
}

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

    /// A drop-down (`ui_kit::Select`) over `(value, label)` options. The entity lives in
    /// [`SettingsView::selects`] under `id`; every render re-syncs its options, theme and
    /// selected value (each is a no-op, with no notification, while nothing moved).
    pub(crate) fn select(
        &mut self,
        cx: &mut Context<Self>,
        id: &'static str,
        options: Vec<(String, String)>,
        selected: &str,
        f: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = self.theme;
        let entity = match self.selects.get_mut(id) {
            Some(slot) => {
                if slot.options != options {
                    slot.options = options.clone();
                    slot.entity.update(cx, |s, cx| s.set_options(select_options(&options), cx));
                }
                slot.entity.update(cx, |s, cx| {
                    s.set_theme(theme, cx);
                    s.select_value(selected, cx);
                });
                slot.entity.clone()
            }
            None => {
                let entity = cx.new(|cx| {
                    Select::new(id, theme, cx).options(select_options(&options)).with_value(selected).width(SELECT_WIDTH)
                });
                let f = Rc::new(f);
                let sub = cx.subscribe(&entity, move |this, select, ev: &SelectEvent, cx| {
                    let SelectEvent::Changed(_) = ev;
                    if let Some(value) = select.read(cx).value().cloned() {
                        f(this, &value, cx);
                    }
                });
                self.selects.insert(id, SelectSlot { entity: entity.clone(), options, _sub: sub });
                entity
            }
        };
        entity.into_any_element()
    }

    /// Closes every open drop-down (a section change must not leave one hanging).
    pub(crate) fn close_selects(&mut self, cx: &mut Context<Self>) {
        for slot in self.selects.values() {
            slot.entity.update(cx, |s, cx| s.close(cx));
        }
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
        let message = self.errors.get(key).cloned();
        field.update(cx, |f, cx| f.set_error(message, cx));
        div().w(px(width)).max_w_full().flex().child(field).into_any_element()
    }

    pub(crate) fn error_for(&self, key: &str) -> Option<SharedString> {
        self.errors.get(key).map(|e| SharedString::from(e.clone()))
    }

    fn is_narrow(&self, window: &Window) -> bool {
        // An explicit hint wins; else the width this view measured on its zone; else the window.
        match self.width_hint {
            Some(w) => w < NARROW_BELOW,
            None => self.measured.narrow(window),
        }
    }

    fn render_section(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.section {
            Section::General => self.general(narrow, cx),
            Section::Appearance => self.appearance(narrow, cx),
            Section::Design => self.design(narrow, cx),
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
        let ty = self.theme.typography;
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
                    .text_size(px(ty.ui_size_lg))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.text)
                    .child("Ajustes"),
            )
            .children(Section::ALL.into_iter().map(|s| {
                let this = this.clone();
                div().w_full().flex().child(
                    Pill::new(format!("nav-{}", s.label()), &self.theme)
                        .icon(s.icon())
                        .label(s.label())
                        .active(s == self.section)
                        .on_click(move |_, _, cx| {
                            this.update(cx, |v, cx| v.set_section(s, cx));
                        }),
                )
            }))
            .child(div().flex_1())
            .child(
                div()
                    .px(px(10.0))
                    .text_size(px(ty.ui_size_xs))
                    .text_color(if is_err { c.error } else { c.muted })
                    .child(SharedString::from(status)),
            )
            .into_any_element()
    }

    /// Top bar of the narrow layout: a section drop-down and the save state.
    fn top_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let (status, is_err) = self.save_status();
        let options = Section::ALL.iter().map(|s| (s.label().to_string(), s.label().to_string())).collect();
        let sections = self.select(cx, "settings-section", options, self.section.label(), |s, v, cx| {
            if let Some(section) = Section::ALL.into_iter().find(|s| s.label() == v) {
                s.set_section(section, cx);
            }
        });
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
            .gap(px(12.0))
            .child(sections)
            .child(
                div()
                    .min_w_0()
                    .text_size(px(ty.ui_size_xs))
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
            .relative()
            .size_full()
            .flex()
            .when(narrow, |d| d.flex_col())
            .when(!narrow, |d| d.flex_row())
            .font_family(t.typography.ui_family)
            .text_size(px(t.typography.ui_size_md))
            .text_color(c.text)
            .child(self.measured.probe())
            .child(if narrow { self.top_menu(cx) } else { self.nav(cx) })
            .child(content)
    }
}
