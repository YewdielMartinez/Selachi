//! `cargo run -p tasks_svc --release --example bench -- [project_root]`
//! Prints discovery time (cold/warm), cache-hit time and the tasks found.

use std::path::PathBuf;
use std::time::Instant;
use tasks_svc::{discover_tasks_with, DiscoverOptions, TaskCache};

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let opts = DiscoverOptions {
        use_just_cli: false,
    };
    let t = Instant::now();
    let tasks = discover_tasks_with(&root, &opts);
    println!("first scan : {:?} ({} tasks)", t.elapsed(), tasks.len());
    let mut best = std::time::Duration::MAX;
    for _ in 0..20 {
        let t = Instant::now();
        let _ = discover_tasks_with(&root, &opts);
        best = best.min(t.elapsed());
    }
    println!("best of 20 : {best:?}");
    let cache = TaskCache::with_options(opts);
    let _ = cache.get_or_scan(&root);
    let mut hit = std::time::Duration::MAX;
    for _ in 0..20 {
        let t = Instant::now();
        let _ = cache.get_or_scan(&root);
        hit = hit.min(t.elapsed());
    }
    println!("cache hit  : {hit:?}");
    if std::env::var_os("LIST").is_some() {
        for t in &tasks {
            println!("  [{}] {} -> {}", t.kind.as_str(), t.label, t.command);
        }
    }
}
