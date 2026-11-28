//! Agentes -> Perfiles: the editor of subagent profiles (PLAN section 21.3).
//!
//! A list of the global and project profiles (with a source badge), a form with every field, a live
//! "Prompt final" preview, a dry run ("Prueba en seco": the final prompt and the limits that would
//! really apply; it runs nothing), import / export and the starter templates. Nothing is written to
//! disk without an explicit click: saving, deleting and creating the templates (which asks first).
//!
//! The permission of a profile can never exceed Delegación: options above the maximum are disabled
//! in the drop-down, and a profile that asks for more is clamped with a note. The files live where
//! PLAN says (`%APPDATA%\Selachi\agents\*.toml`, `<project>\.selachi\agents\*.toml`); the format and
//! its parser are in [`crate::profiles`] and must stay aligned with `agent_harness`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, prelude::*, px,
};
use theme::{Radius, Theme};
use ui_kit::{
    AgentMark, Button, EnterMode, GhostButton, Icon, Select, SelectEvent, SelectOption, TextField,
    TextFieldEvent, icon,
};

use crate::delegation_ui::DELEGATE_AGENTS;
use crate::profiles::{
    self, ContextFile, Effective, Entry, Issue, Limits, Profile, Scope, normalize_context_path,
};
use crate::ui::{self, PillTone, group, group_with_action, note, plain_row, row, status_pill};
use crate::validate::parse_number;

