//! Binary-level tests for `strix comment <list|add|rm|clear|gc>` (plan §3.3, C4).
//!
//! Each test drives the real binary against a temp repo fixture, invoking it with
//! the repo as the working directory (the natural agent form — `cd repo && strix
//! comment …`). Machine output is parsed with `serde_json` and asserted against
//! the §3.3 contract; failures are checked for non-zero exit + stderr wording.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::{git, init_empty_repo, init_repo, init_repo_with_history, write};
use serde_json::Value;
use strix::cli::{Cli, Command as CliCommand, CommentAction, ScopeArg};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_strix"))
        .arg("comment")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn strix")
}

/// Run a comment action expected to succeed and parse its `--json` stdout.
fn json_ok(dir: &Path, args: &[&str]) -> Value {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "expected success for {args:?}; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout should be valid JSON for {args:?}: {e}; stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// The full commit hex `rev` resolves to, via `git rev-parse`.
fn git_rev_parse(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rev])
        .output()
        .expect("spawn git rev-parse");
    assert!(out.status.success(), "git rev-parse {rev} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Write a comments store with a stored `active_range` for `main`, for tests
/// that need range-scoped comments without a live `strix diff` session.
fn seed_active_range(repo: &Path, range: &str) {
    let store_dir = repo.join(".git").join("strix");
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(
        store_dir.join("comments.json"),
        format!(
            r#"{{ "version": 2, "next_id": 1, "branches": {{ "main": {{ "active_range": "{range}", "comments": [] }} }} }}"#
        ),
    )
    .unwrap();
}

/// Assert a comment object carries every §3.3 key with the right JSON types,
/// including the C4-added flat `scope` tag (additive: every milestone-6 key
/// stays present and typed the same).
fn assert_comment_schema(c: &Value) {
    assert!(c["id"].is_u64(), "id is a number: {c}");
    assert!(c["source"].is_string(), "source is a string: {c}");
    assert!(c["file"].is_string(), "file is a string: {c}");
    assert!(c["side"].is_string(), "side is a string: {c}");
    assert!(c["line"].is_u64(), "line is a number: {c}");
    assert!(c["text"].is_string(), "text is a string: {c}");
    assert!(
        c["context"].is_string() || c["context"].is_null(),
        "context is string|null: {c}"
    );
    assert!(c["orphaned"].is_boolean(), "orphaned is a bool: {c}");
    assert!(c["created_at"].is_u64(), "created_at is a number: {c}");
    match c["scope"].as_str() {
        Some("worktree") => assert!(
            c["range"].is_null(),
            "worktree comments carry no range: {c}"
        ),
        Some("range") => assert!(
            c["range"].is_string(),
            "range comments carry their range: {c}"
        ),
        other => panic!("scope must be \"worktree\" or \"range\": {other:?} in {c}"),
    }
}

// --- parse-level (mirrors tests/cli_test.rs) ---

#[test]
fn comment_subcommand_tree_parses() {
    let cli = Cli::try_parse(&["strix", "comment", "list", "--json"]).expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::List { scope, json },
            path,
        }) => {
            assert!(json, "--json set");
            assert_eq!(
                scope, None,
                "no --scope: resolved later per the dirty state"
            );
            assert_eq!(path, None);
        }
        other => panic!("expected comment list, got {other:?}"),
    }

    let cli = Cli::try_parse(&[
        "strix",
        "comment",
        "add",
        "--file",
        "a.rs",
        "--new-line",
        "7",
        "--text",
        "hi",
    ])
    .expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action:
                CommentAction::Add {
                    file,
                    old_line,
                    new_line,
                    text,
                    scope,
                    range,
                    json,
                },
            ..
        }) => {
            assert_eq!(file, "a.rs");
            assert_eq!(old_line, None);
            assert_eq!(new_line, Some(7));
            assert_eq!(text, "hi");
            assert_eq!(scope, None, "no --scope: defaults to worktree at dispatch");
            assert_eq!(range, None);
            assert!(!json);
        }
        other => panic!("expected comment add, got {other:?}"),
    }

    // A repo path is a leading positional at the `comment` level (before the
    // action), so a directory named `list` is still reachable that way.
    let cli = Cli::try_parse(&["strix", "comment", "some/repo", "gc"]).expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::Gc { .. },
            path,
        }) => assert_eq!(path.as_deref(), Some(Path::new("some/repo"))),
        other => panic!("expected comment gc with path, got {other:?}"),
    }

    // The action is required.
    assert!(Cli::try_parse(&["strix", "comment"]).is_err());
}

