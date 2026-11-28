//! Window tests of the projects: adding, creating, removing, the selected project and where new
//! terminals and chats go. The shell runs in a test window with fake tabs; the native folder
//! dialog is never opened (the tests call what its answer calls).

use super::*;

use std::collections::HashMap;
use std::path::Path;

use store::StateDb;
use ui_kit::testing;

use crate::paths::same_path;
use crate::projects::{self as proj, Dest, Known};
use crate::spaces::build_spaces;

/// A tab with a folder and no process.
struct CwdTab {
    cwd: PathBuf,
    body: Entity<Body>,
}

impl EventEmitter<TabEvent> for CwdTab {}

impl TabModel for CwdTab {
    fn title(&self, _: &App) -> SharedString {
        crate::grouping::project_name(&self.cwd).into()
    }
    fn icon(&self, _: &App) -> TabIcon {
        TabIcon::Kind(ui_kit::Icon::Terminal)
    }
    fn cwd(&self, _: &App) -> Option<PathBuf> {
        Some(self.cwd.clone())
    }
    fn content(&self, _: &App) -> AnyView {
        self.body.clone().into()
    }
}

fn db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(&dir.path().join("state.db")).unwrap();
    (dir, db)
}

/// The shell of `window::rig`, with the state database `db` and the sidebar's quick actions on.
fn rig_db(
    cx: &mut TestAppContext,
    db: Option<StateDb>,
) -> (Entity<AppShell>, &mut VisualTestContext) {
    isolate_data_dir();
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("f6", NextZone, None),
            KeyBinding::new("shift-f6", PrevZone, None),
            KeyBinding::new("ctrl-shift-e", FocusSidebar, None),
            KeyBinding::new("delete", CloseFocusedRow, Some("SidebarRow")),
            KeyBinding::new(
                "shift-f10",
                crate::sidebar_view::OpenSpaceMenu,
                Some("SidebarSpace"),
            ),
            KeyBinding::new("enter", ConfirmClose, Some("CloseConfirm")),
            KeyBinding::new("escape", CancelClose, Some("CloseConfirm")),
        ]);
        let mut registry = PanelRegistry::default();
        registry.register(crate::chat_host::ThreadsProvider);
        cx.set_global(registry);
        let services = Services::new(cx);
        cx.set_global(services);
    });
    let mut config = Config::default();
    config
        .layout
        .select_preset(store::BuiltinPreset::EspaciosUnificados.into());
    let boot = ConfigBoot {
        config: Arc::new(config),
        watcher: None,
        session: SessionBoot { db, restore: None },
        start_dir: None,
    };
    let theme = Theme::for_mode(Mode::Dark);
    let (shell, vcx) = testing::open_window(cx, |window, cx| {
        let (shell, _) = AppShell::build(theme, boot, window, cx);
        shell
    });
    (shell, vcx)
}

fn add_cwd_tab(shell: &Entity<AppShell>, vcx: &mut VisualTestContext, cwd: &Path) -> TabId {
    let id = vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let body = cx.new(|cx| Body {
                focus: cx.focus_handle(),
            });
            let tab = cx.new(|_| CwdTab {
                cwd: cwd.to_path_buf(),
                body,
            });
            let handle = TabHandle::new(tab);
            let id = handle.id();
            s.push_tab(handle, Vec::new(), cx);
            s.show_new_tab(id, window, cx);
            id
        })
    });
    vcx.run_until_parked();
    id
}

fn add_project(shell: &Entity<AppShell>, vcx: &mut VisualTestContext, path: &Path) {
    let p = path.to_path_buf();
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.add_project(p, cx)));
    vcx.run_until_parked();
}

fn selected(shell: &Entity<AppShell>, vcx: &mut VisualTestContext) -> Option<PathBuf> {
    vcx.update(|_, cx| shell.read(cx).projects.selected.clone())
}

fn listed(shell: &Entity<AppShell>, vcx: &mut VisualTestContext) -> Vec<Known> {
    vcx.update(|_, cx| shell.read(cx).spaces.known.clone())
}

