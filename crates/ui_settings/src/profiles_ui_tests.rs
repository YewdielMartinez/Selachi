//! Tests of the profile editor: listing, inline validation, clamping against Delegación, the
//! context picker restricted to the project, saving / deleting / templates (always after an
//! explicit step) and the clipboard round trip.

use super::*;
use gpui::TestAppContext;
use ui_kit::testing;

struct Rig {
    dir: tempfile::TempDir,
    project: PathBuf,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(project.join("docs/PLAN.md"), "x").unwrap();
    std::fs::write(project.join("README.md"), "x").unwrap();
    let global = profiles::global_dir(dir.path());
    profiles::write_profile(
        &global,
        &Profile {
            name: "Global uno".into(),
            ..Profile::default()
        },
    )
    .unwrap();
    profiles::write_profile(
        &profiles::project_dir(&project),
        &Profile {
            name: "De proyecto".into(),
            agent: "codex".into(),
            ..Profile::default()
        },
    )
    .unwrap();
    std::fs::write(global.join("roto.toml"), "name = [").unwrap();
    Rig { dir, project }
}

fn editor(cx: &mut TestAppContext, rig: &Rig) -> Entity<ProfilesEditor> {
    let data = rig.dir.path().to_path_buf();
    let project = rig.project.clone();
    let e = cx.new(|cx| ProfilesEditor::new(Theme::dark(), data, cx));
    e.update(cx, |e, cx| {
        e.set_project(Some(project), cx);
        e.reload(cx);
    });
    cx.run_until_parked();
    e
}

fn set_name(cx: &mut TestAppContext, e: &Entity<ProfilesEditor>, name: &str) {
    let f = e.read_with(cx, |e, _| e.inputs.name.clone());
    f.update(cx, |f, cx| f.set_text(name, cx));
}

#[gpui::test]
fn lists_global_and_project_profiles_with_their_scope(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    e.read_with(cx, |e, _| {
        assert!(e.is_loaded());
        let scopes: Vec<_> = e.entries().iter().map(|x| (x.scope, x.title())).collect();
        assert!(scopes.contains(&(Scope::Global, "Global uno".into())));
        assert!(scopes.contains(&(Scope::Project, "De proyecto".into())));
        // the broken file is listed and flagged
        assert!(e.entries().iter().any(|x| x.profile.is_err()));
    });
    // opening the broken one only says so
    let broken = e.read_with(cx, |e, _| {
        e.entries().iter().position(|x| x.profile.is_err()).unwrap()
    });
    e.update(cx, |e, cx| e.select_entry(broken, cx));
    e.read_with(cx, |e, _| {
        assert!(!e.has_draft());
        assert!(e.notice().unwrap().contains("no se puede abrir"));
    });
}

#[gpui::test]
fn the_form_validates_inline(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    e.update(cx, |e, cx| e.start_new(None, cx));
    let fields = |cx: &mut TestAppContext| -> Vec<&'static str> {
        e.read_with(cx, |e, cx| {
            e.issues(cx).into_iter().map(|i| i.field).collect()
        })
    };
    assert!(fields(cx).contains(&"name"));
    set_name(cx, &e, "global UNO");
    assert!(fields(cx).contains(&"name"), "duplicate in the same scope");
    e.update(cx, |e, cx| e.edit(cx, |d| d.scope = Scope::Project));
    assert!(
        !fields(cx).contains(&"name"),
        "same name is fine in the other scope"
    );
    let t = e.read_with(cx, |e, _| e.inputs.timeout.clone());
    t.update(cx, |f, cx| f.set_text("abc", cx));
    let b = e.read_with(cx, |e, _| e.inputs.budget.clone());
    b.update(cx, |f, cx| f.set_text("12x", cx));
    let got = fields(cx);
    assert!(
        got.contains(&"timeout") && got.contains(&"budget_tokens"),
        "{got:?}"
    );
    t.update(cx, |f, cx| f.set_text("7,5", cx));
    b.update(cx, |f, cx| f.set_text("200000", cx));
    let (p, issues) = e.read_with(cx, |e, cx| e.form_profile(cx));
    assert!(issues.is_empty());
    assert_eq!((p.timeout, p.budget_tokens), (Some(450), Some(200_000)));
}

