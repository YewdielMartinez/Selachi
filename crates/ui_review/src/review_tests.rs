//! Window-level tests of the review: real temp repositories, a real gpui test window.

use std::cell::RefCell;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use panel_api::{CloseCheck, TabModel};
use theme::Theme;

use crate::diff::ReviewSource;
use crate::note::{NoteId, NoteState, Side};
use crate::review::{ReviewEvent, ReviewView};
use crate::tab::ReviewTab;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "git {args:?}");
}

fn numbered(n: usize, changed: Option<usize>) -> String {
    (1..=n).map(|i| if Some(i) == changed { "CAMBIADA\n".to_string() } else { format!("linea {i}\n") }).collect()
}

/// A repo with `src.txt` (20 lines, line 10 changed) and `other.txt` (one added line).
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("src.txt"), numbered(20, None)).unwrap();
    std::fs::write(root.join("other.txt"), "uno\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    std::fs::write(root.join("src.txt"), numbered(20, Some(10))).unwrap();
    std::fs::write(root.join("other.txt"), "uno\ndos\n").unwrap();
    dir
}

type Opened<'a> = (Entity<ReviewView>, &'a mut VisualTestContext, Rc<RefCell<Vec<ReviewEvent>>>);

fn open<'a>(cx: &'a mut TestAppContext, root: &Path) -> Opened<'a> {
    cx.update(crate::init);
    let source = ReviewSource::Worktree { root: root.to_path_buf() };
    let (view, vcx) = cx.add_window_view(move |_, cx| ReviewView::new(source, Theme::dark(), cx));
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = events.clone();
    let sub = vcx.update(|_, cx| cx.subscribe(&view, move |_, ev: &ReviewEvent, _| log.borrow_mut().push(ev.clone())));
    std::mem::forget(sub);
    vcx.run_until_parked();
    (view, vcx, events)
}

fn write_note(view: &Entity<ReviewView>, vcx: &mut VisualTestContext, side: Side, line: usize, text: &str) {
    view.update_in(vcx, |v, window, cx| v.begin_note(side, line, false, window, cx));
    view.update(vcx, |v, cx| {
        v.set_editor_text(text, cx);
        v.save_editor(cx);
    });
}

#[gpui::test]
fn loads_the_changed_files(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.read_with(vcx, |v, _| {
        assert!(!v.is_loading());
        assert!(v.load_error().is_none());
        let names: Vec<&str> = v.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["other.txt", "src.txt"]);
        assert_eq!(v.selected_path(), Some("other.txt"));
    });
}

#[gpui::test]
fn gutter_click_note_send_and_mark_sent(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, events) = open(cx, dir.path());
    view.update(vcx, |v, cx| v.select_path("src.txt", cx));
    view.update_in(vcx, |v, window, cx| v.begin_note(Side::New, 10, false, window, cx));
    view.read_with(vcx, |v, cx| {
        assert!(v.is_editing());
        assert_eq!(v.editor_text(cx).as_deref(), Some(""));
    });
    view.update(vcx, |v, cx| {
        v.set_editor_text("  esto debería ser una constante  ", cx);
        v.save_editor(cx);
    });
    view.read_with(vcx, |v, _| {
        assert!(!v.is_editing());
        assert_eq!(v.counts(), (1, 1));
        let n = &v.notes().notes()[0];
        assert_eq!((n.path.as_str(), n.start, n.end, n.side), ("src.txt", 10, 10, Side::New));
        assert_eq!(n.text, "esto debería ser una constante");
        assert_eq!(n.state, NoteState::Draft);
        assert!(n.snippet.iter().any(|(l, t)| *l == 10 && t == "CAMBIADA"));
    });
    assert!(events.borrow().contains(&ReviewEvent::NotesChanged { total: 1, drafts: 1 }));

    let sent = view.update(vcx, |v, cx| v.send(cx));
    assert!(sent);
    let (markdown, refs) = events
        .borrow()
        .iter()
        .find_map(|e| match e {
            ReviewEvent::SendToAgent { markdown, notes } => Some((markdown.clone(), notes.clone())),
            _ => None,
        })
        .unwrap();
    assert!(markdown.contains("### `src.txt:10`"), "{markdown}");
    assert!(markdown.contains("> esto debería ser una constante"));
    assert!(markdown.contains("> 10 | CAMBIADA"));
    assert_eq!(refs.len(), 1);
    assert_eq!((refs[0].path.as_str(), refs[0].line), ("src.txt", 10));

    // Until the host confirms, the note is still a draft.
    view.read_with(vcx, |v, _| assert_eq!(v.counts().1, 1));
    let ids: Vec<NoteId> = refs.iter().map(|r| r.id).collect();
    view.update(vcx, |v, cx| v.mark_sent(&ids, cx));
    view.read_with(vcx, |v, _| {
        assert_eq!(v.counts(), (1, 0));
        assert_eq!(v.notes().notes()[0].state, NoteState::Sent);
    });
    assert!(!view.update(vcx, |v, cx| v.send(cx)), "nothing left to send");
    view.update(vcx, |v, cx| v.mark_resolved(&ids, cx));
    view.read_with(vcx, |v, _| assert_eq!(v.notes().notes()[0].state, NoteState::Resolved));
}

