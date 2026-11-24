//! `OnboardingView`: the entity, its state and its background work. Rendering lives in
//! `render.rs` (frame, header, footer) and `steps.rs` (the six bodies).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agents_driver::DetectedAgent;
use futures::StreamExt;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    PathPromptOptions, ScrollHandle, Subscription, Task, UniformListScrollHandle, Window,
};
use store::{Config, DiscoveredProject, StateDb};
use theme::{Mode, Theme};

use crate::agents::{self, AgentCli, AgentRow};
use crate::flag;
use crate::flow::{Flow, Nav, Step};
use crate::gif::{self, SharkFrame};
use crate::model::{Draft, Outcome, ProjectCounts, ThemeChoice, WorktreeLocation, summary_lines};
use crate::projects::{self, CancelFlag, ProjectList, ScanMsg};
use crate::shells::{self, ShellInfo};

/// What the wizard tells its host. Events are rare (a handful per run), so the size of `Finished`
/// does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum OnboardingEvent {
    /// "Abrir Selachi": `config.toml`, the projects/favorites in `state.db` and the first-run
    /// flag are already written. The payload lets the host apply everything without re-reading.
    Finished {
        config: Config,
        projects: Vec<DiscoveredProject>,
        favorites: Vec<PathBuf>,
    },
    /// "Omitir por ahora" on the welcome step: nothing was changed; the first-run flag is written.
    Skipped,
    /// "Iniciar sesión": open a terminal tab and run `command`.
    OpenLoginTerminal { agent: AgentCli, command: String },
    /// The theme choice changed: re-theme the window behind the wizard to match.
    ThemePreview(Mode),
}

/// The slow, side-effecting things the wizard does, replaceable for tests and demos.
#[derive(Clone)]
pub struct Services {
    pub detect_agents: Arc<dyn Fn() -> Vec<DetectedAgent> + Send + Sync>,
    pub detect_shells: Arc<dyn Fn() -> Vec<ShellInfo> + Send + Sync>,
    /// Home directory used for the default project roots.
    pub home: Option<PathBuf>,
    /// The shark GIF; `None` resolves it next to the exe / in the repo.
    pub gif_path: Option<PathBuf>,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            detect_agents: Arc::new(agents_driver::detect),
            detect_shells: Arc::new(shells::detect_shells),
            home: projects::home_dir(),
            gif_path: None,
        }
    }
}

/// The decoded shark, alive only while the welcome step is on screen.
pub(crate) struct Shark {
    pub(crate) frames: Vec<SharkFrame>,
    pub(crate) ix: usize,
}

/// A root folder row of step 3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Root {
    pub(crate) path: PathBuf,
    pub(crate) checked: bool,
}

pub struct OnboardingView {
    pub(crate) theme: Theme,
    pub(crate) system_dark: bool,
    pub(crate) base: Config,
    pub(crate) draft: Draft,
    pub(crate) flow: Flow,
    pub(crate) db: Option<StateDb>,
    pub(crate) data_dir: PathBuf,
    pub(crate) services: Services,
    pub(crate) focus: FocusHandle,
    pub(crate) scroll: ScrollHandle,
    pub(crate) list_scroll: UniformListScrollHandle,
    pub(crate) window_ready: bool,
    pub(crate) reduced_motion: bool,
    /// Import / save messages shown on the welcome and last steps.
    pub(crate) notice: Option<String>,
    pub(crate) finishing: bool,
    pub(crate) last_error: Option<String>,

    pub(crate) shark: Option<Shark>,
    pub(crate) shark_loading: bool,
    pub(crate) shark_failed: bool,
    shark_task: Option<Task<()>>,

    pub(crate) agents: Vec<AgentRow>,
    agents_started: bool,
    detect_task: Option<Task<()>>,
    pub(crate) copied: Option<AgentCli>,
    copied_task: Option<Task<()>>,

    pub(crate) roots: Vec<Root>,
    pub(crate) projects: ProjectList,
    pub(crate) scan_running: bool,
    scan_started: bool,
    scan_cancel: CancelFlag,
    scan_task: Option<Task<()>>,

    pub(crate) shells: Option<Vec<ShellInfo>>,
    shells_started: bool,
    shells_task: Option<Task<()>>,

