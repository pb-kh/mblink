# mblink Foundation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bootstrap the mblink monorepo with `modules.toml` as the single build-flag source, shared proto + codegen, the LWW config store with file watching, the UDS control plane, and runnable `mblinkd` + `mblink` skeletons that talk to each other.

**Architecture:** Rust workspace under `daemon/` with a generated proto crate, a core library (dirs, config, watch, control service), the daemon binary, and the CLI binary. The control plane is gRPC over a Unix domain socket — same tonic stack the phone link will later use. No LAN, no notisync, no TUI yet; those are plans 2–5.

**Tech Stack:** Rust (edition 2024, rustc 1.94.1), tonic + prost (system protoc 36.1), tokio, `notify` (file watching), `serde`/`toml`, `clap`, `tracing`/`tracing-subscriber`.

**Spec:** `docs/superpowers/specs/2026-11-08-mblink-design.md` (architecture, file layout, config semantics, control plane). D-Bus contract doc exists but is not used in this plan.

**Execution context:** the repo at `/home/alex/projects/mblink` is not yet git — Task 1 initializes it. Execution happens in place; worktrees become available to later plans.

## Global Constraints

- Workspace root = repo root; all Rust code under `daemon/`; shared protos under `proto/`.
- Cargo features are named `module-<modules.toml key>` and `service-<modules.toml key>`; `daemon/crates/mblinkd/Cargo.toml` declares them and its `[features] default` must match `modules.toml` values — `xtask check-modules` enforces this.
- This plan ships `modules.toml` with `notisync = false` (module lands in plan 3), `systemd = true`, `openrc = false`.
- Default paths: config root `$XDG_CONFIG_HOME/mblink` else `~/.config/mblink`; state root `$XDG_STATE_HOME/mblinkd` else `~/.local/state/mblinkd`; control socket `$XDG_RUNTIME_DIR/mblink.sock`, permissions `0600`.
- Module config files live at `<config root>/<module>/config.toml`; core config at `<config root>/config.toml`.
- Config schema per entry: `package`, `enabled`, `updated_at` (unix epoch **millis**, u64), `updated_by` (`"linux"` or `"phone"`).
- LWW merge compares the tuple `(updated_at, updated_by)`; greater wins; exact tie keeps the existing entry.
- All config writes are atomic (write to temp file in the same dir, then rename).
- `mblinkd` never self-daemonizes; SIGTERM/SIGINT → graceful shutdown, exit 0.
- No manual-sync semantics anywhere in the control plane (no refresh/resync RPCs).
- Conventional commits (`feat:`, `test:`, `chore:`); every task's tests green before its commit.

## Review Focus

Failure modes the spec implies but normal task tests don't hit — each pinned to its owning task:

1. **Corrupt/hand-mangled `config.toml`** must not crash the daemon or wipe state: log a parse error, keep serving the last-good in-memory map. → Task 7 test.
2. **Stale socket file** at the socket path (dead daemon) must be replaced, not fail startup forever; a *live* second daemon must be refused. → Task 6 test.
3. **Identical `updated_at` from both peers** must resolve deterministically, not flicker. → Task 4 test (tuple tiebreak).
4. **Interrupted save** must never leave a half-written `config.toml`. → Task 4 test (atomic tmp+rename, no residue).
5. **Idle `WatchStatus` consumer** (subscribed, never reading) must not block the control server. → Task 6 test.

---

### Task 1: Repo bootstrap

**Files:**
- Create: `.gitignore`, `Cargo.toml` (workspace), `.cargo/config.toml`
- Create: `daemon/crates/mblink-core/src/lib.rs`, `daemon/crates/mblink-core/Cargo.toml`
- Create: `daemon/crates/mblink-proto/src/lib.rs`, `daemon/crates/mblink-proto/Cargo.toml`
- Create: `daemon/crates/mblinkd/src/main.rs`, `daemon/crates/mblinkd/Cargo.toml`
- Create: `daemon/crates/mblink/src/main.rs`, `daemon/crates/mblink/Cargo.toml`
- Create: `daemon/xtask/src/main.rs`, `daemon/xtask/Cargo.toml`

