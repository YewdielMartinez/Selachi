//! CPU percentage from deltas between two snapshots.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use crate::snapshot::ProcInfo;

/// Computes per-process CPU usage between consecutive [`sample`](Self::sample)
/// calls. Values are "percent of one core" (100.0 = one core fully busy; a
/// 4-thread busy loop reads ~400). Divide by [`logical_cores`] for the Task
/// Manager style whole-machine percentage.
#[derive(Debug, Default)]
pub struct CpuSampler {
    prev: HashMap<u32, (Option<SystemTime>, Duration)>,
    last: Option<Instant>,
}

impl CpuSampler {
    /// New sampler. The first `sample` returns an empty map (no baseline).
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a snapshot taken "now" and returns `pid -> cpu%` for every process
    /// seen in both this and the previous sample. A reused pid (different
    /// start time) is treated as a new process.
    pub fn sample(&mut self, procs: &[ProcInfo]) -> HashMap<u32, f32> {
        self.sample_at(procs, Instant::now())
    }

    /// Like [`sample`](Self::sample) with an explicit timestamp (testable).
    pub fn sample_at(&mut self, procs: &[ProcInfo], now: Instant) -> HashMap<u32, f32> {
        let mut out = HashMap::new();
        let dt = self.last.map(|l| now.saturating_duration_since(l));
        if let Some(dt) = dt.filter(|d| *d >= Duration::from_millis(1)) {
            for p in procs.iter().filter(|p| !p.access_denied) {
                if let Some((start, cpu)) = self.prev.get(&p.pid) {
                    if *start == p.start_time && p.cpu_time >= *cpu {
                        let d = (p.cpu_time - *cpu).as_secs_f64();
                        out.insert(p.pid, (d / dt.as_secs_f64() * 100.0) as f32);
                    }
                }
            }
        } else if dt.is_some() {
            // Samples too close together: keep the old baseline.
            return out;
        }
        self.prev = procs
            .iter()
            .filter(|p| !p.access_denied)
            .map(|p| (p.pid, (p.start_time, p.cpu_time)))
            .collect();
        self.last = Some(now);
        out
    }

    /// Forgets the baseline (e.g. after the panel was closed for a long time).
    pub fn reset(&mut self) {
        self.prev.clear();
        self.last = None;
    }
}

/// Number of logical cores (at least 1).
pub fn logical_cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn p(pid: u32, cpu_ms: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid: 0,
            name: "x.exe".into(),
            exe_path: None,
            cmdline: None,
            start_time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            working_set: 0,
            private_bytes: 0,
            cpu_time: Duration::from_millis(cpu_ms),
            threads: 1,
            access_denied: false,
        }
    }

    #[test]
    fn computes_delta() {
        let mut s = CpuSampler::new();
        let t0 = Instant::now();
        assert!(s.sample_at(&[p(1, 0)], t0).is_empty());
        let r = s.sample_at(&[p(1, 500), p(2, 10)], t0 + Duration::from_secs(1));
        assert!((r[&1] - 50.0).abs() < 0.01);
        assert!(!r.contains_key(&2), "new pid has no baseline");
    }

    #[test]
    fn pid_reuse_is_not_a_delta() {
        let mut s = CpuSampler::new();
        let t0 = Instant::now();
        s.sample_at(&[p(1, 1000)], t0);
        let mut reused = p(1, 5);
        reused.start_time = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(200));
        assert!(s.sample_at(&[reused], t0 + Duration::from_secs(1)).is_empty());
    }
}
