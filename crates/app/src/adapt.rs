//! Responsive adaptation of the configured layout (docs/DESIGN.md, "Responsive").
//!
//! The layout in `config.toml` is the *intent*. [`adapt_layout`] turns it into what is actually
//! docked for a window width, without ever touching the saved configuration: enlarge the window
//! and the intent comes back. The function is pure so every breakpoint is unit tested.
//!
//! | class | width (logical px) | adaptation |
//! |---|---|---|
//! | XS | < 720 | no rail (icons go to a menu), sidebar and right panel as drawers, one pane |
//! | S | 720-1023 | rail, sidebar collapsed to icons (expands as an overlay), right panel overlay, 2x2 and 1+2 become 2 columns |
//! | M | 1024-1439 | as configured; the right panel drops to its minimum |
//! | L | 1440-2559 | as configured |
//! | XL | >= 2560 | as configured; reading panels capped at 860 px and centred |

use store::{BottomBar, RailVisibility, ResolvedLayout, SidebarSide, Splits};

/// Window width class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Breakpoint {
    Xs,
    S,
    M,
    L,
    Xl,
}

/// First width of each class.
pub const S_FROM: f32 = 720.0;
pub const M_FROM: f32 = 1024.0;
pub const L_FROM: f32 = 1440.0;
pub const XL_FROM: f32 = 2560.0;
/// Width of the icon-only sidebar of the S class.
pub const COLLAPSED_SIDEBAR_W: u16 = 56;
/// Widest a reading panel (viewer, chat, settings) gets at XL.
pub const READING_MAX_W: f32 = 860.0;
/// Right panel width limits (logical px).
pub const RIGHT_MIN_W: f32 = 240.0;
pub const RIGHT_MAX_W: f32 = 520.0;
/// Narrowest the main area is allowed to get before the right panel shrinks to its minimum.
const MAIN_MIN_W: f32 = 420.0;

impl Breakpoint {
    pub fn from_width(width: f32) -> Self {
        if width < S_FROM {
            Self::Xs
        } else if width < M_FROM {
            Self::S
        } else if width < L_FROM {
            Self::M
        } else if width < XL_FROM {
            Self::L
        } else {
            Self::Xl
        }
    }
}

/// How the sidebar is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    /// A docked column of its configured width.
    Docked,
    /// A docked 56 px icon column (S); the full sidebar opens as an overlay.
    Collapsed,
    /// Not docked: a drawer opened on demand (XS).
    Drawer,
    /// The layout has no sidebar.
    Off,
}

/// How the right panel is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightMode {
    Docked,
    /// Overlay card over the main area (S), opened from the rail.
    Overlay,
    /// Drawer with scrim (XS), opened from the menu.
    Drawer,
    /// The layout has no right panel.
    Off,
}

/// The layout to dock for a window width, plus how the parts that are not docked are reached.
#[derive(Clone, Debug, PartialEq)]
pub struct AdaptedLayout {
    /// What is docked now (zones with zero size are simply off).
    pub layout: ResolvedLayout,
    pub breakpoint: Breakpoint,
    pub sidebar: SidebarMode,
    pub right: RightMode,
    /// XS: the rail's buttons live in a `...` menu in the titlebar.
    pub rail_in_menu: bool,
    /// XS: one pane, with a tab selector.
    pub single_pane: bool,
    /// Breadcrumb shows only the last folder (XS, S).
    pub breadcrumb_last_only: bool,
    /// Reading panels are capped at this width and centred (XL).
    pub reading_max_width: Option<f32>,
}

