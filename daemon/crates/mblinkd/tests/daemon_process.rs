use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use mblink_core::config::ConfigMap;
use mblink_core::control::{VERSION, connect_uds};
use mblink_proto::mblink::link::v1::{
    ConfigEntry, GetConfigRequest, SetConfigRequest, StatusRequest,
};

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

/// Review finding (Important #1): `SetConfig` and the watcher reload handler
/// are both read-modify-write writers of the same config file, and only a
/// shared critical section prevents a save based on a pre-write snapshot
/// from silently dropping the other writer's entries.
///
/// Isolates exactly that race: every hand edit is a quiescent, package-
/// preserving read-modify-write of the file (so no writer ever legitimately
/// removes an entry), and each SetConfig burst is fired right at the
/// watcher's 100 ms debounce deadline so the RPC's save lands inside the
/// reload handler's critical section.  Without the shared lock the
/// handler's snapshot-based save wipes the freshly written RPC entries;
/// with it, both writers' entries must all survive, in the API and on disk.
#[tokio::test]
async fn concurrent_set_config_and_hand_edits_lose_no_entries() {
    const HAND_EDITS: usize = 12;
    const RPC_PER_EDIT: usize = 8;
    const FILLERS: usize = 150;

    let (_tmp, cfg, state, sock) = temp_dirs();
    let notisync = cfg.join("notisync");
    std::fs::create_dir_all(&notisync).unwrap();
    let path = notisync.join("config.toml");
    // Seed + filler entries: the fillers grow the config file so each
    // writer's parse/serialize/fsync section is long enough for the
    // concurrent writer to land inside it.
    let mut seed =
        String::from("[apps.\"seed\"]\nenabled = true\nupdated_at = 1\nupdated_by = \"phone\"\n");
    for j in 0..FILLERS {
        seed.push_str(&format!(
            "[apps.\"filler.{j}\"]\nenabled = true\nupdated_at = 1\nupdated_by = \"phone\"\n"
        ));
    }
    std::fs::write(&path, seed).unwrap();

    let mut child = spawn_daemon(&cfg, &state, &sock);
    wait_for_socket(&sock, Duration::from_secs(5)).await;
    let mut client = connect_uds(sock).await.unwrap();

    for i in 0..HAND_EDITS {
        // Hand edit while no other writer is in flight: read the current
        // file, append one entry, write back atomically (tmp + rename, the
        // same shape as the daemon's own saves).  Every package present on
        // disk stays on disk.
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str(&format!(
            "[apps.\"hand.{i}\"]\nenabled = true\nupdated_at = 1\nupdated_by = \"phone\"\n"
        ));
        let tmp = path.with_file_name("hand-edit.tmp");
        std::fs::write(&tmp, &content).unwrap();
        std::fs::rename(&tmp, &path).unwrap();

        // The reload handler fires at the debounce deadline (~100 ms after
        // the edit).  Fire a SetConfig burst right at that deadline, all
        // concurrent, so their saves land inside the handler's
        // read-modify-write section.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut handles = Vec::new();
        for s in 0..RPC_PER_EDIT {
            let mut c = client.clone();
            let package = format!("rpc.{i}.{s}");
            handles.push(tokio::spawn(async move {
                let resp = c
                    .set_config(SetConfigRequest {
                        module: "notisync".to_string(),
                        updates: vec![ConfigEntry {
                            package: package.clone(),
                            enabled: true,
                            updated_at: 1_000_000 + (i * RPC_PER_EDIT + s) as u64,
                            updated_by: "phone".to_string(),
                        }],
                    })
                    .await
                    .expect("SetConfig rpc")
                    .into_inner();
                let entries = resp.merged.expect("merged map in response").entries;
                assert!(
                    entries.iter().any(|e| e.package == package),
                    "SetConfig response must reflect its own write"
                );
            }));
        }
        for h in handles {
            h.await.expect("rpc task");
        }

        // Let the handler (and the second pass triggered by the burst's
        // own saves) settle before the next quiescent hand edit.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Final settle for any remaining watcher passes.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let api = client
        .get_config(GetConfigRequest {
            module: "notisync".to_string(),
        })
        .await
        .unwrap()
        .into_inner()
        .map
        .expect("config map")
        .entries;

    let expected = 1 + FILLERS + HAND_EDITS + HAND_EDITS * RPC_PER_EDIT;
    assert_eq!(
        api.len(),
        expected,
        "expected {expected} entries (seed + filler + hand + rpc), got {}",
        api.len()
    );
    for i in 0..HAND_EDITS {
        assert!(
            api.iter().any(|e| e.package == format!("hand.{i}")),
            "hand.{i} lost: watcher reload dropped a concurrent write"
        );
    }
    for i in 0..HAND_EDITS {
        for s in 0..RPC_PER_EDIT {
            let package = format!("rpc.{i}.{s}");
            let entry = api
                .iter()
                .find(|e| e.package == package)
                .unwrap_or_else(|| panic!("{package} lost: unserialized writers dropped an entry"));
            assert_eq!(
                entry.updated_by, "phone",
                "{package} provenance must survive watcher reloads"
            );
        }
    }

    // The on-disk file must agree with the API state.
    let disk =
        ConfigMap::from_toml(&std::fs::read_to_string(&path).unwrap()).expect("disk file parses");
    let api_map: BTreeMap<_, _> = api.into_iter().map(|e| (e.package.clone(), e)).collect();
    assert_eq!(
        disk.entries.len(),
        api_map.len(),
        "disk and API entry counts differ"
    );
    for (package, entry) in &disk.entries {
        let api_entry = &api_map[package];
        assert_eq!(
            entry.enabled, api_entry.enabled,
            "{package}: enabled mismatch"
        );
        assert_eq!(
            entry.updated_at, api_entry.updated_at,
            "{package}: updated_at mismatch"
        );
        assert_eq!(
            entry.updated_by, api_entry.updated_by,
            "{package}: updated_by mismatch"
        );
    }

    let _ = child.0.kill();
    let _ = child.0.wait();
}
