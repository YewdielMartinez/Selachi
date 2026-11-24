//! Auto-update (wave 6b.2): the app side of the `updater` crate.
//!
//! * **One timer.** The first check happens 30 s after start, then every 24 h while the app runs. The
//!   timer is a single task that sleeps: nothing polls, and switching `[updates] check` off drops it.
//! * **Background only.** Checking and downloading run on the background executor; the UI only sees
//!   the state ([`UpdateState`]) when it changes.
//! * **Unobtrusive.** A finished download becomes a small card ("Actualización 0.2.0 lista ·
//!   Reiniciar y actualizar"); nothing is installed without the user's action. With `install =
//!   "on_exit"` (or after pressing "Al cerrar") the installer runs when the window closes.
//! * **Safe.** https only, size caps, SHA256 against `SHA256SUMS.txt`, Authenticode status recorded
//!   (an unsigned installer is allowed for now; a *broken* signature is refused).
//!
//! `SELACHI_UPDATE_INDEX` (debug builds only) replaces the release URL, for demos against a mock.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};
use updater::{Config as UpdaterConfig, Fetcher, HttpFetcher, ReleaseInfo, Staged, Version};

/// First check, after the window is up (never at startup).
pub const FIRST_CHECK_AFTER: Duration = Duration::from_secs(30);
/// Then every day.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What the update machinery is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Downloading(Version),
    /// Downloaded and verified: waiting for the user.
    Ready(ReadyUpdate),
    Failed(String),
}

/// A verified installer plus what "Novedades" will show after the update.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadyUpdate {
    pub version: Version,
    pub notes: String,
    pub release_url: Option<String>,
    pub staged: Staged,
}

/// The state changed (the shell repaints the card and the settings row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateChanged;

pub struct UpdateHost {
    state: UpdateState,
    check_enabled: bool,
    /// `[updates] install = "on_exit"`.
    config_on_exit: bool,
    /// "Al cerrar" was pressed on the card this session.
    session_on_exit: bool,
    current: Version,
    cfg: UpdaterConfig,
    fetcher: Arc<dyn Fetcher>,
    dir: Option<PathBuf>,
    /// The card was dismissed for this session (the version it was dismissed for).
    dismissed: Option<Version>,
    /// The one timer; dropping it cancels it.
    timer: Option<Task<()>>,
    working: Option<Task<()>>,
}

impl EventEmitter<UpdateChanged> for UpdateHost {}

impl UpdateHost {
    pub fn new(current: &str, updates: &store::Updates) -> Self {
        let mut cfg = UpdaterConfig::default();
        if cfg!(debug_assertions)
            && let Ok(url) = std::env::var("SELACHI_UPDATE_INDEX")
        {
            cfg.release_url = url;
            cfg.asset_url_prefix = None;
        }
        Self {
            state: UpdateState::Idle,
            check_enabled: updates.check,
            config_on_exit: updates.install == "on_exit",
            session_on_exit: false,
            current: Version::parse(current).unwrap_or(Version {
                major: 0,
                minor: 0,
                patch: 0,
                pre: Vec::new(),
            }),
            cfg,
            fetcher: Arc::new(HttpFetcher),
            dir: updater::default_updates_dir(),
            dismissed: None,
            timer: None,
            working: None,
        }
    }

    /// Tests and demos give their own fetcher and folder.
    #[cfg(test)]
    pub fn with_fetcher(
        mut self,
        fetcher: Arc<dyn Fetcher>,
        cfg: UpdaterConfig,
        dir: PathBuf,
    ) -> Self {
        self.fetcher = fetcher;
        self.cfg = cfg;
        self.dir = Some(dir);
        self
    }

    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    pub fn current(&self) -> &Version {
        &self.current
    }

    /// The installer should run when the window closes.
    pub fn installs_on_exit(&self) -> bool {
        self.config_on_exit || self.session_on_exit
    }

    /// The card to show: a ready update the user did not dismiss.
    pub fn visible_update(&self) -> Option<&ReadyUpdate> {
        match &self.state {
            UpdateState::Ready(r) if self.dismissed.as_ref() != Some(&r.version) => Some(r),
            _ => None,
        }
    }

