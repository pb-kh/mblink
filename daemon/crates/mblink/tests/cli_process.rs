use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use mblink_core::config::ConfigStore;
use mblink_core::control::{serve_uds, DaemonState, ModuleDescriptor};

fn mblink_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mblink"))
}

async fn start_test_daemon(socket: std::path::PathBuf) -> tokio::task::JoinHandle<()> {
    let state = Arc::new(DaemonState::new(
        vec![ModuleDescriptor {
            name: "notisync".to_string(),
            status_line: "compiled in".to_string(),
        }],
        ConfigStore::new(socket.parent().unwrap().join("config")),
    ));
    let handle = tokio::spawn(async move {
        let _ = serve_uds(state, socket).await;
    });
    // Give the server a moment to bind and start accepting.
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_status_modules_config_and_unreachable_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mblink.sock");

    let server = start_test_daemon(socket.clone()).await;

    // status → stdout contains `disconnected` and `notisync`
    let out = mblink_bin()
        .args(["--socket", socket.to_str().unwrap(), "status"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("disconnected"),
        "status stdout missing 'disconnected': {stdout}"
    );
    assert!(
        stdout.contains("notisync"),
        "status stdout missing 'notisync': {stdout}"
    );

    // modules → shows notisync
    let out = mblink_bin()
        .args(["--socket", socket.to_str().unwrap(), "modules"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "modules failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("notisync"),
        "modules stdout missing 'notisync': {stdout}"
    );

    // config set notisync com.google.android.gm on → exit 0
    let out = mblink_bin()
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "config",
            "set",
            "notisync",
            "com.google.android.gm",
            "on",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "config set failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // config get notisync → shows the package enabled
    let out = mblink_bin()
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "config",
            "get",
            "notisync",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "config get failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("com.google.android.gm"),
        "config get missing package: {stdout}"
    );
    assert!(
        stdout.contains("true") || stdout.contains("enabled") || stdout.contains("on"),
        "config get missing enabled state: {stdout}"
    );

    // daemon killed → status exits 1 with `not running`
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_file(&socket);

    let out = mblink_bin()
        .args(["--socket", socket.to_str().unwrap(), "status"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "status should fail when daemon not running"
    );
    assert_eq!(out.status.code(), Some(1), "expected exit code 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("mblinkd not running ({})", socket.display())),
        "stderr missing exact error, got: {stderr}"
    );
}