#[test]
fn scope_and_range_flags_parse() {
    let cli = Cli::try_parse(&["strix", "comment", "list", "--scope", "worktree", "--json"])
        .expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::List { scope, json },
            ..
        }) => {
            assert_eq!(scope, Some(ScopeArg::Worktree));
            assert!(json);
        }
        other => panic!("expected comment list, got {other:?}"),
    }

    let cli = Cli::try_parse(&[
        "strix",
        "comment",
        "add",
        "--file",
        "a.rs",
        "--new-line",
        "1",
        "--text",
        "x",
        "--scope",
        "range",
        "--range",
        "main",
    ])
    .expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::Add { scope, range, .. },
            ..
        }) => {
            assert_eq!(scope, Some(ScopeArg::Range));
            assert_eq!(range.as_deref(), Some("main"));
        }
        other => panic!("expected comment add, got {other:?}"),
    }

    let cli = Cli::try_parse(&["strix", "comment", "clear", "--all"]).expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::Clear { scope, all, .. },
            ..
        }) => {
            assert_eq!(scope, None);
            assert!(all);
        }
        other => panic!("expected comment clear, got {other:?}"),
    }

    // clap accepts `--scope all` for `add` at parse time (it's a value shared
    // with list/clear); the CLI rejects it at dispatch instead (see
    // `add_scope_all_is_rejected` below).
    let cli = Cli::try_parse(&[
        "strix",
        "comment",
        "add",
        "--file",
        "a.rs",
        "--new-line",
        "1",
        "--text",
        "x",
        "--scope",
        "all",
    ])
    .expect("parses; rejected at dispatch, not parse time");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::Add { scope, .. },
            ..
        }) => assert_eq!(scope, Some(ScopeArg::All)),
        other => panic!("expected comment add, got {other:?}"),
    }
}

#[test]
fn add_has_no_human_source_flag() {
    // The CLI can only ever author agent notes — there is no flag to set human.
    let err = Cli::try_parse(&[
        "strix",
        "comment",
        "add",
        "--file",
        "a.rs",
        "--new-line",
        "1",
        "--text",
        "x",
        "--source",
        "human",
    ]);
    assert!(err.is_err(), "no --source flag exists");
}

// --- binary lifecycle ---

#[test]
fn never_reviewed_branch_lists_empty_range_null() {
    let repo = init_repo();
    let value = json_ok(repo.path(), &["list", "--json"]);
    assert_eq!(value["branch"], "main");
    assert_eq!(
        value["range"],
        Value::Null,
        "no review recorded → range null"
    );
    assert_eq!(
        value["comments"],
        Value::Array(vec![]),
        "no comments on a never-reviewed branch"
    );

    // Exit 0 even with an empty set.
    let out = run(repo.path(), &["list"]);
    assert!(out.status.success());
}

#[test]
fn add_list_rm_list_lifecycle_matches_schema() {
    let repo = init_repo();
    // Dirty the tracked file so the default (worktree) scope has a real anchor.
    write(repo.path(), "README.md", "# test\nedited\n");

    // add → { "comment": {…} }, always agent-source, worktree scope by default.
    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "2",
            "--text",
            "fix this",
            "--json",
        ],
    );
    let comment = &added["comment"];
    assert_comment_schema(comment);
    assert_eq!(comment["source"], "agent", "CLI always authors agent notes");
    assert_eq!(
        comment["scope"], "worktree",
        "add defaults to worktree scope"
    );
    assert_eq!(comment["file"], "README.md");
    assert_eq!(comment["side"], "new");
    assert_eq!(comment["line"], 2);
    assert_eq!(comment["text"], "fix this");
    assert_eq!(
        comment["orphaned"], false,
        "the anchor is present in the worktree diff"
    );
    assert_eq!(comment["context"], "edited");
    assert!(
        comment["base"].is_string(),
        "a worktree comment stamps the current HEAD as its baseline"
    );
    let id = comment["id"].as_u64().unwrap();

    // list → the one comment, same schema.
    let listed = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    let comments = listed["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["id"].as_u64().unwrap(), id);
    assert_comment_schema(&comments[0]);

    // rm → { "removed": {…}, "remaining": N }.
    let removed = json_ok(repo.path(), &["rm", &id.to_string(), "--json"]);
    assert_eq!(removed["removed"]["id"].as_u64().unwrap(), id);
    assert_eq!(removed["remaining"], 0);

    // list → empty again.
    let listed = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    assert_eq!(listed["comments"], Value::Array(vec![]));
}

