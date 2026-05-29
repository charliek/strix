//! strix — a focused, polished TUI for staging changes and viewing diffs.
//!
//! The binary (`src/main.rs`) is a thin wrapper around [`run`]; all logic lives
//! in the library so it can be exercised from `tests/*_test.rs` and from the
//! `--dump-frame` debugging path without driving a real terminal.

pub mod app;
pub mod cli;
pub mod logging;
pub mod terminal;
pub mod ui;

// Re-exported so consumers (and integration tests) can build input events
// against the exact crossterm version strix renders with.
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
    let app = app::App::new(repo_path)?;

    if cli.dump_frame {
        print!("{}", terminal::dump_frame(&app, cli.width, cli.height)?);
        return Ok(());
    }

    terminal::run(app)
}
