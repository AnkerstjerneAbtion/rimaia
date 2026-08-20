use std::path::Path;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Sends diagnostics to stderr *and* to a daily-rotated file under
/// `<app-data>/logs/`.
///
/// Both, not either: stderr is what a developer running `tauri dev` reads, and
/// the file is what survives an overnight queue nobody was watching. These are
/// Rimaia's own logs — run transcripts are separate JSONL files (ADR-0013).
///
/// The appender writes synchronously. `tracing-appender`'s non-blocking writer
/// would be faster, but it drops buffered lines unless its guard outlives the
/// process — and the log line that matters most here is the one written just
/// before a crash.
pub fn init(logs_dir: &Path) {
    let file_appender = tracing_appender::rolling::daily(logs_dir, "rimaia.log");

    let filter = EnvFilter::try_from_env("RIMAIA_LOG")
        .unwrap_or_else(|_| EnvFilter::new("rimaia=debug,rimaia_core=debug,warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_appender),
        )
        .init();
}
