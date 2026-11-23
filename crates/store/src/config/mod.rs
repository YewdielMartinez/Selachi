//! `config.toml`: typed model, loading with defaults, comment-preserving save and
//! hot reload ([`ConfigWatcher`]).

mod layout;
mod model;
mod watcher;

pub use layout::*;
pub use model::*;
pub use watcher::{ConfigEvent, ConfigWatcher};

use crate::error::{Result, StoreError};
use crate::paths;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};

/// The commented template written on first run. Its values equal [`Config::default`].
pub const TEMPLATE: &str = include_str!("template.toml");

/// A successfully parsed config plus non-fatal findings.
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    pub config: Config,
    /// Dotted paths of keys Selachi does not know (typos or newer-version keys).
    pub warnings: Vec<String>,
}

/// Outcome of [`Config::load_or_default`]. Never fails: on any problem the
/// defaults are used and `error` says why.
#[derive(Debug)]
pub struct ConfigLoad {
    pub config: Config,
    pub path: PathBuf,
    pub warnings: Vec<String>,
    /// Set when the file exists but could not be read or is invalid (it is left untouched).
    pub error: Option<StoreError>,
    /// The file did not exist and the commented template was just written.
    pub created: bool,
}

impl Config {
    /// Parse and validate config text.
    pub fn parse(text: &str) -> Result<ParsedConfig> {
        let text = text.trim_start_matches('\u{feff}');
        let config: Config =
            toml::from_str(text).map_err(|e| StoreError::ConfigParse(e.to_string()))?;
        let problems = config.validate();
        if !problems.is_empty() {
            return Err(StoreError::ConfigParse(problems.join("; ")));
        }
        let mut warnings = unknown_keys(text);
        // Layout problems are corrected, never fatal: report them as warnings.
        warnings.extend(config.layout_warnings());
        Ok(ParsedConfig { config, warnings })
    }

    /// Read, parse and validate the file at `path` (must exist).
    pub fn load_from(path: &Path) -> Result<ParsedConfig> {
        let text = std::fs::read_to_string(path).map_err(|e| StoreError::io(path, e))?;
        Self::parse(&text)
    }

    /// Load `%APPDATA%\Selachi\config.toml`, writing the commented template on first run.
    pub fn load_or_default() -> ConfigLoad {
        match paths::config_path() {
            Ok(p) => Self::load_or_default_at(&p),
            Err(e) => ConfigLoad {
                config: Config::default(),
                path: PathBuf::new(),
                warnings: Vec::new(),
                error: Some(e),
                created: false,
            },
        }
    }

    /// Like [`Config::load_or_default`] for an explicit path.
    pub fn load_or_default_at(path: &Path) -> ConfigLoad {
        let mut out = ConfigLoad {
            config: Config::default(),
            path: path.to_path_buf(),
            warnings: Vec::new(),
            error: None,
            created: false,
        };
        if !path.exists() {
            match write_atomic(path, TEMPLATE) {
                Ok(()) => out.created = true,
                Err(e) => out.error = Some(e),
            }
            // A new install starts from the template (its preset is `espacios_unificados`),
            // not from the serde defaults that existing files without the key keep.
            if let Ok(p) = Self::parse(TEMPLATE) {
                out.config = p.config;
                out.warnings = p.warnings;
            }
            return out;
        }
        match Self::load_from(path) {
            Ok(p) => {
                out.config = p.config;
                out.warnings = p.warnings;
            }
            Err(e) => out.error = Some(e),
        }
        out
    }

    /// Save to the default location. See [`Config::save_to`].
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::config_path()?)
    }

    /// Write this config to `path`, editing the existing file in place so that
    /// comments, key order and unknown keys survive. Values equal to the default
    /// are not added to the file unless it already lists them. The write is
    /// atomic (temp file + rename). Refuses to overwrite a file that is not
    /// valid TOML, so a hand-edit in progress is never clobbered.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let existing = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_string(),
            Err(e) => return Err(StoreError::io(path, e)),
        };
        let mut doc: DocumentMut = existing
            .trim_start_matches('\u{feff}')
            .parse()
            .map_err(|e: toml_edit::TomlError| StoreError::ConfigParse(e.to_string()))?;
        let mut new = toml_edit::ser::to_document(self)
            .map_err(|e| StoreError::ConfigSerialize(e.to_string()))?;
        let mut defaults = toml_edit::ser::to_document(&Config::default())
            .map_err(|e| StoreError::ConfigSerialize(e.to_string()))?;
        expand_inline(new.as_table_mut());
        expand_inline(defaults.as_table_mut());
        merge_table(
            doc.as_table_mut(),
            new.as_table(),
            Some(defaults.as_table()),
            "",
        );
        write_atomic(path, &doc.to_string())
    }
}

