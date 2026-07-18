//! CLI parsing (`Cli::try_parse`) and binary-level exit-code / stderr behaviour
//! for the `diff` subcommand and the global flags (plan §3.1).

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::init_repo_with_diverged_branches;
use strix::cli::{Cli, Command as CliCommand};

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse(args).expect("args should parse")
}

#[test]
fn root_without_path() {
    let cli = parse(&["strix"]);
    assert_eq!(cli.path, None);
    assert!(cli.command.is_none());
    assert_eq!(cli.target(), (None, None));
}

#[test]
fn root_with_path() {
    let cli = parse(&["strix", "some/repo"]);
    assert_eq!(cli.path, Some(PathBuf::from("some/repo")));
    assert_eq!(cli.target(), (Some(PathBuf::from("some/repo")), None));
}

#[test]
fn diff_with_range() {
    let cli = parse(&["strix", "diff", "main"]);
    match &cli.command {
        Some(CliCommand::Diff { range, path }) => {
            assert_eq!(range, "main");
            assert_eq!(path, &None);
        }
        other => panic!("expected diff subcommand, got {other:?}"),
    }
    assert_eq!(cli.target(), (None, Some("main".to_string())));
}

#[test]
fn diff_with_range_and_path() {
    let cli = parse(&["strix", "diff", "main..feature", "some/repo"]);
    assert_eq!(
        cli.target(),
        (
            Some(PathBuf::from("some/repo")),
            Some("main..feature".to_string())
        )
    );
}

#[test]
fn global_flags_before_and_after_subcommand() {
    let before = parse(&["strix", "--theme", "dark", "diff", "main"]);
    assert_eq!(before.theme.as_deref(), Some("dark"));
    assert_eq!(before.target().1, Some("main".to_string()));

    let after = parse(&["strix", "diff", "main", "--theme", "dark"]);
    assert_eq!(after.theme.as_deref(), Some("dark"));
    assert_eq!(after.target().1, Some("main".to_string()));
}

#[test]
fn dump_frame_flags_are_global() {
    let cli = parse(&["strix", "diff", "main", "--dump-frame", "--width", "80"]);
    assert!(cli.dump_frame);
    assert_eq!(cli.width, 80);
}

#[test]
fn width_without_dump_frame_errors() {
    // `requires = "dump_frame"` must survive `global = true`, at the root …
    assert!(Cli::try_parse(&["strix", "--width", "80"]).is_err());
    // … and under the subcommand.
    assert!(Cli::try_parse(&["strix", "diff", "main", "--width", "80"]).is_err());
}

#[test]
fn missing_range_errors() {
    assert!(Cli::try_parse(&["strix", "diff"]).is_err());
}

#[test]
fn diff_shadows_a_bare_path_positional() {
    // The compatibility break: `strix diff` is the subcommand, not a directory
    // named `diff`. It therefore requires a RANGE.
    assert!(Cli::try_parse(&["strix", "diff"]).is_err());
    // `strix ./diff` is the documented escape to open such a directory.
    let cli = parse(&["strix", "./diff"]);
    assert_eq!(cli.path, Some(PathBuf::from("./diff")));
    assert!(cli.command.is_none());
}

// --- Binary-level tests: real process, exit code + stderr ---

fn run_bin(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_strix"))
        .args(args)
        .arg(dir)
        .output()
        .expect("spawn strix")
}

#[test]
fn bad_range_exits_nonzero_naming_the_operand() {
    let repo = init_repo_with_diverged_branches();
    let out = run_bin(repo.path(), &["diff", "does-not-exist", "--dump-frame"]);
    assert!(!out.status.success(), "a bad range must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does-not-exist"),
        "stderr should name the offending operand: {stderr}"
    );
    assert!(
        stderr.contains("unknown revision"),
        "stderr should distinguish an unknown revision: {stderr}"
    );
}

#[test]
fn no_merge_base_exits_nonzero() {
    let repo = common::init_repo_with_orphan_branch();
    let out = run_bin(repo.path(), &["diff", "unrelated", "--dump-frame"]);
    assert!(!out.status.success(), "no merge base must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no merge base"),
        "stderr should explain the missing merge base: {stderr}"
    );
}

#[test]
fn good_range_dump_frame_succeeds() {
    let repo = init_repo_with_diverged_branches();
    let out = run_bin(repo.path(), &["diff", "main", "--dump-frame"]);
    assert!(out.status.success(), "a good range should render a frame");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("main…HEAD"),
        "the review header shows the range: {stdout}"
    );
    assert!(
        stdout.contains("feature.txt"),
        "the review list shows a changed file: {stdout}"
    );
}
