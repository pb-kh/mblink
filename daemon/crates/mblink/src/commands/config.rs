use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use mblink_proto::mblink::link::v1::{
    ConfigEntry, GetConfigRequest, SetConfigRequest,
};

use crate::connect_or_exit;

pub async fn get(socket: &Path, module: &str) {
    let mut client = connect_or_exit(socket).await;
    let resp = match client.get_config(GetConfigRequest {
        module: module.to_string(),
    }).await {
        Ok(r) => r.into_inner(),
        Err(e) => crate::rpc_error_or_not_running(socket, e),
    };

    let entries = resp.map.map(|m| m.entries).unwrap_or_default();
    print_entries(&entries);
}

pub async fn set(socket: &Path, module: &str, package: &str, enabled: bool) {
    let mut client = connect_or_exit(socket).await;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let resp = match client
        .set_config(SetConfigRequest {
            module: module.to_string(),
            updates: vec![ConfigEntry {
                package: package.to_string(),
                enabled,
                updated_at: now_ms,
                updated_by: "linux".to_string(),
            }],
        })
        .await
    {
        Ok(r) => r.into_inner(),
        Err(e) => crate::rpc_error_or_not_running(socket, e),
    };

    let entries = resp
        .merged
        .map(|m| m.entries)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.package == package)
        .collect::<Vec<_>>();
    print_entries(&entries);
}

fn print_entries(entries: &[ConfigEntry]) {
    println!(
        "{:<40} {:<8} {:<16} {}",
        "package", "enabled", "updated_at", "updated_by"
    );
    println!(
        "{:-<40} {:-<8} {:-<16} {:-<10}",
        "", "", "", ""
    );
    for e in entries {
        println!(
            "{:<40} {:<8} {:<16} {}",
            e.package, e.enabled, e.updated_at, e.updated_by
        );
    }
}
