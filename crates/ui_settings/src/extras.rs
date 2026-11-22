//! Settings that live in `config.toml` but that `store::Config` does not model yet.
//!
//! `store::Config` has no `[general]`, `[plugins]` or `[git] external_editor` keys. Until the store
//! grows them (see the crate docs), this module reads and writes exactly those keys with
//! `toml_edit`, so comments and every other key survive. The key names are the ones the store
//! should adopt, so the day it does, `Config::save_to` and [`Extras::write`] agree on the file.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, value};

/// Language ids the UI can be translated to. Only Spanish exists today.
pub const LANGUAGES: &[(&str, &str)] = &[("es", "Español")];

/// Default plugin index (a placeholder until the first release exists).
pub const DEFAULT_INDEX_URL: &str =
    "https://github.com/OWNER/selachi/releases/latest/download/grammars.toml";

/// The keys outside `store::Config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extras {
    /// `[general] language`: UI language id (`es`).
    pub language: String,
    /// `[general] confirm_close_running`: ask before closing a window or tab that still has a
    /// command running.
    pub confirm_close_running: bool,
    /// `[git] external_editor`: command to open a file in the user's editor (empty = system default).
    pub external_editor: String,
    /// `[plugins] index_url`: where `grammars.toml` is downloaded from (empty = [`DEFAULT_INDEX_URL`]).
    pub index_url: String,
    /// Unified-sidebar options (docs/olas/ola-5.md 5.10) the store does not have yet.
    pub sidebar: SidebarPrefs,
}

/// Panels that can have a button in the titlebar (right side), in display order: `(id, label)`.
pub const TITLEBAR_PANELS: &[(&str, &str)] = &[
    ("git", "Git"),
    ("viewer", "Archivos"),
    ("procs", "Procesos"),
    ("usage", "Uso"),
    ("review", "Revisión"),
    ("attention", "Atención"),
    ("layout", "Layout"),
];

/// `[layout.sidebar]` / `[layout.titlebar]` options of the unified sidebar (5.10). They live in
/// the UI (and in `config.toml` under the key names the store will adopt) until
/// `store::config::LayoutConfig` models them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarPrefs {
    /// `layout.sidebar.show_actions`: "Nueva pestaña" and "Buscar" at the top.
    pub show_actions: bool,
    /// `layout.sidebar.panel_icons`: row of registered-panel icons.
    pub panel_icons: bool,
    /// `layout.sidebar.nested_tabs`: tabs / threads nested inside their space.
    pub nested_tabs: bool,
    /// `layout.sidebar.footer_usage`: compact usage meter and settings at the bottom.
    pub footer_usage: bool,
    /// `layout.sidebar.row_model`: show the model (and branch) on each row.
    pub row_model: bool,
    /// `layout.sidebar.row_time`: show the elapsed time on each row.
    pub row_time: bool,
    /// `layout.titlebar.panel_buttons`: ids of the panels with a button in the titlebar.
    pub titlebar_panels: Vec<String>,
}

impl Default for SidebarPrefs {
    fn default() -> Self {
        Self {
            show_actions: true,
            panel_icons: true,
            nested_tabs: true,
            footer_usage: true,
            row_model: true,
            row_time: true,
            titlebar_panels: TITLEBAR_PANELS.iter().map(|(id, _)| (*id).to_string()).collect(),
        }
    }
}

impl SidebarPrefs {
    pub fn titlebar_has(&self, id: &str) -> bool {
        self.titlebar_panels.iter().any(|p| p == id)
    }

    /// Turn the titlebar button of `id` on or off (keeps the canonical order).
    pub fn set_titlebar(&mut self, id: &str, on: bool) {
        self.titlebar_panels.retain(|p| p != id);
        if on {
            self.titlebar_panels.push(id.to_string());
        }
        let order = |p: &String| TITLEBAR_PANELS.iter().position(|(i, _)| *i == p).unwrap_or(usize::MAX);
        self.titlebar_panels.sort_by_key(order);
    }
}

impl Default for Extras {
    fn default() -> Self {
        Self {
            language: "es".to_string(),
            confirm_close_running: true,
            external_editor: String::new(),
            index_url: String::new(),
            sidebar: SidebarPrefs::default(),
        }
    }
}

impl Extras {
    /// The index URL in effect (`index_url` or the default).
    pub fn effective_index_url(&self) -> &str {
        let u = self.index_url.trim();
        if u.is_empty() { DEFAULT_INDEX_URL } else { u }
    }

