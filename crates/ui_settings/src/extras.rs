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
}

impl Default for Extras {
    fn default() -> Self {
        Self {
            language: "es".to_string(),
            confirm_close_running: true,
            external_editor: String::new(),
            index_url: String::new(),
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
        let get = |table: &str, key: &str| {
            doc.get(table).and_then(Item::as_table_like).and_then(|t| t.get(key))
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
    let present =
        doc.get(table).and_then(Item::as_table_like).is_some_and(|t| t.contains_key(key));
    if !present && is_default {
        return;
    }
    let root = doc.as_table_mut();
    if !root.get(table).is_some_and(Item::is_table_like) {
        let mut t = Table::new();
        t.set_implicit(false);
        root.insert(table, Item::Table(t));
    }
    let Some(t) = root.get_mut(table).and_then(Item::as_table_like_mut) else { return };
    match t.get_mut(key) {
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
            t.insert(key, new);
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