#[gpui::test]
fn shift_click_extends_the_range(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.update(vcx, |v, cx| v.select_path("src.txt", cx));
    view.update_in(vcx, |v, window, cx| v.begin_note(Side::New, 9, false, window, cx));
    view.update_in(vcx, |v, window, cx| v.begin_note(Side::New, 12, true, window, cx));
    // Shift-click above the anchor flips the range around it.
    view.update_in(vcx, |v, window, cx| v.begin_note(Side::New, 7, true, window, cx));
    view.update(vcx, |v, cx| {
        v.set_editor_text("este bloque entero", cx);
        v.save_editor(cx);
    });
    view.read_with(vcx, |v, _| {
        let n = &v.notes().notes()[0];
        assert_eq!((n.start, n.end), (7, 9));
        assert_eq!(n.len(), 3);
    });
}

#[gpui::test]
fn removed_lines_get_notes_on_the_old_side(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.update(vcx, |v, cx| v.select_path("src.txt", cx));
    write_note(&view, vcx, Side::Old, 10, "¿por qué se quita?");
    view.read_with(vcx, |v, _| {
        let n = &v.notes().notes()[0];
        assert_eq!(n.side, Side::Old);
        assert!(n.snippet.iter().any(|(l, t)| *l == 10 && t == "linea 10"), "{:?}", n.snippet);
    });
}

#[gpui::test]
fn empty_text_creates_no_note_and_cancel_closes(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.update_in(vcx, |v, window, cx| v.begin_note(Side::New, 1, false, window, cx));
    view.update(vcx, |v, cx| {
        v.set_editor_text("   ", cx);
        v.save_editor(cx);
    });
    view.read_with(vcx, |v, _| {
        assert!(!v.is_editing());
        assert!(v.notes().is_empty());
    });
    view.update_in(vcx, |v, window, cx| v.begin_note(Side::New, 1, false, window, cx));
    view.update(vcx, |v, cx| v.cancel_editor(cx));
    view.read_with(vcx, |v, _| assert!(!v.is_editing()));
}

#[gpui::test]
fn notes_follow_their_lines_when_the_diff_refreshes(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.update(vcx, |v, cx| v.select_path("src.txt", cx));
    write_note(&view, vcx, Side::New, 10, "revisa esta línea");

    // The agent adds two lines at the top of the file: everything shifts down by two.
    let shifted = format!("nueva a\nnueva b\n{}", numbered(20, Some(10)));
    std::fs::write(dir.path().join("src.txt"), shifted).unwrap();
    view.update(vcx, |v, cx| v.refresh(cx));
    vcx.run_until_parked();
    view.read_with(vcx, |v, _| {
        let n = &v.notes().notes()[0];
        assert_eq!((n.start, n.end), (12, 12), "followed its line");
        assert!(!n.stale);
        assert_eq!(v.selected_path(), Some("src.txt"), "selection survives the refresh");
    });

    // The line is rewritten: the note stays where it was, marked outdated.
    std::fs::write(dir.path().join("src.txt"), numbered(20, None).replace("linea 10", "otra cosa")).unwrap();
    view.update(vcx, |v, cx| v.refresh(cx));
    vcx.run_until_parked();
    view.read_with(vcx, |v, _| assert!(v.notes().notes()[0].stale));
}

