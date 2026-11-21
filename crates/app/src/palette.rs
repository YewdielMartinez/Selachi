//! Wiring of the command palette (`Ctrl+K`, PLAN A1): the host actions it lists, the quoting of
//! shell lines, and the translation of git actions into lines typed at the prompt.
//!
//! The palette executes nothing: it emits a `PaletteEvent` and the shell handles it
//! (`AppShell::on_palette`).

use std::path::Path;

use ui_kit::Icon;
use ui_palette::ActionEntry;

/// Ids of the host actions (what `PaletteEvent::Action` carries).
pub mod id {
    pub const TAB_NEW: &str = "tab.new";
    pub const TAB_CLOSE: &str = "tab.close";
    pub const TAB_NEXT: &str = "tab.next";
    pub const TAB_PREV: &str = "tab.prev";
    pub const LAYOUT_TOGGLE: &str = "layout.toggle";
    pub const THEME_TOGGLE: &str = "theme.toggle";
    pub const FULLSCREEN: &str = "window.fullscreen";
    pub const PANEL_PREFIX: &str = "panel.";
}

/// The actions of the palette's `>` scope. Panels are added by the shell from the registry.
pub fn base_actions() -> Vec<ActionEntry> {
    vec![
        ActionEntry::new(id::TAB_NEW, "Nueva pestaña").shortcut("Ctrl+Shift+N").icon(Icon::Plus).keywords("terminal crear abrir"),
        ActionEntry::new(id::TAB_CLOSE, "Cerrar pestaña").shortcut("Ctrl+W").icon(Icon::Close).keywords("terminal"),
        ActionEntry::new(id::TAB_NEXT, "Pestaña siguiente").shortcut("Ctrl+Tab").icon(Icon::ChevronRight),
        ActionEntry::new(id::TAB_PREV, "Pestaña anterior").shortcut("Ctrl+Shift+Tab").icon(Icon::ChevronRight),
        ActionEntry::new(id::LAYOUT_TOGGLE, "Panel de layout").shortcut("Ctrl+Shift+L").icon(Icon::Layout).keywords("diseño preset"),
        ActionEntry::new(id::THEME_TOGGLE, "Cambiar tema").shortcut("Ctrl+Shift+T").icon(Icon::Settings).keywords("claro oscuro oled"),
        ActionEntry::new(id::FULLSCREEN, "Pantalla completa").shortcut("F11").icon(Icon::Maximize),
    ]
}

/// A PowerShell single-quoted literal (`'it''s'`).
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `Set-Location -LiteralPath '<dir>'`, the line that changes directory in PowerShell.
pub fn cd_line(dir: &Path) -> String {
    format!("Set-Location -LiteralPath {}", ps_quote(&dir.to_string_lossy()))
}

/// A shell line for an argv: arguments that need it are quoted.
pub fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@\\".contains(c)) {
                a.clone()
            } else {
                ps_quote(a)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(cd_line(Path::new("C:\\a b\\o'k")), "Set-Location -LiteralPath 'C:\\a b\\o''k'");
        let argv = ["git", "commit", "-m", "fix: a b"].map(str::to_string);
        assert_eq!(shell_line(&argv), "git commit -m 'fix: a b'");
        let argv = ["git", "switch", "feature/x"].map(str::to_string);
        assert_eq!(shell_line(&argv), "git switch feature/x");
    }

    #[test]
    fn actions_have_unique_ids() {
        let a = base_actions();
        for (i, x) in a.iter().enumerate() {
            assert!(a.iter().skip(i + 1).all(|y| y.id != x.id), "{}", x.id);
        }
    }
}
