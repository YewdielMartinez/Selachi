//! Keyboard shortcuts: the action catalog, chord normalization, capture and conflict detection.
//!
//! `store::Config.keybindings` stores overrides only (`action id -> "ctrl+shift+t"`); the app owns
//! the defaults. The settings view therefore needs the catalog of actions with their default
//! chord ([`default_catalog`], replaceable by the host through `SettingsView::set_key_catalog`).
//! A binding equal to the default is not stored.

use std::collections::BTreeMap;

/// One rebindable action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyAction {
    /// Stable id (what `[keybindings]` keys are): `tab.new`.
    pub id: String,
    /// Label shown in the list.
    pub label: String,
    /// Default chord, normalized (`ctrl+shift+n`). Empty = unbound by default.
    pub default: String,
}

impl KeyAction {
    pub fn new(id: &str, label: &str, default: &str) -> Self {
        Self { id: id.into(), label: label.into(), default: default.into() }
    }
}

/// The actions the app binds today (ids match the palette's).
pub fn default_catalog() -> Vec<KeyAction> {
    vec![
        KeyAction::new("palette.toggle", "Paleta de comandos", "ctrl+k"),
        KeyAction::new("tab.new", "Nueva pestaña", "ctrl+shift+n"),
        KeyAction::new("tab.close", "Cerrar pestaña", "ctrl+w"),
        KeyAction::new("tab.next", "Pestaña siguiente", "ctrl+tab"),
        KeyAction::new("tab.prev", "Pestaña anterior", "ctrl+shift+tab"),
        KeyAction::new("layout.toggle", "Panel de layout", "ctrl+shift+l"),
        KeyAction::new("theme.toggle", "Cambiar tema", "ctrl+shift+t"),
        KeyAction::new("window.fullscreen", "Pantalla completa", "f11"),
        KeyAction::new("settings.open", "Abrir Ajustes", "ctrl+,"),
        KeyAction::new("attention.next", "Siguiente agente que espera", "ctrl+shift+a"),
        KeyAction::new("block.prev", "Bloque anterior", "ctrl+up"),
        KeyAction::new("block.next", "Bloque siguiente", "ctrl+down"),
    ]
}

/// Why a captured keystroke cannot be a shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordError {
    /// Only modifiers were pressed.
    ModifierOnly,
    /// A bare letter / digit / punctuation key would steal typing from the terminal.
    NeedsModifier,
}

impl ChordError {
    pub fn message(&self) -> &'static str {
        match self {
            ChordError::ModifierOnly => "Pulsa también una tecla.",
            ChordError::NeedsModifier => "Usa Ctrl, Alt o Win, o una tecla de función.",
        }
    }
}

const MODIFIER_KEYS: &[&str] = &["control", "ctrl", "shift", "alt", "platform", "win", "super", "meta", "function"];

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n))
}

/// Build the normalized chord (`ctrl+alt+shift+win+key`, lowercase) from a keystroke.
pub fn chord_from_keystroke(ctrl: bool, alt: bool, shift: bool, win: bool, key: &str) -> Result<String, ChordError> {
    let key = key.trim().to_lowercase();
    if key.is_empty() || MODIFIER_KEYS.contains(&key.as_str()) {
        return Err(ChordError::ModifierOnly);
    }
    if !(ctrl || alt || win || is_function_key(&key)) {
        return Err(ChordError::NeedsModifier);
    }
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if win {
        parts.push("win");
    }
    parts.push(&key);
    Ok(parts.join("+"))
}

