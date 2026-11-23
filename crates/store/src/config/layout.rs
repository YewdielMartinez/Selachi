//! Layout model: how Selachi's shell is organised (rail, sidebar, tabs, right
//! panel, status bar, splits, density). UI-agnostic: this module only describes
//! the layout; `app_shell` / `ui_layout` render and edit it.
//!
//! Resolution order, lowest to highest priority:
//!
//! 1. the selected preset ([`LayoutConfig::preset`]: built-in or custom),
//! 2. the global overrides (`[layout.overrides]`),
//! 3. every `[[layout.project_overrides]]` whose `path` is a prefix of the
//!    working directory, least specific first (so a nested project wins).
//!
//! The merged result is then validated: invalid combinations are corrected and
//! reported as warnings, never as errors (see [`ResolvedLayout::normalize`]).
//!
//! Editing goes through [`Config::set_layout`] + [`Config::save`], which keep the
//! comments in `config.toml`. Hot reload re-emits the whole `Arc<Config>`; use
//! [`layout_changed`] to re-layout only when something here changed.

use super::Config;
use crate::paths::normalize_key;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;

/// Sidebar width limits in logical pixels (the value is clamped, not rejected).
pub const SIDEBAR_MIN_WIDTH: u16 = 180;
pub const SIDEBAR_MAX_WIDTH: u16 = 480;
/// Upper bound of [`ResolvedLayout::sidebar_translucency`] (percent).
pub const TRANSLUCENCY_MAX: u8 = 100;

/// Well-known right-panel ids. [`PanelId`] is an open string, so a new panel
/// crate can register any other id without touching `store`.
pub mod panel_ids {
    pub const GIT: &str = "git";
    pub const CONTEXT: &str = "context";
    pub const PROCS: &str = "procs";
    pub const VIEWER: &str = "viewer";
    /// The file tree panel (`ui_viewer`).
    pub const FILES: &str = "files";
    pub const USAGE: &str = "usage";
    pub const ATTENTION: &str = "attention";
    /// Not a docked panel: the "Revisión" tab (diff with line notes).
    pub const REVIEW: &str = "review";
    /// Not a docked panel: the layout popover.
    pub const LAYOUT: &str = "layout";
}

/// Ids of the buttons the titlebar shows by default in the unified layout, in display order.
pub const DEFAULT_TITLEBAR_BUTTONS: [&str; 7] = [
    panel_ids::GIT,
    panel_ids::FILES,
    panel_ids::PROCS,
    panel_ids::USAGE,
    panel_ids::REVIEW,
    panel_ids::ATTENTION,
    panel_ids::LAYOUT,
];

// ---------------------------------------------------------------------------
// Simple enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabsPosition {
    /// Pills along the top of the main area.
    #[default]
    Top,
    /// Vertical rows inside the sidebar (requires a visible sidebar).
    Sidebar,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSide {
    Left,
    Right,
    #[default]
    Hidden,
}

/// A block that can live in the sidebar. The order in
/// [`SidebarLayout::sections`] is the display order; a section that is not in
/// the list is switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSection {
    Projects,
    Worktrees,
    Agents,
    Threads,
    Spaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    Project,
    Status,
    Agent,
    #[default]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RailVisibility {
    #[default]
    Visible,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BottomBar {
    /// Pills.
    #[default]
    Compact,
    /// Per-agent meters (5 h window + reset, weekly).
    Full,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Splits {
    #[default]
    Single,
    TwoVertical,
    TwoHorizontal,
    /// One tall pane plus two stacked (Orca style).
    OneAndTwoStacked,
    Grid2x2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
}

// ---------------------------------------------------------------------------
// PanelId
// ---------------------------------------------------------------------------

/// Id of a right-panel provider (`"git"`, `"context"`, `"procs"`, `"viewer"`,
/// or anything a future panel crate registers). Always trimmed and lowercase.
/// "No panel" is expressed as `None`, written `"none"` in TOML.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PanelId(String);

impl PanelId {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(id.as_ref().trim().to_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `"none"` / empty -> `None`, anything else -> `Some(id)`.
    pub fn parse_slot(s: &str) -> Option<PanelId> {
        let id = PanelId::new(s);
        if id.0.is_empty() || id.0 == "none" {
            None
        } else {
            Some(id)
        }
    }
}

impl<'de> Deserialize<'de> for PanelId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(PanelId::new(String::deserialize(d)?))
    }
}

/// serde adapter: `Option<PanelId>` <-> string (`"none"` = no panel).
mod slot {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<PanelId>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v.as_ref().map_or("none", PanelId::as_str))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PanelId>, D::Error> {
        Ok(PanelId::parse_slot(&String::deserialize(d)?))
    }
}

/// Same, for an override (`None` = not overridden, `Some(None)` = force hidden).
mod slot_override {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<Option<PanelId>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => slot::serialize(inner, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<PanelId>>, D::Error> {
        Ok(Some(PanelId::parse_slot(&String::deserialize(d)?)))
    }
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BuiltinPreset {
    #[default]
    Terminal,
    /// herdr: spaces/projects + agents in a sidebar.
    Espacios,
    /// Orca: project -> worktree -> agent tree, vertical tabs, split panes.
    Orquestador,
    /// T3 Code: threads grouped by status, context panel on the right.
    Chat,
    /// Only the terminal; chrome appears on edge hover.
    Zen,
    /// Shell v2 (Odla style): one full-height glass sidebar with the spaces (projects and
    /// worktrees) and their tabs nested as rows, quick actions on top and a usage footer; no
    /// rail, no top tabs; the panel buttons live at the right of the titlebar. Default of new
    /// installs (existing configs keep their preset).
    EspaciosUnificados,
}

impl BuiltinPreset {
    pub const ALL: [BuiltinPreset; 6] = [
        BuiltinPreset::EspaciosUnificados,
        BuiltinPreset::Terminal,
        BuiltinPreset::Espacios,
        BuiltinPreset::Orquestador,
        BuiltinPreset::Chat,
        BuiltinPreset::Zen,
    ];

    /// The id used in `config.toml`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Espacios => "espacios",
            Self::Orquestador => "orquestador",
            Self::Chat => "chat",
            Self::Zen => "zen",
            Self::EspaciosUnificados => "espacios_unificados",
        }
    }

    /// Case-insensitive lookup by id.
    pub fn from_id(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|p| p.id().eq_ignore_ascii_case(s))
    }
}

/// Which preset a layout starts from. Serialized as a plain string: one of the
/// built-in ids, or the name of a `[[layout.custom_presets]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PresetId {
    Builtin(BuiltinPreset),
    Custom(String),
}

impl Default for PresetId {
    fn default() -> Self {
        PresetId::Builtin(BuiltinPreset::Terminal)
    }
}

impl PresetId {
    /// Built-in ids win over custom names; an empty string is the default preset.
    pub fn parse(s: &str) -> PresetId {
        let s = s.trim();
        if s.is_empty() {
            return PresetId::default();
        }
        match BuiltinPreset::from_id(s) {
            Some(b) => PresetId::Builtin(b),
            None => PresetId::Custom(s.to_string()),
        }
    }

    /// The string written to `config.toml`.
    pub fn as_str(&self) -> &str {
        match self {
            PresetId::Builtin(b) => b.id(),
            PresetId::Custom(n) => n,
        }
    }
}

impl From<BuiltinPreset> for PresetId {
    fn from(b: BuiltinPreset) -> Self {
        PresetId::Builtin(b)
    }
}

impl Serialize for PresetId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PresetId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(PresetId::parse(&String::deserialize(d)?))
    }
}

// ---------------------------------------------------------------------------
// Resolved layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarLayout {
    pub side: SidebarSide,
    /// Clamped to [`SIDEBAR_MIN_WIDTH`]..=[`SIDEBAR_MAX_WIDTH`].
    pub width_px: u16,
    /// Ordered; a section that is absent is off.
    pub sections: Vec<SidebarSection>,
    pub group_by: GroupBy,
    /// Unified sidebar: "Nueva pestaña" and "Buscar" at the top.
    pub show_actions: bool,
    /// Unified sidebar: a row of small buttons, one per registered panel.
    pub panel_icons: bool,
    /// Unified sidebar: the tabs are rows nested inside their space (project / worktree).
    pub nested_tabs: bool,
    /// Unified sidebar: compact usage meter and the settings button at the bottom.
    pub footer_usage: bool,
    /// Rows show the model (and the branch) as a second line.
    pub row_model: bool,
    /// Rows show the elapsed time.
    pub row_time: bool,
}

