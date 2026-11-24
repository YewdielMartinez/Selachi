//! `PluginsModel`: the grammar plugin manager behind Settings -> Plugins (docs/PLAN.md section 18).
//!
//! * The index (`grammars.toml`) is fetched over HTTPS **only when the Plugins section is opened**
//!   and cached in memory for one hour ([`INDEX_TTL`]). No timers, no polling.
//! * The list merges what is installed (`GrammarRegistry::available`) with the index; the mapping
//!   is the pure function [`build_rows`].
//! * Install / update / remove run on the background executor. Downloads report progress through
//!   a channel (throttled), so the UI redraws a few times per second while a download runs and
//!   never otherwise.
//! * A plugin is native code: the archive must match the sha256 pinned in the index
//!   (`GrammarRegistry::install_from_archive` verifies it), is capped at
//!   [`MAX_ARCHIVE_BYTES`] and is never installed without an explicit click.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::{Context, EventEmitter, Task};
use viewer_svc::grammars::{
    GrammarIndex, GrammarInfo, GrammarRegistry, GrammarSource, GrammarState, IndexEntry,
    InstallOutcome, UninstallOutcome,
};

use crate::fmt;
use crate::net::{
    ARCHIVE_TIMEOUT, FetchError, Fetcher, INDEX_TIMEOUT, MAX_ARCHIVE_BYTES, MAX_INDEX_BYTES,
};

/// How long a fetched index is trusted.
pub const INDEX_TTL: Duration = Duration::from_secs(60 * 60);

/// Where the list of installable plugins stands.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexState {
    /// Never requested (the section was not opened yet).
    Idle,
    Loading,
    Ready,
    /// The download or parse failed (message in Spanish).
    Failed(String),
}

/// What a row is doing right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    Installing,
    Removing,
    /// The last action failed; kept until the next one.
    Failed(String),
}

/// Why a restart is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartKind {
    /// A newer version is staged.
    Update,
    /// The plugin is deleted at the next start.
    Removal,
}

/// Installation state of a row (independent of what it is doing right now).
#[derive(Debug, Clone, PartialEq)]
pub enum PluginState {
    /// Compiled into Selachi.
    Builtin,
    Installed,
    /// Installed, and the index offers a newer version.
    Update {
        to: String,
    },
    /// Only in the index.
    Available,
    /// Done, but takes effect after restarting.
    PendingRestart(RestartKind),
    /// Installed but its files do not verify or load (message).
    Broken(String),
}

/// One row of the plugin list.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginRow {
    pub id: String,
    pub name: String,
    /// Installed version.
    pub version: Option<String>,
    /// Version the index offers.
    pub index_version: Option<String>,
    /// Download size when the index lists it, else the installed size.
    pub size: u64,
    pub extensions: Vec<String>,
    pub state: PluginState,
    pub op: Option<Op>,
}

impl PluginRow {
    /// "Instalado", "Actualización disponible v0.24.0", "Disponible", "Fallo: ..."...
    pub fn label(&self) -> String {
        if let Some(op) = &self.op {
            match op {
                Op::Downloading { received, total } => {
                    return match total {
                        Some(t) if *t > 0 => {
                            format!(
                                "Descargando {} %",
                                ((*received as f64 / *t as f64) * 100.0).min(100.0) as u32
                            )
                        }
                        _ => format!("Descargando {}", fmt::bytes(*received)),
                    };
                }
                Op::Installing => return "Instalando…".to_string(),
                Op::Removing => return "Quitando…".to_string(),
                Op::Failed(m) => return format!("Fallo: {m}"),
            }
        }
        match &self.state {
            PluginState::Builtin => "Incluido".to_string(),
            PluginState::Installed => "Instalado".to_string(),
            PluginState::Update { to } => {
                format!("Actualización disponible v{}", to.trim_start_matches('v'))
            }
            PluginState::Available => "Disponible".to_string(),
            PluginState::PendingRestart(RestartKind::Update) => {
                "Actualizado: reinicia para completar".to_string()
            }
            PluginState::PendingRestart(RestartKind::Removal) => {
                "Se quitará al reiniciar".to_string()
            }
            PluginState::Broken(m) => format!("Fallo: {m}"),
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.op,
            Some(Op::Downloading { .. } | Op::Installing | Op::Removing)
        )
    }

