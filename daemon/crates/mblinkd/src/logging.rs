use std::path::Path;

use tracing_subscriber::{
    Layer, filter::LevelFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::args::{Args, LogFormat};

pub fn init(args: &Args) {
    let filter = match args.verbose {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };

    match args.log_format {
        LogFormat::Json => init_json(args, filter),
        LogFormat::Text => init_text(args, filter),
    }
}

fn init_text(args: &Args, filter: LevelFilter) {
    if let Some(path) = &args.log_file {
        let (parent, file_name) = split_path(path);
        let appender = tracing_appender::rolling::never(parent, file_name);
        tracing_subscriber::registry()
            .with(fmt::layer().with_writer(appender).with_filter(filter))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(filter),
            )
            .init();
    }
}

fn init_json(args: &Args, filter: LevelFilter) {
    if let Some(path) = &args.log_file {
        let (parent, file_name) = split_path(path);
        let appender = tracing_appender::rolling::never(parent, file_name);
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .json()
                    .with_writer(appender)
                    .with_filter(filter),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .json()
                    .with_writer(std::io::stderr)
                    .with_filter(filter),
            )
            .init();
    }
}

fn split_path(path: &Path) -> (&Path, &str) {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("mblinkd.log");
    std::fs::create_dir_all(parent).ok();
    (parent, file_name)
}
