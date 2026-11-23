//! `SettingsView`: the settings screen (section list on the left, content on the right).
//!
//! The view owns a working copy of `store::Config`. Every control edits that copy through [`SettingsView::edit`]:
//! the candidate is validated with `Config::validate()`, an invalid value is *not* applied and
//! shows an inline error, a valid one is applied at once (`SettingsEvent::ConfigChanged`, so the
//! app reacts live) and written to `config.toml` after 400 ms of quiet, one write in flight at a
//! time (`SaveGate`). A hot reload (`set_config`) is merged three-way so it never clobbers an
//! edit in progress.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, Subscription, Task, actions, point, px};
use store::Config;
use theme::Theme;
use ui_kit::Icon;
use viewer_svc::grammars::GrammarRegistry;

use crate::agents::AgentsState;
use crate::keys::{self, KeyAction};
use crate::net::{Fetcher, HttpFetcher};
use crate::plugins::{PluginsEvent, PluginsModel};
use crate::save::{SAVE_DEBOUNCE, SaveGate, merge_external};
use crate::text_field::{TextField, TextFieldEvent};
use crate::validate::{FieldError, first_error};

actions!(ui_settings, [FocusNextField, FocusPrevField]);

/// Key context of the view root.
pub(crate) const CONTEXT: &str = "SettingsView";

/// The sections of the left list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    General,
    Appearance,
    Design,
    Terminal,
    Agents,
    Git,
    Processes,
    Plugins,
    Keyboard,
    About,
}

impl Section {
    pub const ALL: [Section; 10] = [
        Section::General,
        Section::Appearance,
        Section::Design,
        Section::Terminal,
        Section::Agents,
        Section::Git,
        Section::Processes,
        Section::Plugins,
        Section::Keyboard,
        Section::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Appearance => "Apariencia",
            Section::Design => "Diseño",
            Section::Terminal => "Terminal",
            Section::Agents => "Agentes",
            Section::Git => "Git",
            Section::Processes => "Procesos",
            Section::Plugins => "Plugins",
            Section::Keyboard => "Teclado",
            Section::About => "Acerca de",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Section::General => "Idioma, sesión y cierre.",
            Section::Appearance => "Temas, acento, tipografía y densidad.",
            Section::Design => "Presets, pestañas, barra lateral, paneles y zonas.",
            Section::Terminal => "Shell, perfiles, cursor y completado.",
            Section::Agents => "CLI detectados, permisos y uso.",
            Section::Git => "Confirmaciones y editor externo.",
            Section::Processes => "Qué pasa con los procesos de un hilo al cerrarlo.",
            Section::Plugins => "Resaltado de código para más lenguajes.",
            Section::Keyboard => "Atajos y conflictos.",
            Section::About => "Versión, carpetas y primer arranque.",
        }
    }

    /// Icon of the list item. Placeholders until the dedicated pixel icons exist (TODO-SVG:
    /// appearance, keyboard, plugin, about; see the crate report).
    pub fn icon(self) -> Icon {
        match self {
            Section::General => Icon::Settings,
            Section::Appearance => Icon::Files, // TODO-SVG appearance.svg
            Section::Design => Icon::Layout,
            Section::Terminal => Icon::Terminal,
            Section::Agents => Icon::Agents,
            Section::Git => Icon::GitBranch,
            Section::Processes => Icon::Procs,
            Section::Plugins => Icon::Snippet, // TODO-SVG plugin.svg
            Section::Keyboard => Icon::Task,   // TODO-SVG keyboard.svg
            Section::About => Icon::Doctor,    // TODO-SVG info.svg
        }
    }
}

/// What the host gives the view at creation.
#[derive(Clone)]
pub struct SettingsOptions {
    /// `config.toml`: where edits are written.
    pub config_path: PathBuf,
    /// App version shown in "Acerca de".
    pub version: String,
    /// Plugin registry (default: the process-wide one, else `<data dir>/plugins/grammars`).
    pub registry: Option<Arc<GrammarRegistry>>,
    /// HTTP client for the plugin index and archives (default: [`HttpFetcher`], https only).
    pub fetcher: Option<Arc<dyn Fetcher>>,
    /// Rebindable actions (default: [`keys::default_catalog`]).
    pub key_catalog: Option<Vec<KeyAction>>,
}

impl SettingsOptions {
    pub fn new(config_path: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            config_path: config_path.into(),
            version: version.into(),
            registry: None,
            fetcher: None,
            key_catalog: None,
        }
    }
}

