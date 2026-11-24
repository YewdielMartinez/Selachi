//! Left icon rail (52 px): shark logo on top, then one icon button per registered panel.
//!
//! The buttons come from the shell (`AppShell::rail_items`): the terminal view plus every panel in
//! the `PanelRegistry` that can live on the right. A panel without a provider has no button, so
//! nothing on the rail is a placeholder. In XS the same items go into the titlebar's menu.

use std::rc::Rc;

use gpui::{App, IntoElement, SharedString, Window, WindowControlArea, div, prelude::*, px};
use theme::Theme;
use ui_kit::{Icon, IconButton, Logo};

use crate::titlebar::TITLEBAR_H;

/// Width of the left icon rail.
pub const RAIL_W: f32 = 52.0;

/// Click handler of a rail item.
pub type RailClick = Rc<dyn Fn(&mut Window, &mut App)>;

/// One rail button (or one row of the XS menu).
pub struct RailItem {
    pub id: SharedString,
    pub icon: Icon,
    pub title: SharedString,
    /// The current view / the panel that is open: `surface_3` pill, icon `text`.
    pub active: bool,
    /// `None` for the terminal view, which is what the window shows already.
    pub on_click: Option<RailClick>,
}

pub fn rail(theme: &Theme, items: Vec<RailItem>) -> impl IntoElement {
    let buttons = items.into_iter().map(|item| {
        let RailItem {
            id,
            icon,
            title,
            active,
            on_click,
        } = item;
        // Active is a pill; hover is a lighter one that keeps the muted icon, so a hovered
        // button can never be mistaken for the active one.
        let mut b = IconButton::new(id, theme, icon)
            .active(active)
            .tooltip(title);
        if let Some(f) = on_click {
            b = b.on_click(move |_, window, cx| f(window, cx));
        }
        b
    });
    div()
        .w(px(RAIL_W))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .pb(px(6.0))
        // Logo: also part of the drag area so the window can be grabbed from the top-left.
        .child(
            div()
                .w_full()
                .h(px(TITLEBAR_H))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .window_control_area(WindowControlArea::Drag)
                // Light art on dark/OLED, dark art on light: the file changes, never a chip.
                .child(Logo::new(theme.mode).size(40.0)),
        )
        .child(
            div()
                .mt(px(4.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .children(buttons),
        )
}
