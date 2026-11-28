//! Projects in the shell: adding a folder, creating a new one, taking one off the list, the project
//! that is selected (where every new terminal and chat opens), and the menu and dialog around it.
//!
//! The pure rules (names, canonical paths, what a duplicate is, the words of the destination) live
//! in [`crate::projects`]. Here is the state and the I/O: everything that touches the disk or the
//! state database runs on the background executor, and nothing runs while the app is idle (the
//! "missing folder" check happens when a project is added or listed and when the window gets the
//! focus back).
//!
//! A project the user adds is a favorite of the state database, so it stays in the sidebar with no
//! tab open. "Quitar de la lista" only forgets it: no file is ever deleted.

use super::*;

use std::path::Path;

use gpui::{Point, point};
use store::db::SELECTED_PROJECT;
use ui_kit::{Button, Card, CardTone, Checkbox, Icon, ListRow, TextField, TextFieldEvent};

use crate::grouping::{project_key, project_name};
use crate::paths::same_path;
use crate::projects::{self as proj, AddCheck, CreateError, Dest, Known};

/// Width of the add / project menus.
const MENU_W: f32 = 276.0;
/// Width of the "crear proyecto" dialog.
const DIALOG_W: f32 = 480.0;

/// What a menu entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    OpenExisting,
    CreateNew,
    NewTerminal(PathBuf),
    NewChat(PathBuf),
    Remove(PathBuf),
    /// Right click on a delegated thread of the sidebar (PLAN section 21):
    /// «Abrir hilo», «Ver en el mapa», «Cancelar» and «Archivar terminados».
    OpenChild(i64),
    ViewChildInMap(i64),
    CancelChild(i64),
    ArchiveFinished(Vec<i64>),
}

/// One row of a menu: `key` is the letter that picks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MenuItem {
    pub icon: Icon,
    pub title: &'static str,
    pub hint: &'static str,
    pub key: char,
    pub action: MenuAction,
}

/// An open menu (add project, or the menu of one project) and the row the keyboard is on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ProjectMenu {
    pub items: Vec<MenuItem>,
    pub at: Point<Pixels>,
    pub sel: usize,
}

/// «Agregar proyecto».
pub(super) fn add_items() -> Vec<MenuItem> {
    vec![
        MenuItem {
            icon: Icon::FolderOpen,
            title: "Abrir carpeta existente…",
            hint: "A · Elige una carpeta que ya tienes",
            key: 'a',
            action: MenuAction::OpenExisting,
        },
        MenuItem {
            icon: Icon::Plus,
            title: "Crear proyecto nuevo…",
            hint: "C · Nombre y ubicación",
            key: 'c',
            action: MenuAction::CreateNew,
        },
    ]
}

/// The menu of a project. `full` adds «Quitar de la lista»; a vanished folder only offers that.
pub(super) fn space_items(root: &Path, full: bool, missing: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if !missing {
        items.push(MenuItem {
            icon: Icon::Terminal,
            title: "Nueva terminal aquí",
            hint: "T · En esta carpeta",
            key: 't',
            action: MenuAction::NewTerminal(root.to_path_buf()),
        });
        items.push(MenuItem {
            icon: Icon::Agents,
            title: "Nuevo chat aquí",
            hint: "C · El agente trabaja en esta carpeta",
            key: 'c',
            action: MenuAction::NewChat(root.to_path_buf()),
        });
    }
    if full || missing {
        items.push(MenuItem {
            icon: Icon::Close,
            title: "Quitar de la lista",
            hint: "Q · No borra ningún archivo",
            key: 'q',
            action: MenuAction::Remove(root.to_path_buf()),
        });
    }
    items
}

/// The menu of a delegated thread listed under its parent: what can be done with it, and with its
/// finished siblings.
pub(super) fn child_items(thread: i64, running: bool, finished: Vec<i64>) -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem {
            icon: Icon::Agents,
            title: "Abrir hilo",
            hint: "A · La conversación del subagente",
            key: 'a',
            action: MenuAction::OpenChild(thread),
        },
        MenuItem {
            icon: Icon::Map,
            title: "Ver en el mapa",
            hint: "M · Su lugar en el mapa de subagentes",
            key: 'm',
            action: MenuAction::ViewChildInMap(thread),
        },
    ];
    if running {
        items.push(MenuItem {
            icon: Icon::Close,
            title: "Cancelar",
            hint: "C · Detiene la ejecución",
            key: 'c',
            action: MenuAction::CancelChild(thread),
        });
    }
    if !finished.is_empty() {
        items.push(MenuItem {
            icon: Icon::Archive,
            title: "Archivar terminados",
            hint: "T · Quita de la lista los que ya acabaron",
            key: 't',
            action: MenuAction::ArchiveFinished(finished),
        });
    }
    items
}

