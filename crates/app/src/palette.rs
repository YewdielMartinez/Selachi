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
    pub const TAB_NEW_TERMINAL: &str = "tab.new_terminal";
    pub const CHAT_NEW: &str = "chat.new";
    pub const TAB_CLOSE: &str = "tab.close";
    pub const TAB_NEXT: &str = "tab.next";
    pub const TAB_PREV: &str = "tab.prev";
    pub const LAYOUT_TOGGLE: &str = "layout.toggle";
    pub const THEME_TOGGLE: &str = "theme.toggle";
    pub const FULLSCREEN: &str = "window.fullscreen";
    pub const REVIEW_OPEN: &str = "review.open";
    pub const ATTENTION_NEXT: &str = "attention.next";
    pub const SETTINGS_OPEN: &str = "settings.open";
    pub const SIDEBAR_TOGGLE: &str = "sidebar.toggle";
    pub const AGENTMAP_OPEN: &str = "agentmap.open";
    pub const PROJECT_ADD: &str = "project.add";
    pub const PANEL_PREFIX: &str = "panel.";
}

/// The actions of the palette's `>` scope. Panels are added by the shell from the registry.
pub fn base_actions() -> Vec<ActionEntry> {
    vec![
        ActionEntry::new(id::TAB_NEW, "Nueva pestaña")
            .icon(Icon::Plus)
            .keywords("terminal chat crear abrir"),
        ActionEntry::new(id::TAB_NEW_TERMINAL, "Nueva terminal")
            .icon(Icon::Terminal)
            .keywords("pestaña shell crear abrir"),
        ActionEntry::new(id::CHAT_NEW, "Nuevo chat con un agente")
            .icon(Icon::Agents)
            .keywords("hilo conversación claude codex opencode agente"),
        ActionEntry::new(id::PROJECT_ADD, "Agregar proyecto…")
            .icon(Icon::Folder)
            .keywords("carpeta proyecto abrir crear nuevo espacio"),
        ActionEntry::new(id::TAB_CLOSE, "Cerrar pestaña")
            .icon(Icon::Close)
            .keywords("terminal"),
        ActionEntry::new(id::TAB_NEXT, "Pestaña siguiente").icon(Icon::ChevronRight),
        ActionEntry::new(id::TAB_PREV, "Pestaña anterior").icon(Icon::ChevronRight),
        ActionEntry::new(id::LAYOUT_TOGGLE, "Panel de layout")
            .icon(Icon::Layout)
            .keywords("diseño preset"),
        ActionEntry::new(id::THEME_TOGGLE, "Cambiar tema")
            .icon(Icon::Settings)
            .keywords("claro oscuro oled"),
        ActionEntry::new(id::REVIEW_OPEN, "Revisar cambios")
            .icon(Icon::Review)
            .keywords("diff notas agente review"),
        ActionEntry::new(id::ATTENTION_NEXT, "Siguiente en atención")
            .icon(Icon::Attention)
            .keywords("cola espera aprobación"),
        ActionEntry::new(id::SETTINGS_OPEN, "Ajustes")
            .icon(Icon::Settings)
            .keywords("configuración opciones preferencias"),
        ActionEntry::new(id::AGENTMAP_OPEN, "Mapa de subagentes")
            .icon(ui_agentmap::MAP_ICON)
            .keywords("delegaciones árbol grafo agentes harness orquestador"),
        ActionEntry::new(id::SIDEBAR_TOGGLE, "Plegar la barra lateral")
            .icon(Icon::Layout)
            .keywords("sidebar iconos colapsar"),
        ActionEntry::new(id::FULLSCREEN, "Pantalla completa").icon(Icon::Maximize),
    ]
}

/// The palette's actions: the host's, one per right-panel provider, each with the shortcut it
/// has **now** (`overrides` is `[keybindings]`; an unbound or clashing action shows none).
pub fn palette_actions(
    overrides: &std::collections::BTreeMap<String, String>,
    cx: &gpui::App,
) -> Vec<ActionEntry> {
    let mut actions: Vec<ActionEntry> = base_actions()
        .into_iter()
        .map(|mut e| {
            e.shortcut = crate::keybindings::shortcut_label(overrides, &e.id).map(Into::into);
            e
        })
        .collect();
    if let Some(reg) = cx.try_global::<panel_api::PanelRegistry>() {
        for p in reg.for_zone(panel_api::PanelZone::Right) {
            // The map has its own action (it also picks the session to show).
            if p.id().as_str() == ui_agentmap::PANEL_ID {
                continue;
            }
            actions.push(
                ActionEntry::new(
                    format!("{}{}", id::PANEL_PREFIX, p.id().as_str()),
                    format!("Panel: {}", p.title()),
                )
                .icon(p.icon()),
            );
        }
    }
    actions
}

/// A PowerShell single-quoted literal (`'it''s'`).
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `Set-Location -LiteralPath '<dir>'`, the line that changes directory in PowerShell.
pub fn cd_line(dir: &Path) -> String {
    format!(
        "Set-Location -LiteralPath {}",
        ps_quote(&dir.to_string_lossy())
    )
}

/// `git -C <cwd> ...` when `cwd` is not where the terminal already is: runs the panel's command in
/// its worktree without changing the shell's directory. Anything but `git` is left alone.
pub fn with_dash_c(argv: &[String], cwd: &Path, terminal_cwd: Option<&Path>) -> Vec<String> {
    let same = terminal_cwd
        .is_some_and(|t| crate::paths::is_within(t, cwd) && crate::paths::is_within(cwd, t));
    match argv.split_first() {
        Some((git, rest)) if git == "git" && !same => {
            let mut out = vec![
                git.clone(),
                "-C".to_string(),
                cwd.to_string_lossy().into_owned(),
            ];
            out.extend_from_slice(rest);
            out
        }
        _ => argv.to_vec(),
    }
}

/// A shell line for an argv: arguments that need it are quoted.
pub fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:=@\\".contains(c))
            {
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
        assert_eq!(
            cd_line(Path::new("C:\\a b\\o'k")),
            "Set-Location -LiteralPath 'C:\\a b\\o''k'"
        );
        let argv = ["git", "commit", "-m", "fix: a b"].map(str::to_string);
        assert_eq!(shell_line(&argv), "git commit -m 'fix: a b'");
        let argv = ["git", "switch", "feature/x"].map(str::to_string);
        assert_eq!(shell_line(&argv), "git switch feature/x");
    }

    #[test]
    fn git_commands_target_their_worktree() {
        let argv = ["git", "commit", "-m", "x"].map(str::to_string);
        let there = with_dash_c(&argv, Path::new(r"C:\repo"), Some(Path::new(r"C:\other")));
        assert_eq!(&there[..3], ["git", "-C", r"C:\repo"]);
        assert_eq!(there.len(), 6);
        let here = with_dash_c(&argv, Path::new(r"C:\repo"), Some(Path::new(r"C:\repo")));
        assert_eq!(here, argv);
        let not_git = ["dotnet", "build"].map(str::to_string);
        assert_eq!(with_dash_c(&not_git, Path::new(r"C:\repo"), None), not_git);
    }

    #[test]
    fn actions_have_unique_ids() {
        let a = base_actions();
        for (i, x) in a.iter().enumerate() {
            assert!(a.iter().skip(i + 1).all(|y| y.id != x.id), "{}", x.id);
        }
    }
}
