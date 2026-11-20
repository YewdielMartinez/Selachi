//! Integration tests that spawn real shells through ConPTY.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use term_core::{
    Palette, Rgb8, Session, SessionConfig, SessionEvent, default_shell, install_pwsh_integration,
    pwsh_integration_args,
};

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

#[test]
fn set_palette_restyles_the_next_snapshot() {
    let (session, _rx) = Session::spawn(config("cmd.exe", &[])).expect("spawn cmd");
    assert!(wait_for(&session, ">"));
    let before = session.snapshot();
    assert_eq!(before.lines[0][0].bg, Palette::default().background);

    let light = Palette {
        background: Rgb8::new(0xff, 0xff, 0xff),
        foreground: Rgb8::new(0x10, 0x10, 0x10),
        ..Palette::default()
    };
    session.set_palette(light.clone());
    let after = session.snapshot();
    assert_eq!(after.lines[0][0].bg, light.background);
    assert_ne!(before.row_hash[0], after.row_hash[0], "row cache key must change with the palette");
    let text_cell = after.lines[0].iter().find(|c| c.ch != ' ').expect("prompt text");
    assert_eq!(text_cell.fg, light.foreground);
}

/// Waits for an event matching `pick` (max 15 s), ignoring the others.
fn wait_event<T>(
    rx: &mut futures::channel::mpsc::UnboundedReceiver<SessionEvent>,
    mut pick: impl FnMut(SessionEvent) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        while let Ok(ev) = rx.try_recv() {
            if let Some(v) = pick(ev) {
                return Some(v);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn osc7_and_osc133_survive_conpty() {
    let (program, _) = default_shell();
    let script = "[Console]::Write([char]27 + ']7;file://PC/C:/Windows/System32' + [char]7 + [char]27 + ']133;D;5' + [char]7)";
    let (session, mut rx) =
        Session::spawn(config(&program, &["-NoLogo", "-NoProfile", "-Command", script]))
            .expect("spawn powershell");
    let cwd = wait_event(&mut rx, |e| match e {
        SessionEvent::Cwd(p) => Some(p),
        _ => None,
    });
    assert_eq!(cwd.as_deref(), Some(std::path::Path::new(r"C:\Windows\System32")));
    let code = wait_event(&mut rx, |e| match e {
        SessionEvent::CommandFinished { exit_code } => Some(exit_code),
        _ => None,
    });
    assert_eq!(code, Some(Some(5)));
    drop(session);
}

fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| p.to_string_lossy().to_lowercase().trim_end_matches(['\\', '/']).to_string();
    norm(a) == norm(b)
}

#[test]
fn shell_integration_reports_cwd_and_exit_codes() {
    let (program, _) = default_shell();
    let script = install_pwsh_integration().expect("write integration script");
    // -NoProfile keeps the test independent of the user's profile.
    let mut args = pwsh_integration_args(&script);
    args.insert(1, "-NoProfile".to_string());
    let mut cfg = config(&program, &[]);
    cfg.args = args;
    let (session, mut rx) = Session::spawn(cfg).expect("spawn integrated shell");

    // The first prompt reports the starting directory.
    assert!(wait_event(&mut rx, |e| matches!(e, SessionEvent::Cwd(_)).then_some(())).is_some());

    session.write(&b"cd $env:TEMP\r"[..]);
    let temp = std::env::temp_dir();
    let reached = wait_event(&mut rx, |e| match e {
        SessionEvent::Cwd(p) if same_dir(&p, &temp) => Some(()),
        _ => None,
    });
    assert!(reached.is_some(), "no Cwd for TEMP:\n{}", session.snapshot().text());

    session.write(&b"cmd /c exit 3\r"[..]);
    let code = wait_event(&mut rx, |e| match e {
        SessionEvent::CommandFinished { exit_code } => Some(exit_code),
        _ => None,
    });
    assert_eq!(code, Some(Some(3)), "screen:\n{}", session.snapshot().text());

    session.write(&b"Write-Output ok-$((1+1))\r"[..]);
    let code = wait_event(&mut rx, |e| match e {
        SessionEvent::CommandFinished { exit_code } => Some(exit_code),
        _ => None,
    });
    assert_eq!(code, Some(Some(0)));
    assert!(wait_for(&session, "ok-2"));
}

#[test]
fn shell_integration_chains_an_existing_prompt() {
    let (program, _) = default_shell();
    let script = install_pwsh_integration().expect("write integration script");
    let mut args = pwsh_integration_args(&script);
    args.insert(1, "-NoProfile".to_string());
    // Simulates a profile that defines its own prompt before the integration loads.
    let load = args.pop().expect("command");
    args.push(format!("function global:prompt {{ 'custom-prompt> ' }}; {load}"));
    let mut cfg = config(&program, &[]);
    cfg.args = args;
    let (session, mut rx) = Session::spawn(cfg).expect("spawn integrated shell");
    assert!(wait_event(&mut rx, |e| matches!(e, SessionEvent::Cwd(_)).then_some(())).is_some());
    assert!(wait_for(&session, "custom-prompt>"), "screen:\n{}", session.snapshot().text());
}