#[gpui::test]
fn drafts_come_back_when_the_review_is_reopened(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.update(vcx, |v, cx| v.select_path("src.txt", cx));
    write_note(&view, vcx, Side::New, 10, "guárdame");
    let json = view.read_with(vcx, |v, _| v.export_notes());

    let source = ReviewSource::Worktree { root: dir.path().to_path_buf() };
    let again = vcx.new(|cx| ReviewView::new(source, Theme::light(), cx));
    again.read_with(vcx, |v, _| assert_eq!(v.notes().len(), 1, "restored from the in-memory store"));

    // And across restarts through the JSON export / import.
    let other = tempfile::tempdir().unwrap();
    let (fresh, vcx2, _) = open(cx, other.path());
    let added = fresh.update(vcx2, |v, cx| v.import_notes(&json, cx));
    assert_eq!(added, Ok(1));
    assert_eq!(fresh.update(vcx2, |v, cx| v.import_notes(&json, cx)), Ok(0));
    assert!(fresh.update(vcx2, |v, cx| v.import_notes("{", cx)).is_err());
}

#[gpui::test]
fn edit_and_delete_notes(cx: &mut TestAppContext) {
    let dir = repo();
    let (view, vcx, _) = open(cx, dir.path());
    view.update(vcx, |v, cx| v.select_path("src.txt", cx));
    write_note(&view, vcx, Side::New, 10, "primera versión");
    let id = view.read_with(vcx, |v, _| v.notes().notes()[0].id);
    view.update(vcx, |v, cx| v.mark_sent(&[id], cx));

    view.update_in(vcx, |v, window, cx| v.edit_note(id, window, cx));
    view.read_with(vcx, |v, cx| assert_eq!(v.editor_text(cx).as_deref(), Some("primera versión")));
    view.update(vcx, |v, cx| {
        v.set_editor_text("segunda versión", cx);
        v.save_editor(cx);
    });
    view.read_with(vcx, |v, _| {
        let n = &v.notes().notes()[0];
        assert_eq!(n.text, "segunda versión");
        assert_eq!(n.state, NoteState::Draft, "an edited note has to be sent again");
    });

    view.update(vcx, |v, cx| v.delete_note(id, cx));
    view.read_with(vcx, |v, _| assert!(v.notes().is_empty()));
}

#[gpui::test]
fn a_non_repository_shows_an_error(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (view, vcx, _) = open(cx, dir.path());
    view.read_with(vcx, |v, _| assert!(v.load_error().is_some()));
}

#[gpui::test]
fn the_tab_reports_itself(cx: &mut TestAppContext) {
    let dir = repo();
    cx.update(crate::init);
    let source = ReviewSource::Worktree { root: dir.path().to_path_buf() };
    let tab = cx.new(|cx| ReviewTab::new(source, Theme::dark(), cx));
    cx.run_until_parked();
    tab.read_with(cx, |t, cx| {
        assert_eq!(t.title(cx).as_ref(), "Revisión");
        assert_eq!(t.subtitle(cx).as_ref().map(|s| s.as_ref()), Some("Cambios del worktree"));
        assert!(t.status(cx).is_none());
        assert_eq!(t.can_close(cx), CloseCheck::Allow);
        assert_eq!(t.cwd(cx).as_deref(), Some(dir.path()));
    });

    // A draft shows in the subtitle and the status, and closing asks first.
    let view = tab.read_with(cx, |t, _| t.view().clone());
    view.update(cx, |v, cx| {
        v.select_path("src.txt", cx);
        v.add_note("src.txt", Side::New, 10, 10, "una nota", cx);
    });
    tab.read_with(cx, |t, cx| {
        assert_eq!(t.subtitle(cx).as_ref().map(|s| s.as_ref()), Some("Cambios del worktree · 1 nota"));
        assert_eq!(t.status(cx), Some(ui_kit::RowStatus::Waiting));
        assert!(matches!(t.can_close(cx), CloseCheck::Confirm(_)));
    });
    let json = tab.read_with(cx, |t, cx| t.export_notes(cx));
    assert!(json.contains("una nota"));
}