    /// The index has an entry that can be (re)installed and nothing is running or staged.
    pub fn can_install(&self) -> bool {
        self.index_version.is_some()
            && !self.is_busy()
            && matches!(self.state, PluginState::Available | PluginState::Broken(_))
    }

    pub fn can_update(&self) -> bool {
        !self.is_busy() && matches!(self.state, PluginState::Update { .. })
    }

    pub fn can_remove(&self) -> bool {
        !self.is_busy()
            && matches!(
                self.state,
                PluginState::Installed | PluginState::Update { .. } | PluginState::Broken(_)
            )
    }

    pub fn needs_restart(&self) -> bool {
        matches!(self.state, PluginState::PendingRestart(_))
    }

    /// Group in the list: installed ones first, then installable, then built-ins.
    pub fn group(&self) -> Group {
        match self.state {
            PluginState::Builtin => Group::Builtin,
            PluginState::Available => Group::Available,
            _ => Group::Installed,
        }
    }
}

/// Section of the plugin list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Installed,
    Available,
    Builtin,
}

/// Merge the installed grammars with the index and the running operations. Pure.
///
/// Order: installed plugins (updates first), then available ones, then built-ins; each part by name.
pub fn build_rows(
    index: Option<&GrammarIndex>,
    installed: &[GrammarInfo],
    ops: &BTreeMap<String, Op>,
) -> Vec<PluginRow> {
    let mut rows: Vec<PluginRow> = Vec::new();
    for g in installed {
        let entry = index.and_then(|i| i.entry(&g.id));
        let state = match g.source {
            GrammarSource::Builtin => PluginState::Builtin,
            GrammarSource::Plugin => {
                if g.pending_removal {
                    PluginState::PendingRestart(RestartKind::Removal)
                } else if g.pending_update {
                    PluginState::PendingRestart(RestartKind::Update)
                } else if let GrammarState::Failed(e) = &g.state {
                    PluginState::Broken(e.to_string())
                } else if let (Some(e), Some(v)) = (entry, &g.version)
                    && fmt::is_newer(&e.version, v)
                {
                    PluginState::Update {
                        to: e.version.clone(),
                    }
                } else {
                    PluginState::Installed
                }
            }
        };
        let builtin = g.source == GrammarSource::Builtin;
        rows.push(PluginRow {
            id: g.id.clone(),
            name: g.display_name.clone(),
            version: g.version.clone(),
            index_version: entry.filter(|_| !builtin).map(|e| e.version.clone()),
            size: match entry {
                Some(e) if !builtin && e.size > 0 => e.size,
                _ => g.size_bytes,
            },
            extensions: g.file_extensions.clone(),
            state,
            op: ops.get(&g.id).cloned(),
        });
    }
    if let Some(index) = index {
        for e in &index.grammars {
            if installed.iter().any(|g| g.id == e.lang) {
                continue;
            }
            rows.push(PluginRow {
                id: e.lang.clone(),
                name: e.display_name.clone().unwrap_or_else(|| e.lang.clone()),
                version: None,
                index_version: Some(e.version.clone()),
                size: e.size,
                extensions: e.file_extensions.clone(),
                state: PluginState::Available,
                op: ops.get(&e.lang).cloned(),
            });
        }
    }
    let rank = |r: &PluginRow| match r.state {
        PluginState::Update { .. } => 0,
        PluginState::PendingRestart(_) => 1,
        PluginState::Broken(_) => 1,
        _ => 2,
    };
    rows.sort_by(|a, b| {
        (a.group(), rank(a), a.name.to_lowercase()).cmp(&(
            b.group(),
            rank(b),
            b.name.to_lowercase(),
        ))
    });
    rows
}

