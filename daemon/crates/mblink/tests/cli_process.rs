use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mblink_core::config::ConfigStore;
use mblink_core::control::{DaemonState, ModuleDescriptor, serve_uds};
use tokio::net::UnixListener;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_exits_with_clear_error_when_daemon_accepts_but_never_answers() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mblink.sock");

    let listener = UnixListener::bind(&socket).unwrap();
    let acceptor = tokio::spawn(async move {
        // Accept and hold connections without speaking HTTP/2.
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let socket_arg = socket.to_str().unwrap().to_string();
    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(10), async move {
        tokio::task::spawn_blocking(move || {
            mblink_bin()
                .args(["--socket", &socket_arg, "status"])
                .stdin(Stdio::null())
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    })
    .await;

    acceptor.abort();
    let _ = acceptor.await;

    let out = result.expect("cli should exit within bounded time on a hung socket");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(9),
        "cli took too long ({elapsed:?}), expected bounded timeout"
    );
    assert!(!out.status.success(), "cli should fail on a hung socket");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(socket.to_str().unwrap()),
        "stderr should name the socket, got: {stderr}"
    );
    assert!(
        stderr.contains("timed out") || stderr.contains("not running"),
        "stderr should give a clear error, got: {stderr}"
    );
}