impl Default for SidebarLayout {
    fn default() -> Self {
        Self {
            side: SidebarSide::Hidden,
            width_px: 240,
            sections: vec![SidebarSection::Projects],
            group_by: GroupBy::None,
            show_actions: false,
            panel_icons: false,
            nested_tabs: false,
            footer_usage: false,
            row_model: true,
            row_time: true,
        }
    }
}

/// The titlebar's right side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TitlebarLayout {
    /// Buttons at the right of the titlebar, in order: ids of right panels (`"git"`, `"files"`,
    /// `"procs"`, `"usage"`, `"attention"`...) plus `"review"` (opens the review tab) and
    /// `"layout"` (the layout popover). Ids nobody registered are skipped when drawing.
    pub panel_buttons: Vec<PanelId>,
}

impl Default for TitlebarLayout {
    /// Only the layout popover button: the classic presets keep their rail for the panels.
    fn default() -> Self {
        Self { panel_buttons: vec![PanelId::new(panel_ids::LAYOUT)] }
    }
}

impl TitlebarLayout {
    /// Every button of [`DEFAULT_TITLEBAR_BUTTONS`] (the unified preset).
    pub fn all_buttons() -> Self {
        Self { panel_buttons: DEFAULT_TITLEBAR_BUTTONS.iter().map(PanelId::new).collect() }
    }

    pub fn has(&self, id: &str) -> bool {
        self.panel_buttons.iter().any(|p| p.as_str() == id)
    }
}

/// A complete, concrete layout: what the shell renders. Defaults equal the
/// `terminal` preset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResolvedLayout {
    pub tabs_position: TabsPosition,
    pub sidebar: SidebarLayout,
    pub titlebar: TitlebarLayout,
    pub rail: RailVisibility,
    /// Id of the panel docked on the right; `None` = no right panel.
    #[serde(with = "slot")]
    pub right_panel: Option<PanelId>,
    pub bottom_bar: BottomBar,
    pub splits: Splits,
    pub density: Density,
    /// 0 (opaque) ..= 100 (most translucent) sidebar (Mica/Acrylic).
    pub sidebar_translucency: u8,
    pub breadcrumb: bool,
    pub reduced_motion: bool,
    /// Zen mode: the chrome is hidden and revealed on edge hover. Forces the
    /// sidebar, rail and bottom bar hidden; tabs (if not `hidden`) are only
    /// shown while revealed.
    pub zen: bool,
}

impl Default for ResolvedLayout {
    fn default() -> Self {
        Self::builtin(BuiltinPreset::Terminal)
    }
}

impl ResolvedLayout {
    /// The built-in preset table (see docs/PLAN.md §17).
    pub fn builtin(preset: BuiltinPreset) -> Self {
        use SidebarSection as S;
        let base = ResolvedLayout {
            tabs_position: TabsPosition::Top,
            sidebar: SidebarLayout::default(),
            titlebar: TitlebarLayout::default(),
            rail: RailVisibility::Visible,
            right_panel: None,
            bottom_bar: BottomBar::Compact,
            splits: Splits::Single,
            density: Density::Comfortable,
            sidebar_translucency: 0,
            breadcrumb: false,
            reduced_motion: false,
            zen: false,
        };
        let sidebar = |width_px, sections: &[S], group_by| SidebarLayout {
            side: SidebarSide::Left,
            width_px,
            sections: sections.to_vec(),
            group_by,
            ..SidebarLayout::default()
        };
        match preset {
            BuiltinPreset::Terminal => base,
            BuiltinPreset::Espacios => ResolvedLayout {
                sidebar: sidebar(240, &[S::Spaces, S::Projects, S::Agents], GroupBy::Project),
                splits: Splits::TwoVertical,
                ..base
            },
            BuiltinPreset::Orquestador => ResolvedLayout {
                tabs_position: TabsPosition::Sidebar,
                sidebar: sidebar(
                    300,
                    &[S::Projects, S::Worktrees, S::Agents],
                    GroupBy::Project,
                ),
                bottom_bar: BottomBar::Full,
                splits: Splits::OneAndTwoStacked,
                density: Density::Compact,
                ..base
            },
            BuiltinPreset::Chat => ResolvedLayout {
                tabs_position: TabsPosition::Sidebar,
                sidebar: sidebar(280, &[S::Threads, S::Projects], GroupBy::Status),
                rail: RailVisibility::Hidden,
                right_panel: Some(PanelId::new(panel_ids::CONTEXT)),
                ..base
            },
            BuiltinPreset::Zen => ResolvedLayout {
                rail: RailVisibility::Hidden,
                bottom_bar: BottomBar::Hidden,
                zen: true,
                ..base
            },
            BuiltinPreset::EspaciosUnificados => ResolvedLayout {
                tabs_position: TabsPosition::Sidebar,
                sidebar: SidebarLayout {
                    show_actions: true,
                    // The panel buttons live at the right of the titlebar; the icon row of the
                    // sidebar would show them twice (still configurable).
                    panel_icons: false,
                    nested_tabs: true,
                    footer_usage: true,
                    ..sidebar(260, &[S::Spaces], GroupBy::Project)
                },
                titlebar: TitlebarLayout::all_buttons(),
                rail: RailVisibility::Hidden,
                breadcrumb: false,
                ..base
            },
        }
    }

    /// Preset by id. A [`PresetId::Custom`] cannot be resolved without the
    /// config and falls back to `terminal`; use [`LayoutConfig::resolve`] for those.
    pub fn preset(id: &PresetId) -> Self {
        match id {
            PresetId::Builtin(b) => Self::builtin(*b),
            PresetId::Custom(_) => Self::default(),
        }
    }

    /// Correct invalid combinations in place and describe each correction.
    ///
    /// * `width_px` and `sidebar_translucency` are clamped; duplicate sections removed.
    /// * `zen`: sidebar, rail and bottom bar are forced hidden and tabs cannot be
    ///   in the (hidden) sidebar.
    /// * otherwise: `tabs_position = sidebar` needs a visible sidebar (forced
    ///   `left`), and a hidden rail needs a visible sidebar (else the rail is
    ///   forced visible so the window is never left without navigation).
    /// * a visible sidebar with no sections gets `projects`.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut w = Vec::new();
        let sb = &mut self.sidebar;

        let width = sb.width_px.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        if width != sb.width_px {
            w.push(format!(
                "sidebar.width_px = {} is outside {SIDEBAR_MIN_WIDTH}..={SIDEBAR_MAX_WIDTH}; using {width}",
                sb.width_px
            ));
            sb.width_px = width;
        }
        if self.sidebar_translucency > TRANSLUCENCY_MAX {
            w.push(format!(
                "sidebar_translucency = {} is outside 0..={TRANSLUCENCY_MAX}; using {TRANSLUCENCY_MAX}",
                self.sidebar_translucency
            ));
            self.sidebar_translucency = TRANSLUCENCY_MAX;
        }
        let mut buttons: Vec<PanelId> = Vec::with_capacity(self.titlebar.panel_buttons.len());
        for b in &self.titlebar.panel_buttons {
            if b.as_str().is_empty() || b.as_str() == "none" {
                continue;
            }
            if buttons.contains(b) {
                w.push(format!("titlebar.panel_buttons lists \"{}\" more than once; keeping the first", b.as_str()));
            } else {
                buttons.push(b.clone());
            }
        }
        self.titlebar.panel_buttons = buttons;
        let mut seen = Vec::with_capacity(sb.sections.len());
        for s in &sb.sections {
            if seen.contains(s) {
                w.push(format!(
                    "sidebar.sections lists {s:?} more than once; keeping the first"
                ));
            } else {
                seen.push(*s);
            }
        }
        sb.sections = seen;

