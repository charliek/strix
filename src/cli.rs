use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// A focused, polished TUI for staging changes and viewing diffs.
#[derive(Debug, Parser)]
#[command(name = "strix", version, about)]
pub struct Cli {
    /// Path to the git repository (defaults to the current directory).
    ///
    /// A bare `strix <name>` opens that directory; the subcommand names (e.g.
    /// `diff`) take precedence, so use `strix ./diff` to open a directory named
    /// `diff`.
    pub path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,

    /// Theme for this run: a built-in preset (tokyo-night, dark, light,
    /// catppuccin, gruvbox) or a file in ~/.config/strix/themes/.
    #[arg(long, global = true)]
    pub theme: Option<String>,

    /// Render a single frame to stdout as text, then exit (debugging aid).
    #[arg(long, global = true)]
    pub dump_frame: bool,

    /// Terminal width to use with --dump-frame.
    #[arg(long, global = true, default_value_t = 120, requires = "dump_frame")]
    pub width: u16,

    /// Terminal height to use with --dump-frame.
    #[arg(long, global = true, default_value_t = 40, requires = "dump_frame")]
    pub height: u16,
}

/// A strix subcommand. The root (no subcommand) is the status TUI; each
/// subcommand is a distinct entry point (the review-workflow track adds more).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Review a branch range (a base…head diff), GitHub-PR style.
    ///
    /// RANGE is `BASE` (⇒ merge-base(BASE, HEAD)..HEAD), `A...B`
    /// (⇒ merge-base(A, B)..B), or `A..B` (a direct comparison). An empty side
    /// means HEAD (`main..` ≡ `main..HEAD`).
    Diff {
        /// The range to review.
        range: String,
        /// Path to the git repository (defaults to the current directory).
        path: Option<PathBuf>,
    },
}

impl Cli {
    /// The repository path and, for a review subcommand, the range to resolve.
    /// The path is taken from the active subcommand when present, else the root
    /// positional.
    pub fn target(&self) -> (Option<PathBuf>, Option<String>) {
        match &self.command {
            Some(Command::Diff { range, path }) => (path.clone(), Some(range.clone())),
            None => (self.path.clone(), None),
        }
    }

    /// Parse from an explicit argument vector (including argv[0]). A thin wrapper
    /// over clap's `try_parse_from` so integration tests can exercise argument
    /// parsing without depending on clap directly.
    pub fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        <Cli as Parser>::try_parse_from(args)
    }
}