    finish_task: Option<Task<()>>,
    _appearance_sub: Option<Subscription>,
}

impl EventEmitter<OnboardingEvent> for OnboardingView {}

impl Focusable for OnboardingView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for OnboardingView {
    fn drop(&mut self) {
        self.scan_cancel.cancel();
    }
}

/// Create the wizard, starting on the welcome step.
///
/// `config` is the current configuration (the wizard's starting point and the base its result is
/// built on); `db` receives the imported projects and favorites and locates the first-run flag.
pub fn onboarding(
    config: Config,
    db: Option<StateDb>,
    theme: Theme,
    cx: &mut App,
) -> Entity<OnboardingView> {
    onboarding_with(config, db, theme, Services::default(), Step::Welcome, cx)
}

/// [`onboarding`] with replaceable services and a starting step (tests, demo).
pub fn onboarding_with(
    config: Config,
    db: Option<StateDb>,
    theme: Theme,
    services: Services,
    start: Step,
    cx: &mut App,
) -> Entity<OnboardingView> {
    cx.new(|cx| OnboardingView::new(config, db, theme, services, start, cx))
}

impl OnboardingView {
    pub(crate) fn new(
        config: Config,
        db: Option<StateDb>,
        theme: Theme,
        services: Services,
        start: Step,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::init(cx);
        let data_dir = db
            .as_ref()
            .map(flag::data_dir_of)
            .or_else(|| store::paths::app_data_dir_path().ok())
            .unwrap_or_default();
        let draft = Draft::from_config(&config);
        let roots = services
            .home
            .as_deref()
            .map(projects::default_roots)
            .unwrap_or_default()
            .into_iter()
            .map(|path| Root {
                path,
                checked: true,
            })
            .collect();
        let reduced_motion = ui_kit::reduced_motion() || config.layout.resolve().reduced_motion;
        Self {
            theme,
            system_dark: theme.mode != Mode::Light,
            base: config,
            draft,
            flow: Flow::new(start),
            db,
            data_dir,
            services,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            list_scroll: UniformListScrollHandle::new(),
            window_ready: false,
            reduced_motion,
            notice: None,
            finishing: false,
            last_error: None,
            shark: None,
            shark_loading: false,
            shark_failed: false,
            shark_task: None,
            agents: agents::checking_rows(),
            agents_started: false,
            detect_task: None,
            copied: None,
            copied_task: None,
            roots,
            projects: ProjectList::new(),
            scan_running: false,
            scan_started: false,
            scan_cancel: CancelFlag::default(),
            scan_task: None,
            shells: None,
            shells_started: false,
            shells_task: None,
            finish_task: None,
            _appearance_sub: None,
        }
    }

    // ---- public accessors --------------------------------------------------------------------

