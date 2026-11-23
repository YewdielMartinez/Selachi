//! The data of the doctor: raw samples, the derived [`Health`] and its markdown form. Pure.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Samples kept for the sparklines.
pub const HISTORY: usize = 60;

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

/// `58 MB`, `1,2 GB`, `340 KB` (binary units, Spanish decimal comma).
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= GIB {
        let gb = bytes as f64 / GIB as f64;
        if gb >= 10.0 { format!("{} GB", gb.round() as u64) } else { format!("{} GB", format!("{gb:.1}").replace('.', ",")) }
    } else if bytes >= 10 * MIB {
        format!("{} MB", bytes / MIB)
    } else if bytes >= MIB {
        format!("{} MB", format!("{:.1}", bytes as f64 / MIB as f64).replace('.', ","))
    } else {
        format!("{} KB", bytes / KIB)
    }
}

/// `0%`, `3,4%`, `12%`.
pub fn format_pct(pct: f32) -> String {
    if !pct.is_finite() || pct < 0.05 {
        "0%".into()
    } else if pct < 10.0 {
        format!("{}%", format!("{pct:.1}").replace('.', ","))
    } else {
        format!("{}%", pct.round() as u32)
    }
}

/// `2,3 ms`, `18 ms`, `1,2 s`.
pub fn format_ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{} ms", format!("{ms:.1}").replace('.', ","))
    } else if ms < 1000.0 {
        format!("{} ms", ms.round() as u64)
    } else {
        format!("{} s", format!("{:.1}", ms / 1000.0).replace('.', ","))
    }
}

/// `45s`, `12m`, `2h 05m`, `3d 4h`.
pub fn format_uptime(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3_599 => format!("{}m", s / 60),
        3_600..=86_399 => format!("{}h {:02}m", s / 3_600, (s % 3_600) / 60),
        _ => format!("{}d {}h", s / 86_400, (s % 86_400) / 3_600),
    }
}

/// Thousands separated with a thin dot-less space: `12 400`.
pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// A child process of Selachi (a shell, `OpenConsole`, an agent...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildProc {
    pub pid: u32,
    pub name: String,
    pub working_set: u64,
}

/// One database file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbSize {
    pub label: String,
    /// `None` when the file does not exist yet.
    pub bytes: Option<u64>,
}

/// What one pass of a [`DoctorSource`](crate::DoctorSource) measured.
#[derive(Clone, Debug, PartialEq)]
pub struct RawSample {
    pub at: std::time::Instant,
    pub working_set: u64,
    pub private_bytes: u64,
    /// Kernel + user CPU time of the process so far.
    pub cpu_time: Duration,
    pub threads: u32,
    pub children: Vec<ChildProc>,
    pub dbs: Vec<DbSize>,
    pub started: Option<SystemTime>,
    pub cores: usize,
    pub pid: u32,
}

/// Counts the app reports when they change (no polling).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppStats {
    pub shells: Option<usize>,
    pub agents: Option<usize>,
    /// Loaded language grammar plugins.
    pub grammars: Vec<String>,
    /// Shells tracked by process tree because Windows would not put them in a Job Object
    /// (`proc_svc::TrackingMode::TreeOnly`, for example the Microsoft Store `pwsh`).
    pub tree_tracked: usize,
}

impl AppStats {
    /// No counts yet; fill what the host knows with the `with_*` builders (a new field then does
    /// not break the caller the way a struct literal would).
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_shells(mut self, shells: usize) -> Self {
        self.shells = Some(shells);
        self
    }

    pub fn with_agents(mut self, agents: usize) -> Self {
        self.agents = Some(agents);
        self
    }

    pub fn with_grammars(mut self, grammars: Vec<String>) -> Self {
        self.grammars = grammars;
        self
    }

    pub fn with_tree_tracked(mut self, tracked: usize) -> Self {
        self.tree_tracked = tracked;
        self
    }
}

/// DB files to size, in display order.
pub type DbPaths = Vec<(String, PathBuf)>;

/// Frame numbers read from the shell's [`FrameStats`](crate::FrameStats).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    /// Frames painted since the panel was last opened.
    pub since_open: u64,
    pub last: Option<Duration>,
    /// Slowest frame since the previous sample.
    pub slowest: Option<Duration>,
}

/// Everything the view shows and the markdown reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub pid: u32,
    pub working_set: u64,
    pub private_bytes: u64,
    /// % of the whole machine; `None` until two samples exist.
    pub cpu: Option<f32>,
    pub threads: u32,
    pub children: Vec<ChildProc>,
    pub frames: FrameInfo,
    pub stats: AppStats,
    pub dbs: Vec<DbSize>,
    pub uptime: Option<Duration>,
    /// Working-set bytes, oldest first.
    pub ram_history: Vec<f32>,
    /// CPU %, oldest first.
    pub cpu_history: Vec<f32>,
    pub has_data: bool,
}

impl Health {
    pub fn children_ram(&self) -> u64 {
        self.children.iter().map(|c| c.working_set).sum()
    }

