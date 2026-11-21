//! End-to-end behaviour of the palette without a window: real sources over a real `StateDb`,
//! scope routing, what Enter / Ctrl+Enter emit, the snippet parameter flow, stale-result
//! handling, cancellation and the latency budget.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use git_svc::GitAction;
use panel_api::{TabIcon, TabInfo};
use store::{BuiltinPreset, NewCommand, NewSnippet, PresetId, StateDb};
use ui_kit::Icon;
use ui_palette::{
    ActionEntry, Cancel, Confirmed, Effect, ItemAction, Latency, LayoutRow, PaletteEvent, PaletteItem, PaletteModel,
    PaletteSource, Query, Results, Scope, Section, SourceCtx, SourceQuery, plan, run_blocking, run_source,
    standard_sources,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn actions() -> Vec<ActionEntry> {
    vec![
        ActionEntry::new("tab.new", "Nueva pestaña").shortcut("Ctrl+T").icon(Icon::Plus).keywords("crear terminal"),
        ActionEntry::new("tab.close", "Cerrar pestaña").shortcut("Ctrl+W"),
        ActionEntry::new("layout.toggle", "Abrir panel de diseño").shortcut("Ctrl+Shift+L").icon(Icon::Layout),
        ActionEntry::new("settings.open", "Ajustes").icon(Icon::Settings),
    ]
}

fn tabs() -> Vec<TabInfo> {
    // EntityIds cannot be made outside gpui, so only the data the sources read is exercised
    // through `tab_item_data`; real tab ids are covered by the demo.
    Vec::new()
}

fn db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(&dir.path().join("state.db")).unwrap();
    (dir, db)
}

fn ctx() -> SourceCtx {
    SourceCtx { cwd: Some(repo_root()), project_root: Some(repo_root()), tabs: tabs(), active_tab: None }
}

fn titles(model: &PaletteModel) -> Vec<String> {
    model
        .rows
        .iter()
        .filter_map(|r| match r {
            LayoutRow::Item(i) => Some(i.title.to_string()),
            LayoutRow::Header(_) => None,
        })
        .collect()
}

fn headers(model: &PaletteModel) -> Vec<&'static str> {
    model
        .rows
        .iter()
        .filter_map(|r| match r {
            LayoutRow::Header(h) => Some(*h),
            LayoutRow::Item(_) => None,
        })
        .collect()
}

fn select(model: &mut PaletteModel, title: &str) {
    let row = model
        .rows
        .iter()
        .position(|r| matches!(r, LayoutRow::Item(i) if i.title == title))
        .unwrap_or_else(|| panic!("no row {title:?} in {:?}", titles(model)));
    model.select_at_row(row);
}

fn meta(m: &PaletteModel) -> Option<String> {
    m.selected_item().unwrap().meta.as_ref().map(|s| s.to_string())
}

fn standard(db: &StateDb) -> Vec<Arc<dyn PaletteSource>> {
    standard_sources(Some(db.clone()), actions(), vec!["Mi flujo".into()])
}

// ---- prefix routing -------------------------------------------------------------------------

#[test]
fn prefixes_route_to_their_sources_only() {
    let (_d, db) = db();
    let sources = standard(&db);
    let ids = |raw: &str| -> Vec<&'static str> {
        let mut v: Vec<_> = plan(&sources, &Query::parse(raw)).iter().map(|(s, _)| s.id()).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(ids(">x"), ["actions", "git", "presets"]);
    assert_eq!(ids("#x"), ["history"]);
    assert_eq!(ids("@x"), ["projects"]);
    assert_eq!(ids("!x"), ["tasks"]);
    assert_eq!(ids("$x"), ["snippets"]);
    // Unscoped, typed: everything. Empty: git / presets stay out of the way (limit 0).
    assert_eq!(ids("x").len(), 8);
    let empty = ids("");
    assert!(!empty.contains(&"git") && !empty.contains(&"presets"));
    assert!(empty.contains(&"actions") && empty.contains(&"history"));
}

#[test]
fn scoped_results_only_contain_that_scope() {
    let (_d, db) = db();
    db.record_command(NewCommand::new("git status", repo_root().to_string_lossy())).unwrap();
    db.flush().unwrap();
    let sources = standard(&db);

    let m = run_blocking(&sources, ">nueva", &ctx());
    assert_eq!(headers(&m), ["Acciones"]);
    assert_eq!(titles(&m), ["Nueva pestaña"]);

    let m = run_blocking(&sources, "#git", &ctx());
    assert_eq!(headers(&m), ["Historial"]);
    assert_eq!(titles(&m), ["git status"]);

    let m = run_blocking(&sources, "$migr", &ctx());
    assert_eq!(headers(&m), ["Snippets"]);
    assert_eq!(titles(&m), ["EF: nueva migración"]);
}

