//! Real pwsh through ConPTY: command blocks (OSC 133 + OSC 633;E) and the PSReadLine settings
//! the integration script applies from environment variables.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedReceiver;
use term_core::{
    Block, BlockId, BlockState, Palette, Session, SessionConfig, SessionEvent, default_shell,
    install_pwsh_integration, pwsh_integration_args,
};

fn config(program: &str) -> SessionConfig {
    SessionConfig {
        program: program.to_string(),
        args: Vec::new(),
        cwd: None,
        cols: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 16,
        palette: Palette::default(),
        scrollback: 1000,
    }
}

/// Waits for an event matching `pick` (max 15 s), ignoring the others.
fn wait_event<T>(
    rx: &mut UnboundedReceiver<SessionEvent>,
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

/// Starts the integrated pwsh with `env`; `pre` runs before the integration loads (it plays the
/// part of the user's profile).
fn integrated(env: &[(&str, &str)], pre: &str) -> (Session, UnboundedReceiver<SessionEvent>) {
    let (program, _) = default_shell();
    let script = install_pwsh_integration().expect("write integration script");
    let mut args = pwsh_integration_args(&script);
    args.insert(1, "-NoProfile".to_string());
    if !pre.is_empty() {
        let load = args.pop().expect("command");
        args.push(format!("{pre}; {load}"));
    }
    let mut cfg = config(&program);
    cfg.args = args;
    let env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let (session, mut rx) = Session::spawn_with_env(cfg, env).expect("spawn integrated shell");
    // The first prompt reports the starting directory: the integration is loaded.
    assert!(wait_event(&mut rx, |e| matches!(e, SessionEvent::Cwd(_)).then_some(())).is_some());
    (session, rx)
}

/// Polls until `n` blocks exist and all are finished (max 20 s).
fn wait_blocks(session: &Session, n: usize) -> Vec<Block> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let blocks = session.blocks();
        if blocks.len() >= n && blocks.iter().all(Block::is_finished) {
            return blocks;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("expected {n} finished blocks, got {:?}\nscreen:\n{}", session.blocks(), session.snapshot().text());
}

/// Types `cmd`, waits for its block and returns the trimmed output.
fn run_command(session: &Session, cmd: &str) -> String {
    let n = session.blocks().len();
    session.write(format!("{cmd}\r").into_bytes());
    let blocks = wait_blocks(session, n + 1);
    let id = blocks.last().expect("a block").id;
    session.block_output_text(id, 10_000).expect("output text")
}

#[test]
fn real_pwsh_produces_blocks_with_exit_codes_and_commands() {
    let (session, _rx) = integrated(&[], "");
    session.write(&b"Write-Output hi-1\r"[..]);
    wait_blocks(&session, 1);
    session.write(&b"cmd /c exit 3\r"[..]);
    wait_blocks(&session, 2);
    session.write(&b"Write-Output \"hi-$((1+2))\"; 'x;y'\r"[..]);
    let blocks = wait_blocks(&session, 3);

    assert_eq!(blocks.len(), 3, "{blocks:?}");
    let cmds: Vec<_> = blocks.iter().map(|b| b.command.as_deref()).collect();
    assert_eq!(
        cmds,
        [Some("Write-Output hi-1"), Some("cmd /c exit 3"), Some("Write-Output \"hi-$((1+2))\"; 'x;y'")]
    );
    let codes: Vec<_> = blocks.iter().map(|b| (b.state, b.exit_code)).collect();
    assert_eq!(
        codes,
        [(BlockState::Succeeded, Some(0)), (BlockState::Failed, Some(3)), (BlockState::Succeeded, Some(0))]
    );
    assert!(blocks.iter().all(|b| b.duration.is_some() && b.cwd.is_some()));
    assert!(blocks.windows(2).all(|w| w[0].output_end.is_some_and(|e| e <= w[1].prompt_line)));

    assert_eq!(session.block_output_text(blocks[0].id, 1000).as_deref(), Some("hi-1"));
    assert_eq!(session.block_output_text(blocks[1].id, 1000).as_deref(), Some(""));
    assert_eq!(session.block_output_text(blocks[2].id, 1000).as_deref(), Some("hi-3\nx;y"));

    // The viewport row of the first output line belongs to the first block.
    let snap = session.snapshot();
    let row = (0..snap.rows).find(|&r| snap.line_text(r).trim() == "hi-1").expect("row of hi-1");
    assert_eq!(session.block_at_line(row), Some(blocks[0].id));
    assert_eq!(session.prev_block(Some(blocks[2].id)), Some(blocks[1].id));
    assert_eq!(session.next_block(Some(blocks[0].id)), Some(blocks[1].id));
    assert!(session.scroll_to_block(blocks[0].id));
    assert!(!session.scroll_to_block(BlockId(9999)));
}

#[cfg(feature = "block-events")]
#[test]
fn real_pwsh_emits_block_events() {
    let (session, mut rx) = integrated(&[], "");
    session.write(&b"cmd /c exit 4\r"[..]);
    let id = wait_event(&mut rx, |e| match e {
        SessionEvent::BlockStarted(id) => Some(id),
        _ => None,
    })
    .expect("BlockStarted");
    let (fid, code) = wait_event(&mut rx, |e| match e {
        SessionEvent::BlockFinished { id, exit_code, .. } => Some((id, exit_code)),
        _ => None,
    })
    .expect("BlockFinished");
    assert_eq!((fid, code), (id, Some(4)));
}

#[test]
fn smart_tab_accepts_the_inline_suggestion() {
    let (session, _rx) = integrated(&[("SELACHI_TAB_MODE", "smart")], "");
    // Seed the history, then type a prefix: the gray suggestion appears after it.
    session.write(
        &b"[Microsoft.PowerShell.PSConsoleReadLine]::AddToHistory('echo selachi-history-marker-xyz')\r"[..],
    );
    wait_blocks(&session, 1);
    session.write(&b"echo selachi-hist"[..]);
    assert!(
        wait_for(&session, "selachi-history-marker-xyz"),
        "no inline suggestion rendered:\n{}",
        session.snapshot().text()
    );
    // Tab must complete the line (not cycle completions): Enter then runs the full command.
    session.write(&b"\t"[..]);
    std::thread::sleep(Duration::from_millis(300));
    session.write(&b"\r"[..]);
    let blocks = wait_blocks(&session, 2);
    assert_eq!(blocks[1].command.as_deref(), Some("echo selachi-history-marker-xyz"));
    assert_eq!(
        session.block_output_text(blocks[1].id, 1000).as_deref(),
        Some("selachi-history-marker-xyz")
    );
}

#[test]
fn smart_tab_falls_back_to_the_menu_without_a_suggestion() {
    let (session, _rx) = integrated(&[("SELACHI_TAB_MODE", "smart")], "");
    // A unique directory and no history entry that matches: Tab has nothing to accept.
    let dir = std::env::temp_dir().join(format!("selachi-tab-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("zzqq-unique-dir")).unwrap();
    session.write(format!("cd '{}'\r", dir.display()).into_bytes());
    wait_blocks(&session, 1);
    session.write(&b"cd .\\zzqq-uni"[..]);
    std::thread::sleep(Duration::from_millis(300));
    session.write(&b"\t"[..]);
    std::thread::sleep(Duration::from_millis(300));
    session.write(&b"\r"[..]);
    let blocks = wait_blocks(&session, 2);
    assert_eq!(blocks[1].command.as_deref(), Some("cd .\\zzqq-unique-dir\\"), "completion did not apply");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The function name PSReadLine reports for `key`, read through a real block.
fn bound(session: &Session, key: &str) -> String {
    run_command(
        session,
        &format!("(Get-PSReadLineKeyHandler -Bound | Where-Object Key -eq '{key}').Function"),
    )
}

#[test]
fn tab_modes_map_to_the_right_bindings() {
    for (mode, tab, shift_tab) in [
        ("smart", "SelachiSmartTab", "TabCompletePrevious"),
        ("complete", "TabCompleteNext", "TabCompletePrevious"),
        ("menu", "MenuComplete", "TabCompletePrevious"),
        ("native", "TabCompleteNext", "TabCompletePrevious"),
    ] {
        let (session, _rx) = integrated(&[("SELACHI_TAB_MODE", mode)], "");
        assert_eq!(bound(&session, "Tab"), tab, "mode {mode}");
        assert_eq!(bound(&session, "Shift+Tab"), shift_tab, "mode {mode}");
    }
}

#[test]
fn native_and_implicit_modes_respect_the_users_own_bindings() {
    let user = "Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete; Set-PSReadLineKeyHandler -Key Ctrl+RightArrow -Function ShellForwardWord";
    // native: untouched.
    let (s, _rx) = integrated(&[("SELACHI_TAB_MODE", "native"), ("SELACHI_ACCEPT_WORD_KEY", "native")], user);
    assert_eq!(bound(&s, "Tab"), "MenuComplete");
    assert_eq!(bound(&s, "Ctrl+RightArrow"), "ShellForwardWord");
    // Variables absent (implicit defaults): the user's custom bindings win...
    let (s, _rx) = integrated(&[], user);
    assert_eq!(bound(&s, "Tab"), "MenuComplete");
    assert_eq!(bound(&s, "Ctrl+RightArrow"), "ShellForwardWord");
    // ...but an explicit choice overrides them.
    let (s, _rx) = integrated(&[("SELACHI_TAB_MODE", "smart"), ("SELACHI_ACCEPT_WORD_KEY", "Ctrl+RightArrow")], user);
    assert_eq!(bound(&s, "Tab"), "SelachiSmartTab");
    assert_eq!(bound(&s, "Ctrl+RightArrow"), "SelachiAcceptWord");
    // Stock bindings are replaced by the defaults.
    let (s, _rx) = integrated(&[], "");
    assert_eq!(bound(&s, "Tab"), "SelachiSmartTab");
    assert_eq!(bound(&s, "Ctrl+RightArrow"), "SelachiAcceptWord");
}

#[test]
fn prediction_options_and_accept_word_key_follow_the_environment() {
    let opt = |s: &Session, name: &str| run_command(s, &format!("(Get-PSReadLineOption).{name}"));
    for (var, want) in [("history", "History"), ("history_and_plugin", "HistoryAndPlugin"), ("off", "None")] {
        let (s, _rx) = integrated(&[("SELACHI_PREDICTION", var)], "");
        let got = opt(&s, "PredictionSource");
        // PSReadLine 2.1 has no plugin predictions: the script falls back to History.
        let ok = got == want || (var == "history_and_plugin" && got == "History");
        assert!(ok, "SELACHI_PREDICTION={var}: got {got}");
    }
    let (s, _rx) =
        integrated(&[("SELACHI_PREDICTION", "native")], "Set-PSReadLineOption -PredictionSource None");
    assert_eq!(opt(&s, "PredictionSource"), "None");

    let (s, _rx) = integrated(&[("SELACHI_PREDICTION_VIEW", "list")], "");
    assert_eq!(opt(&s, "PredictionViewStyle"), "ListView");
    let (s, _rx) = integrated(&[], "");
    assert_eq!(opt(&s, "PredictionViewStyle"), "InlineView");
    assert_eq!(opt(&s, "PredictionSource"), "History");
    let (s, _rx) = integrated(
        &[("SELACHI_PREDICTION_VIEW", "native")],
        "Set-PSReadLineOption -PredictionViewStyle ListView",
    );
    assert_eq!(opt(&s, "PredictionViewStyle"), "ListView");

    let (s, _rx) = integrated(&[("SELACHI_ACCEPT_WORD_KEY", "Alt+j")], "");
    assert_eq!(bound(&s, "Alt+j"), "SelachiAcceptWord");
}