#[test]
fn list_defaults_to_worktree_when_dirty_and_shows_flat_scope() {
    let repo = init_repo();
    write(repo.path(), "README.md", "# test\nedited\n");
    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "2",
            "--text",
            "note",
            "--json",
        ],
    );
    assert_eq!(added["comment"]["scope"], "worktree");

    // No --scope: the repo is dirty, so the default is worktree.
    let listed = json_ok(repo.path(), &["list", "--json"]);
    let comments = listed["comments"].as_array().unwrap();
    assert_eq!(
        comments.len(),
        1,
        "default list scope must be worktree here"
    );
    assert_eq!(comments[0]["scope"], "worktree");

    // Explicit --scope worktree agrees with the default.
    let explicit = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    assert_eq!(explicit["comments"], listed["comments"]);
}

#[test]
fn list_scope_range_and_all_partition_by_scope() {
    let repo = init_repo_with_history();
    seed_active_range(repo.path(), "HEAD~2");

    // A range comment, anchored against the stored active range.
    let range_added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "2",
            "--text",
            "range note",
            "--scope",
            "range",
            "--json",
        ],
    );
    assert_eq!(range_added["comment"]["scope"], "range");
    assert_eq!(range_added["comment"]["range"], "HEAD~2");

    // Dirty an unrelated tracked file and add a worktree comment too.
    write(repo.path(), "a.txt", "alpha\nbeta\n");
    let wt_added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "a.txt",
            "--new-line",
            "2",
            "--text",
            "wt note",
            "--json",
        ],
    );
    assert_eq!(wt_added["comment"]["scope"], "worktree");

    let range_only = json_ok(repo.path(), &["list", "--scope", "range", "--json"]);
    let comments = range_only["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1, "range scope excludes the worktree note");
    assert_eq!(comments[0]["scope"], "range");

    let worktree_only = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    let comments = worktree_only["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1, "worktree scope excludes the range note");
    assert_eq!(comments[0]["scope"], "worktree");

    let all = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    assert_eq!(
        all["comments"].as_array().unwrap().len(),
        2,
        "all shows both"
    );
}

#[test]
fn worktree_add_and_list_key_the_inbox_by_status_branch_key_on_detached_head() {
    // codex-C4 #2: `list` and worktree `add` must derive the inbox key from
    // the *same* `Status` snapshot used for the dirty-check/sweep, not a
    // separate `head_branch_key()` read — two independent git reads could
    // momentarily disagree under an external checkout race, keying the inbox
    // to one branch while sweeping with another branch's facts (comment-loss).
    //
    // Detached HEAD is the trickiest case for that derivation: the key falls
    // back to the full commit hex instead of a branch name. This pins that
    // `list`/worktree `add` still land on exactly that hex — i.e. the
    // status-derived key agrees with `Repo::head_branch_key()`'s documented
    // semantics — after routing through `Status::branch_key()`. The race
    // itself needs an external checkout mid-call and isn't deterministically
    // reproducible at the binary level, so it isn't asserted here.
    let repo = init_repo_with_history();
    let head = git_rev_parse(repo.path(), "HEAD");
    git(repo.path(), &["checkout", "-q", "--detach", &head]);
    write(repo.path(), "README.md", "# test\nsecond line\nedited\n");

    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "3",
            "--text",
            "note",
            "--json",
        ],
    );
    assert_eq!(added["comment"]["scope"], "worktree");

    let listed = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    assert_eq!(
        listed["branch"], head,
        "detached HEAD keys the inbox by the full commit hex"
    );
    assert_eq!(listed["comments"].as_array().unwrap().len(), 1);
}