#[cfg(test)]
mod child_menu_tests {
    use super::*;

    #[test]
    fn the_menu_of_a_subagent_offers_cancel_only_while_it_runs_and_archive_when_something_ended() {
        let running = child_items(5, true, vec![]);
        let titles: Vec<&str> = running.iter().map(|i| i.title).collect();
        assert_eq!(titles, vec!["Abrir hilo", "Ver en el mapa", "Cancelar"]);
        let done = child_items(6, false, vec![6, 7]);
        let titles: Vec<&str> = done.iter().map(|i| i.title).collect();
        assert_eq!(titles, vec!["Abrir hilo", "Ver en el mapa", "Archivar terminados"]);
        assert_eq!(done[2].action, MenuAction::ArchiveFinished(vec![6, 7]));
        // every entry has its own key
        let keys: std::collections::HashSet<char> = done.iter().map(|i| i.key).collect();
        assert_eq!(keys.len(), done.len());
    }
}

/// The "crear proyecto nuevo" form.
pub(super) struct CreateDialog {
    pub name: Entity<TextField>,
    pub parent: PathBuf,
    pub git_init: bool,
    /// A message under the form (the folder exists, could not be created...).
    pub error: Option<String>,
    /// The folder already there: the dialog offers to open it instead.
    pub exists: Option<PathBuf>,
    pub busy: bool,
    _sub: Subscription,
}

/// What the shell remembers about projects.
pub(super) struct ProjectsState {
    /// The project new terminals and chats open in (`None`: the active tab's folder, as before).
    pub selected: Option<PathBuf>,
    /// Folders of the listed projects that were not found at the last check.
    pub missing: HashSet<PathBuf>,
    pub menu: Option<ProjectMenu>,
    pub create: Option<CreateDialog>,
    pub menu_focus: FocusHandle,
    persist: Option<Task<()>>,
    check: Option<Task<()>>,
}

impl ProjectsState {
    pub fn new(selected: Option<PathBuf>, menu_focus: FocusHandle) -> Self {
        Self {
            selected,
            missing: HashSet::new(),
            menu: None,
            create: None,
            menu_focus,
            persist: None,
            check: None,
        }
    }

    /// A menu or the dialog is on screen (the palette must not take the focus back).
    pub fn overlay_open(&self) -> bool {
        self.menu.is_some() || self.create.is_some()
    }
}

impl AppShell {
    // ------------------------------------------------------------------------------------
    // Selection and destination
    // ------------------------------------------------------------------------------------

    /// Where new terminals and chats go, given what is known about missing folders.
    pub(super) fn dest(&self) -> Dest {
        proj::resolve(self.projects.selected.as_deref(), &self.projects.missing)
    }

