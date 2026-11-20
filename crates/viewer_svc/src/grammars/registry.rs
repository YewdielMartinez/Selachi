//! The grammar registry: discovery, lazy loading, install and uninstall.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use tree_sitter::Language;
use tree_sitter_highlight::HighlightConfiguration;

use super::error::GrammarError;
use super::index::{verify_archive, IndexEntry};
use super::loader::{check_abi, load_plugin, verify_files, Loaded};
use super::manifest::{valid_id, Manifest, MANIFEST_FILE};
use crate::detect::Lang;

/// Marker file written inside a plugin folder that must be deleted at next startup.
const PENDING_REMOVAL: &str = ".pending_removal";
/// Folder suffix of an update waiting for a restart (`csharp.new`).
const NEW_SUFFIX: &str = ".new";
/// Prefix of temporary install folders.
const STAGING_PREFIX: &str = ".staging-";
/// Archives with more entries are rejected.
const MAX_ARCHIVE_ENTRIES: usize = 64;

/// Where a grammar comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrammarSource {
    /// Compiled into the binary (cargo `lang-*` feature).
    Builtin,
    /// Installed plugin folder.
    Plugin,
}

/// Life-cycle state of a grammar, for the Settings -> Plugins list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrammarState {
    /// Compiled in, always ready.
    Builtin,
    /// Installed and verified at discovery time only by manifest; not loaded yet.
    Installed,
    /// Loaded in this process.
    Loaded,
    /// Loading failed (hash mismatch, bad ABI...). The file shows without colour.
    Failed(GrammarError),
}

/// One row of [`GrammarRegistry::available`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarInfo {
    pub id: String,
    pub display_name: String,
    /// Plugin version (`None` for built-ins).
    pub version: Option<String>,
    pub file_extensions: Vec<String>,
    pub source: GrammarSource,
    pub state: GrammarState,
    /// Installed size in bytes as declared by the manifest (0 for built-ins).
    pub size_bytes: u64,
    /// Uninstall requested but the DLL is mapped: it goes away on next start.
    pub pending_removal: bool,
    /// A newer version is staged and replaces this one on next start.
    pub pending_update: bool,
}

/// A resolved grammar.
#[derive(Debug, Clone)]
pub struct Grammar {
    pub id: String,
    pub display_name: String,
    pub source: GrammarSource,
    pub version: Option<String>,
    language: Option<Language>,
}

impl Grammar {
    /// The tree-sitter language of a loaded plugin (`None` for built-ins).
    pub fn language(&self) -> Option<&Language> {
        self.language.as_ref()
    }
}

/// Result of an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Usable immediately.
    Installed { id: String, version: String },
    /// The previous version is loaded in this process (its DLL cannot be replaced while
    /// mapped): the new one is staged and takes over on next start.
    PendingRestart { id: String, version: String },
}

/// Result of an uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallOutcome {
    Removed,
    /// The grammar is loaded this session; it is deleted on next start.
    PendingRestart,
}

enum Slot {
    Idle,
    Loaded(&'static Loaded),
    Failed(GrammarError),
}

struct Plugin {
    manifest: Manifest,
    dir: PathBuf,
    slot: Slot,
    pending_removal: bool,
    pending_update: bool,
}

#[derive(Default)]
struct Inner {
    plugins: BTreeMap<String, Plugin>,
    problems: Vec<(PathBuf, GrammarError)>,
}

/// Discovers and loads grammar plugins from one folder.
pub struct GrammarRegistry {
    dir: PathBuf,
    inner: RwLock<Inner>,
}

impl std::fmt::Debug for GrammarRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrammarRegistry")
            .field("dir", &self.dir)
            .finish()
    }
}

/// Temporary folder removed on drop unless kept.
struct Staging {
    path: PathBuf,
    keep: bool,
}

