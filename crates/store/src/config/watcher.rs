//! Hot reload of `config.toml`: watch the parent directory (editors save by
//! rename), debounce bursts of events, re-parse, and emit the new config. A file
//! that fails to parse never replaces the last good config; an `Error` event is
//! emitted instead (once per distinct message).

use super::Config;
use crate::error::{Result, StoreError};
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// A valid, different config is now current.
    Reloaded {
        config: Arc<Config>,
        warnings: Vec<String>,
    },
    /// The file on disk is invalid; the previous config stays current.
    Error(String),
}

/// Keeps watching until dropped.
pub struct ConfigWatcher {
    current: Arc<RwLock<Arc<Config>>>,
    watcher: Option<RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl ConfigWatcher {
    /// Start watching `path`. `initial` is what is currently in use (from
    /// [`Config::load_or_default`]); it is what `current()` returns until a reload.
    pub fn start(
        path: &Path,
        initial: Arc<Config>,
        debounce: Duration,
    ) -> Result<(ConfigWatcher, UnboundedReceiver<ConfigEvent>)> {
        let dir: PathBuf = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let file_name = path
            .file_name()
            .map(|n| n.to_os_string())
            .ok_or_else(|| StoreError::Invalid("config path has no file name".into()))?;
        std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;

        let (raw_tx, raw_rx) = channel::<()>();
        let name_filter = file_name;
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if ev
                        .paths
                        .iter()
                        .any(|p| p.file_name() == Some(name_filter.as_os_str()))
                    {
                        let _ = raw_tx.send(());
                    }
                }
            })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        let current = Arc::new(RwLock::new(initial));
        let (tx, rx) = unbounded::<ConfigEvent>();
        let thread_current = Arc::clone(&current);
        let path = path.to_path_buf();

        let thread = std::thread::Builder::new()
            .name("selachi-config-watch".into())
            .spawn(move || {
                let mut last_error: Option<String> = None;
                // Blocks until an event arrives; ends when the watcher is dropped.
                while raw_rx.recv().is_ok() {
                    // Debounce: wait for a quiet period.
                    loop {
                        match raw_rx.recv_timeout(debounce) {
                            Ok(()) => continue,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let event = match Config::load_from(&path) {
                        Ok(parsed) => {
                            last_error = None;
                            let same = thread_current
                                .read()
                                .map(|c| **c == parsed.config)
                                .unwrap_or(false);
                            if same {
                                None
                            } else {
                                let cfg = Arc::new(parsed.config);
                                if let Ok(mut guard) = thread_current.write() {
                                    *guard = Arc::clone(&cfg);
                                }
                                Some(ConfigEvent::Reloaded {
                                    config: cfg,
                                    warnings: parsed.warnings,
                                })
                            }
                        }
                        // Mid-rename or deleted: keep the current config silently.
                        Err(StoreError::Io { source, .. })
                            if source.kind() == std::io::ErrorKind::NotFound =>
                        {
                            None
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            if last_error.as_deref() == Some(msg.as_str()) {
                                None
                            } else {
                                last_error = Some(msg.clone());
                                log::warn!("config.toml invalid, keeping last good config: {msg}");
                                Some(ConfigEvent::Error(msg))
                            }
                        }
                    };
                    if let Some(ev) = event {
                        if tx.unbounded_send(ev).is_err() {
                            return; // receiver gone
                        }
                    }
                }
            })
            .map_err(|e| StoreError::io(&dir, e))?;

        Ok((
            ConfigWatcher {
                current,
                watcher: Some(watcher),
                thread: Some(thread),
            },
            rx,
        ))
    }

    /// The last good config.
    pub fn current(&self) -> Arc<Config> {
        match self.current.read() {
            Ok(g) => Arc::clone(&g),
            Err(p) => Arc::clone(&p.into_inner()),
        }
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // Dropping the watcher drops the event callback and its sender, which
        // wakes the thread with a disconnect.
        self.watcher.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeMode;
    use futures::executor::block_on;
    use futures::StreamExt;

    fn next_event(rx: &mut UnboundedReceiver<ConfigEvent>, secs: u64) -> Option<ConfigEvent> {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        loop {
            if let Ok(ev) = rx.try_recv() {
                return Some(ev);
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn invalid_toml_keeps_last_good_then_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let load = Config::load_or_default_at(&path);
        let (watcher, mut rx) =
            ConfigWatcher::start(&path, Arc::new(load.config), Duration::from_millis(60)).unwrap();

        // Valid change -> Reloaded.
        std::fs::write(&path, "[appearance]\nmode = \"light\"\n").unwrap();
        match next_event(&mut rx, 10) {
            Some(ConfigEvent::Reloaded { config, .. }) => {
                assert_eq!(config.appearance.mode, ThemeMode::Light)
            }
            other => panic!("expected Reloaded, got {other:?}"),
        }
        assert_eq!(watcher.current().appearance.mode, ThemeMode::Light);

        // Broken TOML -> Error, config unchanged.
        std::fs::write(&path, "[appearance\nmode = ").unwrap();
        match next_event(&mut rx, 10) {
            Some(ConfigEvent::Error(msg)) => assert!(!msg.is_empty()),
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(watcher.current().appearance.mode, ThemeMode::Light);

        // Fixed again -> Reloaded with the new value.
        std::fs::write(&path, "[appearance]\nmode = \"oled\"\n").unwrap();
        match next_event(&mut rx, 10) {
            Some(ConfigEvent::Reloaded { config, .. }) => {
                assert_eq!(config.appearance.mode, ThemeMode::Oled)
            }
            other => panic!("expected Reloaded, got {other:?}"),
        }

        drop(watcher);
        // Stream ends once the watcher thread is gone.
        assert!(block_on(rx.next()).is_none());
    }
}
