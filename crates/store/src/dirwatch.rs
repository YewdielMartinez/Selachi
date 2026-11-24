//! A debounced "something changed in this folder" signal, for folders other processes write to
//! (the delegation run registry `agent-runs`). The OS delivers the events (no polling: an idle
//! watcher costs nothing); bursts are folded into one wake-up.

use crate::error::{Result, StoreError};
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Keeps watching until dropped.
pub struct DirWatcher {
    watcher: Option<RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl DirWatcher {
    /// Watches `dir` (created when missing, not recursive). Every burst of changes ends in one `()`
    /// on the receiver, `debounce` after the last event.
    pub fn start(dir: &Path, debounce: Duration) -> Result<(DirWatcher, UnboundedReceiver<()>)> {
        std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        let (raw_tx, raw_rx) = channel::<()>();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if res.is_ok() {
                    let _ = raw_tx.send(());
                }
            })?;
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
        let (tx, rx) = unbounded::<()>();
        let thread = std::thread::Builder::new()
            .name("selachi-dir-watch".into())
            .spawn(move || {
                // Blocks until an event arrives; ends when the watcher is dropped.
                while raw_rx.recv().is_ok() {
                    loop {
                        match raw_rx.recv_timeout(debounce) {
                            Ok(()) => continue,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    if tx.unbounded_send(()).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| StoreError::io(dir, e))?;
        Ok((
            DirWatcher {
                watcher: Some(watcher),
                thread: Some(thread),
            },
            rx,
        ))
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        // Dropping the watcher closes the raw channel, which ends the thread.
        self.watcher.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_wake(rx: &mut UnboundedReceiver<()>, max: Duration) -> bool {
        let end = std::time::Instant::now() + max;
        while std::time::Instant::now() < end {
            if rx.try_recv().is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_burst_of_writes_wakes_once() {
        let dir = tempfile::tempdir().unwrap();
        let (_w, mut rx) = DirWatcher::start(dir.path(), Duration::from_millis(80)).unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("{i}.json")), "{}").unwrap();
        }
        assert!(wait_for_wake(&mut rx, Duration::from_secs(5)));
        // the burst was folded: no second wake-up while the folder is quiet
        assert!(!wait_for_wake(&mut rx, Duration::from_millis(400)));
        std::fs::write(dir.path().join("later.json"), "{}").unwrap();
        assert!(wait_for_wake(&mut rx, Duration::from_secs(5)));
    }
}
