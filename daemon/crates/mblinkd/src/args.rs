use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "mblinkd")]
pub struct Args {
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,

    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    #[arg(long, value_name = "ADDR")]
    pub listen: Option<String>,

    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    #[arg(long, value_name = "FORMAT", value_enum, default_value_t = LogFormat::Text)]
    pub log_format: LogFormat,

    #[arg(short = 'v', action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[arg(long)]
    pub check: bool,

    #[arg(long)]
    pub version: bool,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}