/// What the view tells the host (and, for `InstallGrammar`, what the host can tell the view).
#[derive(Clone, Debug)]
pub enum SettingsEvent {
    /// A valid edit was applied (before it is saved): apply theme, accent, fonts... live.
    ConfigChanged(Arc<Config>),
    /// The debounced write finished.
    Saved,
    /// The write failed (message). The edit stays in memory and is retried on the next edit.
    SaveFailed(String),
    /// "Más opciones de layout": open the layout panel.
    OpenLayoutPanel,
    /// "Volver a configurar": run the first-start wizard again.
    RerunOnboarding,
    /// Deep link from the viewer's banner (`ViewerEvent::InstallGrammar`): show Plugins with `id`
    /// highlighted and its install confirmation open. Nothing is installed without a click.
    InstallGrammar { id: String },
    /// A plugin was installed or updated (`restart`: it takes effect after restarting).
    PluginInstalled { id: String, restart: bool },
    /// A plugin was removed (`restart`: it is deleted at the next start).
    PluginRemoved { id: String, restart: bool },
}

/// Pending confirmation of a plugin action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmKind {
    Install,
    Update,
    Remove,
}

/// A shortcut being captured.
#[derive(Clone, Debug)]
pub(crate) struct Capture {
    pub action: String,
    /// Chord that is already taken, and by whom: waiting for "Intercambiar" or "Cancelar".
    pub conflict: Option<(String, String)>,
    pub error: Option<String>,
    /// The chord was captured (the interceptor is gone); only the conflict prompt remains.
    pub captured: bool,
}

/// The profile being edited. Environment variables live in a draft (stable order while typing).
#[derive(Clone, Debug)]
pub(crate) struct ProfileEditor {
    pub index: usize,
    pub env: Vec<(String, String)>,
}

pub struct SettingsView {
    pub(crate) theme: Theme,
    pub(crate) cfg: Config,
    /// Last config known to be on disk (base of the three-way merge).
    base: Config,
    pub(crate) path: PathBuf,
    pub(crate) version: String,
    pub(crate) section: Section,
    gate: SaveGate,
    save_task: Option<Task<()>>,
    touched: bool,
    save_error: Option<String>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) fields: HashMap<String, Entity<TextField>>,
    field_subs: HashMap<String, Subscription>,
    pub(crate) errors: HashMap<String, String>,
    pub(crate) open_select: Option<&'static str>,
    nav_open: bool,
    pub(crate) profile_editor: Option<ProfileEditor>,
    pub(crate) plugins: Entity<PluginsModel>,
    /// The embedded layout panel (created the first time Diseño is shown).
    pub(crate) layout_panel: Option<Entity<ui_layout::LayoutPanel>>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) plugin_focus: Option<String>,
    pub(crate) confirm: Option<(String, ConfirmKind)>,
    pub(crate) agents: AgentsState,
    pub(crate) agents_task: Option<Task<()>>,
    pub(crate) catalog: Vec<KeyAction>,
    pub(crate) capture: Option<Capture>,
    pub(crate) capture_sub: Option<Subscription>,
    pub(crate) width_hint: Option<f32>,
    /// The zone width really measured while painting (preferred over the window width).
    pub(crate) measured: ui_kit::MeasuredWidth,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Register the key bindings (Tab / Shift+Tab between fields, text editing). Call once at startup.
pub fn init(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextField, Some(CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevField, Some(CONTEXT)),
    ]);
    crate::text_field::bind_keys(cx);
    ui_layout::init(cx);
}

/// Create the view as an entity.
pub fn settings_view(config: Config, theme: Theme, options: SettingsOptions, cx: &mut App) -> Entity<SettingsView> {
    cx.new(|cx| SettingsView::new(config, theme, options, cx))
}

