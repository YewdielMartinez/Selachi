//! Cost of `Ingestor::scan()` against the real logs: `scan_bench <db> [iterations] [sleep_secs]`.
//! Point `<db>` at a COPY of `selachi.db`; prints wall time and what each scan did.
use agents_svc::{IngestConfig, Ingestor};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let db = a.next().ok_or_else(|| anyhow::anyhow!("usage: scan_bench <db> [iterations] [sleep_secs]"))?;
    let n: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let sleep: u64 = a.next().and_then(|s| s.parse().ok()).unwrap_or(2);
    let mut ing = Ingestor::open(std::path::Path::new(&db), IngestConfig::from_home())?;
    for i in 0..n {
        let t = Instant::now();
        let r = ing.scan()?;
        println!(
            "scan {i}: {:>7.1} ms  seen={} scanned={} unchanged={} bytes={} ins={} upd={} errs={}",
            t.elapsed().as_secs_f64() * 1e3,
            r.files_seen,
            r.files_scanned,
            r.files_unchanged,
            r.bytes_read,
            r.events_inserted,
            r.events_updated,
            r.errors.len()
        );
        std::thread::sleep(Duration::from_secs(sleep));
    }
    Ok(())
}