        if self.zen {
            // Zen means "chrome hidden", so this is enforced silently; set
            // `zen = false` to get the zones back.
            sb.side = SidebarSide::Hidden;
            self.rail = RailVisibility::Hidden;
            self.bottom_bar = BottomBar::Hidden;
            if self.tabs_position == TabsPosition::Sidebar {
                self.tabs_position = TabsPosition::Top;
            }
        } else {
            if self.tabs_position == TabsPosition::Sidebar && sb.side == SidebarSide::Hidden {
                w.push("tabs_position = \"sidebar\" needs a visible sidebar; sidebar.side set to \"left\"".into());
                sb.side = SidebarSide::Left;
            }
            if self.rail == RailVisibility::Hidden && sb.side == SidebarSide::Hidden {
                w.push("rail = \"hidden\" needs a visible sidebar; rail set to \"visible\"".into());
                self.rail = RailVisibility::Visible;
            }
        }
        if sb.side != SidebarSide::Hidden && sb.sections.is_empty() {
            w.push("the sidebar has no sections; showing \"projects\"".into());
            sb.sections.push(SidebarSection::Projects);
        }
        w
    }

    /// Zones for a preset thumbnail. See [`wireframe`].
    pub fn wireframe(&self) -> Vec<WireRect> {
        wireframe(self)
    }
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

/// Sparse sidebar changes. Absent fields keep the preset's value.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<SidebarSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_px: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<SidebarSection>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_by: Option<GroupBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_actions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel_icons: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_tabs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer_usage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_model: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_time: Option<bool>,
}

impl SidebarOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Sparse titlebar changes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TitlebarOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel_buttons: Option<Vec<PanelId>>,
}

impl TitlebarOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Sparse changes on top of a preset: only what the user touched is stored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs_position: Option<TabsPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<SidebarOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub titlebar: Option<TitlebarOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rail: Option<RailVisibility>,
    /// `Some(None)` forces the panel off (`right_panel = "none"`).
    #[serde(skip_serializing_if = "Option::is_none", with = "slot_override")]
    pub right_panel: Option<Option<PanelId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom_bar: Option<BottomBar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub splits: Option<Splits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_translucency: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breadcrumb: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduced_motion: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zen: Option<bool>,
}

impl LayoutOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Write every field that is set into `layout` (no validation).
    pub fn apply(&self, layout: &mut ResolvedLayout) {
        if let Some(v) = self.tabs_position {
            layout.tabs_position = v;
        }
        if let Some(sb) = &self.sidebar {
            if let Some(v) = sb.side {
                layout.sidebar.side = v;
            }
            if let Some(v) = sb.width_px {
                layout.sidebar.width_px = v;
            }
            if let Some(v) = &sb.sections {
                layout.sidebar.sections = v.clone();
            }
            if let Some(v) = sb.group_by {
                layout.sidebar.group_by = v;
            }
            let flags = [
                (sb.show_actions, &mut layout.sidebar.show_actions),
                (sb.panel_icons, &mut layout.sidebar.panel_icons),
                (sb.nested_tabs, &mut layout.sidebar.nested_tabs),
                (sb.footer_usage, &mut layout.sidebar.footer_usage),
                (sb.row_model, &mut layout.sidebar.row_model),
                (sb.row_time, &mut layout.sidebar.row_time),
            ];
            for (v, slot) in flags {
                if let Some(v) = v {
                    *slot = v;
                }
            }
        }
        if let Some(v) = self.titlebar.as_ref().and_then(|t| t.panel_buttons.as_ref()) {
            layout.titlebar.panel_buttons = v.clone();
        }
        if let Some(v) = self.rail {
            layout.rail = v;
        }
        if let Some(v) = &self.right_panel {
            layout.right_panel = v.clone();
        }
        if let Some(v) = self.bottom_bar {
            layout.bottom_bar = v;
        }
        if let Some(v) = self.splits {
            layout.splits = v;
        }
        if let Some(v) = self.density {
            layout.density = v;
        }
        if let Some(v) = self.sidebar_translucency {
            layout.sidebar_translucency = v;
        }
        if let Some(v) = self.breadcrumb {
            layout.breadcrumb = v;
        }
        if let Some(v) = self.reduced_motion {
            layout.reduced_motion = v;
        }
        if let Some(v) = self.zen {
            layout.zen = v;
        }
    }

    /// The smallest set of overrides that turns `base` into `target`.
    pub fn diff(base: &ResolvedLayout, target: &ResolvedLayout) -> Self {
        fn changed<T: PartialEq + Clone>(a: &T, b: &T) -> Option<T> {
            (a != b).then(|| b.clone())
        }
        let sidebar = SidebarOverrides {
            side: changed(&base.sidebar.side, &target.sidebar.side),
            width_px: changed(&base.sidebar.width_px, &target.sidebar.width_px),
            sections: changed(&base.sidebar.sections, &target.sidebar.sections),
            group_by: changed(&base.sidebar.group_by, &target.sidebar.group_by),
            show_actions: changed(&base.sidebar.show_actions, &target.sidebar.show_actions),
            panel_icons: changed(&base.sidebar.panel_icons, &target.sidebar.panel_icons),
            nested_tabs: changed(&base.sidebar.nested_tabs, &target.sidebar.nested_tabs),
            footer_usage: changed(&base.sidebar.footer_usage, &target.sidebar.footer_usage),
            row_model: changed(&base.sidebar.row_model, &target.sidebar.row_model),
            row_time: changed(&base.sidebar.row_time, &target.sidebar.row_time),
        };
        let titlebar = TitlebarOverrides {
            panel_buttons: changed(&base.titlebar.panel_buttons, &target.titlebar.panel_buttons),
        };
        LayoutOverrides {
            tabs_position: changed(&base.tabs_position, &target.tabs_position),
            sidebar: (!sidebar.is_empty()).then_some(sidebar),
            titlebar: (!titlebar.is_empty()).then_some(titlebar),
            rail: changed(&base.rail, &target.rail),
            right_panel: changed(&base.right_panel, &target.right_panel),
            bottom_bar: changed(&base.bottom_bar, &target.bottom_bar),
            splits: changed(&base.splits, &target.splits),
            density: changed(&base.density, &target.density),
            sidebar_translucency: changed(&base.sidebar_translucency, &target.sidebar_translucency),
            breadcrumb: changed(&base.breadcrumb, &target.breadcrumb),
            reduced_motion: changed(&base.reduced_motion, &target.reduced_motion),
            zen: changed(&base.zen, &target.zen),
        }
    }
}

// ---------------------------------------------------------------------------
// Config section
// ---------------------------------------------------------------------------

/// A user-defined preset: a name plus a complete layout (its fields sit next to
/// `name` in the TOML table).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomPreset {
    pub name: String,
    #[serde(flatten)]
    pub layout: ResolvedLayout,
}

impl CustomPreset {
    /// A standalone TOML document (to share a preset).
    pub fn export_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Parse a document produced by [`CustomPreset::export_toml`].
    pub fn import_toml(text: &str) -> Result<Self, String> {
        let p: CustomPreset = toml::from_str(text).map_err(|e| e.to_string())?;
        if p.name.trim().is_empty() {
            return Err("a preset needs a non-empty name".into());
        }
        Ok(p)
    }
}

/// Overrides that apply to every working directory under `path`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectLayoutOverride {
    pub path: String,
    #[serde(skip_serializing_if = "LayoutOverrides::is_empty")]
    pub overrides: LayoutOverrides,
}

/// The `[layout]` section of `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub preset: PresetId,
    /// Global tweaks on top of the preset.
    pub overrides: LayoutOverrides,
    pub custom_presets: Vec<CustomPreset>,
    pub project_overrides: Vec<ProjectLayoutOverride>,
}

impl LayoutConfig {
    /// A custom preset by name (case-insensitive). Names equal to a built-in id never match.
    pub fn custom_preset(&self, name: &str) -> Option<&CustomPreset> {
        let name = name.trim();
        if BuiltinPreset::from_id(name).is_some() {
            return None;
        }
        self.custom_presets
            .iter()
            .find(|p| p.name.trim().eq_ignore_ascii_case(name))
    }

    /// The raw (unvalidated, no overrides) layout of `id`, or `None` for an
    /// unknown custom name.
    pub fn preset_layout(&self, id: &PresetId) -> Option<ResolvedLayout> {
        match id {
            PresetId::Builtin(b) => Some(ResolvedLayout::builtin(*b)),
            PresetId::Custom(n) => self.custom_preset(n).map(|p| p.layout.clone()),
        }
    }

    /// Preset + global overrides, validated.
    pub fn resolve(&self) -> ResolvedLayout {
        self.resolve_checked(None).0
    }

    /// Preset + global overrides + matching project overrides, validated.
    pub fn resolve_for(&self, cwd: &Path) -> ResolvedLayout {
        self.resolve_checked(Some(cwd)).0
    }

