use std::path::PathBuf;

use clap::Parser;

/// A focused, polished TUI for staging changes and viewing diffs.
#[derive(Debug, Parser)]
#[command(name = "strix", version, about)]
pub struct Cli {
    /// Path to the git repository (defaults to the current directory).
    pub path: Option<PathBuf>,

    /// Render a single frame to stdout as text, then exit (debugging aid).
    #[arg(long)]
    pub dump_frame: bool,

    /// Terminal width to use with --dump-frame.
    #[arg(long, default_value_t = 120, requires = "dump_frame")]
    pub width: u16,

    /// Terminal height to use with --dump-frame.
    #[arg(long, default_value_t = 40, requires = "dump_frame")]
    pub height: u16,
}
