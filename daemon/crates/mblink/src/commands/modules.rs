use std::path::Path;

use mblink_proto::mblink::link::v1::ModuleListRequest;
use tokio::time::timeout;

use crate::{CLI_TIMEOUT, connect_or_exit};

pub async fn run(socket: &Path) {
    let mut client = connect_or_exit(socket).await;
    let resp = match timeout(CLI_TIMEOUT, client.module_list(ModuleListRequest {})).await {
        Ok(Ok(r)) => r.into_inner(),
        Ok(Err(e)) => crate::rpc_error_or_not_running(socket, e),
        Err(_) => crate::timed_out(socket),
    };

    println!("{:<20} status", "module");
    println!("{:-<20} {:-<20}", "", "");
    for m in &resp.modules {
        println!("{:<20} {}", m.name, m.status_line);
    }
}
