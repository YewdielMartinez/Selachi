//! Integration tests that spawn real shells through ConPTY.

use std::time::{Duration, Instant};

use term_core::{Palette, Session, SessionConfig, SessionEvent, default_shell};

fn config(program: &str, args: &[&str]) -> SessionConfig {
    SessionConfig {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: None,
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
        palette: Palette::default(),
        scrollback: 1000,
    }
}

/// Polls snapshots until some line contains `needle` (max 10 s).
fn wait_for(session: &Session, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let snap = session.snapshot();
        if (0..snap.rows).any(|r| snap.line_text(r).contains(needle)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn cmd_echo_shows_output() {
    let (session, mut rx) =
        Session::spawn(config("cmd.exe", &["/c", "echo", "selachi-ok"])).expect("spawn cmd");
    assert!(wait_for(&session, "selachi-ok"), "output not found:\n{}", session.snapshot().text());
    assert!(session.pid().is_some());

    // The child exits right away; an Exit event must arrive (Wakeups may precede it).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline && !exited {
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, SessionEvent::Exit) {
                exited = true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(exited, "no Exit event");
}

#[test]
fn powershell_interactive_roundtrip() {
    let (program, _) = default_shell();
    let (session, _rx) = Session::spawn(config(&program, &["-NoLogo", "-NoProfile"]))
        .expect("spawn powershell");
    // Computed output so that the typed command line cannot match by itself.
    session.write(&b"Write-Output \"selachi-$((20+22))\"\r"[..]);
    assert!(wait_for(&session, "selachi-42"), "output not found:\n{}", session.snapshot().text());
}

#[test]
fn resize_updates_grid_and_keeps_working() {
    let (session, _rx) = Session::spawn(config("cmd.exe", &[])).expect("spawn cmd");
    assert!(wait_for(&session, ">"), "no prompt:\n{}", session.snapshot().text());
    let snap = session.snapshot();
    assert_eq!((snap.cols, snap.rows), (80, 24));

    session.resize(100, 30, 8, 16);
    let snap = session.snapshot();
    assert_eq!((snap.cols, snap.rows), (100, 30));
    assert_eq!(snap.lines.len(), 30);
    assert!(snap.lines.iter().all(|l| l.len() == 100));

    // Unchanged resize is a no-op.
    session.resize(100, 30, 8, 16);
    assert_eq!(session.snapshot().cols, 100);

    session.write(&b"echo selachi-resized\r\n"[..]);
    assert!(
        wait_for(&session, "selachi-resized"),
        "output not found:\n{}",
        session.snapshot().text()
    );
}

#[test]
fn wakeup_is_coalesced_until_snapshot() {
    let (session, mut rx) = Session::spawn(config("cmd.exe", &[])).expect("spawn cmd");
    assert!(wait_for(&session, ">"));
    // Drain whatever accumulated, then re-arm.
    while rx.try_recv().is_ok() {}
    let _ = session.snapshot();
    session.write(&b"echo a\r\n"[..]);
    session.write(&b"echo b\r\n"[..]);
    std::thread::sleep(Duration::from_millis(800));
    let wakeups = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|e| matches!(e, SessionEvent::Wakeup))
        .count();
    assert_eq!(wakeups, 1, "wakeups must coalesce until snapshot() is called");
}
