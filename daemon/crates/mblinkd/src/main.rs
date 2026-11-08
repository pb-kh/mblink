mod args;
mod logging;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use args::Args;
use clap::Parser;
use mblink_core::config::{ConfigMap, ConfigStore, merge_from_file};
use mblink_core::control::{DaemonState, ModuleDescriptor, VERSION, serve_uds};
use mblink_core::dirs::MblinkDirs;
use mblink_core::watch::ConfigWatcher;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    logging::init(&args);

    if let Some(addr) = &args.listen {
        warn!(addr, "link not implemented yet");
    }

    if args.version {
        print_version();
        return ExitCode::SUCCESS;
    }

    // Config-only dry run: validates every module config under the config
    // root.  No socket is involved, so this must not require a resolvable
    // runtime dir.
    if args.check {
        return run_check(&args);
    }

    let dirs = match MblinkDirs::resolve(args.config_dir, args.state_dir, args.socket) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    info!(
        version = VERSION,
        config_root = %dirs.config_root.display(),
        state_root = %dirs.state_root.display(),
        socket = %dirs.socket.display(),
        "mblinkd starting"
    );

    let state = Arc::new(DaemonState::new(
        module_registry(),
        ConfigStore::new(dirs.config_root.clone()),
    ));

    // Seed the daemon's in-memory config copy for every registry module by
    // loading the file.  `merge_from_file` re-stamps entries whose `enabled`
    // value differs from the in-memory copy, so the copy must reflect the
    // pre-edit state when a hand-edit lands (the watcher emits no initial
    // event), and it must never be an empty map or phone provenance would
    // be destroyed.
    for module in &state.modules {
        match state.config.load(&module.name) {
            Ok(map) => state.set_cached_config(&module.name, map),
            Err(e) => {
                error!(
                    module = %module.name, error = %e,
                    "config unreadable at startup, waiting for a valid write"
                );
            }
        }
    }

    let watcher_state = Arc::clone(&state);
    let _watcher = ConfigWatcher::spawn(dirs.config_root.clone(), move |module| {
        handle_config_change(&watcher_state, &module);
    });

    let mut serve = tokio::spawn(serve_uds(Arc::clone(&state), dirs.socket.clone()));

    tokio::select! {
        res = &mut serve => {
            match res {
                Ok(Ok(())) => {
                    info!("server finished");
                }
                Ok(Err(e)) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
                Err(e) => {
                    eprintln!("server task failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            let mut sigterm = signal(SignalKind::terminate()).expect("sigterm signal");
            sigterm.recv().await;
        } => {}
    }

    serve.abort();
    let _ = serve.await;
    info!("mblinkd stopped");
    ExitCode::SUCCESS
}

/// The static module registry (preflight Ruling R1): every module known to
/// the project, with `status_line` derived from its compile-time feature.
/// Modules without their feature enabled are still listed as `not compiled`
/// so the control-plane module list is never empty, while `--version` only
/// reports compiled modules.
fn module_registry() -> Vec<ModuleDescriptor> {
    vec![ModuleDescriptor {
        name: "notisync".to_string(),
        status_line: if cfg!(feature = "module-notisync") {
            "compiled in"
        } else {
            "not compiled"
        }
        .to_string(),
    }]
}

fn print_version() {
    println!("mblinkd {}", VERSION);
    println!("modules:");
    for module in module_registry() {
        if module.status_line == "compiled in" {
            println!("  {}", module.name);
        }
    }
}

fn run_check(args: &Args) -> ExitCode {
    let config_root = match &args.config_dir {
        Some(dir) => dir.clone(),
        None => match MblinkDirs::resolve(None, None, Some(PathBuf::from("/dev/null"))) {
            Ok(dirs) => dirs.config_root,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
    };

    let store = ConfigStore::new(&config_root);
    let mut errors = Vec::new();
    match std::fs::read_dir(&config_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        errors.push(format!("{}: {e}", config_root.display()));
                        continue;
                    }
                };
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let module = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if module.is_empty() {
                    continue;
                }
                if let Err(e) = store.load(module) {
                    errors.push(format!("{}: {e}", store.path(module).display()));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => errors.push(format!("{}: {e}", config_root.display())),
    }

    if errors.is_empty() {
        println!("config check passed");
        ExitCode::SUCCESS
    } else {
        eprintln!("{}", errors.join("\n"));
        ExitCode::FAILURE
    }
}

fn handle_config_change(state: &Arc<DaemonState>, module: &str) {
    let current = match state.cached_config(module) {
        Some(map) => map,
        None => match state.config.load(module) {
            Ok(map) => map,
            Err(e) => {
                error!(module, error = %e, "failed to seed config cache, skipping reload");
                return;
            }
        },
    };

    let path = state.config.path(module);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            error!(path = %path.display(), error = %e, "failed to read config file");
            return;
        }
    };

    let file_map = match ConfigMap::from_toml(&content) {
        Ok(m) => m,
        Err(e) => {
            error!(module, error = %e, "config parse error, keeping last-good copy");
            return;
        }
    };

    let mtime_ms = match std::fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(t) => t
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        Err(e) => {
            error!(path = %path.display(), error = %e, "failed to read config mtime");
            return;
        }
    };

    let mut merged = current.clone();
    merge_from_file(&mut merged, &file_map, mtime_ms);

    if merged != current
        && let Err(e) = state.config.save(module, &merged)
    {
        error!(module, error = %e, "failed to save merged config");
        return;
    }
    state.set_cached_config(module, merged);
}
