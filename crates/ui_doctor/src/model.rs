//! `DoctorModel`: samples Selachi itself every second **only while the panel is visible**.
//!
//! Hidden = nothing runs (no timer). Visible = [`DoctorModel::set_visible`] or a painting view
//! calling [`DoctorModel::touch`] each render; if painting stops for more than two intervals the
//! loop ends by itself. Sampling runs on the background executor.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{Context, Task};

use crate::frames::FrameStats;
use crate::health::{AppStats, DbPaths, FrameInfo, Health, RawSample, Ring, cpu_percent};
use crate::source::{DoctorSource, SystemSource};

/// Time between samples while visible.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub struct DoctorModel {
    source: Arc<dyn DoctorSource>,
    frames: Arc<FrameStats>,
    frames_at_open: u64,
    stats: AppStats,
    db_paths: DbPaths,
    interval: Duration,
    health: Arc<Health>,
    ram: Ring,
    cpu: Ring,
    prev: Option<(Instant, Duration)>,
    visible: bool,
    viewers: usize,
    last_touch: Option<Instant>,
    loop_running: bool,
    loop_task: Option<Task<()>>,
    in_flight: bool,
}

impl DoctorModel {
    pub fn new() -> Self {
        Self::with_source(Arc::new(SystemSource))
    }

    pub fn with_source(source: Arc<dyn DoctorSource>) -> Self {
        Self {
            source,
            frames: FrameStats::new(),
            frames_at_open: 0,
            stats: AppStats::default(),
            db_paths: Vec::new(),
            interval: SAMPLE_INTERVAL,
            health: Arc::new(Health::default()),
            ram: Ring::default(),
            cpu: Ring::default(),
            prev: None,
            visible: false,
            viewers: 0,
            last_touch: None,
            loop_running: false,
            loop_task: None,
            in_flight: false,
        }
    }

    // ----- hooks the app feeds ---------------------------------------------------------------

    /// The shared counters of the frame probes ([`frame_begin`](crate::frame_begin) /
    /// [`frame_end`](crate::frame_end)). Clone it into the shell's root view.
    pub fn frame_stats(&self) -> Arc<FrameStats> {
        self.frames.clone()
    }

    /// Reports one painted frame measured by the app. Atomics only: never notifies.
    pub fn record_frame(&self, d: Duration) {
        self.frames.record(d);
    }

    /// Shell / agent session counts and loaded grammars. Call it when they change.
    pub fn set_app_stats(&mut self, stats: AppStats, cx: &mut Context<Self>) {
        if self.stats != stats {
            self.stats = stats.clone();
            let mut h = (*self.health).clone();
            h.stats = stats;
            self.health = Arc::new(h);
            cx.notify();
        }
    }

    /// Database files to size, in display order (`("selachi.db", path)`, `("state.db", path)`).
    pub fn set_db_paths(&mut self, paths: DbPaths) {
        self.db_paths = paths;
    }

    // ----- reads -----------------------------------------------------------------------------

    pub fn health(&self) -> &Arc<Health> {
        &self.health
    }

    pub fn is_sampling(&self) -> bool {
        self.loop_running
    }

    pub fn markdown(&self, version: &str) -> String {
        self.health.to_markdown(version)
    }

    // ----- visibility ------------------------------------------------------------------------

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && !self.visible {
            self.frames_at_open = self.frames.frames();
        }
        self.visible = visible;
        if visible {
            self.last_touch = None;
            self.start_loop(cx);
        } else {
            self.stop_loop();
        }
    }

    /// Heartbeat from every render of a view. Never notifies.
    pub fn touch(&mut self, cx: &mut Context<Self>) {
        self.last_touch = Some(Instant::now());
        if !self.visible || !self.loop_running {
            let keep = self.last_touch;
            self.set_visible(true, cx);
            self.last_touch = keep;
        }
    }

    pub fn attach_view(&mut self) {
        self.viewers += 1;
    }

    pub fn detach_view(&mut self, cx: &mut Context<Self>) {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers == 0 {
            self.set_visible(false, cx);
        }
    }

    fn start_loop(&mut self, cx: &mut Context<Self>) {
        if self.loop_running {
            return;
        }
        self.loop_running = true;
        self.sample(cx);
        let interval = self.interval;
        self.loop_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let keep = this
                    .update(cx, |m, cx| {
                        let stale = m.last_touch.is_some_and(|t| {
                            t.elapsed() > interval * 2 + Duration::from_millis(500)
                        });
                        if !m.visible || stale {
                            m.visible = false;
                            m.loop_running = false;
                            m.prev = None;
                            return false;
                        }
                        m.sample(cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn stop_loop(&mut self) {
        self.loop_task = None;
        self.loop_running = false;
        self.prev = None;
    }

    /// Takes a sample now (never two in flight).
    pub fn sample(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        let source = self.source.clone();
        let dbs = self.db_paths.clone();
        cx.spawn(async move |this, cx| {
            let raw = cx
                .background_executor()
                .spawn(async move { source.sample(&dbs) })
                .await;
            let _ = this.update(cx, |m, cx| m.apply(raw, cx));
        })
        .detach();
    }

    fn apply(&mut self, raw: RawSample, cx: &mut Context<Self>) {
        self.in_flight = false;
        let cpu = self
            .prev
            .and_then(|p| cpu_percent(p, (raw.at, raw.cpu_time), raw.cores));
        self.prev = Some((raw.at, raw.cpu_time));
        self.ram.push(raw.working_set as f32);
        self.cpu.push(cpu.unwrap_or(0.0));
        let uptime = raw
            .started
            .and_then(|s| SystemTime::now().duration_since(s).ok());
        self.health = Arc::new(Health {
            pid: raw.pid,
            working_set: raw.working_set,
            private_bytes: raw.private_bytes,
            cpu,
            threads: raw.threads,
            children: raw.children,
            frames: FrameInfo {
                since_open: self.frames.frames().saturating_sub(self.frames_at_open),
                last: self.frames.last(),
                slowest: self.frames.take_slowest(),
            },
            stats: self.stats.clone(),
            dbs: raw.dbs,
            uptime,
            ram_history: self.ram.to_vec(),
            cpu_history: self.cpu.to_vec(),
            has_data: true,
        });
        cx.notify();
    }
}

impl Default for DoctorModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::health::{ChildProc, DbSize};

    pub struct FakeSource {
        pub calls: AtomicUsize,
        pub base: Instant,
    }

    impl FakeSource {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                base: Instant::now(),
            })
        }

        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl DoctorSource for FakeSource {
        fn sample(&self, dbs: &[(String, std::path::PathBuf)]) -> RawSample {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) as u64;
            RawSample {
                at: self.base + Duration::from_secs(n),
                working_set: (50 + n) * 1024 * 1024,
                private_bytes: 30 * 1024 * 1024,
                cpu_time: Duration::from_millis(n * 400),
                threads: 12,
                children: vec![ChildProc {
                    pid: 9,
                    name: "pwsh.exe".into(),
                    working_set: 80 * 1024 * 1024,
                }],
                dbs: dbs
                    .iter()
                    .map(|(l, _)| DbSize {
                        label: l.clone(),
                        bytes: Some(1024),
                    })
                    .collect(),
                started: Some(SystemTime::now() - Duration::from_secs(90)),
                cores: 4,
                pid: 1,
            }
        }
    }
}