    /// Selects `project` (or clears the selection) and remembers it. Repaints only on a change.
    pub(crate) fn set_selected(&mut self, project: Option<PathBuf>, cx: &mut Context<Self>) {
        let same = match (&self.projects.selected, &project) {
            (Some(a), Some(b)) => same_path(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.projects.selected = project;
        self.persist_selected(cx);
        self.sync_chat_projects(cx);
        self.refresh(cx);
    }

    /// A click on a project header. A vanished folder cannot be a destination.
    pub(crate) fn select_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.projects.missing.iter().any(|m| same_path(m, &root)) || !root.is_dir() {
            self.projects.missing.insert(root.clone());
            self.show_hint(
                Hint::text(format!(
                    "La carpeta de «{}» ya no existe. Puedes quitarla de la lista.",
                    project_name(&root)
                )),
                cx,
            );
            return;
        }
        self.set_selected(Some(root), cx);
    }

    /// The user brought another tab forward: its project becomes the selected one. Not called when
    /// the session is restored (the saved selection wins). Tabs without a folder (settings) keep
    /// the selection as it is.
    pub(super) fn sync_selection_to_active(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = self.active_info(cx).and_then(|i| i.project_root.or(i.cwd)) {
            self.set_selected(Some(root), cx);
        }
    }

    /// The folder a new terminal opens in: the selected project, else the active tab's folder.
    /// A selected folder that vanished is reported once and falls back too.
    pub(super) fn new_terminal_cwd(&mut self, cx: &mut Context<Self>) -> Option<PathBuf> {
        match self.usable_destination(cx) {
            Some(dir) => Some(dir),
            None => self.active_cwd(cx),
        }
    }

    /// The project a new chat runs in: the selected project, else the active tab's project.
    pub(super) fn new_chat_project(&mut self, cx: &mut Context<Self>) -> Option<PathBuf> {
        match self.usable_destination(cx) {
            Some(dir) => Some(dir),
            None => self.active_info(cx).and_then(|i| i.project_root.or(i.cwd)),
        }
    }

    /// The selected project when its folder is still there. One `stat` at the moment of the
    /// action: a folder deleted since the last check must not become a tab's directory.
    fn usable_destination(&mut self, cx: &mut Context<Self>) -> Option<PathBuf> {
        let Dest::Project(dir) = self.dest() else {
            return None;
        };
        if dir.is_dir() {
            return Some(dir);
        }
        self.projects.missing.insert(dir.clone());
        self.show_hint(
            Hint::text(format!(
                "La carpeta de «{}» ya no existe: se abrió en la carpeta actual.",
                project_name(&dir)
            )),
            cx,
        );
        None
    }

    fn persist_selected(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.session.read(cx).db().cloned() else {
            return;
        };
        let value = self.projects.selected.clone();
        // A newer selection drops the older write if it has not started.
        self.projects.persist = Some(cx.background_executor().spawn(async move {
            let _ = match value {
                Some(p) => db.meta_set(SELECTED_PROJECT, &p.to_string_lossy()),
                None => db.meta_remove(SELECTED_PROJECT),
            };
        }));
    }

    /// Writes the selection now (the window is closing and a background write could be cut).
    pub(super) fn flush_selected(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.session.read(cx).db().cloned() else {
            return;
        };
        let _ = match &self.projects.selected {
            Some(p) => db.meta_set(SELECTED_PROJECT, &p.to_string_lossy()),
            None => db.meta_remove(SELECTED_PROJECT),
        };
    }

    /// Looks, off the UI thread, for listed projects whose folder is gone. Called when projects
    /// load, when one is added and when the window gets the focus back: never on a timer.
    pub(super) fn refresh_missing(&mut self, cx: &mut Context<Self>) {
        let mut paths: Vec<PathBuf> = self.spaces.known.iter().map(|k| k.path.clone()).collect();
        paths.extend(self.projects.selected.clone());
        if paths.is_empty() {
            if !self.projects.missing.is_empty() {
                self.projects.missing.clear();
                self.refresh(cx);
            }
            return;
        }
        self.projects.check = Some(cx.spawn(async move |this, cx| {
            let gone: HashSet<PathBuf> = cx
                .background_executor()
                .spawn(async move { paths.into_iter().filter(|p| !p.is_dir()).collect() })
                .await;
            this.update(cx, |shell, cx| {
                if shell.projects.missing != gone {
                    shell.projects.missing = gone;
                    shell.refresh(cx);
                }
            })
            .ok();
        }));
    }

    // ------------------------------------------------------------------------------------
    // Adding and removing
    // ------------------------------------------------------------------------------------

    /// Adds the folder `path` as a project (and selects it). Everything that can go wrong is
    /// answered with a hint: it does not exist, it is a file, it is already listed, it is inside a
    /// listed project.
    pub(crate) fn add_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let listed: Vec<PathBuf> = self
            .spaces
            .known
            .iter()
            .filter(|k| k.explicit)
            .map(|k| k.path.clone())
            .collect();
        cx.spawn(async move |this, cx| {
            let check = cx
                .background_executor()
                .spawn(async move { proj::check_add(&path, &listed) })
                .await;
            this.update(cx, |shell, cx| shell.finish_add(check, cx))
                .ok();
        })
        .detach();
    }