    pub fn step(&self) -> Step {
        self.flow.step()
    }

    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    pub fn draft_mut(&mut self) -> &mut Draft {
        &mut self.draft
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// The outcome if the wizard finished now.
    pub fn outcome(&self) -> Outcome {
        let skipped = self.flow.skipped();
        let projects_skipped = self.flow.is_skipped(Step::Projects);
        Outcome {
            config: self.draft.build_config(&self.base, skipped),
            projects: if projects_skipped {
                Vec::new()
            } else {
                self.projects.selected()
            },
            favorites: if projects_skipped {
                Vec::new()
            } else {
                self.projects.favorites()
            },
        }
    }

    /// Whether the shark frames are in memory (only on the welcome step).
    pub fn shark_loaded(&self) -> bool {
        self.shark.is_some()
    }

    /// The error of the last failed save, if any (the wizard still finished).
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn agent_rows(&self) -> &[AgentRow] {
        &self.agents
    }

    pub fn project_list(&self) -> &ProjectList {
        &self.projects
    }

    /// Give the wizard the keyboard focus (Enter = primary action).
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    /// The host re-themed the window (config hot reload): follow it unless the user is previewing.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    // ---- theme -------------------------------------------------------------------------------

    /// The theme of the current theme and accent choices.
    pub(crate) fn preview_theme(&self) -> Theme {
        Theme::for_mode(self.draft.theme.resolve(self.system_dark))
            .with_accent_str(&self.draft.accent)
    }

    pub(crate) fn restyle(&mut self, cx: &mut Context<Self>) {
        self.theme = self.preview_theme();
        cx.notify();
    }

    pub(crate) fn set_theme_choice(&mut self, choice: ThemeChoice, cx: &mut Context<Self>) {
        if self.draft.theme == choice {
            return;
        }
        self.draft.theme = choice;
        let before = self.theme.mode;
        self.restyle(cx);
        if self.theme.mode != before {
            cx.emit(OnboardingEvent::ThemePreview(self.theme.mode));
        }
    }

    /// First frame: focus, and follow the system appearance while "Sistema" is chosen.
    pub(crate) fn init_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.window_ready {
            return;
        }
        self.window_ready = true;
        window.focus(&self.focus);
        self.system_dark = !matches!(
            window.appearance(),
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
        );
        if self.draft.theme == ThemeChoice::System {
            self.theme = self.preview_theme();
        }
        self._appearance_sub = Some(cx.observe_window_appearance(window, |this, window, cx| {
            this.system_dark = !matches!(
                window.appearance(),
                gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
            );
            if this.draft.theme == ThemeChoice::System {
                let before = this.theme.mode;
                this.restyle(cx);
                if this.theme.mode != before {
                    cx.emit(OnboardingEvent::ThemePreview(this.theme.mode));
                }
            }
        }));
        self.enter_step(window, cx);
    }

    // ---- navigation --------------------------------------------------------------------------