    /// Like [`resolve`](Self::resolve) / [`resolve_for`](Self::resolve_for), also
    /// returning what had to be corrected.
    pub fn resolve_checked(&self, cwd: Option<&Path>) -> (ResolvedLayout, Vec<String>) {
        let mut warnings = Vec::new();
        let mut layout = match self.preset_layout(&self.preset) {
            Some(l) => l,
            None => {
                warnings.push(format!(
                    "preset \"{}\" is not defined in [[layout.custom_presets]]; using \"terminal\"",
                    self.preset.as_str()
                ));
                ResolvedLayout::default()
            }
        };
        self.overrides.apply(&mut layout);
        if let Some(cwd) = cwd {
            for entry in self.matching_projects(cwd) {
                entry.overrides.apply(&mut layout);
            }
        }
        warnings.extend(layout.normalize());
        (layout, warnings)
    }

    /// `project_overrides` whose path is a prefix (on a path-component boundary,
    /// case-insensitive, `/` and `\` equivalent) of `cwd`, least specific first.
    fn matching_projects(&self, cwd: &Path) -> Vec<&ProjectLayoutOverride> {
        let cwd = normalize_key(&cwd.to_string_lossy());
        let mut hits: Vec<(String, &ProjectLayoutOverride)> = self
            .project_overrides
            .iter()
            .filter_map(|e| {
                let key = normalize_key(&e.path);
                (!key.is_empty() && is_path_prefix(&key, &cwd)).then_some((key, e))
            })
            .collect();
        hits.sort_by_key(|(k, _)| k.len()); // stable: file order breaks ties
        hits.into_iter().map(|(_, e)| e).collect()
    }

    /// Every correction needed anywhere in this section (global, each custom
    /// preset, each project override). Empty = nothing to fix.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.resolve_checked(None).1);
        for (i, p) in self.custom_presets.iter().enumerate() {
            let name = p.name.trim();
            if name.is_empty() {
                out.push(format!(
                    "custom_presets[{i}] has no name and can not be selected"
                ));
                continue;
            }
            if BuiltinPreset::from_id(name).is_some() {
                out.push(format!(
                    "custom preset \"{name}\" is shadowed by the built-in preset of the same name"
                ));
            }
            if self.custom_presets[..i]
                .iter()
                .any(|q| q.name.trim().eq_ignore_ascii_case(name))
            {
                out.push(format!(
                    "custom preset \"{name}\" is defined more than once; the first wins"
                ));
            }
            let mut l = p.layout.clone();
            out.extend(
                l.normalize()
                    .into_iter()
                    .map(|m| format!("custom preset \"{name}\": {m}")),
            );
        }
        for (i, e) in self.project_overrides.iter().enumerate() {
            if normalize_key(&e.path).is_empty() {
                out.push(format!(
                    "project_overrides[{i}] has an empty path and is ignored"
                ));
                continue;
            }
            let (_, ws) = self.resolve_checked(Some(Path::new(&e.path)));
            let global = self.resolve_checked(None).1;
            out.extend(
                ws.into_iter()
                    .filter(|m| !global.contains(m))
                    .map(|m| format!("project \"{}\": {m}", e.path)),
            );
        }
        out
    }

    /// Switch preset and drop the global overrides, so the preset shows as designed.
    pub fn select_preset(&mut self, id: PresetId) {
        self.preset = id;
        self.overrides = LayoutOverrides::default();
    }

    /// Make `self.resolve()` equal `target` by storing only the difference from
    /// the current preset as global overrides. (Project overrides still apply on top.)
    pub fn set_resolved(&mut self, target: &ResolvedLayout) {
        let base = self.preset_layout(&self.preset).unwrap_or_default();
        self.overrides = LayoutOverrides::diff(&base, target);
    }

    /// Save `layout` as a custom preset (replacing one with the same name) and select it.
    pub fn save_custom_preset(&mut self, name: &str, layout: ResolvedLayout) {
        let name = name.trim().to_string();
        let preset = CustomPreset {
            name: name.clone(),
            layout,
        };
        match self
            .custom_presets
            .iter_mut()
            .find(|p| p.name.trim().eq_ignore_ascii_case(&name))
        {
            Some(slot) => *slot = preset,
            None => self.custom_presets.push(preset),
        }
        self.select_preset(PresetId::parse(&name));
    }

    /// Set the overrides for exactly this project path (replacing an entry with
    /// the same normalized path); empty overrides remove the entry.
    pub fn set_project_overrides(&mut self, path: &str, overrides: LayoutOverrides) {
        let key = normalize_key(path);
        self.project_overrides
            .retain(|e| normalize_key(&e.path) != key);
        if !overrides.is_empty() {
            self.project_overrides.push(ProjectLayoutOverride {
                path: path.trim().to_string(),
                overrides,
            });
        }
    }
}

/// `prefix` equals `path` or is one of its parent directories (both normalized keys).
fn is_path_prefix(prefix: &str, path: &str) -> bool {
    if path == prefix {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some(rest) => prefix.ends_with('/') || rest.starts_with('/'),
        None => false,
    }
}

impl Config {
    /// The layout for a terminal whose working directory is `cwd`.
    pub fn layout_for(&self, cwd: &Path) -> ResolvedLayout {
        self.layout.resolve_for(cwd)
    }

    /// The layout without any per-project override.
    pub fn global_layout(&self) -> ResolvedLayout {
        self.layout.resolve()
    }

    /// Replace the `[layout]` section. Call [`Config::save`] to persist it
    /// (comments in the file are preserved).
    pub fn set_layout(&mut self, layout: LayoutConfig) {
        self.layout = layout;
    }

    /// Corrections the layout needed, ready to show to the user.
    pub fn layout_warnings(&self) -> Vec<String> {
        self.layout
            .warnings()
            .into_iter()
            .map(|m| format!("layout: {m}"))
            .collect()
    }
}

/// Did anything under `[layout]` change between two configs? Lets the UI
/// re-layout only on hot reloads that touched the layout.
pub fn layout_changed(old: &Config, new: &Config) -> bool {
    old.layout != new.layout
}

// ---------------------------------------------------------------------------
// Wireframe
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zone {
    Rail,
    /// Sidebar content. With `tabs_position = sidebar` the lower part of the
    /// sidebar is reported as [`Zone::Tabs`].
    Sidebar,
    Tabs,
    /// One pane of the split area (several `Main` rects for split layouts).
    Main,
    Right,
    Status,
}

/// A rectangle in a 0..100 x 0..100 space (origin top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireRect {
    pub zone: Zone,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

const WIRE_RAIL_W: f32 = 6.0;
const WIRE_RIGHT_W: f32 = 22.0;
const WIRE_TABS_H: f32 = 7.0;
/// Fraction of the sidebar that keeps its sections when tabs live in it.
const WIRE_SIDEBAR_SECTIONS: f32 = 0.4;
/// Window width the sidebar `width_px` is scaled against.
const WIRE_REF_WIDTH_PX: f32 = 1280.0;