    /// The folders of a drop on the sidebar: each becomes a project. Files are explained.
    pub(crate) fn drop_on_sidebar(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let folders = crate::drop::folders(paths);
        if folders.is_empty() {
            self.show_hint(
                Hint::text("Suelta una carpeta para agregarla como proyecto"),
                cx,
            );
            return;
        }
        for f in folders {
            self.add_project(f, cx);
        }
    }

    fn finish_add(&mut self, check: AddCheck, cx: &mut Context<Self>) {
        match check {
            AddCheck::Add(path) => self.commit_add(path, cx),
            AddCheck::Duplicate(p) => {
                let name = project_name(&p);
                self.set_selected(Some(p), cx);
                self.show_hint(Hint::text(format!("«{name}» ya está en la lista")), cx);
            }
            AddCheck::Inside(parent) => {
                let name = project_name(&parent);
                self.set_selected(Some(parent), cx);
                self.show_hint(
                    Hint::text(format!("Esa carpeta ya está dentro de «{name}»")),
                    cx,
                );
            }
            AddCheck::Missing => {
                self.show_hint(Hint::text("Esa carpeta no existe"), cx);
            }
            AddCheck::NotFolder => {
                self.show_hint(Hint::text("Eso es un archivo: elige una carpeta"), cx);
            }
        }
    }

    /// Lists `path` (a canonical folder) as a project: now in the sidebar, then in the database.
    fn commit_add(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = project_name(&path);
        match self
            .spaces
            .known
            .iter_mut()
            .find(|k| same_path(&k.path, &path))
        {
            // A recent project the user now adds on purpose.
            Some(k) => {
                k.explicit = true;
                k.path.clone_from(&path);
            }
            None => {
                let at = self
                    .spaces
                    .known
                    .iter()
                    .position(|k| !k.explicit)
                    .unwrap_or(self.spaces.known.len());
                self.spaces
                    .known
                    .insert(at, Known::new(path.clone(), String::new(), true));
            }
        }
        // Explicit ones stay together, in the order they were added.
        self.spaces.known.sort_by_key(|k| !k.explicit);
        if let Some(db) = self.session.read(cx).db().cloned() {
            let p = path.clone();
            cx.background_executor()
                .spawn(async move {
                    let shown = p.to_string_lossy().into_owned();
                    let _ = db.add_favorite(&shown, "");
                    // The palette's project picker knows it too.
                    let found = store::detect_project(&p).unwrap_or(store::DiscoveredProject {
                        name: project_name(&p),
                        path: p,
                        kind: store::ProjectKind::Other,
                        is_git: false,
                    });
                    let _ = db.save_projects(&[found]);
                })
                .detach();
        }
        self.projects.missing.retain(|m| !same_path(m, &path));
        self.set_selected(Some(path), cx);
        self.sync_chat_projects(cx);
        self.refresh_missing(cx);
        self.show_hint(
            Hint::text(format!(
                "Proyecto «{name}» agregado · las nuevas terminales y chats se abren ahí"
            )),
            cx,
        );
        self.refresh(cx);
    }