impl Staging {
    fn create(root: &Path, id: &str) -> Result<Staging, GrammarError> {
        std::fs::create_dir_all(root).map_err(|e| GrammarError::io(root, &e))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = root.join(format!(
            "{STAGING_PREFIX}{id}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&path).map_err(|e| GrammarError::io(&path, &e))?;
        Ok(Staging { path, keep: false })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn lower_ext(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_ascii_lowercase()
}

impl GrammarRegistry {
    /// Open the registry on `plugins_dir` (created lazily on first install). Applies
    /// work deferred from the previous run (removals, staged updates) and scans the
    /// folder; nothing is loaded yet.
    pub fn new(plugins_dir: impl Into<PathBuf>) -> GrammarRegistry {
        let dir = plugins_dir.into();
        apply_pending(&dir);
        let inner = scan(&dir);
        GrammarRegistry {
            dir,
            inner: RwLock::new(inner),
        }
    }

    /// The plugins folder.
    pub fn plugins_dir(&self) -> &Path {
        &self.dir
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-scan the folder (after files were added by hand). Loaded plugins stay
    /// loaded; failed ones get another chance.
    pub fn rescan(&self) {
        let mut fresh = scan(&self.dir);
        let mut g = self.write();
        for (id, plugin) in &mut fresh.plugins {
            if let Some(old) = g.plugins.remove(id) {
                if matches!(old.slot, Slot::Loaded(_)) {
                    plugin.slot = old.slot;
                    plugin.manifest = old.manifest;
                }
                plugin.pending_removal = old.pending_removal;
                plugin.pending_update = old.pending_update;
            }
        }
        *g = fresh;
    }

    /// Manifests or folders that could not be read at the last scan.
    pub fn problems(&self) -> Vec<(PathBuf, GrammarError)> {
        self.read().problems.clone()
    }

    // ---- listing -----------------------------------------------------------------

    /// Built-in grammars followed by installed plugins (sorted by id).
    pub fn available(&self) -> Vec<GrammarInfo> {
        let mut out: Vec<GrammarInfo> = Lang::ALL
            .into_iter()
            .filter(|l| crate::highlight::builtin_available(*l))
            .map(|l| GrammarInfo {
                id: l.id().to_owned(),
                display_name: l.name().to_owned(),
                version: None,
                file_extensions: l.extensions().iter().map(|e| (*e).to_owned()).collect(),
                source: GrammarSource::Builtin,
                state: GrammarState::Builtin,
                size_bytes: 0,
                pending_removal: false,
                pending_update: false,
            })
            .collect();
        let g = self.read();
        for (id, p) in &g.plugins {
            out.push(GrammarInfo {
                id: id.clone(),
                display_name: p.manifest.display_name.clone(),
                version: Some(p.manifest.version.clone()),
                file_extensions: p.manifest.file_extensions.clone(),
                source: GrammarSource::Plugin,
                state: match &p.slot {
                    Slot::Idle => GrammarState::Installed,
                    Slot::Loaded(_) => GrammarState::Loaded,
                    Slot::Failed(e) => GrammarState::Failed(e.clone()),
                },
                size_bytes: p.manifest.size_bytes,
                pending_removal: p.pending_removal,
                pending_update: p.pending_update,
            });
        }
        out
    }

    /// True when a plugin with this id is installed and usable (not failed, not
    /// scheduled for removal without being loaded).
    pub fn has_plugin(&self, id: &str) -> bool {
        self.read().plugins.get(id).is_some_and(usable)
    }

    /// The load error of a plugin, if its last load failed.
    pub fn failure(&self, id: &str) -> Option<GrammarError> {
        match self.read().plugins.get(id).map(|p| &p.slot) {
            Some(Slot::Failed(e)) => Some(e.clone()),
            _ => None,
        }
    }

    /// The load error of the plugin that declares file extension `ext`, if it failed.
    pub fn failure_for_extension(&self, ext: &str) -> Option<GrammarError> {
        let ext = lower_ext(ext);
        let g = self.read();
        g.plugins.values().find_map(|p| match &p.slot {
            Slot::Failed(e) if p.manifest.file_extensions.contains(&ext) => Some(e.clone()),
            _ => None,
        })
    }

    // ---- detection ---------------------------------------------------------------

    /// Id of the grammar (built-in first, then plugin) for a file extension. Built-in
    /// detection comes from [`Lang::from_extension`]; plugins also contribute the
    /// extensions their manifest declares (`.cs` -> `csharp`, `.go` -> `go`...).
    pub fn id_for_extension(&self, ext: &str) -> Option<String> {
        let ext = lower_ext(ext);
        if ext.is_empty() {
            return None;
        }
        let g = self.read();
        if let Some(lang) = Lang::from_extension(&ext) {
            if crate::highlight::builtin_available(lang)
                || g.plugins.get(lang.id()).is_some_and(usable)
            {
                return Some(lang.id().to_owned());
            }
        }
        g.plugins
            .iter()
            .find(|(_, p)| usable(p) && p.manifest.file_extensions.contains(&ext))
            .map(|(id, _)| id.clone())
    }

    /// Id for a fenced-code / injection name (`rust`, `c#`, `go`...).
    pub(crate) fn plugin_id_for_name(&self, name: &str) -> Option<String> {
        let name = name.trim().to_ascii_lowercase();
        let g = self.read();
        if let Some(lang) = Lang::from_name(&name) {
            if g.plugins.get(lang.id()).is_some_and(usable) {
                return Some(lang.id().to_owned());
            }
        }
        g.plugins
            .iter()
            .find(|(id, p)| usable(p) && (**id == name || p.manifest.names.contains(&name)))
            .map(|(id, _)| id.clone())
    }

    // ---- loading -----------------------------------------------------------------

    /// Resolve a [`Lang`]: compiled-in grammar first, then an installed plugin
    /// (loaded on demand). Errors are typed; callers fall back to plain text.
    pub fn language_for(&self, lang: Lang) -> Result<Grammar, GrammarError> {
        self.language_for_id(lang.id())
    }

    /// Resolve a file extension, see [`GrammarRegistry::id_for_extension`].
    pub fn language_for_extension(&self, ext: &str) -> Result<Grammar, GrammarError> {
        match self.id_for_extension(ext) {
            Some(id) => self.language_for_id(&id),
            None => Err(GrammarError::NotInstalled(lower_ext(ext))),
        }
    }

    /// Resolve a grammar id, built-in first.
    pub fn language_for_id(&self, id: &str) -> Result<Grammar, GrammarError> {
        if let Some(lang) = Lang::from_id(id) {
            if crate::highlight::builtin_config(lang).is_some() {
                return Ok(Grammar {
                    id: id.to_owned(),
                    display_name: lang.name().to_owned(),
                    source: GrammarSource::Builtin,
                    version: None,
                    language: None,
                });
            }
        }
        self.load_plugin(id)
    }

    /// Load the installed *plugin* `id` (even when a built-in of the same id exists).
    pub fn load_plugin(&self, id: &str) -> Result<Grammar, GrammarError> {
        let loaded = self.plugin_loaded(id)?;
        let g = self.read();
        let (display_name, version) = g.plugins.get(id).map_or_else(
            || (id.to_owned(), None),
            |p| {
                (
                    p.manifest.display_name.clone(),
                    Some(p.manifest.version.clone()),
                )
            },
        );
        Ok(Grammar {
            id: id.to_owned(),
            display_name,
            source: GrammarSource::Plugin,
            version,
            language: Some(loaded.language.clone()),
        })
    }

    /// The compiled highlight configuration of plugin `id`, loading it if needed.
    pub(crate) fn plugin_config(
        &self,
        id: &str,
    ) -> Result<&'static HighlightConfiguration, GrammarError> {
        self.plugin_loaded(id).map(|l| &l.config)
    }

    fn plugin_loaded(&self, id: &str) -> Result<&'static Loaded, GrammarError> {
        {
            let g = self.read();
            let p = g
                .plugins
                .get(id)
                .ok_or_else(|| GrammarError::NotInstalled(id.to_owned()))?;
            match &p.slot {
                Slot::Loaded(l) => return Ok(l),
                Slot::Failed(e) => return Err(e.clone()),
                Slot::Idle if p.pending_removal => {
                    return Err(GrammarError::NotInstalled(id.to_owned()))
                }
                Slot::Idle => {}
            }
        }
        let mut g = self.write();
        let p = g
            .plugins
            .get_mut(id)
            .ok_or_else(|| GrammarError::NotInstalled(id.to_owned()))?;
        match &p.slot {
            Slot::Loaded(l) => return Ok(l),
            Slot::Failed(e) => return Err(e.clone()),
            Slot::Idle => {}
        }
        match load_plugin(&p.dir, &p.manifest) {
            Ok(loaded) => {
                // Leaked on purpose: the DLL must outlive every Language made from it.
                let loaded: &'static Loaded = Box::leak(Box::new(loaded));
                p.slot = Slot::Loaded(loaded);
                Ok(loaded)
            }
            Err(e) => {
                p.slot = Slot::Failed(e.clone());
                Err(e)
            }
        }
    }

    /// Check the files of an installed plugin against its manifest without loading it.
    pub fn verify(&self, id: &str) -> Result<(), GrammarError> {
        let (dir, manifest) = {
            let g = self.read();
            let p = g
                .plugins
                .get(id)
                .ok_or_else(|| GrammarError::NotInstalled(id.to_owned()))?;
            (p.dir.clone(), p.manifest.clone())
        };
        check_abi(manifest.tree_sitter_abi as usize)?;
        verify_files(&dir, &manifest).map(|_| ())
    }

    // ---- install / uninstall -----------------------------------------------------

    /// Install a plugin from a folder holding `manifest.toml` and the files it lists
    /// ("developer mode"). Files are copied and then verified against the manifest.
    pub fn install_from_dir(&self, src: &Path) -> Result<InstallOutcome, GrammarError> {
        let manifest = Manifest::read(src)?;
        check_abi(manifest.tree_sitter_abi as usize)?;
        let staging = Staging::create(&self.dir, &manifest.name)?;
        copy_plain(&src.join(MANIFEST_FILE), &staging.path.join(MANIFEST_FILE))?;
        for name in manifest.files.keys() {
            copy_plain(&src.join(name), &staging.path.join(name))?;
        }
        verify_files(&staging.path, &manifest)?;
        self.finalize(staging, manifest)
    }

    /// Install a plugin from the bytes of a downloaded `.zip`. The archive must match
    /// the sha256 (and size) the index pins; its `manifest.toml` must name `entry.lang`.
    pub fn install_from_archive(
        &self,
        bytes: &[u8],
        entry: &IndexEntry,
    ) -> Result<InstallOutcome, GrammarError> {
        if !valid_id(&entry.lang) {
            return Err(GrammarError::InvalidId(entry.lang.clone()));
        }
        verify_archive(bytes, &entry.sha256)?;
        if entry.size != 0 && entry.size != bytes.len() as u64 {
            return Err(GrammarError::Archive(format!(
                "size is {} bytes but the index says {}",
                bytes.len(),
                entry.size
            )));
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| GrammarError::Archive(e.to_string()))?;
        if zip.len() > MAX_ARCHIVE_ENTRIES {
            return Err(GrammarError::Archive("too many entries".into()));
        }
        let manifest_bytes = read_entry(&mut zip, MANIFEST_FILE, 256 * 1024)?;
        let text = String::from_utf8(manifest_bytes)
            .map_err(|_| GrammarError::Archive("manifest is not UTF-8".into()))?;
        let manifest = Manifest::parse(&text, Path::new("archive:manifest.toml"))?;
        if manifest.name != entry.lang {
            return Err(GrammarError::Archive(format!(
                "archive holds `{}` but the index entry is `{}`",
                manifest.name, entry.lang
            )));
        }
        check_abi(manifest.tree_sitter_abi as usize)?;
        let staging = Staging::create(&self.dir, &manifest.name)?;
        write_new(&staging.path.join(MANIFEST_FILE), text.as_bytes())?;
        for name in manifest.files.keys() {
            let limit = if name.ends_with(".scm") {
                super::manifest::MAX_QUERY_BYTES
            } else {
                super::manifest::MAX_FILE_BYTES
            };
            let data = read_entry(&mut zip, name, limit)?;
            write_new(&staging.path.join(name), &data)?;
        }
        verify_files(&staging.path, &manifest)?;
        self.finalize(staging, manifest)
    }

    /// Move a verified staging folder into place.
    fn finalize(
        &self,
        mut staging: Staging,
        manifest: Manifest,
    ) -> Result<InstallOutcome, GrammarError> {
        let id = manifest.name.clone();
        let version = manifest.version.clone();
        let target = self.dir.join(&id);
        let newdir = self.dir.join(format!("{id}{NEW_SUFFIX}"));
        let mut g = self.write();
        let loaded = matches!(g.plugins.get(&id).map(|p| &p.slot), Some(Slot::Loaded(_)));

        if !loaded && try_replace(&staging.path, &target) {
            staging.keep = true;
            g.plugins.insert(
                id.clone(),
                Plugin {
                    manifest,
                    dir: target,
                    slot: Slot::Idle,
                    pending_removal: false,
                    pending_update: false,
                },
            );
            g.problems.retain(|(p, _)| p != &self.dir.join(&id));
            return Ok(InstallOutcome::Installed { id, version });
        }

        // The old DLL is mapped (or could not be removed): stage for next start.
        if newdir.exists() {
            std::fs::remove_dir_all(&newdir).map_err(|e| GrammarError::io(&newdir, &e))?;
        }
        std::fs::rename(&staging.path, &newdir).map_err(|e| GrammarError::io(&newdir, &e))?;
        staging.keep = true;
        if let Some(p) = g.plugins.get_mut(&id) {
            p.pending_update = true;
            p.pending_removal = false;
        }
        Ok(InstallOutcome::PendingRestart { id, version })
    }

    /// Remove a plugin. Refused while its DLL is loaded in this process: it is marked
    /// for removal and deleted on next start ([`UninstallOutcome::PendingRestart`]).
    pub fn uninstall(&self, id: &str) -> Result<UninstallOutcome, GrammarError> {
        if !valid_id(id) {
            return Err(GrammarError::InvalidId(id.to_owned()));
        }
        let mut g = self.write();
        let (dir, loaded) = match g.plugins.get(id) {
            Some(p) => (p.dir.clone(), matches!(p.slot, Slot::Loaded(_))),
            None => return Err(GrammarError::NotInstalled(id.to_owned())),
        };
        let _ = std::fs::remove_dir_all(self.dir.join(format!("{id}{NEW_SUFFIX}")));
        if !loaded && std::fs::remove_dir_all(&dir).is_ok() {
            g.plugins.remove(id);
            return Ok(UninstallOutcome::Removed);
        }
        // Mapped DLL (or a locked file): delete at next start, before anything loads.
        let marker = dir.join(PENDING_REMOVAL);
        std::fs::write(&marker, b"").map_err(|e| GrammarError::io(&marker, &e))?;
        if let Some(p) = g.plugins.get_mut(id) {
            p.pending_removal = true;
            p.pending_update = false;
        }
        Ok(UninstallOutcome::PendingRestart)
    }
}

fn usable(p: &Plugin) -> bool {
    match p.slot {
        Slot::Failed(_) => false,
        Slot::Loaded(_) => true,
        Slot::Idle => !p.pending_removal,
    }
}

/// Replace `target` by `staging`. False when the old folder cannot be removed (a DLL
/// that is in use somewhere) so the caller stages for next start instead.
fn try_replace(staging: &Path, target: &Path) -> bool {
    if target.exists() && std::fs::remove_dir_all(target).is_err() {
        return false;
    }
    std::fs::rename(staging, target).is_ok()
}

fn copy_plain(from: &Path, to: &Path) -> Result<(), GrammarError> {
    let meta = std::fs::metadata(from).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GrammarError::MissingFile {
                file: from
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            }
        } else {
            GrammarError::io(from, &e)
        }
    })?;
    if !meta.is_file() || meta.len() > super::manifest::MAX_FILE_BYTES {
        return Err(GrammarError::manifest(
            from,
            "not a regular file or too large",
        ));
    }
    std::fs::copy(from, to).map_err(|e| GrammarError::io(from, &e))?;
    Ok(())
}