**Interfaces:**
- Consumes: nothing (first task).
- Produces: a compiling workspace; crate names `mblink-core`, `mblink-proto`, `mblinkd`, `mblink`, `xtask`; the `cargo xtask` alias.

- [ ] **Step 1: Initialize git and commit existing docs**

Run: `git init && git add .gitignore docs && git commit -m "chore: init repo with design docs"` — `.gitignore` contains at minimum `/target/` and `/.superpowers/`.

- [ ] **Step 2: Write the workspace manifest**

Root `Cargo.toml`: `[workspace]` with `resolver = "2"`, members as listed above; `[workspace.package]` with `edition = "2024"`, `version = "0.1.0"` (all crates inherit); `[workspace.dependencies]` pinning `tokio = { features = ["full"] }`, `tonic`, `prost`, `clap = { features = ["derive"] }`, `tracing`, `tracing-subscriber`, `notify`, `serde`, `toml`, `tempfile` (dev). Stub binaries print nothing and exit 0; lib.rs files are empty.

- [ ] **Step 3: Add the xtask alias**

`.cargo/config.toml`: `[alias] xtask = "run -p xtask --"`. `daemon/xtask/src/main.rs` prints `xtask: no command` and exits 1 (subcommand added in Task 2).

- [ ] **Step 4: Verify the workspace builds clean**

Run: `cargo build --workspace`
Expected: success, zero warnings.

- [ ] **Step 5: Commit**

`git add -A && git commit -m "chore: bootstrap rust workspace skeleton"`

### Task 2: modules.toml + xtask check-modules

**Files:**
- Create: `modules.toml` (repo root)
- Modify: `daemon/xtask/src/main.rs` (add `check-modules` subcommand)
- Create: `daemon/xtask/src/modules_check.rs` (+ `#[cfg(test)]` tests inside)
- Modify: `daemon/crates/mblinkd/Cargo.toml` (declare features)

**Interfaces:**
- Consumes: workspace from Task 1.
- Produces: `xtask::modules_check::check(modules_path, mblinkd_manifest_path) -> Result<(), Vec<String>>`; the `cargo xtask check-modules` command; feature names `module-notisync`, `service-systemd`, `service-openrc` on the `mblinkd` crate.

- [ ] **Step 1: Write modules.toml and mblinkd features**

`modules.toml` (exact content):

```toml
[modules]
notisync = false   # plan 3 flips this to true

[service]
systemd = true
openrc = false
```

`daemon/crates/mblinkd/Cargo.toml`: `[features] default = ["service-systemd"]`, `module-notisync = []`, `service-systemd = []`, `service-openrc = []`.

- [ ] **Step 2: Write the failing tests for `check`**