/// Download and parse the index (blocking: run it off the UI thread).
pub fn fetch_index(fetcher: &dyn Fetcher, url: &str) -> Result<GrammarIndex, String> {
    let bytes = fetcher
        .get(url, MAX_INDEX_BYTES, INDEX_TIMEOUT, &mut |_, _| {})
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "El índice no es texto UTF-8.".to_string())?;
    GrammarIndex::parse(&text).map_err(|e| e.to_string())
}

/// Download `entry` and install it (blocking). `progress` gets `(received, total)`.
///
/// Refuses entries whose declared size is above [`MAX_ARCHIVE_BYTES`] before any download starts;
/// `install_from_archive` then verifies sha256 and size against the index.
pub fn download_and_install(
    registry: &GrammarRegistry,
    fetcher: &dyn Fetcher,
    entry: &IndexEntry,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<InstallOutcome, String> {
    if entry.size > MAX_ARCHIVE_BYTES {
        return Err(FetchError::TooLarge {
            cap: MAX_ARCHIVE_BYTES,
        }
        .to_string());
    }
    let bytes = fetcher
        .get(&entry.url, MAX_ARCHIVE_BYTES, ARCHIVE_TIMEOUT, progress)
        .map_err(|e| e.to_string())?;
    registry
        .install_from_archive(&bytes, entry)
        .map_err(|e| e.to_string())
}

/// What the model tells the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginsEvent {
    /// `id` was installed or updated. `restart` = it only takes effect after restarting.
    Installed { id: String, restart: bool },
    /// `id` was removed (`restart` = it is deleted at the next start).
    Removed { id: String, restart: bool },
}

enum Msg {
    Progress(u64, Option<u64>),
    Installing,
}

/// The plugin manager (a gpui entity: notifies on every visible change).
pub struct PluginsModel {
    registry: Arc<GrammarRegistry>,
    fetcher: Arc<dyn Fetcher>,
    index_url: String,
    index: Option<GrammarIndex>,
    index_state: IndexState,
    fetched_at: Option<Instant>,
    ops: BTreeMap<String, Op>,
    tasks: BTreeMap<String, Task<()>>,
    index_task: Option<Task<()>>,
}

impl EventEmitter<PluginsEvent> for PluginsModel {}