#[gpui::test]
fn the_permission_never_exceeds_delegation(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    let seed = Profile {
        name: "Edita".into(),
        permission: "accept-edits".into(),
        worktree: true,
        ..Profile::default()
    };
    e.update(cx, |e, cx| {
        e.set_limits(
            Limits {
                max_permission: "ask".into(),
                ..Limits::default()
            },
            cx,
        );
        e.start_new(Some(seed.clone()), cx);
    });
    e.read_with(cx, |e, cx| {
        assert_eq!(e.form_profile(cx).0.permission, "ask");
        assert!(e.notice().unwrap().contains("Delegación"));
    });
    // lowering the maximum later clamps the open draft too
    e.update(cx, |e, cx| {
        e.set_limits(
            Limits {
                max_permission: "read-only".into(),
                ..Limits::default()
            },
            cx,
        );
    });
    e.read_with(cx, |e, cx| {
        assert_eq!(e.form_profile(cx).0.permission, "read-only")
    });
    // an imported profile asking for `full` is clamped as well
    e.update(cx, |e, cx| {
        e.import_text("name = \"X\"\npermission = \"full\"", "x", cx)
    });
    e.read_with(cx, |e, cx| {
        assert_eq!(e.form_profile(cx).0.permission, "read-only")
    });
}

#[gpui::test]
fn context_files_stay_inside_the_project(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    e.update(cx, |e, cx| e.start_new(None, cx));
    let abs = rig
        .project
        .join("docs")
        .join("PLAN.md")
        .display()
        .to_string();
    e.update(cx, |e, cx| {
        e.add_item("context_files", "../secretos.txt", cx);
        e.add_item("context_files", "C:\\Windows\\win.ini", cx);
    });
    e.read_with(cx, |e, cx| {
        assert!(e.form_profile(cx).0.context_files.is_empty());
        assert!(e.add_errors.contains_key("context_files"));
    });
    e.update(cx, |e, cx| {
        e.add_item("context_files", &abs, cx);
        e.add_item("context_files", "README.md", cx);
        e.add_item("context_files", "readme.md", cx);
    });
    e.read_with(cx, |e, cx| {
        assert_eq!(
            e.form_profile(cx).0.context_files,
            vec!["docs/PLAN.md", "README.md"]
        );
    });
    // the picker only suggests project files that are not attached yet
    let ctx = e.read_with(cx, |e, _| e.inputs.context.clone());
    ctx.update(cx, |f, cx| f.set_text("plan", cx));
    e.update(cx, |e, cx| e.refresh_suggestions(cx));
    cx.run_until_parked();
    e.update(cx, |e, cx| e.update_suggestions(cx));
    e.read_with(cx, |e, _| {
        assert!(e.suggestions.is_empty(), "{:?}", e.suggestions)
    });
    ctx.update(cx, |f, cx| f.set_text("read", cx));
    e.update(cx, |e, cx| e.remove_item("context_files", 1, cx));
    e.update(cx, |e, cx| e.update_suggestions(cx));
    e.read_with(cx, |e, _| assert_eq!(e.suggestions, vec!["README.md"]));
}

#[gpui::test]
fn saving_writes_the_file_and_renaming_removes_the_old_one(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    e.update(cx, |e, cx| e.start_new(None, cx));
    e.update(cx, |e, cx| e.save(cx));
    cx.run_until_parked();
    assert!(e.read_with(cx, |e, _| e.notice().unwrap().contains("Corrige")));
    set_name(cx, &e, "Mi perfil");
    e.update(cx, |e, cx| e.save(cx));
    cx.run_until_parked();
    let global = profiles::global_dir(rig.dir.path());
    let path = global.join("mi-perfil.toml");
    assert!(path.exists());
    let saved = profiles::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved.name, "Mi perfil");
    // rename
    set_name(cx, &e, "Otro nombre");
    e.update(cx, |e, cx| e.save(cx));
    cx.run_until_parked();
    assert!(!path.exists());
    assert!(global.join("otro-nombre.toml").exists());
    // a name whose file name already exists is refused, nothing overwritten
    e.update(cx, |e, cx| e.start_new(None, cx));
    set_name(cx, &e, "Otro-Nombre");
    e.update(cx, |e, cx| e.save(cx));
    cx.run_until_parked();
    assert!(e.read_with(cx, |e, _| e.notice().unwrap().contains("Ya existe")));
    // the project scope writes under .selachi\agents
    set_name(cx, &e, "Solo aqui");
    e.update(cx, |e, cx| e.edit(cx, |d| d.scope = Scope::Project));
    e.update(cx, |e, cx| e.save(cx));
    cx.run_until_parked();
    assert!(
        profiles::project_dir(&rig.project)
            .join("solo-aqui.toml")
            .exists()
    );
}

