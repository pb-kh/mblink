use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::{Endpoint, Server};
use tonic::{Request, Response, Status};

use mblink_proto::mblink::link::v1::{
    control_client::ControlClient,
    control_server::{Control as ControlTrait, ControlServer},
    ConfigEntry as ProtoConfigEntry,
    GetConfigRequest, GetConfigResponse, ModuleInfo, ModuleListRequest, ModuleListResponse,
    SetConfigRequest, SetConfigResponse, StatusRequest, StatusResponse, WatchStatusRequest,
};

use crate::config::{merge_lww, ConfigEntry, ConfigMap, ConfigStore};

/// Daemon version, re-exported from the crate manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Description of a loaded module for the control plane.
#[derive(Debug, Clone)]
pub struct ModuleDescriptor {
    pub name: String,
    pub status_line: String,
}

/// Shared daemon state exposed through the control service.
pub struct DaemonState {
    pub started_at: Instant,
    pub modules: Vec<ModuleDescriptor>,
    pub config: ConfigStore,
    pub status_tx: watch::Sender<StatusResponse>,
}

impl DaemonState {
    pub fn new(modules: Vec<ModuleDescriptor>, config: ConfigStore) -> Self {
        let status = build_status_response(&modules);
        let (status_tx, _rx) = watch::channel(status);
        Self {
            started_at: Instant::now(),
            modules,
            config,
            status_tx,
        }
    }

    pub fn status(&self) -> StatusResponse {
        let mut response = build_status_response(&self.modules);
        response.uptime_ms = self.started_at.elapsed().as_millis() as u64;
        response
    }
}

fn build_status_response(modules: &[ModuleDescriptor]) -> StatusResponse {
    StatusResponse {
        version: VERSION.to_string(),
        uptime_ms: 0,
        link_state: "disconnected".to_string(),
        peer_name: "".to_string(),
        modules: modules
            .iter()
            .map(|m| ModuleInfo {
                name: m.name.clone(),
                status_line: m.status_line.clone(),
            })
            .collect(),
    }
}

/// Serve the gRPC control service on a Unix domain socket.
///
/// If `path` already exists, the function probes whether another daemon is
/// listening.  A live daemon causes an immediate `already running` error.  A
/// stale socket or plain file is removed before binding.
pub async fn serve_uds(
    state: Arc<DaemonState>,
    path: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if path.exists() {
        // Probe for a live daemon.
        if let Ok(mut client) = connect_uds(path.clone()).await {
            if client.status(StatusRequest {}).await.is_ok() {
                return Err(format!("already running ({})", path.display()).into());
            }
        }
        // Not live: remove stale socket / plain file.
        tokio::fs::remove_file(&path).await?;
    }

    let listener = tokio::net::UnixListener::bind(&path)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;

    let server = ControlServer::new(ControlService::new(state));
    Server::builder()
        .add_service(server)
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await?;
    Ok(())
}

/// Connect a gRPC control client to a Unix domain socket.
pub async fn connect_uds(
    path: PathBuf,
) -> Result<ControlClient<tonic::transport::Channel>, tonic::transport::Error> {
    let channel = Endpoint::from_static("http://localhost")
        .connect_with_connector(tower::service_fn({
            let path = path.clone();
            move |_| {
                let path = path.clone();
                async move {
                    let io = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::tokio::TokioIo::new(io))
                }
            }
        }))
        .await?;
    Ok(ControlClient::new(channel))
}

pub struct ControlService {
    state: Arc<DaemonState>,
    /// Serializes load → merge → save for SetConfig so concurrent RPCs cannot
    /// overwrite each other and violate LWW.
    set_config_lock: tokio::sync::Mutex<()>,
}

impl ControlService {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self {
            state,
            set_config_lock: tokio::sync::Mutex::new(()),
        }
    }
}

#[tonic::async_trait]
impl ControlTrait for ControlService {
    async fn status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        Ok(Response::new(self.state.status()))
    }

    async fn module_list(
        &self,
        _request: Request<ModuleListRequest>,
    ) -> Result<Response<ModuleListResponse>, Status> {
        Ok(Response::new(ModuleListResponse {
            modules: self
                .state
                .modules
                .iter()
                .map(|m| ModuleInfo {
                    name: m.name.clone(),
                    status_line: m.status_line.clone(),
                })
                .collect(),
        }))
    }

    async fn get_config(
        &self,
        request: Request<GetConfigRequest>,
    ) -> Result<Response<GetConfigResponse>, Status> {
        let module = request.into_inner().module;
        validate_module(&module)?;
        let map = self
            .state
            .config
            .load(&module)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(GetConfigResponse {
            map: Some(map.into()),
        }))
    }

    async fn set_config(
        &self,
        request: Request<SetConfigRequest>,
    ) -> Result<Response<SetConfigResponse>, Status> {
        let request = request.into_inner();
        validate_module(&request.module)?;

        // Hold the lock across the entire load → merge → save critical section
        // so concurrent SetConfig RPCs see each other's writes.
        let _guard = self.set_config_lock.lock().await;

        let mut map = self
            .state
            .config
            .load(&request.module)
            .map_err(|e| Status::internal(e.to_string()))?;
        let incoming = config_map_from_proto_entries(request.updates);
        merge_lww(&mut map, &incoming);
        self.state
            .config
            .save(&request.module, &map)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(SetConfigResponse {
            merged: Some(map.into()),
        }))
    }

    type WatchStatusStream =
        Pin<Box<dyn Stream<Item = Result<StatusResponse, Status>> + Send + 'static>>;

    async fn watch_status(
        &self,
        _request: Request<WatchStatusRequest>,
    ) -> Result<Response<Self::WatchStatusStream>, Status> {
        let rx = self.state.status_tx.subscribe();
        let stream = tokio_stream::wrappers::WatchStream::new(rx).map(Ok);
        Ok(Response::new(Box::pin(stream)))
    }
}

fn validate_module(name: &str) -> Result<(), Status> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => {
            return Err(Status::invalid_argument(
                "module names must match ^[a-z0-9][a-z0-9-]*$",
            ))
        }
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(Status::invalid_argument(
                "module names must match ^[a-z0-9][a-z0-9-]*$",
            ));
        }
    }
    Ok(())
}

fn config_map_from_proto_entries(entries: Vec<ProtoConfigEntry>) -> ConfigMap {
    let mut map = ConfigMap::default();
    for entry in entries {
        map.entries.insert(
            entry.package.clone(),
            ConfigEntry {
                package: entry.package,
                enabled: entry.enabled,
                updated_at: entry.updated_at,
                updated_by: entry.updated_by,
            },
        );
    }
    map
}
