//! strix — a focused, polished TUI for staging changes and viewing diffs.
//!
//! The binary (`src/main.rs`) is a thin wrapper around [`run`]; all logic lives
//! in the library so it can be exercised from `tests/*_test.rs` and from the
//! `--dump-frame` debugging path without driving a real terminal.

pub mod app;
pub mod cli;
pub mod config;
pub mod git;
pub mod graph;
pub mod keys;
pub mod logging;
pub mod terminal;
pub mod ui;
pub mod util;
pub mod watch;

// Re-exported so consumers (and integration tests) can build input events and
// reference styles against the exact ratatui/crossterm version strix renders with.
pub use ratatui;
pub use ratatui::crossterm;

use anyhow::Result;
use clap::Parser;

/// Parse CLI arguments and run strix.
pub fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    let _log_guard = logging::init();

    let repo_path = match cli.path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };

    let mut config = config::load();
    config.theme = cli.theme.or(config.theme);
    let app = app::App::with_config(repo_path, &config)?;

    if cli.dump_frame {
        print!("{}", terminal::dump_frame(&app, cli.width, cli.height)?);
        return Ok(());
    }

    // Watch the true working-tree root (not a possibly-subdir CLI path) so every
    // change is caught. A watcher that fails to start degrades to manual refresh.
    let watch_rx = if config.auto_refresh() {
        match watch::spawn(app.repo.workdir().to_path_buf()) {
            Ok(rx) => Some(rx),
            Err(err) => {
                tracing::warn!("file watcher failed to start: {err:#}");
                None
            }
        }
    } else {
        None
    };

    terminal::run(app, watch_rx)
}
