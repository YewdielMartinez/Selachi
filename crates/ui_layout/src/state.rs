//! Pure editing logic of the layout panel (no gpui): which layout is being edited (global or
//! one project), how a change is stored (only the diff against the preset) and which
//! corrections `ResolvedLayout::normalize` made, in Spanish.

use std::path::{Path, PathBuf};

use store::paths::normalize_key;
use store::{
    BuiltinPreset, CustomPreset, LayoutConfig, LayoutOverrides, PresetId, RailVisibility,
    ResolvedLayout, SidebarSection, SidebarSide, TabsPosition,
};

/// Which layer the panel edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    /// `[layout]`: preset + global overrides.
    #[default]
    Global,
    /// `[[layout.project_overrides]]` of the working directory.
    Project,
}

/// Every sidebar section, in the canonical order shown for the disabled ones.
pub const ALL_SECTIONS: [SidebarSection; 5] = [
    SidebarSection::Projects,
    SidebarSection::Worktrees,
    SidebarSection::Agents,
    SidebarSection::Threads,
    SidebarSection::Spaces,
];

pub fn section_label(s: SidebarSection) -> &'static str {
    match s {
        SidebarSection::Projects => "Proyectos",
        SidebarSection::Worktrees => "Worktrees",
        SidebarSection::Agents => "Agentes",
        SidebarSection::Threads => "Hilos",
        SidebarSection::Spaces => "Espacios",
    }
}

pub fn builtin_name(b: BuiltinPreset) -> &'static str {
    match b {
        BuiltinPreset::Terminal => "Terminal",
        BuiltinPreset::Espacios => "Espacios",
        BuiltinPreset::Orquestador => "Orquestador",
        BuiltinPreset::Chat => "Chat",
        BuiltinPreset::Zen => "Zen",
        BuiltinPreset::EspaciosUnificados => "Unificado",
    }
}

pub fn builtin_description(b: BuiltinPreset) -> &'static str {
    match b {
        BuiltinPreset::Terminal => "Pestañas arriba, sin sidebar",
        BuiltinPreset::Espacios => "Sidebar de espacios y agentes",
        BuiltinPreset::Orquestador => "Proyecto, worktree y agentes",
        BuiltinPreset::Chat => "Hilos y panel de contexto",
        BuiltinPreset::Zen => "Solo la terminal",
        BuiltinPreset::EspaciosUnificados => "Sidebar con pestañas anidadas",
    }
}

/// Spanish messages for what `normalize` changed between `before` and `after`.
fn describe_corrections(before: &ResolvedLayout, after: &ResolvedLayout) -> Vec<String> {
    let mut out = Vec::new();
    if before.zen {
        if before.sidebar.side != SidebarSide::Hidden
            || before.rail != RailVisibility::Hidden
            || before.bottom_bar != store::BottomBar::Hidden
        {
            out.push(
                "Zen oculta la sidebar, el rail y la barra inferior mientras esté activo".into(),
            );
        }
        return out;
    }
    if before.tabs_position == TabsPosition::Sidebar
        && before.sidebar.side == SidebarSide::Hidden
        && after.sidebar.side != SidebarSide::Hidden
    {
        out.push("La sidebar se mostró porque las pestañas están en ella".into());
    }
    if before.rail == RailVisibility::Hidden && after.rail == RailVisibility::Visible {
        out.push("El rail se mostró porque la sidebar está oculta".into());
    }
    if before.sidebar.sections.is_empty()
        && after.sidebar.side != SidebarSide::Hidden
        && !after.sidebar.sections.is_empty()
    {
        out.push("La sidebar necesita al menos una sección; se activó Proyectos".into());
    }
    out
}

/// Normalize `layout` in place and describe what was corrected, in Spanish.
pub fn normalize_es(layout: &mut ResolvedLayout) -> Vec<String> {
    let before = layout.clone();
    // The store's own (English) messages are for logs; the corrections are diffed instead.
    let _ = layout.normalize();
    describe_corrections(&before, layout)
}

/// The editing state behind the panel.
#[derive(Debug, Clone)]
pub struct LayoutState {
    config: LayoutConfig,
    cwd: Option<PathBuf>,
    scope: Scope,
    warnings: Vec<String>,
}

impl LayoutState {
    pub fn new(config: LayoutConfig, cwd: Option<PathBuf>) -> Self {
        Self {
            config,
            cwd,
            scope: Scope::Global,
            warnings: Vec::new(),
        }
    }

    pub fn config(&self) -> &LayoutConfig {
        &self.config
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        self.cwd = cwd;
        if self.cwd.is_none() {
            self.scope = Scope::Global;
        }
    }

