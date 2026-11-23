//! Typed view of `config.toml`. Every field has a default, so a partial (or
//! empty) file is valid. Unknown keys are ignored by serde; `unknown_keys`
//! (in the parent module) reports them as warnings and `save()` leaves them
//! untouched in the file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// App-wide behaviour: language, close confirmation (`[general]`).
    pub general: General,
    pub appearance: Appearance,
    pub terminal: Terminal,
    /// Extra shell profiles. Empty = the app auto-detects (pwsh, powershell, cmd, WSL).
    pub profiles: Vec<Profile>,
    /// Overrides only: action name -> chord (`"ctrl+shift+t"`). The app owns the
    /// default bindings; anything listed here replaces them.
    pub keybindings: BTreeMap<String, String>,
    pub agents: Agents,
    pub git: GitConfig,
    pub processes: Processes,
    pub retention: RetentionConfig,
    /// Restauración de la sesión al arrancar (`[session]`).
    pub session: SessionConfig,
    /// Shell layout: preset, overrides, custom presets, per-project overrides
    /// (see `config/layout.rs`).
    pub layout: super::LayoutConfig,
    /// Grammar plugins (`[plugins]`).
    pub plugins: Plugins,
}

/// `[general]`: app-wide behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// UI language id (`"es"`). Only Spanish exists today.
    pub language: String,
    /// Ask before closing a tab or the window while a command is still running.
    pub confirm_close_running: bool,
}

impl Default for General {
    fn default() -> Self {
        Self { language: "es".to_string(), confirm_close_running: true }
    }
}

/// Default grammar index: the latest release of the official repository.
pub const DEFAULT_PLUGIN_INDEX_URL: &str =
    "https://github.com/YewdielMartinez/Selachi/releases/latest/download/grammars.toml";

/// `[plugins]`: where language grammars come from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugins {
    /// URL of `grammars.toml` (empty = [`DEFAULT_PLUGIN_INDEX_URL`]). Must be `https://`.
    pub index_url: String,
}

impl Plugins {
    /// The index URL in effect.
    pub fn effective_index_url(&self) -> &str {
        let u = self.index_url.trim();
        if u.is_empty() { DEFAULT_PLUGIN_INDEX_URL } else { u }
    }
}

/// Where git worktrees created by Selachi live (`[git] worktrees`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorktreeLocation {
    /// Next to the repository folder (`<repo>-<branch>`).
    #[default]
    Beside,
    /// In `<repo>\.worktrees`.
    Inside,
    /// One folder for all of them.
    Custom(String),
}

impl WorktreeLocation {
    /// `"beside"`, `"inside"` or the folder path.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Beside => "beside",
            Self::Inside => "inside",
            Self::Custom(p) => p,
        }
    }

    /// Parses the config string; empty means the default.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "" | "beside" => Self::Beside,
            "inside" => Self::Inside,
            _ => Self::Custom(s.to_string()),
        }
    }
}

impl Serialize for WorktreeLocation {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WorktreeLocation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(d)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    Oled,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: ThemeMode,
    /// Brand accent as `#RRGGBB`.
    pub accent: String,
    pub ui_font: String,
    pub mono_font: String,
    /// Terminal font size in points.
    pub mono_size: f64,
    /// Line height multiplier for the terminal grid.
    pub line_height: f64,
    pub translucency: Translucency,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Dark,
            accent: "#00AAFF".to_string(),
            ui_font: "DM Sans".to_string(),
            mono_font: "Geist Mono".to_string(),
            mono_size: 13.0,
            line_height: 1.3,
            translucency: Translucency::default(),
        }
    }
}

impl Appearance {
    /// Accent as an `(r, g, b)` triple, or `None` if the string is not a valid hex colour.
    pub fn accent_rgb(&self) -> Option<(u8, u8, u8)> {
        parse_hex_color(&self.accent)
    }
}

/// Parse `#RGB` or `#RRGGBB` (the `#` is optional).
pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    if !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some((byte(0)?, byte(2)?, byte(4)?)),
        3 => {
            let nib = |i: usize| {
                u8::from_str_radix(h.get(i..i + 1)?, 16)
                    .ok()
                    .map(|v| v * 17)
            };
            Some((nib(0)?, nib(1)?, nib(2)?))
        }
        _ => None,
    }
}