/// The serializer emits inline tables (`a = { b = 1 }`); turn them into regular
/// `[a]` tables (and arrays of inline tables into `[[a]]`) so they line up with
/// how a hand-written file is laid out.
fn expand_inline(table: &mut Table) {
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for key in keys {
        let Some(item) = table.get_mut(&key) else {
            continue;
        };
        let taken = std::mem::take(item);
        *item = match taken {
            Item::Value(toml_edit::Value::InlineTable(t)) => {
                let mut t = t.into_table();
                expand_inline(&mut t);
                Item::Table(t)
            }
            Item::Value(toml_edit::Value::Array(arr))
                if !arr.is_empty() && arr.iter().all(|v| v.is_inline_table()) =>
            {
                let mut aot = toml_edit::ArrayOfTables::new();
                for v in arr.into_iter() {
                    if let toml_edit::Value::InlineTable(t) = v {
                        let mut t = t.into_table();
                        expand_inline(&mut t);
                        aot.push(t);
                    }
                }
                Item::ArrayOfTables(aot)
            }
            other => other,
        };
    }
}

/// Tables whose keys are user-defined: stale keys are removed on save.
const OPEN_MAPS: &[&str] = &[
    "keybindings",
    "layout.overrides",
    "layout.overrides.sidebar",
    "layout.overrides.titlebar",
];

fn merge_table(old: &mut Table, new: &Table, defaults: Option<&Table>, path: &str) {
    for (key, new_item) in new.iter() {
        let child_path = if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        };
        let def_item = defaults.and_then(|d| d.get(key));
        // A list of `[[tables]]` that became empty disappears from the file
        // instead of leaving a dangling `key = []`.
        let emptied = matches!(new_item, Item::Value(toml_edit::Value::Array(a)) if a.is_empty())
            && matches!(old.get(key), Some(Item::ArrayOfTables(_)));
        if emptied {
            old.remove(key);
            continue;
        }
        match old.get_mut(key) {
            Some(old_item) => merge_item(old_item, new_item, def_item, &child_path),
            None => {
                let is_default = def_item.is_some_and(|d| same_content(d, new_item));
                if !is_default {
                    old.insert(key, new_item.clone());
                }
            }
        }
    }
    if OPEN_MAPS.contains(&path) {
        let stale: Vec<String> = old
            .iter()
            .filter(|(k, _)| !new.contains_key(k))
            .map(|(k, _)| k.to_string())
            .collect();
        for k in stale {
            old.remove(&k);
        }
    }
}

fn merge_item(old: &mut Item, new: &Item, def: Option<&Item>, path: &str) {
    if let (Item::Table(ot), Item::Table(nt)) = (&mut *old, new) {
        merge_table(ot, nt, def.and_then(|d| d.as_table()), path);
        return;
    }
    if same_content(old, new) {
        return;
    }
    if let (Item::Value(ov), Item::Value(nv)) = (&*old, new) {
        // Keep the user's inline comment/whitespace around the value.
        let decor = ov.decor().clone();
        let mut replacement = nv.clone();
        *replacement.decor_mut() = decor;
        *old = Item::Value(replacement);
    } else {
        *old = new.clone();
    }
}

