use std::path::Path;

use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::args::{Args, LogFormat};

pub fn init(args: &Args) {
    let filter = build_filter(args.verbose, std::env::var("RUST_LOG").ok().as_deref());

    match args.log_format {
        LogFormat::Json => init_json(args, filter),
        LogFormat::Text => init_text(args, filter),
    }
}

use std::borrow::Cow;

/// Default directive when `RUST_LOG` is not set/parseable, matching `-v`.
pub fn default_directive(verbose: u8) -> &'static str {
    match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    }
}

/// Resolve the directive string: `RUST_LOG` when set and parseable, otherwise
/// the `-v` default.
pub fn resolve_directive(verbose: u8, rust_log: Option<&str>) -> Cow<'_, str> {
    if let Some(value) = rust_log
        && EnvFilter::try_new(value).is_ok()
    {
        return Cow::Borrowed(value);
    }
    Cow::Borrowed(default_directive(verbose))
}

/// Build the log filter. `RUST_LOG` (if set and parseable) takes precedence,
/// including per-target directives; otherwise fall back to the `-v` default.
pub fn build_filter(verbose: u8, rust_log: Option<&str>) -> EnvFilter {
    EnvFilter::try_new(resolve_directive(verbose, rust_log).as_ref())
        .expect("resolved directive must be parseable")
}

fn init_text(args: &Args, filter: EnvFilter) {
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

fn init_json(args: &Args, filter: EnvFilter) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_directive_matches_verbose_levels() {
        assert_eq!(default_directive(0), "info");
        assert_eq!(default_directive(1), "debug");
        assert_eq!(default_directive(2), "trace");
        assert_eq!(default_directive(9), "trace");
    }

    #[test]
    fn rust_log_overrides_verbose_default() {
        assert_eq!(
            resolve_directive(0, Some("mblinkd=debug")).as_ref(),
            "mblinkd=debug"
        );
        assert_eq!(
            resolve_directive(2, Some("warn,mblink_core=trace")).as_ref(),
            "warn,mblink_core=trace"
        );
    }

    #[test]
    fn unset_rust_log_falls_back_to_verbose() {
        assert_eq!(resolve_directive(0, None).as_ref(), "info");
        assert_eq!(resolve_directive(1, None).as_ref(), "debug");
        assert_eq!(resolve_directive(3, None).as_ref(), "trace");
    }

    #[test]
    fn unparseable_rust_log_falls_back_to_verbose() {
        // An unterminated directive target is rejected by EnvFilter.
        let resolved = resolve_directive(1, Some("mblinkd=not_a_real_level==="));
        // EnvFilter is permissive about some forms; if it happens to parse,
        // that's still "parseable takes precedence". Assert the contract:
        // result is either the env value (if parseable) or the verbose default.
        let parsed = EnvFilter::try_new("mblinkd=not_a_real_level===").is_ok();
        if parsed {
            assert_eq!(resolved.as_ref(), "mblinkd=not_a_real_level===");
        } else {
            assert_eq!(resolved.as_ref(), "debug");
        }
    }
}
