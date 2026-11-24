//! Integration point of the layout customization popover (docs/PLAN.md section 17).
//!
//! The popover itself lives in the `ui_layout` crate. The shell only owns the button, the
//! anchoring ([`ui_kit::Popover`]) and the `Ctrl+Shift+L` shortcut; **what is inside** comes from a
//! [`LayoutPanelHost`] registered at startup:
//!
//! ```ignore
//! layout_host::register_layout_panel_host(cx, layout_host::ui_layout_host);
//! ```
//!
//! Without a registered host the real `ui_layout` panel is used.
//!
//! The host gets the current [`LayoutConfig`] and a callback to emit a new one. The shell applies
//! it live (with the zone transition) and saves it, debounced. The view is rebuilt every time the
//! popover opens, so it can keep its own state.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{AnyView, App, Global, Window};
use store::LayoutConfig;
use theme::Theme;

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
    ui_layout::layout_panel(
        args.layout,
        args.cwd,
        args.theme,
        move |layout, _, cx| on_change(layout, cx),
        cx,
    )
    .into()
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
        None => ui_layout_host(args, window, cx),
    }
}