    /// Markdown for "Copiar diagnóstico".
    pub fn to_markdown(&self, version: &str) -> String {
        let na = "—";
        let mut s = String::new();
        s.push_str(&format!("## Selachi doctor ({version})\n\n"));
        s.push_str(&format!("- **PID:** {}\n", self.pid));
        s.push_str(&format!("- **Tiempo activo:** {}\n", self.uptime.map_or(na.to_string(), format_uptime)));
        s.push_str(&format!(
            "- **RAM:** {} (working set) · {} (privada)\n",
            format_bytes(self.working_set),
            format_bytes(self.private_bytes)
        ));
        s.push_str(&format!("- **CPU:** {}\n", self.cpu.map_or(na.to_string(), format_pct)));
        s.push_str(&format!("- **Hilos:** {}\n", self.threads));
        s.push_str(&format!(
            "- **Procesos hijos:** {} · {}\n",
            self.children.len(),
            format_bytes(self.children_ram())
        ));
        for c in &self.children {
            s.push_str(&format!("  - {} (pid {}) · {}\n", c.name, c.pid, format_bytes(c.working_set)));
        }
        s.push_str(&format!("- **Fotogramas desde que se abrió el doctor:** {}\n", format_count(self.frames.since_open)));
        s.push_str(&format!("- **Último fotograma:** {}\n", self.frames.last.map_or(na.to_string(), format_ms)));
        s.push_str(&format!("- **Fotograma más lento (último intervalo):** {}\n", self.frames.slowest.map_or(na.to_string(), format_ms)));
        s.push_str(&format!(
            "- **Shells / agentes:** {} / {}\n",
            self.stats.shells.map_or(na.to_string(), |n| n.to_string()),
            self.stats.agents.map_or(na.to_string(), |n| n.to_string())
        ));
        for d in &self.dbs {
            s.push_str(&format!("- **{}:** {}\n", d.label, d.bytes.map_or("no existe".to_string(), format_bytes)));
        }
        s.push_str(&format!(
            "- **Gramáticas cargadas:** {}\n",
            if self.stats.grammars.is_empty() { "ninguna".to_string() } else { self.stats.grammars.join(", ") }
        ));
        s
    }
}

/// Fixed-size history of one metric.
#[derive(Clone, Debug, Default)]
pub struct Ring(VecDeque<f32>);

impl Ring {
    pub fn push(&mut self, v: f32) {
        if self.0.len() == HISTORY {
            self.0.pop_front();
        }
        self.0.push_back(v);
    }

    pub fn to_vec(&self) -> Vec<f32> {
        self.0.iter().copied().collect()
    }
}

/// CPU % of the whole machine from two cumulative CPU times.
pub fn cpu_percent(prev: (std::time::Instant, Duration), now: (std::time::Instant, Duration), cores: usize) -> Option<f32> {
    let wall = now.0.checked_duration_since(prev.0)?;
    if wall < Duration::from_millis(100) || now.1 < prev.1 {
        return None;
    }
    let busy = (now.1 - prev.1).as_secs_f64();
    Some((busy / wall.as_secs_f64() / cores.max(1) as f64 * 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(format_bytes(340 * KIB), "340 KB");
        assert_eq!(format_bytes(MIB + MIB / 5), "1,2 MB");
        assert_eq!(format_bytes(58 * MIB), "58 MB");
        assert_eq!(format_bytes(GIB + GIB / 5), "1,2 GB");
        assert_eq!(format_pct(0.01), "0%");
        assert_eq!(format_pct(3.44), "3,4%");
        assert_eq!(format_ms(Duration::from_micros(2_300)), "2,3 ms");
        assert_eq!(format_ms(Duration::from_millis(18)), "18 ms");
        assert_eq!(format_ms(Duration::from_millis(1_200)), "1,2 s");
        assert_eq!(format_count(1_240), "1 240");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_count(1_234_567), "1 234 567");
        assert_eq!(format_uptime(Duration::from_secs(2 * 3_600 + 300)), "2h 05m");
    }

    #[test]
    fn ring_keeps_the_last_sixty() {
        let mut r = Ring::default();
        for i in 0..100 {
            r.push(i as f32);
        }
        let v = r.to_vec();
        assert_eq!(v.len(), HISTORY);
        assert_eq!((v[0], v[HISTORY - 1]), (40.0, 99.0));
    }

    #[test]
    fn cpu_is_a_share_of_the_whole_machine() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        // 0.5 s of CPU in 1 s on 4 cores = 12.5 %.
        let p = cpu_percent((t0, Duration::from_secs(10)), (t1, Duration::from_millis(10_500)), 4).unwrap();
        assert!((p - 12.5).abs() < 0.01);
        assert!(cpu_percent((t0, Duration::ZERO), (t0 + Duration::from_millis(10), Duration::ZERO), 4).is_none());
    }

    #[test]
    fn markdown_reports_everything_and_marks_gaps() {
        let h = Health {
            pid: 42,
            working_set: 58 * MIB,
            private_bytes: 31 * MIB,
            cpu: Some(0.3),
            threads: 17,
            children: vec![ChildProc { pid: 7, name: "pwsh.exe".into(), working_set: 90 * MIB }],
            frames: FrameInfo { since_open: 1_240, last: Some(Duration::from_micros(2_300)), slowest: None },
            stats: AppStats { shells: Some(2), agents: None, grammars: vec!["rust".into(), "toml".into()], tree_tracked: 0 },
            dbs: vec![DbSize { label: "selachi.db".into(), bytes: Some(MIB) }, DbSize { label: "state.db".into(), bytes: None }],
            uptime: Some(Duration::from_secs(125)),
            ..Health::default()
        };
        let md = h.to_markdown("0.0.1");
        for needle in [
            "## Selachi doctor (0.0.1)", "**RAM:** 58 MB (working set) · 31 MB (privada)", "**CPU:** 0,3%", "**Hilos:** 17",
            "**Procesos hijos:** 1 · 90 MB", "pwsh.exe (pid 7)", "1 240", "2,3 ms", "**Shells / agentes:** 2 / —",
            "**selachi.db:** 1,0 MB", "**state.db:** no existe", "rust, toml", "**Tiempo activo:** 2m",
        ] {
            assert!(md.contains(needle), "missing {needle:?} in\n{md}");
        }
    }
}
