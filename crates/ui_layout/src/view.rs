//! The gpui view of the layout panel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, Entity, FocusHandle, Focusable, IntoElement,
    Render, ScrollHandle, SharedString, Stateful, Subscription, Transformation, Window, div,
    prelude::*, px, radians, transparent_black,
};
use store::{
    BottomBar, BuiltinPreset, Density, GroupBy, LayoutConfig, PanelId, PresetId, RailVisibility,
    ResolvedLayout, SidebarSide, Splits, TabsPosition,
};
use theme::{Radius, Rgba, Theme};
use ui_kit::{
    Button, Card, Icon, Pill, SectionLabel, SegmentedControl, Slider, Toggle, Tooltip, icon,
};

use crate::state::{
    ALL_SECTIONS, LayoutState, Scope, builtin_description, builtin_name, section_label,
};
use crate::text_field::{TextField, TextFieldEvent};
use crate::wire;
use crate::{FocusNextField, FocusPrevField, PANEL_CONTEXT};

/// Width of the panel content in px.
pub const PANEL_WIDTH: f32 = 560.0;
const PAD: f32 = 14.0;
const CARD_W: f32 = 101.0;

/// Buttons the titlebar can show, in display order: `(id, label)`. `review` opens the review tab
/// and `layout` is this panel's own popover.
const TITLEBAR_BUTTONS: [(&str, &str); 7] = [
    ("git", "Git"),
    ("files", "Archivos"),
    ("procs", "Procesos"),
    ("usage", "Uso"),
    ("review", "Revisión"),
    ("attention", "Atención"),
    ("layout", "Layout"),
];

type ChangeFn = Rc<dyn Fn(LayoutConfig, &mut Window, &mut App) + 'static>;

/// A one-line status under the preset row (export / import results, validation errors).
struct Message {
    text: String,
    error: bool,
}

/// The layout customization panel. Build it with [`layout_panel`].
pub struct LayoutPanel {
    state: LayoutState,
    theme: Theme,
    on_change: ChangeFn,
    root_focus: FocusHandle,
    fields: HashMap<String, FocusHandle>,
    naming: Option<Entity<TextField>>,
    _naming_sub: Option<Subscription>,
    message: Option<Message>,
    scroll: ScrollHandle,
    /// The last input was the keyboard: only then are focus rings painted.
    kbd: bool,
    /// Child index of: header, presets, tabs, sidebar, zones, look in the scroll container.
    section_ix: [usize; 6],
    /// Hosted inside another screen (Settings > Diseño): full width, no card chrome, no own scroll.
    embedded: bool,
}

/// Create the panel.
///
/// * `initial`: the `[layout]` section of the config.
/// * `cwd`: working directory of the active terminal, enabling the "Este proyecto" scope.
/// * `on_change`: called (deferred, never re-entrantly) with the full new `LayoutConfig` after
///   every change. Sliders emit while dragging, so persist it with a debounce.
pub fn layout_panel(
    initial: LayoutConfig,
    cwd: Option<PathBuf>,
    theme: Theme,
    on_change: impl Fn(LayoutConfig, &mut Window, &mut App) + 'static,
    cx: &mut App,
) -> Entity<LayoutPanel> {
    cx.new(|cx| LayoutPanel {
        state: LayoutState::new(initial, cwd),
        theme,
        on_change: Rc::new(on_change),
        root_focus: cx.focus_handle(),
        fields: HashMap::new(),
        naming: None,
        _naming_sub: None,
        message: None,
        scroll: ScrollHandle::new(),
        kbd: false,
        section_ix: [0, 1, 2, 3, 4, 5],
        embedded: false,
    })
}

impl Focusable for LayoutPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root_focus.clone()
    }
}

impl LayoutPanel {
    /// Embedded mode (Settings > Diseño): the panel takes the full width of its host, drops the
    /// 560 px card (background, rounded corners) and its own scroll area (the host scrolls).
    pub fn set_embedded(&mut self, embedded: bool, cx: &mut Context<Self>) {
        if self.embedded != embedded {
            self.embedded = embedded;
            cx.notify();
        }
    }

    pub fn is_embedded(&self) -> bool {
        self.embedded
    }

