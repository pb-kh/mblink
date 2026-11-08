use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, RecursiveMode, Watcher};

/// Watches `<config_root>/**/config.toml` and emits debounced module names.
pub struct ConfigWatcher {
    _handle: thread::JoinHandle<()>,
}

impl ConfigWatcher {
    pub fn spawn<F: Fn(String) + Send + 'static>(
        config_root: PathBuf,
        on_change: F,
    ) -> Self {
        let (tx, rx) = mpsc::channel();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        })
        .expect("create file watcher");

        watcher
            .watch(&config_root, RecursiveMode::Recursive)
            .expect("watch config root");

        let handle = thread::spawn(move || {
            let _watcher = watcher; // keep alive for the lifetime of the thread
            let debounce = Duration::from_millis(100);
            let mut pending: HashSet<String> = HashSet::new();
            let mut deadline: Option<Instant> = None;

            loop {
                let timeout = deadline
                    .map(|d| d.saturating_duration_since(Instant::now()));

                match timeout {
                    Some(t) => match rx.recv_timeout(t) {
                        Ok(event) => {
                            for path in &event.paths {
                                if let Some(module) = module_from_path(path) {
                                    pending.insert(module);
                                }
                            }
                            deadline = Some(Instant::now() + debounce);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            for module in pending.drain() {
                                on_change(module);
                            }
                            deadline = None;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                    None => match rx.recv() {
                        Ok(event) => {
                            for path in &event.paths {
                                if let Some(module) = module_from_path(path) {
                                    pending.insert(module);
                                }
                            }
                            deadline = Some(Instant::now() + debounce);
                        }
                        Err(_) => break,
                    },
                }
            }
        });

        ConfigWatcher { _handle: handle }
    }
}

fn module_from_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if file_name != "config.toml" {
        return None;
    }
    let parent = path.parent()?;
    let module = parent.file_name()?.to_str()?;
    Some(module.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn watcher_emits_module_on_config_change() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("notisync");
        std::fs::create_dir_all(&module_dir).unwrap();
        let path = module_dir.join("config.toml");
        std::fs::write(&path, "").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::spawn(dir.path().to_path_buf(), move |module| {
            let _ = tx.send(module);
        });

        // Give the watcher a moment to register with the OS.
        thread::sleep(Duration::from_millis(50));

        std::fs::write(&path, "x = 1").unwrap();

        let module = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(module, "notisync");
    }
}
