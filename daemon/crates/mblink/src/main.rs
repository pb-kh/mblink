mod commands;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use mblink_core::control::connect_uds;
use mblink_core::dirs::MblinkDirs;
use mblink_proto::mblink::link::v1::control_client::ControlClient;
use tokio::time::timeout;
use tonic::transport::Channel;

pub(crate) const CLI_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "mblink")]
#[command(about = "Control the mblinkd daemon")]
struct Cli {
    /// Path to the mblinkd control socket
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show daemon status
    Status,
    /// List modules
    Modules,
    /// Get or set module configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Get configuration for a module
    Get {
        /// Module name
        module: String,
    },
    /// Set a package's enabled state for a module
    Set {
        /// Module name
        module: String,
        /// Package name
        package: String,
        /// New state: on or off
        state: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let socket = match resolve_socket(cli.socket) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cli.command {
        Command::Status => commands::status::run(&socket).await,
        Command::Modules => commands::modules::run(&socket).await,
        Command::Config { action } => match action {
            ConfigAction::Get { module } => commands::config::get(&socket, &module).await,
            ConfigAction::Set {
                module,
                package,
                state,
            } => {
                let enabled = match parse_state(&state) {
                    Some(v) => v,
                    None => {
                        eprintln!("error: state must be 'on' or 'off', got '{state}'");
                        return ExitCode::FAILURE;
                    }
                };
                commands::config::set(&socket, &module, &package, enabled).await;
            }
        },
    }

    ExitCode::SUCCESS
}

fn resolve_socket(override_socket: Option<PathBuf>) -> Result<PathBuf, String> {
    match MblinkDirs::resolve(None, None, override_socket) {
        Ok(dirs) => Ok(dirs.socket),
        Err(e) => Err(e.to_string()),
    }
}

fn parse_state(s: &str) -> Option<bool> {
    match s {
        "on" | "true" | "1" | "yes" | "enabled" => Some(true),
        "off" | "false" | "0" | "no" | "disabled" => Some(false),
        _ => None,
    }
}

pub(crate) async fn connect_or_exit(socket: &Path) -> ControlClient<Channel> {
    match timeout(CLI_TIMEOUT, connect_uds(socket.to_path_buf())).await {
        Ok(Ok(client)) => client,
        Ok(Err(_)) => not_running(socket),
        Err(_) => timed_out(socket),
    }
}

pub(crate) fn timed_out(socket: &Path) -> ! {
    eprintln!("mblinkd timed out ({})", socket.display());
    std::process::exit(1);
}

pub(crate) fn rpc_error_or_not_running(socket: &Path, e: tonic::Status) -> ! {
    if e.code() == tonic::Code::Unavailable {
        not_running(socket);
    }
    eprintln!("error: {e}");
    std::process::exit(1);
}

fn not_running(socket: &Path) -> ! {
    eprintln!("mblinkd not running ({})", socket.display());
    std::process::exit(1);
}
