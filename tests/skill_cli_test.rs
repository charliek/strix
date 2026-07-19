//! Binary-level tests for `strix skill path` (plan §3.5, C7).
//!
//! Every test injects `STRIX_DATA_DIR` pointing at a fresh tempdir, so nothing
//! ever touches the real platform data directory. The materialized file is
//! asserted byte-identical to the in-repo `skills/strix-review/SKILL.md` read at
//! test time (the same source `include_str!` embeds into the binary).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use strix::cli::{Cli, Command as CliCommand, SkillAction};
use tempfile::tempdir;

/// The in-repo skill source — the single source of truth the binary embeds.
fn repo_skill() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/strix-review/SKILL.md")
}

/// Run `strix skill <args>` with `STRIX_DATA_DIR` set to `data_dir` and the
/// process cwd set to `cwd`. `HOME`/`XDG_STATE_HOME` are redirected into
/// `data_dir` too, so best-effort file logging lands in the test's tempdir
/// rather than touching the developer's real log directory.
fn run(data_dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_strix"))
        .arg("skill")
        .args(args)
        .env("STRIX_DATA_DIR", data_dir)
        .env("HOME", data_dir)
        .env("XDG_STATE_HOME", data_dir)
        .current_dir(cwd)
        .output()
        .expect("spawn strix")
}

fn expected_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join("strix")
        .join("skills")
        .join("strix-review")
        .join("SKILL.md")
}

#[test]
fn path_prints_an_existing_file() {
    let data = tempdir().unwrap();
    let out = run(data.path(), data.path(), &["path"]);
    assert!(
        out.status.success(),
        "expected success; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let printed = String::from_utf8(out.stdout).unwrap();
    let printed = printed.trim_end();
    let path = PathBuf::from(printed);
    assert!(path.is_absolute(), "printed path is absolute: {printed}");
    assert!(path.exists(), "printed path exists: {printed}");
    assert_eq!(path, expected_path(data.path()));
}

#[test]
fn materialized_bytes_match_the_in_repo_skill() {
    let data = tempdir().unwrap();
    let out = run(data.path(), data.path(), &["path"]);
    assert!(out.status.success());

    let materialized = std::fs::read(expected_path(data.path())).unwrap();
    let source = std::fs::read(repo_skill()).unwrap();
    assert_eq!(
        materialized, source,
        "materialized SKILL.md must be byte-identical to the in-repo source"
    );
}

#[test]
fn reinvocation_restores_a_mutated_copy() {
    let data = tempdir().unwrap();
    assert!(run(data.path(), data.path(), &["path"]).status.success());

    let target = expected_path(data.path());
    std::fs::write(&target, b"stale garbage").unwrap();

    assert!(run(data.path(), data.path(), &["path"]).status.success());
    let restored = std::fs::read(&target).unwrap();
    let source = std::fs::read(repo_skill()).unwrap();
    assert_eq!(restored, source, "re-invocation restores the file verbatim");
}

#[test]
fn works_outside_any_git_repo() {
    let data = tempdir().unwrap();
    // A cwd that is not (and is not inside) a git repository — the skill command
    // is repo-independent and must succeed anyway.
    let cwd = tempdir().unwrap();
    let out = run(data.path(), cwd.path(), &["path"]);
    assert!(
        out.status.success(),
        "skill path works outside a repo; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(expected_path(data.path()).exists());
}

#[test]
fn json_shape_parses_and_path_matches() {
    let data = tempdir().unwrap();
    let out = run(data.path(), data.path(), &["path", "--json"]);
    assert!(out.status.success());

    let value: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let obj = value.as_object().expect("envelope is a JSON object");
    assert_eq!(
        obj.len(),
        1,
        "envelope has exactly one key (`path`), got {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    let path = obj["path"].as_str().expect("path is a string");
    assert_eq!(PathBuf::from(path), expected_path(data.path()));
    assert!(Path::new(path).exists(), "the JSON path exists on disk");
}

// --- parse-level (mirrors tests/comment_cli_test.rs / cli_test.rs) ---

#[test]
fn parses_skill_path_subcommand() {
    let cli = Cli::try_parse(&["strix", "skill", "path"]).expect("parse skill path");
    match cli.command {
        Some(CliCommand::Skill {
            action: SkillAction::Path { json },
        }) => assert!(!json, "json defaults off"),
        other => panic!("expected Skill(Path), got {other:?}"),
    }
}

#[test]
fn parses_skill_path_json_flag() {
    let cli =
        Cli::try_parse(&["strix", "skill", "path", "--json"]).expect("parse skill path --json");
    match cli.command {
        Some(CliCommand::Skill {
            action: SkillAction::Path { json },
        }) => assert!(json, "--json sets the flag"),
        other => panic!("expected Skill(Path) with json, got {other:?}"),
    }
}