Tests in `daemon/xtask/src/modules_check.rs`, using `tempfile` fixtures (a modules.toml + a Cargo.toml manifest string):
- valid pair → `Ok(())`
- modules.toml key `foo` without `module-foo` feature → error naming `module-foo`
- feature exists but `default` membership disagrees with the toml value → error naming the feature and expected state

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p xtask`
Expected: FAIL (module not found).

- [ ] **Step 4: Implement `check`**

Parse both files with the `toml` crate; for each `[modules]`/`[service]` key assert the `module-<key>`/`service-<key>` feature exists in `mblinkd/Cargo.toml` and that `default` contains it iff the toml value is true. `main.rs` gains clap-derived subcommand `CheckModules` that locates the repo root via `CARGO_MANIFEST_DIR/../../..` and exits 1 printing each error.

- [ ] **Step 5: Verify tests pass and the real tree validates**

Run: `cargo test -p xtask && cargo xtask check-modules`
Expected: PASS, then `modules.toml consistent`.

- [ ] **Step 6: Commit**

`git add -A && git commit -m "feat: modules.toml as single source of build flags, enforced by xtask"`

### Task 3: proto — link.v1 Control service

**Files:**
- Create: `proto/mblink/link/v1/link.proto`
- Create: `daemon/crates/mblink-proto/build.rs`, modify its `Cargo.toml`/`src/lib.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `mblink_proto::mblink::link::v1::{ConfigEntry, ConfigMap, StatusRequest, StatusResponse, ModuleInfo, ModuleListRequest, ModuleListResponse, GetConfigRequest, GetConfigResponse, SetConfigRequest, SetConfigResponse, WatchStatusRequest}` and `mblink_proto::mblink::link::v1::control_server::{Control, ControlServer}` / `control_client::ControlClient`. Field names and numbers are frozen (spec's proto discipline: additive-only, never reused).

- [ ] **Step 1: Write link.proto**

Exact content:

```protobuf
syntax = "proto3";
package mblink.link.v1;

message ConfigEntry {
  string package = 1;
  bool enabled = 2;
  uint64 updated_at = 3;   // unix epoch millis
  string updated_by = 4;   // "linux" | "phone"
}

message ConfigMap { repeated ConfigEntry entries = 1; }

message StatusRequest {}
message StatusResponse {
  string version = 1;
  uint64 uptime_ms = 2;
  string link_state = 3;   // "disconnected" until the link plan
  string peer_name = 4;    // empty until paired
  repeated ModuleInfo modules = 5;
}

message ModuleInfo { string name = 1; string status_line = 2; }

message ModuleListRequest {}
message ModuleListResponse { repeated ModuleInfo modules = 1; }

message GetConfigRequest { string module = 1; }
message GetConfigResponse { ConfigMap map = 1; }

message SetConfigRequest {
  string module = 1;
  repeated ConfigEntry updates = 2;
}
message SetConfigResponse { ConfigMap merged = 1; }

message WatchStatusRequest {}

service Control {
  rpc Status(StatusRequest) returns (StatusResponse);
  rpc ModuleList(ModuleListRequest) returns (ModuleListResponse);
  rpc GetConfig(GetConfigRequest) returns (GetConfigResponse);
  rpc SetConfig(SetConfigRequest) returns (SetConfigResponse);
  rpc WatchStatus(WatchStatusRequest) returns (stream StatusResponse);
}
```

- [ ] **Step 2: Write the codegen smoke test**

Test in `mblink-proto` (or a sibling `tests/` file): construct a `SetConfigRequest` with one `ConfigEntry { package: "com.google.android.gm".into(), enabled: false, updated_at: 1, updated_by: "linux".into() }`, assert its `module` field round-trips and `entries.len() == 1`. Proves codegen produced the named types with the named fields.

- [ ] **Step 3: Implement build.rs and lib.rs**

`build.rs`: `tonic_build::configure().build_client(true).build_server(true).compile(&["../../../proto/mblink/link/v1/link.proto"])` (relative path resolves from the crate dir to the repo root's `proto/`). `src/lib.rs`: `tonic::include_proto!("mblink.link.v1");` re-exported as `pub mod mblink { pub mod link { pub mod v1 { ... } } }`.

- [ ] **Step 4: Verify**

Run: `cargo test -p mblink-proto`
Expected: PASS (codegen ran, types exist).

- [ ] **Step 5: Commit**

`git add -A && git commit -m "feat: link.v1 control proto + tonic codegen"`

### Task 4: mblink-core — config types, LWW merge, store

**Files:**
- Create: `daemon/crates/mblink-core/src/config.rs` (types, merge, store, toml (de)serialization)
- Test: `daemon/crates/mblink-core/src/config.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `mblink_proto` types (a `From<ConfigMap> proto ⇄ core` conversion).
- Produces:
  - `pub struct ConfigEntry { pub package: String, pub enabled: bool, pub updated_at: u64, pub updated_by: String }`
  - `pub struct ConfigMap { pub entries: BTreeMap<String, ConfigEntry> }` with `to_toml(&self) -> String` and `from_toml(&str) -> Result<Self, toml::de::Error>`
  - `pub fn merge_lww(base: &mut ConfigMap, incoming: &ConfigMap)` — per package: absent → insert; else incoming wins iff `(updated_at, updated_by)` tuple is greater; tie keeps base
  - `pub fn merge_from_file(current: &mut ConfigMap, file: &ConfigMap, mtime_ms: u64)` — entries whose `enabled` differs from `current` get `updated_at = mtime_ms, updated_by = "linux"`, then `merge_lww(current, file)` (hand-edit semantics from the spec)
  - `pub struct ConfigStore { root: PathBuf }` with `new(root)`, `path(&self, module: &str) -> PathBuf` (`root/<module>/config.toml`), `load(&self, module: &str) -> Result<ConfigMap, ConfigLoadError>` (missing file → `Ok` empty; parse failure → `Err(ConfigLoadError::Parse)`), `save(&self, module: &str, map: &ConfigMap) -> std::io::Result<()>` (atomic tmp+rename)

- [ ] **Step 1: Write the failing tests**

Cover, with exact values: TOML roundtrip; merge (newer wins / older ignored / absent inserted / **tie on `updated_at` resolved by `updated_by`**); `merge_from_file` (a flipped `enabled` with stale `updated_at` still wins with `mtime_ms`); `save` then `load` roundtrip; `save` leaves no `*.tmp` residue in the module dir; corrupt TOML → `Err(Parse)`; missing file → empty `Ok`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mblink-core`
Expected: FAIL (module not found).

- [ ] **Step 3: Implement `config.rs`**

TOML shape for a map (hand-editable per spec):

```toml
[apps."com.google.android.gm"]
enabled = true
updated_at = 1760000000123
updated_by = "linux"
```

Atomic save: write `<path>.tmp<N>` in the same directory, `fsync`, `rename`.

- [ ] **Step 4: Verify**

Run: `cargo test -p mblink-core`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "feat: core config store with LWW merge and atomic toml persistence"`

### Task 5: mblink-core — dirs + config watcher

**Files:**
- Create: `daemon/crates/mblink-core/src/dirs.rs`
- Create: `daemon/crates/mblink-core/src/watch.rs`
- Modify: `daemon/crates/mblink-core/src/lib.rs` (pub mods)
- Test: inside both modules

**Interfaces:**
- Consumes: `ConfigStore` (Task 4) for path conventions.
- Produces:
  - `pub struct MblinkDirs { pub config_root: PathBuf, pub state_root: PathBuf, pub socket: PathBuf }` and `pub fn resolve(config_dir: Option<PathBuf>, state_dir: Option<PathBuf>, socket: Option<PathBuf>) -> Result<Self, DirsError>` — XDG defaults per Global Constraints; `DirsError::NoRuntimeDir` when `$XDG_RUNTIME_DIR` is unset and no socket override given
  - `pub struct ConfigWatcher { /* handle */ }` with `pub fn spawn<F: Fn(String) + Send + 'static>(config_root: PathBuf, on_change: F) -> Self` — watches `<config_root>/**/config.toml` recursively via `notify`, debounces 100 ms, calls `on_change(module_name)` where `module_name` is the parent directory's name

- [ ] **Step 1: Write the failing tests**

`dirs`: with `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`XDG_RUNTIME_DIR` pointed at tempdirs (set env per-test, serially), `resolve` returns the spec paths; unset runtime dir + no override → `Err(NoRuntimeDir)`.
`watch`: tempdir config root; create `notisync/config.toml`; `ConfigWatcher::spawn` with an mpsc sender; modify the file; `recv_timeout(Duration::from_secs(2))` yields `"notisync"`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mblink-core`
Expected: FAIL.

- [ ] **Step 3: Implement both modules**

`notify`'s `recommended_watcher` in a dedicated thread with a 100 ms debounce window (collect events, emit distinct module names once the window closes). Env access via `std::env::var`.

- [ ] **Step 4: Verify**

Run: `cargo test -p mblink-core`
Expected: PASS (watcher test is time-based — 2 s timeout, generous debounce margin).

- [ ] **Step 5: Commit**

`git commit -m "feat: xdg dir resolution and debounced config file watcher"`

### Task 6: mblink-core — UDS control server + client

**Files:**
- Create: `daemon/crates/mblink-core/src/control.rs`
- Modify: `daemon/crates/mblink-core/src/lib.rs`
- Test: `daemon/crates/mblink-core/tests/control_uds.rs`

**Interfaces:**
- Consumes: proto `Control` service + types (Task 3); `ConfigStore`, `merge_lww` (Task 4); `ConfigWatcher` (Task 5, used by the daemon in Task 7 — not this task).
- Produces:
  - `pub const VERSION: &str = env!("CARGO_PKG_VERSION")` (re-exported from lib root)
  - `pub struct DaemonState { pub started_at: Instant, pub modules: Vec<ModuleDescriptor>, pub config: ConfigStore, pub status_tx: tokio::sync::watch::Sender<StatusResponse> }` with `pub fn new(modules: Vec<ModuleDescriptor>, config: ConfigStore) -> Self` and `pub fn status(&self) -> StatusResponse` (`link_state: "disconnected"`, `peer_name: ""`, `uptime_ms` computed)
  - `pub struct ModuleDescriptor { pub name: String, pub status_line: String }`
  - `pub async fn serve_uds(state: Arc<DaemonState>, path: PathBuf) -> Result<(), Box<dyn std::error::Error>>` — if `path` exists and is not a live socket, remove it and bind; if a live daemon answers, return `Err("already running (<path>)")`; bind `tokio::net::UnixListener`, `set_permissions(0o600)`, `tonic::transport::Server::builder().add_service(ControlServer::new(ControlService::new(state))).serve_with_incoming(...)`
  - `pub async fn connect_uds(path: PathBuf) -> Result<ControlClient<tonic::transport::Channel>, tonic::transport::Error>` — `Endpoint::from_static("http://localhost").connect_with_connector(tower::service_fn(move |_| tokio::net::UnixStream::connect(path.clone())))`
  - `ControlService` implements the five RPCs; module names are validated `^[a-z0-9][a-z0-9-]*$` → else `InvalidArgument`; `SetConfig` = load → `merge_lww` → save → respond with merged

- [ ] **Step 1: Write the failing integration test**

`tests/control_uds.rs` (tokio test): build `DaemonState` on a tempfile config root with one `ModuleDescriptor { name: "notisync", status_line: "stub" }`; `serve_uds` on a tempdir socket; then via `connect_uds`:
- `Status` → `version == VERSION`, `link_state == "disconnected"`, modules contains `notisync`
- `GetConfig("notisync")` → empty map
- `SetConfig("notisync", [entry com.google.android.gm enabled=true updated_at=100 updated_by="linux"])` → merged contains it, **and the file `<root>/notisync/config.toml` exists on disk with `enabled = true`**
- second `SetConfig` with older `updated_at` → merged keeps the newer (LWW visible through the API)
- `GetConfig("../etc")` → `InvalidArgument` status
- **stale socket**: drop the server, leave a plain file at the socket path, `serve_uds` again → succeeds
- **live conflict**: with the first server still running on a path, a second `serve_uds` on the same path → `Err` containing `already running`
- **idle WatchStatus**: open `WatchStatus` but never poll the stream; a second client's `Status` completes within 2 s

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mblink-core --test control_uds`
Expected: FAIL.

- [ ] **Step 3: Implement `control.rs`**

`WatchStatus`: `let mut rx = state.status_tx.subscribe();` yield `state.status()` first, then `loop { rx.changed().await; yield latest }`. `serve_with_incoming` takes the `UnixListener`'s accept stream mapped to tonic `Connected`/`IO` types.

- [ ] **Step 4: Verify**

Run: `cargo test -p mblink-core --test control_uds`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "feat: uds gRPC control plane with lww config write-through"`

### Task 7: mblinkd binary

**Files:**
- Create: `daemon/crates/mblinkd/src/main.rs`
- Create: `daemon/crates/mblinkd/src/args.rs` (clap), `daemon/crates/mblinkd/src/logging.rs`
- Test: `daemon/crates/mblinkd/tests/daemon_process.rs`

**Interfaces:**
- Consumes: `MblinkDirs`, `ConfigStore`, `merge_from_file`, `ConfigWatcher`, `DaemonState`, `serve_uds`, `ModuleDescriptor` (Tasks 4–6).
- Produces: the `mblinkd` binary with the spec's flag set: `--config-dir`, `--state-dir`, `--socket`, `--listen` (accepted, logs a warning "link not implemented yet" until plan 2), `--log-file`, `--log-format text|json`, `-v` (info → debug → trace), `--check`, `--version` (version + compiled module list from `cfg!(feature = ...)`).

- [ ] **Step 1: Write the failing process test**

`tests/daemon_process.rs` spawns the built binary (`env!("CARGO_BIN_EXE_mblinkd")`) with `--config-dir/--state-dir/--socket` on tempdirs:
- `--version` → stdout contains the version and `modules:` line; exit 0
- `--check` on a fresh tree → report + exit 0; on a tree with a corrupt `notisync/config.toml` → exit 1 naming the file
- daemon startup → socket file appears within 5 s; a UDS client `Status` succeeds
- **corrupt hand-edit while running** (Review Focus #1): write garbage into `notisync/config.toml`; wait 1 s; `GetConfig` still returns the pre-edit map
- **valid hand-edit** (spec's mtime semantics): pre-seed `notisync/config.toml` with an unticked entry (stale `updated_at`); start daemon; flip `enabled = true` in the file; within 2 s `GetConfig` shows `enabled = true` with `updated_at` = file mtime (millis) and `updated_by = "linux"`
- SIGTERM → process exits 0 within 2 s

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mblinkd`
Expected: FAIL.

- [ ] **Step 3: Implement the binary**

`clap` derive per the flag list (exact long names from the Interfaces block). `main`: init `tracing` (stderr default; `--log-file` appends via `tracing_appender`; `--log-format json` switches the fmt layer). `--check` and `--version` short-circuit before serving. Otherwise: `MblinkDirs::resolve` → `DaemonState::new(compiled_modules(), ConfigStore::new(dirs.config_root))` → `ConfigWatcher::spawn` reload handler (load file; on `Err(Parse)` log error + keep memory copy; on `Ok` apply `merge_from_file(current, file, mtime_ms)`) → `serve_uds` → await `ctrl_c` **or** SIGTERM (`tokio::signal::unix::signal(SignalKind::terminate())`), then drop and exit 0. Bind failure prints `already running (<path>)` to stderr, exit 1.

- [ ] **Step 4: Verify**

Run: `cargo test -p mblinkd`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "feat: mblinkd headless daemon with watcher merge, signals, and check mode"`

### Task 8: mblink CLI

**Files:**
- Create: `daemon/crates/mblink/src/main.rs`
- Create: `daemon/crates/mblink/src/commands/{status.rs, modules.rs, config.rs}`
- Test: `daemon/crates/mblink/tests/cli_process.rs`

**Interfaces:**
- Consumes: `connect_uds`, proto client (Task 6); binary path via `CARGO_BIN_EXE_mblinkd` in tests.
- Produces: `mblink status` (version, uptime, link state, peer, modules table); `mblink modules` (name + status line); `mblink config get <module>` (package | enabled | updated_at | updated_by table); `mblink config set <module> <package> <on|off>` (single-entry `SetConfig` with `updated_at = now_ms`, `updated_by = "linux"`; prints the merged state of that package). Unreachable daemon → stderr `mblinkd not running (<socket path>)`, exit 1. Note for later plans: `pair`, `service`, and the TUI subcommands arrive in plans 2/4.

- [ ] **Step 1: Write the failing process test**

Spawn `mblinkd` on tempdirs; then run the `mblink` binary:
- `status` → stdout contains `disconnected` and `notisync`
- `config set notisync com.google.android.gm on` → exit 0, then `config get notisync` shows the package `enabled`
- daemon killed → `status` exits 1 with `not running`

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mblink`
Expected: FAIL.

- [ ] **Step 3: Implement the CLI**

`clap` derive with subcommands `Status`, `Modules`, `Config { Get(module), Set(module, package, state) }`; global `--socket` override (default from `MblinkDirs::resolve`). Table printing is plain `println!` column alignment — no table dependency.

- [ ] **Step 4: Verify**

Run: `cargo test -p mblink`
Expected: PASS.

- [ ] **Step 5: Commit**

`git commit -m "feat: mblink cli with status, modules, and config subcommands"`

### Task 9: README + workspace verification

**Files:**
- Create: `README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: build/run documentation; a clean workspace gate.

- [ ] **Step 1: Write README.md**

Sections: what mblink is (one paragraph + link to the spec doc); build (`cargo build --workspace`, `cargo xtask check-modules`); run (`mblinkd` flags table — copy from Task 7's Interfaces block; niri `spawn-at-startup` snippet including `--log-file`); CLI examples (`mblink status`, `mblink config set …`); layout (the tree from the spec's Repo layout section).

- [ ] **Step 2: Run the full gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo xtask check-modules`
Expected: all pass.

- [ ] **Step 3: Commit**

`git add -A && git commit -m "chore: readme and workspace verification gate"`
