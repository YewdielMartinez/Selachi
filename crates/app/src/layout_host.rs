//! Integration point of the layout customization popover (docs/PLAN.md section 17).
//!
//! The popover itself lives in the `ui_layout` crate. The shell only owns the button, the
//! anchoring ([`ui_kit::Popover`]) and the `Ctrl+Shift+L` shortcut; **what is inside** comes from a
//! [`LayoutPanelHost`] registered at startup:
//!
//! ```ignore
//! // main.rs, once ui_layout exposes its entry point:
//! layout_host::register_layout_panel_host(cx, UiLayoutHost);
//! ```
//!
//! Until a host is registered the shell shows [`FallbackLayoutPanel`]: the five presets as a
//! segmented control, which already works end to end (select, apply live, save to `config.toml`).
//!
//! The host gets the current [`LayoutConfig`] and a callback to emit a new one. The shell applies
//! it live (with the zone transition) and saves it, debounced. The view is rebuilt every time the
//! popover opens, so it can keep its own state.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyView, App, AppContext as _, Context, Global, IntoElement, ParentElement, Render, Styled, Window, div,
    px,
};
use store::{BuiltinPreset, LayoutConfig, PresetId};
use theme::Theme;
use ui_kit::{SectionLabel, SegmentedControl};

/// Callback that receives a new layout.
pub type OnLayoutChange = Rc<dyn Fn(LayoutConfig, &mut App)>;

/// What the shell gives the layout panel when it opens.
#[derive(Clone)]
pub struct LayoutPanelArgs {
    pub theme: Theme,
    /// The `[layout]` section as it is now.
    pub layout: LayoutConfig,
    /// Working directory of the active tab (for per-project overrides).
    pub cwd: Option<PathBuf>,
    /// Emit a new layout: applied live and saved.
    pub on_change: OnLayoutChange,
}

/// Builds the view shown inside the layout popover.
pub trait LayoutPanelHost: 'static {
    fn build(&self, args: LayoutPanelArgs, window: &mut Window, cx: &mut App) -> AnyView;
}

/// Convenience: any closure is a host.
impl<F> LayoutPanelHost for F
where
    F: Fn(LayoutPanelArgs, &mut Window, &mut App) -> AnyView + 'static,
{
    fn build(&self, args: LayoutPanelArgs, window: &mut Window, cx: &mut App) -> AnyView {
        self(args, window, cx)
    }
}

#[derive(Default)]
struct HostGlobal(Option<Rc<dyn LayoutPanelHost>>);

impl Global for HostGlobal {}

/// The real panel: `ui_layout::layout_panel`. The shell keeps the returned view, downcasts it to
/// `ui_layout::LayoutPanel` and calls `set_config` / `set_theme` / `set_cwd` on it while open.
pub fn ui_layout_host(args: LayoutPanelArgs, _window: &mut Window, cx: &mut App) -> AnyView {
    let on_change = args.on_change.clone();
    ui_layout::layout_panel(args.layout, args.cwd, args.theme, move |layout, _, cx| on_change(layout, cx), cx).into()
}

/// Registers the popover content (call once at startup, before the window opens).
pub fn register_layout_panel_host(cx: &mut App, host: impl LayoutPanelHost) {
    cx.set_global(HostGlobal(Some(Rc::new(host))));
}

/// The registered host's view, or the fallback.
pub fn build_layout_panel(args: LayoutPanelArgs, window: &mut Window, cx: &mut App) -> AnyView {
    let host = cx.try_global::<HostGlobal>().and_then(|h| h.0.clone());
    match host {
        Some(h) => h.build(args, window, cx),
        None => cx.new(|_| FallbackLayoutPanel { args }).into(),
    }
}

/// Minimal popover content: the five built-in presets.
pub struct FallbackLayoutPanel {
    args: LayoutPanelArgs,
}

/// Preset labels, in [`BuiltinPreset::ALL`] order.
const LABELS: [&str; 5] = ["Terminal", "Espacios", "Orquestador", "Chat", "Zen"];

/// Index of the selected preset in [`LABELS`], or `usize::MAX` for a custom preset.
fn selected_index(preset: &PresetId) -> usize {
    match preset {
        PresetId::Builtin(b) => BuiltinPreset::ALL.iter().position(|p| p == b).unwrap_or(usize::MAX),
        PresetId::Custom(_) => usize::MAX,
    }
}

impl Render for FallbackLayoutPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.args.theme;
        let this = cx.entity();
        let selected = selected_index(&self.args.layout.preset);
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(SectionLabel::new(&theme, "Layout"))
            .child(SegmentedControl::new("layout-presets", &theme, LABELS).selected(selected).on_change(
                move |ix, _, cx| {
                    let Some(preset) = BuiltinPreset::ALL.get(ix).copied() else { return };
                    this.update(cx, |panel, cx| {
                        panel.args.layout.select_preset(preset.into());
                        let layout = panel.args.layout.clone();
                        (panel.args.on_change)(layout, cx);
                        cx.notify();
                    });
                },
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_index_follows_builtin_order() {
        for (i, b) in BuiltinPreset::ALL.into_iter().enumerate() {
            assert_eq!(selected_index(&b.into()), i);
            assert_eq!(LABELS[i].to_lowercase(), b.id());
        }
        assert_eq!(selected_index(&PresetId::Custom("x".into())), usize::MAX);
    }
}