fn hint(shell: &Entity<AppShell>, vcx: &mut VisualTestContext) -> String {
    vcx.update(|_, cx| {
        shell
            .read(cx)
            .prompts
            .hint
            .as_ref()
            .map(|h| h.text.to_string())
            .unwrap_or_default()
    })
}

fn mkdir(parent: &Path, name: &str) -> PathBuf {
    let p = parent.join(name);
    std::fs::create_dir_all(&p).unwrap();
    proj::canonical(&p).unwrap()
}

#[gpui::test]
fn adding_an_existing_folder_lists_selects_and_saves_it(cx: &mut TestAppContext) {
    let (_dd, db) = db();
    let (shell, vcx) = rig_db(cx, Some(db.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let tron = mkdir(tmp.path(), "TRON Project");
    add_project(&shell, vcx, &tron);

    assert_eq!(selected(&shell, vcx), Some(tron.clone()));
    let known = listed(&shell, vcx);
    assert!(known.len() == 1 && known[0].explicit && same_path(&known[0].path, &tron));
    // Saved: a favorite of the database, the selection and the palette's project list.
    let favs = db.favorites().unwrap();
    assert_eq!(favs.len(), 1);
    assert!(same_path(Path::new(&favs[0].path), &tron));
    assert_eq!(
        db.meta_get(store::db::SELECTED_PROJECT).unwrap().as_deref(),
        Some(tron.to_string_lossy().as_ref())
    );
    assert_eq!(db.projects().unwrap().len(), 1);
    // It is in the sidebar with no tab open.
    let spaces = build_spaces(&[], store::GroupBy::Project, &HashMap::new(), &known);
    assert_eq!(spaces.len(), 1);
    assert!(!spaces[0].is_open() && spaces[0].explicit);
    assert!(hint(&shell, vcx).contains("TRON Project"));
}

#[gpui::test]
fn a_project_stays_in_the_list_after_a_restart(cx: &mut TestAppContext) {
    let (_dd, db) = db();
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    let b = mkdir(tmp.path(), "b");
    {
        let (shell, vcx) = rig_db(cx, Some(db.clone()));
        add_project(&shell, vcx, &a);
        add_project(&shell, vcx, &b);
    }
    // A new shell on the same database loads them again (no tab is open).
    let (shell, vcx) = rig_db(cx, Some(db));
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.load_known_projects(cx)));
    vcx.run_until_parked();
    let known = listed(&shell, vcx);
    assert_eq!(known.len(), 2);
    assert!(known.iter().all(|k| k.explicit));
    assert!(same_path(&known[0].path, &a) && same_path(&known[1].path, &b));
    // ...and the selection is the last one.
    assert_eq!(selected(&shell, vcx), Some(b));
}

#[gpui::test]
fn creating_a_project_makes_the_folder_only_on_confirm(cx: &mut TestAppContext) {
    let (_dd, db) = db();
    let (shell, vcx) = rig_db(cx, Some(db.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let parent = proj::canonical(tmp.path()).unwrap();

    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_create_dialog(window, cx);
            let d = s.projects.create.as_mut().unwrap();
            d.parent = parent.clone();
            assert!(!d.git_init, "git init is off by default");
            d.name.update(cx, |f, cx| f.set_text("mi-app", cx));
        })
    });
    vcx.run_until_parked();
    assert!(
        !parent.join("mi-app").exists(),
        "opening and typing creates nothing"
    );

    vcx.update(|window, cx| shell.update(cx, |s, cx| s.confirm_create(window, cx)));
    vcx.run_until_parked();
    assert!(parent.join("mi-app").is_dir());
    assert!(
        !parent.join("mi-app").join(".git").exists(),
        "no git init unless asked"
    );
    assert!(vcx.update(|_, cx| shell.read(cx).projects.create.is_none()));
    assert_eq!(selected(&shell, vcx), Some(parent.join("mi-app")));
    assert_eq!(db.favorites().unwrap().len(), 1);
}