#[gpui::test]
fn delete_asks_first(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    let ix = e.read_with(cx, |e, _| {
        e.entries()
            .iter()
            .position(|x| x.title() == "Global uno")
            .unwrap()
    });
    e.update(cx, |e, cx| e.select_entry(ix, cx));
    let path = profiles::global_dir(rig.dir.path()).join("global-uno.toml");
    // the confirmation only appears; nothing is deleted yet
    e.update(cx, |e, cx| {
        e.confirm = Some(Confirm::Delete);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(path.exists());
    e.update(cx, |e, cx| e.delete(cx));
    cx.run_until_parked();
    assert!(!path.exists());
    assert!(!e.read_with(cx, |e, _| e.has_draft()));
}

#[gpui::test]
fn templates_are_written_only_after_confirming(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    let global = profiles::global_dir(rig.dir.path());
    let before = std::fs::read_dir(&global).unwrap().count();
    e.update(cx, |e, cx| e.ask_templates(cx));
    cx.run_until_parked();
    assert!(matches!(
        e.read_with(cx, |e, _| e.confirm.clone()),
        Some(Confirm::Templates(n)) if n.len() == 4
    ));
    assert_eq!(
        std::fs::read_dir(&global).unwrap().count(),
        before,
        "asking writes nothing"
    );
    // an existing profile with a template's file name is never overwritten
    std::fs::write(
        global.join("tests.toml"),
        "name = \"Tests\"\ndescription = \"mio\"",
    )
    .unwrap();
    e.update(cx, |e, cx| e.ask_templates(cx));
    e.update(cx, |e, cx| e.create_templates(cx));
    cx.run_until_parked();
    let mine = std::fs::read_to_string(global.join("tests.toml")).unwrap();
    assert!(mine.contains("mio"));
    assert!(global.join("docs.toml").exists() && global.join("investigador.toml").exists());
    // everything exists now
    e.update(cx, |e, cx| e.ask_templates(cx));
    assert!(e.read_with(cx, |e, _| e.confirm.is_none()
        && e.notice().unwrap().contains("ya existen")));
}

#[gpui::test]
fn export_and_import_round_trip_through_the_clipboard(cx: &mut TestAppContext) {
    let rig = rig();
    let e = editor(cx, &rig);
    let seed = Profile {
        name: "Viaje".into(),
        description: "ida y vuelta".into(),
        allowed_tools: vec!["Read".into()],
        instructions: "Una\nDos".into(),
        ..Profile::default()
    };
    e.update(cx, |e, cx| e.start_new(Some(seed.clone()), cx));
    e.update(cx, |e, cx| e.export(cx));
    let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
    assert!(text.contains("name = \"Viaje\""));
    e.update(cx, |e, cx| e.start_new(None, cx));
    e.update(cx, |e, cx| e.import_clipboard(cx));
    let (p, issues) = e.read_with(cx, |e, cx| e.form_profile(cx));
    assert_eq!(p, seed);
    assert!(issues.is_empty());
    // garbage is reported, not applied
    e.update(cx, |e, cx| e.import_text("name = [", "x", cx));
    assert!(e.read_with(cx, |e, _| {
        e.notice().unwrap().contains("No se pudo importar")
    }));
}

#[gpui::test]
fn the_buttons_and_rows_are_clickable(cx: &mut TestAppContext) {
    let rig = rig();
    let data = rig.dir.path().to_path_buf();
    let project = rig.project;
    let (e, vcx) = testing::open_window(cx, move |_, cx| {
        let mut e = ProfilesEditor::new(Theme::dark(), data, cx);
        e.project_root = Some(project);
        e
    });
    e.update(vcx, |e, cx| e.reload(cx));
    vcx.run_until_parked();
    testing::click_id(vcx, "profiles-new");
    assert!(e.read_with(vcx, |e, _| e.has_draft()));
    // the lower part of the form is below the fold of the test window: drive it directly
    e.update(vcx, |e, cx| {
        e.show_dry_run = true;
        cx.notify();
    });
    vcx.run_until_parked();
    testing::click_id(vcx, "profiles-templates");
    assert!(e.read_with(vcx, |e, _| e.confirm.is_some()));
    testing::click_id(vcx, "profiles-templates-no");
    assert!(e.read_with(vcx, |e, _| e.confirm.is_none()));
    e.update(vcx, |e, cx| {
        e.draft = None;
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(!e.read_with(vcx, |e, _| e.has_draft()));
}