/// Compare two items ignoring whitespace/comments, by round-tripping through `toml::Value`.
fn same_content(a: &Item, b: &Item) -> bool {
    match (item_value(a), item_value(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn item_value(item: &Item) -> Option<toml::Value> {
    let mut root = Table::new();
    root.insert("k", item.clone());
    let text = DocumentMut::from(root).to_string();
    let mut value: toml::Value = toml::from_str(&text).ok()?;
    value.as_table_mut()?.remove("k")
}

/// Keys present in `text` that the schema does not define.
fn unknown_keys(text: &str) -> Vec<String> {
    let Ok(actual) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    let Ok(schema) = toml::Value::try_from(Config::default()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_unknown(&actual, &schema, "", &mut out);
    out
}

fn walk_unknown(actual: &toml::Value, schema: &toml::Value, path: &str, out: &mut Vec<String>) {
    let (toml::Value::Table(a), toml::Value::Table(s)) = (actual, schema) else {
        return;
    };
    if s.is_empty() {
        return; // open map (keybindings)
    }
    for (k, v) in a {
        let p = if path.is_empty() {
            k.clone()
        } else {
            format!("{path}.{k}")
        };
        match s.get(k) {
            None => out.push(p),
            Some(sv) => walk_unknown(v, sv, &p, out),
        }
    }
}

/// Write `contents` to `path` atomically: temp file in the same directory, fsync, rename.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        }
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(StoreError::io(path, e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_equals_defaults_except_the_new_install_preset() {
        let parsed = Config::parse(TEMPLATE).unwrap();
        let mut expected = Config::default();
        expected.layout.preset = PresetId::Builtin(BuiltinPreset::EspaciosUnificados);
        assert_eq!(parsed.config, expected);
        // Existing files without the key keep the classic preset.
        assert_eq!(
            Config::parse("[layout]").unwrap().config.layout.preset,
            PresetId::Builtin(BuiltinPreset::Terminal)
        );
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn completion_defaults_and_env() {
        let c = Config::default();
        assert_eq!(c.terminal.completion.tab, TabMode::Smart);
        assert_eq!(
            c.completion_env(),
            vec![
                ("SELACHI_TAB_MODE".to_string(), "smart".to_string()),
                ("SELACHI_PREDICTION".to_string(), "history".to_string()),
                ("SELACHI_PREDICTION_VIEW".to_string(), "inline".to_string()),
                ("SELACHI_ACCEPT_WORD_KEY".to_string(), "Ctrl+RightArrow".to_string()),
            ]
        );
        let text = "[terminal.completion]\ntab = \"menu\"\nprediction = \"history_and_plugin\"\nprediction_view = \"list\"\naccept_word_key = \" Alt+j \"\n";
        let p = Config::parse(text).unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let env: std::collections::BTreeMap<_, _> = p.config.completion_env().into_iter().collect();
        assert_eq!(env["SELACHI_TAB_MODE"], "menu");
        assert_eq!(env["SELACHI_PREDICTION"], "history_and_plugin");
        assert_eq!(env["SELACHI_PREDICTION_VIEW"], "list");
        assert_eq!(env["SELACHI_ACCEPT_WORD_KEY"], "Alt+j");
        // Empty key = default; native values are passed through.
        let p = Config::parse("[terminal.completion]\ntab = \"native\"\nprediction = \"native\"\nprediction_view = \"native\"\naccept_word_key = \"\"\n").unwrap();
        let env: std::collections::BTreeMap<_, _> = p.config.completion_env().into_iter().collect();
        assert_eq!(env["SELACHI_TAB_MODE"], "native");
        assert_eq!(env["SELACHI_PREDICTION"], "native");
        assert_eq!(env["SELACHI_PREDICTION_VIEW"], "native");
        assert_eq!(env["SELACHI_ACCEPT_WORD_KEY"], "Ctrl+RightArrow");
        // Bad values are rejected.
        assert!(Config::parse("[terminal.completion]\ntab = \"tabby\"").is_err());
        assert!(Config::parse("[terminal.completion]\nprediction = \"magic\"").is_err());
        assert!(Config::parse("[terminal.completion]\naccept_word_key = \"a'b\"").is_err());
        assert!(Config::parse("[terminal.completion]\nsparkle = 1").unwrap().warnings.contains(&"terminal.completion.sparkle".to_string()));
    }

    #[test]
    fn template_documents_every_key() {
        let schema = toml::Value::try_from(Config::default()).unwrap();
        fn leaves(v: &toml::Value, out: &mut Vec<String>) {
            if let toml::Value::Table(t) = v {
                for (k, v) in t {
                    if matches!(v, toml::Value::Table(_)) {
                        leaves(v, out);
                    } else {
                        out.push(k.clone());
                    }
                }
            }
        }
        let mut keys = Vec::new();
        leaves(&schema, &mut keys);
        for k in keys {
            if matches!(
                k.as_str(),
                "profiles" | "custom_presets" | "project_overrides"
            ) {
                continue;
            }
            assert!(TEMPLATE.contains(&format!("{k} =")), "template misses {k}");
        }
    }

    #[test]
    fn defaults_roundtrip_through_toml() {
        let c = Config::default();
        let text = toml::to_string(&c).unwrap();
        assert_eq!(Config::parse(&text).unwrap().config, c);
    }

    #[test]
    fn empty_file_is_default() {
        assert_eq!(Config::parse("").unwrap().config, Config::default());
        assert_eq!(Config::parse("\u{feff}").unwrap().config, Config::default());
    }

    #[test]
    fn full_roundtrip_with_values() {
        let mut c = Config::default();
        c.appearance.mode = ThemeMode::Oled;
        c.appearance.accent = "#112233".into();
        c.terminal.cursor_style = CursorStyle::Bar;
        c.agents.permission_default = PermissionDefault::AcceptEdits;
        c.processes.auto_stop_on_thread_close = AutoStop::Never;
        c.keybindings
            .insert("terminal.new_tab".into(), "ctrl+t".into());
        c.profiles.push(Profile {
            name: "Dev".into(),
            program: "pwsh.exe".into(),
            args: vec!["-NoLogo".into()],
            cwd: Some("C:\\code".into()),
            env: [("A".to_string(), "1".to_string())].into(),
        });
        let text = toml::to_string(&c).unwrap();
        assert_eq!(Config::parse(&text).unwrap().config, c);
    }

    #[test]
    fn unknown_keys_tolerated_and_reported() {
        let text = "future = 1\n[appearance]\nmode = \"light\"\nsparkle = true\n[keybindings]\nanything = \"x\"\n";
        let p = Config::parse(text).unwrap();
        assert_eq!(p.config.appearance.mode, ThemeMode::Light);
        assert_eq!(
            p.warnings,
            vec!["appearance.sparkle".to_string(), "future".to_string()]
        );
    }

    #[test]
    fn invalid_values_are_errors() {
        assert!(Config::parse("[appearance]\naccent = \"blue\"").is_err());
        assert!(Config::parse("[appearance]\nmode = \"neon\"").is_err());
        assert!(Config::parse("[appearance\nmode").is_err());
        assert!(Config::parse("[appearance]\nmono_size = 0.5").is_err());
    }

    #[test]
    fn integer_accepted_for_float() {
        let p = Config::parse("[appearance]\nmono_size = 14").unwrap();
        assert_eq!(p.config.appearance.mono_size, 14.0);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#00AAFF"), Some((0, 170, 255)));
        assert_eq!(parse_hex_color("0af"), Some((0, 170, 255)));
        assert_eq!(parse_hex_color("#12"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
    }

    #[test]
    fn first_run_writes_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let load = Config::load_or_default_at(&path);
        assert!(load.created);
        assert!(load.error.is_none());
        assert_eq!(
            load.config.layout.preset,
            PresetId::Builtin(BuiltinPreset::EspaciosUnificados)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);
        let again = Config::load_or_default_at(&path);
        assert!(!again.created);
    }

    #[test]
    fn invalid_file_gives_defaults_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[appearance\nbroken").unwrap();
        let load = Config::load_or_default_at(&path);
        assert!(load.error.is_some());
        assert_eq!(load.config, Config::default());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[appearance\nbroken"
        );
        // And save refuses to clobber it.
        assert!(Config::default().save_to(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[appearance\nbroken"
        );
    }

    #[test]
    fn save_preserves_comments_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = format!(
            "# my header comment\nfuture_key = \"keep me\"\n{}\n[extra]\nx = 1 # trailing\n",
            TEMPLATE.replace(
                "accent = \"#00AAFF\"",
                "accent = \"#00AAFF\" # my favourite"
            )
        );
        std::fs::write(&path, &text).unwrap();

        let mut cfg = Config::load_from(&path).unwrap().config;
        cfg.appearance.accent = "#FF00AA".into();
        cfg.terminal.scrollback = 20_000;
        cfg.keybindings.insert("a.b".into(), "ctrl+k".into());
        cfg.save_to(&path).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# my header comment"));
        assert!(saved.contains("future_key = \"keep me\""));
        assert!(saved.contains("x = 1 # trailing"));
        assert!(saved.contains("# Brand accent, #RRGGBB."), "{saved}");
        assert!(
            saved.contains("accent = \"#FF00AA\" # my favourite"),
            "{saved}"
        );
        assert!(saved.contains("scrollback = 20000"));
        assert!(
            saved.contains("\"a.b\" = \"ctrl+k\"")
                || saved.contains("a.b = \"ctrl+k\"")
                || saved.contains("[keybindings]")
        );
        assert_eq!(Config::parse(&saved).unwrap().config, cfg);

        // Removing a keybinding removes it from the file; saving defaults is stable.
        cfg.keybindings.clear();
        cfg.save_to(&path).unwrap();
        let saved2 = std::fs::read_to_string(&path).unwrap();
        assert!(!saved2.contains("ctrl+k"));
        assert_eq!(Config::parse(&saved2).unwrap().config, cfg);
    }

    #[test]
    fn save_to_missing_file_creates_it_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.git.confirm_sensitive = false;
        cfg.save_to(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("confirm_sensitive = false"));
        assert!(saved.contains("# Selachi configuration"));
        assert_eq!(Config::parse(&saved).unwrap().config, cfg);
    }

    #[test]
    fn save_profiles_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.profiles.push(Profile {
            name: "WSL".into(),
            program: "wsl.exe".into(),
            ..Profile::default()
        });
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap().config, cfg);
        cfg.profiles.clear();
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap().config, cfg);
    }
}
