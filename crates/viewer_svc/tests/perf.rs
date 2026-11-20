#![allow(clippy::unwrap_used, clippy::expect_used)]

//! PLAN 3b criterion, enforced only in release builds (`cargo test --release`):
//! open + first 100 lines of a 10 MB file in under 100 ms.

use viewer_svc::{Document, Limits};

#[test]
#[cfg(not(debug_assertions))]
fn open_10mb_and_first_100_lines_under_100ms() {
    use std::io::Write;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.txt");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
    let mut total = 0;
    let mut i = 0;
    while total < 10 * 1024 * 1024 {
        let line = format!("{i:08} the quick brown fox jumps over the lazy dog\r\n");
        total += line.len();
        f.write_all(line.as_bytes()).unwrap();
        i += 1;
    }
    f.flush().unwrap();
    drop(f);

    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let d = Document::open(&p, Limits::default()).unwrap();
        let lines = d.lines(0..100);
        let took = t.elapsed();
        assert_eq!(lines.len(), 100);
        best = best.min(took);
    }
    assert!(
        best < Duration::from_millis(100),
        "open + first 100 lines took {best:?}"
    );
}

/// Debug builds still exercise the same path, without the timing assertion.
#[test]
fn open_10mb_and_first_100_lines_works() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.txt");
    let line = "0123456789 the quick brown fox jumps over the lazy dog\n";
    std::fs::write(&p, line.repeat(10 * 1024 * 1024 / line.len())).unwrap();
    let d = Document::open(&p, Limits::default()).unwrap();
    assert_eq!(d.lines(0..100).len(), 100);
}
