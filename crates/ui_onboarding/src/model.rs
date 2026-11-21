//! What the user picked ([`Draft`]) and how it becomes a `store::Config` (comment-preserving
//! save), the projects/favorites to import and the summary of step 6. Pure: no gpui.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use store::config::{PredictionSource, Profile, TabMode, ThemeMode};
use store::{BuiltinPreset, Config, Density, DiscoveredProject, PresetId, StateDb};

use crate::agents::{AgentRow, AgentStatus};
use crate::flow::Step;

/// Theme choices of step 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    System,
    Dark,
    Light,
    Oled,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 4] = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::Oled];

    pub const fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Sistema",
            ThemeChoice::Dark => "Oscuro",
            ThemeChoice::Light => "Claro",
            ThemeChoice::Oled => "OLED",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }

    pub const fn from_config(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::System => ThemeChoice::System,
            ThemeMode::Dark => ThemeChoice::Dark,
            ThemeMode::Light => ThemeChoice::Light,
            ThemeMode::Oled => ThemeChoice::Oled,
        }
    }

    pub const fn to_config(self) -> ThemeMode {
        match self {
            ThemeChoice::System => ThemeMode::System,
            ThemeChoice::Dark => ThemeMode::Dark,
            ThemeChoice::Light => ThemeMode::Light,
            ThemeChoice::Oled => ThemeMode::Oled,
        }
    }

    /// The concrete theme mode; `system_dark` says what "Sistema" means right now.
    pub const fn resolve(self, system_dark: bool) -> theme::Mode {
        match self {
            ThemeChoice::Dark => theme::Mode::Dark,
            ThemeChoice::Light => theme::Mode::Light,
            ThemeChoice::Oled => theme::Mode::Oled,
            ThemeChoice::System => {
                if system_dark {
                    theme::Mode::Dark
                } else {
                    theme::Mode::Light
                }
            }
        }
    }
}

/// Accent swatches: name and `#RRGGBB` (the first one is the brand accent).
pub const ACCENTS: [(&str, &str); 6] = [
    ("Cian", "#00AAFF"),
    ("Verde", "#7FCB8F"),
    ("Violeta", "#BC8CE8"),
    ("Ámbar", "#E0B96A"),
    ("Coral", "#E8707B"),
    ("Turquesa", "#4FC9D6"),
];

/// Index of `hex` in [`ACCENTS`] (case-insensitive).
pub fn accent_index(hex: &str) -> Option<usize> {
    ACCENTS.iter().position(|(_, h)| h.eq_ignore_ascii_case(hex.trim()))
}

/// Where new git worktrees go by default.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum WorktreeLocation {
    /// `<repo>-worktrees` next to the repository.
    #[default]
    Beside,
    /// `<repo>\.worktrees` inside the repository.
    Inside,
    /// One folder for all of them.
    Custom(PathBuf),
}

impl WorktreeLocation {
    pub fn describe(&self) -> String {
        match self {
            WorktreeLocation::Beside => "junto al repositorio".to_string(),
            WorktreeLocation::Inside => "dentro del repositorio (.worktrees)".to_string(),
            WorktreeLocation::Custom(p) => p.display().to_string(),
        }
    }
}

/// Choices that `store::Config` cannot hold yet (see the report for the wanted keys).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Extras {
    pub worktrees: WorktreeLocation,
    /// Claude OAuth usage endpoint (experimental, off by default).
    pub oauth_endpoint: bool,
}

/// Everything the wizard can change, as plain values.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub claude_statusline_hook: bool,
    pub oauth_endpoint: bool,
    pub worktrees: WorktreeLocation,
    pub theme: ThemeChoice,
    /// `#RRGGBB`.
    pub accent: String,
    /// `None`: the config uses a custom preset, no card is selected and it is left alone.
    pub preset: Option<BuiltinPreset>,
    /// Sidebar translucency, 0..=100.
    pub translucency: u8,
    pub mono_size: f32,
    pub density: Density,
    /// `None`: automatic (pwsh > powershell > cmd) or whatever the config already says.
    pub shell: Option<crate::shells::ShellInfo>,
    pub tab: TabMode,
    pub prediction: PredictionSource,
    pub restore_session: bool,
}

