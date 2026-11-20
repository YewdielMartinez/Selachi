#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use viewer_svc::{find_paths, resolve, PathMatch};

fn cwd() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\work\proj")
    } else {
        PathBuf::from("/work/proj")
    }
}

/// `(line text, expected matched text, expected raw path, line, col)`
type Row = (
    &'static str,
    &'static str,
    &'static str,
    Option<u32>,
    Option<u32>,
);

fn one(line: &str) -> PathMatch {
    let m = find_paths(line, &cwd());
    assert_eq!(m.len(), 1, "{line:?} -> {m:?}");
    m.into_iter().next().unwrap()
}

#[test]
fn table_of_recognised_forms() {
    let rows: &[Row] = &[
        // relative, with locations
        (
            "src/app.ts:42:7",
            "src/app.ts:42:7",
            "src/app.ts",
            Some(42),
            Some(7),
        ),
        (
            "src/app.ts:42",
            "src/app.ts:42",
            "src/app.ts",
            Some(42),
            None,
        ),
        ("src/app.ts", "src/app.ts", "src/app.ts", None, None),
        (
            "src\\app.ts:3",
            "src\\app.ts:3",
            "src\\app.ts",
            Some(3),
            None,
        ),
        // MSVC
        (
            r"C:\x\y.rs(12,5)",
            r"C:\x\y.rs(12,5)",
            r"C:\x\y.rs",
            Some(12),
            Some(5),
        ),
        (
            r"C:\x\y.rs(12)",
            r"C:\x\y.rs(12)",
            r"C:\x\y.rs",
            Some(12),
            None,
        ),
        (
            r"C:\x\y.cpp(12, 5): error C2065",
            r"C:\x\y.cpp(12, 5)",
            r"C:\x\y.cpp",
            Some(12),
            Some(5),
        ),
        (
            "main.cpp(8,2): warning",
            "main.cpp(8,2)",
            "main.cpp",
            Some(8),
            Some(2),
        ),
        // absolute Windows + colon location
        (
            r"C:\Users\me\f.rs:10:2",
            r"C:\Users\me\f.rs:10:2",
            r"C:\Users\me\f.rs",
            Some(10),
            Some(2),
        ),
        (
            r"D:/code/a.py:5",
            "D:/code/a.py:5",
            "D:/code/a.py",
            Some(5),
            None,
        ),
        // UNC
        (
            r"\\server\share\f.txt",
            r"\\server\share\f.txt",
            r"\\server\share\f.txt",
            None,
            None,
        ),
        // dot-relative and home
        ("./a/b", "./a/b", "./a/b", None, None),
        (r".\a\b.rs:9", r".\a\b.rs:9", r".\a\b.rs", Some(9), None),
        ("../x/y.md", "../x/y.md", "../x/y.md", None, None),
        (r"~\x", r"~\x", r"~\x", None, None),
        (
            "~/notes/todo.md:2",
            "~/notes/todo.md:2",
            "~/notes/todo.md",
            Some(2),
            None,
        ),
        // unix absolute
        ("/usr/bin/env", "/usr/bin/env", "/usr/bin/env", None, None),
        (
            "/etc/hosts.conf:4",
            "/etc/hosts.conf:4",
            "/etc/hosts.conf",
            Some(4),
            None,
        ),
        // bare names
        ("Cargo.toml", "Cargo.toml", "Cargo.toml", None, None),
        ("README.md:10", "README.md:10", "README.md", Some(10), None),
        ("a.out:7", "a.out:7", "a.out", Some(7), None),
        // hidden dirs
        (
            ".github/workflows/ci.yml",
            ".github/workflows/ci.yml",
            ".github/workflows/ci.yml",
            None,
            None,
        ),
        // github anchors
        (
            "src/lib.rs#L33",
            "src/lib.rs#L33",
            "src/lib.rs",
            Some(33),
            None,
        ),
        (
            "src/lib.rs#L33-L40",
            "src/lib.rs#L33",
            "src/lib.rs",
            Some(33),
            None,
        ),
    ];
    for (line, matched, raw, l, c) in rows {
        let ms = find_paths(line, &cwd());
        assert_eq!(ms.len(), 1, "{line:?} -> {ms:?}");
        let m = &ms[0];
        assert_eq!(&line[m.range.clone()], *matched, "range of {line:?}");
        assert_eq!(m.raw, *raw, "raw of {line:?}");
        assert_eq!((m.line, m.col), (*l, *c), "location of {line:?}");
    }
}

#[test]
fn surrounded_by_prose_and_punctuation() {
    let cases: &[(&str, &str, Option<u32>)] = &[
        (
            "error at src/app.ts:42:7: unexpected token",
            "src/app.ts:42:7",
            Some(42),
        ),
        ("see (src/app.ts:42) for details", "src/app.ts:42", Some(42)),
        ("modified: src/app.ts.", "src/app.ts", None),
        ("wrote [out/log.txt], done", "out/log.txt", None),
        ("  --> src/main.rs:5:3", "src/main.rs:5:3", Some(5)),
        ("--config=conf/app.toml", "conf/app.toml", None),
        ("PATH=./bin/tool.sh", "./bin/tool.sh", None),
        ("src/app.ts:42:7:", "src/app.ts:42:7", Some(42)),
        ("a.rs, b.rs", "a.rs", None),
    ];
    for (line, matched, l) in cases {
        let ms = find_paths(line, &cwd());
        assert!(!ms.is_empty(), "{line:?}");
        assert_eq!(&line[ms[0].range.clone()], *matched, "{line:?}");
        assert_eq!(ms[0].line, *l, "{line:?}");
    }
}