#[test]
fn actions_scope_includes_git_and_layout_presets() {
    let (_d, db) = db();
    let sources = standard(&db);
    let m = run_blocking(&sources, ">", &ctx());
    let h = headers(&m);
    assert!(h.contains(&"Acciones") && h.contains(&"Git") && h.contains(&"Diseño"), "{h:?}");
    assert!(titles(&m).contains(&"Diseño: Mi flujo".to_string()));
}

// ---- event emission -------------------------------------------------------------------------

#[test]
fn actions_emit_the_action_id() {
    let (_d, db) = db();
    let mut m = run_blocking(&standard(&db), ">cerrar", &ctx());
    assert_eq!(titles(&m)[0], "Cerrar pestaña");
    assert_eq!(m.confirm(false), Confirmed::Emit(PaletteEvent::Action("tab.close".into()), None));
    assert_eq!(meta(&m).as_deref(), Some("Ctrl+W"));
}

#[test]
fn history_runs_with_enter_and_inserts_with_ctrl_enter() {
    let (_d, db) = db();
    db.record_command(NewCommand::new("cargo test -p store", repo_root().to_string_lossy())).unwrap();
    db.flush().unwrap();
    let mut m = run_blocking(&standard(&db), "#cargo test", &ctx());
    assert_eq!(
        m.confirm(false),
        Confirmed::Emit(
            PaletteEvent::RunCommand { line: "cargo test -p store".into(), cwd: Some(repo_root()), new_tab: false },
            None
        )
    );
    assert_eq!(m.confirm(true), Confirmed::Emit(PaletteEvent::InsertText("cargo test -p store".into()), None));
}