impl Draft {
    /// The wizard's starting point: the values of `config`.
    pub fn from_config(config: &Config) -> Self {
        let layout = config.layout.resolve();
        Self {
            claude_statusline_hook: config.agents.claude_statusline_hook,
            oauth_endpoint: false,
            worktrees: WorktreeLocation::default(),
            theme: ThemeChoice::from_config(config.appearance.mode),
            accent: config.appearance.accent.clone(),
            preset: match &config.layout.preset {
                PresetId::Builtin(b) => Some(*b),
                PresetId::Custom(_) => None,
            },
            translucency: layout.sidebar_translucency,
            mono_size: config.appearance.mono_size as f32,
            density: layout.density,
            shell: None,
            tab: config.terminal.completion.tab,
            prediction: config.terminal.completion.prediction,
            restore_session: config.session.restore,
        }
    }

    /// Config with the choices of every step that was not skipped applied on top of `base`.
    /// Save it with `Config::save_to` to keep the file's comments.
    pub fn build_config(&self, base: &Config, skipped: &BTreeSet<Step>) -> Config {
        let mut cfg = base.clone();
        if !skipped.contains(&Step::Agents) {
            cfg.agents.claude_statusline_hook = self.claude_statusline_hook;
        }
        if !skipped.contains(&Step::Appearance) {
            cfg.appearance.mode = self.theme.to_config();
            cfg.appearance.accent = self.accent.clone();
            cfg.appearance.mono_size = f64::from(self.mono_size);
            if let Some(p) = self.preset
                && cfg.layout.preset != PresetId::Builtin(p)
            {
                cfg.layout.select_preset(PresetId::Builtin(p));
            }
            let mut target = cfg.layout.resolve();
            target.density = self.density;
            target.sidebar_translucency = self.translucency;
            cfg.layout.set_resolved(&target);
        }
        if !skipped.contains(&Step::Terminal) {
            cfg.terminal.completion.tab = self.tab;
            cfg.terminal.completion.prediction = self.prediction;
            cfg.session.restore = self.restore_session;
            if let Some(shell) = &self.shell {
                let profile = Profile {
                    name: shell.name.clone(),
                    program: shell.program.clone(),
                    args: shell.args.clone(),
                    ..Profile::default()
                };
                match cfg.profiles.iter_mut().find(|p| p.name == profile.name) {
                    Some(slot) => *slot = profile,
                    None => cfg.profiles.push(profile),
                }
                cfg.terminal.default_profile = shell.name.clone();
            }
        }
        cfg
    }

    pub fn extras(&self, skipped: &BTreeSet<Step>) -> Extras {
        Extras {
            worktrees: if skipped.contains(&Step::Projects) { WorktreeLocation::default() } else { self.worktrees.clone() },
            oauth_endpoint: !skipped.contains(&Step::Agents) && self.oauth_endpoint,
        }
    }
}

/// The result of the wizard.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub config: Config,
    pub projects: Vec<DiscoveredProject>,
    pub favorites: Vec<PathBuf>,
    pub extras: Extras,
}

/// Write the outcome: `config.toml` (comments kept), projects and favorites into `state.db`, and
/// the first-run flag, but only when the config was saved (so a failed save shows the wizard
/// again next time). Blocking I/O: call it from a background thread. Returns the first error.
pub fn persist(dir: &Path, outcome: &Outcome, db: Option<&StateDb>) -> Result<(), String> {
    let mut first_error = None;
    let mut note = |what: &str, e: &dyn std::fmt::Display| {
        log::warn!("onboarding: {what}: {e}");
        first_error.get_or_insert_with(|| format!("{what}: {e}"));
    };
    let config_saved = match outcome.config.save_to(&store::paths::config_path_in(dir)) {
        Ok(()) => true,
        Err(e) => {
            note("no se pudo guardar config.toml", &e);
            false
        }
    };
    if let Some(db) = db {
        if !outcome.projects.is_empty()
            && let Err(e) = db.save_projects(&outcome.projects)
        {
            note("no se pudieron guardar los proyectos", &e);
        }
        for fav in &outcome.favorites {
            if let Err(e) = db.add_favorite(&fav.to_string_lossy(), "") {
                note("no se pudo guardar un favorito", &e);
            }
        }
    }
    if config_saved
        && let Err(e) = crate::flag::mark_done_in(dir)
    {
        note("no se pudo guardar la bandera de primer arranque", &e);
    }
    first_error.map_or(Ok(()), Err)
}