#[test]
fn several_paths_in_one_line() {
    let line = "diff src/a.rs:1 and ./b/c.ts:2:3 vs C:\\z\\d.py(4)";
    let ms = find_paths(line, &cwd());
    let found: Vec<_> = ms.iter().map(|m| &line[m.range.clone()]).collect();
    assert_eq!(found, ["src/a.rs:1", "./b/c.ts:2:3", "C:\\z\\d.py(4)"]);
    // ranges are ordered and disjoint
    for w in ms.windows(2) {
        assert!(w[0].range.end <= w[1].range.start);
    }
}

#[test]
fn quoted_paths_may_contain_spaces() {
    let m = one(r#"open "C:\Program Files\App\run me.exe" now"#);
    assert_eq!(m.raw, r"C:\Program Files\App\run me.exe");
    assert_eq!(
        &r#"open "C:\Program Files\App\run me.exe" now"#[m.range],
        r"C:\Program Files\App\run me.exe"
    );

    let m = one("cat 'my docs/notes v2.md:12'");
    assert_eq!(m.raw, "my docs/notes v2.md");
    assert_eq!(m.line, Some(12));

    let line = "see `src/lib.rs:7`";
    let m = one(line);
    assert_eq!(&line[m.range], "src/lib.rs:7");
}

#[test]
fn python_traceback_line() {
    let line = r#"  File "/home/u/app/main.py", line 17, in <module>"#;
    let m = one(line);
    assert_eq!(m.raw, "/home/u/app/main.py");
    assert_eq!(m.line, Some(17));
    assert_eq!(&line[m.range], r#"/home/u/app/main.py", line 17"#);

    let line = r#"  File "C:\proj\app.py", line 3"#;
    let m = one(line);
    assert_eq!(m.line, Some(3));
}

#[test]
fn quoted_text_that_is_not_a_path_still_scans_inside() {
    let line = r#"echo "see src/a.rs:3 please""#;
    let m = one(line);
    assert_eq!(&line[m.range], "src/a.rs:3");
    // apostrophes in prose do not swallow a following path
    let line = "don't edit src/b.rs";
    assert_eq!(&line[one(line).range], "src/b.rs");
}

#[test]
fn not_paths() {
    for line in [
        "",
        "hello world",
        "12:30:45 started",
        "version 1.2.3 released",
        "https://example.com/a/b.html",
        "http://localhost:3000/x.js",
        "git@github.com:user/repo.git",
        "and/or either",
        "2024/01/02 done",
        "// a comment",
        "cmd /c dir",
        "e.g. this",
        "a -> b",
        "user@example.com",
        "10.0.0.1:8080",
        "100%",
        "C:",
        "error[E0308]: mismatched types",
        "fn(x)",
        "/ /",
    ] {
        let m = find_paths(line, &cwd());
        assert!(m.is_empty(), "{line:?} -> {m:?}");
    }
}

#[test]
fn unicode_is_handled_with_byte_ranges() {
    let line = "résumé → src/café.rs:3 ✓";
    let m = one(line);
    assert_eq!(&line[m.range.clone()], "src/café.rs:3");
    assert_eq!(m.line, Some(3));
}

#[test]
fn paths_are_absolutized_and_normalized() {
    let c = cwd();
    assert_eq!(one("src/app.ts").path, c.join("src").join("app.ts"));
    assert_eq!(one("./a/./b.rs").path, c.join("a").join("b.rs"));
    assert_eq!(
        one("../up/x.rs").path,
        c.parent().unwrap().join("up").join("x.rs")
    );
    assert_eq!(one("a/../b/c.rs").path, c.join("b").join("c.rs"));
    if cfg!(windows) {
        assert_eq!(one(r"D:\a\..\b.rs").path, PathBuf::from(r"D:\b.rs"));
    }
}

#[test]
fn home_expansion_uses_the_environment() {
    let m = one("~/x/y.rs");
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(h) = home {
        assert_eq!(m.path, PathBuf::from(h).join("x").join("y.rs"));
    }
}

#[test]
fn resolve_checks_existence_lazily() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src").join("app.ts"), "x").unwrap();

    let ms = find_paths("src/app.ts:42:7 and src/missing.ts:1 and src/", dir.path());
    assert_eq!(ms.len(), 2, "{ms:?}");
    // find_paths itself never touches the disk: both are reported
    let hit = resolve(&ms[0]).unwrap();
    assert!(!hit.is_dir);
    assert!(hit.path.ends_with(Path::new("src").join("app.ts")));
    assert!(resolve(&ms[1]).is_none());
    assert!(ms[1].resolve().is_none());

    let dirs = find_paths("./src/", dir.path());
    let r = resolve(&dirs[0]).unwrap();
    assert!(r.is_dir);
}

#[test]
fn very_long_and_degenerate_lines_do_not_panic() {
    let long = "a/".repeat(50_000) + "b.rs";
    let _ = find_paths(&long, &cwd());
    let _ = find_paths(&"\"".repeat(10_000), &cwd());
    let _ = find_paths("\u{1b}[31mred src/a.rs:1\u{1b}[0m", &cwd());
    let _ = find_paths(":::((()))###LL", &cwd());
    let _ = find_paths("a.rs(", &cwd());
    let _ = find_paths("a.rs()", &cwd());
    let _ = find_paths("a.rs(999999999999,1)", &cwd());
}
