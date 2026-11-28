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
        Self {
            id: id.into(),
            label: label.into(),
            default: default.into(),
        }
    }
}

/// Every shortcut the app binds, in the order Teclado lists them. This is the **single source of
/// truth** for ids, labels and default chords: the app builds its key bindings from it (see
/// `selachi::keybindings`, which also checks that every id here has a real action), so a default
/// cannot drift between what Ajustes shows and what the window does. Ids match the palette's.
pub fn default_catalog() -> Vec<KeyAction> {
    let mut v = vec![
        KeyAction::new("palette.toggle", "Paleta de comandos", "ctrl+k"),
        KeyAction::new("tab.new", "Nueva pestaña (terminal o chat)", "ctrl+n"),
        KeyAction::new("tab.new_terminal", "Nueva terminal", "ctrl+shift+n"),
        KeyAction::new("tab.close", "Cerrar pestaña", "ctrl+w"),
        KeyAction::new("tab.next", "Pestaña siguiente", "ctrl+tab"),
        KeyAction::new("tab.prev", "Pestaña anterior", "ctrl+shift+tab"),
    ];
    for n in 1..=9 {
        v.push(KeyAction::new(
            &format!("tab.jump.{n}"),
            &format!("Ir a la pestaña {n}"),
            &format!("ctrl+{n}"),
        ));
    }
    v.extend([
        KeyAction::new("layout.toggle", "Panel de layout", "ctrl+shift+l"),
        KeyAction::new("theme.toggle", "Cambiar tema", "ctrl+shift+t"),
        KeyAction::new("window.fullscreen", "Pantalla completa", "f11"),
        KeyAction::new("settings.open", "Abrir Ajustes", "ctrl+,"),
        KeyAction::new("sidebar.toggle", "Plegar la barra lateral", "ctrl+b"),
        KeyAction::new(
            "attention.next",
            "Siguiente agente que espera",
            "ctrl+shift+a",
        ),
        KeyAction::new("zone.next", "Siguiente zona (barra, terminal, panel)", "f6"),
        KeyAction::new("zone.prev", "Zona anterior", "shift+f6"),
        KeyAction::new("focus.sidebar", "Foco en la barra lateral", "ctrl+shift+e"),
        KeyAction::new("focus.terminal", "Foco en la terminal", "ctrl+shift+`"),
        KeyAction::new("block.prev", "Bloque anterior", "ctrl+up"),
        KeyAction::new("block.next", "Bloque siguiente", "ctrl+down"),
    ]);
    v
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

const MODIFIER_KEYS: &[&str] = &[
    "control", "ctrl", "shift", "alt", "platform", "win", "super", "meta", "function",
];

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

/// Build the normalized chord (`ctrl+alt+shift+win+key`, lowercase) from a keystroke.
pub fn chord_from_keystroke(
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
    key: &str,
) -> Result<String, ChordError> {
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

/// Can `chord` (canonical form) be a global shortcut? It must parse as a keystroke and either
/// carry Ctrl / Alt / Win or be a function key: a bare letter would steal typing from the
/// terminal. Hand-edited `[keybindings]` can hold anything, so the app checks before binding.
pub fn is_bindable(chord: &str) -> bool {
    let Some((ctrl, alt, _shift, win, key)) = split_chord(chord) else {
        return false;
    };
    (ctrl || alt || win || is_function_key(key)) && gpui::Keystroke::parse(&to_gpui(chord)).is_ok()
}

/// `(ctrl, alt, shift, win, key)` of a canonical chord (`ctrl+shift+t`, `ctrl++`).
fn split_chord(chord: &str) -> Option<(bool, bool, bool, bool, &str)> {
    let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
    let mut rest = chord;
    loop {
        if let Some(r) = rest.strip_prefix("ctrl+") {
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("alt+") {
            alt = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("shift+") {
            shift = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("win+") {
            win = true;
            rest = r;
        } else {
            break;
        }
    }
    (!rest.is_empty()).then_some((ctrl, alt, shift, win, rest))
}

/// The chord in gpui's keystroke syntax: `ctrl+shift+t` -> `ctrl-shift-t`, `ctrl+,` -> `ctrl-,`.
pub fn to_gpui(chord: &str) -> String {
    let Some((ctrl, alt, shift, win, key)) = split_chord(chord) else {
        return chord.to_string();
    };
    let mut out = String::new();
    for (on, name) in [(ctrl, "ctrl"), (alt, "alt"), (shift, "shift"), (win, "win")] {
        if on {
            out.push_str(name);
            out.push('-');
        }
    }
    out.push_str(key);
    out
}

/// The chord in effect for `id`: the override (when it can be a shortcut) or the default.
pub fn effective(catalog: &[KeyAction], overrides: &BTreeMap<String, String>, id: &str) -> String {
    if let Some(c) = overrides
        .get(id)
        .and_then(|c| normalize_chord(c))
        .filter(|c| is_bindable(c))
    {
        return c;
    }
    catalog
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.default.clone())
        .unwrap_or_default()
}

/// Two or more actions that ended up on the same chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clash {
    pub chord: String,
    /// The action that keeps the chord: one the user bound explicitly beats a default; among
    /// equals the first of the catalog.
    pub winner: String,
    /// The actions that lost it: they are not bound until one of them changes.
    pub losers: Vec<String>,
}

/// The shortcuts the window should have: `(action id, chord)` for every action with a chord that
/// does not lose a [`Clash`], plus the clashes themselves. Never two actions on one chord.
pub fn plan(
    catalog: &[KeyAction],
    overrides: &BTreeMap<String, String>,
) -> (Vec<(String, String)>, Vec<Clash>) {
    let mut by_chord: Vec<(String, Vec<&KeyAction>)> = Vec::new();
    for a in catalog {
        let chord = effective(catalog, overrides, &a.id);
        if chord.is_empty() {
            continue;
        }
        match by_chord.iter_mut().find(|(c, _)| *c == chord) {
            Some((_, v)) => v.push(a),
            None => by_chord.push((chord, vec![a])),
        }
    }
    let mut bound = Vec::new();
    let mut clashes = Vec::new();
    for (chord, group) in by_chord {
        let winner = group
            .iter()
            .find(|a| overrides.contains_key(&a.id))
            .or(group.first())
            .map(|a| a.id.clone())
            .unwrap_or_default();
        if group.len() > 1 {
            clashes.push(Clash {
                chord: chord.clone(),
                winner: winner.clone(),
                losers: group
                    .iter()
                    .filter(|a| a.id != winner)
                    .map(|a| a.id.clone())
                    .collect(),
            });
        }
        bound.push((winner, chord));
    }
    // Keep the catalog order (stable bindings, stable tests).
    bound.sort_by_key(|(id, _)| catalog.iter().position(|a| &a.id == id));
    (bound, clashes)
}

/// What Teclado says on the row of `id` when it is part of a [`Clash`] (hand-edited config:
/// the UI itself never creates one), or `None`.
pub fn clash_note(catalog: &[KeyAction], clashes: &[Clash], id: &str) -> Option<String> {
    let label = |i: &str| {
        catalog
            .iter()
            .find(|a| a.id == i)
            .map_or_else(|| i.to_string(), |a| a.label.clone())
    };
    let clash = clashes
        .iter()
        .find(|c| c.winner == id || c.losers.iter().any(|l| l == id))?;
    let chord = display_chord(&clash.chord);
    Some(if clash.winner == id {
        let others: Vec<String> = clash
            .losers
            .iter()
            .map(|l| format!("«{}»", label(l)))
            .collect();
        format!(
            "{chord} también está en {}: solo funciona aquí. Cambia el otro atajo.",
            others.join(", ")
        )
    } else {
        format!(
            "{chord} ya es el atajo de «{}»: este no funciona hasta que cambies uno de los dos.",
            label(&clash.winner)
        )
    })
}

/// The *other* action already using `chord`, if any.
pub fn find_conflict<'a>(
    catalog: &'a [KeyAction],
    overrides: &BTreeMap<String, String>,
    id: &str,
    chord: &str,
) -> Option<&'a KeyAction> {
    catalog
        .iter()
        .find(|a| a.id != id && effective(catalog, overrides, &a.id) == chord)
}

/// Bind `id` to `chord`. A chord equal to the default removes the override.
pub fn set_binding(
    catalog: &[KeyAction],
    overrides: &mut BTreeMap<String, String>,
    id: &str,
    chord: &str,
) {
    let default = catalog
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.default.as_str())
        .unwrap_or("");
    if chord == default {
        overrides.remove(id);
    } else {
        overrides.insert(id.to_string(), chord.to_string());
    }
}

