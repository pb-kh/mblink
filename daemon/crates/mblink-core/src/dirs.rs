use std::path::PathBuf;

/// Resolved XDG-based directory layout for mblink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MblinkDirs {
    pub config_root: PathBuf,
    pub state_root: PathBuf,
    pub socket: PathBuf,
}

/// Errors that can occur while resolving directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirsError {
    NoRuntimeDir,
}

impl std::fmt::Display for DirsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirsError::NoRuntimeDir => {
                write!(
                    f,
                    "XDG_RUNTIME_DIR is not set and no socket override provided"
                )
            }
        }
    }
}

impl std::error::Error for DirsError {}

impl MblinkDirs {
    pub fn resolve(
        config_dir: Option<PathBuf>,
        state_dir: Option<PathBuf>,
        socket: Option<PathBuf>,
    ) -> Result<Self, DirsError> {
        let config_root = match config_dir {
            Some(p) => p,
            None => match std::env::var_os("XDG_CONFIG_HOME") {
                Some(v) => PathBuf::from(v).join("mblink"),
                None => home_dir().join(".config").join("mblink"),
            },
        };

        let state_root = match state_dir {
            Some(p) => p,
            None => match std::env::var_os("XDG_STATE_HOME") {
                Some(v) => PathBuf::from(v).join("mblinkd"),
                None => home_dir().join(".local").join("state").join("mblinkd"),
            },
        };

        let socket = match socket {
            Some(p) => p,
            None => match std::env::var_os("XDG_RUNTIME_DIR") {
                Some(v) => PathBuf::from(v).join("mblink.sock"),
                None => return Err(DirsError::NoRuntimeDir),
            },
        };

        Ok(MblinkDirs {
            config_root,
            state_root,
            socket,
        })
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_uses_xdg_vars() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", cfg.path());
            std::env::set_var("XDG_STATE_HOME", state.path());
            std::env::set_var("XDG_RUNTIME_DIR", run.path());
        }

        let dirs = MblinkDirs::resolve(None, None, None).unwrap();
        assert_eq!(dirs.config_root, cfg.path().join("mblink"));
        assert_eq!(dirs.state_root, state.path().join("mblinkd"));
        assert_eq!(dirs.socket, run.path().join("mblink.sock"));
    }

    #[test]
    fn resolve_overrides_take_precedence() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let override_cfg = tempfile::tempdir().unwrap();
        let override_state = tempfile::tempdir().unwrap();
        let override_socket = run.path().join("custom.sock");

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", cfg.path());
            std::env::set_var("XDG_STATE_HOME", state.path());
            std::env::set_var("XDG_RUNTIME_DIR", run.path());
        }

        let dirs = MblinkDirs::resolve(
            Some(override_cfg.path().to_path_buf()),
            Some(override_state.path().to_path_buf()),
            Some(override_socket.clone()),
        )
        .unwrap();
        assert_eq!(dirs.config_root, override_cfg.path());
        assert_eq!(dirs.state_root, override_state.path());
        assert_eq!(dirs.socket, override_socket);
    }

    #[test]
    fn resolve_falls_back_to_home_dot_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let run = tempfile::tempdir().unwrap();

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_STATE_HOME");
            std::env::set_var("XDG_RUNTIME_DIR", run.path());
            std::env::set_var("HOME", "/home/testuser");
        }

        let dirs = MblinkDirs::resolve(None, None, None).unwrap();
        assert_eq!(
            dirs.config_root,
            PathBuf::from("/home/testuser/.config/mblink")
        );
        assert_eq!(
            dirs.state_root,
            PathBuf::from("/home/testuser/.local/state/mblinkd")
        );
        assert_eq!(dirs.socket, run.path().join("mblink.sock"));
    }

    #[test]
    fn resolve_missing_runtime_dir_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", cfg.path());
            std::env::set_var("XDG_STATE_HOME", state.path());
            std::env::remove_var("XDG_RUNTIME_DIR");
        }

        let result = MblinkDirs::resolve(None, None, None);
        assert!(matches!(result, Err(DirsError::NoRuntimeDir)));
    }
}
