use std::sync::Arc;
use std::time::Duration;

use mblink_core::config::ConfigStore;
use mblink_core::control::{connect_uds, serve_uds, DaemonState, ModuleDescriptor, VERSION};
use mblink_proto::mblink::link::v1::{
    ConfigEntry, GetConfigRequest, SetConfigRequest, StatusRequest, WatchStatusRequest,
};

#[tokio::test]
async fn control_uds_roundtrip() {
    let config_root = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("mblink.sock");

    let state = Arc::new(DaemonState::new(
        vec![ModuleDescriptor {
            name: "notisync".to_string(),
            status_line: "stub".to_string(),
        }],
        ConfigStore::new(config_root.path()),
    ));

    let server_state = Arc::clone(&state);
    let server_path = socket_path.clone();
    let mut server_handle = tokio::spawn(async move { serve_uds(server_state, server_path).await });

    // Wait for the socket to become available.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !socket_path.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(tokio::time::Instant::now() < deadline, "server did not start");
    }

    let mut client = connect_uds(socket_path.clone()).await.unwrap();

    // Status
    let status = client.status(StatusRequest {}).await.unwrap().into_inner();
    assert_eq!(status.version, VERSION);
    assert_eq!(status.link_state, "disconnected");
    assert_eq!(status.peer_name, "");
    let names: Vec<_> = status.modules.iter().map(|m| m.name.clone()).collect();
    assert!(names.contains(&"notisync".to_string()));

    // GetConfig for an unknown but valid module returns an empty map.
    let cfg = client
        .get_config(GetConfigRequest {
            module: "notisync".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(cfg.map.as_ref().unwrap().entries.is_empty());

    // SetConfig writes through to disk.
    let set = client
        .set_config(SetConfigRequest {
            module: "notisync".to_string(),
            updates: vec![ConfigEntry {
                package: "com.google.android.gm".to_string(),
                enabled: true,
                updated_at: 100,
                updated_by: "linux".to_string(),
            }],
        })
        .await
        .unwrap()
        .into_inner();
    let entries = &set.merged.as_ref().unwrap().entries;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.package, "com.google.android.gm");
    assert!(entry.enabled);

    let disk_path = config_root.path().join("notisync").join("config.toml");
    assert!(disk_path.exists());
    let disk = std::fs::read_to_string(&disk_path).unwrap();
    assert!(disk.contains("enabled = true"));

    // SetConfig with an older timestamp is ignored.
    let set2 = client
        .set_config(SetConfigRequest {
            module: "notisync".to_string(),
            updates: vec![ConfigEntry {
                package: "com.google.android.gm".to_string(),
                enabled: false,
                updated_at: 50,
                updated_by: "linux".to_string(),
            }],
        })
        .await
        .unwrap()
        .into_inner();
    let entries2 = &set2.merged.as_ref().unwrap().entries;
    assert_eq!(entries2.len(), 1);
    assert!(entries2[0].enabled);

    // Invalid module name returns InvalidArgument.
    let err = client
        .get_config(GetConfigRequest {
            module: "../etc".to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // Stale plain file at the socket path is replaced.
    server_handle.abort();
    let _ = server_handle.await;
    std::fs::remove_file(&socket_path).ok();
    std::fs::write(&socket_path, "not a socket").unwrap();

    let stale_state = Arc::clone(&state);
    let stale_path = socket_path.clone();
    server_handle = tokio::spawn(async move { serve_uds(stale_state, stale_path).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !socket_path.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "stale-socket server did not start"
        );
    }
    assert!(
        !server_handle.is_finished(),
        "stale-socket server exited unexpectedly"
    );

    // Live conflict: a second server on the same path fails fast.
    let live_socket = socket_dir.path().join("live.sock");
    let live_state = Arc::new(DaemonState::new(
        vec![ModuleDescriptor {
            name: "notisync".to_string(),
            status_line: "stub".to_string(),
        }],
        ConfigStore::new(config_root.path()),
    ));
    let live_server_path = live_socket.clone();
    let live_state_for_conflict = Arc::clone(&live_state);
    let live_handle =
        tokio::spawn(async move { serve_uds(live_state, live_server_path).await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !live_socket.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(tokio::time::Instant::now() < deadline, "live server did not start");
    }

    let conflict = serve_uds(live_state_for_conflict, live_socket.clone()).await;
    assert!(conflict.is_err());
    let msg = format!("{}", conflict.unwrap_err());
    assert!(
        msg.contains("already running"),
        "expected 'already running' in error, got: {}",
        msg
    );

    live_handle.abort();
    let _ = live_handle.await;

    // Idle WatchStatus stream does not block concurrent Status calls.
    let mut watch_client = connect_uds(socket_path.clone()).await.unwrap();
    let _stream = watch_client
        .watch_status(WatchStatusRequest {})
        .await
        .unwrap()
        .into_inner();

    let mut status_client = connect_uds(socket_path.clone()).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        status_client.status(StatusRequest {}),
    )
    .await;
    assert!(result.is_ok(), "Status call blocked by idle WatchStatus");

    server_handle.abort();
    let _ = server_handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_set_config_serializes_lww() {
    use std::collections::HashMap;

    let config_root = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("concurrent.sock");

    let state = Arc::new(DaemonState::new(
        vec![ModuleDescriptor {
            name: "notisync".to_string(),
            status_line: "stub".to_string(),
        }],
        ConfigStore::new(config_root.path()),
    ));

    let server_state = Arc::clone(&state);
    let server_path = socket_path.clone();
    let server_handle = tokio::spawn(async move { serve_uds(server_state, server_path).await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !socket_path.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(tokio::time::Instant::now() < deadline, "server did not start");
    }

    let mut client = connect_uds(socket_path.clone()).await.unwrap();

    // Race many concurrent SetConfig calls against the same module.
    let mut handles = Vec::new();
    for i in 0..100 {
        let mut c = connect_uds(socket_path.clone()).await.unwrap();
        handles.push(tokio::spawn(async move {
            c.set_config(SetConfigRequest {
                module: "notisync".to_string(),
                updates: vec![ConfigEntry {
                    package: format!("pkg{}", i),
                    enabled: i % 2 == 0,
                    updated_at: 100 + i as u64,
                    updated_by: "linux".to_string(),
                }],
            })
            .await
            .unwrap()
            .into_inner()
        }));
    }

    // Race updates to a shared package with out-of-order timestamps.
    for (updated_at, enabled) in [(30, false), (300, true), (20, false), (200, false)] {
        let mut c = connect_uds(socket_path.clone()).await.unwrap();
        handles.push(tokio::spawn(async move {
            c.set_config(SetConfigRequest {
                module: "notisync".to_string(),
                updates: vec![ConfigEntry {
                    package: "racer".to_string(),
                    enabled,
                    updated_at,
                    updated_by: "linux".to_string(),
                }],
            })
            .await
            .unwrap()
            .into_inner()
        }));
    }

    for h in handles {
        let _ = h.await.unwrap();
    }

    let final_cfg = client
        .get_config(GetConfigRequest {
            module: "notisync".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    let entries: HashMap<_, _> = final_cfg
        .map
        .unwrap()
        .entries
        .into_iter()
        .map(|e| (e.package.clone(), e))
        .collect();

    // No lost updates: every distinct package is present.
    for i in 0..100 {
        assert!(entries.contains_key(&format!("pkg{}", i)), "missing pkg{}", i);
    }

    // LWW is respected for the racing package.
    let racer = entries.get("racer").expect("racer missing");
    assert!(racer.enabled, "racer should reflect newest update (enabled=true)");
    assert_eq!(racer.updated_at, 300);

    // On-disk state matches the API state.
    let disk = std::fs::read_to_string(config_root.path().join("notisync").join("config.toml"))
        .unwrap();
    assert!(disk.contains("[apps.racer]"));
    assert!(disk.contains("enabled = true"));
    assert!(disk.contains("updated_at = 300"));

    server_handle.abort();
    let _ = server_handle.await;
}