    /// Problems that make these values unsavable, as `(config key, message)`.
    pub fn validate(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !LANGUAGES.iter().any(|(id, _)| *id == self.language) {
            out.push(("general.language", format!("Idioma desconocido: {}", self.language)));
        }
        let url = self.index_url.trim();
        if !url.is_empty() && !url.starts_with("https://") {
            out.push(("plugins.index_url", "La URL del índice debe empezar por https://".to_string()));
        }
        if self.external_editor.chars().any(char::is_control) {
            out.push(("git.external_editor", "El comando no puede tener saltos de línea".to_string()));
        }
        out
    }

    /// Parse the extras out of config text (missing or ill-typed keys keep their default).
    pub fn parse(text: &str) -> Extras {
        let mut e = Extras::default();
        let Ok(doc) = text.trim_start_matches('\u{feff}').parse::<DocumentMut>() else {
            return e;
        };
        let get = |table: &str, key: &str| -> Option<&Item> {
            let mut it = table.split('.');
            let mut cur = doc.get(it.next()?)?.as_table_like()?;
            for part in it {
                cur = cur.get(part)?.as_table_like()?;
            }
            cur.get(key)
        };
        if let Some(s) = get("general", "language").and_then(Item::as_str) {
            e.language = s.to_string();
        }
        if let Some(b) = get("general", "confirm_close_running").and_then(Item::as_bool) {
            e.confirm_close_running = b;
        }
        if let Some(s) = get("git", "external_editor").and_then(Item::as_str) {
            e.external_editor = s.to_string();
        }
        if let Some(s) = get("plugins", "index_url").and_then(Item::as_str) {
            e.index_url = s.to_string();
        }
        let sb = &mut e.sidebar;
        for (key, slot) in [
            ("show_actions", &mut sb.show_actions),
            ("panel_icons", &mut sb.panel_icons),
            ("nested_tabs", &mut sb.nested_tabs),
            ("footer_usage", &mut sb.footer_usage),
            ("row_model", &mut sb.row_model),
            ("row_time", &mut sb.row_time),
        ] {
            if let Some(b) = get("layout.sidebar", key).and_then(Item::as_bool) {
                *slot = b;
            }
        }
        if let Some(arr) = get("layout.titlebar", "panel_buttons").and_then(Item::as_array) {
            sb.titlebar_panels = arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        e
    }

    /// Read from the config file (defaults when it is missing or unreadable).
    pub fn read(path: &Path) -> Extras {
        std::fs::read_to_string(path).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    /// Merge into `text`: keys equal to their default are only written when the file already
    /// lists them, like `Config::save_to` does.
    pub fn apply_to(&self, text: &str) -> Result<String, String> {
        let mut doc: DocumentMut = text
            .trim_start_matches('\u{feff}')
            .parse()
            .map_err(|e: toml_edit::TomlError| e.to_string())?;
        let d = Extras::default();
        set(&mut doc, "general", "language", value(self.language.clone()), self.language == d.language);
        set(
            &mut doc,
            "general",
            "confirm_close_running",
            value(self.confirm_close_running),
            self.confirm_close_running == d.confirm_close_running,
        );
        set(
            &mut doc,
            "git",
            "external_editor",
            value(self.external_editor.clone()),
            self.external_editor == d.external_editor,
        );
        set(&mut doc, "plugins", "index_url", value(self.index_url.clone()), self.index_url == d.index_url);
        let (sb, db) = (&self.sidebar, &d.sidebar);
        for (key, v, dv) in [
            ("show_actions", sb.show_actions, db.show_actions),
            ("panel_icons", sb.panel_icons, db.panel_icons),
            ("nested_tabs", sb.nested_tabs, db.nested_tabs),
            ("footer_usage", sb.footer_usage, db.footer_usage),
            ("row_model", sb.row_model, db.row_model),
            ("row_time", sb.row_time, db.row_time),
        ] {
            set(&mut doc, "layout.sidebar", key, value(v), v == dv);
        }
        let mut arr = toml_edit::Array::new();
        for p in &sb.titlebar_panels {
            arr.push(p.as_str());
        }
        set(&mut doc, "layout.titlebar", "panel_buttons", value(arr), sb.titlebar_panels == db.titlebar_panels);
        Ok(doc.to_string())
    }

    /// Write into the config file atomically (temp file + rename). Refuses to touch a file that
    /// is not valid TOML.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let new = self.apply_to(&text)?;
        if new == text {
            return Ok(());
        }
        write_atomic(path, &new)
    }
}

fn set(doc: &mut DocumentMut, table: &str, key: &str, new: Item, is_default: bool) {
    let parts: Vec<&str> = table.split('.').collect();
    let present = {
        let mut cur: Option<&dyn toml_edit::TableLike> = Some(doc.as_table());
        for p in &parts {
            cur = cur.and_then(|t| t.get(p)).and_then(Item::as_table_like);
        }
        cur.is_some_and(|t| t.contains_key(key))
    };
    if !present && is_default {
        return;
    }
    let mut cur: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for p in &parts {
        if !cur.get(p).is_some_and(Item::is_table_like) {
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(p, Item::Table(t));
        }
        let Some(next) = cur.get_mut(p).and_then(Item::as_table_like_mut) else { return };
        cur = next;
    }
    match cur.get_mut(key) {
        Some(old) => {
            // Keep the user's inline comment around the value.
            let decor = old.as_value().map(|v| v.decor().clone());
            let same = match (old.as_value(), new.as_value()) {
                (Some(a), Some(b)) => a.to_string().trim() == b.to_string().trim(),
                _ => false,
            };
            if !same {
                *old = new;
                if let (Some(v), Some(decor)) = (old.as_value_mut(), decor) {
                    *v.decor_mut() = decor;
                }
            }
        }
        None => {
            cur.insert(key, new);
        }
    }
}

/// Write `contents` to `path`: temp file in the same directory, fsync, rename.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".settings{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_not_written_into_a_file_that_lacks_them() {
        let text = "# hi\n[appearance]\nmode = \"dark\"\n";
        let out = Extras::default().apply_to(text).unwrap();
        assert_eq!(out, text);
    }

    #[test]
    fn non_default_values_roundtrip_and_keep_comments() {
        let text =
            "# header\n[git]\nconfirm_sensitive = true # keep\n[general]\nlanguage = \"es\" # idioma\n";
        let e = Extras {
            confirm_close_running: false,
            external_editor: "code -g".into(),
            index_url: "https://example.com/g.toml".into(),
            ..Extras::default()
        };
        let out = e.apply_to(text).unwrap();
        assert!(out.contains("# header"));
        assert!(out.contains("confirm_sensitive = true # keep"));
        assert!(out.contains("language = \"es\" # idioma"));
        assert_eq!(Extras::parse(&out), e);
        // Back to defaults: keys the file lists are rewritten, others stay absent.
        let back = Extras::default().apply_to(&out).unwrap();
        assert_eq!(Extras::parse(&back), Extras::default());
    }

    #[test]
    fn sidebar_prefs_roundtrip_under_their_future_store_keys() {
        let mut e = Extras::default();
        e.sidebar.show_actions = false;
        e.sidebar.row_time = false;
        e.sidebar.set_titlebar("usage", false);
        let out = e.apply_to("# c\n[layout]\npreset = \"chat\"\n").unwrap();
        assert!(out.contains("# c") && out.contains("preset = \"chat\""));
        assert!(out.contains("show_actions = false"), "{out}");
        assert!(!out.contains("panel_icons"), "defaults are not written: {out}");
        let back = Extras::parse(&out);
        assert_eq!(back.sidebar, e.sidebar);
        assert!(!back.sidebar.titlebar_has("usage") && back.sidebar.titlebar_has("git"));
        // Back to defaults: only keys already in the file are rewritten.
        let reset = Extras::default().apply_to(&out).unwrap();
        assert_eq!(Extras::parse(&reset).sidebar, SidebarPrefs::default());
        // Titlebar order stays canonical.
        let mut p = SidebarPrefs::default();
        p.set_titlebar("git", false);
        p.set_titlebar("git", true);
        assert_eq!(p.titlebar_panels, SidebarPrefs::default().titlebar_panels);
    }

    #[test]
    fn validation() {
        let mut e = Extras::default();
        assert!(e.validate().is_empty());
        e.index_url = "http://insecure/g.toml".into();
        assert_eq!(e.validate()[0].0, "plugins.index_url");
        e.index_url = "  ".into();
        assert_eq!(e.effective_index_url(), DEFAULT_INDEX_URL);
        e.language = "xx".into();
        assert_eq!(e.validate()[0].0, "general.language");
    }

    #[test]
    fn invalid_toml_is_never_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[broken\n").unwrap();
        let e = Extras { confirm_close_running: false, ..Extras::default() };
        assert!(e.write(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[broken\n");
    }
}