#[gpui::test]
fn an_invalid_name_creates_nothing_and_says_why(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    for bad in ["a/b", "con", "   ", "x."] {
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                if s.projects.create.is_none() {
                    s.open_create_dialog(window, cx);
                }
                let d = s.projects.create.as_mut().unwrap();
                d.parent = tmp.path().to_path_buf();
                d.name.update(cx, |f, cx| f.set_text(bad, cx));
                s.confirm_create(window, cx);
            })
        });
        vcx.run_until_parked();
        let (open, has_error) = vcx.update(|_, cx| {
            let s = shell.read(cx);
            let d = s.projects.create.as_ref();
            (
                d.is_some(),
                d.is_some_and(|d| d.name.read(cx).error().is_some()),
            )
        });
        assert!(open, "{bad:?}: the dialog stays open");
        assert!(has_error, "{bad:?}: the field explains the problem");
    }
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    assert!(listed(&shell, vcx).is_empty());
}

#[gpui::test]
fn an_existing_folder_is_never_overwritten_and_can_be_opened_instead(cx: &mut TestAppContext) {
    let (_dd, db) = db();
    let (shell, vcx) = rig_db(cx, Some(db));
    let tmp = tempfile::tempdir().unwrap();
    let existing = mkdir(tmp.path(), "ya-existe");
    std::fs::write(existing.join("keep.txt"), "mine").unwrap();

    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_create_dialog(window, cx);
            let d = s.projects.create.as_mut().unwrap();
            d.parent = proj::canonical(tmp.path()).unwrap();
            d.name.update(cx, |f, cx| f.set_text("ya-existe", cx));
            s.confirm_create(window, cx);
        })
    });
    vcx.run_until_parked();
    let (offer, error) = vcx.update(|_, cx| {
        let d = shell
            .read(cx)
            .projects
            .create
            .as_ref()
            .unwrap()
            .exists
            .clone();
        let e = shell
            .read(cx)
            .projects
            .create
            .as_ref()
            .unwrap()
            .error
            .clone();
        (d, e)
    });
    assert!(offer.is_some_and(|p| same_path(&p, &existing)));
    assert!(error.unwrap().contains("No se modificó nada"));
    assert_eq!(
        std::fs::read_to_string(existing.join("keep.txt")).unwrap(),
        "mine"
    );
    assert!(listed(&shell, vcx).is_empty(), "nothing was added yet");

    // The offer: open the one that is there.
    vcx.update(|window, cx| shell.update(cx, |s, cx| s.open_existing_from_dialog(window, cx)));
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| shell.read(cx).projects.create.is_none()));
    assert_eq!(selected(&shell, vcx), Some(existing.clone()));
    assert_eq!(listed(&shell, vcx).len(), 1);
    assert!(existing.join("keep.txt").exists());
}

#[gpui::test]
fn duplicates_nested_and_missing_folders_are_answered(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "Alpha");
    let inner = mkdir(&a, "sub");
    let other = mkdir(tmp.path(), "Beta");
    add_project(&shell, vcx, &a);
    add_project(&shell, vcx, &other);
    assert_eq!(selected(&shell, vcx), Some(other.clone()));

    // The same folder again, spelled differently: no second entry, and it gets selected.
    let shouty = PathBuf::from(a.to_string_lossy().to_uppercase());
    add_project(&shell, vcx, if cfg!(windows) { &shouty } else { &a });
    assert_eq!(listed(&shell, vcx).len(), 2);
    assert_eq!(selected(&shell, vcx), Some(a.clone()));
    assert!(hint(&shell, vcx).contains("ya está en la lista"));

    // Inside a listed project: that project is selected instead.
    add_project(&shell, vcx, &other);
    add_project(&shell, vcx, &inner);
    assert_eq!(listed(&shell, vcx).len(), 2);
    assert_eq!(selected(&shell, vcx), Some(a.clone()));
    assert!(hint(&shell, vcx).contains("dentro de"));

    // A folder that does not exist, and a file.
    add_project(&shell, vcx, &tmp.path().join("nope"));
    assert!(hint(&shell, vcx).contains("no existe"));
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    add_project(&shell, vcx, &file);
    assert!(hint(&shell, vcx).contains("archivo"));
    assert_eq!(listed(&shell, vcx).len(), 2);
}

