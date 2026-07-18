//! strix — a focused, polished TUI for staging changes and viewing diffs.
//!
//! The binary (`src/main.rs`) is a thin wrapper around [`run`]; all logic lives
//! in the library so it can be exercised from `tests/*_test.rs` and from the
//! `--dump-frame` debugging path without driving a real terminal.

pub mod app;
pub mod cli;
pub mod comments;
pub mod comments_cli;
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

    // `comment` is a CLI-only surface (no TUI): route it before any App
    // construction, exactly like the `--dump-frame` early return below.
    if let Some(cli::Command::Comment { action, path }) = &cli.command {
        let repo_path = match path {
            Some(path) => path.clone(),
            None => std::env::current_dir()?,
        };
        return comments_cli::run(&repo_path, action);
    }

    let (path, range) = cli.target();
    let repo_path = match path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };

    let mut config = config::load();
    config.theme = cli.theme.or(config.theme);
    // A `diff <RANGE>` invocation opens a review session; the range is resolved
    // here, before terminal setup, so a bad range bubbles out of main as a fatal
    // anyhow error (naming the offending operand) rather than a blank TUI.
    let app = match range {
        Some(range) => app::App::for_review(repo_path, &config, &range)?,
        None => app::App::with_config(repo_path, &config)?,
    }
    .with_config_dir(config::config_dir());

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