    /// "Continuar" / "Empezar" / "Abrir Selachi".
    pub fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        let from = self.flow.step();
        let nav = self.flow.advance();
        self.apply_nav(from, nav, window, cx);
    }

    /// "Atrás".
    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        let from = self.flow.step();
        let nav = self.flow.back();
        self.apply_nav(from, nav, window, cx);
    }

    /// "Omitir por ahora".
    pub fn skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        let from = self.flow.step();
        let nav = self.flow.skip();
        self.apply_nav(from, nav, window, cx);
    }

    /// Breadcrumb click: jump to a step already reached.
    pub fn go_to(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        let from = self.flow.step();
        let nav = self.flow.goto(step);
        self.apply_nav(from, nav, window, cx);
    }

    fn apply_nav(&mut self, from: Step, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        match nav {
            Nav::Moved(step) => {
                if from == Step::Welcome && step != Step::Welcome {
                    self.release_shark(window);
                }
                self.notice = None;
                self.scroll
                    .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
                self.enter_step(window, cx);
                cx.notify();
            }
            Nav::Finish => self.finish(window, cx),
            Nav::SkipAll => self.skip_all(window, cx),
            Nav::Stay => {}
        }
    }

    /// Start whatever the current step needs.
    fn enter_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.flow.step() {
            Step::Welcome => self.start_shark(window, cx),
            Step::Agents => self.ensure_detection(window, cx),
            Step::Projects => self.ensure_scan(window, cx),
            Step::Terminal => self.ensure_shells(window, cx),
            Step::Appearance | Step::Done => {}
        }
        // The CLI probes are cheap and finish while the user reads the welcome screen. WSL (it can
        // wake a VM) and the disk scan wait for their own step.
        self.ensure_detection(window, cx);
    }

    // ---- finishing ---------------------------------------------------------------------------

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finishing = true;
        cx.notify();
        let outcome = self.outcome();
        let dir = self.data_dir.clone();
        let db = self.db.clone();
        self.finish_task = Some(cx.spawn_in(window, async move |this, cx| {
            let to_save = outcome.clone();
            let saved = cx
                .background_executor()
                .spawn(async move { crate::model::persist(&dir, &to_save, db.as_ref()) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.last_error = saved.err();
                this.shutdown(window);
                cx.emit(OnboardingEvent::Finished {
                    config: outcome.config,
                    projects: outcome.projects,
                    favorites: outcome.favorites,
                });
                cx.notify();
            });
        }));
    }

    fn skip_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finishing = true;
        cx.notify();
        let dir = self.data_dir.clone();
        let db = self.db.clone();
        self.finish_task = Some(cx.spawn_in(window, async move |this, cx| {
            let marked = cx
                .background_executor()
                .spawn(async move { flag::mark_done_with(db.as_ref(), &dir) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = marked {
                    log::warn!("onboarding: cannot write the first-run flag: {e}");
                    this.last_error = Some(e);
                }
                this.shutdown(window);
                cx.emit(OnboardingEvent::Skipped);
                cx.notify();
            });
        }));
    }

    /// Stop everything and free the memory: scan cancelled, tasks dropped, GIF frames released.
    pub(crate) fn shutdown(&mut self, window: &mut Window) {
        self.scan_cancel.cancel();
        self.scan_task = None;
        self.scan_running = false;
        self.detect_task = None;
        self.shells_task = None;
        self.copied_task = None;
        self.release_shark(window);
        self.projects = ProjectList::new();
    }

    // ---- shark -------------------------------------------------------------------------------

    fn start_shark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shark.is_some() || self.shark_loading {
            return;
        }
        let Some(path) = self.services.gif_path.clone().or_else(gif::resolve_path) else {
            self.shark_failed = true;
            return;
        };
        self.shark_loading = true;
        self.shark_failed = false;
        let animate = !self.reduced_motion;
        self.shark_task = Some(cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { gif::load(&path) })
                .await;
            let started = this.update_in(cx, |this, _window, cx| {
                this.shark_loading = false;
                if this.flow.step() != Step::Welcome {
                    return false; // the user already left: drop the frames right here
                }
                match loaded {
                    Ok(mut frames) => {
                        if !animate {
                            frames.truncate(1);
                        }
                        let many = frames.len() > 1;
                        this.shark = Some(Shark { frames, ix: 0 });
                        cx.notify();
                        many
                    }
                    Err(e) => {
                        log::warn!("onboarding: cannot load the shark: {e}");
                        this.shark_failed = true;
                        cx.notify();
                        false
                    }
                }
            });
            if !matches!(started, Ok(true)) {
                return;
            }
            // Advance by each frame's own delay, only while the step is shown and the window is
            // active; the loop ends (and with it every wake-up) when the step is left.
            // While the window is inactive the frame does not advance and the loop only
            // re-checks once per second (instead of every frame delay), so an unfocused
            // wizard costs ~nothing.
            const INACTIVE_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);
            let mut active = true;
            loop {
                let delay = this
                    .read_with(cx, |this, _| {
                        this.shark.as_ref().map(|s| s.frames[s.ix].delay)
                    })
                    .ok()
                    .flatten();
                let Some(delay) = delay else { break };
                let wait = if active { delay } else { INACTIVE_RECHECK };
                cx.background_executor().timer(wait).await;
                let state = this.update_in(cx, |this, window, cx| {
                    if this.flow.step() != Step::Welcome {
                        return None;
                    }
                    let shark = this.shark.as_mut()?;
                    let is_active = window.is_window_active();
                    if is_active {
                        shark.ix = (shark.ix + 1) % shark.frames.len();
                        cx.notify();
                    }
                    Some(is_active)
                });
                match state {
                    Ok(Some(is_active)) => active = is_active,
                    _ => break,
                }
            }
        }));
    }

    /// Free the frames (CPU copies and GPU atlas) and stop the animation.
    pub(crate) fn release_shark(&mut self, window: &mut Window) {
        self.shark_task = None;
        self.shark_loading = false;
        if let Some(shark) = self.shark.take() {
            for frame in shark.frames {
                let _ = window.drop_image(frame.image);
            }
        }
    }

    // ---- agents ------------------------------------------------------------------------------

    pub(crate) fn ensure_detection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.agents_started {
            self.start_detection(window, cx);
        }
    }

    /// Run `detect()` on the background executor (also behind "Volver a comprobar").
    pub(crate) fn start_detection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agents_started = true;
        self.agents = agents::checking_rows();
        let detect = self.services.detect_agents.clone();
        self.detect_task = Some(cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { detect() })
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                this.agents = agents::rows_from_detected(&found);
                this.detect_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn open_login(&mut self, cli: AgentCli, cx: &mut Context<Self>) {
        cx.emit(OnboardingEvent::OpenLoginTerminal {
            agent: cli,
            command: cli.login_command().to_string(),
        });
    }

    /// Copy the install command; the button says "Copiado" for two seconds.
    pub(crate) fn copy_install(
        &mut self,
        cli: AgentCli,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(cli.install_command()));
        self.copied = Some(cli);
        self.copied_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = None;
                this.copied_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    // ---- projects ----------------------------------------------------------------------------

    pub(crate) fn ensure_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.scan_started {
            self.start_scan(window, cx);
        }
    }

    /// (Re)start discovery over the checked roots; results stream into the list.
    pub(crate) fn start_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scan_cancel.cancel();
        self.scan_started = true;
        self.scan_cancel = CancelFlag::default();
        self.projects.clear_keep_choices();
        let roots: Vec<PathBuf> = self
            .roots
            .iter()
            .filter(|r| r.checked)
            .map(|r| r.path.clone())
            .collect();
        if roots.is_empty() {
            self.scan_running = false;
            self.scan_task = None;
            cx.notify();
            return;
        }
        self.scan_running = true;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let cancel = self.scan_cancel.clone();
        cx.background_executor()
            .spawn(async move { projects::scan_streaming(&roots, cancel.as_atomic(), &tx) })
            .detach();
        self.scan_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    match msg {
                        ScanMsg::Found(batch) => this.projects.push_batch(batch),
                        ScanMsg::Done => this.scan_running = false,
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    pub(crate) fn toggle_root(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.roots.get_mut(ix) {
            r.checked = !r.checked;
            self.start_scan(window, cx);
        }
    }

    /// "Añadir carpeta…": the native folder picker.
    pub(crate) fn add_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Añadir carpeta".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let mut added = false;
                for path in paths {
                    let key = store::paths::normalize_key(&path.to_string_lossy());
                    match this
                        .roots
                        .iter_mut()
                        .find(|r| store::paths::normalize_key(&r.path.to_string_lossy()) == key)
                    {
                        Some(existing) => {
                            added |= !existing.checked;
                            existing.checked = true;
                        }
                        None => {
                            this.roots.push(Root {
                                path,
                                checked: true,
                            });
                            added = true;
                        }
                    }
                }
                if added {
                    this.start_scan(window, cx);
                }
            });
        })
        .detach();
    }

    /// "Otra carpeta" for worktrees: pick one folder.
    pub(crate) fn pick_worktrees_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Carpeta de worktrees".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.draft.worktrees = WorktreeLocation::Custom(path);
                cx.notify();
            });
        })
        .detach();
    }

    // ---- shells ------------------------------------------------------------------------------

    fn ensure_shells(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shells_started {
            return;
        }
        self.shells_started = true;
        let detect = self.services.detect_shells.clone();
        self.shells_task = Some(cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { detect() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.shells = Some(found);
                this.shells_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    // ---- import ------------------------------------------------------------------------------

    /// "Importar configuración": read a `config.toml` from another machine as the new starting point.
    pub(crate) fn import_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Importar config.toml".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = path.clone();
            let parsed = cx
                .background_executor()
                .spawn(async move { Config::load_from(&read).map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |this, cx| match parsed {
                Ok(p) => {
                    this.draft = Draft::from_config(&p.config);
                    this.base = p.config;
                    this.notice = Some(format!("Configuración importada de {}", path.display()));
                    this.restyle(cx);
                }
                Err(e) => {
                    this.notice = Some(format!("No se pudo importar: {e}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---- summary -----------------------------------------------------------------------------

    pub(crate) fn shell_label(&self) -> String {
        match (&self.draft.shell, &self.shells) {
            (Some(s), _) => s.name.clone(),
            (None, Some(list)) => shells::auto_index(list)
                .map_or_else(|| "shell automático".to_string(), |i| list[i].name.clone()),
            (None, None) => "shell automático".to_string(),
        }
    }

    pub(crate) fn summary(&self) -> Vec<crate::model::SummaryLine> {
        summary_lines(
            &self.draft,
            self.flow.skipped(),
            &self.agents,
            ProjectCounts {
                selected: self.projects.checked_count(),
                favorites: self.projects.favorite_count(),
            },
            &self.shell_label(),
        )
    }
}
