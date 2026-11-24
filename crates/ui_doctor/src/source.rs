//! Measuring Selachi itself. [`DoctorSource`] is the seam for tests; [`SystemSource`] reads the own
//! process and its descendants through `proc_svc` (one ToolHelp snapshot, no command lines).

use std::path::PathBuf;
use std::time::Instant;

use proc_svc::{SnapshotOptions, collect_tree, logical_cores, snapshot_with};

use crate::health::{ChildProc, DbSize, RawSample};

/// Blocking; runs on the background executor.
pub trait DoctorSource: Send + Sync + 'static {
    fn sample(&self, dbs: &[(String, PathBuf)]) -> RawSample;
}

/// Size of a DB file plus its WAL (`-wal`), which is where recent writes live.
fn db_size(path: &PathBuf) -> Option<u64> {
    let main = std::fs::metadata(path).ok()?.len();
    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    Some(main + std::fs::metadata(PathBuf::from(wal)).map_or(0, |m| m.len()))
}

pub struct SystemSource;

impl DoctorSource for SystemSource {
    fn sample(&self, dbs: &[(String, PathBuf)]) -> RawSample {
        let pid = std::process::id();
        let procs = snapshot_with(SnapshotOptions { cmdline: false });
        let me = procs.iter().find(|p| p.pid == pid);
        let children = collect_tree(pid, &procs)
            .into_iter()
            .filter(|p| *p != pid)
            .filter_map(|p| procs.iter().find(|i| i.pid == p))
            .map(|i| ChildProc {
                pid: i.pid,
                name: i.name.clone(),
                working_set: i.working_set,
            })
            .collect();
        RawSample {
            at: Instant::now(),
            working_set: me.map_or(0, |p| p.working_set),
            private_bytes: me.map_or(0, |p| p.private_bytes),
            cpu_time: me.map_or_else(Default::default, |p| p.cpu_time),
            threads: me.map_or(0, |p| p.threads),
            children,
            dbs: dbs
                .iter()
                .map(|(label, path)| DbSize {
                    label: label.clone(),
                    bytes: db_size(path),
                })
                .collect(),
            started: me.and_then(|p| p.start_time),
            cores: logical_cores(),
            pid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_this_test_process() {
        let child = std::process::Command::new("cmd")
            .args(["/c", "ping", "-n", "5", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn();
        let dir = std::env::temp_dir().join("ui_doctor_test.db");
        std::fs::write(&dir, vec![0u8; 2048]).unwrap();
        let s = SystemSource.sample(&[
            ("t.db".into(), dir.clone()),
            ("missing.db".into(), PathBuf::from(r"C:\nope\x.db")),
        ]);
        assert_eq!(s.pid, std::process::id());
        assert!(s.working_set > 1 << 20 && s.private_bytes > 0 && s.threads >= 1);
        assert_eq!(s.dbs[0].bytes, Some(2048));
        assert_eq!(s.dbs[1].bytes, None);
        assert!(s.started.is_some());
        if let Ok(mut c) = child {
            assert!(
                s.children.iter().any(|ch| ch.pid == c.id()),
                "own child must be listed"
            );
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_file(dir);
    }
}