#[test]
fn add_rejects_zero_and_ambiguous_line_flags() {
    let repo = init_repo();
    let base = ["add", "--file", "README.md", "--text", "x"];

    // --old-line 0 / --new-line 0 rejected (1-based).
    for flag in ["--old-line", "--new-line"] {
        let mut args = base.to_vec();
        args.extend([flag, "0"]);
        let out = run(repo.path(), &args);
        assert!(!out.status.success(), "{flag} 0 must be rejected");
    }

    // Both flags at once rejected.
    let out = run(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--old-line",
            "1",
            "--new-line",
            "1",
            "--text",
            "x",
        ],
    );
    assert!(!out.status.success(), "both line flags must be rejected");

    // Neither flag rejected.
    let out = run(repo.path(), &["add", "--file", "README.md", "--text", "x"]);
    assert!(
        !out.status.success(),
        "a missing line flag must be rejected"
    );
}

#[test]
fn add_rejects_whitespace_only_text() {
    let repo = init_repo();
    let out = run(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "   \t  ",
        ],
    );
    assert!(
        !out.status.success(),
        "whitespace-only --text must be rejected"
    );
}

#[test]
fn add_scope_all_is_rejected() {
    let repo = init_repo();
    let out = run(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "x",
            "--scope",
            "all",
        ],
    );
    assert!(
        !out.status.success(),
        "--scope all must be rejected for add"
    );
}

#[test]
fn add_stores_multiline_text_raw() {
    let repo = init_repo();
    let body = "line one\nline two\n  indented three";
    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            body,
            "--json",
        ],
    );
    assert_eq!(added["comment"]["text"], body, "newlines stored verbatim");

    // And it round-trips through list unchanged (explicit scope: the repo is
    // clean, so the default list scope would be `range`, not the worktree
    // comment just added).
    let listed = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    assert_eq!(listed["comments"][0]["text"], body);
}

#[test]
fn add_scope_range_without_range_or_active_range_errors() {
    // Neither --range nor a stored active_range: range scope has nothing to
    // anchor against, so `add` must error rather than persist the milestone-6
    // empty-range placeholder (codex-C2a finding #8).
    let repo = init_repo();
    let out = run(
        repo.path(),
        &[
            "add",
            "--file",
            "whatever.rs",
            "--new-line",
            "42",
            "--text",
            "x",
            "--scope",
            "range",
        ],
    );
    assert!(
        !out.status.success(),
        "range scope with no range source must error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("range"),
        "stderr should mention the missing range: {stderr}"
    );

    // Nothing was written: a failed add must not even create the store.
    let store_path = repo.path().join(".git").join("strix").join("comments.json");
    assert!(
        !store_path.exists(),
        "a failed add must not create the store"
    );
}

#[test]
fn rm_unknown_id_exits_nonzero_naming_the_branch() {
    let repo = init_repo();
    let out = run(repo.path(), &["rm", "999"]);
    assert!(!out.status.success(), "unknown id must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("main"),
        "stderr should name the branch key: {stderr}"
    );
    assert!(
        stderr.contains("999"),
        "stderr should name the missing id: {stderr}"
    );
}

#[test]
fn clear_reports_the_count() {
    let repo = init_repo();
    for _ in 0..3 {
        json_ok(
            repo.path(),
            &[
                "add",
                "--file",
                "README.md",
                "--new-line",
                "1",
                "--text",
                "x",
                "--json",
            ],
        );
    }
    let cleared = json_ok(repo.path(), &["clear", "--all", "--json"]);
    assert_eq!(cleared["cleared"], 3);

    let listed = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    assert_eq!(listed["comments"], Value::Array(vec![]));
}

#[test]
fn clear_without_scope_or_all_errors() {
    let repo = init_repo();
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "x",
            "--json",
        ],
    );

    let out = run(repo.path(), &["clear"]);
    assert!(!out.status.success(), "clear with no scope must error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("scope"),
        "stderr should mention the missing scope: {stderr}"
    );

    // The comment must still be there — clear never wipes implicitly.
    let listed = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    assert_eq!(listed["comments"].as_array().unwrap().len(), 1);
}

#[test]
fn clear_scope_and_all_flag_together_errors() {
    let repo = init_repo();
    let out = run(repo.path(), &["clear", "--scope", "worktree", "--all"]);
    assert!(
        !out.status.success(),
        "combining --scope and --all must be rejected"
    );
}