/// Normalize a hand-written chord (`Ctrl + Shift + T`, `ctrl-shift-t`, `Control+T`) to the
/// canonical form, or `None` when it has no key.
pub fn normalize_chord(text: &str) -> Option<String> {
    let t = text.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    // `ctrl+,` and `ctrl++` keep their key; split on `+` / `-` between known modifier words only.
    let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
    let mut rest = t.as_str();
    loop {
        let rest_trim = rest.trim_start();
        let mut matched = false;
        for (word, flag) in [
            ("ctrl", 0),
            ("control", 0),
            ("alt", 1),
            ("shift", 2),
            ("win", 3),
            ("super", 3),
            ("cmd", 3),
        ] {
            if let Some(after) = rest_trim.strip_prefix(word)
                && let Some(after) = after.trim_start().strip_prefix(['+', '-'])
            {
                match flag {
                    0 => ctrl = true,
                    1 => alt = true,
                    2 => shift = true,
                    _ => win = true,
                }
                rest = after;
                matched = true;
                break;
            }
        }
        if !matched {
            break;
        }
    }
    let key = rest.trim();
    if key.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if win {
        parts.push("win");
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// `ctrl+shift+t` -> `Ctrl+Shift+T`; `f11` -> `F11`.
pub fn display_chord(chord: &str) -> String {
    chord
        .split('+')
        .map(|p| match p {
            "" => "+".to_string(),
            "ctrl" => "Ctrl".into(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            "win" => "Win".into(),
            "pageup" => "Re Pág".into(),
            "pagedown" => "Av Pág".into(),
            other => {
                let mut cs = other.chars();
                match cs.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The chord in effect for `id`: the override or the default.
pub fn effective(catalog: &[KeyAction], overrides: &BTreeMap<String, String>, id: &str) -> String {
    if let Some(c) = overrides.get(id).and_then(|c| normalize_chord(c)) {
        return c;
    }
    catalog.iter().find(|a| a.id == id).map(|a| a.default.clone()).unwrap_or_default()
}

/// The *other* action already using `chord`, if any.
pub fn find_conflict<'a>(
    catalog: &'a [KeyAction],
    overrides: &BTreeMap<String, String>,
    id: &str,
    chord: &str,
) -> Option<&'a KeyAction> {
    catalog.iter().find(|a| a.id != id && effective(catalog, overrides, &a.id) == chord)
}

/// Bind `id` to `chord`. A chord equal to the default removes the override.
pub fn set_binding(catalog: &[KeyAction], overrides: &mut BTreeMap<String, String>, id: &str, chord: &str) {
    let default = catalog.iter().find(|a| a.id == id).map(|a| a.default.as_str()).unwrap_or("");
    if chord == default {
        overrides.remove(id);
    } else {
        overrides.insert(id.to_string(), chord.to_string());
    }
}

/// Give `chord` to `id` and hand `id`'s previous chord to the action that held `chord`.
pub fn swap_bindings(catalog: &[KeyAction], overrides: &mut BTreeMap<String, String>, id: &str, other: &str, chord: &str) {
    let previous = effective(catalog, overrides, id);
    set_binding(catalog, overrides, id, chord);
    set_binding(catalog, overrides, other, &previous);
}

/// Is any non-default binding stored?
pub fn is_overridden(overrides: &BTreeMap<String, String>, id: &str) -> bool {
    overrides.contains_key(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_are_built_in_canonical_order() {
        assert_eq!(chord_from_keystroke(true, false, true, false, "T").unwrap(), "ctrl+shift+t");
        assert_eq!(chord_from_keystroke(true, true, true, true, "x").unwrap(), "ctrl+alt+shift+win+x");
        assert_eq!(chord_from_keystroke(false, false, false, false, "F11").unwrap(), "f11");
        assert_eq!(chord_from_keystroke(true, false, false, false, "tab").unwrap(), "ctrl+tab");
    }

    #[test]
    fn captures_that_cannot_be_shortcuts_are_rejected() {
        assert_eq!(chord_from_keystroke(true, false, false, false, "shift"), Err(ChordError::ModifierOnly));
        assert_eq!(chord_from_keystroke(false, false, false, false, ""), Err(ChordError::ModifierOnly));
        assert_eq!(chord_from_keystroke(false, false, false, false, "a"), Err(ChordError::NeedsModifier));
        assert_eq!(chord_from_keystroke(false, false, true, false, "a"), Err(ChordError::NeedsModifier));
    }

    #[test]
    fn hand_written_chords_normalize() {
        assert_eq!(normalize_chord("Ctrl + Shift + T").as_deref(), Some("ctrl+shift+t"));
        assert_eq!(normalize_chord("shift-ctrl-t").as_deref(), Some("ctrl+shift+t"));
        assert_eq!(normalize_chord("Control+,").as_deref(), Some("ctrl+,"));
        assert_eq!(normalize_chord("F11").as_deref(), Some("f11"));
        assert_eq!(normalize_chord("ctrl+"), None);
        assert_eq!(normalize_chord("  "), None);
    }

    #[test]
    fn display_is_readable() {
        assert_eq!(display_chord("ctrl+shift+t"), "Ctrl+Shift+T");
        assert_eq!(display_chord("f11"), "F11");
        assert_eq!(display_chord("ctrl+pageup"), "Ctrl+Re Pág");
    }

    #[test]
    fn effective_uses_override_then_default() {
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        assert_eq!(effective(&cat, &ov, "tab.new"), "ctrl+shift+n");
        ov.insert("tab.new".into(), "Ctrl+T".into());
        assert_eq!(effective(&cat, &ov, "tab.new"), "ctrl+t");
        assert_eq!(effective(&cat, &ov, "nope"), "");
    }

    #[test]
    fn conflicts_are_found_against_defaults_and_overrides() {
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        // ctrl+w is tab.close by default.
        assert_eq!(find_conflict(&cat, &ov, "tab.new", "ctrl+w").unwrap().id, "tab.close");
        // Rebinding to its own chord is not a conflict.
        assert!(find_conflict(&cat, &ov, "tab.close", "ctrl+w").is_none());
        assert!(find_conflict(&cat, &ov, "tab.new", "ctrl+alt+9").is_none());
        // Once tab.close moves away, ctrl+w is free and its new chord is taken.
        ov.insert("tab.close".into(), "ctrl+alt+w".into());
        assert!(find_conflict(&cat, &ov, "tab.new", "ctrl+w").is_none());
        assert_eq!(find_conflict(&cat, &ov, "tab.new", "ctrl+alt+w").unwrap().id, "tab.close");
    }

    #[test]
    fn binding_the_default_drops_the_override_and_swap_exchanges() {
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        set_binding(&cat, &mut ov, "tab.new", "ctrl+t");
        assert_eq!(ov.get("tab.new").map(String::as_str), Some("ctrl+t"));
        set_binding(&cat, &mut ov, "tab.new", "ctrl+shift+n");
        assert!(ov.is_empty());

        // tab.new takes ctrl+w from tab.close; tab.close gets ctrl+shift+n.
        swap_bindings(&cat, &mut ov, "tab.new", "tab.close", "ctrl+w");
        assert_eq!(effective(&cat, &ov, "tab.new"), "ctrl+w");
        assert_eq!(effective(&cat, &ov, "tab.close"), "ctrl+shift+n");
        // No two actions share a chord afterwards.
        let mut seen = std::collections::HashSet::new();
        for a in &cat {
            assert!(seen.insert(effective(&cat, &ov, &a.id)), "duplicate chord for {}", a.id);
        }
    }

    #[test]
    fn default_catalog_has_unique_ids_and_chords() {
        let cat = default_catalog();
        let mut ids = std::collections::HashSet::new();
        let mut chords = std::collections::HashSet::new();
        for a in &cat {
            assert!(ids.insert(a.id.clone()));
            assert!(chords.insert(a.default.clone()), "{} repeats a chord", a.id);
            assert_eq!(normalize_chord(&a.default).as_deref(), Some(a.default.as_str()));
        }
    }
}
