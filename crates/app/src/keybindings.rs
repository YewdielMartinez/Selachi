//! The window's keyboard shortcuts, built from `[keybindings]` of `config.toml`.
//!
//! The catalog (ids, labels, default chords) lives in `ui_settings` ([`ui_settings::default_catalog`])
//! and is the only place a default chord is written: Teclado lists it and [`install`] binds from it,
//! so what Ajustes shows is what the window does. This module owns the other half: which real
//! action each id runs ([`target_exists`]). A config change (settings screen, hot reload, reset)
//! calls [`install`] again: it replaces only the bindings of the catalog and leaves the rest of
//! the keymap (palette navigation, text fields, viewer...) alone.

use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};

use gpui::{App, Global, KeyBinding, Keystroke};
use ui_settings::{default_catalog, effective, plan, to_gpui};

use crate::app_shell::{
    CancelClose, CancelOverlay, CloseTab, ConfirmClose, FocusSidebar, FocusTerminal, Jump1, Jump2,
    Jump3, Jump4, Jump5, Jump6, Jump7, Jump8, Jump9, NewTab, NewTerminalTab, NextAttention,
    NextTab, NextZone, OpenSettings, PrevTab, PrevZone, ToggleFullscreen, ToggleLayoutPanel,
    ToggleSidebar, ToggleTheme,
};
use crate::sidebar_view;

/// Ids handled by the terminal itself (it only scrolls when the shell has command blocks, and
/// otherwise the keys must reach the shell), so they are not gpui actions.
const BLOCK_PREV: &str = "block.prev";
const BLOCK_NEXT: &str = "block.next";

/// The gpui binding of the catalog id `id` on `k` (gpui keystroke syntax), or `None` when the id
/// has no action (or is a terminal hook, see [`block_direction`]).
fn action_binding(id: &str, k: &str) -> Option<KeyBinding> {
    Some(match id {
        "palette.toggle" => ui_palette::toggle_binding(k),
        "tab.new" => KeyBinding::new(k, NewTab, None),
        "tab.new_terminal" => KeyBinding::new(k, NewTerminalTab, None),
        "tab.close" => KeyBinding::new(k, CloseTab, None),
        "tab.next" => KeyBinding::new(k, NextTab, None),
        "tab.prev" => KeyBinding::new(k, PrevTab, None),
        "tab.jump.1" => KeyBinding::new(k, Jump1, None),
        "tab.jump.2" => KeyBinding::new(k, Jump2, None),
        "tab.jump.3" => KeyBinding::new(k, Jump3, None),
        "tab.jump.4" => KeyBinding::new(k, Jump4, None),
        "tab.jump.5" => KeyBinding::new(k, Jump5, None),
        "tab.jump.6" => KeyBinding::new(k, Jump6, None),
        "tab.jump.7" => KeyBinding::new(k, Jump7, None),
        "tab.jump.8" => KeyBinding::new(k, Jump8, None),
        "tab.jump.9" => KeyBinding::new(k, Jump9, None),
        "layout.toggle" => KeyBinding::new(k, ToggleLayoutPanel, None),
        "theme.toggle" => KeyBinding::new(k, ToggleTheme, None),
        "window.fullscreen" => KeyBinding::new(k, ToggleFullscreen, None),
        "settings.open" => KeyBinding::new(k, OpenSettings, None),
        "sidebar.toggle" => KeyBinding::new(k, ToggleSidebar, None),
        "attention.next" => KeyBinding::new(k, NextAttention, None),
        "zone.next" => KeyBinding::new(k, NextZone, None),
        "zone.prev" => KeyBinding::new(k, PrevZone, None),
        "focus.sidebar" => KeyBinding::new(k, FocusSidebar, None),
        "focus.terminal" => KeyBinding::new(k, FocusTerminal, None),
        _ => return None,
    })
}

/// Does the catalog id `id` run something real?
pub fn target_exists(id: &str) -> bool {
    id == BLOCK_PREV || id == BLOCK_NEXT || action_binding(id, "f1").is_some()
}

/// The bindings that are not rebindable (they belong to a focused row, a prompt or a drawer).
pub fn bind_static(cx: &mut App) {
    cx.bind_keys([
        // A focused sidebar row closes its tab (asks first when it is running).
        KeyBinding::new("delete", sidebar_view::CloseFocusedRow, Some("SidebarRow")),
        // A focused project header: its menu (new terminal / chat here, remove from the list).
        KeyBinding::new(
            "shift-f10",
            sidebar_view::OpenSpaceMenu,
            Some("SidebarSpace"),
        ),
        KeyBinding::new("menu", sidebar_view::OpenSpaceMenu, Some("SidebarSpace")),
        // The close prompt: Enter confirms, Esc cancels.
        KeyBinding::new("enter", ConfirmClose, Some("CloseConfirm")),
        KeyBinding::new("escape", CancelClose, Some("CloseConfirm")),
        // Drawers / overlays of the narrow layouts close with Esc.
        KeyBinding::new("escape", CancelOverlay, Some("Drawer")),
    ]);
}

/// The chords of the terminal's own shortcuts (previous / next command block).
struct BlockKeys {
    prev: Option<Keystroke>,
    next: Option<Keystroke>,
}

