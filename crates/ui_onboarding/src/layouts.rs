//! The layout cards of step 4, including "Espacios unificados" (docs/olas/ola-5.md §5.10).
//!
//! `store` has no `espacios_unificados` preset yet (wave 5.10 adds it). Until then the card is
//! described here (label and wireframe) and choosing it writes the closest existing preset,
//! `espacios`, plus `Extras::unified_layout = true` so the host can apply the real layout.
//! As soon as `BuiltinPreset::from_id("espacios_unificados")` resolves, the card writes that id
//! and the flag stays false: no change is needed here.

use store::{BuiltinPreset, PresetId};
use ui_kit::{WireFill, WirePreset, WireRect};

/// Config id of the new default preset.
pub const UNIFIED_ID: &str = "espacios_unificados";

/// One card of the layout picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutCard {
    Unified,
    Builtin(BuiltinPreset),
}

impl LayoutCard {
    pub const fn label(self) -> &'static str {
        match self {
            LayoutCard::Unified => "Espacios unificados",
            LayoutCard::Builtin(BuiltinPreset::Terminal) => "Terminal",
            LayoutCard::Builtin(BuiltinPreset::Espacios) => "Espacios",
            LayoutCard::Builtin(BuiltinPreset::Orquestador) => "Orquestador",
            LayoutCard::Builtin(BuiltinPreset::Chat) => "Chat",
            LayoutCard::Builtin(BuiltinPreset::Zen) => "Zen",
        }
    }

    pub fn is_recommended(self) -> bool {
        self == LayoutCard::Unified
    }

    /// Pixel-art blocks on the default 32 x 20 grid.
    pub fn rects(self) -> Vec<WireRect> {
        match self {
            LayoutCard::Unified => unified_rects(),
            LayoutCard::Builtin(b) => builtin_wire(b).rects(),
        }
    }
}

fn builtin_wire(b: BuiltinPreset) -> WirePreset {
    match b {
        BuiltinPreset::Terminal => WirePreset::Terminal,
        BuiltinPreset::Espacios => WirePreset::Spaces,
        BuiltinPreset::Orquestador => WirePreset::Orchestrator,
        BuiltinPreset::Chat => WirePreset::Chat,
        BuiltinPreset::Zen => WirePreset::Zen,
    }
}

/// Wireframe of "Espacios unificados": a full-height sidebar (actions, panel icons, spaces with
/// nested tab rows, usage footer), a titlebar with the tab title on the left and the panel
/// buttons grouped on the right, and the work card. No rail, no tab strip.
pub fn unified_rects() -> Vec<WireRect> {
    use WireFill::{Dither, Outline, Solid};
    let r = WireRect::new;
    vec![
        r(0, 0, 9, 20, Dither),    // sidebar
        r(1, 1, 7, 1, Solid),      // new tab / search actions
        r(1, 3, 7, 1, Solid),      // panel icon row
        r(1, 6, 7, 1, Solid),      // space
        r(2, 8, 5, 1, Solid),      // nested tab
        r(2, 10, 5, 1, Solid),     // nested tab
        r(1, 12, 7, 1, Solid),     // space
        r(1, 18, 7, 1, Solid),     // usage footer
        r(10, 0, 8, 2, Solid),     // active tab title
        r(24, 0, 2, 2, Solid),     // panel buttons, grouped on the right
        r(27, 0, 2, 2, Solid),
        r(30, 0, 2, 2, Dither),
        r(10, 3, 22, 17, Outline), // work card
    ]
}

/// Cards in display order: the recommended one first. A builtin whose id is the unified one
/// (once `store` has it) is not shown twice.
pub fn cards() -> Vec<LayoutCard> {
    let mut v = vec![LayoutCard::Unified];
    v.extend(BuiltinPreset::ALL.into_iter().filter(|b| b.id() != UNIFIED_ID).map(LayoutCard::Builtin));
    v
}

/// The preset `store` knows for "Espacios unificados", if any.
pub fn store_unified() -> Option<BuiltinPreset> {
    BuiltinPreset::from_id(UNIFIED_ID)
}

/// What to write for `card`: the preset id and whether the host must still apply the unified
/// layout itself.
pub fn resolve(card: LayoutCard) -> (PresetId, bool) {
    match card {
        LayoutCard::Builtin(b) => (PresetId::Builtin(b), false),
        LayoutCard::Unified => match store_unified() {
            Some(b) => (PresetId::Builtin(b), false),
            None => (PresetId::Builtin(BuiltinPreset::Espacios), true),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_comes_first_and_is_the_only_recommended_card() {
        let c = cards();
        assert_eq!(c[0], LayoutCard::Unified);
        assert_eq!(c.iter().filter(|c| c.is_recommended()).count(), 1);
        assert!(c.len() >= 6);
    }

    #[test]
    fn unified_wireframe_fits_the_grid() {
        for r in unified_rects() {
            assert!(u16::from(r.x) + u16::from(r.w) <= 32, "{r:?}");
            assert!(u16::from(r.y) + u16::from(r.h) <= 20, "{r:?}");
        }
    }

    #[test]
    fn unified_falls_back_to_espacios_until_store_has_it() {
        let (id, host_applies) = resolve(LayoutCard::Unified);
        match store_unified() {
            None => {
                assert_eq!(id, PresetId::Builtin(BuiltinPreset::Espacios));
                assert!(host_applies);
            }
            Some(b) => {
                assert_eq!(id, PresetId::Builtin(b));
                assert!(!host_applies);
            }
        }
        assert_eq!(resolve(LayoutCard::Builtin(BuiltinPreset::Chat)), (PresetId::Builtin(BuiltinPreset::Chat), false));
    }
}