/// Window translucency. Opacities are `0.0..=1.0` (1.0 = fully opaque).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Translucency {
    pub sidebar: f64,
    pub main: f64,
    /// Blur the backdrop behind translucent areas (Windows acrylic/mica).
    pub blur: bool,
}

impl Default for Translucency {
    fn default() -> Self {
        Self {
            sidebar: 1.0,
            main: 1.0,
            blur: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Terminal {
    /// Name of the profile to open by default. Empty = auto-detect.
    pub default_profile: String,
    /// Scrollback lines kept in memory (never persisted).
    pub scrollback: u32,
    pub cursor_style: CursorStyle,
    pub copy_on_select: bool,
    /// Ctrl+V pastes (instead of sending ^V to the program).
    pub paste_ctrl_v: bool,
    /// Tab completion and inline predictions of the shell (PSReadLine).
    pub completion: Completion,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            default_profile: String::new(),
            scrollback: 10_000,
            cursor_style: CursorStyle::Block,
            copy_on_select: false,
            paste_ctrl_v: true,
            completion: Completion::default(),
        }
    }
}

/// What the Tab key does in the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabMode {
    /// Accept the inline suggestion if there is one, otherwise open the completion menu.
    #[default]
    Smart,
    /// `TabCompleteNext`: cycle through the completions.
    Complete,
    /// `MenuComplete`: always open the completion menu.
    Menu,
    /// Leave the shell's own binding alone.
    Native,
}

impl TabMode {
    fn env_value(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Complete => "complete",
            Self::Menu => "menu",
            Self::Native => "native",
        }
    }
}

/// Where inline predictions come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionSource {
    /// Command history only.
    #[default]
    History,
    /// History plus prediction plugins (PSReadLine 2.2+; falls back to history).
    HistoryAndPlugin,
    /// No predictions.
    Off,
    /// Leave the shell's own setting alone.
    Native,
}

impl PredictionSource {
    fn env_value(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::HistoryAndPlugin => "history_and_plugin",
            Self::Off => "off",
            Self::Native => "native",
        }
    }
}

/// How predictions are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionView {
    /// Gray text after the cursor.
    #[default]
    Inline,
    /// A list under the prompt.
    List,
    /// Leave the shell's own setting alone.
    Native,
}

impl PredictionView {
    fn env_value(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::List => "list",
            Self::Native => "native",
        }
    }
}

/// `[terminal.completion]`: applied by the shell integration through environment variables
/// (see [`Config::completion_env`]); never touches the user's profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Completion {
    pub tab: TabMode,
    pub prediction: PredictionSource,
    pub prediction_view: PredictionView,
    /// Key that accepts the next word of the suggestion (`"Ctrl+RightArrow"`).
    /// `"native"` leaves the key alone; empty = the default.
    pub accept_word_key: String,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            tab: TabMode::Smart,
            prediction: PredictionSource::History,
            prediction_view: PredictionView::Inline,
            accept_word_key: "Ctrl+RightArrow".to_string(),
        }
    }
}

