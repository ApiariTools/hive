//! Tracing subscriber initialization for interactive and daemon modes.

use tracing_subscriber::EnvFilter;

/// Initialize tracing for interactive CLI subcommands.
///
/// Writes to stderr with timestamps and level. Defaults to `warn` unless
/// `RUST_LOG` is set.
pub fn init_interactive() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Initialize tracing for daemon (foreground) mode.
///
/// Writes to a non-blocking file appender (`daemon.log` in the given directory).
/// Defaults to `info` unless `RUST_LOG` is set. Returns a `WorkerGuard` that
/// must be held alive for the process lifetime to avoid dropped logs.
pub fn init_daemon(
    log_dir: &std::path::Path,
) -> tracing_appender::non_blocking::WorkerGuard {
    let file_appender = tracing_appender::rolling::never(log_dir, "daemon.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    guard
}
