//! `DropZone`: a drop target for files dragged from Explorer (`gpui::ExternalPaths`).
//!
//! Put it as the **last child of a `relative()` container** (it is an absolute overlay that covers
//! it). It never intercepts the pointer by itself (its hitbox does not occlude): it only reacts while
//! a file drag is over it, painting a 1.5 px accent outline, a faint accent wash and a
//! "Soltar para ..." label; when the files are released it calls `on_drop` with their paths.
//! Nothing is painted, and nothing ticks, when no file is being dragged.
//!
//! `SELACHI_DEBUG_DROP=1` forces the highlight on (screenshots without synthetic input).

use std::path::PathBuf;

use gpui::{
    AbsoluteLength, App, ElementId, ExternalPaths, FontWeight, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
};
use theme::Theme;

type DropFn = Box<dyn Fn(&[PathBuf], &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct DropZone {
    id: ElementId,
    theme: Theme,
    label: SharedString,
    radius: f32,
    compact: bool,
    on_drop: Option<DropFn>,
    forced: bool,
}

/// Is the debug override set (the highlight shown without an actual drag)?
pub fn debug_forced() -> bool {
    std::env::var_os("SELACHI_DEBUG_DROP").is_some_and(|v| v == "1")
}

impl DropZone {
    /// `label` completes "Soltar para ...": pass the whole text, e.g. `"Soltar para pegar las rutas"`.
    pub fn new(id: impl Into<ElementId>, theme: &Theme, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            label: label.into(),
            radius: 12.0,
            compact: false,
            on_drop: None,
            forced: debug_forced(),
        }
    }

    /// Corner radius of the outline (match the container's).
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// A small label for thin targets (a list row).
    pub fn compact(mut self, on: bool) -> Self {
        self.compact = on;
        self
    }

    /// Called with the dropped paths (never empty).
    pub fn on_drop(mut self, f: impl Fn(&[PathBuf], &mut Window, &mut App) + 'static) -> Self {
        self.on_drop = Some(Box::new(f));
        self
    }

    /// Show the highlight always (tests / screenshots).
    pub fn forced(mut self, on: bool) -> Self {
        self.forced = on;
        self
    }
}

/// Outline width of the highlight.
pub const OUTLINE_PX: f32 = 1.5;

impl RenderOnce for DropZone {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let c = self.theme.colors;
        let ty = self.theme.typography;
        let accent: gpui::Hsla = c.accent.into();
        let wash = gpui::Hsla { a: 0.10, ..accent };
        let transparent = gpui::Hsla { a: 0.0, ..accent };
        let outline = move |mut s: gpui::StyleRefinement, color: gpui::Hsla, fill: gpui::Hsla| {
            let w: AbsoluteLength = px(OUTLINE_PX).into();
            s.border_widths.top = Some(w);
            s.border_widths.right = Some(w);
            s.border_widths.bottom = Some(w);
            s.border_widths.left = Some(w);
            s.border_color = Some(color);
            s.background = Some(fill.into());
            s
        };
        let label = {
            let pill = div()
                .px(px(if self.compact { 8.0 } else { 14.0 }))
                .py(px(if self.compact { 1.0 } else { 7.0 }))
                .rounded(px(if self.compact { 7.0 } else { 10.0 }))
                .bg(c.surface_3)
                .text_color(c.text)
                .font_family(ty.ui_family)
                .text_size(px(if self.compact {
                    ty.ui_size_sm
                } else {
                    ty.ui_size_md
                }))
                .font_weight(FontWeight::MEDIUM)
                .child(self.label);
            let slot = div().size_full().flex().items_center().justify_center();
            if self.forced {
                slot.child(pill)
            } else {
                // Only painted while files are over the zone.
                slot.opacity(0.0)
                    .drag_over::<ExternalPaths>(|s, _, _, _| s.opacity(1.0))
                    .child(pill)
            }
        };
        let mut zone = div()
            .id(self.id)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .rounded(px(self.radius))
            .child(label);
        zone = if self.forced {
            let st = zone.style();
            *st = outline(st.clone(), accent, wash);
            zone
        } else {
            zone.drag_over::<ExternalPaths>(move |s, _, _, _| outline(s, accent, wash))
                .border_color(transparent)
        };
        if let Some(f) = self.on_drop {
            zone = zone.on_drop(move |paths: &ExternalPaths, window, cx| {
                let paths = paths.paths();
                if !paths.is_empty() {
                    f(paths, window, cx);
                }
            });
        }
        zone
    }
}