#[gpui::test]
fn removing_from_the_list_keeps_the_files(cx: &mut TestAppContext) {
    let (_dd, db) = db();
    let (shell, vcx) = rig_db(cx, Some(db.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let p = mkdir(tmp.path(), "mio");
    std::fs::write(p.join("code.rs"), "fn main() {}").unwrap();
    add_project(&shell, vcx, &p);
    assert_eq!(listed(&shell, vcx).len(), 1);

    vcx.update(|_, cx| shell.update(cx, |s, cx| s.remove_project(p.clone(), cx)));
    vcx.run_until_parked();
    assert!(listed(&shell, vcx).is_empty());
    assert_eq!(
        selected(&shell, vcx),
        None,
        "the removed project is deselected"
    );
    assert!(db.favorites().unwrap().is_empty());
    assert!(db.projects().unwrap().is_empty());
    assert_eq!(db.meta_get(store::db::SELECTED_PROJECT).unwrap(), None);
    assert_eq!(
        std::fs::read_to_string(p.join("code.rs")).unwrap(),
        "fn main() {}",
        "no file is touched"
    );
    assert!(hint(&shell, vcx).contains("tus archivos siguen"));
}

#[gpui::test]
fn the_selection_is_saved_and_comes_back_after_a_restart(cx: &mut TestAppContext) {
    let (_dd, db) = db();
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    {
        let (shell, vcx) = rig_db(cx, Some(db.clone()));
        vcx.update(|_, cx| shell.update(cx, |s, cx| s.set_selected(Some(a.clone()), cx)));
        vcx.run_until_parked();
        assert_eq!(
            db.meta_get(store::db::SELECTED_PROJECT).unwrap().as_deref(),
            Some(a.to_string_lossy().as_ref())
        );
    }
    let (shell, vcx) = rig_db(cx, Some(db.clone()));
    assert_eq!(selected(&shell, vcx), Some(a));
    // Clearing it is saved too.
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.set_selected(None, cx)));
    vcx.run_until_parked();
    assert_eq!(db.meta_get(store::db::SELECTED_PROJECT).unwrap(), None);
}

#[gpui::test]
fn the_selected_project_is_where_new_terminals_and_chats_go(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    let b = mkdir(tmp.path(), "b");
    add_cwd_tab(&shell, vcx, &a);

    let ask = |vcx: &mut VisualTestContext| {
        vcx.update(|_, cx| {
            shell.update(cx, |s, cx| (s.new_terminal_cwd(cx), s.new_chat_project(cx)))
        })
    };
    // Nothing selected: today's behaviour, the active tab's folder (and the label says so).
    assert_eq!(ask(vcx), (Some(a.clone()), Some(a.clone())));
    assert_eq!(
        vcx.update(|_, cx| shell.read(cx).dest().line()),
        "en la carpeta actual"
    );
    // A project selected: both go there, whatever tab is in front.
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.set_selected(Some(b.clone()), cx)));
    assert_eq!(ask(vcx), (Some(b.clone()), Some(b.clone())));
    assert_eq!(
        vcx.update(|_, cx| shell.read(cx).dest().line()),
        "en b".to_string()
    );
    // The new-thread picker of the chat starts on it too.
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.sync_chat_projects(cx)));
    // Ctrl+N's new terminal really opens in the project: the tab model is asked for it.
    assert_eq!(vcx.update(|_, cx| shell.read(cx).dest()), Dest::Project(b));
}

#[gpui::test]
fn bringing_a_tab_forward_selects_its_project(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    let b = mkdir(tmp.path(), "b");
    let ta = add_cwd_tab(&shell, vcx, &a);
    let tb = add_cwd_tab(&shell, vcx, &b);
    // Opening tabs by itself selects nothing (a restored session keeps its saved selection).
    assert_eq!(selected(&shell, vcx), None);
    vcx.update(|window, cx| shell.update(cx, |s, cx| s.select_tab(ta, window, cx)));
    assert_eq!(selected(&shell, vcx), Some(a.clone()));
    vcx.update(|window, cx| shell.update(cx, |s, cx| s.select_tab(tb, window, cx)));
    assert_eq!(selected(&shell, vcx), Some(b.clone()));
    // Ctrl+Tab does the same.
    vcx.update(|window, cx| shell.update(cx, |s, cx| s.cycle_tab(1, window, cx)));
    assert_eq!(selected(&shell, vcx), Some(a.clone()));
    // A re-sync of the focused tab (what a restore does) keeps a selection made by hand.
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.set_selected(Some(b.clone()), cx);
            s.after_active_changed(window, cx);
        })
    });
    assert_eq!(selected(&shell, vcx), Some(b));
}

