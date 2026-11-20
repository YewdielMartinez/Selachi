//! PLAN 3b criterion: opening a 10 MB file and reading its first 100 lines must take
//! under 100 ms (release build).
//!
//! `cargo run --release -p viewer_svc --example bench`
//!
//! The binary also serves as the probe for per-grammar binary size: build it with
//! `--no-default-features --features lang-xxx` and compare the executable sizes.

use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use viewer_svc::{available_languages, Document, Limits};

const RUNS: usize = 15;
const BUDGET: Duration = Duration::from_millis(100);

fn generate(path: &Path, target_bytes: usize, source: bool) -> std::io::Result<usize> {
    let mut f = BufWriter::new(File::create(path)?);
    let mut total = 0;
    let mut i = 0usize;
    while total < target_bytes {
        let line = if source {
            format!("pub fn function_{i}(a: u32, b: &str) -> Option<u32> {{ let s = \"text {i}\"; Some(a + {i}) }} // c{i}\n")
        } else {
            format!("{i:08} the quick brown fox jumps over the lazy dog 0123456789 abcdef\r\n")
        };
        f.write_all(line.as_bytes())?;
        total += line.len();
        i += 1;
    }
    f.flush()?;
    Ok(i)
}

fn median(v: &mut [Duration]) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("viewer_svc_bench_{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let mut failed = false;

    println!("grammars compiled in: {:?}", available_languages());

    // 1. the criterion: 10 MB plain text, CRLF
    let txt = dir.join("big10.txt");
    let lines = generate(&txt, 10 * 1024 * 1024, false)?;
    let mut open_only = Vec::new();
    let mut open_100 = Vec::new();
    for _ in 0..RUNS {
        let t = Instant::now();
        let d = Document::open(&txt, Limits::default())?;
        open_only.push(t.elapsed());
        let first = d.lines(0..100);
        open_100.push(t.elapsed());
        assert_eq!(first.len(), 100);
        drop(d);
    }
    let m = median(&mut open_100);
    println!(
        "10 MB text ({lines} lines): open = {:?} (median), open + first 100 lines = {:?} (median), max = {:?}",
        median(&mut open_only),
        m,
        open_100.iter().max().copied().unwrap_or_default()
    );
    if m >= BUDGET {
        failed = true;
        eprintln!("FAIL: open + first 100 lines took {m:?} (budget {BUDGET:?})");
    }

    // 2. same file, also asking for the minimap and highlight spans of the first screen
    let t = Instant::now();
    let d = Document::open(&txt, Limits::default())?;
    let _ = d.lines(0..100);
    let _ = d.highlight_lines(0..100);
    let _ = d.minimap(0..100, 50);
    println!(
        "10 MB text: first screen incl. highlight+minimap = {:?}",
        t.elapsed()
    );
    drop(d);
    let d = Document::open_lazy(&txt, Limits::default())?;
    let t = Instant::now();
    let p = d.wait_indexed();
    println!(
        "10 MB text: full line index ({} lines) = {:?}",
        p.lines,
        t.elapsed()
    );
    drop(d);

    // 3. a 4 MB Rust file: whole-file tree-sitter parse in the background
    let rs = dir.join("big4.rs");
    generate(&rs, 4 * 1024 * 1024, true)?;
    let t = Instant::now();
    let d = Document::open(&rs, Limits::default())?;
    let first = d.lines(0..100);
    let spans = d.highlight_lines(0..100); // window fallback until the parse is done
    println!(
        "4 MB Rust: open + 100 lines + window highlight ({} spans on line 0) = {:?}",
        spans.first().map_or(0, Vec::len),
        t.elapsed()
    );
    assert_eq!(first.len(), 100);
    let t = Instant::now();
    while !d.highlight_ready() && available_languages().contains(&viewer_svc::Lang::Rust) {
        if t.elapsed() > Duration::from_secs(60) {
            eprintln!("whole-file highlight did not finish in 60 s");
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!(
        "4 MB Rust: whole-file highlight ready after {:?}",
        t.elapsed()
    );
    drop(d);

    // 4. a 40 MB file (under the 50 MB limit): memory-mapped, highlight off
    let big = dir.join("big40.log");
    generate(&big, 40 * 1024 * 1024, false)?;
    let t = Instant::now();
    let d = Document::open(&big, Limits::default())?;
    let _ = d.lines(0..100);
    println!("40 MB text: open + first 100 lines = {:?}", t.elapsed());
    // jump deep into the file: indexes on demand up to there
    let t = Instant::now();
    let deep = d.lines(300_000..300_050);
    println!(
        "40 MB text: lines 300000..300050 ({} lines) = {:?}",
        deep.len(),
        t.elapsed()
    );
    drop(d);

    let _ = std::fs::remove_dir_all(&dir);
    if failed {
        return Err("performance budget exceeded".into());
    }
    Ok(())
}