    /// «Quitar de la lista»: forgets the project. Its files are never touched.
    pub(crate) fn remove_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let name = project_name(&root);
        self.spaces.known.retain(|k| !same_path(&k.path, &root));
        self.spaces.folded.remove(&root);
        self.projects.missing.retain(|m| !same_path(m, &root));
        let has_tabs = self
            .infos(cx)
            .iter()
            .any(|i| project_key(i).is_some_and(|k| same_path(&k, &root)));
        if self
            .projects
            .selected
            .as_ref()
            .is_some_and(|s| same_path(s, &root))
        {
            self.set_selected(None, cx);
        }
        if let Some(db) = self.session.read(cx).db().cloned() {
            cx.background_executor()
                .spawn(async move {
                    let shown = root.to_string_lossy().into_owned();
                    let _ = db.remove_favorite(&shown);
                    let _ = db.remove_project(&shown);
                })
                .detach();
        }
        self.sync_chat_projects(cx);
        let tail = if has_tabs {
            " · sigue visible mientras tenga pestañas abiertas"
        } else {
            ""
        };
        self.show_hint(
            Hint::text(format!(
                "«{name}» se quitó de la lista · tus archivos siguen donde estaban{tail}"
            )),
            cx,
        );
        self.refresh(cx);
    }

    /// The native folder picker; the chosen folder is added as a project.
    pub(crate) fn pick_project_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Agregar proyecto".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut picked))) = rx.await
                && let Some(p) = picked.pop()
            {
                this.update(cx, |shell, cx| shell.add_project(p, cx)).ok();
            }
        })
        .detach();
    }

    // ------------------------------------------------------------------------------------
    // Menus
    // ------------------------------------------------------------------------------------

    /// Where a menu opens when nothing was clicked (the palette, a shortcut): under the sidebar's
    /// "Espacios" header, or the top-left corner without a sidebar.
    fn default_menu_anchor(&self) -> Point<Pixels> {
        let l = &self.eff.layout;
        let left_w = Self::sidebar_zone_w(l, SidebarSide::Left);
        let x = Self::rail_w(l) + if left_w > 0.0 { 24.0 } else { 12.0 };
        let y = TITLEBAR_H
            + if Self::is_unified(l) && left_w > 0.0 {
                120.0
            } else {
                6.0
            };
        point(px(x), px(y))
    }

    pub(super) fn open_menu(
        &mut self,
        items: Vec<MenuItem>,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let at = at.unwrap_or_else(|| self.default_menu_anchor());
        self.prompts.new_tab_menu = false;
        self.projects.menu = Some(ProjectMenu { items, at, sel: 0 });
        self.chrome.titlebar.update(cx, |t, cx| t.close_menus(cx));
        window.focus(&self.projects.menu_focus);
        self.refresh(cx);
    }

    /// «Agregar proyecto…»: the two ways to get one.
    pub(crate) fn open_add_project_menu(
        &mut self,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(add_items(), at, window, cx);
    }

    /// The menu of a project header (right click, "+", "...", Shift+F10).
    pub(crate) fn open_space_menu(
        &mut self,
        root: PathBuf,
        at: Point<Pixels>,
        full: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let missing = self.projects.missing.iter().any(|m| same_path(m, &root));
        self.open_menu(space_items(&root, full, missing), Some(at), window, cx);
    }

    pub(super) fn close_project_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.menu.take().is_some() {
            self.refocus_active(window, cx);
            self.refresh(cx);
        }
    }

    fn refocus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(slot) = self.active_id().and_then(|id| self.slot(id)) {
            slot.handle.focus(window, cx);
        }
    }

    pub(super) fn run_menu_action(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.projects.menu = None;
        match action {
            MenuAction::OpenExisting => {
                self.refocus_active(window, cx);
                self.pick_project_folder(cx);
            }
            MenuAction::CreateNew => self.open_create_dialog(window, cx),
            MenuAction::NewTerminal(root) => {
                if root.is_dir() {
                    self.new_tab_in(root, window, cx);
                } else {
                    self.select_project(root, cx);
                }
            }
            MenuAction::NewChat(root) => {
                if root.is_dir() {
                    self.set_selected(Some(root.clone()), cx);
                    self.new_chat_tab(Some(root), window, cx);
                } else {
                    self.select_project(root, cx);
                }
            }
            MenuAction::Remove(root) => {
                self.refocus_active(window, cx);
                self.remove_project(root, cx);
            }
            MenuAction::OpenChild(id) => self.open_chat_thread(ThreadId(id), window, cx),
            MenuAction::ViewChildInMap(id) => self.open_agentmap_for_run(ThreadId(id), cx),
            MenuAction::CancelChild(id) => self.agentmap_cancel(id.to_string(), cx),
            MenuAction::ArchiveFinished(ids) => self.archive_threads(&ids, cx),
        }
        self.refresh(cx);
    }

    fn menu_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.projects.menu else {
            return;
        };
        let n = menu.items.len();
        match key {
            "escape" => self.close_project_menu(window, cx),
            "down" | "tab" => {
                menu.sel = (menu.sel + 1) % n;
                cx.notify();
            }
            "up" => {
                menu.sel = (menu.sel + n - 1) % n;
                cx.notify();
            }
            "enter" => {
                let action = menu.items[menu.sel.min(n - 1)].action.clone();
                self.run_menu_action(action, window, cx);
            }
            k => {
                let pick = k.chars().next().filter(|_| k.chars().count() == 1);
                if let Some(item) = pick.and_then(|c| menu.items.iter().find(|i| i.key == c)) {
                    let action = item.action.clone();
                    self.run_menu_action(action, window, cx);
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // «Crear proyecto nuevo»
    // ------------------------------------------------------------------------------------

    pub(crate) fn open_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme;
        let name = cx.new(|cx| TextField::new(theme, cx).placeholder("mi-proyecto"));
        let sub = cx.subscribe_in(
            &name,
            window,
            |this, field, ev: &TextFieldEvent, window, cx| match ev {
                TextFieldEvent::Submit => this.confirm_create(window, cx),
                TextFieldEvent::Cancel => this.close_create_dialog(window, cx),
                TextFieldEvent::Changed => {
                    let text = field.read(cx).text().to_string();
                    let msg = match proj::validate_name(&text) {
                        Err(proj::NameError::Empty) | Ok(_) => None,
                        Err(e) => Some(e.message()),
                    };
                    field.update(cx, |f, cx| f.set_error(msg, cx));
                    if let Some(d) = &mut this.projects.create {
                        d.error = None;
                        d.exists = None;
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        let parent = proj::default_parent(self.projects.selected.as_deref());
        self.projects.menu = None;
        self.projects.create = Some(CreateDialog {
            name: name.clone(),
            parent,
            git_init: false,
            error: None,
            exists: None,
            busy: false,
            _sub: sub,
        });
        name.read(cx).focus(window);
        self.refresh(cx);
    }

    pub(super) fn close_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.create.take().is_some() {
            self.refocus_active(window, cx);
            self.refresh(cx);
        }
    }

    fn pick_create_parent(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Ubicación del proyecto".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut picked))) = rx.await
                && let Some(p) = picked.pop()
            {
                this.update(cx, |shell, cx| {
                    if let Some(d) = &mut shell.projects.create {
                        d.parent = p;
                        d.error = None;
                        d.exists = None;
                    }
                    shell.refresh(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// «Crear»: the folder is made now (never over an existing one), then listed and selected.
    pub(crate) fn confirm_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &mut self.projects.create else {
            return;
        };
        if d.busy {
            return;
        }
        let text = d.name.read(cx).text().to_string();
        let name = match proj::validate_name(&text) {
            Ok(n) => n.to_string(),
            Err(e) => {
                let msg = e.message();
                d.name.update(cx, |f, cx| f.set_error(Some(msg), cx));
                d.name.read(cx).focus(window);
                return;
            }
        };
        d.busy = true;
        d.error = None;
        d.exists = None;
        let (parent, git) = (d.parent.clone(), d.git_init);
        self.refresh(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let made = proj::create_folder(&parent, &name)?;
                    let git_note = if git {
                        proj::git_init(&made).err()
                    } else {
                        None
                    };
                    Ok::<_, CreateError>((made, git_note))
                })
                .await;
            this.update_in(cx, |shell, window, cx| {
                shell.finish_create(result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_create(
        &mut self,
        result: Result<(PathBuf, Option<String>), CreateError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((path, git_note)) => {
                self.projects.create = None;
                self.refocus_active(window, cx);
                self.commit_add(path, cx);
                if let Some(why) = git_note {
                    self.show_hint(
                        Hint::text(format!("Proyecto creado, pero `git init` falló: {why}")),
                        cx,
                    );
                }
            }
            Err(e) => {
                if let Some(d) = &mut self.projects.create {
                    d.busy = false;
                    d.error = Some(e.message());
                    d.exists = match &e {
                        CreateError::Exists(p) => Some(p.clone()),
                        _ => None,
                    };
                    if let CreateError::Name(n) = &e {
                        let msg = n.message();
                        d.name.update(cx, |f, cx| f.set_error(Some(msg), cx));
                    }
                }
                self.refresh(cx);
            }
        }
    }

    /// The dialog's second way out: the folder is already there, so open it as a project.
    pub(super) fn open_existing_from_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(existing) = self.projects.create.as_ref().and_then(|d| d.exists.clone()) else {
            return;
        };
        self.close_create_dialog(window, cx);
        self.add_project(existing, cx);
    }

    // ------------------------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------------------------

    /// The menu or the dialog, over everything else (empty while neither is open).
    pub(super) fn project_overlays(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some(menu) = &self.projects.menu {
            out.push(self.project_menu_el(menu, window, cx));
        }
        if let Some(d) = &self.projects.create {
            out.push(self.create_dialog_el(d, cx));
        }
        out
    }

    fn project_menu_el(
        &self,
        menu: &ProjectMenu,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let vp = window.viewport_size();
        let h = menu.items.len() as f32 * 52.0 + 10.0;
        let x = f32::from(menu.at.x)
            .min(f32::from(vp.width) - MENU_W - 8.0)
            .max(8.0);
        let y = f32::from(menu.at.y)
            .min(f32::from(vp.height) - h - 8.0)
            .max(8.0);
        let (scrim, keys) = (cx.entity(), cx.entity());
        let mut card = Card::new(&theme)
            .tone(CardTone::High, &theme)
            .radius(Radius::MD)
            .p(px(4.0))
            .flex()
            .flex_col();
        for (i, item) in menu.items.iter().enumerate() {
            let shell = cx.entity();
            let action = item.action.clone();
            card = card.child(
                ListRow::new(("project-menu", i), &theme)
                    .icon(item.icon)
                    .title(item.title)
                    .subtitle(item.hint)
                    .selected(i == menu.sel)
                    .on_click(move |_, window, cx| {
                        let a = action.clone();
                        shell.update(cx, |s, cx| s.run_menu_action(a, window, cx));
                    }),
            );
        }
        div()
            .absolute()
            .inset_0()
            .id("project-menu-scrim")
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                scrim.update(cx, |s, cx| s.close_project_menu(window, cx));
            })
            .child(
                div()
                    .id("project-menu")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(MENU_W))
                    .track_focus(&self.projects.menu_focus)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(move |e: &gpui::KeyDownEvent, window, cx| {
                        let key = e.keystroke.key.clone();
                        keys.update(cx, |s, cx| s.menu_key(&key, window, cx));
                        cx.stop_propagation();
                    })
                    .child(card),
            )
            .into_any_element()
    }

    fn create_dialog_el(&self, d: &CreateDialog, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let (cancel, go, pick, git, keys, existing) = (
            cx.entity(),
            cx.entity(),
            cx.entity(),
            cx.entity(),
            cx.entity(),
            cx.entity(),
        );
        let typed = d.name.read(cx).text().trim().to_string();
        let preview = if typed.is_empty() {
            None
        } else {
            Some(proj::target_path(&d.parent, &typed))
        };
        let parent_shown = crate::paths::breadcrumb(&d.parent);
        let label = |t: &'static str| {
            div()
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .child(t)
        };
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("Nombre de la carpeta"))
                    .child(d.name.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("Ubicación"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(32.0))
                                    .px(px(10.0))
                                    .flex()
                                    .items_center()
                                    .rounded(px(Radius::CONTROL))
                                    .bg(c.surface_2)
                                    .text_size(px(ty.ui_size_md))
                                    .text_color(c.text)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(parent_shown)),
                            )
                            .child(Button::tonal("pc-parent", &theme, "Cambiar…").on_click(
                                move |_, _, cx| pick.update(cx, |s, cx| s.pick_create_parent(cx)),
                            )),
                    ),
            )
            .child(
                Checkbox::new("pc-git", &theme)
                    .label("Inicializar un repositorio git (git init)")
                    .checked(d.git_init)
                    .on_change(move |on, _, cx| {
                        git.update(cx, |s, cx| {
                            if let Some(d) = &mut s.projects.create {
                                d.git_init = on;
                            }
                            cx.notify();
                        })
                    }),
            );
        if let Some(p) = preview {
            form = form.child(
                div()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.muted)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(format!(
                        "Se creará  {}",
                        crate::paths::middle_truncate(&crate::paths::breadcrumb(&p), 56)
                    ))),
            );
        }
        if let Some(e) = &d.error {
            form = form.child(
                div()
                    .text_size(px(ty.ui_size_sm))
                    .text_color(c.error)
                    .child(SharedString::from(e.clone())),
            );
        }
        let mut buttons = div().flex().flex_row().justify_end().gap(px(8.0)).child(
            Button::ghost("pc-cancel", &theme, "Cancelar").on_click(move |_, window, cx| {
                cancel.update(cx, |s, cx| s.close_create_dialog(window, cx))
            }),
        );
        if d.exists.is_some() {
            buttons = buttons.child(
                Button::tonal("pc-open-existing", &theme, "Abrir la carpeta existente").on_click(
                    move |_, window, cx| {
                        existing.update(cx, |s, cx| s.open_existing_from_dialog(window, cx))
                    },
                ),
            );
        }
        buttons = buttons.child(
            Button::primary(
                "pc-create",
                &theme,
                if d.busy {
                    "Creando…"
                } else {
                    "Crear proyecto"
                },
            )
            .disabled(d.busy)
            .on_click(move |_, window, cx| go.update(cx, |s, cx| s.confirm_create(window, cx))),
        );
        div()
            .id("create-project")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(Hsla::from(c.scrim))
            .on_key_down(move |e: &gpui::KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    keys.update(cx, |s, cx| s.close_create_dialog(window, cx));
                    cx.stop_propagation();
                }
            })
            .child(
                div()
                    .id("create-project-card")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                Card::new(&theme)
                    .radius(Radius::MD)
                    .p_4()
                    .w(px(DIALOG_W))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .text_size(px(ty.ui_size_lg))
                            .text_color(c.text)
                            .child("Crear proyecto nuevo"),
                    )
                    .child(
                        div()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.muted)
                            .child("La carpeta se crea al confirmar. Si ya existe una con ese nombre, no se toca."),
                    )
                    .child(form)
                    .child(buttons),
                    ),
            )
            .into_any_element()
    }
}