/// Adapts `intent` to a window `width` (logical px).
pub fn adapt_layout(intent: &ResolvedLayout, width: f32) -> AdaptedLayout {
    let bp = Breakpoint::from_width(width);
    let mut layout = intent.clone();
    let has_sidebar = intent.sidebar.side != SidebarSide::Hidden;
    let has_right = intent.right_panel.is_some();
    let zen = intent.zen;

    let (sidebar, right) = match bp {
        Breakpoint::Xs => (
            if has_sidebar { SidebarMode::Drawer } else { SidebarMode::Off },
            if has_right { RightMode::Drawer } else { RightMode::Off },
        ),
        Breakpoint::S => (
            if has_sidebar { SidebarMode::Collapsed } else { SidebarMode::Off },
            if has_right { RightMode::Overlay } else { RightMode::Off },
        ),
        _ => (
            if has_sidebar { SidebarMode::Docked } else { SidebarMode::Off },
            if has_right { RightMode::Docked } else { RightMode::Off },
        ),
    };

    match bp {
        Breakpoint::Xs => {
            layout.rail = RailVisibility::Hidden;
            layout.sidebar.side = SidebarSide::Hidden;
            layout.right_panel = None;
            layout.splits = Splits::Single;
            if layout.bottom_bar == BottomBar::Full {
                layout.bottom_bar = BottomBar::Compact;
            }
        },
        Breakpoint::S => {
            if layout.bottom_bar == BottomBar::Full {
                layout.bottom_bar = BottomBar::Compact;
            }
            if has_sidebar {
                layout.sidebar.width_px = COLLAPSED_SIDEBAR_W;
            }
            layout.right_panel = None;
            if matches!(layout.splits, Splits::Grid2x2 | Splits::OneAndTwoStacked) {
                layout.splits = Splits::TwoVertical;
            }
        },
        _ => {},
    }

    AdaptedLayout {
        layout,
        breakpoint: bp,
        sidebar,
        right,
        // Zen has no rail to begin with; do not offer a menu for it.
        rail_in_menu: bp == Breakpoint::Xs && !zen,
        single_pane: bp == Breakpoint::Xs,
        breadcrumb_last_only: bp <= Breakpoint::S,
        reading_max_width: (bp == Breakpoint::Xl).then_some(READING_MAX_W),
    }
}

/// Width of a docked right panel. `stored` is the user's dragged width (or the default share of
/// the window); it is kept between 240 and 520 px and shrinks to the minimum when the main area
/// would get narrower than [`MAIN_MIN_W`] (`other_zones` = rail + sidebar + margins already
/// docked).
pub fn right_panel_width(window_width: f32, stored: Option<f32>, other_zones: f32) -> f32 {
    let wanted = stored.unwrap_or(window_width * 0.22).clamp(RIGHT_MIN_W, RIGHT_MAX_W);
    let room = window_width - other_zones - MAIN_MIN_W;
    if room < wanted { RIGHT_MIN_W.max(room.min(wanted)) } else { wanted }
}

