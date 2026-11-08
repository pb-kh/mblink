use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod modules_check;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify modules.toml and mblinkd Cargo.toml features are consistent.
    CheckModules,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::CheckModules => {
            let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .canonicalize()
                .expect("failed to locate repo root");
            let modules_path = repo_root.join("modules.toml");
            let manifest_path = repo_root.join("daemon/crates/mblinkd/Cargo.toml");

            match modules_check::check(&modules_path, &manifest_path) {
                Ok(()) => println!("modules.toml consistent"),
                Err(errors) => {
                    for err in errors {
                        eprintln!("{err}");
                    }
                    std::process::exit(1);
                }
            }
        }
    }
}