/// Where each zone goes, in a 0..100 unit space, for preset thumbnails. Pure.
/// The rects tile the whole square without overlapping; in zen mode the chrome
/// is not drawn (it only appears on hover), so `Main` fills everything.
pub fn wireframe(l: &ResolvedLayout) -> Vec<WireRect> {
    let mut out = Vec::new();
    let status_h = match l.bottom_bar {
        BottomBar::Compact => 6.0,
        BottomBar::Full => 11.0,
        BottomBar::Hidden => 0.0,
    };
    let body_h = 100.0 - status_h;
    let rect = |zone, x, y, w, h| WireRect { zone, x, y, w, h };
    let mut left = 0.0_f32;
    let mut right = 100.0_f32;

    if l.rail == RailVisibility::Visible && !l.zen {
        out.push(rect(Zone::Rail, 0.0, 0.0, WIRE_RAIL_W, body_h));
        left = WIRE_RAIL_W;
    }
    if !l.zen && l.sidebar.side != SidebarSide::Hidden {
        let width = l
            .sidebar
            .width_px
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        let sw = f32::from(width) / WIRE_REF_WIDTH_PX * 100.0;
        let x = if l.sidebar.side == SidebarSide::Left {
            let x = left;
            left += sw;
            x
        } else {
            right -= sw;
            right
        };
        // With nested tabs the rows live inside the spaces: one full-height sidebar.
        if l.tabs_position == TabsPosition::Sidebar && !l.sidebar.nested_tabs {
            let top = body_h * WIRE_SIDEBAR_SECTIONS;
            out.push(rect(Zone::Sidebar, x, 0.0, sw, top));
            out.push(rect(Zone::Tabs, x, top, sw, body_h - top));
        } else {
            out.push(rect(Zone::Sidebar, x, 0.0, sw, body_h));
        }
    }
    if l.right_panel.is_some() {
        right -= WIRE_RIGHT_W;
        out.push(rect(Zone::Right, right, 0.0, WIRE_RIGHT_W, body_h));
    }

    let width = right - left;
    let mut top = 0.0;
    if l.tabs_position == TabsPosition::Top && !l.zen {
        out.push(rect(Zone::Tabs, left, 0.0, width, WIRE_TABS_H));
        top = WIRE_TABS_H;
    }
    let (x, y, w, h) = (left, top, width, body_h - top);
    let (hw, hh) = (w / 2.0, h / 2.0);
    let main = |x, y, w, h| rect(Zone::Main, x, y, w, h);
    match l.splits {
        Splits::Single => out.push(main(x, y, w, h)),
        Splits::TwoVertical => {
            out.push(main(x, y, hw, h));
            out.push(main(x + hw, y, hw, h));
        }
        Splits::TwoHorizontal => {
            out.push(main(x, y, w, hh));
            out.push(main(x, y + hh, w, hh));
        }
        Splits::OneAndTwoStacked => {
            out.push(main(x, y, hw, h));
            out.push(main(x + hw, y, hw, hh));
            out.push(main(x + hw, y + hh, hw, hh));
        }
        Splits::Grid2x2 => {
            out.push(main(x, y, hw, hh));
            out.push(main(x + hw, y, hw, hh));
            out.push(main(x, y + hh, hw, hh));
            out.push(main(x + hw, y + hh, hw, hh));
        }
    }
    if status_h > 0.0 {
        out.push(rect(Zone::Status, 0.0, body_h, 100.0, status_h));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GitConfig, WorktreeLocation, DEFAULT_PLUGIN_INDEX_URL, TEMPLATE};
    use std::path::PathBuf;

    #[test]
    fn defaults_roundtrip() {
        let c = LayoutConfig::default();
        assert_eq!(c.preset, PresetId::Builtin(BuiltinPreset::Terminal));
        assert_eq!(
            c.resolve(),
            ResolvedLayout::builtin(BuiltinPreset::Terminal)
        );
        assert_eq!(
            ResolvedLayout::default(),
            ResolvedLayout::builtin(BuiltinPreset::Terminal)
        );
        assert!(c.warnings().is_empty());
        let text = toml::to_string(&Config::default()).unwrap();
        assert_eq!(Config::parse(&text).unwrap().config, Config::default());
        // An empty/absent [layout] gives the defaults too.
        assert_eq!(Config::parse("[layout]").unwrap().config.layout, c);
    }

    #[test]
    fn builtin_presets_are_valid_and_resolve_as_specified() {
        use SidebarSection as S;
        for b in BuiltinPreset::ALL {
            let mut l = ResolvedLayout::builtin(b);
            assert!(l.normalize().is_empty(), "{b:?} needs no correction");
            let c = LayoutConfig {
                preset: b.into(),
                ..Default::default()
            };
            assert_eq!(c.resolve(), l);
            assert_eq!(ResolvedLayout::preset(&b.into()), l);
        }
        let t = ResolvedLayout::builtin(BuiltinPreset::Terminal);
        assert_eq!(t.tabs_position, TabsPosition::Top);
        assert_eq!(t.sidebar.side, SidebarSide::Hidden);
        assert_eq!(t.rail, RailVisibility::Visible);
        assert_eq!(t.right_panel, None);

        let e = ResolvedLayout::builtin(BuiltinPreset::Espacios);
        assert_eq!(e.tabs_position, TabsPosition::Top);
        assert_eq!(e.sidebar.side, SidebarSide::Left);
        assert_eq!(e.sidebar.sections, vec![S::Spaces, S::Projects, S::Agents]);
        assert_eq!(e.splits, Splits::TwoVertical);

        let o = ResolvedLayout::builtin(BuiltinPreset::Orquestador);
        assert_eq!(o.tabs_position, TabsPosition::Sidebar);
        assert_eq!(
            o.sidebar.sections,
            vec![S::Projects, S::Worktrees, S::Agents]
        );
        assert_eq!(o.bottom_bar, BottomBar::Full);
        assert_eq!(o.splits, Splits::OneAndTwoStacked);
        assert_eq!(o.density, Density::Compact);

        let c = ResolvedLayout::builtin(BuiltinPreset::Chat);
        assert_eq!(c.sidebar.group_by, GroupBy::Status);
        assert_eq!(c.sidebar.sections[0], S::Threads);
        assert_eq!(c.right_panel, Some(PanelId::new("context")));
        assert_eq!(c.rail, RailVisibility::Hidden);

        let z = ResolvedLayout::builtin(BuiltinPreset::Zen);
        assert!(z.zen);
        assert_eq!(z.sidebar.side, SidebarSide::Hidden);
        assert_eq!(z.rail, RailVisibility::Hidden);
        assert_eq!(z.bottom_bar, BottomBar::Hidden);
        assert_eq!(z.tabs_position, TabsPosition::Top);
    }

    #[test]
    fn preset_id_strings() {
        for b in BuiltinPreset::ALL {
            assert_eq!(PresetId::parse(b.id()), PresetId::Builtin(b));
            assert_eq!(
                PresetId::parse(&b.id().to_uppercase()),
                PresetId::Builtin(b)
            );
        }
        assert_eq!(
            PresetId::parse("Mi layout"),
            PresetId::Custom("Mi layout".into())
        );
        assert_eq!(PresetId::parse(""), PresetId::default());
        let c = Config::parse("[layout]\npreset = \"espacios\"")
            .unwrap()
            .config;
        assert_eq!(c.layout.preset, PresetId::Builtin(BuiltinPreset::Espacios));
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("preset = \"espacios\""), "{text}");
    }

    #[test]
    fn override_merge() {
        let text = r#"
[layout]
preset = "espacios"
[layout.overrides]
density = "compact"
right_panel = "git"
sidebar_translucency = 40
[layout.overrides.sidebar]
side = "right"
width_px = 320
"#;
        let p = Config::parse(text).unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let l = p.config.global_layout();
        let base = ResolvedLayout::builtin(BuiltinPreset::Espacios);
        assert_eq!(l.density, Density::Compact);
        assert_eq!(l.right_panel, Some(PanelId::new("git")));
        assert_eq!(l.sidebar_translucency, 40);
        assert_eq!(l.sidebar.side, SidebarSide::Right);
        assert_eq!(l.sidebar.width_px, 320);
        // Untouched fields keep the preset's values.
        assert_eq!(l.sidebar.sections, base.sidebar.sections);
        assert_eq!(l.sidebar.group_by, base.sidebar.group_by);
        assert_eq!(l.splits, base.splits);
        assert_eq!(l.tabs_position, base.tabs_position);

        // right_panel = "none" forces a preset's panel off.
        let c = Config::parse(
            "[layout]\npreset = \"chat\"\n[layout.overrides]\nright_panel = \"none\"",
        )
        .unwrap()
        .config;
        assert_eq!(c.global_layout().right_panel, None);
        // Open panel ids are accepted as they are (lowercased).
        let c = Config::parse("[layout.overrides]\nright_panel = \"Usage\"")
            .unwrap()
            .config;
        assert_eq!(c.global_layout().right_panel, Some(PanelId::new("usage")));
    }

    #[test]
    fn diff_and_set_resolved_roundtrip() {
        for b in BuiltinPreset::ALL {
            let mut cfg = LayoutConfig {
                preset: b.into(),
                ..Default::default()
            };
            assert!(cfg.overrides.is_empty());
            let mut target = ResolvedLayout::builtin(BuiltinPreset::Chat);
            target.density = Density::Compact;
            target.sidebar.width_px = 400;
            target.zen = false;
            target.splits = Splits::Grid2x2;
            target.right_panel = Some(PanelId::new("procs"));
            cfg.set_resolved(&target);
            assert_eq!(cfg.resolve(), target, "from {b:?}");
            // Back to the preset itself leaves no overrides.
            cfg.set_resolved(&ResolvedLayout::builtin(b));
            assert!(cfg.overrides.is_empty());
        }
    }

    #[test]
    fn invalid_combinations_are_corrected_with_warnings() {
        // tabs in sidebar without a sidebar -> sidebar left.
        let c = Config::parse("[layout.overrides]\ntabs_position = \"sidebar\"").unwrap();
        let l = c.config.global_layout();
        assert_eq!(l.sidebar.side, SidebarSide::Left);
        assert_eq!(l.tabs_position, TabsPosition::Sidebar);
        assert_eq!(c.warnings.len(), 1, "{:?}", c.warnings);
        assert!(c.warnings[0].contains("tabs_position"));

        // rail hidden and no sidebar -> rail visible.
        let c = Config::parse("[layout.overrides]\nrail = \"hidden\"").unwrap();
        assert_eq!(c.config.global_layout().rail, RailVisibility::Visible);
        assert_eq!(c.warnings.len(), 1);

        // Both at once: sidebar forced left first, so the rail may stay hidden.
        let c = Config::parse("[layout.overrides]\ntabs_position = \"sidebar\"\nrail = \"hidden\"")
            .unwrap();
        let l = c.config.global_layout();
        assert_eq!(
            (l.sidebar.side, l.rail),
            (SidebarSide::Left, RailVisibility::Hidden)
        );

        // rail hidden is fine with a visible sidebar (no warning).
        let c = Config::parse(
            "[layout.overrides]\nrail = \"hidden\"\n[layout.overrides.sidebar]\nside = \"left\"",
        )
        .unwrap();
        assert!(c.warnings.is_empty());

        // Zen forces the chrome hidden and moves tabs out of the sidebar.
        let c = Config::parse(
            "[layout]\npreset = \"zen\"\n[layout.overrides]\ntabs_position = \"sidebar\"\nbottom_bar = \"full\"\n[layout.overrides.sidebar]\nside = \"left\"",
        )
        .unwrap();
        let l = c.config.global_layout();
        assert_eq!(l.sidebar.side, SidebarSide::Hidden);
        assert_eq!(l.bottom_bar, BottomBar::Hidden);
        assert_eq!(l.rail, RailVisibility::Hidden);
        assert_eq!(l.tabs_position, TabsPosition::Top);
        assert!(
            c.warnings.is_empty(),
            "zen enforcement is silent: {:?}",
            c.warnings
        );

        // zen = false lifts a zen preset's restrictions.
        let c = Config::parse(
            "[layout]\npreset = \"zen\"\n[layout.overrides]\nzen = false\nrail = \"visible\"\nbottom_bar = \"compact\"",
        )
        .unwrap();
        assert!(c.warnings.is_empty());
        assert!(!c.config.global_layout().zen);

        // Clamps, duplicates, empty sections, unknown preset: all non-fatal.
        let c = Config::parse(
            "[layout]\npreset = \"nope\"\n[layout.overrides]\nsidebar_translucency = 200\n[layout.overrides.sidebar]\nside = \"left\"\nwidth_px = 90\nsections = [\"agents\", \"agents\"]",
        )
        .unwrap();
        let l = c.config.global_layout();
        assert_eq!(l.sidebar.width_px, SIDEBAR_MIN_WIDTH);
        assert_eq!(l.sidebar_translucency, 100);
        assert_eq!(l.sidebar.sections, vec![SidebarSection::Agents]);
        assert_eq!(c.warnings.len(), 4, "{:?}", c.warnings);
        let c = Config::parse(
            "[layout.overrides.sidebar]\nside = \"left\"\nwidth_px = 900\nsections = []",
        )
        .unwrap();
        let l = c.config.global_layout();
        assert_eq!(l.sidebar.width_px, SIDEBAR_MAX_WIDTH);
        assert_eq!(l.sidebar.sections, vec![SidebarSection::Projects]);
        assert_eq!(c.warnings.len(), 2);

        // Corrected layouts are stable.
        for text in [
            "[layout.overrides]\ntabs_position = \"sidebar\"",
            "[layout.overrides]\nrail = \"hidden\"",
        ] {
            let mut l = Config::parse(text).unwrap().config.global_layout();
            assert!(l.normalize().is_empty());
        }
    }

    #[test]
    fn project_override_matching() {
        let text = r#"
[[layout.project_overrides]]
path = 'C:\Code'
[layout.project_overrides.overrides]
density = "compact"
breadcrumb = true

[[layout.project_overrides]]
path = "c:/code/APP/"
[layout.project_overrides.overrides]
density = "comfortable"
splits = "grid2x2"

[[layout.project_overrides]]
path = 'D:\Other'
[layout.project_overrides.overrides]
zen = true
"#;
        let cfg = Config::parse(text).unwrap().config;
        let g = cfg.global_layout();
        assert_eq!(g, ResolvedLayout::default());

        // Case-insensitive, either slash, trailing slash, verbatim prefix.
        for cwd in [
            r"C:\Code\App",
            r"c:\code\app\src\deep",
            r"C:/Code/App/",
            r"\\?\C:\CODE\app",
        ] {
            let l = cfg.layout_for(Path::new(cwd));
            assert_eq!(l.density, Density::Comfortable, "{cwd}");
            assert_eq!(l.splits, Splits::Grid2x2, "{cwd}");
            assert!(
                l.breadcrumb,
                "{cwd}: the less specific entry layers underneath"
            );
        }
        // Parent entry only.
        let l = cfg.layout_for(&PathBuf::from(r"C:\Code\Other\x"));
        assert_eq!(
            (l.density, l.breadcrumb, l.splits),
            (Density::Compact, true, Splits::Single)
        );
        // Component boundary: "App2" is not under "App", "Codex" is not under "Code".
        let l = cfg.layout_for(Path::new(r"C:\Code\App2"));
        assert_eq!(l.density, Density::Compact);
        assert_eq!(l.splits, Splits::Single);
        assert_eq!(cfg.layout_for(Path::new(r"C:\Codex")), g);
        // Another drive / unrelated path.
        assert_eq!(cfg.layout_for(Path::new(r"E:\Code\App")), g);
        assert!(cfg.layout_for(Path::new(r"D:\other\proj")).zen);
        // A bare drive root entry matches everything on that drive.
        let mut lc = LayoutConfig::default();
        lc.set_project_overrides(
            r"C:\",
            LayoutOverrides {
                density: Some(Density::Compact),
                ..Default::default()
            },
        );
        assert_eq!(
            lc.resolve_for(Path::new(r"C:\anything\x")).density,
            Density::Compact
        );
        assert_eq!(
            lc.resolve_for(Path::new(r"D:\x")).density,
            Density::Comfortable
        );
        // Empty overrides remove the entry.
        lc.set_project_overrides(r"c:/", LayoutOverrides::default());
        assert!(lc.project_overrides.is_empty());
    }

    #[test]
    fn project_override_warnings_only_when_the_project_causes_them() {
        let text = r#"
[[layout.project_overrides]]
path = 'C:\Code\x'
[layout.project_overrides.overrides]
rail = "hidden"

[[layout.project_overrides]]
path = ""
"#;
        let p = Config::parse(text).unwrap();
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
        assert!(p
            .warnings
            .iter()
            .any(|w| w.contains("C:\\Code\\x") && w.contains("rail")));
        assert!(p.warnings.iter().any(|w| w.contains("empty path")));
        // The corrected rail is what the UI gets.
        assert_eq!(
            p.config.layout_for(Path::new(r"C:\Code\x")).rail,
            RailVisibility::Visible
        );
    }

    #[test]
    fn custom_preset_roundtrip_and_resolution() {
        let mut cfg = Config::default();
        let mut mine = ResolvedLayout::builtin(BuiltinPreset::Chat);
        mine.splits = Splits::TwoHorizontal;
        mine.sidebar.width_px = 333;
        mine.right_panel = Some(PanelId::new("viewer"));
        cfg.layout.save_custom_preset("  Mi Layout ", mine.clone());
        assert_eq!(cfg.layout.preset, PresetId::Custom("Mi Layout".into()));
        assert_eq!(cfg.global_layout(), mine);

        let text = toml::to_string(&cfg).unwrap();
        let back = Config::parse(&text).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        assert_eq!(back.config, cfg);
        assert_eq!(back.config.global_layout(), mine);

        // Hand-written, partial custom preset: missing fields use the defaults.
        let c = Config::parse(
            "[layout]\npreset = \"quick\"\n[[layout.custom_presets]]\nname = \"Quick\"\nsplits = \"grid2x2\"\ndensity = \"compact\"\n",
        )
        .unwrap()
        .config;
        let l = c.global_layout();
        assert_eq!((l.splits, l.density), (Splits::Grid2x2, Density::Compact));
        assert_eq!(l.tabs_position, TabsPosition::Top);

        // Replacing by name does not duplicate; overrides apply on top.
        cfg.layout
            .save_custom_preset("mi layout", ResolvedLayout::default());
        assert_eq!(cfg.layout.custom_presets.len(), 1);
        assert_eq!(cfg.global_layout(), ResolvedLayout::default());

        // Shadowed / nameless / duplicate customs are reported, not fatal.
        let c = Config::parse(
            "[[layout.custom_presets]]\nname = \"zen\"\n[[layout.custom_presets]]\nname = \"a\"\n[[layout.custom_presets]]\nname = \"A\"\n[[layout.custom_presets]]\ndensity = \"compact\"",
        )
        .unwrap();
        assert_eq!(c.warnings.len(), 3, "{:?}", c.warnings);
        // A custom named like a built-in never wins over it.
        let lc = LayoutConfig {
            preset: PresetId::parse("zen"),
            custom_presets: vec![CustomPreset {
                name: "zen".into(),
                layout: ResolvedLayout::default(),
            }],
            ..Default::default()
        };
        assert!(lc.resolve().zen);

        // Export / import a single preset.
        let p = CustomPreset {
            name: "Share me".into(),
            layout: mine,
        };
        assert_eq!(CustomPreset::import_toml(&p.export_toml()).unwrap(), p);
        assert!(CustomPreset::import_toml("density = \"compact\"").is_err());
    }

    #[test]
    fn template_layout_section_parses_to_defaults() {
        assert!(TEMPLATE.contains("[layout]"));
        let p = Config::parse(TEMPLATE).unwrap();
        // The only difference with the serde defaults is the preset of new installs.
        let expected = LayoutConfig {
            preset: BuiltinPreset::EspaciosUnificados.into(),
            ..LayoutConfig::default()
        };
        assert_eq!(p.config.layout, expected);
        for b in BuiltinPreset::ALL {
            assert!(
                TEMPLATE.contains(&format!("\"{}\"", b.id())),
                "template lists {b:?}"
            );
        }
        for key in [
            "tabs_position",
            "side",
            "width_px",
            "sections",
            "group_by",
            "rail",
            "right_panel",
            "bottom_bar",
            "splits",
            "density",
            "sidebar_translucency",
            "breadcrumb",
            "reduced_motion",
            "zen",
        ] {
            assert!(
                TEMPLATE.contains(&format!("{key} =")),
                "template documents {key}"
            );
        }
    }

    #[test]
    fn set_layout_save_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = TEMPLATE
            .replace(
                "preset = \"espacios_unificados\"",
                "preset = \"espacios_unificados\" # mi preset",
            )
            .replace("mode = \"dark\"", "mode = \"dark\" # mi modo");
        std::fs::write(&path, format!("# cabecera propia\n{text}")).unwrap();
        let template_comment = TEMPLATE
            .lines()
            .find(|l| l.starts_with('#') && l.contains("orquestador"))
            .unwrap()
            .to_string();

        let mut cfg = Config::load_from(&path).unwrap().config;
        let mut layout = cfg.layout.clone();
        layout.select_preset(BuiltinPreset::Orquestador.into());
        let mut target = layout.resolve();
        target.density = Density::Comfortable;
        target.sidebar.width_px = 360;
        target.right_panel = Some(PanelId::new("git"));
        layout.set_resolved(&target);
        layout.set_project_overrides(
            r"C:\Code\App",
            LayoutOverrides {
                breadcrumb: Some(true),
                ..Default::default()
            },
        );
        cfg.set_layout(layout);
        cfg.save_to(&path).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# cabecera propia"), "{saved}");
        assert!(
            saved.contains("preset = \"orquestador\" # mi preset"),
            "{saved}"
        );
        assert!(saved.contains("mode = \"dark\" # mi modo"), "{saved}");
        assert!(saved.contains(&template_comment), "{saved}");
        assert!(saved.contains("# Selachi configuration"));
        let back = Config::load_from(&path).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        assert_eq!(back.config, cfg);
        assert_eq!(back.config.global_layout().sidebar.width_px, 360);
        assert!(
            back.config
                .layout_for(Path::new(r"c:\code\app\x"))
                .breadcrumb
        );
        assert!(!back.config.global_layout().breadcrumb);

        // Clearing overrides removes the stale keys from the file (no ghost values).
        let mut cleared = cfg.clone();
        cleared.layout.select_preset(BuiltinPreset::Zen.into());
        cleared
            .layout
            .set_project_overrides(r"C:\Code\App", LayoutOverrides::default());
        cleared.save_to(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("width_px = 360"), "{saved}");
        assert!(!saved.contains("\nbreadcrumb = true"), "{saved}");
        assert!(
            !saved.contains("[[layout.project_overrides]]\npath"),
            "{saved}"
        );
        assert!(saved.contains("# cabecera propia"));
        assert_eq!(Config::parse(&saved).unwrap().config, cleared);

        // Partial sidebar override shrinking does not leave ghost keys either.
        let mut a = cleared.clone();
        a.layout.overrides.sidebar = Some(SidebarOverrides {
            side: Some(SidebarSide::Left),
            width_px: Some(300),
            ..Default::default()
        });
        a.save_to(&path).unwrap();
        a.layout.overrides.sidebar = Some(SidebarOverrides {
            side: Some(SidebarSide::Right),
            ..Default::default()
        });
        a.save_to(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("width_px = 300"), "{saved}");
        assert_eq!(Config::parse(&saved).unwrap().config, a);
    }

    #[test]
    fn layout_changed_detects_only_layout_edits() {
        let a = Config::default();
        let mut b = a.clone();
        assert!(!layout_changed(&a, &b));
        b.appearance.accent = "#112233".into();
        b.terminal.scrollback = 5;
        assert!(!layout_changed(&a, &b));
        b.layout.overrides.density = Some(Density::Compact);
        assert!(layout_changed(&a, &b));
        let mut c = a.clone();
        c.layout.set_project_overrides(
            r"C:\x",
            LayoutOverrides {
                zen: Some(true),
                ..Default::default()
            },
        );
        assert!(layout_changed(&a, &c));
    }

    // ----- wireframe -----

    const EPS: f32 = 1e-3;

    fn check_tiling(name: &str, rects: &[WireRect]) {
        let mut area = 0.0;
        for r in rects {
            assert!(r.w > EPS && r.h > EPS, "{name}: empty rect {r:?}");
            assert!(r.x >= -EPS && r.y >= -EPS, "{name}: {r:?} out of bounds");
            assert!(
                r.x + r.w <= 100.0 + EPS && r.y + r.h <= 100.0 + EPS,
                "{name}: {r:?} out of bounds"
            );
            area += r.w * r.h;
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(ox <= EPS || oy <= EPS, "{name}: {a:?} overlaps {b:?}");
            }
        }
        assert!(
            (area - 10_000.0).abs() < 1.0,
            "{name}: zones cover {area}, not the whole square"
        );
    }

    fn count(rects: &[WireRect], z: Zone) -> usize {
        rects.iter().filter(|r| r.zone == z).count()
    }

    #[test]
    fn wireframe_every_preset_tiles_the_square() {
        for b in BuiltinPreset::ALL {
            let l = ResolvedLayout::builtin(b);
            let rects = wireframe(&l);
            check_tiling(b.id(), &rects);
            assert_eq!(rects, l.wireframe());
        }
    }

    #[test]
    fn wireframe_zones_per_preset() {
        let w = |b| wireframe(&ResolvedLayout::builtin(b));
        let t = w(BuiltinPreset::Terminal);
        assert_eq!(
            (
                count(&t, Zone::Rail),
                count(&t, Zone::Sidebar),
                count(&t, Zone::Tabs),
                count(&t, Zone::Main),
                count(&t, Zone::Right),
                count(&t, Zone::Status)
            ),
            (1, 0, 1, 1, 0, 1)
        );
        let e = w(BuiltinPreset::Espacios);
        assert_eq!(
            (
                count(&e, Zone::Sidebar),
                count(&e, Zone::Tabs),
                count(&e, Zone::Main)
            ),
            (1, 1, 2)
        );
        let o = w(BuiltinPreset::Orquestador);
        assert_eq!(
            (
                count(&o, Zone::Sidebar),
                count(&o, Zone::Tabs),
                count(&o, Zone::Main)
            ),
            (1, 1, 3)
        );
        // Vertical tabs sit inside the sidebar column, below its sections.
        let (sb, tabs) = (
            o.iter().find(|r| r.zone == Zone::Sidebar).unwrap(),
            o.iter().find(|r| r.zone == Zone::Tabs).unwrap(),
        );
        assert!((sb.x - tabs.x).abs() < EPS && (sb.w - tabs.w).abs() < EPS);
        assert!(tabs.y >= sb.y + sb.h - EPS);
        let c = w(BuiltinPreset::Chat);
        assert_eq!(
            (
                count(&c, Zone::Rail),
                count(&c, Zone::Right),
                count(&c, Zone::Main)
            ),
            (0, 1, 1)
        );
        let z = w(BuiltinPreset::Zen);
        assert_eq!(z.len(), 1);
        assert_eq!(
            z[0],
            WireRect {
                zone: Zone::Main,
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0
            }
        );
        // Left sidebar is right of the rail; right sidebar hugs the right edge.
        let e_sb = e.iter().find(|r| r.zone == Zone::Sidebar).unwrap();
        assert!((e_sb.x - 6.0).abs() < EPS);
    }

    #[test]
    fn wireframe_exhaustive_combinations_stay_valid() {
        let splits = [
            Splits::Single,
            Splits::TwoVertical,
            Splits::TwoHorizontal,
            Splits::OneAndTwoStacked,
            Splits::Grid2x2,
        ];
        let tabs = [
            TabsPosition::Top,
            TabsPosition::Sidebar,
            TabsPosition::Hidden,
        ];
        let sides = [SidebarSide::Left, SidebarSide::Right, SidebarSide::Hidden];
        let rails = [RailVisibility::Visible, RailVisibility::Hidden];
        let bars = [BottomBar::Compact, BottomBar::Full, BottomBar::Hidden];
        let panels = [None, Some(PanelId::new("git"))];
        let mut n = 0;
        for &s in &splits {
            for &t in &tabs {
                for &side in &sides {
                    for &r in &rails {
                        for &bar in &bars {
                            for p in &panels {
                                for (zen, width) in [(false, 180), (false, 480), (true, 300)] {
                                    let mut l = ResolvedLayout {
                                        splits: s,
                                        tabs_position: t,
                                        rail: r,
                                        bottom_bar: bar,
                                        right_panel: p.clone(),
                                        zen,
                                        ..Default::default()
                                    };
                                    l.sidebar.side = side;
                                    l.sidebar.width_px = width;
                                    l.normalize();
                                    check_tiling(&format!("{l:?}"), &wireframe(&l));
                                    n += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 5 * 3 * 3 * 2 * 3 * 2 * 3);
    }

    #[test]
    fn wireframe_splits_counts() {
        for (s, n) in [
            (Splits::Single, 1),
            (Splits::TwoVertical, 2),
            (Splits::TwoHorizontal, 2),
            (Splits::OneAndTwoStacked, 3),
            (Splits::Grid2x2, 4),
        ] {
            let l = ResolvedLayout {
                splits: s,
                ..Default::default()
            };
            assert_eq!(count(&wireframe(&l), Zone::Main), n);
        }
    }

    #[test]
    fn unified_preset_is_shell_v2() {
        let u = ResolvedLayout::builtin(BuiltinPreset::EspaciosUnificados);
        assert_eq!(u.tabs_position, TabsPosition::Sidebar);
        assert_eq!(u.rail, RailVisibility::Hidden);
        assert_eq!(u.sidebar.side, SidebarSide::Left);
        assert_eq!(u.sidebar.sections, vec![SidebarSection::Spaces]);
        let s = &u.sidebar;
        assert!(s.show_actions && !s.panel_icons && s.nested_tabs && s.footer_usage);
        assert!(s.row_model && s.row_time);
        let ids: Vec<&str> = u.titlebar.panel_buttons.iter().map(PanelId::as_str).collect();
        assert_eq!(ids, DEFAULT_TITLEBAR_BUTTONS);
        let mut n = u.clone();
        assert!(n.normalize().is_empty());
        assert_eq!(BuiltinPreset::from_id("ESPACIOS_UNIFICADOS"), Some(BuiltinPreset::EspaciosUnificados));
        // The classic presets keep the old chrome.
        for b in [BuiltinPreset::Terminal, BuiltinPreset::Espacios, BuiltinPreset::Orquestador, BuiltinPreset::Chat, BuiltinPreset::Zen] {
            let l = ResolvedLayout::builtin(b);
            assert!(!l.sidebar.nested_tabs && !l.sidebar.show_actions, "{b:?}");
            assert_eq!(l.titlebar.panel_buttons, vec![PanelId::new("layout")]);
        }
        // One full-height sidebar, no rail, no top tabs.
        let w = u.wireframe();
        assert_eq!(count(&w, Zone::Rail), 0);
        assert_eq!(count(&w, Zone::Tabs), 0);
        assert_eq!(count(&w, Zone::Sidebar), 1);
        let sb = w.iter().find(|r| r.zone == Zone::Sidebar).unwrap();
        assert!((sb.h - (100.0 - 6.0)).abs() < 0.01 && sb.x == 0.0);
    }

    #[test]
    fn sidebar_and_titlebar_options_roundtrip_as_overrides() {
        let mut c = LayoutConfig::default();
        c.select_preset(BuiltinPreset::EspaciosUnificados.into());
        let mut target = c.resolve();
        target.sidebar.footer_usage = false;
        target.sidebar.row_time = false;
        target.titlebar.panel_buttons = vec![PanelId::new("git"), PanelId::new("layout")];
        c.set_resolved(&target);
        assert_eq!(c.resolve(), target);
        let o = &c.overrides;
        assert_eq!(o.sidebar.as_ref().unwrap().footer_usage, Some(false));
        assert_eq!(o.sidebar.as_ref().unwrap().nested_tabs, None);
        assert_eq!(
            o.titlebar.as_ref().unwrap().panel_buttons,
            Some(vec![PanelId::new("git"), PanelId::new("layout")])
        );
        let text = toml::to_string(&Config { layout: c.clone(), ..Config::default() }).unwrap();
        assert!(text.contains("footer_usage = false"), "{text}");
        assert!(text.contains("panel_buttons"), "{text}");
        assert_eq!(Config::parse(&text).unwrap().config.layout, c);
        // Duplicates in the button list are dropped with a warning.
        let mut l = ResolvedLayout::default();
        l.titlebar.panel_buttons = vec![PanelId::new("git"), PanelId::new("git")];
        assert_eq!(l.normalize().len(), 1);
        assert_eq!(l.titlebar.panel_buttons.len(), 1);
    }

    #[test]
    fn new_config_fields_parse_validate_and_default() {
        let c = Config::default();
        assert_eq!(c.general.language, "es");
        assert!(c.general.confirm_close_running);
        assert!(!c.agents.oauth_usage_endpoint);
        assert_eq!(c.git.worktrees, WorktreeLocation::Beside);
        assert_eq!(c.plugins.effective_index_url(), DEFAULT_PLUGIN_INDEX_URL);
        assert!(DEFAULT_PLUGIN_INDEX_URL.starts_with("https://github.com/YewdielMartinez/Selachi/releases/"));
        assert!(!DEFAULT_PLUGIN_INDEX_URL.contains("OWNER"));
        let p = Config::parse(
            "[general]
language = \"es\"
confirm_close_running = false
[git]
external_editor = \"code -g\"
worktrees = \"inside\"
[plugins]
index_url = \"https://x.test/g.toml\"
[agents]
oauth_usage_endpoint = true
",
        )
        .unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(!p.config.general.confirm_close_running);
        assert_eq!(p.config.git.external_editor, "code -g");
        assert_eq!(p.config.git.worktrees, WorktreeLocation::Inside);
        assert_eq!(p.config.plugins.effective_index_url(), "https://x.test/g.toml");
        assert!(p.config.agents.oauth_usage_endpoint);
        let custom = Config::parse("[git]\nworktrees = 'D:\\wt'").unwrap().config;
        assert_eq!(custom.git.worktrees, WorktreeLocation::Custom(r"D:\wt".into()));
        assert!(Config::parse("[plugins]\nindex_url = \"http://insecure\"").is_err());
        let text = toml::to_string(&Config { git: GitConfig { worktrees: custom.git.worktrees.clone(), ..GitConfig::default() }, ..Config::default() }).unwrap();
        assert_eq!(Config::parse(&text).unwrap().config.git.worktrees, custom.git.worktrees);
    }
}