impl Completion {
    /// The environment variables `integration.ps1` reads. All four are always present, so the
    /// settings count as explicit (they override a custom binding from the user's profile
    /// except in `native` mode).
    pub fn env(&self) -> Vec<(String, String)> {
        let key = self.accept_word_key.trim();
        vec![
            ("SELACHI_TAB_MODE".to_string(), self.tab.env_value().to_string()),
            ("SELACHI_PREDICTION".to_string(), self.prediction.env_value().to_string()),
            ("SELACHI_PREDICTION_VIEW".to_string(), self.prediction_view.env_value().to_string()),
            (
                "SELACHI_ACCEPT_WORD_KEY".to_string(),
                if key.is_empty() { "Ctrl+RightArrow" } else { key }.to_string(),
            ),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekStart {
    #[default]
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDefault {
    #[default]
    Ask,
    AcceptEdits,
    ReadOnly,
    Full,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agents {
    pub usage_week_start: WeekStart,
    pub show_estimates: bool,
    pub permission_default: PermissionDefault,
    /// Install the Claude Code statusline hook (reads exact context/cost numbers).
    pub claude_statusline_hook: bool,
    /// Experimental: ask Anthropic's OAuth usage endpoint for the exact 5 h / weekly limits.
    pub oauth_usage_endpoint: bool,
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            usage_week_start: WeekStart::Monday,
            show_estimates: true,
            permission_default: PermissionDefault::Ask,
            claude_statusline_hook: false,
            oauth_usage_endpoint: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    /// Ask before force-push, hard reset, branch delete and similar.
    pub confirm_sensitive: bool,
    /// Command that opens a file in the user's editor (empty = the system default).
    pub external_editor: String,
    /// Where new worktrees are created.
    pub worktrees: WorktreeLocation,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            confirm_sensitive: true,
            external_editor: String::new(),
            worktrees: WorktreeLocation::Beside,
        }
    }
}

/// `[session]`: qué se restaura al abrir la aplicación.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Reabre las pestañas, los paneles divididos y sus carpetas de la última vez.
    /// Nunca se guarda el contenido de la terminal (ni el scrollback ni el entorno).
    pub restore: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { restore: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoStop {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Processes {
    /// What to do with processes a thread started when the thread is closed.
    pub auto_stop_on_thread_close: AutoStop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionConfig {
    /// Command history older than this is pruned (0 = keep forever).
    pub history_days: u32,
    /// Raw agent usage events older than this are pruned (applied by `agents_svc`).
    pub usage_raw_days: u32,
    /// Hard cap on command-history rows (0 = no cap).
    pub history_max_rows: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            history_days: 90,
            usage_raw_days: 30,
            history_max_rows: 500_000,
        }
    }
}

impl Config {
    /// Environment variables for the shell integration (Tab mode, predictions, accept-word key).
    ///
    /// Pass them to `term_core::Session::spawn_with_env`.
    pub fn completion_env(&self) -> Vec<(String, String)> {
        self.terminal.completion.env()
    }

    /// Semantic problems that serde cannot catch. A non-empty result makes the
    /// config invalid (the previous good one is kept on hot reload).
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let a = &self.appearance;
        if a.accent_rgb().is_none() {
            problems.push(format!(
                "appearance.accent = {:?} is not a hex colour (#RRGGBB)",
                a.accent
            ));
        }
        if !(a.mono_size.is_finite() && (4.0..=96.0).contains(&a.mono_size)) {
            problems.push(format!(
                "appearance.mono_size = {} must be within 4..=96",
                a.mono_size
            ));
        }
        if !(a.line_height.is_finite() && (0.8..=3.0).contains(&a.line_height)) {
            problems.push(format!(
                "appearance.line_height = {} must be within 0.8..=3.0",
                a.line_height
            ));
        }
        for (name, v) in [
            ("appearance.translucency.sidebar", a.translucency.sidebar),
            ("appearance.translucency.main", a.translucency.main),
        ] {
            if !(v.is_finite() && (0.0..=1.0).contains(&v)) {
                problems.push(format!("{name} = {v} must be within 0.0..=1.0"));
            }
        }
        if self.terminal.scrollback > 1_000_000 {
            problems.push(format!(
                "terminal.scrollback = {} exceeds the 1000000 line limit",
                self.terminal.scrollback
            ));
        }
        let key = &self.terminal.completion.accept_word_key;
        if key.len() > 64 || key.chars().any(|c| c.is_control() || c == '\'' || c == '"') {
            problems.push(format!(
                "terminal.completion.accept_word_key = {key:?} is not a key chord (e.g. \"Ctrl+RightArrow\")"
            ));
        }
        let url = self.plugins.index_url.trim();
        if !url.is_empty() && !url.starts_with("https://") {
            problems.push(format!("plugins.index_url = {url:?} must start with https://"));
        }
        if self.git.external_editor.chars().any(char::is_control) {
            problems.push("git.external_editor must not contain line breaks".to_string());
        }
        for (i, p) in self.profiles.iter().enumerate() {
            if p.name.trim().is_empty() || p.program.trim().is_empty() {
                problems.push(format!("profiles[{i}] needs a non-empty name and program"));
            }
        }
        problems
    }
}