#[gpui::test]
fn a_vanished_folder_is_muted_and_never_a_destination(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let keep = mkdir(tmp.path(), "keep");
    let gone = mkdir(tmp.path(), "gone");
    add_cwd_tab(&shell, vcx, &keep);
    add_project(&shell, vcx, &gone);
    assert_eq!(selected(&shell, vcx), Some(gone.clone()));

    std::fs::remove_dir_all(&gone).unwrap();
    // Before any check: the action itself notices (one stat), says so, and falls back.
    let cwd = vcx.update(|_, cx| shell.update(cx, |s, cx| s.new_terminal_cwd(cx)));
    assert_eq!(cwd, Some(keep.clone()));
    assert!(hint(&shell, vcx).contains("ya no existe"));
    // The check (window focus) marks it muted in the sidebar.
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.refresh_missing(cx)));
    vcx.run_until_parked();
    let dest = vcx.update(|_, cx| shell.read(cx).dest());
    assert!(matches!(dest, Dest::Missing(_)), "{dest:?}");
    assert!(dest.line().contains("no existe"));
    let (infos, known, missing) = vcx.update(|_, cx| {
        let s = shell.read(cx);
        (
            s.infos(cx),
            s.spaces.known.clone(),
            s.projects.missing.clone(),
        )
    });
    assert!(missing.iter().any(|m| same_path(m, &gone)));
    let spaces = build_spaces(&infos, store::GroupBy::Project, &HashMap::new(), &known);
    assert!(spaces.iter().any(|s| same_path(&s.root, &gone)));
    // It cannot be selected again, but it can be removed from the list.
    vcx.update(|_, cx| {
        shell.update(cx, |s, cx| {
            s.set_selected(None, cx);
            s.select_project(gone.clone(), cx);
        })
    });
    assert_eq!(selected(&shell, vcx), None);
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.remove_project(gone.clone(), cx)));
    vcx.run_until_parked();
    assert!(listed(&shell, vcx).is_empty());
}

#[gpui::test]
fn clicking_a_project_header_selects_it_and_the_chevron_only_folds(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    let b = mkdir(tmp.path(), "b");
    add_cwd_tab(&shell, vcx, &a);
    vcx.update(|_, cx| {
        shell.update(cx, |s, cx| {
            s.spaces.known = vec![Known::new(b.clone(), String::new(), true)];
            s.refresh(cx);
        })
    });
    vcx.run_until_parked();

    // A project without tabs: a click selects it (it used to open a terminal).
    testing::click_id(vcx, &format!("space-{}", b.display()));
    assert_eq!(selected(&shell, vcx), Some(b.clone()));
    assert_eq!(vcx.update(|_, cx| shell.read(cx).tabs.len()), 1);

    // An open project: the chevron folds it and leaves the selection alone...
    testing::click_id(vcx, &format!("space-fold-{}", a.display()));
    assert!(vcx.update(|_, cx| shell.read(cx).spaces.folded.contains(&a)));
    assert_eq!(selected(&shell, vcx), Some(b.clone()));
    // ...and the rest of its header selects without folding.
    testing::click_id(vcx, &format!("space-{}", a.display()));
    assert_eq!(selected(&shell, vcx), Some(a.clone()));
    assert!(vcx.update(|_, cx| shell.read(cx).spaces.folded.contains(&a)));
}

