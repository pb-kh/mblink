use std::path::Path;

use mblink_proto::mblink::link::v1::ModuleListRequest;

use crate::connect_or_exit;

pub async fn run(socket: &Path) {
    let mut client = connect_or_exit(socket).await;
    let resp = match client.module_list(ModuleListRequest {}).await {
        Ok(r) => r.into_inner(),
        Err(e) => crate::rpc_error_or_not_running(socket, e),
    };

    println!("{:<20} {}", "module", "status");
    println!("{:-<20} {:-<20}", "", "");
    for m in &resp.modules {
        println!("{:<20} {}", m.name, m.status_line);
    }
}
