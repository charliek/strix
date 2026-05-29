use std::path::PathBuf;

use clap::Parser;

/// A focused, polished TUI for staging changes and viewing diffs.
#[derive(Debug, Parser)]
#[command(name = "strix", version, about)]
pub struct Cli {
    /// Path to the git repository (defaults to the current directory).
    pub path: Option<PathBuf>,

    /// Theme for this run: a built-in preset (tokyo-night, dark, light,
    /// catppuccin, gruvbox) or a file in ~/.config/strix/themes/.
    #[arg(long)]
    pub theme: Option<String>,

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
