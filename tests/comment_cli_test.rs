//! Binary-level tests for `strix comment <list|add|rm|clear|gc>` (plan §3.3, C2).
//!
//! Each test drives the real binary against a temp repo fixture, invoking it with
//! the repo as the working directory (the natural agent form — `cd repo && strix
//! comment …`). Machine output is parsed with `serde_json` and asserted against
//! the §3.3 contract; failures are checked for non-zero exit + stderr wording.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::{git, init_empty_repo, init_repo, init_repo_with_history};
use serde_json::Value;
use strix::cli::{Cli, Command as CliCommand, CommentAction};

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

/// Assert a comment object carries every §3.3 key with the right JSON types.
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
}

// --- parse-level (mirrors tests/cli_test.rs) ---

#[test]
fn comment_subcommand_tree_parses() {
    let cli = Cli::try_parse(&["strix", "comment", "list", "--json"]).expect("parse");
    match cli.command {
        Some(CliCommand::Comment {
            action: CommentAction::List { json },
            path,
        }) => {
            assert!(json, "--json set");
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
                    json,
                },
            ..
        }) => {
            assert_eq!(file, "a.rs");
            assert_eq!(old_line, None);
            assert_eq!(new_line, Some(7));
            assert_eq!(text, "hi");
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

    // add → { "comment": {…} }, always agent-source.
    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "README.md",
            "--new-line",
            "1",
            "--text",
            "fix this",
            "--json",
        ],
    );
    let comment = &added["comment"];
    assert_comment_schema(comment);
    assert_eq!(comment["source"], "agent", "CLI always authors agent notes");
    assert_eq!(comment["file"], "README.md");
    assert_eq!(comment["side"], "new");
    assert_eq!(comment["line"], 1);
    assert_eq!(comment["text"], "fix this");
    assert_eq!(comment["orphaned"], false);
    assert_eq!(
        comment["context"],
        Value::Null,
        "no stored range → context null"
    );
    let id = comment["id"].as_u64().unwrap();

    // list → the one comment, same schema.
    let listed = json_ok(repo.path(), &["list", "--json"]);
    let comments = listed["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["id"].as_u64().unwrap(), id);
    assert_comment_schema(&comments[0]);

    // rm → { "removed": {…}, "remaining": N }.
    let removed = json_ok(repo.path(), &["rm", &id.to_string(), "--json"]);
    assert_eq!(removed["removed"]["id"].as_u64().unwrap(), id);
    assert_eq!(removed["remaining"], 0);

    // list → empty again.
    let listed = json_ok(repo.path(), &["list", "--json"]);
    assert_eq!(listed["comments"], Value::Array(vec![]));
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

    // And it round-trips through list unchanged.
    let listed = json_ok(repo.path(), &["list", "--json"]);
    assert_eq!(listed["comments"][0]["text"], body);
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
    let cleared = json_ok(repo.path(), &["clear", "--json"]);
    assert_eq!(cleared["cleared"], 3);

    let listed = json_ok(repo.path(), &["list", "--json"]);
    assert_eq!(listed["comments"], Value::Array(vec![]));
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
    let repo = init_empty_repo();
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

    let listed = json_ok(repo.path(), &["list", "--json"]);
    assert_eq!(
        listed["comments"].as_array().unwrap().len(),
        1,
        "the comment survives gc on an unborn HEAD"
    );
}

#[test]
fn startup_gc_keeps_the_unborn_head_inbox() {
    // The startup GC (`App::build`) runs on every launch; on an unborn HEAD it
    // must not drop the current session's own comments (plan §3.1).
    let repo = init_empty_repo();
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

    let listed = json_ok(repo.path(), &["list", "--json"]);
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
    let store_dir = repo.path().join(".git").join("strix");
    std::fs::create_dir_all(&store_dir).unwrap();
    // `HEAD~2` resolves (base = init, head = HEAD), so the pass has a diff to
    // anchor against; `README.md` is edited in it, `nope.rs` is absent.
    std::fs::write(
        store_dir.join("comments.json"),
        r#"{ "version": 1, "next_id": 1, "branches": { "main": { "range": "HEAD~2", "comments": [] } } }"#,
    )
    .unwrap();

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
            "--json",
        ],
    );
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
fn add_does_not_orphan_when_no_range_recorded() {
    // No stored range → the anchor is unknown, not orphaned (plan §3.2/§3.3).
    let repo = init_repo();
    let added = json_ok(
        repo.path(),
        &[
            "add",
            "--file",
            "whatever.rs",
            "--new-line",
            "42",
            "--text",
            "x",
            "--json",
        ],
    );
    assert_eq!(
        added["comment"]["orphaned"], false,
        "unknown (no range) is not orphaned"
    );
    assert_eq!(added["comment"]["context"], Value::Null);
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
        &["clear"],
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