impl PluginsModel {
    pub fn new(
        registry: Arc<GrammarRegistry>,
        fetcher: Arc<dyn Fetcher>,
        index_url: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            fetcher,
            index_url: index_url.into(),
            index: None,
            index_state: IndexState::Idle,
            fetched_at: None,
            ops: BTreeMap::new(),
            tasks: BTreeMap::new(),
            index_task: None,
        }
    }

    pub fn registry(&self) -> &Arc<GrammarRegistry> {
        &self.registry
    }

    pub fn index_url(&self) -> &str {
        &self.index_url
    }

    /// Point at another index (the user edited the URL). Drops the cached index.
    pub fn set_index_url(&mut self, url: impl Into<String>, cx: &mut Context<Self>) {
        let url = url.into();
        if url == self.index_url {
            return;
        }
        self.index_url = url;
        self.index = None;
        self.fetched_at = None;
        self.index_state = IndexState::Idle;
        self.index_task = None;
        cx.notify();
    }

    pub fn index_state(&self) -> &IndexState {
        &self.index_state
    }

    pub fn index(&self) -> Option<&GrammarIndex> {
        self.index.as_ref()
    }

    /// The current rows (cheap: one registry read and a merge).
    pub fn rows(&self) -> Vec<PluginRow> {
        build_rows(self.index.as_ref(), &self.registry.available(), &self.ops)
    }

    /// Any row that needs a restart to take effect.
    pub fn needs_restart(&self) -> bool {
        self.rows().iter().any(PluginRow::needs_restart)
    }

    fn index_is_fresh(&self) -> bool {
        self.fetched_at.is_some_and(|t| t.elapsed() < INDEX_TTL) && self.index.is_some()
    }

    /// Fetch the index unless a fresh one is cached or a fetch is running. Call when the Plugins
    /// section opens.
    pub fn ensure_index(&mut self, cx: &mut Context<Self>) {
        if self.index_state == IndexState::Loading || self.index_is_fresh() {
            return;
        }
        self.fetch(cx);
    }

    /// Fetch again regardless of the cache ("Reintentar").
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.index_state == IndexState::Loading {
            return;
        }
        self.fetch(cx);
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        self.index_state = IndexState::Loading;
        cx.notify();
        let fetcher = self.fetcher.clone();
        let url = self.index_url.clone();
        self.index_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { fetch_index(fetcher.as_ref(), &url) })
                .await;
            let _ = this.update(cx, |m, cx| m.index_done(result, cx));
        }));
    }

    fn index_done(&mut self, result: Result<GrammarIndex, String>, cx: &mut Context<Self>) {
        match result {
            Ok(index) => {
                self.index = Some(index);
                self.fetched_at = Some(Instant::now());
                self.index_state = IndexState::Ready;
            }
            Err(e) => {
                // Keep the stale index (if any) usable, but say the refresh failed.
                self.index_state = IndexState::Failed(e);
            }
        }
        self.index_task = None;
        cx.notify();
    }

    /// Install (or update / reinstall) `id` from the index. The caller has already asked the user.
    pub fn install(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.tasks.contains_key(id) {
            return;
        }
        let Some(entry) = self.index.as_ref().and_then(|i| i.entry(id)).cloned() else {
            self.ops.insert(
                id.to_string(),
                Op::Failed("no está en el índice".to_string()),
            );
            cx.notify();
            return;
        };
        self.ops.insert(
            id.to_string(),
            Op::Downloading {
                received: 0,
                total: (entry.size > 0).then_some(entry.size),
            },
        );
        cx.notify();
        let registry = self.registry.clone();
        let fetcher = self.fetcher.clone();
        let key = id.to_string();
        let task = cx.spawn(async move |this, cx| {
            let (tx, mut rx) = unbounded::<Msg>();
            let work = cx.background_executor().spawn(async move {
                let mut last = Instant::now();
                let mut last_pct = u64::MAX;
                let result = download_and_install(
                    registry.as_ref(),
                    fetcher.as_ref(),
                    &entry,
                    &mut |n, t| {
                        // Throttle: at most ~8 redraws/s, plus the first and the last.
                        let pct = t.filter(|t| *t > 0).map_or(n / 65_536, |t| n * 100 / t);
                        if pct != last_pct
                            && (last.elapsed() >= Duration::from_millis(120) || Some(n) == t)
                        {
                            last = Instant::now();
                            last_pct = pct;
                            let _ = tx.unbounded_send(Msg::Progress(n, t));
                        }
                        if Some(n) == t {
                            let _ = tx.unbounded_send(Msg::Installing);
                        }
                    },
                );
                drop(tx);
                result
            });
            while let Some(msg) = rx.next().await {
                let key = key.clone();
                let _ = this.update(cx, |m, cx| m.on_msg(&key, msg, cx));
            }
            let result = work.await;
            let _ = this.update(cx, |m, cx| m.install_done(&key, result, cx));
        });
        self.tasks.insert(id.to_string(), task);
    }

    fn on_msg(&mut self, id: &str, msg: Msg, cx: &mut Context<Self>) {
        let Some(op) = self.ops.get_mut(id) else {
            return;
        };
        match msg {
            Msg::Progress(received, total) => *op = Op::Downloading { received, total },
            Msg::Installing => *op = Op::Installing,
        }
        cx.notify();
    }

    fn install_done(
        &mut self,
        id: &str,
        result: Result<InstallOutcome, String>,
        cx: &mut Context<Self>,
    ) {
        self.tasks.remove(id);
        match result {
            Ok(outcome) => {
                self.ops.remove(id);
                let restart = matches!(outcome, InstallOutcome::PendingRestart { .. });
                cx.emit(PluginsEvent::Installed {
                    id: id.to_string(),
                    restart,
                });
            }
            Err(e) => {
                self.ops.insert(id.to_string(), Op::Failed(e));
            }
        }
        cx.notify();
    }

    /// Remove an installed plugin. The caller has already asked the user.
    pub fn uninstall(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.tasks.contains_key(id) {
            return;
        }
        self.ops.insert(id.to_string(), Op::Removing);
        cx.notify();
        let registry = self.registry.clone();
        let key = id.to_string();
        let task = cx.spawn(async move |this, cx| {
            let id = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move { registry.uninstall(&id).map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |m, cx| m.uninstall_done(&key, result, cx));
        });
        self.tasks.insert(id.to_string(), task);
    }

    fn uninstall_done(
        &mut self,
        id: &str,
        result: Result<UninstallOutcome, String>,
        cx: &mut Context<Self>,
    ) {
        self.tasks.remove(id);
        match result {
            Ok(outcome) => {
                self.ops.remove(id);
                cx.emit(PluginsEvent::Removed {
                    id: id.to_string(),
                    restart: outcome == UninstallOutcome::PendingRestart,
                });
            }
            Err(e) => {
                self.ops.insert(id.to_string(), Op::Failed(e));
            }
        }
        cx.notify();
    }

    /// Forget a failed action's message (the row goes back to its installation state).
    pub fn dismiss_failure(&mut self, id: &str, cx: &mut Context<Self>) {
        if matches!(self.ops.get(id), Some(Op::Failed(_))) {
            self.ops.remove(id);
            cx.notify();
        }
    }

    /// Size and message the confirmation popover shows before installing `id`.
    pub fn confirm_text(&self, id: &str) -> Option<(u64, String)> {
        let e = self.index.as_ref()?.entry(id)?;
        Some((
            e.size,
            format!(
                "v{} · {}",
                e.version.trim_start_matches('v'),
                fmt::bytes(e.size)
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewer_svc::grammars::GrammarError;

    fn info(id: &str, version: Option<&str>, source: GrammarSource) -> GrammarInfo {
        GrammarInfo {
            id: id.into(),
            display_name: id.to_uppercase(),
            version: version.map(Into::into),
            file_extensions: vec![id.into()],
            state: if source == GrammarSource::Builtin {
                GrammarState::Builtin
            } else {
                GrammarState::Installed
            },
            source,
            size_bytes: 1000,
            pending_removal: false,
            pending_update: false,
        }
    }

    fn index(entries: &[(&str, &str, u64)]) -> GrammarIndex {
        let mut text = String::new();
        for (lang, version, size) in entries {
            text.push_str(&format!(
                "[[grammar]]\nlang = \"{lang}\"\nversion = \"{version}\"\nurl = \"https://example.com/{lang}.zip\"\nsha256 = \"{}\"\nsize = {size}\ndisplay_name = \"{}\"\n\n",
                "ab".repeat(32),
                lang.to_uppercase()
            ));
        }
        GrammarIndex::parse(&text).unwrap()
    }

    fn row<'a>(rows: &'a [PluginRow], id: &str) -> &'a PluginRow {
        rows.iter().find(|r| r.id == id).unwrap()
    }

    #[test]
    fn states_map_from_registry_and_index() {
        let installed = vec![
            info("json", None, GrammarSource::Builtin),
            info("rust", Some("0.24.2"), GrammarSource::Plugin),
            info("csharp", Some("0.23.5"), GrammarSource::Plugin),
            info("python", Some("0.25.0"), GrammarSource::Plugin),
        ];
        let idx = index(&[
            ("rust", "0.24.2", 500),
            ("csharp", "0.24.0", 600),
            ("go", "1.0.0", 700),
        ]);
        let rows = build_rows(Some(&idx), &installed, &BTreeMap::new());
        assert_eq!(rows.len(), 5);
        assert_eq!(row(&rows, "json").state, PluginState::Builtin);
        assert_eq!(row(&rows, "json").label(), "Incluido");
        assert_eq!(row(&rows, "rust").state, PluginState::Installed);
        assert_eq!(row(&rows, "rust").label(), "Instalado");
        assert_eq!(
            row(&rows, "csharp").state,
            PluginState::Update {
                to: "0.24.0".into()
            }
        );
        assert_eq!(
            row(&rows, "csharp").label(),
            "Actualización disponible v0.24.0"
        );
        assert!(row(&rows, "csharp").can_update());
        assert_eq!(
            row(&rows, "csharp").size,
            600,
            "the index size wins while it is listed"
        );
        // Installed but not in the index: still Installed, removable, not updatable.
        assert_eq!(row(&rows, "python").state, PluginState::Installed);
        assert!(row(&rows, "python").can_remove() && !row(&rows, "python").can_update());
        assert_eq!(row(&rows, "go").state, PluginState::Available);
        assert_eq!(row(&rows, "go").label(), "Disponible");
        assert!(row(&rows, "go").can_install() && !row(&rows, "go").can_remove());
        // Order: update first, then installed, then available, built-ins last.
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["csharp", "python", "rust", "go", "json"]);
    }

    #[test]
    fn a_newer_installed_version_is_not_an_update() {
        let installed = vec![info("rust", Some("0.25.0"), GrammarSource::Plugin)];
        let idx = index(&[("rust", "0.24.2", 500)]);
        let rows = build_rows(Some(&idx), &installed, &BTreeMap::new());
        assert_eq!(rows[0].state, PluginState::Installed);
    }

    #[test]
    fn without_an_index_only_installed_rows_exist() {
        let installed = vec![info("rust", Some("0.24.2"), GrammarSource::Plugin)];
        let rows = build_rows(None, &installed, &BTreeMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, PluginState::Installed);
        assert!(!rows[0].can_install());
    }

    #[test]
    fn pending_restart_and_broken_states() {
        let mut upd = info("rust", Some("0.24.2"), GrammarSource::Plugin);
        upd.pending_update = true;
        let mut rem = info("csharp", Some("0.23.5"), GrammarSource::Plugin);
        rem.pending_removal = true;
        let mut bad = info("python", Some("0.25.0"), GrammarSource::Plugin);
        bad.state = GrammarState::Failed(GrammarError::MissingFile {
            file: "grammar.dll".into(),
        });
        let idx = index(&[("python", "0.25.0", 10)]);
        let rows = build_rows(Some(&idx), &[upd, rem, bad], &BTreeMap::new());
        assert_eq!(
            row(&rows, "rust").state,
            PluginState::PendingRestart(RestartKind::Update)
        );
        assert!(row(&rows, "rust").needs_restart() && !row(&rows, "rust").can_remove());
        assert_eq!(row(&rows, "csharp").label(), "Se quitará al reiniciar");
        assert!(row(&rows, "python").label().starts_with("Fallo: "));
        assert!(
            row(&rows, "python").can_install(),
            "a broken plugin can be reinstalled from the index"
        );
        assert!(row(&rows, "python").can_remove());
    }

    #[test]
    fn operations_override_the_label_and_block_actions() {
        let idx = index(&[("go", "1.0.0", 1000)]);
        let mut ops = BTreeMap::new();
        ops.insert(
            "go".to_string(),
            Op::Downloading {
                received: 250,
                total: Some(1000),
            },
        );
        let rows = build_rows(Some(&idx), &[], &ops);
        assert_eq!(rows[0].label(), "Descargando 25 %");
        assert!(rows[0].is_busy() && !rows[0].can_install());
        ops.insert("go".to_string(), Op::Installing);
        assert_eq!(build_rows(Some(&idx), &[], &ops)[0].label(), "Instalando…");
        ops.insert("go".to_string(), Op::Failed("sha256 distinto".into()));
        let rows = build_rows(Some(&idx), &[], &ops);
        assert_eq!(rows[0].label(), "Fallo: sha256 distinto");
        assert!(rows[0].can_install(), "retry is allowed after a failure");
        ops.insert(
            "go".to_string(),
            Op::Downloading {
                received: 2048,
                total: None,
            },
        );
        assert_eq!(
            build_rows(Some(&idx), &[], &ops)[0].label(),
            "Descargando 2 KB"
        );
    }

    #[test]
    fn builtins_ignore_same_id_index_entries() {
        let installed = vec![info("json", None, GrammarSource::Builtin)];
        let idx = index(&[("json", "9.9.9", 1)]);
        let rows = build_rows(Some(&idx), &installed, &BTreeMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, PluginState::Builtin);
        assert!(rows[0].index_version.is_none() && !rows[0].can_install() && !rows[0].can_remove());
    }

    #[test]
    fn index_parse_rejects_non_https_and_bad_hashes() {
        let bad_url = "[[grammar]]\nlang = \"x\"\nversion = \"1\"\nurl = \"http://example.com/x.zip\"\nsha256 = \"".to_string()
            + &"ab".repeat(32)
            + "\"\n";
        assert!(GrammarIndex::parse(&bad_url).is_err());
        let bad_sha = "[[grammar]]\nlang = \"x\"\nversion = \"1\"\nurl = \"https://example.com/x.zip\"\nsha256 = \"abc\"\n";
        assert!(GrammarIndex::parse(bad_sha).is_err());
        assert!(GrammarIndex::parse("not toml [").is_err());
    }

    struct Fixed(Vec<u8>);
    impl Fetcher for Fixed {
        fn get(
            &self,
            url: &str,
            cap: u64,
            _t: Duration,
            _p: &mut dyn FnMut(u64, Option<u64>),
        ) -> Result<Vec<u8>, FetchError> {
            crate::net::check_https(url)?;
            if self.0.len() as u64 > cap {
                return Err(FetchError::TooLarge { cap });
            }
            Ok(self.0.clone())
        }
    }

    #[test]
    fn fetch_index_parses_and_surfaces_errors_in_spanish() {
        let good = index(&[("go", "1.0.0", 1)]).to_toml();
        let idx = fetch_index(
            &Fixed(good.into_bytes()),
            "https://example.com/grammars.toml",
        )
        .unwrap();
        assert_eq!(idx.grammars.len(), 1);
        let err =
            fetch_index(&Fixed(b"garbage = ".to_vec()), "https://example.com/g.toml").unwrap_err();
        assert!(
            err.contains("índice") || err.to_lowercase().contains("index"),
            "{err}"
        );
        let err = fetch_index(&Fixed(vec![0xff, 0xfe]), "https://example.com/g.toml").unwrap_err();
        assert!(err.contains("UTF-8"));
        let err = fetch_index(&Fixed(vec![]), "http://example.com/g.toml").unwrap_err();
        assert!(err.contains("https"), "{err}");
        // The 1 MB cap applies to the index.
        let big = Fixed(vec![b'#'; MAX_INDEX_BYTES as usize + 1]);
        assert!(
            fetch_index(&big, "https://example.com/g.toml")
                .unwrap_err()
                .contains("límite")
        );
    }

    #[test]
    fn oversized_archives_are_refused_before_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let registry = GrammarRegistry::new(dir.path());
        let mut entry = index(&[("go", "1.0.0", 10)]).grammars[0].clone();
        entry.size = MAX_ARCHIVE_BYTES + 1;
        struct Never;
        impl Fetcher for Never {
            fn get(
                &self,
                _: &str,
                _: u64,
                _: Duration,
                _: &mut dyn FnMut(u64, Option<u64>),
            ) -> Result<Vec<u8>, FetchError> {
                panic!("must not download");
            }
        }
        let err = download_and_install(&registry, &Never, &entry, &mut |_, _| {}).unwrap_err();
        assert!(err.contains("límite"), "{err}");
    }

    #[test]
    fn a_tampered_archive_is_rejected_by_the_pinned_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let registry = GrammarRegistry::new(dir.path());
        let entry = index(&[("go", "1.0.0", 0)]).grammars[0].clone(); // pins sha "abab..."
        let err = download_and_install(
            &registry,
            &Fixed(b"PK-not-the-pinned-bytes".to_vec()),
            &entry,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(err.contains("sha256"), "{err}");
        assert!(!registry.has_plugin("go"));
    }
}