/// Maximum width and horizontal margin of a reading column inside `available` px. Reading panels
/// (viewer, chat, settings) use it once they register; the git / context panels are lists.
#[allow(dead_code)]
pub fn reading_column(available: f32, max: Option<f32>) -> (f32, f32) {
    match max {
        Some(m) if available > m => (m, (available - m) / 2.0),
        _ => (available, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::BuiltinPreset as B;

    fn orq() -> ResolvedLayout {
        let mut l = ResolvedLayout::builtin(B::Orquestador);
        l.right_panel = Some(store::PanelId::new("git"));
        l.splits = Splits::Grid2x2;
        l.bottom_bar = BottomBar::Full;
        l
    }

    #[test]
    fn breakpoints_by_width() {
        assert_eq!(Breakpoint::from_width(300.0), Breakpoint::Xs);
        assert_eq!(Breakpoint::from_width(719.9), Breakpoint::Xs);
        assert_eq!(Breakpoint::from_width(720.0), Breakpoint::S);
        assert_eq!(Breakpoint::from_width(1023.0), Breakpoint::S);
        assert_eq!(Breakpoint::from_width(1024.0), Breakpoint::M);
        assert_eq!(Breakpoint::from_width(1439.0), Breakpoint::M);
        assert_eq!(Breakpoint::from_width(1440.0), Breakpoint::L);
        assert_eq!(Breakpoint::from_width(2559.0), Breakpoint::L);
        assert_eq!(Breakpoint::from_width(2560.0), Breakpoint::Xl);
        assert_eq!(Breakpoint::from_width(3840.0), Breakpoint::Xl);
    }

    #[test]
    fn xs_uses_drawers_one_pane_and_no_rail() {
        let a = adapt_layout(&orq(), 640.0);
        assert_eq!(a.breakpoint, Breakpoint::Xs);
        assert_eq!(a.layout.rail, RailVisibility::Hidden);
        assert!(a.rail_in_menu);
        assert_eq!(a.sidebar, SidebarMode::Drawer);
        assert_eq!(a.layout.sidebar.side, SidebarSide::Hidden);
        assert_eq!(a.right, RightMode::Drawer);
        assert!(a.layout.right_panel.is_none());
        assert_eq!(a.layout.splits, Splits::Single);
        assert!(a.single_pane);
        assert_eq!(a.layout.bottom_bar, BottomBar::Compact);
        assert!(a.breadcrumb_last_only);
        assert!(a.reading_max_width.is_none());
    }

    #[test]
    fn s_collapses_the_sidebar_and_folds_splits() {
        let a = adapt_layout(&orq(), 900.0);
        assert_eq!(a.breakpoint, Breakpoint::S);
        assert_eq!(a.sidebar, SidebarMode::Collapsed);
        assert_eq!(a.layout.sidebar.width_px, COLLAPSED_SIDEBAR_W);
        assert_eq!(a.right, RightMode::Overlay);
        assert!(a.layout.right_panel.is_none());
        assert_eq!(a.layout.splits, Splits::TwoVertical);
        assert_eq!(a.layout.bottom_bar, BottomBar::Compact);
        assert!(!a.rail_in_menu);
        assert!(!a.single_pane);
        let mut l = orq();
        l.splits = Splits::OneAndTwoStacked;
        assert_eq!(adapt_layout(&l, 800.0).layout.splits, Splits::TwoVertical);
        l.splits = Splits::TwoHorizontal;
        assert_eq!(adapt_layout(&l, 800.0).layout.splits, Splits::TwoHorizontal);
        // The configured rail stays.
        assert_eq!(a.layout.rail, orq().rail);
    }

    #[test]
    fn m_and_l_keep_the_intent() {
        for w in [1024.0, 1280.0, 1439.0, 1440.0, 1920.0, 2559.0] {
            let a = adapt_layout(&orq(), w);
            assert_eq!(a.layout, orq(), "width {w}");
            assert_eq!(a.sidebar, SidebarMode::Docked);
            assert_eq!(a.right, RightMode::Docked);
            assert!(a.reading_max_width.is_none());
        }
    }

    #[test]
    fn xl_keeps_the_intent_and_caps_reading_panels() {
        let a = adapt_layout(&orq(), 3840.0);
        assert_eq!(a.layout, orq());
        assert_eq!(a.reading_max_width, Some(860.0));
        assert_eq!(reading_column(3000.0, a.reading_max_width), (860.0, 1070.0));
        assert_eq!(reading_column(700.0, a.reading_max_width), (700.0, 0.0));
        assert_eq!(reading_column(3000.0, None), (3000.0, 0.0));
    }

    #[test]
    fn missing_zones_stay_off() {
        let term = ResolvedLayout::builtin(B::Terminal);
        for w in [500.0, 800.0, 1200.0] {
            let a = adapt_layout(&term, w);
            assert_eq!(a.sidebar, SidebarMode::Off, "width {w}");
            assert_eq!(a.right, RightMode::Off, "width {w}");
        }
    }

    #[test]
    fn adapting_never_changes_the_intent_and_is_reversible() {
        let intent = orq();
        let small = adapt_layout(&intent, 500.0);
        assert_ne!(small.layout, intent);
        assert_eq!(adapt_layout(&intent, 1600.0).layout, intent);
        // Idempotent for the same width.
        assert_eq!(adapt_layout(&intent, 500.0), small);
    }

    #[test]
    fn right_panel_width_clamps_and_yields_to_the_main_area() {
        // Default share of the window, clamped.
        assert_eq!(right_panel_width(1024.0, None, 100.0), 240.0);
        assert_eq!(right_panel_width(1600.0, None, 100.0), 352.0);
        assert_eq!(right_panel_width(3840.0, None, 100.0), 520.0);
        // Dragged width, clamped to 240..520.
        assert_eq!(right_panel_width(2000.0, Some(100.0), 100.0), 240.0);
        assert_eq!(right_panel_width(2000.0, Some(900.0), 100.0), 520.0);
        assert_eq!(right_panel_width(2000.0, Some(400.0), 100.0), 400.0);
        // Not enough room: down to the minimum, never below.
        assert_eq!(right_panel_width(1024.0, Some(500.0), 400.0), 240.0);
        assert_eq!(right_panel_width(1024.0, Some(500.0), 100.0), 500.0);
    }
}
