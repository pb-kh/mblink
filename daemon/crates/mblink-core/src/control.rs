use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::{Endpoint, Server};
use tonic::{Request, Response, Status};

/// Maximum time to wait for an existing endpoint to answer a probe before
/// treating it as live (and refusing to steal its socket).
const STALE_SOCKET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

use mblink_proto::mblink::link::v1::{
    ConfigEntry as ProtoConfigEntry, GetConfigRequest, GetConfigResponse, ModuleInfo,
    ModuleListRequest, ModuleListResponse, SetConfigRequest, SetConfigResponse, StatusRequest,
    StatusResponse, WatchStatusRequest,
    control_client::ControlClient,
    control_server::{Control as ControlTrait, ControlServer},
};

use crate::config::{ConfigEntry, ConfigMap, ConfigStore, merge_lww};

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
    config_cache: Mutex<HashMap<String, ConfigMap>>,
    /// Serializes every writer of a module's config (control-plane `SetConfig`
    /// and the watcher reload handler).  Both are read-modify-write cycles
    /// over the same file and cache; without this lock a save based on a
    /// pre-write snapshot silently drops the other writer's entries.
    config_write_lock: Mutex<()>,
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
            config_cache: Mutex::new(HashMap::new()),
            config_write_lock: Mutex::new(()),
        }
    }

    pub fn status(&self) -> StatusResponse {
        let mut response = build_status_response(&self.modules);
        response.uptime_ms = self.started_at.elapsed().as_millis() as u64;
        response
    }

    /// Return a cached config map, if one has been loaded or merged.
    pub fn cached_config(&self, module: &str) -> Option<ConfigMap> {
        self.config_cache.lock().unwrap().get(module).cloned()
    }

    /// Replace the cached config map for a module.
    pub fn set_cached_config(&self, module: &str, map: ConfigMap) {
        self.config_cache
            .lock()
            .unwrap()
            .insert(module.to_string(), map);
    }

    /// Acquire the lock serializing all writers of module config files.
    ///
    /// The guard must not be held across an `.await` (it is a sync mutex);
    /// the critical sections it protects are purely synchronous.
    /// Poison-tolerant on purpose: a panicking writer must not wedge config
    /// writes for the rest of the daemon's lifetime.
    pub fn lock_config_write(&self) -> MutexGuard<'_, ()> {
        match self.config_write_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
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
/// listening.  A live daemon (or an endpoint that accepts but does not answer
/// within the probe timeout) causes an `already running` error without
/// removing the path.  A provably stale endpoint (connect refused, not a
/// socket, or an immediate RPC error) is removed before binding.
pub async fn serve_uds(
    state: Arc<DaemonState>,
    path: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if path.exists() {
        let already_running = || -> Box<dyn std::error::Error + Send + Sync> {
            format!("already running ({})", path.display()).into()
        };
        match connect_uds(path.clone()).await {
            Ok(mut client) => {
                match tokio::time::timeout(
                    STALE_SOCKET_PROBE_TIMEOUT,
                    client.status(StatusRequest {}),
                )
                .await
                {
                    Ok(Ok(_)) => return Err(already_running()),
                    Ok(Err(_)) => {
                        // Immediate RPC error: endpoint spoke but is not a live
                        // mblinkd; treat as stale.
                    }
                    Err(_elapsed) => {
                        // Accepts but never answers: treat as live and do not
                        // steal the socket.
                        return Err(already_running());
                    }
                }
            }
            Err(_) => {
                // Connect refused / not a socket: treat as stale.
            }
        }
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
}

impl ControlService {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
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
        // Hold the shared write lock across the cache check, load, and cache
        // fill so a concurrent SetConfig (which saves to disk then updates the
        // cache under the same lock) cannot have its newer cache overwritten by
        // a get_config that loaded an older disk snapshot. The section is purely
        // synchronous, so holding a sync mutex here is safe.
        let _guard = self.state.lock_config_write();
        if let Some(cached) = self.state.cached_config(&module) {
            return Ok(Response::new(GetConfigResponse {
                map: Some(cached.into()),
            }));
        }
        let map = self
            .state
            .config
            .load(&module)
            .map_err(|e| Status::internal(e.to_string()))?;
        self.state.set_cached_config(&module, map.clone());
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

        // Hold the shared write lock across the entire load → merge → save
        // critical section so neither concurrent SetConfig RPCs nor the
        // watcher reload handler can base a save on a stale snapshot and
        // silently drop the other writer's entries.  The section is purely
        // synchronous, so holding a sync mutex here is safe.
        let _guard = self.state.lock_config_write();

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
        self.state.set_cached_config(&request.module, map.clone());
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
            ));
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