fn write_new(path: &Path, data: &[u8]) -> Result<(), GrammarError> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| GrammarError::io(path, &e))?;
    f.write_all(data).map_err(|e| GrammarError::io(path, &e))
}

/// Read one entry of a zip by exact name, refusing anything above `limit` bytes.
fn read_entry(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, GrammarError> {
    let file = zip.by_name(name).map_err(|_| GrammarError::MissingFile {
        file: name.to_owned(),
    })?;
    if file.is_dir() || file.size() > limit {
        return Err(GrammarError::Archive(format!(
            "`{name}` is a directory or too large"
        )));
    }
    let mut out = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| GrammarError::Archive(e.to_string()))?;
    if out.len() as u64 > limit {
        return Err(GrammarError::Archive(format!("`{name}` is too large")));
    }
    Ok(out)
}

/// Startup housekeeping: removals marked last run, staged updates, stale staging folders.
fn apply_pending(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    for path in &entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with(STAGING_PREFIX) {
            let _ = std::fs::remove_dir_all(path);
        } else if path.join(PENDING_REMOVAL).exists() {
            remove_keeping_marker(path);
        }
    }
    for path in &entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let Some(id) = name.strip_suffix(NEW_SUFFIX) else {
            continue;
        };
        if !valid_id(id) || !path.exists() {
            continue;
        }
        let target = dir.join(id);
        if !target.exists() || std::fs::remove_dir_all(&target).is_ok() {
            let _ = std::fs::rename(path, &target);
        }
    }
}