#[test]
fn clear_scope_worktree_and_all_clear_the_right_set() {
    let repo = init_repo_with_history();
    seed_active_range(repo.path(), "HEAD~2");
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "2",
            "--text",
            "range note",
            "--scope",
            "range",
            "--json",
        ],
    );
    write(repo.path(), "a.txt", "alpha\nbeta\n");
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "a.txt",
            "--new-line",
            "2",
            "--text",
            "wt note",
            "--json",
        ],
    );

    // Clearing only the worktree scope leaves the range comment untouched.
    let cleared = json_ok(repo.path(), &["clear", "--scope", "worktree", "--json"]);
    assert_eq!(cleared["cleared"], 1);
    let remaining = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    let comments = remaining["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["scope"], "range");

    // --all clears whatever is left.
    let cleared_all = json_ok(repo.path(), &["clear", "--all", "--json"]);
    assert_eq!(cleared_all["cleared"], 1);
    let remaining = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    assert_eq!(remaining["comments"], Value::Array(vec![]));
}

#[test]
fn headless_list_sweeps_a_worktree_comment_after_its_commit_lands() {
    let repo = init_repo();
    write(repo.path(), "README.md", "# test\nedited\n");
    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "2",
            "--text",
            "note",
            "--json",
        ],
    );
    let id = added["comment"]["id"].as_u64().unwrap();
    assert_eq!(added["comment"]["orphaned"], false);

    // Commit exactly the change the comment anchors to.
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-q", "-m", "land it"]);

    // A headless `list` (no live TUI session ever ran) must still reflect the
    // sweep the plan §3.2 lifecycle prescribes once HEAD moves past the note's
    // baseline and the change resolves there.
    let listed = json_ok(repo.path(), &["list", "--scope", "worktree", "--json"]);
    let comments = listed["comments"].as_array().unwrap();
    assert!(
        comments.iter().all(|c| c["id"].as_u64() != Some(id)),
        "the committed comment should have swept: {comments:?}"
    );
}

#[test]
fn gc_on_a_clean_repo_removes_nothing() {
    let repo = init_repo();
    let result = json_ok(repo.path(), &["gc", "--json"]);
    assert_eq!(result["removed_branches"], Value::Array(vec![]));
    assert_eq!(result["removed_comments"], 0);
    // A no-op gc must not create the store: a fresh write here would also wake
    // any watcher on the store dir for nothing.
    let store_path = repo.path().join(".git").join("strix").join("comments.json");
    assert!(
        !store_path.exists(),
        "a no-op gc must not create comments.json"
    );
}

#[test]
fn noop_gc_leaves_an_existing_store_byte_identical() {
    let repo = init_repo();
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "keep",
            "--json",
        ],
    );
    let store_path = repo.path().join(".git").join("strix").join("comments.json");
    let before = std::fs::read(&store_path).unwrap();

    let result = json_ok(repo.path(), &["gc", "--json"]);
    assert_eq!(result["removed_branches"], Value::Array(vec![]));
    assert_eq!(result["removed_comments"], 0);
    assert_eq!(
        std::fs::read(&store_path).unwrap(),
        before,
        "a gc that removed nothing must not rewrite the store"
    );
}

#[test]
fn gc_drops_a_deleted_branch_set() {
    let repo = init_repo();
    // Author a comment while checked out on a scratch branch, so it is filed
    // under that branch key.
    git(repo.path(), &["checkout", "-q", "-b", "scratch"]);
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "x",
            "--json",
        ],
    );
    // Leave the branch and delete it: its inbox is now stale.
    git(repo.path(), &["checkout", "-q", "main"]);
    git(repo.path(), &["branch", "-D", "scratch"]);

    let result = json_ok(repo.path(), &["gc", "--json"]);
    assert_eq!(
        result["removed_branches"],
        Value::Array(vec![Value::String("scratch".into())])
    );
    assert_eq!(result["removed_comments"], 1);
}

#[test]
fn rm_unknown_id_leaves_the_store_byte_identical() {
    // An unknown id must be detected without writing: under an unwritable store
    // the user should see "not found", not a write error — and the file the agent
    // reads must be untouched (plan §3.3).
    let repo = init_repo();
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "real",
            "--json",
        ],
    );
    let store_path = repo.path().join(".git").join("strix").join("comments.json");
    let before = std::fs::read(&store_path).unwrap();

    let out = run(repo.path(), &["rm", "999"]);
    assert!(!out.status.success(), "unknown id must exit non-zero");
    assert_eq!(
        std::fs::read(&store_path).unwrap(),
        before,
        "rm of an unknown id must not write the store"
    );
}