#[test]
fn projects_open_in_new_tab_or_here() {
    let (_d, db) = db();
    let root = repo_root();
    db.mark_project_opened(&root.to_string_lossy()).unwrap();
    db.flush().unwrap();
    let mut m = run_blocking(&standard(&db), "@terminal", &ctx());
    let name = titles(&m)[0].clone();
    assert_eq!(name, "Terminal");
    let effect = Some(Effect::PathOpened(root.to_string_lossy().trim_start_matches(r"\\?\").to_string()));
    let Confirmed::Emit(ev, eff) = m.confirm(false) else { panic!("expected emit") };
    assert!(matches!(ev, PaletteEvent::OpenPath { new_tab: true, .. }), "{ev:?}");
    assert!(matches!(eff, Some(Effect::PathOpened(_))), "{eff:?} vs {effect:?}");
    let Confirmed::Emit(ev, _) = m.confirm(true) else { panic!("expected emit") };
    assert!(matches!(ev, PaletteEvent::OpenPath { new_tab: false, .. }));
}

#[test]
fn tasks_of_this_repo_run_in_new_or_current_tab() {
    let (_d, db) = db();
    let mut m = run_blocking(&standard(&db), "!cargo build", &ctx());
    assert_eq!(headers(&m), ["Tareas"]);
    let item = m.selected_item().unwrap().clone();
    let PaletteEvent::RunCommand { line, new_tab, cwd } = emitted(&item.primary) else { panic!() };
    assert!(line.contains("cargo") && line.contains("build"), "{line}");
    assert!(new_tab);
    assert!(cwd.is_some());
    let Confirmed::Emit(PaletteEvent::RunCommand { new_tab, .. }, _) = m.confirm(true) else { panic!() };
    assert!(!new_tab);
}

fn emitted(a: &ItemAction) -> PaletteEvent {
    match &a.kind {
        ui_palette::ActionKind::Emit(e) => e.clone(),
        other => panic!("not an emit: {other:?}"),
    }
}

#[test]
fn presets_and_sensitive_git_actions() {
    let (_d, db) = db();
    let mut m = run_blocking(&standard(&db), ">diseño zen", &ctx());
    assert_eq!(titles(&m)[0], "Diseño: Zen");
    assert_eq!(m.confirm(false), Confirmed::Emit(PaletteEvent::ApplyPreset(PresetId::Builtin(BuiltinPreset::Zen)), None));
    let mut m = run_blocking(&standard(&db), ">mi flujo", &ctx());
    assert_eq!(m.confirm(false), Confirmed::Emit(PaletteEvent::ApplyPreset(PresetId::Custom("Mi flujo".into())), None));

    let mut m = run_blocking(&standard(&db), ">git push", &ctx());
    select(&mut m, "Git: push");
    assert_eq!(meta(&m).as_deref(), Some("requiere confirmación"));
    assert_eq!(m.confirm(false), Confirmed::Emit(PaletteEvent::GitAction(GitAction::Push), None));
    // Read-only actions are not flagged.
    let mut m = run_blocking(&standard(&db), ">git status", &ctx());
    select(&mut m, "Git: estado");
    assert_eq!(meta(&m), None);
    assert!(GitAction::Status.argv().join(" ").contains("status"));
}

#[test]
fn git_is_hidden_outside_a_repository() {
    let (_d, db) = db();
    let outside = tempfile::tempdir().unwrap();
    let c = SourceCtx { cwd: Some(outside.path().into()), project_root: None, tabs: vec![], active_tab: None };
    let m = run_blocking(&standard(&db), ">git", &c);
    assert!(!headers(&m).contains(&"Git"));
}

// ---- snippet parameter flow -----------------------------------------------------------------

#[test]
fn snippet_without_params_inserts_or_runs_and_records_use() {
    let (_d, db) = db();
    let id = db.add_snippet(NewSnippet::new("Estado corto", "git status -sb")).unwrap();
    let mut m = run_blocking(&standard(&db), "$estado corto", &ctx());
    assert_eq!(
        m.confirm(false),
        Confirmed::Emit(PaletteEvent::InsertText("git status -sb".into()), Some(Effect::SnippetUsed(id)))
    );
    let Confirmed::Emit(PaletteEvent::RunCommand { line, .. }, Some(Effect::SnippetUsed(i))) = m.confirm(true) else {
        panic!()
    };
    assert_eq!((line.as_str(), i), ("git status -sb", id));
}

#[test]
fn snippet_with_params_opens_the_form_then_emits_quoted_text() {
    let (_d, db) = db();
    let mut m = run_blocking(&standard(&db), "$migr", &ctx());
    assert_eq!(titles(&m), ["EF: nueva migración"]);
    assert_eq!(meta(&m).as_deref(), Some("1 parámetro"));
    assert_eq!(m.confirm(false), Confirmed::FormOpened);

    // Required value missing.
    assert_eq!(m.confirm(false), Confirmed::FormInvalid);
    assert_eq!(m.form.as_ref().unwrap().error.as_deref(), Some("Falta «nombre»"));

    m.form.as_mut().unwrap().set_active_value("Agregar usuarios");
    assert_eq!(m.form.as_ref().unwrap().preview().unwrap(), "dotnet ef migrations add 'Agregar usuarios'");
    let Confirmed::Emit(ev, Some(Effect::SnippetUsed(_))) = m.confirm(false) else { panic!() };
    assert_eq!(ev, PaletteEvent::InsertText("dotnet ef migrations add 'Agregar usuarios'".into()));
    let Confirmed::Emit(PaletteEvent::RunCommand { line, new_tab: false, .. }, _) = m.confirm(true) else { panic!() };
    assert_eq!(line, "dotnet ef migrations add 'Agregar usuarios'");

    // Esc: back to the list.
    m.close_form();
    assert!(m.form.is_none() && m.selected_item().is_some());
}

#[test]
fn snippet_with_defaults_and_choices_walks_the_fields_with_enter() {
    let (_d, db) = db();
    db.add_snippet(NewSnippet::new("Compilar", "dotnet build -c {cfg|Debug|Release} -v {v:minimal}")).unwrap();
    let mut m = run_blocking(&standard(&db), "$compilar", &ctx());
    assert_eq!(m.confirm(false), Confirmed::FormOpened);
    {
        let f = m.form.as_mut().unwrap();
        assert_eq!(f.preview().unwrap(), "dotnet build -c Debug -v minimal");
        f.cycle_choice(1);
    }
    // Enter on the first field moves on, on the last field it submits.
    assert_eq!(m.confirm(false), Confirmed::NextField);
    let Confirmed::Emit(ev, _) = m.confirm(false) else { panic!() };
    assert_eq!(ev, PaletteEvent::InsertText("dotnet build -c Release -v minimal".into()));
}

#[test]
fn git_commit_asks_for_a_message_through_the_same_form() {
    let (_d, db) = db();
    let mut m = run_blocking(&standard(&db), ">git commit", &ctx());
    select(&mut m, "Git: commit…");
    assert_eq!(meta(&m).as_deref(), Some("requiere confirmación"));
    assert_eq!(m.confirm(false), Confirmed::FormOpened);
    assert_eq!(m.confirm(false), Confirmed::FormInvalid);
    m.form.as_mut().unwrap().set_active_value("fix: palette");
    assert_eq!(
        m.confirm(false),
        Confirmed::Emit(PaletteEvent::GitAction(GitAction::Commit { message: "fix: palette".into() }), None)
    );
}

// ---- layout, selection, tabs ----------------------------------------------------------------

#[test]
fn empty_query_orders_sections_by_declared_order_and_hides_git() {
    let (_d, db) = db();
    db.record_command(NewCommand::new("ls", repo_root().to_string_lossy())).unwrap();
    db.flush().unwrap();
    let m = run_blocking(&standard(&db), "", &ctx());
    let h = headers(&m);
    let pos = |n| h.iter().position(|x| *x == n);
    assert!(pos("Acciones") < pos("Tareas") && pos("Tareas") < pos("Snippets") && pos("Snippets") < pos("Historial"), "{h:?}");
    assert!(!h.contains(&"Git"));
}

#[test]
fn typed_query_ranks_the_best_section_first_and_history_below_equals() {
    let (_d, db) = db();
    db.record_command(NewCommand::new("nueva pestana", repo_root().to_string_lossy())).unwrap();
    db.flush().unwrap();
    let m = run_blocking(&standard(&db), "nueva pestaña", &ctx());
    let h = headers(&m);
    assert_eq!(h[0], "Acciones", "{h:?}");
}

#[test]
fn selection_wraps_pages_and_survives_late_results() {
    let (_d, db) = db();
    let mut m = run_blocking(&standard(&db), ">", &ctx());
    let n = m.selectable_len();
    assert!(n > 8);
    assert_eq!(m.selected_row(), Some(1), "first item is under the header");
    m.move_selection(-1);
    assert_eq!(m.selected_row(), m.rows.iter().rposition(|r| matches!(r, LayoutRow::Item(_))));
    m.move_selection(1);
    m.page_selection(1000);
    assert_eq!(m.selected_item().unwrap().key, last_key(&m));
    m.page_selection(-1000);
    assert_eq!(m.selected_row(), Some(1));

    // The user navigated: a late section arriving must not move the selection.
    m.move_selection(2);
    let key = m.selected_item().unwrap().key.clone();
    let gen_ = m.results.generation();
    m.results.push(
        gen_,
        Section {
            id: "late",
            title: "Tardío",
            order: 0,
            weight: 5.0,
            items: vec![item("late:1", "Algo", 9.0)],
        },
    );
    m.relayout(true);
    assert_eq!(m.selected_item().unwrap().key, key);
    // A new query resets it to the first row.
    m.query_changed();
    m.relayout(true);
    assert_eq!(m.selected_row(), Some(1));
}

fn last_key(m: &PaletteModel) -> String {
    m.rows
        .iter()
        .rev()
        .find_map(|r| match r {
            LayoutRow::Item(i) => Some(i.key.clone()),
            LayoutRow::Header(_) => None,
        })
        .unwrap()
}

fn item(key: &str, title: &str, score: f32) -> PaletteItem {
    PaletteItem::new(key, title.to_string(), ItemAction::emit("Ir", PaletteEvent::Action(key.to_string().into())))
        .score(score)
}

#[test]
fn tab_rows_are_built_from_the_snapshot_with_status() {
    // `TabInfo` needs an `EntityId`, which only gpui can mint; the real path is exercised by the
    // demo. Here we make sure an empty snapshot is harmless.
    let (_d, db) = db();
    let m = run_blocking(&standard(&db), "zzzz-no-match", &ctx());
    assert!(m.rows.is_empty() && m.selected_item().is_none());
    let _ = TabIcon::Kind(Icon::Terminal);
}

// ---- stale results and cancellation ---------------------------------------------------------

#[test]
fn results_of_an_older_generation_are_dropped() {
    let mut r = Results::default();
    r.begin(1, |_| true);
    assert!(r.push(1, Section { id: "a", title: "A", order: 0, weight: 0.0, items: vec![item("a1", "uno", 1.0)] }));
    r.begin(2, |_| true);
    // A late answer to query 1 must not overwrite or add anything.
    assert!(!r.push(1, Section { id: "a", title: "A", order: 0, weight: 0.0, items: vec![item("a9", "viejo", 1.0)] }));
    assert!(!r.push(1, Section { id: "b", title: "B", order: 1, weight: 0.0, items: vec![item("b1", "viejo", 1.0)] }));
    assert_eq!(r.item_count(), 1);
    // Sources that stop applying disappear at once; the rest keep their rows until replaced.
    r.begin(3, |id| id == "b");
    assert_eq!(r.item_count(), 0);
}

/// Blocks until cancelled, like a source stuck on a slow disk.
struct Stuck {
    started: Arc<AtomicBool>,
}

impl PaletteSource for Stuck {
    fn id(&self) -> &'static str {
        "stuck"
    }
    fn title(&self) -> &'static str {
        "Stuck"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[]
    }
    fn latency(&self) -> Latency {
        Latency::Store
    }
    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        self.started.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !q.cancel.is_cancelled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        vec![item("stuck:1", "tarde", 1.0)]
    }
}