/// Delete a plugin folder marked for removal. If a file cannot be deleted (a DLL still
/// mapped by some process) the marker stays, so the removal is retried next start and
/// the half-deleted plugin stays hidden.
fn remove_keeping_marker(dir: &Path) {
    let mut all_gone = true;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name() == PENDING_REMOVAL {
                continue;
            }
            let p = e.path();
            let r = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
            all_gone &= r.is_ok();
        }
    }
    if all_gone {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn scan(dir: &Path) -> Inner {
    let mut inner = Inner::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return inner;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with(NEW_SUFFIX) || !path.is_dir() {
            continue;
        }
        if !path.join(MANIFEST_FILE).exists() {
            continue;
        }
        match Manifest::read(&path) {
            Ok(m) if m.name == name => {
                let pending_removal = path.join(PENDING_REMOVAL).exists();
                let pending_update = dir.join(format!("{name}{NEW_SUFFIX}")).exists();
                inner.plugins.insert(
                    name,
                    Plugin {
                        manifest: m,
                        dir: path,
                        slot: Slot::Idle,
                        pending_removal,
                        pending_update,
                    },
                );
            }
            Ok(m) => {
                let e = GrammarError::manifest(
                    &path,
                    format!("`name = \"{}\"` does not match the folder `{name}`", m.name),
                );
                inner.problems.push((path, e));
            }
            Err(e) => inner.problems.push((path, e)),
        }
    }
    inner
}
