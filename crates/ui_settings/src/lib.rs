//! `ui_settings`: Selachi's settings screen and the grammar plugin manager.
//!
//! * [`SettingsView`]: a full Main-zone view. A section list on the left (General, Apariencia,
//!   Terminal, Agentes, Git, Procesos, Plugins, Teclado, Acerca de) and a content column of at most
//!   860 px; below 720 px the list folds into a menu at the top. Every control edits the working
//!   `store::Config`, is validated with `Config::validate()` (inline errors, invalid values are
//!   never applied), applied live ([`SettingsEvent::ConfigChanged`]) and saved to `config.toml`
//!   with `toml_edit` (comments and unknown keys preserved) after 400 ms of quiet, one write in
//!   flight at a time. Hot reloads are merged three-way ([`SettingsView::set_config`]).
//! * [`PluginsModel`]: language grammars. The index (`grammars.toml`) is downloaded over HTTPS
//!   only when Plugins is opened (cached for one hour), 1 MB cap; archives are capped at 20 MB and
//!   installed with `GrammarRegistry::install_from_archive` (sha256 pinned in the index) only
//!   after the user confirms. `file://` exists only in `LocalFetcher` (tests, `dev-file-urls`).
//! * [`SettingsPanelProvider`] and [`thumbnail`]: the `panel_api` / layout-panel hooks.
//!
//! ```ignore
//! ui_settings::init(cx); // once: key bindings (Tab, text field)
//! let opts = ui_settings::SettingsOptions::new(store::paths::config_path()?, env!("CARGO_PKG_VERSION"));
//! let settings = ui_settings::settings_view(config, theme, opts, cx);
//! cx.subscribe(&settings, |_, ev: &ui_settings::SettingsEvent, cx| match ev {
//!     SettingsEvent::ConfigChanged(cfg) => { /* apply theme, accent, fonts, layout live */ }
//!     SettingsEvent::OpenLayoutPanel => { /* open the layout popover */ }
//!     SettingsEvent::RerunOnboarding => { /* show the wizard */ }
//!     _ => {}
//! }).detach();
//! // hot reload:                settings.update(cx, |s, cx| s.set_config(new_config, cx));
//! // viewer banner deep link:   settings.update(cx, |s, cx| s.open_plugin("csharp", cx));
//! // window closing:            settings.update(cx, |s, _| s.flush());
//! ```
//!
//! Settings that `store::Config` does not model yet (`[general] language`,
//! `[general] confirm_close_running`, `[git] external_editor`, `[plugins] index_url`) are read and
//! written by [`Extras`]; see its docs and the wave report for the store change that retires it.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod agents;
mod agents_ui;
mod extras;
mod fields;
mod fmt;
mod keyboard_ui;
mod keys;
mod net;
mod panel;
mod plugins;
mod plugins_ui;
mod render;
mod save;
mod sections;
mod terminal_ui;
mod text_field;
mod ui;
mod validate;
mod view;

pub use agents::{AgentRow, AgentStatus, AgentsState};
pub use extras::{DEFAULT_INDEX_URL, Extras, LANGUAGES};
pub use keys::{
    ChordError, KeyAction, chord_from_keystroke, default_catalog, display_chord, effective, find_conflict, normalize_chord,
    set_binding, swap_bindings,
};
#[cfg(any(test, feature = "dev-file-urls"))]
pub use net::LocalFetcher;
pub use net::{
    ARCHIVE_TIMEOUT, FetchError, Fetcher, HttpFetcher, INDEX_TIMEOUT, MAX_ARCHIVE_BYTES, MAX_INDEX_BYTES, check_https,
};
pub use panel::{PANEL_ID, SettingsPanelProvider, thumbnail};
pub use plugins::{
    Group, INDEX_TTL, IndexState, Op, PluginRow, PluginState, PluginsEvent, PluginsModel, RestartKind, build_rows,
    download_and_install, fetch_index,
};
pub use save::{SAVE_DEBOUNCE, SaveGate, merge_external};
pub use validate::{FieldError, field_errors, first_error, join_args, parse_count, parse_number, split_args};
pub use view::{Section, SettingsEvent, SettingsOptions, SettingsView, init, settings_view};

#[cfg(test)]
mod tests;
