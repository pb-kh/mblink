use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use mblink_core::control::{VERSION, connect_uds};
use mblink_proto::mblink::link::v1::{GetConfigRequest, StatusRequest};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mblinkd")
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn temp_dirs() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("cfg");
    let state = tmp.path().join("state");
    let sock = tmp.path().join("mblink.sock");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    (tmp, cfg, state, sock)
}

fn spawn_daemon(cfg: &Path, state: &Path, sock: &Path) -> ChildGuard {
    let child = Command::new(bin())
        .args([
            "--config-dir",
            &cfg.to_string_lossy(),
            "--state-dir",
            &state.to_string_lossy(),
            "--socket",
            &sock.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mblinkd");
    ChildGuard(child)
}

async fn wait_for_socket(sock: &Path, timeout: Duration) {
    let start = Instant::now();
    while !sock.exists() {
        if start.elapsed() > timeout {
            panic!("daemon socket did not appear within {:?}", timeout);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn wait_for_exit(child: &mut ChildGuard, timeout: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return Some(status);
        }
        if start.elapsed() > timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn file_mtime_ms(path: &Path) -> u64 {
    let meta = std::fs::metadata(path).unwrap();
    let dur = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    dur.as_millis() as u64
}

#[tokio::test]
async fn version_prints_version_and_modules() {
    let out = Command::new(bin()).arg("--version").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(VERSION), "version missing: {}", stdout);
    assert!(
        stdout.contains("modules:"),
        "modules header missing: {}",
        stdout
    );
}

#[tokio::test]
async fn check_valid_tree_passes() {
    let (_tmp, cfg, state, _sock) = temp_dirs();
    let out = Command::new(bin())
        .args([
            "--check",
            "--config-dir",
            &cfg.to_string_lossy(),
            "--state-dir",
            &state.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("check"), "check report missing: {}", stdout);
}

#[tokio::test]
async fn check_corrupt_tree_fails() {
    let (_tmp, cfg, state, _sock) = temp_dirs();
    let notisync = cfg.join("notisync");
    std::fs::create_dir_all(&notisync).unwrap();
    std::fs::write(notisync.join("config.toml"), "this is not valid toml [[[").unwrap();

    let out = Command::new(bin())
        .args([
            "--check",
            "--config-dir",
            &cfg.to_string_lossy(),
            "--state-dir",
            &state.to_string_lossy(),
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("notisync/config.toml"),
        "stderr should name corrupt file: {}",
        stderr
    );
}

#[tokio::test]
async fn daemon_starts_and_status_succeeds() {
    let (_tmp, cfg, state, sock) = temp_dirs();
    let mut child = spawn_daemon(&cfg, &state, &sock);
    wait_for_socket(&sock, Duration::from_secs(5)).await;

    let mut client = connect_uds(sock).await.unwrap();
    let resp = client.status(StatusRequest {}).await.unwrap();
    let inner = resp.into_inner();
    assert_eq!(inner.version, VERSION);
    // Ruling R1: the registry lists every known module, with compile state
    // reflected in its status line.
    let notisync = inner
        .modules
        .iter()
        .find(|m| m.name == "notisync")
        .expect("registry must list notisync");
    assert_eq!(
        notisync.status_line,
        if cfg!(feature = "module-notisync") {
            "compiled in"
        } else {
            "not compiled"
        }
    );

    // ChildGuard will kill the process on drop if still alive.
    let _ = child.0.kill();
    let _ = child.0.wait();
}

#[tokio::test]
async fn corrupt_hand_edit_keeps_last_good_config() {
    let (_tmp, cfg, _state, sock) = temp_dirs();
    let notisync = cfg.join("notisync");
    std::fs::create_dir_all(&notisync).unwrap();
    let path = notisync.join("config.toml");
    let initial = r#"[apps."com.example.app"]
enabled = false
updated_at = 1
updated_by = "phone"
"#;
    std::fs::write(&path, initial).unwrap();

    let mut child = spawn_daemon(&cfg, &_state, &sock);
    wait_for_socket(&sock, Duration::from_secs(5)).await;

    let mut client = connect_uds(sock).await.unwrap();
    let pre = client
        .get_config(GetConfigRequest {
            module: "notisync".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    let pre_entry = pre
        .map
        .expect("missing config map")
        .entries
        .into_iter()
        .find(|e| e.package == "com.example.app")
        .expect("pre-edit entry");
    assert!(!pre_entry.enabled);

    // Corrupt the file while the daemon is running.
    std::fs::write(&path, "garbage [[[ ").unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let post = client
        .get_config(GetConfigRequest {
            module: "notisync".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    let post_entry = post
        .map
        .expect("missing config map")
        .entries
        .into_iter()
        .find(|e| e.package == "com.example.app")
        .expect("post-edit entry preserved");
    assert!(
        !post_entry.enabled,
        "corrupt hand-edit should keep last-good map"
    );

    let _ = child.0.kill();
    let _ = child.0.wait();
}

#[tokio::test]
async fn valid_hand_edit_merges_with_mtime_semantics() {
    let (_tmp, cfg, _state, sock) = temp_dirs();
    let notisync = cfg.join("notisync");
    std::fs::create_dir_all(&notisync).unwrap();
    let path = notisync.join("config.toml");
    let initial = r#"[apps."com.example.app"]
enabled = false
updated_at = 1
updated_by = "phone"
"#;
    std::fs::write(&path, initial).unwrap();

    let mut child = spawn_daemon(&cfg, &_state, &sock);
    wait_for_socket(&sock, Duration::from_secs(5)).await;

    // The daemon seeds its in-memory copy at startup; nothing to wait for
    // here, but give startup a moment before the edit lands regardless.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let edited = r#"[apps."com.example.app"]
enabled = true
updated_at = 1
updated_by = "phone"
"#;
    std::fs::write(&path, edited).unwrap();
    let mtime_ms = file_mtime_ms(&path);

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found = None;
    while Instant::now() < deadline {
        let mut client = connect_uds(sock.clone()).await.unwrap();
        let resp = client
            .get_config(GetConfigRequest {
                module: "notisync".to_string(),
            })
            .await
            .unwrap()
            .into_inner();
        if let Some(map) = resp.map
            && let Some(e) = map
                .entries
                .into_iter()
                .find(|e| e.package == "com.example.app" && e.enabled)
        {
            found = Some(e);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let entry = found.expect("enabled=true entry should appear within 2s");
    assert_eq!(
        entry.updated_at, mtime_ms,
        "updated_at should match file mtime"
    );
    assert_eq!(entry.updated_by, "linux", "updated_by should be linux");

    let _ = child.0.kill();
    let _ = child.0.wait();
}

#[tokio::test]
async fn sigterm_exits_zero_within_two_seconds() {
    let (_tmp, cfg, state, sock) = temp_dirs();
    let mut child = spawn_daemon(&cfg, &state, &sock);
    wait_for_socket(&sock, Duration::from_secs(5)).await;

    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGTERM);
    }

    let status = wait_for_exit(&mut child, Duration::from_secs(2))
        .expect("process should exit within 2s of SIGTERM");
    assert!(
        status.success(),
        "expected exit 0 after SIGTERM, got {:?}",
        status
    );
}

#[tokio::test]
async fn second_daemon_on_live_socket_fails_with_already_running() {
    let (_tmp, cfg, state, sock) = temp_dirs();
    let mut first = spawn_daemon(&cfg, &state, &sock);
    wait_for_socket(&sock, Duration::from_secs(5)).await;

    let out = Command::new(bin())
        .args([
            "--config-dir",
            &cfg.to_string_lossy(),
            "--state-dir",
            &state.to_string_lossy(),
            "--socket",
            &sock.to_string_lossy(),
        ])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(1),
        "second daemon should exit 1, got {:?} (stderr: {})",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already running"),
        "stderr should say already running: {}",
        stderr
    );
    assert!(
        stderr.contains(sock.to_string_lossy().as_ref()),
        "stderr should name the socket path: {}",
        stderr
    );

    let _ = first.0.kill();
    let _ = first.0.wait();
}