/// Give `chord` to `id` and hand `id`'s previous chord to the action that held `chord`.
pub fn swap_bindings(
    catalog: &[KeyAction],
    overrides: &mut BTreeMap<String, String>,
    id: &str,
    other: &str,
    chord: &str,
) {
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
        assert_eq!(
            chord_from_keystroke(true, false, true, false, "T").unwrap(),
            "ctrl+shift+t"
        );
        assert_eq!(
            chord_from_keystroke(true, true, true, true, "x").unwrap(),
            "ctrl+alt+shift+win+x"
        );
        assert_eq!(
            chord_from_keystroke(false, false, false, false, "F11").unwrap(),
            "f11"
        );
        assert_eq!(
            chord_from_keystroke(true, false, false, false, "tab").unwrap(),
            "ctrl+tab"
        );
    }

    #[test]
    fn captures_that_cannot_be_shortcuts_are_rejected() {
        assert_eq!(
            chord_from_keystroke(true, false, false, false, "shift"),
            Err(ChordError::ModifierOnly)
        );
        assert_eq!(
            chord_from_keystroke(false, false, false, false, ""),
            Err(ChordError::ModifierOnly)
        );
        assert_eq!(
            chord_from_keystroke(false, false, false, false, "a"),
            Err(ChordError::NeedsModifier)
        );
        assert_eq!(
            chord_from_keystroke(false, false, true, false, "a"),
            Err(ChordError::NeedsModifier)
        );
    }

    #[test]
    fn hand_written_chords_normalize() {
        assert_eq!(
            normalize_chord("Ctrl + Shift + T").as_deref(),
            Some("ctrl+shift+t")
        );
        assert_eq!(
            normalize_chord("shift-ctrl-t").as_deref(),
            Some("ctrl+shift+t")
        );
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
        assert_eq!(effective(&cat, &ov, "tab.new_terminal"), "ctrl+shift+n");
        ov.insert("tab.new".into(), "Ctrl+T".into());
        assert_eq!(effective(&cat, &ov, "tab.new"), "ctrl+t");
        assert_eq!(effective(&cat, &ov, "nope"), "");
    }

    #[test]
    fn conflicts_are_found_against_defaults_and_overrides() {
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        // ctrl+w is tab.close by default.
        assert_eq!(
            find_conflict(&cat, &ov, "tab.new", "ctrl+w").unwrap().id,
            "tab.close"
        );
        // Rebinding to its own chord is not a conflict.
        assert!(find_conflict(&cat, &ov, "tab.close", "ctrl+w").is_none());
        assert!(find_conflict(&cat, &ov, "tab.new", "ctrl+alt+9").is_none());
        // Once tab.close moves away, ctrl+w is free and its new chord is taken.
        ov.insert("tab.close".into(), "ctrl+alt+w".into());
        assert!(find_conflict(&cat, &ov, "tab.new", "ctrl+w").is_none());
        assert_eq!(
            find_conflict(&cat, &ov, "tab.new", "ctrl+alt+w")
                .unwrap()
                .id,
            "tab.close"
        );
    }

    #[test]
    fn binding_the_default_drops_the_override_and_swap_exchanges() {
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        set_binding(&cat, &mut ov, "tab.new", "ctrl+t");
        assert_eq!(ov.get("tab.new").map(String::as_str), Some("ctrl+t"));
        set_binding(&cat, &mut ov, "tab.new", "ctrl+n");
        assert!(ov.is_empty());

        // tab.new takes ctrl+w from tab.close; tab.close gets ctrl+n.
        swap_bindings(&cat, &mut ov, "tab.new", "tab.close", "ctrl+w");
        assert_eq!(effective(&cat, &ov, "tab.new"), "ctrl+w");
        assert_eq!(effective(&cat, &ov, "tab.close"), "ctrl+n");
        // No two actions share a chord afterwards.
        let mut seen = std::collections::HashSet::new();
        for a in &cat {
            assert!(
                seen.insert(effective(&cat, &ov, &a.id)),
                "duplicate chord for {}",
                a.id
            );
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
            assert_eq!(
                normalize_chord(&a.default).as_deref(),
                Some(a.default.as_str())
            );
        }
    }

    #[test]
    fn every_default_can_be_bound() {
        for a in default_catalog() {
            assert!(is_bindable(&a.default), "{} -> {}", a.id, a.default);
        }
    }

    #[test]
    fn chords_translate_to_gpui_syntax() {
        assert_eq!(to_gpui("ctrl+shift+t"), "ctrl-shift-t");
        assert_eq!(to_gpui("f11"), "f11");
        assert_eq!(to_gpui("ctrl+,"), "ctrl-,");
        assert_eq!(to_gpui("ctrl+shift+`"), "ctrl-shift-`");
        assert_eq!(to_gpui("ctrl+-"), "ctrl--");
        assert_eq!(to_gpui("ctrl++"), "ctrl-+");
        for c in [
            "ctrl+shift+t",
            "f11",
            "ctrl+,",
            "ctrl+shift+`",
            "ctrl+-",
            "ctrl++",
        ] {
            assert!(gpui::Keystroke::parse(&to_gpui(c)).is_ok(), "{c}");
        }
    }

    #[test]
    fn bare_keys_cannot_be_shortcuts() {
        assert!(!is_bindable("a"));
        assert!(!is_bindable("shift+a"));
        assert!(!is_bindable("enter"));
        assert!(is_bindable("f6"));
        assert!(is_bindable("alt+a"));
        // A hand-edited bare key is ignored: the default stays in effect.
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        ov.insert("tab.close".into(), "a".into());
        assert_eq!(effective(&cat, &ov, "tab.close"), "ctrl+w");
    }

    #[test]
    fn the_plan_binds_every_action_once_and_reports_clashes() {
        let cat = default_catalog();
        let (bound, clashes) = plan(&cat, &BTreeMap::new());
        assert!(clashes.is_empty());
        assert_eq!(bound.len(), cat.len(), "every default is bound");

        // theme.toggle is hand-edited onto ctrl+w, which tab.close has by default.
        let mut ov = BTreeMap::new();
        ov.insert("theme.toggle".to_string(), "ctrl+w".to_string());
        let (bound, clashes) = plan(&cat, &ov);
        assert_eq!(
            clashes,
            vec![Clash {
                chord: "ctrl+w".into(),
                winner: "theme.toggle".into(),
                losers: vec!["tab.close".into()],
            }]
        );
        let ctrl_w: Vec<_> = bound.iter().filter(|(_, c)| c == "ctrl+w").collect();
        assert_eq!(ctrl_w.len(), 1, "never two actions on one chord");
        assert_eq!(ctrl_w[0].0, "theme.toggle");
        assert!(bound.iter().all(|(id, _)| id != "tab.close"));
        assert!(
            bound.iter().any(|(id, _)| id == "tab.next"),
            "the rest still work"
        );
    }

    #[test]
    fn two_defaults_or_two_overrides_on_one_chord_keep_the_first_in_the_catalog() {
        let cat = vec![
            KeyAction::new("a", "A", "ctrl+x"),
            KeyAction::new("b", "B", "ctrl+y"),
        ];
        let mut ov = BTreeMap::new();
        ov.insert("b".to_string(), "ctrl+x".to_string());
        // `b` was chosen by the user, `a` is a default: `b` wins.
        let (bound, clashes) = plan(&cat, &ov);
        assert_eq!(bound, vec![("b".to_string(), "ctrl+x".to_string())]);
        assert_eq!(clashes[0].losers, vec!["a".to_string()]);
        ov.insert("a".to_string(), "ctrl+x".to_string());
        let (bound, _) = plan(&cat, &ov);
        assert_eq!(bound, vec![("a".to_string(), "ctrl+x".to_string())]);
    }

    #[test]
    fn clash_notes_name_the_other_action() {
        let cat = default_catalog();
        let mut ov = BTreeMap::new();
        ov.insert("theme.toggle".to_string(), "ctrl+w".to_string());
        let (_, clashes) = plan(&cat, &ov);
        let loser = clash_note(&cat, &clashes, "tab.close").unwrap();
        assert!(
            loser.contains("Cambiar tema") && loser.contains("Ctrl+W"),
            "{loser}"
        );
        let winner = clash_note(&cat, &clashes, "theme.toggle").unwrap();
        assert!(winner.contains("Cerrar pestaña"), "{winner}");
        assert_eq!(clash_note(&cat, &clashes, "tab.next"), None);
    }
}