/// What the editor tells the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfilesEvent {
    /// A profile was written to this file.
    Saved(PathBuf),
    /// A profile file was deleted.
    Deleted(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirm {
    Delete,
    Templates(Vec<String>),
}

/// The profile being edited. Text fields live in their own entities; everything else is here.
#[derive(Clone, Debug)]
struct Draft {
    profile: Profile,
    scope: Scope,
    /// The file it was loaded from (renaming or moving it removes this one after the save).
    origin: Option<PathBuf>,
    dirty: bool,
}

struct SelectSlot {
    entity: Entity<Select>,
    options: Vec<(String, String, bool)>,
    _sub: Subscription,
}

/// The text inputs of the form.
struct Inputs {
    name: Entity<TextField>,
    description: Entity<TextField>,
    acp: Entity<TextField>,
    model: Entity<TextField>,
    timeout: Entity<TextField>,
    budget: Entity<TextField>,
    instructions: Entity<TextField>,
    allow: Entity<TextField>,
    deny: Entity<TextField>,
    /// New `allowed_commands` pattern.
    command: Entity<TextField>,
    context: Entity<TextField>,
    task: Entity<TextField>,
    import: Entity<TextField>,
}

const SAMPLE_TASK: &str = "Revisa el módulo de autenticación y resume los riesgos.";

pub struct ProfilesEditor {
    theme: Theme,
    data_dir: PathBuf,
    project_root: Option<PathBuf>,
    limits: Limits,
    narrow: bool,
    entries: Vec<Entry>,
    loaded: bool,
    draft: Option<Draft>,
    inputs: Inputs,
    selects: HashMap<&'static str, SelectSlot>,
    notice: Option<(String, bool)>,
    confirm: Option<Confirm>,
    attempted: bool,
    show_dry_run: bool,
    show_import: bool,
    add_errors: HashMap<&'static str, String>,
    files: Option<Rc<Vec<String>>>,
    files_for: Option<PathBuf>,
    suggestions: Vec<String>,
    /// The models each agent itself reported (`models-catalog.json`, read off the UI thread).
    catalog: agents_driver::catalog::Catalog,
    /// The agent whose list is being asked for right now («Actualizar lista»).
    models_busy: Option<String>,
    tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ProfilesEvent> for ProfilesEditor {}

fn field(theme: Theme, placeholder: &str, cx: &mut Context<ProfilesEditor>) -> Entity<TextField> {
    let placeholder = placeholder.to_string();
    cx.new(|cx| TextField::new(theme, cx).placeholder(placeholder))
}

/// The catalog name of a profile's agent (`None` for `acp:<command>`).
fn catalog_agent(agent: &str) -> Option<&'static str> {
    agents_driver::catalog::AGENTS
        .iter()
        .copied()
        .find(|a| a.eq_ignore_ascii_case(agent.trim()))
}

/// «consultada hace 5 min» for the age of a fetched list.
fn age_text(fetched_ms: i64) -> String {
    if fetched_ms <= 0 {
        return "nunca consultada".into();
    }
    let s = agents_driver::catalog::now_ms().saturating_sub(fetched_ms) / 1000;
    match s {
        0..=89 => "consultada ahora".into(),
        90..=5399 => format!("consultada hace {} min", s / 60),
        5400..=172_799 => format!("consultada hace {} h", s / 3600),
        _ => format!("consultada hace {} días", s / 86_400),
    }
}

impl ProfilesEditor {
    /// `data_dir` is `%APPDATA%\Selachi` (the global profiles live in `data_dir\agents`).
    pub fn new(theme: Theme, data_dir: impl Into<PathBuf>, cx: &mut Context<Self>) -> Self {
        let inputs = Inputs {
            name: field(theme, "Revisor de seguridad", cx),
            description: field(theme, "Qué hace y cuándo conviene usarlo", cx),
            acp: field(theme, "comando --acp", cx),
            model: field(theme, "por defecto del agente", cx),
            timeout: field(theme, "15", cx),
            budget: field(theme, "sin límite", cx),
            instructions: cx.new(|cx| {
                TextField::area(theme, cx)
                    .lines(6, 14)
                    .enter_mode(EnterMode::Newline)
                    .placeholder("Prompt de sistema o prefijo en Markdown")
            }),
            allow: field(theme, "Añadir herramienta permitida (Enter)", cx),
            deny: field(theme, "Añadir herramienta prohibida (Enter)", cx),
            command: field(
                theme,
                "Añadir comando permitido, p. ej. cargo * (Enter)",
                cx,
            ),
            context: field(theme, "Buscar un archivo del proyecto…", cx),
            task: cx.new(|cx| TextField::new(theme, cx).with_text(SAMPLE_TASK)),
            import: field(theme, "C:\\ruta\\perfil.toml o .claude\\agents\\x.md", cx),
        };
        let mut subs = Vec::new();
        let text_inputs: [(&'static str, Entity<TextField>); 8] = [
            ("name", inputs.name.clone()),
            ("description", inputs.description.clone()),
            ("acp", inputs.acp.clone()),
            ("model", inputs.model.clone()),
            ("timeout", inputs.timeout.clone()),
            ("budget_tokens", inputs.budget.clone()),
            ("instructions", inputs.instructions.clone()),
            ("task", inputs.task.clone()),
        ];
        for (key, f) in text_inputs {
            subs.push(cx.subscribe(&f, move |this, _, ev: &TextFieldEvent, cx| {
                if matches!(ev, TextFieldEvent::Changed) {
                    if key != "task"
                        && let Some(d) = this.draft.as_mut()
                    {
                        d.dirty = true;
                    }
                    cx.notify();
                }
            }));
        }
        for (list, f) in [
            ("allowed_tools", inputs.allow.clone()),
            ("denied_tools", inputs.deny.clone()),
            ("allowed_commands", inputs.command.clone()),
            ("context_files", inputs.context.clone()),
        ] {
            subs.push(
                cx.subscribe(&f, move |this, input, ev: &TextFieldEvent, cx| match ev {
                    TextFieldEvent::Submit => {
                        let text = input.read(cx).text().trim().to_string();
                        this.add_item(list, &text, cx);
                    }
                    TextFieldEvent::Changed if list == "context_files" => {
                        this.add_errors.remove("context_files");
                        this.refresh_suggestions(cx);
                        cx.notify();
                    }
                    TextFieldEvent::Changed => {
                        this.add_errors.remove(list);
                        cx.notify();
                    }
                    _ => {}
                }),
            );
        }
        Self {
            theme,
            data_dir: data_dir.into(),
            project_root: None,
            limits: Limits::default(),
            narrow: false,
            entries: Vec::new(),
            loaded: false,
            draft: None,
            inputs,
            selects: HashMap::new(),
            notice: None,
            confirm: None,
            attempted: false,
            show_dry_run: false,
            show_import: false,
            add_errors: HashMap::new(),
            files: None,
            files_for: None,
            suggestions: Vec::new(),
            catalog: agents_driver::catalog::Catalog::default(),
            models_busy: None,
            tasks: Vec::new(),
            _subs: subs,
        }
    }

    // ----- host API ---------------------------------------------------------------------------

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        for f in self.all_inputs() {
            f.update(cx, |f, cx| f.set_theme(theme, cx));
        }
        for s in self.selects.values() {
            s.entity.update(cx, |s, cx| s.set_theme(theme, cx));
        }
        cx.notify();
    }

    pub fn set_narrow(&mut self, narrow: bool, cx: &mut Context<Self>) {
        if self.narrow != narrow {
            self.narrow = narrow;
            cx.notify();
        }
    }

    /// The project the "Proyecto" scope points at (`None`: only global profiles).
    pub fn set_project(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.project_root != root {
            self.project_root = root;
            self.files = None;
            self.files_for = None;
            if self
                .draft
                .as_ref()
                .is_some_and(|d| d.scope == Scope::Project)
                && self.project_root.is_none()
                && let Some(d) = self.draft.as_mut()
            {
                d.scope = Scope::Global;
            }
            if self.loaded {
                self.reload(cx);
            }
            cx.notify();
        }
    }

    /// The Delegación limits (permission maximum, timeout, depth).
    pub fn set_limits(&mut self, limits: Limits, cx: &mut Context<Self>) {
        if self.limits != limits {
            self.limits = limits;
            self.clamp_draft_permission();
            cx.notify();
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(t, _)| t.as_str())
    }

    pub fn has_draft(&self) -> bool {
        self.draft.is_some()
    }

    /// Opens the profile named `name` (case-insensitive) for editing, as a click on its row does.
    pub fn open_profile(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        match self
            .entries
            .iter()
            .position(|e| e.title().eq_ignore_ascii_case(name))
        {
            Some(i) => {
                self.select_entry(i, cx);
                true
            }
            None => false,
        }
    }

    /// Shows or hides the dry run ("Prueba en seco").
    pub fn set_dry_run(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.show_dry_run != on {
            self.show_dry_run = on;
            cx.notify();
        }
    }

    /// Reads the profile folders off the UI thread (nothing is polled: the host calls this, and the
    /// editor itself after every write).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_catalog(cx);
        let global = profiles::global_dir(&self.data_dir);
        let project = self.project_root.as_deref().map(profiles::project_dir);
        self.tasks.push(cx.spawn(async move |this, cx| {
            let entries = cx
                .background_executor()
                .spawn(async move {
                    let mut all = profiles::scan_dir(&global, Scope::Global);
                    if let Some(p) = project {
                        all.extend(profiles::scan_dir(&p, Scope::Project));
                    }
                    all
                })
                .await;
            this.update(cx, |s, cx| {
                s.entries = entries;
                s.loaded = true;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Reads the cached model catalog (a file read: it never starts an agent).
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let dir = self.data_dir.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let catalog = cx
                .background_executor()
                .spawn(async move { agents_driver::catalog::load(&dir) })
                .await;
            this.update(cx, |s, cx| {
                s.catalog = catalog;
                cx.notify();
            })
            .ok();
        }));
    }

    /// «Actualizar lista»: asks the agent itself for its models (a background task, started only by
    /// this click) and stores the answer in the shared catalog.
    fn refresh_models(&mut self, agent: &str, cx: &mut Context<Self>) {
        if self.models_busy.is_some() {
            return;
        }
        self.models_busy = Some(agent.to_string());
        cx.notify();
        let (dir, name) = (self.data_dir.clone(), agent.to_string());
        self.tasks.push(cx.spawn(async move |this, cx| {
            let catalog = cx
                .background_executor()
                .spawn(async move {
                    agents_driver::catalog::refresh(
                        &dir,
                        &name,
                        agents_driver::catalog::FETCH_TIMEOUT,
                    );
                    agents_driver::catalog::load(&dir)
                })
                .await;
            this.update(cx, |s, cx| {
                s.catalog = catalog;
                s.models_busy = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Loads the list the first time the section is shown.
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if !self.loaded {
            self.reload(cx);
        }
    }

    /// Opens a new profile (optionally seeded: "Duplicar como perfil" from the map).
    pub fn start_new(&mut self, seed: Option<Profile>, cx: &mut Context<Self>) {
        let profile = seed.unwrap_or_default();
        self.open_draft(
            Draft {
                profile,
                scope: Scope::Global,
                origin: None,
                dirty: true,
            },
            cx,
        );
    }

    /// The profile as the form holds it now, and what is wrong with the numbers typed.
    pub fn form_profile(&self, cx: &App) -> (Profile, Vec<Issue>) {
        let Some(d) = &self.draft else {
            return (Profile::default(), Vec::new());
        };
        let text = |f: &Entity<TextField>| f.read(cx).text().to_string();
        let mut p = d.profile.clone();
        let mut issues = Vec::new();
        p.name = text(&self.inputs.name).trim().to_string();
        p.description = text(&self.inputs.description).trim().to_string();
        if p.agent.starts_with("acp:") || p.agent == "acp" {
            p.agent = format!("acp:{}", text(&self.inputs.acp).trim());
        }
        p.model = Some(text(&self.inputs.model).trim().to_string()).filter(|m| !m.is_empty());
        p.instructions = text(&self.inputs.instructions);
        let t = text(&self.inputs.timeout);
        p.timeout = if t.trim().is_empty() {
            None
        } else {
            match parse_number(&t) {
                Ok(min) if min > 0.0 => Some((min * 60.0).round() as u64),
                _ => {
                    issues.push(Issue {
                        field: "timeout",
                        message: "Escribe los minutos (por ejemplo 15 o 7,5).".into(),
                    });
                    None
                }
            }
        };
        let b = text(&self.inputs.budget);
        p.budget_tokens = if b.trim().is_empty() {
            None
        } else {
            match b.trim().replace(['.', ' '], "").parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    issues.push(Issue {
                        field: "budget_tokens",
                        message: "Escribe un número entero de tokens.".into(),
                    });
                    None
                }
            }
        };
        (p, issues)
    }

    /// Every problem of the form (typed numbers included).
    pub fn issues(&self, cx: &App) -> Vec<Issue> {
        let Some(d) = &self.draft else {
            return Vec::new();
        };
        let (p, mut issues) = self.form_profile(cx);
        issues.extend(profiles::validate(&p, &self.taken_names(d)));
        issues
    }

    fn taken_names(&self, d: &Draft) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.scope == d.scope && Some(&e.path) != d.origin.as_ref())
            .filter_map(|e| e.profile.as_ref().ok().map(|p| p.name.clone()))
            .collect()
    }

    // ----- draft ------------------------------------------------------------------------------

    fn all_inputs(&self) -> Vec<Entity<TextField>> {
        let i = &self.inputs;
        vec![
            i.name.clone(),
            i.description.clone(),
            i.acp.clone(),
            i.model.clone(),
            i.timeout.clone(),
            i.budget.clone(),
            i.instructions.clone(),
            i.allow.clone(),
            i.deny.clone(),
            i.command.clone(),
            i.context.clone(),
            i.task.clone(),
            i.import.clone(),
        ]
    }

    fn open_draft(&mut self, mut d: Draft, cx: &mut Context<Self>) {
        let p = &d.profile;
        let set = |f: &Entity<TextField>, t: &str, cx: &mut Context<Self>| {
            f.update(cx, |f, cx| f.set_text(t, cx))
        };
        set(&self.inputs.name, &p.name, cx);
        set(&self.inputs.description, &p.description, cx);
        set(
            &self.inputs.acp,
            p.agent.strip_prefix("acp:").unwrap_or(""),
            cx,
        );
        set(&self.inputs.model, p.model.as_deref().unwrap_or(""), cx);
        set(
            &self.inputs.timeout,
            &p.timeout
                .map(crate::delegation_ui::minutes_text)
                .unwrap_or_default(),
            cx,
        );
        set(
            &self.inputs.budget,
            &p.budget_tokens.map(|b| b.to_string()).unwrap_or_default(),
            cx,
        );
        set(&self.inputs.instructions, &p.instructions, cx);
        for f in [
            &self.inputs.allow,
            &self.inputs.deny,
            &self.inputs.command,
            &self.inputs.context,
        ] {
            set(f, "", cx);
        }
        if d.scope == Scope::Project && self.project_root.is_none() {
            d.scope = Scope::Global;
        }
        self.draft = Some(d);
        self.clamp_draft_permission();
        self.attempted = false;
        self.confirm = None;
        self.add_errors.clear();
        self.suggestions.clear();
        self.show_dry_run = false;
        cx.notify();
    }

    /// A profile never gets more than Delegación allows: lower it and say so.
    fn clamp_draft_permission(&mut self) {
        let max = self.limits.max_permission.clone();
        if let Some(d) = self.draft.as_mut() {
            let (perm, clamped) = profiles::clamp_permission(&d.profile.permission, &max);
            if clamped || d.profile.permission == "full" {
                d.profile.permission = perm.clone();
                d.dirty = true;
                self.notice = Some((
                    format!(
                        "El permiso se bajó a «{}»: es lo máximo que permite Delegación.",
                        profiles::permission_label(&perm)
                    ),
                    false,
                ));
            }
        }
    }

    fn select_entry(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get(i).cloned() else {
            return;
        };
        match e.profile {
            Ok(p) => {
                self.notice = None;
                self.open_draft(
                    Draft {
                        profile: p,
                        scope: e.scope,
                        origin: Some(e.path),
                        dirty: false,
                    },
                    cx,
                );
            }
            Err(msg) => {
                self.notice = Some((
                    format!("{} no se puede abrir: {msg}", e.path.display()),
                    true,
                ));
                cx.notify();
            }
        }
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Draft)) {
        if let Some(d) = self.draft.as_mut() {
            f(d);
            d.dirty = true;
        }
        cx.notify();
    }

    fn add_item(&mut self, list: &'static str, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() || self.draft.is_none() {
            return;
        }
        let value = if list == "context_files" {
            match normalize_context_path(text, self.project_root.as_deref()) {
                Some(v) => v,
                None => {
                    self.add_errors.insert(
                        list,
                        "Solo archivos dentro de la carpeta del proyecto.".to_string(),
                    );
                    cx.notify();
                    return;
                }
            }
        } else if list == "allowed_commands" {
            let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if let Some(why) = profiles::command_pattern_error(&one_line) {
                self.add_errors.insert(list, why.to_string());
                cx.notify();
                return;
            }
            one_line
        } else {
            text.to_string()
        };
        self.add_errors.remove(list);
        self.edit(cx, |d| {
            let v = match list {
                "allowed_tools" => &mut d.profile.allowed_tools,
                "denied_tools" => &mut d.profile.denied_tools,
                "allowed_commands" => &mut d.profile.allowed_commands,
                _ => &mut d.profile.context_files,
            };
            if !v.iter().any(|x| x.eq_ignore_ascii_case(&value)) {
                v.push(value);
            }
        });
        let input = match list {
            "allowed_tools" => &self.inputs.allow,
            "denied_tools" => &self.inputs.deny,
            "allowed_commands" => &self.inputs.command,
            _ => &self.inputs.context,
        };
        input.update(cx, |f, cx| f.set_text("", cx));
        self.suggestions.clear();
    }

    fn remove_item(&mut self, list: &'static str, i: usize, cx: &mut Context<Self>) {
        self.edit(cx, |d| {
            let v = match list {
                "allowed_tools" => &mut d.profile.allowed_tools,
                "denied_tools" => &mut d.profile.denied_tools,
                "allowed_commands" => &mut d.profile.allowed_commands,
                _ => &mut d.profile.context_files,
            };
            if i < v.len() {
                v.remove(i);
            }
        });
    }

    /// The picker: files of the project that match what was typed. The list of files is read once
    /// per project, off the UI thread.
    fn refresh_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.project_root.clone() else {
            self.suggestions.clear();
            return;
        };
        if self.files_for.as_ref() != Some(&root) {
            self.files_for = Some(root.clone());
            self.files = None;
            let r = root.clone();
            self.tasks.push(cx.spawn(async move |this, cx| {
                let files = cx
                    .background_executor()
                    .spawn(async move { profiles::project_files(&r, 2000) })
                    .await;
                this.update(cx, |s, cx| {
                    if s.files_for.as_ref() == Some(&root) {
                        s.files = Some(Rc::new(files));
                        s.update_suggestions(cx);
                        cx.notify();
                    }
                })
                .ok();
            }));
            return;
        }
        self.update_suggestions(cx);
    }

    fn update_suggestions(&mut self, cx: &App) {
        let q = self.inputs.context.read(cx).text().trim().to_lowercase();
        let have: Vec<String> = self
            .draft
            .as_ref()
            .map(|d| d.profile.context_files.clone())
            .unwrap_or_default();
        self.suggestions = match (&self.files, q.is_empty()) {
            (Some(files), false) => files
                .iter()
                .filter(|f| f.to_lowercase().contains(&q) && !have.contains(f))
                .take(8)
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
    }

    // ----- actions ----------------------------------------------------------------------------

    fn dir_for(&self, scope: Scope) -> Option<PathBuf> {
        match scope {
            Scope::Global => Some(profiles::global_dir(&self.data_dir)),
            Scope::Project => self.project_root.as_deref().map(profiles::project_dir),
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.draft.clone() else { return };
        self.attempted = true;
        let issues = self.issues(cx);
        if !issues.is_empty() {
            self.notice = Some((
                "Corrige los campos marcados antes de guardar.".to_string(),
                true,
            ));
            cx.notify();
            return;
        }
        let (mut p, _) = self.form_profile(cx);
        let (perm, _) = profiles::clamp_permission(&p.permission, &self.limits.max_permission);
        p.permission = perm;
        let Some(dir) = self.dir_for(d.scope) else {
            self.notice = Some(("No hay proyecto abierto para este ámbito.".into(), true));
            cx.notify();
            return;
        };
        let target = dir.join(format!("{}.toml", profiles::slug(&p.name)));
        if target.exists() && Some(&target) != d.origin.as_ref() {
            self.notice = Some((
                format!(
                    "Ya existe el archivo {}: usa otro nombre.",
                    target
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                true,
            ));
            cx.notify();
            return;
        }
        let origin = d.origin.clone();
        let scope = d.scope;
        self.notice = Some(("Guardando…".into(), false));
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let path = profiles::write_profile(&dir, &p)?;
                    // renamed or moved: the old file goes away only after the new one exists
                    if let Some(old) = origin
                        && old != path
                    {
                        let _ = std::fs::remove_file(old);
                    }
                    Ok::<_, std::io::Error>((path, p))
                })
                .await;
            this.update(cx, |s, cx| s.saved(result, scope, cx)).ok();
        }));
        cx.notify();
    }

    fn saved(
        &mut self,
        result: std::io::Result<(PathBuf, Profile)>,
        scope: Scope,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((path, profile)) => {
                self.notice = Some((format!("Guardado en {}", path.display()), false));
                self.draft = Some(Draft {
                    profile,
                    scope,
                    origin: Some(path.clone()),
                    dirty: false,
                });
                self.reload(cx);
                cx.emit(ProfilesEvent::Saved(path));
            }
            Err(e) => self.notice = Some((format!("No se pudo guardar: {e}"), true)),
        }
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        let Some(path) = self.draft.as_ref().and_then(|d| d.origin.clone()) else {
            self.draft = None;
            cx.notify();
            return;
        };
        self.tasks.push(cx.spawn(async move |this, cx| {
            let p = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::remove_file(&p) })
                .await;
            this.update(cx, |s, cx| {
                match result {
                    Ok(()) => {
                        s.draft = None;
                        s.notice = Some((format!("Se borró {}", path.display()), false));
                        s.reload(cx);
                        cx.emit(ProfilesEvent::Deleted(path));
                    }
                    Err(e) => s.notice = Some((format!("No se pudo borrar: {e}"), true)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn ask_templates(&mut self, cx: &mut Context<Self>) {
        let dir = profiles::global_dir(&self.data_dir);
        let missing: Vec<String> = profiles::templates()
            .into_iter()
            .filter(|t| {
                !dir.join(format!("{}.toml", profiles::slug(&t.name)))
                    .exists()
            })
            .map(|t| t.name)
            .collect();
        if missing.is_empty() {
            self.notice = Some(("Las plantillas iniciales ya existen.".into(), false));
        } else {
            self.confirm = Some(Confirm::Templates(missing));
        }
        cx.notify();
    }

    fn create_templates(&mut self, cx: &mut Context<Self>) {
        let Some(Confirm::Templates(names)) = self.confirm.take() else {
            return;
        };
        let dir = profiles::global_dir(&self.data_dir);
        let wanted: Vec<Profile> = profiles::templates()
            .into_iter()
            .filter(|t| names.contains(&t.name))
            .collect();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut n = 0usize;
                    for p in &wanted {
                        let path = dir.join(format!("{}.toml", profiles::slug(&p.name)));
                        // never overwrite what the user has
                        if !path.exists() {
                            profiles::write_profile(&dir, p)?;
                            n += 1;
                        }
                    }
                    Ok::<_, std::io::Error>(n)
                })
                .await;
            this.update(cx, |s, cx| {
                s.notice = Some(match result {
                    Ok(n) => (format!("Se crearon {n} plantillas."), false),
                    Err(e) => (format!("No se pudieron crear: {e}"), true),
                });
                s.reload(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Copies the TOML of the profile in the form to the clipboard.
    fn export(&mut self, cx: &mut Context<Self>) {
        let (p, _) = self.form_profile(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(profiles::to_toml(&p)));
        self.notice = Some(("TOML copiado al portapapeles.".into(), false));
        cx.notify();
    }

    fn import_text(&mut self, text: &str, fallback: &str, cx: &mut Context<Self>) {
        let parsed = if text.trim_start().starts_with("---") {
            profiles::from_claude_md(text, fallback)
        } else {
            profiles::from_toml(text)
        };
        match parsed {
            Ok(mut p) => {
                if p.name.trim().is_empty() {
                    p.name = fallback.to_string();
                }
                p.permission =
                    profiles::clamp_permission(&p.permission, &self.limits.max_permission).0;
                self.start_new(Some(p), cx);
                self.show_import = false;
                self.notice = Some((
                    "Importado como borrador: revísalo y pulsa Guardar.".into(),
                    false,
                ));
            }
            Err(e) => self.notice = Some((format!("No se pudo importar: {e}"), true)),
        }
        cx.notify();
    }

    fn import_clipboard(&mut self, cx: &mut Context<Self>) {
        match cx.read_from_clipboard().and_then(|c| c.text()) {
            Some(t) if !t.trim().is_empty() => self.import_text(&t, "Importado", cx),
            _ => {
                self.notice = Some(("El portapapeles no tiene texto.".into(), true));
                cx.notify();
            }
        }
    }

    fn import_file(&mut self, cx: &mut Context<Self>) {
        let path = PathBuf::from(self.inputs.import.read(cx).text().trim().trim_matches('"'));
        let ext_ok = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml") || e.eq_ignore_ascii_case("md"));
        if !ext_ok {
            self.notice = Some(("Elige un archivo .toml o .md.".into(), true));
            cx.notify();
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(t) => {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Importado".into());
                self.import_text(&t, &stem, cx);
            }
            Err(e) => {
                self.notice = Some((format!("No se pudo leer el archivo: {e}"), true));
                cx.notify();
            }
        }
    }

    // ----- small builders ---------------------------------------------------------------------

    fn btn(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &str,
        ghost: bool,
        f: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let this = cx.entity();
        let on = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            this.update(cx, |s, cx| f(s, cx));
        };
        if ghost {
            GhostButton::new(id, &self.theme, label.to_string())
                .on_click(on)
                .into_any_element()
        } else {
            Button::tonal(id, &self.theme, label.to_string())
                .on_click(on)
                .into_any_element()
        }
    }

    fn select(
        &mut self,
        cx: &mut Context<Self>,
        id: &'static str,
        options: Vec<(String, String, bool)>,
        selected: &str,
        f: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = self.theme;
        let to_opts = |o: &[(String, String, bool)]| -> Vec<SelectOption> {
            o.iter()
                .map(|(v, l, dis)| SelectOption::new(v.clone(), l.clone()).disabled(*dis))
                .collect()
        };
        let entity = match self.selects.get_mut(id) {
            Some(slot) => {
                if slot.options != options {
                    slot.options = options.clone();
                    slot.entity
                        .update(cx, |s, cx| s.set_options(to_opts(&options), cx));
                }
                slot.entity.update(cx, |s, cx| {
                    s.set_theme(theme, cx);
                    s.select_value(selected, cx);
                });
                slot.entity.clone()
            }
            None => {
                let entity = cx.new(|cx| {
                    Select::new(id, theme, cx)
                        .options(to_opts(&options))
                        .with_value(selected)
                        .width(220.0)
                });
                let sub = cx.subscribe(&entity, move |this, select, ev: &SelectEvent, cx| {
                    let SelectEvent::Changed(_) = ev;
                    if let Some(v) = select.read(cx).value().cloned() {
                        f(this, &v, cx);
                    }
                });
                self.selects.insert(
                    id,
                    SelectSlot {
                        entity: entity.clone(),
                        options,
                        _sub: sub,
                    },
                );
                entity
            }
        };
        entity.into_any_element()
    }

    fn input(&self, f: &Entity<TextField>, width: f32) -> AnyElement {
        div()
            .w(px(width))
            .max_w_full()
            .flex()
            .child(f.clone())
            .into_any_element()
    }

    fn chips(&self, cx: &mut Context<Self>, list: &'static str, items: &[String]) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let mut wrap = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.0));
        for (i, item) in items.iter().enumerate() {
            let this = cx.entity();
            wrap = wrap.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(24.0))
                    .pl(px(9.0))
                    .pr(px(4.0))
                    .rounded(px(Radius::CHIP))
                    .bg(c.surface_3)
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.text)
                    .child(SharedString::from(item.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("chip-x-{list}-{i}")))
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(Radius::CHIP))
                            .cursor_pointer()
                            .hover(|s| s.bg(c.surface_4))
                            .on_click(move |_, _, cx| {
                                this.update(cx, |s, cx| s.remove_item(list, i, cx))
                            })
                            .child(icon(Icon::Close, 12.0, c.muted)),
                    ),
            );
        }
        wrap.into_any_element()
    }
}