impl SettingsView {
    pub fn new(config: Config, theme: Theme, options: SettingsOptions, cx: &mut Context<Self>) -> Self {
        let registry = options.registry.clone().unwrap_or_else(|| {
            viewer_svc::grammars::plugin_registry().unwrap_or_else(|| {
                let dir = options
                    .config_path
                    .parent()
                    .map(|p| p.join("plugins").join("grammars"))
                    .unwrap_or_else(|| PathBuf::from("plugins/grammars"));
                Arc::new(GrammarRegistry::new(dir))
            })
        });
        let fetcher: Arc<dyn Fetcher> = options.fetcher.clone().unwrap_or_else(|| Arc::new(HttpFetcher));
        let index_url = config.plugins.effective_index_url().to_string();
        let plugins = cx.new(|_| PluginsModel::new(registry, fetcher, index_url));
        let mut subs = Vec::new();
        subs.push(cx.observe(&plugins, |_, _, cx| cx.notify()));
        subs.push(cx.subscribe(&plugins, |_, _, ev: &PluginsEvent, cx| match ev {
            PluginsEvent::Installed { id, restart } => {
                cx.emit(SettingsEvent::PluginInstalled { id: id.clone(), restart: *restart });
            }
            PluginsEvent::Removed { id, restart } => {
                cx.emit(SettingsEvent::PluginRemoved { id: id.clone(), restart: *restart });
            }
        }));
        Self {
            theme,
            base: config.clone(),
            cfg: config,
            path: options.config_path,
            version: options.version,
            section: Section::General,
            gate: SaveGate::default(),
            save_task: None,
            touched: false,
            save_error: None,
            scroll: ScrollHandle::new(),
            fields: HashMap::new(),
            field_subs: HashMap::new(),
            errors: HashMap::new(),
            open_select: None,
            nav_open: false,
            profile_editor: None,
            plugins,
            layout_panel: None,
            cwd: None,
            plugin_focus: None,
            confirm: None,
            agents: AgentsState::Unknown,
            agents_task: None,
            catalog: options.key_catalog.unwrap_or_else(keys::default_catalog),
            capture: None,
            capture_sub: None,
            width_hint: None,
            measured: ui_kit::MeasuredWidth::new(crate::ui::NARROW_BELOW),
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    // ----- public API -------------------------------------------------------------------------

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn section(&self) -> Section {
        self.section
    }

    /// The plugin manager (for the host: `registry`, rows, events).
    pub fn plugins(&self) -> &Entity<PluginsModel> {
        &self.plugins
    }

    /// There are edits not yet on disk (or a write is running).
    pub fn has_pending_save(&self) -> bool {
        self.gate.is_pending()
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            for f in self.fields.values() {
                f.update(cx, |f, cx| {
                    f.set_theme(theme);
                    cx.notify();
                });
            }
            if let Some(p) = &self.layout_panel {
                p.update(cx, |p, cx| p.set_theme(theme, cx));
            }
            cx.notify();
        }
    }

    /// Working directory of the active terminal: enables the "Este proyecto" scope of the embedded
    /// layout panel.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.cwd != cwd {
            self.cwd = cwd.clone();
            if let Some(p) = &self.layout_panel {
                p.update(cx, |p, cx| p.set_cwd(cwd, cx));
            }
        }
    }

    /// Keep the embedded layout panel in step with `cfg.layout` (a no-op when equal).
    pub(crate) fn sync_layout_panel(&self, cx: &mut Context<Self>) {
        if let Some(p) = &self.layout_panel {
            let layout = self.cfg.layout.clone();
            p.update(cx, |p, cx| p.set_config(layout, cx));
        }
    }

    /// Width the view really has (the shell passes its zone width). Without it the window width
    /// decides between the section list and the top menu.
    pub fn set_width_hint(&mut self, width: Option<f32>, cx: &mut Context<Self>) {
        if self.width_hint != width {
            self.width_hint = width;
            cx.notify();
        }
    }

    /// Replace the rebindable actions (the app knows its real catalog).
    pub fn set_key_catalog(&mut self, catalog: Vec<KeyAction>, cx: &mut Context<Self>) {
        self.catalog = catalog;
        cx.notify();
    }

    pub fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section != section {
            self.section = section;
            self.open_select = None;
            self.confirm = None;
            self.nav_open = false;
            self.cancel_capture();
            self.scroll.set_offset(point(px(0.0), px(0.0)));
        }
        match section {
            Section::Plugins => self.plugins.update(cx, |m, cx| m.ensure_index(cx)),
            Section::Agents => self.ensure_agents(false, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Scroll the content column to its end (screenshots, deep links to the last blocks).
    pub fn scroll_to_end(&self) {
        self.scroll.scroll_to_bottom();
    }

    /// Deep link: show Plugins with `id` highlighted and its confirmation open.
    pub fn open_plugin(&mut self, id: &str, cx: &mut Context<Self>) {
        self.set_section(Section::Plugins, cx);
        self.plugin_focus = Some(id.to_string());
        let installed = self.plugins.read(cx).rows().iter().any(|r| r.id == id && !r.can_install());
        // The confirmation needs the index (size and version), so it opens once the row is
        // installable; until the index arrives the row is simply highlighted.
        self.confirm = (!installed).then(|| (id.to_string(), ConfirmKind::Install));
        cx.notify();
    }

    /// Handle the commands the host can send (`InstallGrammar`); other variants are outputs.
    pub fn handle(&mut self, event: &SettingsEvent, cx: &mut Context<Self>) {
        if let SettingsEvent::InstallGrammar { id } = event {
            self.open_plugin(id, cx);
        }
    }

    /// A hot reload of `config.toml` (from `ConfigWatcher`). Merged three-way: what the user is
    /// editing wins, everything else follows the file. The echo of our own save changes nothing.
    pub fn set_config(&mut self, config: Config, cx: &mut Context<Self>) {
        let before = self.cfg.clone();
        let merged = merge_external(&self.base, &self.cfg, &config);
        let old_url = self.cfg.plugins.effective_index_url().to_string();
        self.base = config;
        self.cfg = merged;
        if self.cfg != self.base && !self.gate.is_pending() {
            // Edits that never reached the disk (a failed save): try again.
            self.touch_save(cx);
        }
        if self.cfg != before {
            self.sync_fields(&before, cx);
            self.sync_index_url(&old_url, cx);
            self.sync_layout_panel(cx);
            self.errors.clear();
            cx.notify();
        }
    }

    /// Write pending edits now, on the calling thread (closing the window).
    pub fn flush(&mut self) {
        if self.gate.force() {
            let result = self.cfg.save_to(&self.path).map_err(|e| e.to_string());
            self.gate.finish();
            match result {
                Ok(()) => {
                    self.base = self.cfg.clone();
                    self.save_error = None;
                }
                Err(e) => self.save_error = Some(e),
            }
        }
    }

    // ----- editing ----------------------------------------------------------------------------

    /// Apply `f` to a copy of the config; adopt it if it is valid (and different).
    pub(crate) fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Config)) -> Result<(), FieldError> {
        let mut candidate = self.cfg.clone();
        f(&mut candidate);
        if candidate == self.cfg {
            return Ok(());
        }
        if let Some(e) = first_error(&candidate) {
            return Err(e);
        }
        let old_url = self.cfg.plugins.effective_index_url().to_string();
        self.cfg = candidate;
        self.sync_index_url(&old_url, cx);
        self.sync_layout_panel(cx);
        cx.emit(SettingsEvent::ConfigChanged(Arc::new(self.cfg.clone())));
        self.touch_save(cx);
        cx.notify();
        Ok(())
    }

    /// [`edit`](Self::edit) for controls that cannot produce an invalid value.
    pub(crate) fn edit_cfg(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Config)) {
        if let Err(e) = self.edit(cx, f) {
            self.errors.insert(e.field, e.message);
            cx.notify();
        }
    }

    /// [`edit`](Self::edit) for a text field: the error (or its absence) is tied to `key`.
    pub(crate) fn edit_field(&mut self, key: &str, cx: &mut Context<Self>, f: impl FnOnce(&mut Config)) {
        match self.edit(cx, f) {
            Ok(()) => {
                self.errors.remove(key);
            }
            Err(e) => {
                self.errors.insert(key.to_string(), e.message);
            }
        }
        self.sync_invalid(cx);
        cx.notify();
    }

    /// The plugin manager follows `[plugins] index_url`.
    fn sync_index_url(&mut self, old: &str, cx: &mut Context<Self>) {
        let url = self.cfg.plugins.effective_index_url().to_string();
        if url != old {
            self.plugins.update(cx, |m, cx| m.set_index_url(url, cx));
            if self.section == Section::Plugins {
                self.plugins.update(cx, |m, cx| m.ensure_index(cx));
            }
        }
    }

    fn touch_save(&mut self, cx: &mut Context<Self>) {
        self.touched = true;
        let generation = self.gate.touch();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |s, cx| s.on_save_timer(generation, cx));
        }));
        cx.notify();
    }

    fn on_save_timer(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.gate.fire(generation) {
            self.start_write(cx);
        }
        cx.notify();
    }

    fn start_write(&mut self, cx: &mut Context<Self>) {
        let cfg = self.cfg.clone();
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let (c, p) = (cfg.clone(), path);
            let result = cx.background_executor().spawn(async move { c.save_to(&p).map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |s, cx| s.write_done(cfg, result, cx));
        })
        .detach();
        cx.notify();
    }

    fn write_done(&mut self, cfg: Config, result: Result<(), String>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                self.base = cfg;
                self.save_error = None;
                cx.emit(SettingsEvent::Saved);
            }
            Err(e) => {
                self.save_error = Some(e.clone());
                cx.emit(SettingsEvent::SaveFailed(e));
            }
        }
        if self.gate.finish() {
            self.start_write(cx);
        }
        cx.notify();
    }

    // ----- small accessors used by the renderers -----------------------------------------------

    pub(crate) fn save_error_text(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Footer text of the section list.
    pub(crate) fn save_label(&self) -> &'static str {
        if !self.touched {
            "Los cambios se guardan solos"
        } else if self.gate.in_flight() {
            "Guardando…"
        } else if self.gate.is_dirty() {
            "Cambios sin guardar"
        } else {
            "Guardado"
        }
    }

    pub(crate) fn field_subs_retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.field_subs.retain(|k, _| keep(k));
    }

    pub(crate) fn nav_open_flag(&self) -> bool {
        self.nav_open
    }

    pub(crate) fn toggle_nav(&mut self) {
        self.nav_open = !self.nav_open;
    }

    pub(crate) fn close_nav(&mut self) {
        self.nav_open = false;
    }

    pub(crate) fn focus_handle_ref(&self) -> FocusHandle {
        self.focus.clone()
    }

    // ----- text fields ------------------------------------------------------------------------

    /// The config-derived text of a plain field (`None` for profile fields).
    pub(crate) fn field_value(&self, key: &str) -> Option<String> {
        config_field_value(&self.cfg, key)
    }

    /// After an external config change: refresh the fields whose value moved, drop profile fields.
    fn sync_fields(&mut self, before: &Config, cx: &mut Context<Self>) {
        for key in ["ui_font", "mono_font", "accent_hex", "scrollback", "accept_word_key", "external_editor", "index_url"] {
            let (o, n) = (config_field_value(before, key), self.field_value(key));
            if o != n
                && let (Some(f), Some(v)) = (self.fields.get(key), n)
            {
                f.update(cx, |f, cx| f.set_text(&v, cx));
            }
        }
        if before.profiles != self.cfg.profiles {
            self.fields.retain(|k, _| !k.starts_with("profile."));
            self.field_subs.retain(|k, _| !k.starts_with("profile."));
            self.profile_editor = None;
        }
    }

    /// The text field `key`, created (with `initial`) on first use.
    pub(crate) fn field(
        &mut self,
        key: &str,
        initial: impl FnOnce(&Self) -> String,
        placeholder: &str,
        mono: bool,
        cx: &mut Context<Self>,
    ) -> Entity<TextField> {
        if let Some(f) = self.fields.get(key) {
            return f.clone();
        }
        let text = initial(self);
        let theme = self.theme;
        let placeholder = placeholder.to_string();
        let f = cx.new(|cx| TextField::new(theme, placeholder, cx).with_text(&text).monospace(mono));
        let this_key = key.to_string();
        let sub = cx.subscribe(&f, move |this, _, ev: &TextFieldEvent, cx| match ev {
            TextFieldEvent::Change => this.on_field_change(&this_key, cx),
            TextFieldEvent::Submit => {}
        });
        self.field_subs.insert(key.to_string(), sub);
        self.fields.insert(key.to_string(), f.clone());
        f
    }

    /// Mark fields with an error in red.
    pub(crate) fn sync_invalid(&mut self, cx: &mut Context<Self>) {
        for (k, f) in &self.fields {
            let bad = self.errors.contains_key(k);
            f.update(cx, |f, cx| f.set_invalid(bad, cx));
        }
    }

    /// Text of field `key` as typed.
    pub(crate) fn field_text(&self, key: &str, cx: &App) -> String {
        self.fields.get(key).map(|f| f.read(cx).text().to_string()).unwrap_or_default()
    }

    fn on_field_change(&mut self, key: &str, cx: &mut Context<Self>) {
        let text = self.field_text(key, cx);
        crate::fields::apply(self, key, &text, cx);
    }
}

/// Text of a plain field as derived from the config (see [`SettingsView::field_value`]).
pub(crate) fn config_field_value(cfg: &Config, key: &str) -> Option<String> {
    let a = &cfg.appearance;
    Some(match key {
        "ui_font" => a.ui_font.clone(),
        "mono_font" => a.mono_font.clone(),
        "accent_hex" => a.accent.clone(),
        "scrollback" => cfg.terminal.scrollback.to_string(),
        "accept_word_key" => cfg.terminal.completion.accept_word_key.clone(),
        "external_editor" => cfg.git.external_editor.clone(),
        "index_url" => cfg.plugins.index_url.clone(),
        _ => return None,
    })
}