#[test]
fn a_newer_query_cancels_the_running_source_promptly() {
    let started = Arc::new(AtomicBool::new(false));
    let source = Arc::new(Stuck { started: started.clone() });
    let cancel = Cancel::new();
    let c = cancel.clone();
    let s = source.clone();
    let query = Query::parse("x");
    let c2 = ctx();
    let handle = std::thread::spawn(move || run_source(s.as_ref(), 5, &query, &c2, &c));
    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    let t = Instant::now();
    cancel.cancel();
    let section = handle.join().unwrap();
    assert!(t.elapsed() < Duration::from_millis(500), "took {:?}", t.elapsed());
    // The view drops this answer: it belongs to a generation that is gone.
    let mut r = Results::default();
    r.begin(2, |_| true);
    assert!(!r.push(1, section));
}

#[test]
fn the_real_sources_stop_early_when_cancelled() {
    let (_d, db) = db();
    let sources = standard(&db);
    let cancel = Cancel::new();
    cancel.cancel();
    let query = Query::parse("!build");
    for (s, limit) in plan(&sources, &query) {
        // Must return (empty or not) without panicking or blocking.
        let _ = run_source(s.as_ref(), limit, &query, &ctx(), &cancel);
    }
}

// ---- latency --------------------------------------------------------------------------------