#[test]
fn gc_keeps_the_unborn_head_inbox() {
    // On an unborn HEAD the checked-out branch ("main") has no ref, so it is
    // absent from `branch_names()`; GC must still keep its inbox (plan §3.1).
    //
    // The file must actually exist on disk: a worktree comment anchored to a
    // path absent from *both* HEAD and the worktree reads as "vanished" under
    // an unchanged baseline (both `None` on an unborn HEAD) and the very next
    // sweep pass (this test's own `gc`/`list`) would legitimately retire it —
    // a real anchor, not GC, is what this test means to exercise.
    let repo = init_empty_repo();
    write(repo.path(), "README.md", "# test\n");
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "keep me",
            "--json",
        ],
    );

    let result = json_ok(repo.path(), &["gc", "--json"]);
    assert_eq!(
        result["removed_branches"],
        Value::Array(vec![]),
        "the unborn current branch must not be swept"
    );
    assert_eq!(result["removed_comments"], 0);

    // `--scope all`: an unborn, file-less repo is clean, so the plain default
    // would resolve to `range` and miss the worktree-scoped comment added above.
    let listed = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    assert_eq!(
        listed["comments"].as_array().unwrap().len(),
        1,
        "the comment survives gc on an unborn HEAD"
    );
}

#[test]
fn startup_gc_keeps_the_unborn_head_inbox() {
    // The startup GC (`App::build`) runs on every launch; on an unborn HEAD it
    // must not drop the current session's own comments (plan §3.1). The file
    // must exist on disk — see `gc_keeps_the_unborn_head_inbox` for why.
    let repo = init_empty_repo();
    write(repo.path(), "README.md", "# test\n");
    json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "keep me",
            "--json",
        ],
    );

    // Launch strix headlessly (renders one frame, running startup GC first).
    let out = Command::new(env!("CARGO_BIN_EXE_strix"))
        .arg("--dump-frame")
        .current_dir(repo.path())
        .output()
        .expect("spawn strix");
    assert!(
        out.status.success(),
        "--dump-frame failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = json_ok(repo.path(), &["list", "--scope", "all", "--json"]);
    assert_eq!(
        listed["comments"].as_array().unwrap().len(),
        1,
        "startup GC must not drop the unborn HEAD's inbox"
    );
}

#[test]
fn add_orphans_when_line_absent_on_a_resolvable_range() {
    // With a stored range that resolves, an anchor whose file/line isn't in the
    // diff is orphaned honestly (context null); an anchor that is present captures
    // context and is not orphaned (plan §3.2/§3.3).
    let repo = init_repo_with_history();
    // `HEAD~2` resolves (base = init, head = HEAD), so the pass has a diff to
    // anchor against; `README.md` is edited in it, `nope.rs` is absent.
    seed_active_range(repo.path(), "HEAD~2");

    let orphan = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "nope.rs",
            "--new-line",
            "999",
            "--text",
            "x",
            "--scope",
            "range",
            "--json",
        ],
    );
    assert_eq!(orphan["comment"]["scope"], "range");
    assert_eq!(
        orphan["comment"]["orphaned"], true,
        "an absent anchor on a resolvable range orphans"
    );
    assert_eq!(
        orphan["comment"]["context"],
        Value::Null,
        "an orphaned anchor has no context"
    );

    let anchored = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "2",
            "--text",
            "y",
            "--scope",
            "range",
            "--json",
        ],
    );
    assert_eq!(
        anchored["comment"]["orphaned"], false,
        "a present anchor is not orphaned"
    );
    assert!(
        anchored["comment"]["context"].is_string(),
        "a present anchor captures its line's context"
    );
}

#[test]
fn corrupt_store_fails_every_action_and_leaves_the_file_untouched() {
    let repo = init_repo();
    let store_dir = repo.path().join(".git").join("strix");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store_path = store_dir.join("comments.json");
    let garbage = b"{ not valid json ]".to_vec();
    std::fs::write(&store_path, &garbage).unwrap();

    let actions: &[&[&str]] = &[
        &["list", "--json"],
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "x",
        ],
        &["rm", "1"],
        &["clear", "--all"],
        &["gc"],
    ];
    for action in actions {
        let out = run(repo.path(), action);
        assert!(
            !out.status.success(),
            "{action:?} must exit non-zero on a corrupt store"
        );
        assert_eq!(
            std::fs::read(&store_path).unwrap(),
            garbage,
            "{action:?} must leave the corrupt store byte-identical"
        );
    }
}
