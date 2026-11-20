//! Typed view of `config.toml`. Every field has a default, so a partial (or
//! empty) file is valid. Unknown keys are ignored by serde; `unknown_keys`
//! (in the parent module) reports them as warnings and `save()` leaves them
//! untouched in the file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
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
    /// Shell layout: preset, overrides, custom presets, per-project overrides
    /// (see `config/layout.rs`).
    pub layout: super::LayoutConfig,
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
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            default_profile: String::new(),
            scrollback: 10_000,
            cursor_style: CursorStyle::Block,
            copy_on_select: false,
            paste_ctrl_v: true,
        }
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
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            usage_week_start: WeekStart::Monday,
            show_estimates: true,
            permission_default: PermissionDefault::Ask,
            claude_statusline_hook: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    /// Ask before force-push, hard reset, branch delete and similar.
    pub confirm_sensitive: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            confirm_sensitive: true,
        }
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
        for (i, p) in self.profiles.iter().enumerate() {
            if p.name.trim().is_empty() || p.program.trim().is_empty() {
                problems.push(format!("profiles[{i}] needs a non-empty name and program"));
            }
        }
        problems
    }
}