    /// Follow a theme change.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        if let Some(n) = &self.naming {
            n.update(cx, |f, _| f.set_theme(theme));
        }
        cx.notify();
    }

    /// Follow a config hot reload. Identical configs (the echo of our own change) are ignored.
    pub fn set_config(&mut self, config: LayoutConfig, cx: &mut Context<Self>) {
        if *self.state.config() != config {
            self.state.set_config(config);
            cx.notify();
        }
    }

    /// The working directory of the active terminal changed.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>, cx: &mut Context<Self>) {
        self.state.set_cwd(cwd);
        cx.notify();
    }

    /// The layout currently shown (resolved for the scope being edited).
    pub fn layout(&self) -> ResolvedLayout {
        self.state.layout()
    }

    pub fn config(&self) -> &LayoutConfig {
        self.state.config()
    }

    pub fn scope(&self) -> Scope {
        self.state.scope()
    }

    /// Move the keyboard focus into the panel (call when the popover opens).
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.root_focus);
    }

    /// Scroll the section that holds the focused control into view (after Tab / Shift+Tab).
    fn reveal_focus(&self, window: &Window) {
        let Some(key) = self
            .fields
            .iter()
            .find(|(_, h)| h.is_focused(window))
            .map(|(k, _)| k.as_str())
        else {
            return;
        };
        let slot = match key {
            "scope" => 0,
            "presets" => 1,
            "tabs" => 2,
            "sidebar-side" | "sidebar-width" | "group-by" => 3,
            "rail" | "right-panel" | "bottom-bar" | "splits" => 4,
            _ => 5,
        };
        self.scroll.scroll_to_item(self.section_ix[slot]);
    }

    /// Scroll the panel to its end (screenshots, tests).
    pub fn scroll_to_end(&self) {
        self.scroll.scroll_to_bottom();
    }

    fn focus_for(&mut self, key: &str, cx: &mut Context<Self>) -> FocusHandle {
        self.fields
            .entry(key.to_string())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    // -------------------------------------------------------------------------------------
    // Actions
    // -------------------------------------------------------------------------------------

    fn emit(&mut self, changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if changed {
            let f = self.on_change.clone();
            let config = self.state.config().clone();
            window.defer(cx, move |window, cx| f(config, window, cx));
        }
        cx.notify();
    }

    /// Apply a programmatic change to the layout of the current scope (a palette command, a
    /// shortcut): same path as the controls, so it is diffed, validated and reported the same.
    pub fn edit_layout(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ResolvedLayout),
    ) {
        self.edit(window, cx, f);
    }

    fn edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ResolvedLayout),
    ) {
        self.message = None;
        let changed = self.state.apply(f);
        self.emit(changed, window, cx);
    }

    fn pick_preset(&mut self, id: PresetId, window: &mut Window, cx: &mut Context<Self>) {
        self.message = None;
        let changed = self.state.select_preset(id);
        self.emit(changed, window, cx);
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.message = None;
        let changed = self.state.reset();
        self.emit(changed, window, cx);
    }

    fn remove_override(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.message = None;
        let changed = self.state.remove_project_override();
        self.emit(changed, window, cx);
    }

    /// Switch between editing the global layout and this project's override.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.message = None;
        self.state.set_scope(scope);
        cx.notify();
    }

    fn start_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme;
        let field = cx.new(|cx| TextField::new(theme, "Nombre del preset", cx));
        self._naming_sub = Some(cx.subscribe_in(
            &field,
            window,
            |this, _, event: &TextFieldEvent, window, cx| match event {
                TextFieldEvent::Submit => this.confirm_naming(window, cx),
            },
        ));
        window.focus(&field.focus_handle(cx));
        self.naming = Some(field);
        self.message = None;
        cx.notify();
    }

    fn cancel_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        self._naming_sub = None;
        window.focus(&self.root_focus);
        cx.notify();
    }

    fn confirm_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = &self.naming else { return };
        let name = field.read(cx).text().to_string();
        match self.state.save_custom_preset(&name) {
            Ok(()) => {
                self.message = Some(Message {
                    text: format!("Preset \"{}\" guardado", name.trim()),
                    error: false,
                });
                self.naming = None;
                self._naming_sub = None;
                window.focus(&self.root_focus);
                self.emit(true, window, cx);
            }
            Err(e) => {
                self.message = Some(Message {
                    text: e,
                    error: true,
                });
                cx.notify();
            }
        }
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.state.export_toml()));
        self.message = Some(Message {
            text: "TOML del layout copiado al portapapeles".into(),
            error: false,
        });
        cx.notify();
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            self.message = Some(Message {
                text: "El portapapeles no tiene texto".into(),
                error: true,
            });
            cx.notify();
            return;
        };
        match self.state.import_toml(&text) {
            Ok(name) => {
                self.message = Some(Message {
                    text: format!("Preset \"{name}\" importado"),
                    error: false,
                });
                self.emit(true, window, cx);
            }
            Err(e) => {
                self.message = Some(Message {
                    text: e,
                    error: true,
                });
                cx.notify();
            }
        }
    }

    fn delete_preset(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let removed = self.state.delete_custom_preset(name);
        self.message = None;
        self.emit(removed, window, cx);
    }

    // -------------------------------------------------------------------------------------
    // Building blocks
    // -------------------------------------------------------------------------------------

    /// The accent focus ring: a 2 px border that is only painted while focused.
    fn ring(&self, el: Stateful<Div>, focused: bool, radius: f32) -> Stateful<Div> {
        let c = self.theme.colors;
        el.rounded(px(radius)).border_2().border_color(if focused {
            c.accent.into()
        } else {
            transparent_black()
        })
    }

    /// A labelled settings row: label on the left, control on the right (the shared
    /// [`ui_kit::SettingRow`]: the label wraps, the control never gets covered).
    fn row(&self, label: &str, control: impl IntoElement) -> Div {
        div().w_full().child(
            ui_kit::SettingRow::new(&self.theme, SharedString::from(label.to_string()), control)
                .padded(false)
                .min_height(36.0)
                .regular_label(),
        )
    }

    fn group(&self, title: &str, rows: Vec<AnyElement>) -> Div {
        let t = self.theme;
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .px(px(4.0))
                    .child(SectionLabel::new(&t, title.to_string())),
            )
            .child(
                Card::new(&t)
                    .px(px(14.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .children(rows),
            )
    }

    /// A focusable segmented control; `pick` receives the index of the chosen segment.
    #[allow(clippy::too_many_arguments)]
    fn seg_core(
        &mut self,
        key: &str,
        labels: Vec<SharedString>,
        selected: Option<usize>,
        track: Rgba,
        pick: impl Fn(&mut Self, usize, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let handle = self.focus_for(key, cx);
        let focused = self.kbd && handle.is_focused(window);
        let this = cx.entity();
        let pick = Rc::new(pick);
        let n = labels.len();
        let control = SegmentedControl::new(SharedString::from(key.to_string()), &t, labels)
            .track(track)
            .selected(selected.unwrap_or(usize::MAX))
            .on_change({
                let (this, pick) = (this.clone(), pick.clone());
                move |ix, window, cx| this.update(cx, |p, cx| pick(p, ix, window, cx))
            });
        self.ring(
            div().id(SharedString::from(format!("{key}-focus"))),
            focused,
            14.0,
        )
        .track_focus(&handle)
        .on_key_down(move |ev, window, cx| {
            let ix = match ev.keystroke.key.as_str() {
                "left" => selected.map_or(0, |s| s.saturating_sub(1)),
                "right" => selected.map_or(0, |s| (s + 1).min(n.saturating_sub(1))),
                _ => return,
            };
            cx.stop_propagation();
            if Some(ix) != selected {
                this.update(cx, |p, cx| pick(p, ix, window, cx));
            }
        })
        .child(control)
        .into_any_element()
    }

    /// A segmented control over enum values, applied with `set`.
    fn seg<T: Clone + PartialEq + 'static>(
        &mut self,
        key: &str,
        options: &[(T, &'static str)],
        current: Option<T>,
        set: fn(&mut ResolvedLayout, T),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let values: Vec<T> = options.iter().map(|o| o.0.clone()).collect();
        let labels = options.iter().map(|o| SharedString::from(o.1)).collect();
        let selected = current.and_then(|c| values.iter().position(|v| *v == c));
        self.seg_core(
            key,
            labels,
            selected,
            self.theme.colors.surface_2,
            move |p, ix, window, cx| {
                if let Some(v) = values.get(ix).cloned() {
                    p.edit(window, cx, |l| set(l, v));
                }
            },
            window,
            cx,
        )
    }

    fn toggle(
        &mut self,
        key: &str,
        value: bool,
        set: fn(&mut ResolvedLayout, bool),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let handle = self.focus_for(key, cx);
        let focused = self.kbd && handle.is_focused(window);
        let flip = {
            let this = cx.entity();
            move |v: bool, window: &mut Window, cx: &mut App| {
                this.update(cx, |p, cx| p.edit(window, cx, |l| set(l, v)));
            }
        };
        let on_key = flip.clone();
        self.ring(
            div().id(SharedString::from(format!("{key}-focus"))),
            focused,
            12.0,
        )
        .track_focus(&handle)
        .on_key_down(move |ev, window, cx| {
            if matches!(ev.keystroke.key.as_str(), "space" | "enter") {
                cx.stop_propagation();
                on_key(!value, window, cx);
            }
        })
        .p(px(2.0))
        .child(
            Toggle::new(SharedString::from(key.to_string()), &t)
                .on(value)
                .on_change(move |v, window, cx| flip(v, window, cx)),
        )
        .into_any_element()
    }

    /// A slider with a value label; `set` receives the rounded value.
    #[allow(clippy::too_many_arguments)]
    fn slider(
        &mut self,
        key: &str,
        (min, max, step): (f32, f32, f32),
        value: f32,
        default: f32,
        unit: &'static str,
        set: fn(&mut ResolvedLayout, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let handle = self.focus_for(key, cx);
        let focused = self.kbd && handle.is_focused(window);
        let apply = {
            let this = cx.entity();
            move |v: f32, window: &mut Window, cx: &mut App| {
                this.update(cx, |p, cx| p.edit(window, cx, |l| set(l, v)));
            }
        };
        let on_key = apply.clone();
        let ring = self
            .ring(
                div().id(SharedString::from(format!("{key}-focus"))),
                focused,
                12.0,
            )
            .track_focus(&handle)
            .on_key_down(move |ev, window, cx| {
                let big = if ev.keystroke.modifiers.shift {
                    5.0
                } else {
                    1.0
                };
                let v = match ev.keystroke.key.as_str() {
                    "left" | "down" => value - step * big,
                    "right" | "up" => value + step * big,
                    "home" => min,
                    "end" => max,
                    _ => return,
                };
                cx.stop_propagation();
                on_key(v.clamp(min, max), window, cx);
            });
        ring.p(px(2.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                Slider::new(SharedString::from(key.to_string()), &t)
                    .range(min, max)
                    .step(step)
                    .value(value)
                    .default_value(default)
                    .width(196.0)
                    .on_change(move |v, window, cx| apply(v, window, cx)),
            )
            .child(
                div()
                    .w(px(46.0))
                    .flex_none()
                    .text_size(px(t.typography.ui_size_sm))
                    .text_color(t.colors.muted)
                    .child(SharedString::from(format!(
                        "{} {unit}",
                        value.round() as i32
                    ))),
            )
            .into_any_element()
    }

    // -------------------------------------------------------------------------------------
    // Sections
    // -------------------------------------------------------------------------------------

    fn header(
        &mut self,
        layout: &ResolvedLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();

        let scope_control = self.state.project_name().map(|name| {
            let name: String = if name.chars().count() > 16 {
                name.chars().take(15).chain(['…']).collect()
            } else {
                name
            };
            let labels = vec![
                SharedString::from("Global"),
                SharedString::from(format!("Este proyecto ({name})")),
            ];
            let selected = Some(usize::from(self.state.scope() == Scope::Project));
            self.seg_core(
                "scope",
                labels,
                selected,
                t.colors.surface_1,
                |p, ix, _, cx| {
                    p.set_scope(
                        if ix == 1 {
                            Scope::Project
                        } else {
                            Scope::Global
                        },
                        cx,
                    )
                },
                window,
                cx,
            )
        });

        let override_row =
            (self.state.cwd().is_some() && self.state.has_project_override()).then(|| {
                let this = this.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(Pill::compact("override-pill", &t).label("Override activo"))
                    .child(
                        Button::ghost("override-remove", &t, "Quitar override").on_click(
                            move |_, window, cx| {
                                this.update(cx, |p, cx| p.remove_override(window, cx));
                            },
                        ),
                    )
            });

        let reset = Button::tonal("reset", &t, "Restablecer preset")
            .disabled(!self.state.is_modified())
            .on_click(move |_, window, cx| this.update(cx, |p, cx| p.reset(window, cx)));

        let left = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(t.typography.ui_size_lg))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Diseño"),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(c.muted)
                            .child("Los cambios se aplican al instante"),
                    ),
            )
            .children(scope_control.map(|ctl| div().flex().flex_row().child(ctl)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(reset)
                    .children(override_row),
            );

        let preview = div()
            .flex_none()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(6.0))
            .child(wire::wireframe(&t, layout, wire::PREVIEW).selected(true))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(c.muted)
                    .child("Vista previa"),
            );

        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(16.0))
            .child(left)
            .child(preview)
    }

    fn notes(&self) -> Option<Div> {
        let t = self.theme;
        let c = t.colors;
        let warnings = self.state.warnings();
        if warnings.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .px(px(4.0))
                .children(warnings.iter().map(|w| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(icon(Icon::Alert, 14.0, c.muted))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(c.muted)
                                .child(SharedString::from(w.clone())),
                        )
                })),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn preset_card(
        &self,
        ix: usize,
        id: PresetId,
        name: String,
        description: &'static str,
        layout: &ResolvedLayout,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let custom_name = matches!(id, PresetId::Custom(_)).then(|| name.clone());
        let pick = id;
        let mut card = div()
            .id(("preset", ix))
            .relative()
            .w(px(CARD_W))
            .flex_none()
            .p(px(3.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded(px(Radius::SM))
            .bg(if selected { c.surface_3 } else { c.surface_1 })
            .border_2()
            .border_color(if selected {
                c.accent.into()
            } else {
                transparent_black()
            })
            .cursor_pointer()
            .when(!selected, |d| d.hover(move |s| s.bg(c.surface_3)))
            .child(wire::wireframe(&t, layout, wire::THUMB).selected(selected))
            .child(
                div()
                    .px(px(4.0))
                    .pb(px(4.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(t.typography.ui_size_md))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c.text)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .min_h(px(42.0))
                            .text_size(px(11.0))
                            .text_color(c.muted)
                            .child(description),
                    ),
            )
            .on_click({
                let this = this.clone();
                move |_, window, cx| {
                    let id = pick.clone();
                    this.update(cx, |p, cx| p.pick_preset(id, window, cx));
                }
            });
        if let Some(n) = custom_name {
            let del = div()
                .id(("preset-del", ix))
                .occlude()
                .absolute()
                .top(px(8.0))
                .right(px(8.0))
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .bg(c.surface_3)
                .cursor_pointer()
                .hover(move |s| s.bg(c.surface_4))
                .tooltip(Tooltip::text(&t, "Eliminar preset"))
                .child(icon(Icon::Close, 12.0, c.muted))
                .on_click(move |_, window, cx| {
                    let n = n.clone();
                    this.update(cx, |p, cx| p.delete_preset(&n, window, cx));
                });
            card = card.child(del);
        }
        card
    }

    fn presets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let mut entries: Vec<(PresetId, String, &'static str, ResolvedLayout)> = BuiltinPreset::ALL
            .iter()
            .map(|b| {
                let mut l = ResolvedLayout::builtin(*b);
                let _ = l.normalize();
                (
                    PresetId::Builtin(*b),
                    builtin_name(*b).to_string(),
                    builtin_description(*b),
                    l,
                )
            })
            .collect();
        for p in &self.state.config().custom_presets {
            let mut l = p.layout.clone();
            let _ = l.normalize();
            entries.push((PresetId::parse(&p.name), p.name.clone(), "Preset propio", l));
        }
        let active = self.state.active_preset();
        let ids: Vec<PresetId> = entries.iter().map(|e| e.0.clone()).collect();
        let active_ix = active
            .as_ref()
            .and_then(|a| ids.iter().position(|i| i == a));

        let handle = self.focus_for("presets", cx);
        let focused = self.kbd && handle.is_focused(window);
        let this = cx.entity();
        let n = ids.len();
        let cards: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(ix, (id, name, desc, layout))| {
                let selected = Some(&id) == active.as_ref();
                self.preset_card(ix, id, name, desc, &layout, selected, cx)
                    .into_any_element()
            })
            .collect();

        let grid = self
            .ring(div().id("presets-focus"), focused, 16.0)
            .track_focus(&handle)
            .on_key_down(move |ev, window, cx| {
                let ix = match ev.keystroke.key.as_str() {
                    "left" | "up" => active_ix.map_or(0, |s| s.saturating_sub(1)),
                    "right" | "down" => active_ix.map_or(0, |s| (s + 1).min(n.saturating_sub(1))),
                    _ => return,
                };
                cx.stop_propagation();
                if let Some(id) = ids.get(ix).cloned()
                    && Some(ix) != active_ix
                {
                    this.update(cx, |p, cx| p.pick_preset(id, window, cx));
                }
            })
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(5.0))
            .children(cards);

        let actions = self.preset_actions(window, cx);
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().px(px(4.0)).child(SectionLabel::new(&t, "Presets")))
            .child(grid)
            .child(actions);
        if let Some(m) = &self.message {
            let c = t.colors;
            col = col.child(
                div()
                    .px(px(4.0))
                    .text_size(px(11.0))
                    .text_color(if m.error { c.error } else { c.muted })
                    .child(SharedString::from(m.text.clone())),
            );
        }
        col
    }

    fn preset_actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let this = cx.entity();
        if let Some(field) = self.naming.clone() {
            let (a, b) = (this.clone(), this);
            return div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(14.0))
                .pb(px(12.0))
                .child(field)
                .child(
                    Button::action("preset-save", &t, "Guardar")
                        .no_icon()
                        .on_click(move |_, window, cx| {
                            a.update(cx, |p, cx| p.confirm_naming(window, cx));
                        }),
                )
                .child(Button::ghost("preset-cancel", &t, "Cancelar").on_click(
                    move |_, window, cx| {
                        b.update(cx, |p, cx| p.cancel_naming(window, cx));
                    },
                ));
        }
        let (a, b, c) = (this.clone(), this.clone(), this);
        // The action button's shadow reaches ~16 px below it: room on the sides and under the
        // row so it never touches the ghost buttons or the next label.
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .pb(px(12.0))
            .child(
                Button::action("preset-new", &t, "Guardar como preset…").on_click(
                    move |_, window, cx| a.update(cx, |p, cx| p.start_naming(window, cx)),
                ),
            )
            .child(
                Button::ghost("preset-export", &t, "Exportar")
                    .tooltip("Copia el layout como TOML al portapapeles")
                    .on_click(move |_, _, cx| b.update(cx, |p, cx| p.export(cx))),
            )
            .child(
                Button::ghost("preset-import", &t, "Importar")
                    .tooltip("Lee un preset en TOML del portapapeles")
                    .on_click(move |_, window, cx| c.update(cx, |p, cx| p.import(window, cx))),
            )
    }

    fn sections_row(&mut self, layout: &ResolvedLayout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let enabled = layout.sidebar.sections.clone();
        let order = enabled.iter().map(|s| (*s, true)).chain(
            ALL_SECTIONS
                .iter()
                .filter(|s| !enabled.contains(s))
                .map(|s| (*s, false)),
        );
        let last_on = enabled.len().saturating_sub(1);

        // A 24 px arrow button; `up` rotates the chevron half a turn.
        let arrow = |id: SharedString, up: bool| {
            div()
                .id(id)
                .occlude()
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .cursor_pointer()
                .hover(move |s| s.bg(c.surface_4))
                .child(icon(Icon::ChevronDown, 14.0, c.muted).with_transformation(
                    Transformation::rotate(radians(if up { std::f32::consts::PI } else { 0.0 })),
                ))
        };

        let rows = order.enumerate().map(|(ix, (section, on))| {
            let toggle = {
                let this = this.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    this.update(cx, |p, cx| {
                        p.edit(window, cx, |l| {
                            if l.sidebar.sections.contains(&section) {
                                l.sidebar.sections.retain(|s| *s != section);
                            } else {
                                l.sidebar.sections.push(section);
                            }
                        })
                    });
                }
            };
            let mv = |dir: isize| {
                let this = this.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    this.update(cx, |p, cx| {
                        p.edit(window, cx, |l| {
                            let s = &mut l.sidebar.sections;
                            if let Some(i) = s.iter().position(|x| *x == section) {
                                let j = i.saturating_add_signed(dir).min(s.len().saturating_sub(1));
                                s.swap(i, j);
                            }
                        })
                    });
                }
            };
            let mut row = div()
                .id(SharedString::from(format!("section-{ix}")))
                .w(px(200.0))
                .h(px(28.0))
                .flex_none()
                .pl(px(10.0))
                .pr(px(3.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .rounded(px(10.0))
                .cursor_pointer()
                .text_size(px(t.typography.ui_size_md))
                .on_click(toggle);
            row = if on {
                row.bg(c.surface_3).text_color(c.text)
            } else {
                row.text_color(c.muted)
                    .hover(move |s| s.bg(c.surface_2).text_color(c.text))
            };
            row = row
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .when(on, |d| d.child(icon(Icon::Check, 14.0, c.text))),
                )
                .child(div().flex_1().child(section_label(section)));
            if on {
                let up = arrow(SharedString::from(format!("section-up-{ix}")), true);
                let down = arrow(SharedString::from(format!("section-down-{ix}")), false);
                let up = if ix > 0 {
                    up.on_click(mv(-1)).into_any_element()
                } else {
                    div().size(px(22.0)).into_any_element()
                };
                let down = if ix < last_on {
                    down.on_click(mv(1)).into_any_element()
                } else {
                    div().size(px(22.0)).into_any_element()
                };
                row = row.child(up).child(down);
            }
            row.into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .py(px(4.0))
            .children(rows)
            .into_any_element()
    }

    fn splits_row(
        &mut self,
        current: Splits,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const ALL: [(Splits, &str); 5] = [
            (Splits::Single, "Un panel"),
            (Splits::TwoVertical, "2 en columnas"),
            (Splits::TwoHorizontal, "2 en filas"),
            (Splits::OneAndTwoStacked, "1 + 2 apiladas"),
            (Splits::Grid2x2, "Cuadrícula 2×2"),
        ];
        let t = self.theme;
        let c = t.colors;
        let handle = self.focus_for("splits", cx);
        let focused = self.kbd && handle.is_focused(window);
        let this = cx.entity();
        let cur_ix = ALL.iter().position(|(s, _)| *s == current);
        let key_this = this.clone();
        self.ring(div().id("splits-focus"), focused, 14.0)
            .track_focus(&handle)
            .on_key_down(move |ev, window, cx| {
                let ix = match ev.keystroke.key.as_str() {
                    "left" => cur_ix.map_or(0, |s| s.saturating_sub(1)),
                    "right" => cur_ix.map_or(0, |s| (s + 1).min(ALL.len() - 1)),
                    _ => return,
                };
                cx.stop_propagation();
                let v = ALL[ix].0;
                key_this.update(cx, |p, cx| p.edit(window, cx, |l| l.splits = v));
            })
            .flex()
            .flex_row()
            .gap(px(4.0))
            .children(ALL.iter().enumerate().map(|(ix, (s, name))| {
                let this = this.clone();
                let s = *s;
                let sel = s == current;
                div()
                    .id(("split", ix))
                    .p(px(2.0))
                    .rounded(px(12.0))
                    .border_2()
                    .border_color(if sel {
                        c.accent.into()
                    } else {
                        transparent_black()
                    })
                    .cursor_pointer()
                    .tooltip(Tooltip::text(&t, *name))
                    .child(wire::split_icon(&t, s, sel))
                    .on_click(move |_, window, cx| {
                        this.update(cx, |p, cx| p.edit(window, cx, |l| l.splits = s));
                    })
            }))
            .into_any_element()
    }

    fn fine_options(
        &mut self,
        l: &ResolvedLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> [Div; 5] {
        let base = self.state.base_layout();
        let right = |id: &str| Some(PanelId::new(id));

        // Pestañas.
        let tabs = self.seg(
            "tabs",
            &[
                (TabsPosition::Top, "Arriba"),
                (TabsPosition::Sidebar, "Sidebar"),
                (TabsPosition::Hidden, "Ocultas"),
            ],
            Some(l.tabs_position),
            |l, v| l.tabs_position = v,
            window,
            cx,
        );
        let tabs_group = self.group(
            "Pestañas",
            vec![self.row("Posición", tabs).into_any_element()],
        );

        // Sidebar.
        let side = self.seg(
            "sidebar-side",
            &[
                (SidebarSide::Left, "Izquierda"),
                (SidebarSide::Right, "Derecha"),
                (SidebarSide::Hidden, "Oculta"),
            ],
            Some(l.sidebar.side),
            |l, v| l.sidebar.side = v,
            window,
            cx,
        );
        let width = self.slider(
            "sidebar-width",
            (180.0, 480.0, 10.0),
            f32::from(l.sidebar.width_px),
            f32::from(base.sidebar.width_px),
            "px",
            |l, v| l.sidebar.width_px = v as u16,
            window,
            cx,
        );
        let sections = self.sections_row(l, cx);
        let group_by = self.seg(
            "group-by",
            &[
                (GroupBy::Project, "Proyecto"),
                (GroupBy::Status, "Estado"),
                (GroupBy::Agent, "Agente"),
                (GroupBy::None, "Ninguno"),
            ],
            Some(l.sidebar.group_by),
            |l, v| l.sidebar.group_by = v,
            window,
            cx,
        );
        let show_actions = self.toggle("sb-actions", l.sidebar.show_actions, |l, v| l.sidebar.show_actions = v, window, cx);
        let panel_icons = self.toggle("sb-icons", l.sidebar.panel_icons, |l, v| l.sidebar.panel_icons = v, window, cx);
        let nested = self.toggle("sb-nested", l.sidebar.nested_tabs, |l, v| l.sidebar.nested_tabs = v, window, cx);
        let footer = self.toggle("sb-footer", l.sidebar.footer_usage, |l, v| l.sidebar.footer_usage = v, window, cx);
        let row_model = self.toggle("sb-model", l.sidebar.row_model, |l, v| l.sidebar.row_model = v, window, cx);
        let row_time = self.toggle("sb-time", l.sidebar.row_time, |l, v| l.sidebar.row_time = v, window, cx);
        let sidebar_group = self.group(
            "Sidebar",
            vec![
                self.row("Posición", side).into_any_element(),
                self.row("Ancho", width).into_any_element(),
                self.row("Secciones", sections)
                    .items_start()
                    .into_any_element(),
                self.row("Agrupar por", group_by).into_any_element(),
                self.row("Acciones «Nueva pestaña» y «Buscar»", show_actions)
                    .into_any_element(),
                self.row("Fila de iconos de paneles", panel_icons)
                    .into_any_element(),
                self.row("Pestañas dentro de su espacio", nested)
                    .into_any_element(),
                self.row("Uso y Ajustes al pie", footer).into_any_element(),
                self.row("Modelo y rama en cada fila", row_model)
                    .into_any_element(),
                self.row("Tiempo en cada fila", row_time).into_any_element(),
            ],
        );

        // Botones de la barra de título.
        let chips = self.titlebar_chips(l, cx);
        let titlebar_group = self.group(
            "Barra de título",
            vec![self
                .row("Botones de paneles", chips)
                .items_start()
                .into_any_element()],
        );

        // Zonas.
        let rail = self.toggle(
            "rail",
            l.rail == RailVisibility::Visible,
            |l, v| {
                l.rail = if v {
                    RailVisibility::Visible
                } else {
                    RailVisibility::Hidden
                }
            },
            window,
            cx,
        );
        let right_panel = self.seg(
            "right-panel",
            &[
                (right(store::config::panel_ids::GIT), "Git"),
                (right(store::config::panel_ids::CONTEXT), "Contexto"),
                (right(store::config::panel_ids::PROCS), "Procesos"),
                (right(store::config::panel_ids::FILES), "Archivos"),
                (None, "Ninguno"),
            ],
            Some(l.right_panel.clone()),
            |l, v| l.right_panel = v,
            window,
            cx,
        );
        let bottom = self.seg(
            "bottom-bar",
            &[
                (BottomBar::Compact, "Compacta"),
                (BottomBar::Full, "Completa"),
                (BottomBar::Hidden, "Oculta"),
            ],
            Some(l.bottom_bar),
            |l, v| l.bottom_bar = v,
            window,
            cx,
        );
        let splits = self.splits_row(l.splits, window, cx);
        let zones_group = self.group(
            "Zonas",
            vec![
                self.row("Rail de iconos", rail).into_any_element(),
                self.row("Panel derecho", right_panel).into_any_element(),
                self.row("Barra inferior", bottom).into_any_element(),
                self.row("Splits", splits).into_any_element(),
            ],
        );

        // Apariencia.
        let density = self.seg(
            "density",
            &[
                (Density::Compact, "Compacta"),
                (Density::Comfortable, "Cómoda"),
            ],
            Some(l.density),
            |l, v| l.density = v,
            window,
            cx,
        );
        let translucency = self.slider(
            "translucency",
            (0.0, 100.0, 5.0),
            f32::from(l.sidebar_translucency),
            f32::from(base.sidebar_translucency),
            "%",
            |l, v| l.sidebar_translucency = v as u8,
            window,
            cx,
        );
        let breadcrumb = self.toggle(
            "breadcrumb",
            l.breadcrumb,
            |l, v| l.breadcrumb = v,
            window,
            cx,
        );
        let motion = self.toggle(
            "reduced-motion",
            l.reduced_motion,
            |l, v| l.reduced_motion = v,
            window,
            cx,
        );
        let zen = self.toggle("zen", l.zen, |l, v| l.zen = v, window, cx);
        let look_group = self.group(
            "Apariencia",
            vec![
                self.row("Densidad", density).into_any_element(),
                self.row("Translucidez", translucency).into_any_element(),
                self.row("Breadcrumb", breadcrumb).into_any_element(),
                self.row("Reducir movimiento", motion).into_any_element(),
                self.row("Modo Zen", zen).into_any_element(),
            ],
        );

        [tabs_group, sidebar_group, titlebar_group, zones_group, look_group]
    }

    /// One toggle chip per button the titlebar can show (right panels, review, layout).
    fn titlebar_chips(&mut self, l: &ResolvedLayout, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_end()
            .gap(px(6.0))
            .max_w(px(300.0))
            .children(TITLEBAR_BUTTONS.iter().map(|(id, label)| {
                let on = l.titlebar.has(id);
                let this = this.clone();
                let id = *id;
                div()
                    .id(SharedString::from(format!("tb-btn-{id}")))
                    .occlude()
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .rounded(px(9.0))
                    .cursor_pointer()
                    .bg(if on { c.surface_4 } else { c.surface_1 })
                    .text_size(px(12.0))
                    .text_color(if on { c.text } else { c.muted })
                    .hover(move |s| s.bg(c.surface_3).text_color(c.text))
                    .when(on, |d| d.child(icon(Icon::Check, 12.0, c.text)))
                    .child(*label)
                    .on_click(move |_, window, cx| {
                        this.update(cx, |p, cx| {
                            p.edit(window, cx, |l| {
                                let list = &mut l.titlebar.panel_buttons;
                                if on {
                                    list.retain(|b| b.as_str() != id);
                                } else {
                                    list.push(PanelId::new(id));
                                    // Keep the canonical order of the chips.
                                    let pos = |b: &PanelId| {
                                        TITLEBAR_BUTTONS
                                            .iter()
                                            .position(|(i, _)| *i == b.as_str())
                                            .unwrap_or(usize::MAX)
                                    };
                                    list.sort_by_key(pos);
                                }
                            });
                        });
                    })
            }))
    }
}

impl Render for LayoutPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let layout = self.state.layout();
        let max_h = (f32::from(window.viewport_size().height) - 96.0).clamp(280.0, 880.0);

        let header = self.header(&layout, window, cx);
        let notes = self.notes();
        let presets = self.presets(window, cx);
        let [tabs, sidebar, titlebar, zones, look] = self.fine_options(&layout, window, cx);
        // Child index of each section in the scroll container (see `reveal_focus`).
        let first = 1 + usize::from(notes.is_some());
        self.section_ix = [0, first, first + 1, first + 2, first + 4, first + 5];
        let embedded = self.embedded;

        div()
            .key_context(PANEL_CONTEXT)
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &FocusNextField, window, cx| {
                this.kbd = true;
                window.focus_next();
                this.reveal_focus(window);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusPrevField, window, cx| {
                this.kbd = true;
                window.focus_prev();
                this.reveal_focus(window);
                cx.notify();
            }))
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if !this.kbd {
                    this.kbd = true;
                    cx.notify();
                }
            }))
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if this.kbd {
                    this.kbd = false;
                    cx.notify();
                }
            }))
            .when(!embedded, |d| {
                d.w(px(PANEL_WIDTH)).flex_none().rounded(px(Radius::SM)).bg(c.surface_2)
            })
            .when(embedded, |d| d.w_full().min_w_0())
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .text_size(px(t.typography.ui_size_md))
            .child(
                div()
                    .id("layout-panel-scroll")
                    .when(!embedded, |d| {
                        d.max_h(px(max_h)).overflow_y_scroll().track_scroll(&self.scroll).p(px(PAD))
                    })
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(header)
                    .children(notes)
                    .child(presets)
                    .child(tabs)
                    .child(sidebar)
                    .child(titlebar)
                    .child(zones)
                    .child(look),
            )
    }
}
