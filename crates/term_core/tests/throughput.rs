//! Output-throughput sanity check: `cmd /c type <20 MB file>` through ConPTY, one snapshot per
//! wakeup (the way the app consumes a session). Prints the elapsed time and MB/s; run with
//! `cargo test -p term_core --test throughput --release -- --nocapture` to compare numbers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::executor::block_on;
use term_core::{Palette, Session, SessionConfig, SessionEvent};

const SIZE_MB: usize = 20;

fn config(program: &str, args: Vec<String>) -> SessionConfig {
    SessionConfig {
        program: program.to_string(),
        args,
        cwd: None,
        cols: 120,
        rows: 40,
        cell_width: 8,
        cell_height: 16,
        palette: Palette::default(),
        scrollback: 10_000,
    }
}

/// Writes ~`SIZE_MB` MB of colored text lines and returns the path.
fn make_file() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("selachi-throughput-{}.txt", std::process::id()));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut written = 0usize;
    let mut i = 0usize;
    while written < SIZE_MB * 1024 * 1024 {
        let line = format!(
            "\x1b[3{}mline {i:08}\x1b[0m lorem ipsum dolor sit amet, consectetur adipiscing elit {}\r\n",
            i % 8,
            "x".repeat(i % 37)
        );
        out.write_all(line.as_bytes()).unwrap();
        written += line.len();
        i += 1;
    }
    path
}

/// Runs the flood with `make_session` and returns the elapsed time.
fn flood(
    label: &str,
    pace: Duration,
    spawn: impl FnOnce(
        SessionConfig,
    ) -> (
        Session,
        futures::channel::mpsc::UnboundedReceiver<SessionEvent>,
    ),
) -> Duration {
    let path = make_file();
    let cmd = format!("type {} & echo FLOOD-DONE", path.display());
    let (session, mut rx) = spawn(config("cmd.exe", vec!["/c".into(), cmd]));
    // Watchdog: a stuck session must not hang the test run.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(120));
        eprintln!("throughput test watchdog: aborting");
        std::process::exit(2);
    });

    let start = Instant::now();
    let mut snap = term_core::Snapshot::default();
    let mut wakeups = 0u64;
    let mut last = Instant::now();
    let mut exited = false;
    while !exited {
        match block_on(rx.next()) {
            Some(SessionEvent::Wakeup) => {
                wakeups += 1;
                if let Some(wait) = pace.checked_sub(last.elapsed()) {
                    std::thread::sleep(wait);
                }
                last = Instant::now();
                session.snapshot_into(&mut snap);
            }
            Some(SessionEvent::Exit) | None => exited = true,
            Some(_) => {}
        }
    }
    session.snapshot_into(&mut snap);
    let elapsed = start.elapsed();
    let _ = std::fs::remove_file(&path);
    assert!(
        (0..snap.rows).any(|r| snap.line_text(r).contains("FLOOD-DONE")),
        "end marker not found:\n{}",
        snap.text()
    );
    println!(
        "THROUGHPUT[{label}] {SIZE_MB} MB in {:.2?} = {:.1} MB/s ({wakeups} wakeups)",
        elapsed,
        SIZE_MB as f64 / elapsed.as_secs_f64()
    );
    elapsed
}

/// A snapshot after every wakeup (worst case for the consumer).
#[test]
fn type_20mb_snapshot_per_wakeup() {
    let elapsed = flood("per-wakeup", Duration::ZERO, |cfg| {
        Session::spawn(cfg).expect("spawn cmd")
    });
    assert!(elapsed < Duration::from_secs(90), "too slow: {elapsed:?}");
}

/// A snapshot at most every 16 ms, like a 60 Hz UI.
#[test]
fn type_20mb_snapshot_at_60hz() {
    let elapsed = flood("60hz", Duration::from_millis(16), |cfg| {
        Session::spawn(cfg).expect("spawn cmd")
    });
    assert!(elapsed < Duration::from_secs(60), "too slow: {elapsed:?}");
}