// ----------------------------------------------------------------------------------------------
// Scripted demo (screenshots without synthetic input)
// ----------------------------------------------------------------------------------------------

/// `SELACHI_DEMO=` values of the projects (use a throwaway `SELACHI_DATA_DIR`): `projects` (five
/// projects, one selected, one with its folder gone, tabs open in two of them), `projects-menu`
/// (the same with the "Agregar proyecto" menu open), `projects-space-menu` (the menu of a project),
/// `projects-create` (the "Crear proyecto nuevo" form filled in) and `projects-empty` (nothing
/// added: the sidebar's empty state).
pub(super) const DEMOS: [&str; 5] = [
    "projects",
    "projects-menu",
    "projects-space-menu",
    "projects-create",
    "projects-empty",
];

impl AppShell {
    pub(super) fn run_projects_demo(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !DEMOS.contains(&name) {
            return false;
        }
        if name == "projects-empty" {
            return true;
        }
        let Some(base) = super::data_dir() else {
            return true;
        };
        let root = base.join("demo-projects");
        let names = [
            "TRON Project",
            "selachi",
            "api-gateway",
            "docs-site",
            "old-prototype",
        ];
        let mut paths = Vec::new();
        for n in names {
            let p = root.join(n);
            if std::fs::create_dir_all(p.join(".git")).is_ok()
                && let Ok(c) = proj::canonical(&p)
            {
                paths.push(c);
            }
        }
        if paths.len() != names.len() {
            return true;
        }
        for p in &paths {
            self.commit_add(p.clone(), cx);
        }
        // A project whose folder is gone, listed muted.
        let _ = std::fs::remove_dir_all(&paths[4]);
        self.refresh_missing(cx);
        // Tabs in two projects; the one in front is not the selected project.
        self.new_tab_in(paths[0].clone(), window, cx);
        self.new_tab_in(paths[1].clone(), window, cx);
        self.set_selected(Some(paths[0].clone()), cx);
        self.dismiss_hint(cx);
        match name {
            "projects-menu" => self.open_add_project_menu(None, window, cx),
            "projects-space-menu" => {
                let at = self.default_menu_anchor();
                self.open_space_menu(paths[2].clone(), at, true, window, cx);
            }
            "projects-create" => {
                self.open_create_dialog(window, cx);
                if let Some(d) = &mut self.projects.create {
                    d.parent.clone_from(&root);
                    d.name
                        .update(cx, |f, cx| f.set_text("mi-nuevo-proyecto", cx));
                }
            }
            _ => {}
        }
        self.refresh(cx);
        true
    }
}