impl Global for BlockKeys {}

impl BlockKeys {
    fn of(bound: &[(String, String)]) -> Self {
        let find = |id: &str| {
            let chord = bound.iter().find(|(i, _)| i == id).map(|(_, c)| c)?;
            Keystroke::parse(&to_gpui(chord)).ok()
        };
        Self {
            prev: find(BLOCK_PREV),
            next: find(BLOCK_NEXT),
        }
    }
}

fn same(want: &Option<Keystroke>, ks: &Keystroke) -> bool {
    want.as_ref()
        .is_some_and(|w| w.key == ks.key && w.modifiers == ks.modifiers)
}

/// `-1` / `+1` when `ks` is the chord of "Bloque anterior" / "Bloque siguiente". Cheap: a couple
/// of comparisons, no allocation. Before [`install`] ran, the defaults apply.
pub fn block_direction(ks: &Keystroke, cx: &App) -> Option<i32> {
    let default;
    let keys = match cx.try_global::<BlockKeys>() {
        Some(k) => k,
        None => {
            let catalog = default_catalog();
            let (bound, _) = plan(&catalog, &BTreeMap::new());
            default = BlockKeys::of(&bound);
            &default
        }
    };
    if same(&keys.prev, ks) {
        Some(-1)
    } else if same(&keys.next, ks) {
        Some(1)
    } else {
        None
    }
}

/// (Re)binds every catalog shortcut from `overrides` (`config.keybindings`). Safe to call again
/// on every config change: only the catalog's own global bindings are replaced. Two actions on
/// one chord are logged and only one keeps it (see [`ui_settings::plan`]); Teclado shows it.
pub fn install(cx: &mut App, overrides: &BTreeMap<String, String>) {
    let catalog = default_catalog();
    for a in catalog.iter().filter(|a| !target_exists(&a.id)) {
        log::error!("[keybindings] {} is in the catalog but runs nothing", a.id);
    }
    let (bound, clashes) = plan(&catalog, overrides);
    for c in &clashes {
        log::warn!(
            "[keybindings] {} is the shortcut of {} and of {:?}: only {} keeps it",
            c.chord,
            c.winner,
            c.losers,
            c.winner
        );
    }
    let ours: HashSet<TypeId> = catalog
        .iter()
        .filter_map(|a| action_binding(&a.id, "f1"))
        .map(|b| b.action().as_any().type_id())
        .collect();
    let fresh: Vec<KeyBinding> = bound
        .iter()
        .filter_map(|(id, chord)| action_binding(id, &to_gpui(chord)))
        .collect();
    {
        let keymap = cx.key_bindings();
        let mut keymap = keymap.borrow_mut();
        let keep: Vec<KeyBinding> = keymap
            .bindings()
            .filter(|b| !(b.predicate().is_none() && ours.contains(&b.action().as_any().type_id())))
            .cloned()
            .collect();
        keymap.clear();
        keymap.add_bindings(keep);
    }
    // `bind_keys` also refreshes the windows.
    cx.bind_keys(fresh);
    cx.set_global(BlockKeys::of(&bound));
}

/// The shortcut text of an action id (`Ctrl+Shift+T`), `None` when it has none.
pub fn shortcut_label(overrides: &BTreeMap<String, String>, id: &str) -> Option<String> {
    let catalog = default_catalog();
    let chord = effective(&catalog, overrides, id);
    let (bound, _) = plan(&catalog, overrides);
    bound
        .iter()
        .any(|(i, _)| i == id)
        .then(|| ui_settings::display_chord(&chord))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn every_catalog_entry_runs_a_real_action() {
        for a in default_catalog() {
            assert!(target_exists(&a.id), "{} has no action", a.id);
        }
        assert!(!target_exists("nope"));
    }

    #[gpui::test]
    fn install_binds_the_defaults_and_follows_overrides(cx: &mut TestAppContext) {
        cx.update(|cx| {
            install(cx, &BTreeMap::new());
            let km = cx.key_bindings();
            let defaults = km.borrow().bindings().len();
            assert_eq!(
                defaults,
                default_catalog().len() - 2,
                "block keys are hooks"
            );
            let f11 = Keystroke::parse("f11").unwrap();
            assert_eq!(
                block_direction(&Keystroke::parse("ctrl-up").unwrap(), cx),
                Some(-1)
            );
            assert!(block_direction(&f11, cx).is_none());

            let mut ov = BTreeMap::new();
            ov.insert("block.prev".to_string(), "ctrl+alt+k".to_string());
            ov.insert("window.fullscreen".to_string(), "ctrl+alt+f".to_string());
            install(cx, &ov);
            assert_eq!(
                km.borrow().bindings().len(),
                defaults,
                "replaced, not duplicated"
            );
            assert_eq!(
                block_direction(&Keystroke::parse("ctrl-up").unwrap(), cx),
                None
            );
            assert_eq!(
                block_direction(&Keystroke::parse("ctrl-alt-k").unwrap(), cx),
                Some(-1)
            );
            assert_eq!(
                block_direction(&Keystroke::parse("ctrl-down").unwrap(), cx),
                Some(1)
            );
        });
    }
}
