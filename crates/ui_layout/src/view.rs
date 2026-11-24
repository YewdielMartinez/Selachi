//! The gpui view of the layout panel.

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
    Button, Card, Icon, InputModality, MeasuredWidth, Pill, SectionLabel, SegmentedControl, Slider,
    TextField, TextFieldEvent, Toggle, Tooltip, icon, input_modality,
};

use crate::state::{
    ALL_SECTIONS, LayoutState, Scope, builtin_description, builtin_name, section_label,
};
use crate::wire;
use crate::{FocusNextField, FocusPrevField, PANEL_CONTEXT, STEP_CONTEXT, StepNext, StepPrev};

/// Width of the panel content in px.
pub const PANEL_WIDTH: f32 = 560.0;
const PAD: f32 = 14.0;
const CARD_W: f32 = 101.0;
/// Below this measured width a row puts its control under the label.
const STACK_BELOW: f32 = 480.0;
/// Blocks of the scroll column: header, presets, tabs, sidebar, titlebar, zones, look.
const BLOCKS: usize = 7;

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
    /// Roving lists: one tab stop each, the arrows (`StepPrev` / `StepNext`) move the selection.
    presets_focus: FocusHandle,
    splits_focus: FocusHandle,
    /// One non-tab-stop handle per block of the scroll column, to scroll the block that holds the
    /// focus into view after `Tab` (see `reveal_focus`).
    blocks: [FocusHandle; BLOCKS],
    naming: Option<Entity<TextField>>,
    _naming_sub: Option<Subscription>,
    message: Option<Message>,
    scroll: ScrollHandle,
    /// Child index of each block in the scroll container.
    block_ix: [usize; BLOCKS],
    /// The zone width really measured (the panel is a popover, a tab or a Settings block).
    measured: MeasuredWidth,
    /// Rows stack their control under the label (decided each frame from `measured`).
    stacked: bool,
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
        presets_focus: cx.focus_handle().tab_stop(true),
        splits_focus: cx.focus_handle().tab_stop(true),
        blocks: std::array::from_fn(|_| cx.focus_handle()),
        naming: None,
        _naming_sub: None,
        message: None,
        scroll: ScrollHandle::new(),
        block_ix: [0, 1, 2, 3, 4, 5, 6],
        measured: MeasuredWidth::new(STACK_BELOW),
        stacked: false,
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
            n.update(cx, |f, cx| f.set_theme(theme, cx));
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

    /// Scroll the block that holds the focused control into view (after Tab / Shift+Tab).
    fn reveal_focus(&self, window: &Window, cx: &App) {
        if let Some(slot) = self
            .blocks
            .iter()
            .position(|h| h.contains_focused(window, cx))
        {
            self.scroll.scroll_to_item(self.block_ix[slot]);
        }
    }

    /// Scroll the panel to its end (screenshots, tests).
    pub fn scroll_to_end(&self) {
        self.scroll.scroll_to_bottom();
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

    /// The arrow keys on the preset grid: pick the neighbour of the active card.
    fn step_preset(
        &mut self,
        ids: &[PresetId],
        active: Option<usize>,
        dir: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = stepped(active, ids.len(), dir);
        if let Some(id) = ids.get(ix).cloned()
            && Some(ix) != active
        {
            self.pick_preset(id, window, cx);
        }
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
        let field = cx.new(|cx| TextField::new(theme, cx).placeholder("Nombre del preset"));
        self._naming_sub = Some(cx.subscribe_in(
            &field,
            window,
            |this, _, event: &TextFieldEvent, window, cx| match event {
                TextFieldEvent::Submit => this.confirm_naming(window, cx),
                TextFieldEvent::Cancel => this.cancel_naming(window, cx),
                _ => {}
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

    /// The accent focus ring of a roving list: a 2 px border painted only while the list has the
    /// focus and the last input was the keyboard (the kit's controls draw their own ring).
    fn ring(
        &self,
        el: Stateful<Div>,
        focus: &FocusHandle,
        window: &Window,
        radius: f32,
    ) -> Stateful<Div> {
        let c = self.theme.colors;
        let focused = input_modality() == InputModality::Keyboard && focus.is_focused(window);
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
                .stacked(self.stacked)
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

    /// A segmented control (each segment is a tab stop); `pick` receives the index of the chosen one.
    fn seg_core(
        &self,
        key: &str,
        labels: Vec<SharedString>,
        selected: Option<usize>,
        track: Rgba,
        pick: impl Fn(&mut Self, usize, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity();
        SegmentedControl::new(SharedString::from(key.to_string()), &self.theme, labels)
            .track(track)
            .selected(selected.unwrap_or(usize::MAX))
            .on_change(move |ix, window, cx| this.update(cx, |p, cx| pick(p, ix, window, cx)))
            .into_any_element()
    }

    /// A segmented control over enum values, applied with `set`.
    fn seg<T: Clone + PartialEq + 'static>(
        &self,
        key: &str,
        options: &[(T, &'static str)],
        current: Option<T>,
        set: fn(&mut ResolvedLayout, T),
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
            cx,
        )
    }

    fn toggle(
        &self,
        key: &str,
        value: bool,
        set: fn(&mut ResolvedLayout, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity();
        Toggle::new(SharedString::from(key.to_string()), &self.theme)
            .on(value)
            .on_change(move |v, window, cx| {
                this.update(cx, |p, cx| p.edit(window, cx, |l| set(l, v)))
            })
            .into_any_element()
    }

    /// A slider (arrow keys, `Home` / `End`, `PageUp` / `PageDown` come from the kit) with a value
    /// label; `set` receives the rounded value.
    #[allow(clippy::too_many_arguments)]
    fn slider(
        &self,
        key: &str,
        (min, max, step): (f32, f32, f32),
        value: f32,
        default: f32,
        unit: &'static str,
        set: fn(&mut ResolvedLayout, f32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let this = cx.entity();
        div()
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
                    .on_change(move |v, window, cx| {
                        this.update(cx, |p, cx| p.edit(window, cx, |l| set(l, v)));
                    }),
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

    fn header(&mut self, layout: &ResolvedLayout, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
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
                            .text_size(px(ty.ui_size_xs))
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
                    .text_size(px(ty.ui_size_xs))
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
        let ty = t.typography;
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
                                .text_size(px(ty.ui_size_xs))
                                .text_color(c.muted)
                                .child(SharedString::from(w.clone())),
                        )
                })),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn preset_card(
        &self,
        id: PresetId,
        name: String,
        description: &'static str,
        layout: &ResolvedLayout,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let this = cx.entity();
        let custom_name = matches!(id, PresetId::Custom(_)).then(|| name.clone());
        let key = preset_key(&id);
        let pick = id;
        let mut card = div()
            .id(SharedString::from(format!("preset-{key}")))
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
                            .text_size(px(ty.ui_size_xs))
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
                .id(SharedString::from(format!("preset-del-{key}")))
                .occlude()
                .absolute()
                .top(px(8.0))
                .right(px(8.0))
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::CHIP))
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
        let ids: Rc<Vec<PresetId>> = Rc::new(entries.iter().map(|e| e.0.clone()).collect());
        let active_ix = active
            .as_ref()
            .and_then(|a| ids.iter().position(|i| i == a));

        let cards: Vec<AnyElement> = entries
            .into_iter()
            .map(|(id, name, desc, layout)| {
                let selected = Some(&id) == active.as_ref();
                self.preset_card(id, name, desc, &layout, selected, cx)
                    .into_any_element()
            })
            .collect();

        let grid = self
            .ring(div().id("presets-focus"), &self.presets_focus, window, 16.0)
            .track_focus(&self.presets_focus)
            .key_context(STEP_CONTEXT)
            .on_action(cx.listener({
                let ids = ids.clone();
                move |p, _: &StepPrev, window, cx| p.step_preset(&ids, active_ix, -1, window, cx)
            }))
            .on_action(cx.listener(move |p, _: &StepNext, window, cx| {
                p.step_preset(&ids, active_ix, 1, window, cx)
            }))
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
            let ty = t.typography;
            col = col.child(
                div()
                    .px(px(4.0))
                    .text_size(px(ty.ui_size_xs))
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
                .tab_index(0)
                .focus(move |s| s.border_color(c.accent))
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .border_2()
                .border_color(transparent_black())
                .rounded(px(Radius::CHIP))
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
            let name = section_label(section);
            let mut row = div()
                .id(SharedString::from(format!("section-{name}")))
                .tab_index(0)
                .focus(move |s| s.bg(c.surface_4))
                .w(px(200.0))
                .h(px(28.0))
                .flex_none()
                .pl(px(10.0))
                .pr(px(3.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .rounded(px(Radius::PILL))
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
                let up = arrow(SharedString::from(format!("section-up-{name}")), true);
                let down = arrow(SharedString::from(format!("section-down-{name}")), false);
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
        let this = cx.entity();
        let cur_ix = ALL.iter().position(|(s, _)| *s == current);
        let step = move |p: &mut Self, dir: isize, window: &mut Window, cx: &mut Context<Self>| {
            let ix = stepped(cur_ix, ALL.len(), dir);
            if Some(ix) != cur_ix {
                let v = ALL[ix].0;
                p.edit(window, cx, |l| l.splits = v);
            }
        };
        self.ring(
            div().id("splits-focus"),
            &self.splits_focus,
            window,
            Radius::SM + 2.0,
        )
        .track_focus(&self.splits_focus)
        .key_context(STEP_CONTEXT)
        .on_action(cx.listener(move |p, _: &StepPrev, window, cx| step(p, -1, window, cx)))
        .on_action(cx.listener(move |p, _: &StepNext, window, cx| step(p, 1, window, cx)))
        .flex()
        .flex_row()
        .gap(px(4.0))
        .children(ALL.iter().map(|(s, name)| {
            let this = this.clone();
            let s = *s;
            let sel = s == current;
            div()
                .id(SharedString::from(format!("split-{name}")))
                .p(px(2.0))
                .rounded(px(Radius::SM))
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
            cx,
        );
        let width = self.slider(
            "sidebar-width",
            (180.0, 480.0, 10.0),
            f32::from(l.sidebar.width_px),
            f32::from(base.sidebar.width_px),
            "px",
            |l, v| l.sidebar.width_px = v as u16,
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
            cx,
        );
        let show_actions = self.toggle(
            "sb-actions",
            l.sidebar.show_actions,
            |l, v| l.sidebar.show_actions = v,
            cx,
        );
        let panel_icons = self.toggle(
            "sb-icons",
            l.sidebar.panel_icons,
            |l, v| l.sidebar.panel_icons = v,
            cx,
        );
        let nested = self.toggle(
            "sb-nested",
            l.sidebar.nested_tabs,
            |l, v| l.sidebar.nested_tabs = v,
            cx,
        );
        let footer = self.toggle(
            "sb-footer",
            l.sidebar.footer_usage,
            |l, v| l.sidebar.footer_usage = v,
            cx,
        );
        let row_model = self.toggle(
            "sb-model",
            l.sidebar.row_model,
            |l, v| l.sidebar.row_model = v,
            cx,
        );
        let row_time = self.toggle(
            "sb-time",
            l.sidebar.row_time,
            |l, v| l.sidebar.row_time = v,
            cx,
        );
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
            vec![
                self.row("Botones de paneles", chips)
                    .items_start()
                    .into_any_element(),
            ],
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
            cx,
        );
        let translucency = self.slider(
            "translucency",
            (0.0, 100.0, 5.0),
            f32::from(l.sidebar_translucency),
            f32::from(base.sidebar_translucency),
            "%",
            |l, v| l.sidebar_translucency = v as u8,
            cx,
        );
        let breadcrumb = self.toggle("breadcrumb", l.breadcrumb, |l, v| l.breadcrumb = v, cx);
        let motion = self.toggle(
            "reduced-motion",
            l.reduced_motion,
            |l, v| l.reduced_motion = v,
            cx,
        );
        let zen = self.toggle("zen", l.zen, |l, v| l.zen = v, cx);
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

        [
            tabs_group,
            sidebar_group,
            titlebar_group,
            zones_group,
            look_group,
        ]
    }

    /// One toggle chip per button the titlebar can show (right panels, review, layout). Each chip is
    /// a focusable `Pill`; the active ones carry a check.
    fn titlebar_chips(&mut self, l: &ResolvedLayout, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
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
                Pill::compact(format!("tb-btn-{id}"), &t)
                    .label(*label)
                    .active(on)
                    .when(on, |p| p.icon(Icon::Check))
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

/// Stable element-id key of a preset: its name, tagged so a custom preset that shares a built-in
/// name never collides with it (and survives other presets being added or deleted).
fn preset_key(id: &PresetId) -> String {
    match id {
        PresetId::Builtin(b) => format!("b-{}", builtin_name(*b)),
        PresetId::Custom(name) => format!("c-{name}"),
    }
}

/// The neighbour of `current` in a roving list of `len` items: `dir` < 0 is the previous one
/// (the first when nothing is selected), `dir` > 0 the next one; it clamps at both ends.
fn stepped(current: Option<usize>, len: usize, dir: isize) -> usize {
    let last = len.saturating_sub(1);
    match (current, dir < 0) {
        (None, _) => 0,
        (Some(s), true) => s.saturating_sub(1),
        (Some(s), false) => (s + 1).min(last),
    }
}

impl Render for LayoutPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let layout = self.state.layout();
        let max_h = (f32::from(window.viewport_size().height) - 96.0).clamp(280.0, 880.0);
        self.stacked = self.measured.narrow(window);

        let header = self.header(&layout, cx);
        let notes = self.notes();
        let presets = self.presets(window, cx);
        let [tabs, sidebar, titlebar, zones, look] = self.fine_options(&layout, window, cx);
        // Child index of each block in the scroll container (see `reveal_focus`).
        let first = 1 + usize::from(notes.is_some());
        self.block_ix = [
            0,
            first,
            first + 1,
            first + 2,
            first + 3,
            first + 4,
            first + 5,
        ];
        let embedded = self.embedded;
        let blocks = &self.blocks;
        let block = |ix: usize, el: Div| div().track_focus(&blocks[ix]).child(el);

        div()
            .key_context(PANEL_CONTEXT)
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &FocusNextField, window, cx| {
                window.focus_next();
                this.reveal_focus(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusPrevField, window, cx| {
                window.focus_prev();
                this.reveal_focus(window, cx);
                cx.notify();
            }))
            .relative()
            .when(!embedded, |d| {
                d.w(px(PANEL_WIDTH))
                    .flex_none()
                    .rounded(px(Radius::SM))
                    .bg(c.surface_2)
            })
            .when(embedded, |d| d.w_full().min_w_0())
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .text_size(px(t.typography.ui_size_md))
            .child(self.measured.probe())
            .child(
                div()
                    .id("layout-panel-scroll")
                    .when(!embedded, |d| {
                        d.max_h(px(max_h))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .p(px(PAD))
                    })
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(block(0, header))
                    .children(notes)
                    .child(block(1, presets))
                    .child(block(2, tabs))
                    .child(block(3, sidebar))
                    .child(block(4, titlebar))
                    .child(block(5, zones))
                    .child(block(6, look)),
            )
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