#[gpui::test]
fn the_plus_of_the_header_and_the_palette_open_the_add_menu(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    add_cwd_tab(&shell, vcx, Path::new("C:/dev/a"));
    let open = |vcx: &mut VisualTestContext| {
        vcx.update(|_, cx| shell.read(cx).projects.menu.as_ref().map(|m| m.items.len()))
    };
    assert_eq!(open(vcx), None);
    testing::click_id(vcx, "sb-add-project");
    assert_eq!(
        open(vcx),
        Some(2),
        "«Abrir carpeta existente…» and «Crear proyecto nuevo…»"
    );
    // Keyboard: Esc closes, the palette entry opens it again, `c` goes to the form.
    testing::press(vcx, "escape");
    assert_eq!(open(vcx), None);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.on_palette(
                ui_palette::PaletteEvent::Action(crate::palette::id::PROJECT_ADD.into()),
                window,
                cx,
            )
        })
    });
    assert_eq!(open(vcx), Some(2));
    testing::press(vcx, "down down up");
    testing::press(vcx, "c");
    assert_eq!(open(vcx), None);
    assert!(vcx.update(|_, cx| shell.read(cx).projects.create.is_some()));
    // Esc leaves the form without creating anything.
    testing::press(vcx, "escape");
    assert!(vcx.update(|_, cx| shell.read(cx).projects.create.is_none()));
}

#[gpui::test]
fn the_empty_state_offers_adding_a_project(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    assert!(
        testing::bounds_of(vcx, "sb-empty-add").is_some(),
        "no projects: the sidebar says so and offers the button"
    );
    testing::click_id(vcx, "sb-empty-add");
    assert!(vcx.update(|_, cx| shell.read(cx).projects.menu.is_some()));
    testing::press(vcx, "escape");
    add_project(&shell, vcx, &a);
    assert_eq!(listed(&shell, vcx).len(), 1);
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    assert!(
        testing::bounds_of(vcx, &format!("space-{}", a.display())).is_some(),
        "the new project is painted"
    );
    // (gpui's debug bounds are never cleared between frames, so an old entry cannot be asserted
    // away: the empty state is gated on the list being empty, which the line above checks.)
}

#[gpui::test]
fn the_menu_of_a_project_offers_new_terminal_chat_and_remove(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "a");
    add_project(&shell, vcx, &a);
    let titles = |vcx: &mut VisualTestContext| {
        vcx.update(|_, cx| {
            shell
                .read(cx)
                .projects
                .menu
                .as_ref()
                .map(|m| m.items.iter().map(|i| i.title).collect::<Vec<_>>())
        })
    };
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_space_menu(
                a.clone(),
                gpui::point(gpui::px(40.0), gpui::px(120.0)),
                true,
                window,
                cx,
            )
        })
    });
    assert_eq!(
        titles(vcx),
        Some(vec![
            "Nueva terminal aquí",
            "Nuevo chat aquí",
            "Quitar de la lista"
        ])
    );
    // The "+" menu has no removal.
    testing::press(vcx, "escape");
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_space_menu(
                a.clone(),
                gpui::point(gpui::px(40.0), gpui::px(120.0)),
                false,
                window,
                cx,
            )
        })
    });
    assert_eq!(
        titles(vcx),
        Some(vec!["Nueva terminal aquí", "Nuevo chat aquí"])
    );
    testing::press(vcx, "escape");
    // `q` on the full menu removes the project from the list.
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_space_menu(
                a.clone(),
                gpui::point(gpui::px(40.0), gpui::px(120.0)),
                true,
                window,
                cx,
            )
        })
    });
    testing::press(vcx, "q");
    vcx.run_until_parked();
    assert!(listed(&shell, vcx).is_empty());
    assert!(a.is_dir(), "the folder is still there");
}

#[gpui::test]
fn dropping_folders_on_the_sidebar_adds_them_and_files_are_explained(cx: &mut TestAppContext) {
    let (shell, vcx) = rig_db(cx, None);
    let tmp = tempfile::tempdir().unwrap();
    let a = mkdir(tmp.path(), "dropped");
    let f = tmp.path().join("f.txt");
    std::fs::write(&f, "x").unwrap();
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.drop_on_sidebar(std::slice::from_ref(&f), cx)));
    vcx.run_until_parked();
    assert!(listed(&shell, vcx).is_empty());
    assert!(hint(&shell, vcx).contains("carpeta"));
    vcx.update(|_, cx| shell.update(cx, |s, cx| s.drop_on_sidebar(&[f, a.clone()], cx)));
    vcx.run_until_parked();
    assert_eq!(listed(&shell, vcx).len(), 1);
    assert_eq!(selected(&shell, vcx), Some(a));
}