fn mono_preview(theme: &Theme, id: &'static str, text: &str) -> AnyElement {
    let c = theme.colors;
    let ty = theme.typography;
    div()
        .id(id)
        .w_full()
        .max_h(px(260.0))
        .overflow_y_scroll()
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(Radius::CONTROL))
        .bg(c.surface_2)
        .font_family(ty.mono_family)
        .text_size(px(ty.ui_size_sm))
        .text_color(c.text)
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

impl ProfilesEditor {
    fn list_group(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let mut rows: Vec<AnyElement> = Vec::new();
        if !self.loaded {
            rows.push(plain_row(note(&t, "Leyendo los perfiles…")).into_any_element());
        } else if self.entries.is_empty() {
            rows.push(
                plain_row(note(
                    &t,
                    "Aún no hay perfiles. Crea uno con «Nuevo perfil» o añade las plantillas iniciales.",
                ))
                .into_any_element(),
            );
        }
        let selected_path = self.draft.as_ref().and_then(|d| d.origin.clone());
        for (i, e) in self.entries.iter().enumerate() {
            let sel = selected_path.as_ref() == Some(&e.path);
            let (title, desc, mark) = match &e.profile {
                Ok(p) => (
                    e.title(),
                    p.description.clone(),
                    Some(AgentMark::from_name(p.agent.trim_start_matches("acp:"))),
                ),
                Err(msg) => (e.title(), format!("Archivo con errores: {msg}"), None),
            };
            let invalid = e.profile.is_err();
            let badge = status_pill(&t, e.scope.label(), PillTone::Neutral);
            rows.push(
                div()
                    .id(SharedString::from(format!("profile-row-{i}")))
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(8.0))
                    .mx(px(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(Radius::PILL))
                    .cursor_pointer()
                    .when(sel, |d| d.bg(c.surface_3))
                    .when(!sel, |d| d.hover(|s| s.bg(c.surface_2)))
                    .on_click(cx.listener(move |s, _, _, cx| s.select_entry(i, cx)))
                    .child(match mark {
                        Some(m) => ui_kit::agent_icon(m, 16.0, c.muted).into_any_element(),
                        None => icon(Icon::Alert, 16.0, c.error).into_any_element(),
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(ty.ui_size_md))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if invalid { c.error } else { c.text })
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(title)),
                            )
                            .when(!desc.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(px(ty.ui_size_sm))
                                        .text_color(c.muted)
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(SharedString::from(desc)),
                                )
                            }),
                    )
                    .child(badge)
                    .into_any_element(),
            );
        }

        // actions under the list
        let mut actions = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(6.0));
        actions = actions
            .child(self.btn(
                cx,
                "profiles-templates",
                "Plantillas iniciales",
                true,
                |s, cx| s.ask_templates(cx),
            ))
            .child(self.btn(cx, "profiles-import", "Importar", true, |s, cx| {
                s.show_import = !s.show_import;
                cx.notify();
            }))
            .child(
                self.btn(cx, "profiles-reload", "Volver a leer", true, |s, cx| {
                    s.reload(cx)
                }),
            );
        rows.push(actions.into_any_element());

        if let Some(Confirm::Templates(names)) = &self.confirm {
            let dir = profiles::global_dir(&self.data_dir);
            let text = format!(
                "Se crearán {} perfiles globales en {}: {}. No se sobrescribe nada.",
                names.len(),
                dir.display(),
                names.join(", ")
            );
            rows.push(
                plain_row(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(px(ty.ui_size_md))
                                .text_color(c.text)
                                .child(SharedString::from(text)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.0))
                                .child(self.btn(
                                    cx,
                                    "profiles-templates-go",
                                    "Crear plantillas",
                                    false,
                                    |s, cx| s.create_templates(cx),
                                ))
                                .child(self.btn(
                                    cx,
                                    "profiles-templates-no",
                                    "Cancelar",
                                    true,
                                    |s, cx| {
                                        s.confirm = None;
                                        cx.notify();
                                    },
                                )),
                        ),
                )
                .into_any_element(),
            );
        }
        if self.show_import {
            rows.push(
                plain_row(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(note(
                            &t,
                            "Importa un perfil .toml, un subagente de Claude Code (.claude/agents/*.md) o el TOML del portapapeles. Se abre como borrador; no se guarda hasta que pulses Guardar.",
                        ))
                        .child(self.input(&self.inputs.import.clone(), 520.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.0))
                                .child(self.btn(cx, "profiles-import-file", "Desde archivo", false, |s, cx| {
                                    s.import_file(cx)
                                }))
                                .child(self.btn(cx, "profiles-import-clip", "Desde el portapapeles", false, |s, cx| {
                                    s.import_clipboard(cx)
                                })),
                        ),
                )
                .into_any_element(),
            );
        }
        let new_btn = self.btn(cx, "profiles-new", "Nuevo perfil", false, |s, cx| {
            s.start_new(None, cx)
        });
        group_with_action(&t, "Perfiles", new_btn, rows).into_any_element()
    }

    fn form_groups(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let narrow = self.narrow;
        let Some(d) = self.draft.clone() else {
            return Vec::new();
        };
        let issues = if self.attempted || !d.profile.name.is_empty() || d.origin.is_some() {
            self.issues(cx)
        } else {
            // a fresh draft does not shout about the empty name until the first save
            self.issues(cx)
                .into_iter()
                .filter(|i| i.field != "name")
                .collect()
        };
        let (form, _) = self.form_profile(cx);
        let issue = |f: &str| -> Option<SharedString> {
            issues
                .iter()
                .find(|i| i.field == f)
                .map(|i| SharedString::from(i.message.clone()))
        };

        // ---- identity ----
        let scope_ix = usize::from(d.scope == Scope::Project);
        let this = cx.entity();
        let has_project = self.project_root.is_some();
        let scope_control: AnyElement = ui_kit::SegmentedControl::new(
            "profile-scope",
            &t,
            [SharedString::from("Global"), SharedString::from("Proyecto")],
        )
        .selected(scope_ix)
        .track(c.surface_2)
        .on_change(move |ix, _, cx| {
            this.update(cx, |s, cx| {
                let scope = if ix == 1 && s.project_root.is_some() {
                    Scope::Project
                } else {
                    Scope::Global
                };
                s.edit(cx, |d| d.scope = scope);
            })
        })
        .into_any_element();
        let scope_desc = match (&d.origin, has_project) {
            (_, false) => "Global (%APPDATA%\\Selachi\\agents). Abre un proyecto para guardar perfiles versionables.".to_string(),
            (_, true) => "Global: todos tus proyectos. Proyecto: .selachi\\agents, se puede versionar con git.".to_string(),
        };

        let agent_value = if d.profile.agent.starts_with("acp") {
            "acp".to_string()
        } else {
            d.profile.agent.clone()
        };
        let mut agent_opts: Vec<(String, String, bool)> = DELEGATE_AGENTS
            .iter()
            .map(|(id, label)| ((*id).to_string(), (*label).to_string(), false))
            .collect();
        agent_opts.push(("acp".into(), "Otro agente (ACP)".into(), false));
        let agent_sel = self.select(cx, "profile-agent", agent_opts, &agent_value, |s, v, cx| {
            let v = v.to_string();
            s.edit(cx, |d| {
                d.profile.agent = if v == "acp" { "acp:".into() } else { v }
            });
        });
        let is_acp = d.profile.agent.starts_with("acp");

        // models and efforts come from what the agent itself reported; without a list the fixed
        // levels and a free-text model apply
        let cat_agent = catalog_agent(&d.profile.agent);
        let listed = cat_agent
            .and_then(|a| self.catalog.get(a))
            .filter(|m| !m.models.is_empty())
            .cloned();
        let form_model = form.model.clone().unwrap_or_default();
        let offered: Vec<String> = match &listed {
            Some(m) => m.effort_options(Some(form_model.as_str()).filter(|m| !m.is_empty())),
            None => profiles::EFFORTS.iter().map(|e| (*e).to_string()).collect(),
        };
        let mut effort_opts: Vec<(String, String, bool)> =
            std::iter::once(("".to_string(), "Por defecto".to_string(), false))
                .chain(
                    offered
                        .iter()
                        .map(|e| (e.clone(), profiles::effort_label(e).to_string(), false)),
                )
                .collect();
        if let Some(cur) = d.profile.effort.as_deref().filter(|c| !c.is_empty())
            && !offered.iter().any(|o| o == cur)
        {
            effort_opts.push((
                cur.to_string(),
                format!("{} (el modelo no lo ofrece)", profiles::effort_label(cur)),
                false,
            ));
        }
        let effort_sel = self.select(
            cx,
            "profile-effort",
            effort_opts,
            d.profile.effort.as_deref().unwrap_or(""),
            |s, v, cx| {
                let v = Some(v.to_string()).filter(|v| !v.is_empty());
                s.edit(cx, |d| d.profile.effort = v);
            },
        );

        // ---- model: a Select over the agent's own list (or free text without one) + refresh ----
        let model_control: AnyElement = match &listed {
            Some(m) => {
                let mut opts: Vec<(String, String, bool)> =
                    vec![("".into(), "Por defecto del agente".into(), false)];
                for e in m.models.iter().filter(|e| e.available) {
                    let label = if e.label == e.id || e.label.is_empty() {
                        e.id.clone()
                    } else {
                        format!("{} · {}", e.label, e.id)
                    };
                    let label = if e.default {
                        format!("{label} (por defecto)")
                    } else {
                        label
                    };
                    opts.push((e.id.clone(), label, false));
                }
                if !form_model.is_empty() && m.find(&form_model).is_none() {
                    opts.push((
                        form_model.clone(),
                        format!("{form_model} (no listado)"),
                        false,
                    ));
                }
                let current = m
                    .find(&form_model)
                    .map(|(e, _)| e.id.clone())
                    .unwrap_or_else(|| form_model.clone());
                self.select(cx, "profile-model", opts, &current, |s, v, cx| {
                    let v = v.to_string();
                    s.inputs.model.update(cx, |f, cx| f.set_text(&v, cx));
                    // an effort the new model does not offer is dropped
                    let offered = s
                        .draft
                        .as_ref()
                        .and_then(|d| catalog_agent(&d.profile.agent))
                        .and_then(|a| s.catalog.get(a))
                        .map(|m| m.effort_options(Some(v.as_str()).filter(|x| !x.is_empty())));
                    s.edit(cx, |d| {
                        if let (Some(o), Some(e)) = (&offered, d.profile.effort.clone())
                            && !o.contains(&e)
                        {
                            d.profile.effort = None;
                        }
                    });
                })
            }
            None => self.input(&self.inputs.model.clone(), 240.0),
        };
        let models_note: String = match (cat_agent, &listed) {
            (None, _) => "Este agente no publica su lista de modelos: escribe el nombre.".into(),
            (Some(_), Some(m)) => format!(
                "Lista del propio agente ({}), {}.{}",
                m.source,
                age_text(m.fetched_ms),
                m.error
                    .as_deref()
                    .map(|e| format!(" Última actualización fallida: {e}"))
                    .unwrap_or_default()
            ),
            (Some(a), None) => match self.catalog.get(a).and_then(|m| m.error.as_deref()) {
                Some(e) => format!("No se pudo leer la lista: {e}"),
                None => "Sin lista todavía: pulsa «Actualizar lista» para preguntársela al agente."
                    .into(),
            },
        };
        let model_row: AnyElement = {
            let busy = self.models_busy.is_some();
            let refresh: Option<AnyElement> = cat_agent.map(|a| {
                let a = a.to_string();
                self.btn(
                    cx,
                    "profile-models-refresh",
                    if busy {
                        "Actualizando…"
                    } else {
                        "Actualizar lista"
                    },
                    true,
                    move |s, cx| s.refresh_models(&a, cx),
                )
            });
            let control = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .child(model_control)
                .children(refresh)
                .into_any_element();
            row(
                &t,
                narrow,
                "Modelo",
                Some(models_note.into()),
                control,
                None,
            )
            .into_any_element()
        };

        let max = self.limits.max_permission.clone();
        let perm_opts: Vec<(String, String, bool)> = profiles::PERMISSIONS
            .iter()
            .map(|p| {
                let above = profiles::clamp_permission(p, &max).1;
                let label = if above {
                    format!(
                        "{} (Delegación no lo permite)",
                        profiles::permission_label(p)
                    )
                } else {
                    profiles::permission_label(p).to_string()
                };
                ((*p).to_string(), label, above)
            })
            .collect();
        let perm_sel = self.select(
            cx,
            "profile-permission",
            perm_opts,
            &d.profile.permission,
            |s, v, cx| {
                let v = v.to_string();
                s.edit(cx, |d| d.profile.permission = v);
                s.clamp_draft_permission();
            },
        );
        let perm_note = {
            let (_, clamped) = profiles::clamp_permission(&d.profile.permission, &max);
            let base = format!(
                "Máximo de Delegación: {}. Nunca se supera.",
                profiles::permission_label(&max)
            );
            let _ = clamped;
            base
        };

        // Approvals: a profile can only lower Delegación's mode, so the higher ones are disabled.
        let global_approvals = profiles::normalize_approvals(&self.limits.approvals);
        let mut approvals_opts: Vec<(String, String, bool)> =
            vec![(String::new(), "Heredar de Delegación".to_string(), false)];
        approvals_opts.extend(profiles::APPROVAL_MODES.iter().map(|m| {
            let above = profiles::approvals_rank(m) > profiles::approvals_rank(global_approvals);
            let label = if above {
                format!(
                    "{} (Delegación no lo permite)",
                    profiles::approvals_label(m)
                )
            } else {
                profiles::approvals_label(m).to_string()
            };
            ((*m).to_string(), label, above)
        }));
        let approvals_sel = self.select(
            cx,
            "profile-approvals",
            approvals_opts,
            d.profile.approvals.as_deref().unwrap_or(""),
            |s, v, cx| {
                let v = Some(v.to_string()).filter(|v| !v.is_empty());
                s.edit(cx, |d| d.profile.approvals = v);
            },
        );
        let approvals_note = format!(
            "Delegación: {}. Un perfil solo puede bajarlo, nunca subirlo.",
            profiles::approvals_label(global_approvals)
        );

        let worktree = {
            let this = cx.entity();
            ui_kit::Toggle::new("profile-worktree", &t)
                .on(d.profile.worktree)
                .on_change(move |on, _, cx| {
                    this.update(cx, |s, cx| s.edit(cx, |d| d.profile.worktree = on))
                })
        };
        let fmt_opts: Vec<(String, String, bool)> =
            [("text", "Texto"), ("json", "JSON"), ("diff", "Diff")]
                .iter()
                .map(|(v, l)| ((*v).to_string(), (*l).to_string(), false))
                .collect();
        let fmt_sel = self.select(
            cx,
            "profile-format",
            fmt_opts,
            &d.profile.output_format,
            |s, v, cx| {
                let v = v.to_string();
                s.edit(cx, |d| d.profile.output_format = v);
            },
        );

        let mut identity_rows = vec![
            row(
                &t,
                narrow,
                "Nombre",
                None,
                self.input(&self.inputs.name.clone(), 320.0),
                issue("name"),
            )
            .into_any_element(),
            row(
                &t,
                narrow,
                "Descripción",
                Some("Cuándo debe elegir este perfil el agente que delega.".into()),
                self.input(&self.inputs.description.clone(), 420.0),
                None,
            )
            .into_any_element(),
            row(
                &t,
                narrow,
                "Ámbito",
                Some(scope_desc.into()),
                scope_control,
                None,
            )
            .into_any_element(),
            row(&t, narrow, "Agente", None, agent_sel, issue("agent")).into_any_element(),
        ];
        if is_acp {
            identity_rows.push(
                row(
                    &t,
                    narrow,
                    "Comando ACP",
                    Some("Ejecutable que habla ACP por stdio.".into()),
                    self.input(&self.inputs.acp.clone(), 320.0),
                    None,
                )
                .into_any_element(),
            );
        }
        identity_rows.extend([
            model_row,
            row(&t, narrow, "Esfuerzo", None, effort_sel, issue("effort")).into_any_element(),
            row(
                &t,
                narrow,
                "Permiso",
                Some(perm_note.into()),
                perm_sel,
                issue("permission"),
            )
            .into_any_element(),
            row(
                &t,
                narrow,
                "Worktree propio",
                Some("Con «Aceptar ediciones» siempre se usa uno.".into()),
                worktree,
                None,
            )
            .into_any_element(),
            row(
                &t,
                narrow,
                "Tiempo máximo (minutos)",
                Some("Nunca más que el de Delegación.".into()),
                self.input(&self.inputs.timeout.clone(), 100.0),
                issue("timeout"),
            )
            .into_any_element(),
            row(
                &t,
                narrow,
                "Presupuesto (tokens)",
                Some("Tokens facturables de una ejecución. Vacío = sin límite propio.".into()),
                self.input(&self.inputs.budget.clone(), 160.0),
                issue("budget_tokens"),
            )
            .into_any_element(),
            row(
                &t,
                narrow,
                "Formato de salida",
                None,
                fmt_sel,
                issue("output_format"),
            )
            .into_any_element(),
        ]);
        let title = if d.origin.is_some() {
            "Editar perfil"
        } else {
            "Perfil nuevo"
        };
        let identity = group(&t, Some(title), identity_rows).into_any_element();

        // ---- instructions ----
        let instructions = group(
            &t,
            Some("Instrucciones"),
            vec![plain_row(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(self.input(&self.inputs.instructions.clone(), 760.0))
                    .child(note(&t, "Markdown. Es el prompt de sistema (o el prefijo) que recibe el agente."))
                    .children(issue("instructions").map(|m| {
                        div().text_size(px(ty.ui_size_sm)).text_color(c.error).child(m)
                    })),
            )
            .into_any_element()],
        )
        .into_any_element();

        // ---- tools ----
        let tools = group(
            &t,
            Some("Herramientas"),
            vec![
                row(
                    &t,
                    narrow,
                    "Permitidas",
                    Some("Solo si el agente lo soporta. Vacío = las que traiga.".into()),
                    div()
                        .w(px(300.0))
                        .max_w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .items_end()
                        .child(self.chips(cx, "allowed_tools", &d.profile.allowed_tools))
                        .child(self.input(&self.inputs.allow.clone(), 300.0)),
                    None,
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Prohibidas",
                    None,
                    div()
                        .w(px(300.0))
                        .max_w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .items_end()
                        .child(self.chips(cx, "denied_tools", &d.profile.denied_tools))
                        .child(self.input(&self.inputs.deny.clone(), 300.0)),
                    issue("denied_tools"),
                )
                .into_any_element(),
            ],
        )
        .into_any_element();

        // ---- approvals ----
        let commands_err = self
            .add_errors
            .get("allowed_commands")
            .map(|m| SharedString::from(m.clone()))
            .or_else(|| issue("allowed_commands"));
        let approvals = group(
            &t,
            Some("Aprobaciones"),
            vec![
                row(
                    &t,
                    narrow,
                    "Modo",
                    Some(approvals_note.into()),
                    approvals_sel,
                    issue("approvals"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Comandos permitidos",
                    Some("Con «Solo comandos permitidos», lo que case se aprueba solo (patrones como `cargo *`, `git diff *`). Un comando con `;`, `|`, `&&` o `>` nunca casa; `git push`, `git commit` y `rm -rf` fuera del proyecto se rechazan siempre.".into()),
                    div()
                        .w(px(300.0))
                        .max_w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .items_end()
                        .child(self.chips(cx, "allowed_commands", &d.profile.allowed_commands))
                        .child(self.input(&self.inputs.command.clone(), 300.0)),
                    commands_err,
                )
                .into_any_element(),
            ],
        )
        .into_any_element();

        // ---- context files (only inside the project) ----
        let mut ctx_control = div()
            .w(px(360.0))
            .max_w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .items_end()
            .child(self.chips(cx, "context_files", &d.profile.context_files));
        if has_project {
            ctx_control = ctx_control.child(self.input(&self.inputs.context.clone(), 360.0));
            for (i, s) in self.suggestions.clone().into_iter().enumerate() {
                let value = s.clone();
                ctx_control =
                    ctx_control.child(
                        div()
                            .id(SharedString::from(format!("ctx-suggest-{i}")))
                            .w(px(360.0))
                            .max_w_full()
                            .px(px(10.0))
                            .py(px(5.0))
                            .rounded(px(Radius::CONTROL))
                            .bg(c.surface_2)
                            .hover(|st| st.bg(c.surface_3))
                            .cursor_pointer()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.text)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .on_click(cx.listener(move |st, _, _, cx| {
                                st.add_item("context_files", &value, cx)
                            }))
                            .child(SharedString::from(s)),
                    );
            }
        }
        let ctx_err = self
            .add_errors
            .get("context_files")
            .map(|m| SharedString::from(m.clone()))
            .or_else(|| issue("context_files"));
        let context = group(
            &t,
            Some("Contexto"),
            vec![row(
                &t,
                narrow,
                "Archivos adjuntos",
                Some(
                    if has_project {
                        "Solo archivos de este proyecto (rutas relativas). Se adjuntan al prompt."
                    } else {
                        "Abre un proyecto para elegir archivos de contexto."
                    }
                    .into(),
                ),
                ctx_control,
                ctx_err,
            )
            .into_any_element()],
        )
        .into_any_element();

        // ---- final prompt + dry run ----
        let files: Vec<ContextFile> = profiles::context_files(&form, self.project_root.as_deref());
        let task = self.inputs.task.read(cx).text().to_string();
        let prompt = profiles::final_prompt(
            &form,
            if task.trim().is_empty() {
                SAMPLE_TASK
            } else {
                &task
            },
            &files,
        );
        let mut preview_rows = vec![
            row(
                &t,
                narrow,
                "Tarea de ejemplo",
                Some("Solo para esta vista previa.".into()),
                self.input(&self.inputs.task.clone(), 420.0),
                None,
            )
            .into_any_element(),
            plain_row(mono_preview(&t, "profile-prompt", &prompt)).into_any_element(),
        ];
        if self.show_dry_run {
            let e = profiles::effective(&form, &self.limits);
            preview_rows.push(plain_row(self.dry_run_panel(&e, &files)).into_any_element());
        }
        let dry_btn = self.btn(
            cx,
            "profile-dry-run",
            if self.show_dry_run {
                "Ocultar la prueba en seco"
            } else {
                "Prueba en seco"
            },
            false,
            |s, cx| {
                s.show_dry_run = !s.show_dry_run;
                cx.notify();
            },
        );
        let preview =
            group_with_action(&t, "Prompt final", dry_btn, preview_rows).into_any_element();

        // ---- buttons ----
        let dirty = d.dirty;
        let mut bar = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(8.0));
        bar = bar.child(self.btn(cx, "profile-save", "Guardar", false, |s, cx| s.save(cx)));
        bar = bar.child(
            self.btn(cx, "profile-export", "Exportar TOML", true, |s, cx| {
                s.export(cx)
            }),
        );
        if d.origin.is_some() {
            bar = bar.child(
                self.btn(cx, "profile-duplicate", "Duplicar", true, |s, cx| {
                    let (mut p, _) = s.form_profile(cx);
                    p.name = format!("{} (copia)", p.name);
                    let scope = s.draft.as_ref().map_or(Scope::Global, |d| d.scope);
                    s.open_draft(
                        Draft {
                            profile: p,
                            scope,
                            origin: None,
                            dirty: true,
                        },
                        cx,
                    );
                }),
            );
            bar = bar.child(self.btn(cx, "profile-delete", "Eliminar", true, |s, cx| {
                s.confirm = Some(Confirm::Delete);
                cx.notify();
            }));
        }
        bar = bar.child(self.btn(cx, "profile-close", "Cerrar", true, |s, cx| {
            s.draft = None;
            s.confirm = None;
            cx.notify();
        }));
        if dirty {
            bar = bar.child(status_pill(&t, "Sin guardar", PillTone::Warn));
        }
        let mut footer = div().flex().flex_col().gap(px(10.0)).child(bar);
        if self.confirm == Some(Confirm::Delete) {
            let path = d
                .origin
                .clone()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            footer = footer.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .p(px(12.0))
                    .rounded(px(Radius::SM))
                    .bg(c.surface_2)
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.text)
                            .child(SharedString::from(format!("Se borrará el archivo {path}."))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.0))
                            .child(self.btn(
                                cx,
                                "profile-delete-go",
                                "Borrar el archivo",
                                false,
                                |s, cx| s.delete(cx),
                            ))
                            .child(
                                self.btn(cx, "profile-delete-no", "Cancelar", true, |s, cx| {
                                    s.confirm = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        }
        vec![
            identity,
            instructions,
            tools,
            approvals,
            context,
            preview,
            footer.into_any_element(),
        ]
    }

    /// What a run of the form's profile would really get.
    fn dry_run_panel(&self, e: &Effective, files: &[ContextFile]) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let line = |label: &str, value: String| -> AnyElement {
            div()
                .flex()
                .flex_row()
                .gap(px(10.0))
                .child(
                    div()
                        .w(px(150.0))
                        .flex_none()
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .child(SharedString::from(label.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(ty.ui_size_md))
                        .text_color(c.text)
                        .child(SharedString::from(value)),
                )
                .into_any_element()
        };
        let mut col = div()
            .id("profile-dry-run-panel")
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(ty.ui_size_xs))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.muted)
                    .child("Límites efectivos (no se ejecuta nada)"),
            )
            .child(line(
                "Permiso",
                profiles::permission_label(&e.permission).to_string(),
            ))
            .child(line(
                "Worktree",
                if e.worktree {
                    if e.worktree_forced {
                        "sí (obligatorio con «Aceptar ediciones»)".into()
                    } else {
                        "sí".into()
                    }
                } else {
                    "no".into()
                },
            ))
            .child(line(
                "Tiempo máximo",
                crate::delegation_ui::minutes_text(e.timeout_secs) + " min",
            ))
            .child(line(
                "Presupuesto",
                e.budget_tokens
                    .map_or("el de Delegación".to_string(), |b| format!("{b} tokens")),
            ))
            .child(line(
                "Aprobación",
                profiles::approvals_label(&e.approvals).to_string(),
            ))
            .child(line("Profundidad máxima", e.depth.to_string()));
        if !files.is_empty() {
            let missing = files.iter().filter(|f| f.bytes.is_none()).count();
            col = col.child(line(
                "Contexto",
                format!(
                    "{} archivo{}{}",
                    files.len(),
                    if files.len() == 1 { "" } else { "s" },
                    if missing > 0 {
                        format!(" ({missing} no existen)")
                    } else {
                        String::new()
                    }
                ),
            ));
        }
        for n in &e.notes {
            col = col.child(
                div()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .child(SharedString::from(format!("· {n}"))),
            );
        }
        col.into_any_element()
    }
}

impl Render for ProfilesEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        self.ensure_loaded(cx);
        let list = self.list_group(cx);
        let mut col = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(list);
        if let Some((text, err)) = self.notice.clone() {
            col = col.child(ui::banner(
                &t,
                if err { Icon::Alert } else { Icon::Check },
                if err { "No se pudo" } else { "Listo" },
                &text,
                None,
            ));
        }
        let _ = c;
        col.children(self.form_groups(cx))
    }
}

#[cfg(test)]
#[path = "profiles_ui_tests.rs"]
mod tests;