    /// Replace the whole config (hot reload from the file). Pending notes are dropped.
    pub fn set_config(&mut self, config: LayoutConfig) {
        self.config = config;
        self.warnings.clear();
    }

    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = if scope == Scope::Project && self.cwd.is_none() {
            Scope::Global
        } else {
            scope
        };
        self.warnings.clear();
    }

    /// Folder name of the project (last path component).
    pub fn project_name(&self) -> Option<String> {
        let cwd = self.cwd.as_ref()?;
        Some(
            cwd.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cwd.to_string_lossy().into_owned()),
        )
    }

    fn project_key(&self) -> Option<String> {
        self.cwd.as_ref().map(|c| c.to_string_lossy().into_owned())
    }

    fn exact_override(&self) -> Option<&LayoutOverrides> {
        let key = normalize_key(&self.project_key()?);
        self.config
            .project_overrides
            .iter()
            .find(|e| normalize_key(&e.path) == key)
            .map(|e| &e.overrides)
    }

    /// A project override exists for exactly this working directory.
    pub fn has_project_override(&self) -> bool {
        self.exact_override().is_some_and(|o| !o.is_empty())
    }

    /// The layout `Project` edits are diffed against: the global layout (plus any override
    /// inherited from a parent folder), without this folder's own override.
    fn project_base(&self) -> ResolvedLayout {
        let (Some(key), Some(cwd)) = (self.project_key(), self.cwd.as_deref()) else {
            return self.config.resolve();
        };
        let mut c = self.config.clone();
        c.set_project_overrides(&key, LayoutOverrides::default());
        c.resolve_for(cwd)
    }

    /// What the edits of the current scope are stored against (used for "default" marks).
    pub fn base_layout(&self) -> ResolvedLayout {
        match self.scope {
            Scope::Global => self
                .config
                .preset_layout(&self.config.preset)
                .unwrap_or_default(),
            Scope::Project => self.project_base(),
        }
    }

    /// The edited layout before validation (what is stored, applied).
    fn raw_layout(&self) -> ResolvedLayout {
        match self.scope {
            Scope::Global => {
                let mut l = self
                    .config
                    .preset_layout(&self.config.preset)
                    .unwrap_or_default();
                self.config.overrides.apply(&mut l);
                l
            }
            Scope::Project => {
                let mut l = self.project_base();
                if let Some(o) = self.exact_override() {
                    o.apply(&mut l);
                }
                l
            }
        }
    }

    /// The layout the shell shows for the current scope (validated).
    pub fn layout(&self) -> ResolvedLayout {
        let mut l = self.raw_layout();
        let _ = l.normalize();
        l
    }

    /// The preset to highlight: the selected one (global) or, in a project, the one the layout
    /// equals (else the global preset it derives from).
    pub fn active_preset(&self) -> Option<PresetId> {
        match self.scope {
            Scope::Global => Some(self.config.preset.clone()),
            Scope::Project => {
                let now = self.layout();
                let norm = |mut l: ResolvedLayout| {
                    let _ = l.normalize();
                    l
                };
                BuiltinPreset::ALL
                    .iter()
                    .map(|b| PresetId::Builtin(*b))
                    .chain(
                        self.config
                            .custom_presets
                            .iter()
                            .map(|p| PresetId::parse(&p.name)),
                    )
                    .find(|id| self.config.preset_layout(id).map(norm).as_ref() == Some(&now))
                    // A tweaked layout still derives from the global preset.
                    .or_else(|| Some(self.config.preset.clone()))
            }
        }
    }

    /// Has the current scope anything to reset?
    pub fn is_modified(&self) -> bool {
        match self.scope {
            Scope::Global => !self.config.overrides.is_empty(),
            Scope::Project => self.has_project_override(),
        }
    }

    /// Edit the layout of the current scope. Returns whether the config changed.
    pub fn apply(&mut self, edit: impl FnOnce(&mut ResolvedLayout)) -> bool {
        let before = self.raw_layout();
        let mut after = before.clone();
        edit(&mut after);
        self.commit(&before, after)
    }

    fn commit(&mut self, before: &ResolvedLayout, mut raw: ResolvedLayout) -> bool {
        let mut notes = Vec::new();
        // Asking for a zone that Zen hides means leaving Zen.
        if before.zen && raw.zen {
            let shows_chrome = (raw.sidebar.side != before.sidebar.side
                && raw.sidebar.side != SidebarSide::Hidden)
                || (raw.rail != before.rail && raw.rail == RailVisibility::Visible)
                || (raw.bottom_bar != before.bottom_bar
                    && raw.bottom_bar != store::BottomBar::Hidden)
                || (raw.tabs_position != before.tabs_position
                    && raw.tabs_position == TabsPosition::Sidebar);
            if shows_chrome {
                raw.zen = false;
                notes.push("Zen se desactivó para mostrar esa zona".to_string());
            }
        }
        let mut norm = raw.clone();
        notes.extend(normalize_es(&mut norm));
        let mut store = norm.clone();
        if norm.zen {
            // Zen only hides the chrome while on: keep what the user had for when it is off.
            store.sidebar.side = raw.sidebar.side;
            store.rail = raw.rail;
            store.bottom_bar = raw.bottom_bar;
            store.tabs_position = raw.tabs_position;
        }
        let old = self.config.clone();
        match self.scope {
            Scope::Global => self.config.set_resolved(&store),
            Scope::Project => {
                if let Some(key) = self.project_key() {
                    let o = LayoutOverrides::diff(&self.project_base(), &store);
                    self.config.set_project_overrides(&key, o);
                }
            }
        }
        self.warnings = notes;
        self.config != old
    }

    /// Pick a preset. Global: clears the overrides. Project: the project override becomes the
    /// difference between the preset and the global layout.
    pub fn select_preset(&mut self, id: PresetId) -> bool {
        let old = self.config.clone();
        match self.scope {
            Scope::Global => {
                if self.config.preset_layout(&id).is_none() {
                    return false;
                }
                self.config.select_preset(id);
                self.warnings.clear();
            }
            Scope::Project => {
                let Some(target) = self.config.preset_layout(&id) else {
                    return false;
                };
                let before = self.raw_layout();
                self.commit(&before, target);
            }
        }
        self.config != old
    }

    /// "Restablecer preset": drop the overrides of the current scope.
    pub fn reset(&mut self) -> bool {
        let old = self.config.clone();
        match self.scope {
            Scope::Global => self.config.select_preset(self.config.preset.clone()),
            Scope::Project => {
                if let Some(key) = self.project_key() {
                    self.config
                        .set_project_overrides(&key, LayoutOverrides::default());
                }
            }
        }
        self.warnings.clear();
        self.config != old
    }

    /// "Quitar override": remove the override of this project, whatever the scope.
    pub fn remove_project_override(&mut self) -> bool {
        let old = self.config.clone();
        if let Some(key) = self.project_key() {
            self.config
                .set_project_overrides(&key, LayoutOverrides::default());
        }
        self.warnings.clear();
        self.config != old
    }

    fn check_name(&self, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Escribe un nombre para el preset".into());
        }
        if BuiltinPreset::from_id(name).is_some() {
            return Err(format!("\"{name}\" es el nombre de un preset integrado"));
        }
        Ok(name.to_string())
    }

    /// Save the layout being edited as a custom preset and (in the global scope) select it.
    pub fn save_custom_preset(&mut self, name: &str) -> Result<(), String> {
        let name = self.check_name(name)?;
        let layout = self.layout();
        let (preset, overrides) = (self.config.preset.clone(), self.config.overrides.clone());
        self.config.save_custom_preset(&name, layout);
        if self.scope == Scope::Project {
            // A project layout does not change the global preset; the new preset is only stored.
            self.config.preset = preset;
            self.config.overrides = overrides;
        }
        self.warnings.clear();
        Ok(())
    }

    /// Remove a custom preset; if it was selected the default preset takes over.
    pub fn delete_custom_preset(&mut self, name: &str) -> bool {
        let before = self.config.custom_presets.len();
        self.config
            .custom_presets
            .retain(|p| !p.name.trim().eq_ignore_ascii_case(name.trim()));
        let removed = self.config.custom_presets.len() != before;
        if removed
            && matches!(&self.config.preset, PresetId::Custom(n) if n.trim().eq_ignore_ascii_case(name.trim()))
        {
            self.config.select_preset(PresetId::default());
        }
        removed
    }

    /// The layout being edited as a standalone TOML preset (what "Exportar" copies).
    pub fn export_toml(&self) -> String {
        let name = match self.active_preset() {
            Some(PresetId::Custom(n)) => n,
            _ => "Mi preset".to_string(),
        };
        CustomPreset {
            name,
            layout: self.layout(),
        }
        .export_toml()
    }

    /// Add a preset exported with [`export_toml`](Self::export_toml). Returns its name.
    pub fn import_toml(&mut self, text: &str) -> Result<String, String> {
        let preset = CustomPreset::import_toml(text.trim())
            .map_err(|e| format!("TOML no válido: {}", e.lines().next().unwrap_or("")))?;
        let name = self.check_name(&preset.name)?;
        self.save_custom_preset_layout(&name, preset.layout);
        Ok(name)
    }

    fn save_custom_preset_layout(&mut self, name: &str, mut layout: ResolvedLayout) {
        let _ = layout.normalize();
        let (preset, overrides) = (self.config.preset.clone(), self.config.overrides.clone());
        self.config.save_custom_preset(name, layout);
        if self.scope == Scope::Project {
            self.config.preset = preset;
            self.config.overrides = overrides;
        }
        self.warnings.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::{BottomBar, Density, Splits};

    fn state() -> LayoutState {
        LayoutState::new(
            LayoutConfig::default(),
            Some(PathBuf::from(r"C:\Users\me\code\selachi")),
        )
    }

    #[test]
    fn selecting_a_preset_clears_overrides() {
        let mut s = state();
        assert!(s.apply(|l| l.density = Density::Compact));
        assert!(!s.config().overrides.is_empty());
        assert!(s.select_preset(BuiltinPreset::Chat.into()));
        assert!(s.config().overrides.is_empty());
        assert_eq!(s.config().preset, PresetId::Builtin(BuiltinPreset::Chat));
        assert_eq!(s.layout(), ResolvedLayout::builtin(BuiltinPreset::Chat));
    }

    #[test]
    fn an_option_change_stores_the_minimal_diff() {
        let mut s = state();
        s.apply(|l| l.density = Density::Compact);
        let o = &s.config().overrides;
        assert_eq!(o.density, Some(Density::Compact));
        let expected = LayoutOverrides {
            density: Some(Density::Compact),
            ..LayoutOverrides::default()
        };
        assert_eq!(*o, expected);
        // Changing it back leaves no override at all.
        s.apply(|l| l.density = Density::Comfortable);
        assert!(s.config().overrides.is_empty());
        // A change on top of a non-default preset is stored relative to that preset.
        s.select_preset(BuiltinPreset::Orquestador.into());
        s.apply(|l| l.splits = Splits::Grid2x2);
        let expected = LayoutOverrides {
            splits: Some(Splits::Grid2x2),
            ..LayoutOverrides::default()
        };
        assert_eq!(s.config().overrides, expected);
    }

    #[test]
    fn normalization_warnings_are_surfaced_in_spanish() {
        let mut s = state();
        s.apply(|l| l.tabs_position = TabsPosition::Sidebar);
        assert_eq!(
            s.warnings(),
            ["La sidebar se mostró porque las pestañas están en ella"]
        );
        let l = s.layout();
        assert_eq!(l.sidebar.side, SidebarSide::Left);
        assert!(!l.sidebar.sections.is_empty());
        // The stored overrides are already consistent (resolve() has nothing to correct).
        assert!(s.config().resolve_checked(None).1.is_empty());
        // The next clean edit clears the note.
        s.apply(|l| l.breadcrumb = true);
        assert!(s.warnings().is_empty());
    }

    #[test]
    fn hiding_the_rail_without_a_sidebar_is_corrected() {
        let mut s = state();
        s.apply(|l| l.rail = RailVisibility::Hidden);
        assert_eq!(
            s.warnings(),
            ["El rail se mostró porque la sidebar está oculta"]
        );
        assert_eq!(s.layout().rail, RailVisibility::Visible);
    }

    #[test]
    fn zen_is_reversible_and_leaving_it_is_explicit() {
        let mut s = state();
        s.select_preset(BuiltinPreset::Espacios.into());
        s.apply(|l| l.zen = true);
        assert!(s.layout().zen);
        assert_eq!(s.layout().sidebar.side, SidebarSide::Hidden);
        assert_eq!(s.warnings().len(), 1);
        // The sidebar choice survives for when Zen is turned off.
        s.apply(|l| l.zen = false);
        assert_eq!(s.layout().sidebar.side, SidebarSide::Left);
        // Asking for chrome while in Zen switches Zen off.
        s.apply(|l| l.zen = true);
        s.apply(|l| l.bottom_bar = BottomBar::Full);
        assert!(!s.layout().zen);
        assert_eq!(s.layout().bottom_bar, BottomBar::Full);
    }

    #[test]
    fn project_scope_writes_a_project_override_only() {
        let mut s = state();
        s.set_scope(Scope::Project);
        assert_eq!(s.scope(), Scope::Project);
        assert!(!s.has_project_override());
        s.apply(|l| l.density = Density::Compact);
        assert!(s.config().overrides.is_empty(), "global config untouched");
        assert!(s.has_project_override());
        assert_eq!(s.config().project_overrides.len(), 1);
        let cwd = PathBuf::from(r"C:\Users\me\code\selachi");
        assert_eq!(s.config().resolve_for(&cwd).density, Density::Compact);
        assert_eq!(s.config().resolve().density, Density::Comfortable);
        // Back to the global value: the entry disappears.
        s.apply(|l| l.density = Density::Comfortable);
        assert!(!s.has_project_override());
        assert!(s.config().project_overrides.is_empty());
    }

    #[test]
    fn project_preset_pick_and_remove_override() {
        let mut s = state();
        s.set_scope(Scope::Project);
        s.select_preset(BuiltinPreset::Chat.into());
        let cwd = PathBuf::from(r"C:\Users\me\code\selachi");
        assert_eq!(
            s.config().resolve_for(&cwd),
            ResolvedLayout::builtin(BuiltinPreset::Chat)
        );
        assert_eq!(
            s.config().preset,
            PresetId::default(),
            "the global preset is untouched"
        );
        assert_eq!(
            s.active_preset(),
            Some(PresetId::Builtin(BuiltinPreset::Chat))
        );
        assert!(s.remove_project_override());
        assert_eq!(s.config().resolve_for(&cwd), s.config().resolve());
        assert!(!s.remove_project_override(), "nothing left to remove");
    }

    #[test]
    fn project_scope_without_a_folder_stays_global() {
        let mut s = LayoutState::new(LayoutConfig::default(), None);
        s.set_scope(Scope::Project);
        assert_eq!(s.scope(), Scope::Global);
    }

    #[test]
    fn reset_drops_the_overrides_of_the_scope() {
        let mut s = state();
        s.select_preset(BuiltinPreset::Espacios.into());
        s.apply(|l| l.breadcrumb = true);
        assert!(s.is_modified());
        assert!(s.reset());
        assert!(!s.is_modified());
        assert_eq!(
            s.config().preset,
            PresetId::Builtin(BuiltinPreset::Espacios)
        );
        assert!(!s.reset());
    }

    #[test]
    fn custom_preset_save_export_import_roundtrip() {
        let mut s = state();
        s.select_preset(BuiltinPreset::Orquestador.into());
        s.apply(|l| {
            l.sidebar.width_px = 360;
            l.breadcrumb = true;
        });
        let saved = s.layout();
        s.save_custom_preset("  Mi flujo ").unwrap();
        assert_eq!(s.config().preset, PresetId::Custom("Mi flujo".into()));
        assert!(s.config().overrides.is_empty());
        assert_eq!(s.layout(), saved);

        let text = s.export_toml();
        assert!(text.contains("Mi flujo"));
        // Import into a fresh state (another machine).
        let mut other = LayoutState::new(LayoutConfig::default(), None);
        let name = other.import_toml(&text).unwrap();
        assert_eq!(name, "Mi flujo");
        assert_eq!(other.config().custom_presets.len(), 1);
        assert_eq!(other.layout(), saved);
        // Re-importing replaces instead of duplicating.
        other.import_toml(&text).unwrap();
        assert_eq!(other.config().custom_presets.len(), 1);

        // Deleting the selected preset falls back to the default one.
        assert!(other.delete_custom_preset("mi flujo"));
        assert!(other.config().custom_presets.is_empty());
        assert_eq!(other.config().preset, PresetId::default());
    }

    #[test]
    fn preset_names_are_validated() {
        let mut s = state();
        assert!(s.save_custom_preset("   ").is_err());
        assert!(s.save_custom_preset("Zen").is_err());
        assert!(s.import_toml("esto no es toml = = =").is_err());
        assert!(s.import_toml("name = \"Chat\"").is_err());
        assert!(s.config().custom_presets.is_empty());
    }

    #[test]
    fn saving_a_preset_in_project_scope_keeps_the_global_preset() {
        let mut s = state();
        s.set_scope(Scope::Project);
        s.apply(|l| l.density = Density::Compact);
        s.save_custom_preset("Compacto").unwrap();
        assert_eq!(s.config().preset, PresetId::default());
        assert_eq!(s.config().custom_presets.len(), 1);
        assert!(s.has_project_override());
    }

    #[test]
    fn hot_reload_replaces_the_config() {
        let mut s = state();
        s.apply(|l| l.density = Density::Compact);
        let mut fresh = LayoutConfig::default();
        fresh.select_preset(BuiltinPreset::Zen.into());
        s.set_config(fresh);
        assert!(s.layout().zen);
        assert!(s.warnings().is_empty());
    }
}