/// One line of the summary on the last step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryLine {
    pub step: Step,
    pub title: &'static str,
    pub value: String,
    pub skipped: bool,
}

/// Counts shown by the projects line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectCounts {
    pub selected: usize,
    pub favorites: usize,
}

/// The summary of what will be configured.
pub fn summary_lines(
    draft: &Draft,
    skipped: &BTreeSet<Step>,
    agents: &[AgentRow],
    projects: ProjectCounts,
    shell_label: &str,
) -> Vec<SummaryLine> {
    let line = |step: Step, title: &'static str, value: String| SummaryLine {
        step,
        title,
        skipped: skipped.contains(&step),
        value: if skipped.contains(&step) { "Omitido: se queda como está".to_string() } else { value },
    };
    let ready: Vec<&str> = agents.iter().filter(|a| a.status == AgentStatus::Ready).map(|a| a.cli.name()).collect();
    let pending = agents
        .iter()
        .filter(|a| a.status.offers_login() || a.status == AgentStatus::NotInstalled)
        .count();
    let agents_value = match (ready.is_empty(), pending) {
        (true, 0) => "Sin agentes detectados".to_string(),
        (true, n) => format!("Ninguno listo todavía ({n} por configurar)"),
        (false, 0) => format!("{} listos", ready.join(", ")),
        (false, n) => format!("{} listos · {n} por configurar", ready.join(", ")),
    };
    let hook = if draft.claude_statusline_hook { " · hook statusLine de Claude" } else { "" };
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let projects_value = format!(
        "{} · {} · worktrees {}",
        plural(projects.selected, "proyecto", "proyectos"),
        plural(projects.favorites, "favorito", "favoritos"),
        draft.worktrees.describe(),
    );
    let preset = draft.preset.map_or("personalizado", |p| p.id());
    let appearance_value = format!(
        "{} · acento {} · layout {} · letra {} pt · densidad {}",
        draft.theme.label(),
        draft.accent.to_uppercase(),
        preset,
        draft.mono_size,
        if draft.density == Density::Compact { "compacta" } else { "cómoda" },
    );
    let tab = match draft.tab {
        TabMode::Smart => "Tab inteligente",
        TabMode::Complete => "Tab recorre opciones",
        TabMode::Menu => "Tab abre menú",
        TabMode::Native => "Tab nativo",
    };
    let prediction = match draft.prediction {
        PredictionSource::History => "predicción por historial",
        PredictionSource::HistoryAndPlugin => "predicción con plugins",
        PredictionSource::Off => "sin predicción",
        PredictionSource::Native => "predicción nativa",
    };
    let restore = if draft.restore_session { "restaura la sesión" } else { "no restaura la sesión" };
    vec![
        line(Step::Agents, "Agentes", format!("{agents_value}{hook}")),
        line(Step::Projects, "Proyectos", projects_value),
        line(Step::Appearance, "Apariencia", appearance_value),
        line(Step::Terminal, "Terminal", format!("{shell_label} · {tab} · {prediction} · {restore}")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentCli;
    use crate::shells::{ShellInfo, ShellKind};

    fn skipped(steps: &[Step]) -> BTreeSet<Step> {
        steps.iter().copied().collect()
    }

    fn cmd_shell() -> ShellInfo {
        ShellInfo {
            kind: ShellKind::Cmd,
            name: "Símbolo del sistema".into(),
            program: "cmd.exe".into(),
            args: vec![],
            detail: String::new(),
        }
    }

    #[test]
    fn untouched_draft_changes_nothing() {
        let base = Config::default();
        let draft = Draft::from_config(&base);
        assert_eq!(draft.build_config(&base, &BTreeSet::new()), base);
    }

    #[test]
    fn selections_land_in_the_config() {
        let base = Config::default();
        let mut d = Draft::from_config(&base);
        d.claude_statusline_hook = true;
        d.theme = ThemeChoice::Light;
        d.accent = "#BC8CE8".into();
        d.preset = Some(BuiltinPreset::Espacios);
        d.translucency = 35;
        d.mono_size = 15.0;
        d.density = Density::Compact;
        d.tab = TabMode::Menu;
        d.prediction = PredictionSource::Off;
        d.restore_session = false;
        d.shell = Some(cmd_shell());
        let cfg = d.build_config(&base, &BTreeSet::new());
        assert!(cfg.agents.claude_statusline_hook);
        assert_eq!(cfg.appearance.mode, ThemeMode::Light);
        assert_eq!(cfg.appearance.accent, "#BC8CE8");
        assert_eq!(cfg.appearance.mono_size, 15.0);
        assert_eq!(cfg.layout.preset, PresetId::Builtin(BuiltinPreset::Espacios));
        let l = cfg.layout.resolve();
        assert_eq!(l.sidebar_translucency, 35);
        assert_eq!(l.density, Density::Compact);
        assert_eq!(cfg.terminal.completion.tab, TabMode::Menu);
        assert_eq!(cfg.terminal.completion.prediction, PredictionSource::Off);
        assert!(!cfg.session.restore);
        assert_eq!(cfg.terminal.default_profile, "Símbolo del sistema");
        assert_eq!(cfg.profiles.len(), 1);
        assert_eq!(cfg.profiles[0].program, "cmd.exe");
        assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
    }

    #[test]
    fn skipped_steps_do_not_touch_the_config() {
        let base = Config::default();
        let mut d = Draft::from_config(&base);
        d.claude_statusline_hook = true;
        d.theme = ThemeChoice::Oled;
        d.tab = TabMode::Native;
        d.shell = Some(cmd_shell());
        let cfg = d.build_config(&base, &skipped(&[Step::Agents, Step::Appearance, Step::Terminal]));
        assert_eq!(cfg, base);
        // Only the appearance one is applied when the others are skipped.
        let cfg = d.build_config(&base, &skipped(&[Step::Agents, Step::Terminal]));
        assert_eq!(cfg.appearance.mode, ThemeMode::Oled);
        assert!(!cfg.agents.claude_statusline_hook);
        assert_eq!(cfg.terminal.completion.tab, TabMode::Smart);
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn an_existing_profile_is_replaced_not_duplicated() {
        let mut base = Config::default();
        base.profiles.push(Profile { name: "Símbolo del sistema".into(), program: "old.exe".into(), ..Profile::default() });
        let mut d = Draft::from_config(&base);
        d.shell = Some(cmd_shell());
        let cfg = d.build_config(&base, &BTreeSet::new());
        assert_eq!(cfg.profiles.len(), 1);
        assert_eq!(cfg.profiles[0].program, "cmd.exe");
    }

    #[test]
    fn keeping_a_custom_preset_and_its_overrides() {
        let mut base = Config::default();
        base.layout.select_preset(PresetId::Custom("Mi layout".into()));
        let d = Draft::from_config(&base);
        assert_eq!(d.preset, None);
        let cfg = d.build_config(&base, &BTreeSet::new());
        assert_eq!(cfg.layout.preset, base.layout.preset);
    }

    #[test]
    fn saving_through_store_keeps_comments_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = format!("# my hand-written header\n{}\n[experimental]\nkeep_me = true\n", store::config::TEMPLATE);
        std::fs::write(&path, &text).unwrap();
        let base = Config::load_from(&path).unwrap().config;
        let mut d = Draft::from_config(&base);
        d.theme = ThemeChoice::Light;
        d.preset = Some(BuiltinPreset::Chat);
        d.density = Density::Compact;
        d.mono_size = 14.0;
        d.shell = Some(cmd_shell());
        let outcome = Outcome {
            config: d.build_config(&base, &BTreeSet::new()),
            projects: vec![],
            favorites: vec![],
            extras: Extras::default(),
        };
        persist(dir.path(), &outcome, None).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with("# my hand-written header"));
        assert!(saved.contains("# Edit by hand; changes are picked up"), "template comments survive");
        assert!(saved.contains("[experimental]") && saved.contains("keep_me = true"));
        assert!(saved.contains("mode = \"light\""));
        assert!(saved.contains("preset = \"chat\""));
        assert!(saved.contains("default_profile = \"Símbolo del sistema\""));
        let reloaded = Config::load_from(&path).unwrap();
        assert_eq!(reloaded.config, outcome.config);
        assert!(reloaded.warnings.iter().any(|w| w == "experimental"), "unknown keys only warn");
        assert!(crate::flag::flag_path(dir.path()).is_file(), "flag written after a good save");
    }

    #[test]
    fn a_failed_config_save_leaves_the_wizard_pending() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "this = is [not toml").unwrap();
        let outcome = Outcome {
            config: Config::default(),
            projects: vec![],
            favorites: vec![],
            extras: Extras::default(),
        };
        let err = persist(dir.path(), &outcome, None).unwrap_err();
        assert!(err.contains("config.toml"), "{err}");
        assert!(crate::flag::needs_onboarding_in(dir.path()));
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), "this = is [not toml");
    }

    #[test]
    fn projects_and_favorites_reach_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        let p = |name: &str| DiscoveredProject {
            path: PathBuf::from(format!("C:\\dev\\{name}")),
            name: name.to_string(),
            kind: store::ProjectKind::Rust,
            is_git: true,
        };
        let outcome = Outcome {
            config: Config::default(),
            projects: vec![p("one"), p("two")],
            favorites: vec![PathBuf::from("C:\\dev\\two")],
            extras: Extras::default(),
        };
        persist(dir.path(), &outcome, Some(&db)).unwrap();
        assert_eq!(db.projects().unwrap().len(), 2);
        let favs = db.favorites().unwrap();
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0].label, "two");
        persist(dir.path(), &outcome, Some(&db)).unwrap(); // re-running the wizard is idempotent
        assert_eq!(db.projects().unwrap().len(), 2);
        assert_eq!(db.favorites().unwrap().len(), 1);
    }

    #[test]
    fn theme_choice_round_trips_and_resolves() {
        for c in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_config(c.to_config()), c);
            assert_eq!(ThemeChoice::ALL[c.index()], c);
        }
        assert_eq!(ThemeChoice::System.resolve(true), theme::Mode::Dark);
        assert_eq!(ThemeChoice::System.resolve(false), theme::Mode::Light);
        assert_eq!(ThemeChoice::Oled.resolve(false), theme::Mode::Oled);
    }

    #[test]
    fn accents_are_valid_and_found_by_hex() {
        for (_, hex) in ACCENTS {
            assert!(store::config::parse_hex_color(hex).is_some(), "{hex}");
        }
        assert_eq!(accent_index("#00aaff"), Some(0));
        assert_eq!(accent_index("#BC8CE8"), Some(2));
        assert_eq!(accent_index("#123456"), None);
    }

    #[test]
    fn extras_ignore_skipped_steps() {
        let mut d = Draft::from_config(&Config::default());
        d.oauth_endpoint = true;
        d.worktrees = WorktreeLocation::Inside;
        let e = d.extras(&BTreeSet::new());
        assert!(e.oauth_endpoint);
        assert_eq!(e.worktrees, WorktreeLocation::Inside);
        let e = d.extras(&skipped(&[Step::Agents, Step::Projects]));
        assert_eq!(e, Extras::default());
    }

    #[test]
    fn summary_describes_choices_and_marks_skipped_steps() {
        let d = Draft::from_config(&Config::default());
        let agents = vec![
            AgentRow { cli: AgentCli::Claude, status: AgentStatus::Ready, version: None, path: None },
            AgentRow { cli: AgentCli::Codex, status: AgentStatus::NotInstalled, version: None, path: None },
        ];
        let lines = summary_lines(&d, &skipped(&[Step::Terminal]), &agents, ProjectCounts { selected: 5, favorites: 1 }, "PowerShell 7");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].value, "Claude Code listos · 1 por configurar");
        assert!(lines[1].value.starts_with("5 proyectos · 1 favorito"));
        assert!(lines[2].value.contains("Oscuro") && lines[2].value.contains("#00AAFF") && lines[2].value.contains("terminal"));
        assert!(lines[3].skipped && lines[3].value.starts_with("Omitido"));
        let none = summary_lines(&d, &BTreeSet::new(), &[], ProjectCounts::default(), "auto");
        assert_eq!(none[0].value, "Sin agentes detectados");
    }
}