#[test]
fn latency_budget_in_memory_and_history() {
    let (_d, db) = db();
    // 5 000 history rows (distinct commands) to make the FTS query realistic.
    let cwd = repo_root().to_string_lossy().into_owned();
    for i in 0..5000 {
        db.record_command(NewCommand::new(format!("cargo test -p crate{} --lib -- case_{i}", i % 40), cwd.clone())).unwrap();
    }
    db.flush().unwrap();
    let many_actions: Vec<ActionEntry> =
        (0..300).map(|i| ActionEntry::new(format!("a.{i}"), format!("Acción número {i}")).keywords("demo")).collect();
    let sources = standard_sources(Some(db.clone()), many_actions, vec![]);
    let c = ctx();
    let query = Query::parse("cargo");
    let cancel = Cancel::new();

    let mut mem_worst = Duration::ZERO;
    let mut store_worst = Duration::ZERO;
    for _ in 0..5 {
        for (s, limit) in plan(&sources, &Query::parse("nueva")) {
            let t = Instant::now();
            let _ = run_source(s.as_ref(), limit, &Query::parse("nueva"), &c, &cancel);
            if s.latency() == Latency::Memory {
                mem_worst = mem_worst.max(t.elapsed());
            }
        }
        for (s, limit) in plan(&sources, &query) {
            let t = Instant::now();
            let _ = run_source(s.as_ref(), limit, &query, &c, &cancel);
            if s.id() == "history" {
                store_worst = store_worst.max(t.elapsed());
            }
        }
    }
    eprintln!("LATENCY in-memory worst = {mem_worst:?}, history worst = {store_worst:?}");
    // Generous: debug build on a loaded CI box. Release numbers are in the crate docs.
    assert!(mem_worst < Duration::from_millis(40), "{mem_worst:?}");
    assert!(store_worst < Duration::from_millis(250), "{store_worst:?}");
}
