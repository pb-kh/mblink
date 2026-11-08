use std::path::Path;

use mblink_proto::mblink::link::v1::StatusRequest;

use crate::connect_or_exit;

pub async fn run(socket: &Path) {
    let mut client = connect_or_exit(socket).await;
    let resp = match client.status(StatusRequest {}).await {
        Ok(r) => r.into_inner(),
        Err(e) => crate::rpc_error_or_not_running(socket, e),
    };

    println!("version:   {}", resp.version);
    println!("uptime_ms: {}", resp.uptime_ms);
    println!("link:      {}", resp.link_state);
    println!("peer:      {}", resp.peer_name);

    if !resp.modules.is_empty() {
        println!();
        println!("{:<20} {}", "module", "status");
        println!("{:-<20} {:-<20}", "", "");
        for m in &resp.modules {
            println!("{:<20} {}", m.name, m.status_line);
        }
    }
}
