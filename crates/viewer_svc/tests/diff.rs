#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use viewer_svc::{diff_against_git, diff_text, Error, GitBase, Hunk, LineKind};

fn kinds(h: &Hunk) -> Vec<LineKind> {
    h.lines.iter().map(|l| l.kind).collect()
}

#[test]
fn identical_texts_have_no_hunks() {
    assert!(diff_text("a\nb\n", "a\nb\n").is_empty());
    assert!(diff_text("", "").is_empty());
}

#[test]
fn eol_style_alone_is_not_a_change() {
    assert!(diff_text("a\r\nb\r\n", "a\nb\n").is_empty());
}

#[test]
fn single_change_with_context() {
    let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let new = "1\n2\n3\n4\nFIVE\n6\n7\n8\n9\n";
    let hunks = diff_text(old, new);
    assert_eq!(hunks.len(), 1);
    let h = &hunks[0];
    assert_eq!((h.old_start, h.old_len), (2, 7));
    assert_eq!((h.new_start, h.new_len), (2, 7));
    assert_eq!(h.header(), "@@ -2,7 +2,7 @@");
    assert_eq!((h.added(), h.removed()), (1, 1));
    let removed = h
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Removed)
        .unwrap();
    assert_eq!(removed.text, "5");
    assert_eq!(removed.old_line, Some(5));
    assert_eq!(removed.new_line, None);
    let added = h.lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
    assert_eq!(added.text, "FIVE");
    assert_eq!(added.new_line, Some(5));
    assert_eq!(added.old_line, None);
}

#[test]
fn distant_changes_split_into_hunks() {
    let old: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    let new = old
        .replace("line 3\n", "THREE\n")
        .replace("line 38\n", "THIRTY-EIGHT\n");
    let hunks = diff_text(&old, &new);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].old_start, 1);
    assert!(hunks[1].old_start > 30);
}

#[test]
fn pure_addition_and_removal() {
    let h = diff_text("a\nb\n", "a\nb\nc\nd\n");
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].added(), h[0].removed()), (2, 0));
    assert!(h[0].lines.iter().all(|l| l.inline.is_empty()));

    let h = diff_text("a\nb\nc\n", "a\n");
    assert_eq!((h[0].added(), h[0].removed()), (0, 2));

    let h = diff_text("", "x\ny\n");
    assert_eq!(h.len(), 1);
    assert_eq!(kinds(&h[0]), vec![LineKind::Added, LineKind::Added]);
    assert_eq!((h[0].old_start, h[0].old_len), (1, 0));
}

#[test]
fn inline_word_diff_marks_changed_words() {
    let hunks = diff_text(
        "let value = compute(1, 2);\n",
        "let value = compute(1, 3);\n",
    );
    let h = &hunks[0];
    let removed = h
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Removed)
        .unwrap();
    let added = h.lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
    assert!(!removed.inline.is_empty());
    assert!(!added.inline.is_empty());
    let hl = |l: &viewer_svc::DiffLine| -> String {
        l.inline.iter().map(|r| &l.text[r.clone()]).collect()
    };
    assert_eq!(hl(removed), "2");
    assert_eq!(hl(added), "3");
    // ranges are valid char boundaries
    for r in added.inline.iter().chain(&removed.inline) {
        assert!(added.text.len() >= r.end || removed.text.len() >= r.end);
    }
}

#[test]
fn inline_ranges_handle_unicode() {
    let hunks = diff_text("héllo wörld\n", "héllo wörld!\n");
    let added = hunks[0]
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Added)
        .unwrap();
    for r in &added.inline {
        assert!(added.text.is_char_boundary(r.start) && added.text.is_char_boundary(r.end));
    }
}

#[test]
fn completely_rewritten_line_has_no_inline_noise() {
    let h = diff_text("aaaa\n", "bbbb\n");
    for l in &h[0].lines {
        assert!(l.inline.is_empty(), "{l:?}");
    }
}

#[test]
fn missing_final_newline_is_just_a_changed_line() {
    let h = diff_text("a\nb", "a\nb\n");
    assert_eq!(h.len(), 1);
    assert!(h[0].lines.iter().any(|l| l.kind != LineKind::Context));
}

// ---- git ---------------------------------------------------------------------------

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn diff_against_head_and_commit() {
    if !git_available() {
        eprintln!("git not installed, skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    fs::create_dir_all(root.join("src")).unwrap();
    let file = root.join("src").join("lib.rs");
    fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "one"]);
    let first = rev_parse(root, "HEAD");
    fs::write(&file, "fn a() {}\nfn b2() {}\nfn c() {}\n").unwrap();
    git(root, &["commit", "-qam", "two"]);
    fs::write(&file, "fn a() {}\nfn b2() {}\nfn c() { 1 }\n").unwrap();

    // working tree vs HEAD: only `c` changed
    let h = diff_against_git(root, &file, GitBase::Head).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].added(), h[0].removed()), (1, 1));

    // vs first commit: b changed, c added, plus the working edit
    let h = diff_against_git(root, &file, GitBase::Commit(first)).unwrap();
    assert_eq!(h.len(), 1);
    assert!(h[0].added() >= 2 && h[0].removed() >= 1);

    // relative paths work too
    let h = diff_against_git(root, Path::new("src/lib.rs"), GitBase::Head).unwrap();
    assert_eq!(h.len(), 1);
}

#[test]
fn untracked_and_deleted_files() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "x\ny\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "one"]);

    // new file not in HEAD: everything is added
    let new = root.join("new.txt");
    fs::write(&new, "1\n2\n3\n").unwrap();
    let h = diff_against_git(root, &new, GitBase::Head).unwrap();
    assert_eq!((h[0].added(), h[0].removed()), (3, 0));

    // deleted from disk: everything is removed
    let a = root.join("a.txt");
    fs::remove_file(&a).unwrap();
    let h = diff_against_git(root, &a, GitBase::Head).unwrap();
    assert_eq!((h[0].added(), h[0].removed()), (0, 2));

    // in neither place
    let err = diff_against_git(root, &root.join("ghost.txt"), GitBase::Head).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
}

#[test]
fn repo_without_commits_diffs_as_all_added() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    let f = root.join("a.txt");
    fs::write(&f, "x\n").unwrap();
    let h = diff_against_git(root, &f, GitBase::Head).unwrap();
    assert_eq!((h[0].added(), h[0].removed()), (1, 0));
}

#[test]
fn bad_revisions_and_paths_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    fs::write(&f, "x\n").unwrap();
    for bad in ["", "--output=x", "a:b", "a b"] {
        let err = diff_against_git(dir.path(), &f, GitBase::Commit(bad.into())).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{bad:?}: {err:?}");
    }
    // outside of the repo root
    let other = tempfile::tempdir().unwrap();
    let g = other.path().join("b.txt");
    fs::write(&g, "x\n").unwrap();
    let err = diff_against_git(dir.path(), &g, GitBase::Head).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
}

#[test]
fn not_a_repo_is_a_git_error() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    fs::write(&f, "x\n").unwrap();
    let err = diff_against_git(dir.path(), &f, GitBase::Head).unwrap_err();
    assert!(matches!(err, Error::Git(_)), "{err:?}");
}
