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

    /// Read and edit review comments for the checked-out branch (agent-facing).
    ///
    /// Every action operates on the current HEAD branch's inbox. Machine output
    /// is JSON on stdout (`--json`); diagnostics go to stderr and any failure
    /// exits non-zero (plan §3.3).
    Comment {
        #[command(subcommand)]
        action: CommentAction,
        /// Path to the git repository (defaults to the current directory).
        path: Option<PathBuf>,
    },

    /// Manage the bundled agent skill (`strix-review`).
    ///
    /// Repo-independent: it materializes a file under the user's data directory,
    /// so it takes no `[PATH]` and works from anywhere.
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
}

/// A `strix skill` action.
#[derive(Debug, Subcommand)]
pub enum SkillAction {
    /// Write the bundled `strix-review` skill to the data directory and print
    /// its absolute path (overwritten each run, so it tracks this binary).
    ///
    /// The location is `<data_dir>/strix/skills/strix-review/SKILL.md`, where
    /// `data_dir` is `$STRIX_DATA_DIR` if set to a non-empty value (an empty
    /// value counts as unset), else the platform data directory; a relative
    /// `$STRIX_DATA_DIR` is resolved against the current directory.
    Path {
        /// Emit `{"path": "…"}` on stdout instead of the bare path.
        #[arg(long)]
        json: bool,
    },
}

/// A `strix comment` action. All act on the current HEAD branch key.
#[derive(Debug, Subcommand)]
pub enum CommentAction {
    /// List this branch's comments (re-anchoring against the stored range first).
    List {
        /// Emit the machine-readable JSON contract instead of a plain table.
        #[arg(long)]
        json: bool,
    },
    /// Add an agent-authored comment anchored to a line of a file's diff.
    ///
    /// Exactly one of `--old-line` / `--new-line` selects the side; both must be
    /// ≥ 1. `--text` must be non-empty after trimming (its raw bytes, newlines
    /// included, are stored verbatim).
    Add {
        /// The file's new-side path, as listed in the review.
        #[arg(long)]
        file: String,
        /// Anchor to this 1-based line on the old (base) side.
        #[arg(long)]
        old_line: Option<usize>,
        /// Anchor to this 1-based line on the new (head) side.
        #[arg(long)]
        new_line: Option<usize>,
        /// The comment body (stored raw; may contain newlines).
        #[arg(long)]
        text: String,
        /// Emit the machine-readable JSON contract instead of the new id.
        #[arg(long)]
        json: bool,
    },
    /// Remove one comment by id from this branch.
    Rm {
        /// The id to remove (from `list`).
        id: u64,
        /// Emit the machine-readable JSON contract.
        #[arg(long)]
        json: bool,
    },
    /// Remove every comment on this branch.
    Clear {
        /// Emit the machine-readable JSON contract.
        #[arg(long)]
        json: bool,
    },
    /// Drop inboxes for branches/commits that no longer exist.
    Gc {
        /// Emit the machine-readable JSON contract.
        #[arg(long)]
        json: bool,
    },
}

impl Cli {
    /// The repository path and, for a review subcommand, the range to resolve.
    /// The path is taken from the active subcommand when present, else the root
    /// positional.
    pub fn target(&self) -> (Option<PathBuf>, Option<String>) {
        match &self.command {
            Some(Command::Diff { range, path }) => (path.clone(), Some(range.clone())),
            Some(Command::Comment { path, .. }) => (path.clone(), None),
            Some(Command::Skill { .. }) => (None, None),
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
