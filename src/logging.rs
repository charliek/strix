use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
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
/// dropping it flushes and stops the background writer. Logging is best-effort:
/// if the log directory can't be created or the file appender can't be opened
/// (e.g. an unwritable log dir), we warn once on stderr and run without file
/// logging rather than aborting — repo-independent commands like `strix skill`
/// and `strix comment` must work even when the log location is unusable.
///
/// Set `STRIX_LOG` (same syntax as `RUST_LOG`) to adjust verbosity.
pub fn init() -> Option<WorkerGuard> {
    let dir = log_dir();
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "strix: file logging disabled (creating {}: {err})",
            dir.display()
        );
        return None;
    }

    // Build the appender fallibly — `rolling::never` panics if the file can't be
    // opened (unwritable dir), which would take down repo-independent commands.
    let appender = match RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix("strix.log")
        .build(&dir)
    {
        Ok(appender) => appender,
        Err(err) => {
            eprintln!(
                "strix: file logging disabled (opening {}/strix.log: {err})",
                dir.display()
            );
            return None;
        }
    };
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
