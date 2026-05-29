use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Directory where strix writes its log file (`~/Library/Logs/strix` on macOS,
/// `$XDG_STATE_HOME/strix` on Linux). Both branches compile on every platform —
/// `cfg!` selects at runtime rather than `#[cfg]` excluding code — so neither
/// path can silently rot.
pub fn log_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Some(base) = BaseDirs::new() {
            return base.home_dir().join("Library/Logs/strix");
        }
    } else if let Some(proj) = ProjectDirs::from("", "", "strix") {
        return proj
            .state_dir()
            .unwrap_or_else(|| proj.cache_dir())
            .to_path_buf();
    }
    std::env::temp_dir().join("strix")
}

/// Initialise file-based logging. The returned guard must outlive the process;
/// dropping it flushes and stops the background writer. Logging never aborts
/// startup — if the log directory can't be created we simply run without logs.
///
/// Set `STRIX_LOG` (same syntax as `RUST_LOG`) to adjust verbosity.
pub fn init() -> Option<WorkerGuard> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok()?;

    let appender = tracing_appender::rolling::never(&dir, "strix.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_env("STRIX_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .ok()?;

    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .init();

    tracing::info!("strix {} starting", env!("CARGO_PKG_VERSION"));
    Some(guard)
}