    fn set(&mut self, state: UpdateState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.emit(UpdateChanged);
            cx.notify();
        }
    }

    /// Applies `[updates]`: starts or stops the timer and remembers when to install.
    pub fn apply_config(&mut self, updates: &store::Updates, cx: &mut Context<Self>) {
        self.config_on_exit = updates.install == "on_exit";
        let was = self.check_enabled;
        self.check_enabled = updates.check;
        if !self.check_enabled {
            self.timer = None;
            if matches!(
                self.state,
                UpdateState::Checking | UpdateState::Downloading(_)
            ) {
                self.working = None;
                self.state = UpdateState::Idle;
            }
        } else if !was {
            self.start(cx);
        }
        cx.emit(UpdateChanged);
        cx.notify();
    }

    /// Starts the schedule (no-op when checking is off): 30 s, then every 24 h.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.timer = None;
        if !self.check_enabled {
            return;
        }
        self.timer = Some(cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK_AFTER;
            loop {
                cx.background_executor().timer(wait).await;
                if this.update(cx, |h, cx| h.check_now(cx)).is_err() {
                    break;
                }
                wait = CHECK_EVERY;
            }
        }));
    }

    /// "Buscar ahora" and the scheduled check: look, and download a newer release in the same go.
    pub fn check_now(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.state,
            UpdateState::Checking | UpdateState::Downloading(_) | UpdateState::Ready(_)
        ) {
            return;
        }
        self.set(UpdateState::Checking, cx);
        let (fetcher, cfg, current, dir) = (
            Arc::clone(&self.fetcher),
            self.cfg.clone(),
            self.current.clone(),
            self.dir.clone(),
        );
        self.working = Some(cx.spawn(async move |this, cx| {
            let (f1, c1) = (Arc::clone(&fetcher), cfg.clone());
            let found = cx
                .background_executor()
                .spawn(async move { updater::check(&*f1, &c1, &current) })
                .await;
            let release = match found {
                Ok(Some(r)) => r,
                Ok(None) => return finish(&this, UpdateState::UpToDate, cx),
                Err(e) => return finish(&this, UpdateState::Failed(e.to_string()), cx),
            };
            this.update(cx, |h, cx| {
                h.set(UpdateState::Downloading(release.version.clone()), cx)
            })
            .ok();
            let Some(dir) = dir else {
                return finish(
                    &this,
                    UpdateState::Failed("no se encontró la carpeta de actualizaciones".into()),
                    cx,
                );
            };
            let rel = release.clone();
            let staged = cx
                .background_executor()
                .spawn(async move {
                    let staged = updater::stage(&*fetcher, &cfg, &rel, &dir, &mut |_, _| {});
                    if staged.is_ok() {
                        updater::cleanup(&dir, Some(&rel.version));
                    }
                    staged
                })
                .await;
            let state = match staged {
                Ok(staged) => UpdateState::Ready(ready_from(&release, staged)),
                Err(e) => UpdateState::Failed(e.to_string()),
            };
            finish(&this, state, cx);
        }));
    }

    /// The card's "Más tarde": hidden until the next version (or the next start).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if let UpdateState::Ready(r) = &self.state {
            self.dismissed = Some(r.version.clone());
            cx.emit(UpdateChanged);
            cx.notify();
        }
    }

    /// The card's "Al cerrar": install silently when the window closes.
    pub fn install_when_closing(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, UpdateState::Ready(_)) {
            self.session_on_exit = true;
            cx.emit(UpdateChanged);
            cx.notify();
        }
    }

    /// The ready update, for the shell to install (it saves the session first).
    pub fn ready(&self) -> Option<&ReadyUpdate> {
        match &self.state {
            UpdateState::Ready(r) => Some(r),
            _ => None,
        }
    }

    /// Demo flag (`SELACHI_DEMO=update`): shows the card for `version` without any network. The
    /// installer path is empty, so pressing the button only reports that it could not start.
    pub fn demo_ready(&mut self, version: &str, notes: &str, cx: &mut Context<Self>) {
        let Some(v) = Version::parse(version) else {
            return;
        };
        let staged = Staged {
            path: PathBuf::new(),
            version: v.clone(),
            sha256: "0".repeat(64),
            size: 10 * 1024 * 1024,
            authenticode: updater::Authenticode::Unsigned,
        };
        self.set(
            UpdateState::Ready(ReadyUpdate {
                version: v,
                notes: notes.to_string(),
                release_url: None,
                staged,
            }),
            cx,
        );
    }

    /// Wipes old installers once the running version is the newest one.
    pub fn cleanup_old(&self) {
        if let Some(dir) = &self.dir {
            updater::cleanup(dir, None);
        }
    }
}

/// Ends a check: records the time and publishes the state.
fn finish(this: &gpui::WeakEntity<UpdateHost>, state: UpdateState, cx: &mut gpui::AsyncApp) {
    this.update(cx, |h, cx| {
        h.set(state, cx);
    })
    .ok();
}

fn ready_from(release: &ReleaseInfo, staged: Staged) -> ReadyUpdate {
    ReadyUpdate {
        version: release.version.clone(),
        notes: release.notes.clone(),
        release_url: release.html_url.clone(),
        staged,
    }
}

/// Text of the card: `Actualización 0.2.0 lista`.
pub fn ready_text(version: &Version) -> String {
    format!("Actualización {version} lista")
}

/// What the settings row says about the state.
pub fn status_text(state: &UpdateState, current: &Version) -> String {
    match state {
        UpdateState::Idle => format!("Versión {current}"),
        UpdateState::Checking => "Buscando…".to_string(),
        UpdateState::UpToDate => format!("Estás al día · versión {current}"),
        UpdateState::Downloading(v) => format!("Descargando {v}…"),
        UpdateState::Ready(r) => format!(
            "{} · {}",
            ready_text(&r.version),
            r.staged.authenticode.label()
        ),
        UpdateState::Failed(e) => format!("No se pudo buscar: {e}"),
    }
}

#[cfg(test)]
mod tests;
