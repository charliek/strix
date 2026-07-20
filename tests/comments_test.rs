//! Comments store I/O, GC, and the re-anchor engine (plan §3.1 / §3.2, C1).
//! Every test drives the store against an injected tempdir — never a real repo
//! store — and the re-anchor matrix is exercised against hand-constructed
//! `FileDiff` values, which are clearer than seeding a diff per case.

mod common;

use std::collections::{BTreeMap, HashSet};

use common::{init_empty_repo, init_repo, init_repo_with_worktree};
use strix::comments::{self, Branch, Comment, FileFacts, Scope, Side, Source, Store};
use strix::git::{ChangeKind, CommitFile, CommitStat, DiffLine, FileDiff, LineKind, Repo};

// --- builders ---

// The store/re-anchor/gc suites here are scope-agnostic (the engine ignores
// scope), so the default builder scope is a don't-care worktree; the scope-,
// contract-, and version-specific cases below construct their scopes explicitly.
fn comment(id: u64, file: &str, side: Side, line: usize, context: Option<&str>) -> Comment {
    Comment {
        scope: Scope::WorkTree,
        id,
        source: Source::Human,
        file: file.to_string(),
        side,
        line,
        text: format!("note {id}"),
        context: context.map(str::to_string),
        orphaned: false,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    }
}

fn modified(path: &str) -> CommitFile {
    CommitFile {
        path: path.to_string(),
        orig_path: None,
        change: ChangeKind::Modified,
        stat: CommitStat::default(),
    }
}

fn renamed(from: &str, to: &str) -> CommitFile {
    CommitFile {
        path: to.to_string(),
        orig_path: Some(from.to_string()),
        change: ChangeKind::Renamed,
        stat: CommitStat::default(),
    }
}

/// A New-side context line at `n` with `text` (both sides numbered `n`).
fn line(n: usize, text: &str) -> DiffLine {
    DiffLine {
        kind: LineKind::Context,
        old_no: Some(n),
        new_no: Some(n),
        text: text.to_string(),
    }
}

/// Re-anchor a single file's comments against one hand-built diff.
fn reanchor_one_file(comments: &mut [Comment], file: CommitFile, diff: FileDiff) -> bool {
    comments::reanchor(comments, std::slice::from_ref(&file), |_| diff.clone())
}

// --- store round-trip & load rules ---

#[test]
fn multi_branch_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    let mut store = Store::default();
    let mut main = Branch {
        active_range: Some("origin/main".to_string()),
        comments: vec![comment(
            store.take_id(),
            "a.rs",
            Side::New,
            3,
            Some("fn a() {"),
        )],
    };
    main.comments
        .push(comment(store.take_id(), "b.rs", Side::Old, 7, None));
    store.branches.insert("main".to_string(), main);
    let feature_id = store.take_id();
    store.branches.insert(
        "feature".to_string(),
        Branch {
            active_range: None,
            comments: vec![comment(feature_id, "c.rs", Side::New, 1, Some("x"))],
        },
    );

    comments::mutate(dir.path(), |s| *s = store.clone()).unwrap();
    let loaded = comments::load(dir.path()).unwrap();

    assert_eq!(loaded, store);
    assert_eq!(loaded.version, 2);
    assert_eq!(loaded.next_id, 4);
    assert_eq!(loaded.branches.len(), 2);
}

#[test]
fn take_id_skips_past_existing_max_id() {
    // A hand-edited store can carry a stale next_id at or below an existing id;
    // take_id must still mint a fresh unique id (max(next_id, max_id + 1)).
    let mut store = Store {
        version: 2,
        next_id: 2,
        branches: BTreeMap::new(),
    };
    store.branches.insert(
        "main".to_string(),
        Branch {
            active_range: None,
            comments: vec![
                comment(1, "a.rs", Side::New, 1, Some("x")),
                comment(2, "a.rs", Side::New, 2, Some("y")),
            ],
        },
    );

    assert_eq!(store.take_id(), 3, "minted past the existing max id, not 2");
    assert_eq!(store.next_id, 4, "counter advanced past the minted id");
}

#[test]
fn source_and_side_serialize_as_lowercase_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    let mut c = comment(store.take_id(), "a.rs", Side::Old, 2, Some(""));
    c.source = Source::Agent;
    store.branches.insert(
        "main".to_string(),
        Branch {
            active_range: None,
            comments: vec![c],
        },
    );
    comments::mutate(dir.path(), |s| *s = store).unwrap();

    let text = std::fs::read_to_string(dir.path().join("comments.json")).unwrap();
    assert!(text.contains("\"source\": \"agent\""), "{text}");
    assert!(text.contains("\"side\": \"old\""), "{text}");
    // A blank-line context is a valid Some(""), not null.
    assert!(text.contains("\"context\": \"\""), "{text}");
}

/// The pinned, flat, additive scope contract (plan §3.3): a worktree comment
/// carries `"scope":"worktree"` and no `range` key (but does carry its `base`
/// baseline); a range comment carries `"scope":"range","range":"…"` and omits
/// `base`. Both must round-trip. Asserted byte-for-byte on compact JSON so a
/// silent shape change (nesting, a renamed/removed field, a reordered `scope`)
/// fails loudly.
#[test]
fn scope_serializes_as_the_flat_additive_contract() {
    let worktree = Comment {
        scope: Scope::WorkTree,
        id: 7,
        source: Source::Human,
        file: "src/app.rs".to_string(),
        side: Side::New,
        line: 42,
        text: "needs a guard".to_string(),
        context: Some("fn run() {".to_string()),
        orphaned: false,
        created_at: 1_700_000_000,
        base: Some("a".repeat(40)),
        stale: false,
    };
    assert_eq!(
        serde_json::to_string(&worktree).unwrap(),
        "{\"scope\":\"worktree\",\"id\":7,\"source\":\"human\",\"file\":\"src/app.rs\",\
         \"side\":\"new\",\"line\":42,\"text\":\"needs a guard\",\"context\":\"fn run() {\",\
         \"orphaned\":false,\"created_at\":1700000000,\
         \"base\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"stale\":false}"
    );

    let range = Comment {
        scope: Scope::Range {
            range: "origin/main".to_string(),
        },
        id: 8,
        source: Source::Agent,
        file: "src/lib.rs".to_string(),
        side: Side::Old,
        line: 3,
        text: "why?".to_string(),
        context: None,
        orphaned: true,
        created_at: 1_700_000_000,
        base: None,
        stale: false,
    };
    assert_eq!(
        serde_json::to_string(&range).unwrap(),
        "{\"scope\":\"range\",\"range\":\"origin/main\",\"id\":8,\"source\":\"agent\",\
         \"file\":\"src/lib.rs\",\"side\":\"old\",\"line\":3,\"text\":\"why?\",\
         \"context\":null,\"orphaned\":true,\"created_at\":1700000000,\"stale\":false}"
    );

    for c in [worktree, range] {
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Comment>(&json).unwrap(), c);
    }
}

#[test]
fn invalid_json_is_never_clobbered_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let broken = b"{ this is not valid json ]".to_vec();
    std::fs::write(dir.path().join("comments.json"), &broken).unwrap();

    assert!(
        comments::load(dir.path()).is_err(),
        "load must report the parse error"
    );
    let mutate = comments::mutate(dir.path(), |s| s.take_id());
    assert!(
        mutate.is_err(),
        "mutate must refuse to write over an unparseable store"
    );

    assert_eq!(
        std::fs::read(dir.path().join("comments.json")).unwrap(),
        broken,
        "the broken file must stay byte-identical"
    );
    assert!(tmp_residue(dir.path()).is_empty(), "no temp residue");
}

#[test]
fn version_three_refuses_read_and_write() {
    let dir = tempfile::tempdir().unwrap();
    let future = br#"{ "version": 3, "next_id": 1, "branches": {} }"#.to_vec();
    std::fs::write(dir.path().join("comments.json"), &future).unwrap();

    let err = comments::load(dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("version 3"),
        "message should name the version: {err}"
    );
    assert!(
        comments::mutate(dir.path(), |s| s.branches.clear()).is_err(),
        "a newer-version store must not be written"
    );
    assert_eq!(
        std::fs::read(dir.path().join("comments.json")).unwrap(),
        future,
        "the newer-version file must stay byte-identical"
    );
}

/// A version-1 (milestone-6) store: `load` backs it up once to
/// `comments.json.v1.bak` and returns an empty v2 store — not an error, and the
/// old comments are intentionally dropped (plan §3.0). `comments.json` itself is
/// left untouched (a later write is what re-stamps it to v2).
#[test]
fn version_one_is_backed_up_and_reset_to_empty() {
    let dir = tempfile::tempdir().unwrap();
    // A *real* milestone-6 comment: no `scope`/`base`/`stale` fields. A full v2
    // parse would reject it, so `load` must route on `version` alone — this is the
    // case an empty-comments fixture would silently miss.
    let v1 = br#"{ "version": 1, "next_id": 5, "branches": { "main": { "range": "origin/main", "comments": [ { "id": 4, "source": "human", "file": "a.rs", "side": "new", "line": 1, "text": "hi", "context": "x", "orphaned": false, "created_at": 100 } ] } } }"#.to_vec();
    std::fs::write(dir.path().join("comments.json"), &v1).unwrap();

    let loaded = comments::load(dir.path()).unwrap();
    assert_eq!(loaded.version, 2);
    assert!(
        loaded.branches.is_empty(),
        "v1 comments are intentionally dropped"
    );
    assert_eq!(
        loaded.next_id, 5,
        "the v1 id counter carries forward so a new id can't reuse a backup id"
    );

    assert_eq!(
        std::fs::read(dir.path().join("comments.json.v1.bak")).unwrap(),
        v1,
        "the backup holds the original v1 bytes verbatim"
    );
    assert_eq!(
        std::fs::read(dir.path().join("comments.json")).unwrap(),
        v1,
        "load must not rewrite the original file — only back it up"
    );
    assert!(tmp_residue(dir.path()).is_empty(), "no temp residue");
}

/// A *differing* pre-existing backup is never destroyed: `load` keeps it and
/// writes the current v1 bytes to a numbered sibling instead.
#[test]
fn version_one_backup_never_clobbers_a_differing_backup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("comments.json.v1.bak"), b"OTHER V1 BACKUP").unwrap();
    let v1 = br#"{ "version": 1, "next_id": 1, "branches": {} }"#.to_vec();
    std::fs::write(dir.path().join("comments.json"), &v1).unwrap();

    let loaded = comments::load(dir.path()).unwrap();
    assert_eq!(loaded.version, 2);
    assert_eq!(
        std::fs::read(dir.path().join("comments.json.v1.bak")).unwrap(),
        b"OTHER V1 BACKUP",
        "the pre-existing backup is preserved untouched"
    );
    assert_eq!(
        std::fs::read(dir.path().join("comments.json.v1.bak.1")).unwrap(),
        v1,
        "the current v1 bytes go to the first free numbered sibling"
    );
}

/// Re-running `load` on the same v1 file (an identical backup already present) is
/// an idempotent no-op — no duplicate backup, no error.
#[test]
fn version_one_backup_is_idempotent_when_identical() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = br#"{ "version": 1, "next_id": 3, "branches": {} }"#.to_vec();
    std::fs::write(dir.path().join("comments.json"), &v1).unwrap();

    comments::load(dir.path()).unwrap();
    comments::load(dir.path()).unwrap(); // second run: identical backup already exists
    assert_eq!(
        std::fs::read(dir.path().join("comments.json.v1.bak")).unwrap(),
        v1
    );
    assert!(
        !dir.path().join("comments.json.v1.bak.1").exists(),
        "an identical backup is not duplicated"
    );
}

#[test]
fn zero_byte_and_missing_files_are_empty_stores() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        comments::load(dir.path()).unwrap(),
        Store::default(),
        "missing = empty"
    );

    std::fs::write(dir.path().join("comments.json"), b"").unwrap();
    assert_eq!(
        comments::load(dir.path()).unwrap(),
        Store::default(),
        "zero-byte = empty"
    );
}

#[test]
fn writes_leave_no_temp_residue() {
    let dir = tempfile::tempdir().unwrap();
    comments::mutate(dir.path(), |s| {
        let id = s.take_id();
        s.branches.insert(
            "main".to_string(),
            Branch {
                active_range: None,
                comments: vec![comment(id, "a.rs", Side::New, 1, Some("x"))],
            },
        );
    })
    .unwrap();
    comments::mutate(dir.path(), |_| {}).unwrap();

    assert!(tmp_residue(dir.path()).is_empty());
    assert!(dir.path().join("comments.json").is_file());
}

fn tmp_residue(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("comments.json.tmp."))
                .collect()
        })
        .unwrap_or_default()
}

// --- gc ---

#[test]
fn gc_drops_dead_branches_and_stale_detached_keys_only() {
    let live_hex = "a".repeat(40);
    let dead_hex = "b".repeat(40);

    let mut store = Store::default();
    for (key, n) in [
        ("main", 2usize),
        ("gone", 1),
        (live_hex.as_str(), 3),
        (dead_hex.as_str(), 1),
    ] {
        let comments: Vec<Comment> = (0..n)
            .map(|i| comment(i as u64, "a.rs", Side::New, 1, Some("x")))
            .collect();
        store.branches.insert(
            key.to_string(),
            Branch {
                active_range: None,
                comments,
            },
        );
    }

    let live_branches: HashSet<String> = ["main".to_string()].into_iter().collect();
    let result = comments::gc(&mut store, &live_branches, |key| key == live_hex);

    let mut removed = result.removed_branches.clone();
    removed.sort();
    let mut expected = vec!["gone".to_string(), dead_hex.clone()];
    expected.sort();
    assert_eq!(removed, expected);
    assert_eq!(result.removed_comments, 2, "1 (gone) + 1 (dead detached)");
    assert!(store.branches.contains_key("main"), "live branch kept");
    assert!(
        store.branches.contains_key(&live_hex),
        "reachable detached kept"
    );
    assert!(!store.branches.contains_key("gone"));
    assert!(!store.branches.contains_key(&dead_hex));
}

#[test]
fn gc_keeps_a_live_branch_named_as_commit_hex() {
    // A real branch can legally be named as 40 hex chars. Live-branch membership
    // is checked before the commit-hex shape test, so it survives gc regardless
    // of whether commit_exists would report it as a resolvable commit.
    let hex_branch = "c".repeat(40);
    let mut store = Store::default();
    store.branches.insert(
        hex_branch.clone(),
        Branch {
            active_range: None,
            comments: vec![comment(1, "a.rs", Side::New, 1, Some("x"))],
        },
    );

    let live: HashSet<String> = [hex_branch.clone()].into_iter().collect();
    // commit_exists says "no" — irrelevant, because the key is a live branch.
    let result = comments::gc(&mut store, &live, |_| false);

    assert!(result.is_empty(), "a live branch is never dropped");
    assert!(
        store.branches.contains_key(&hex_branch),
        "the hex-named live branch survives gc"
    );
}

#[test]
fn gc_on_a_clean_store_removes_nothing() {
    let mut store = Store::default();
    store.branches.insert("main".to_string(), Branch::default());
    let live: HashSet<String> = ["main".to_string()].into_iter().collect();
    let result = comments::gc(&mut store, &live, |_| true);
    assert!(result.is_empty());
    assert!(store.branches.contains_key("main"));
}

// --- repo plumbing: head_branch_key / branch_names / strix_dir ---

#[test]
fn head_branch_key_is_the_branch_name_and_unborn_head_is_well_formed() {
    let repo = init_repo();
    let opened = Repo::open(repo.path()).unwrap();
    assert_eq!(opened.head_branch_key().unwrap(), "main");
    assert_eq!(opened.branch_names().unwrap(), vec!["main".to_string()]);

    // Unborn HEAD (no commits): the key is still the symbolic branch name, and
    // it is not a commit hex — gc treats it as a live branch, not a detached key.
    let empty = init_empty_repo();
    let opened = Repo::open(empty.path()).unwrap();
    assert_eq!(opened.head_branch_key().unwrap(), "main");
    assert!(
        opened.branch_names().unwrap().is_empty(),
        "no branch ref exists yet"
    );
}

#[test]
fn detached_head_key_is_the_commit_hex() {
    let repo = init_repo();
    common::git(repo.path(), &["checkout", "-q", "--detach"]);
    let opened = Repo::open(repo.path()).unwrap();
    let key = opened.head_branch_key().unwrap();
    assert_eq!(key.len(), 40, "detached key is a full commit hex: {key}");
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn a_linked_worktree_resolves_the_same_store_file() {
    let repos = init_repo_with_worktree();
    let main = Repo::open(repos.main.path()).unwrap();
    let side = Repo::open(&repos.worktree()).unwrap();

    // The two checkouts are on different branches but share one store file.
    assert_eq!(main.head_branch_key().unwrap(), "main");
    assert_eq!(side.head_branch_key().unwrap(), "side");

    // Write from the main checkout, read from the linked worktree.
    comments::mutate(&main.strix_dir(), |store| {
        let id = store.take_id();
        store.branches.insert(
            "main".to_string(),
            Branch {
                active_range: None,
                comments: vec![comment(id, "a.rs", Side::New, 1, Some("x"))],
            },
        );
    })
    .unwrap();

    let from_worktree = comments::load(&side.strix_dir()).unwrap();
    assert!(
        from_worktree.branches.contains_key("main"),
        "the worktree sees the main checkout's write"
    );
    assert_eq!(
        std::fs::canonicalize(main.strix_dir()).unwrap(),
        std::fs::canonicalize(side.strix_dir()).unwrap(),
        "both resolve to the same common-dir store directory"
    );
}

// --- re-anchor matrix (plan §3.2) ---

#[test]
fn reanchor_exact_hit_anchors() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, Some("fn keep() {"))];
    c[0].orphaned = true;
    let diff = FileDiff::Text(vec![
        line(9, "before"),
        line(10, "fn keep() {"),
        line(11, "after"),
    ]);
    let changed = reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(!c[0].orphaned, "exact match anchors");
    assert_eq!(c[0].line, 10, "line unchanged");
    assert!(changed, "orphaned flipped false → reported as changed");
}

#[test]
fn reanchor_no_change_pass_elides_the_write() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, Some("fn keep() {"))];
    // Already correctly anchored: orphaned=false, exact line present.
    let diff = FileDiff::Text(vec![line(10, "fn keep() {")]);
    let changed = reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(!changed, "nothing changed → write elided");
    assert!(!c[0].orphaned);
    assert_eq!(c[0].line, 10);
}

#[test]
fn reanchor_moved_within_window_reanchors_and_reports_changed() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, Some("target line"))];
    // The line drifted to 13 (within ±10); nothing sits at 10 anymore.
    let diff = FileDiff::Text(vec![line(12, "x"), line(13, "target line"), line(14, "y")]);
    let changed = reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(changed);
    assert_eq!(c[0].line, 13, "re-anchored to the moved line");
    assert!(!c[0].orphaned);
}

#[test]
fn reanchor_agent_edited_line_orphans() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, Some("original text"))];
    // The anchored line was rewritten; the original text appears nowhere.
    let diff = FileDiff::Text(vec![
        line(9, "keep"),
        line(10, "rewritten by agent"),
        line(11, "keep"),
    ]);
    let changed = reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(c[0].orphaned, "an edited anchor line orphans");
    assert_eq!(c[0].line, 10, "stored line kept for display");
    assert!(changed, "orphaned flipped false → true");
}

#[test]
fn reanchor_match_beyond_window_orphans() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, Some("far"))];
    // The only match is 15 lines away (> ±10): orphan, do not teleport.
    let diff = FileDiff::Text(vec![line(25, "far")]);
    reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(c[0].orphaned);
    assert_eq!(c[0].line, 10);
}

#[test]
fn reanchor_tie_break_prefers_the_smaller_line() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, Some("dup"))];
    // Equidistant matches at 8 and 12 (distance 2 each): smaller line wins.
    let diff = FileDiff::Text(vec![line(8, "dup"), line(12, "dup")]);
    reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert_eq!(c[0].line, 8, "tie broken toward the smaller line");
    assert!(!c[0].orphaned);
}

#[test]
fn reanchor_blank_line_exact_and_moved() {
    // Exact: a blank anchored line is a valid Some("").
    let mut c = vec![comment(1, "a.rs", Side::New, 5, Some(""))];
    let diff = FileDiff::Text(vec![line(4, "code"), line(5, ""), line(6, "code")]);
    reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(!c[0].orphaned);
    assert_eq!(c[0].line, 5);

    // Moved: the blank line drifted to 7; nothing blank sits at 5.
    let mut c = vec![comment(1, "a.rs", Side::New, 5, Some(""))];
    let diff = FileDiff::Text(vec![line(5, "code"), line(6, "code"), line(7, "")]);
    let changed = reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(changed);
    assert_eq!(c[0].line, 7);
    assert!(!c[0].orphaned);
}

#[test]
fn reanchor_context_none_orphans_on_drift() {
    let mut c = vec![comment(1, "a.rs", Side::New, 10, None)];
    // Even with an identical-looking line present, unavailable context never
    // matches: it orphans.
    let diff = FileDiff::Text(vec![line(10, "whatever"), line(11, "else")]);
    reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(c[0].orphaned, "context None never anchors");
    assert_eq!(c[0].line, 10);
}

#[test]
fn reanchor_file_gone_orphans() {
    let mut c = vec![comment(1, "gone.rs", Side::New, 3, Some("x"))];
    // The review lists a different file; the comment's file dropped out.
    let changed = comments::reanchor(&mut c, &[modified("other.rs")], |_| unreachable!());
    assert!(c[0].orphaned);
    assert!(changed);
}

#[test]
fn reanchor_rename_orphans_both_sides() {
    // `file` stores the new-side path at authoring time; a later rename means no
    // CommitFile matches that path, so both old- and new-side comments orphan.
    let mut c = vec![
        comment(1, "old.rs", Side::New, 3, Some("x")),
        comment(2, "old.rs", Side::Old, 4, Some("y")),
    ];
    let files = [renamed("old.rs", "new.rs")];
    comments::reanchor(&mut c, &files, |_| {
        unreachable!("renamed path never matches")
    });
    assert!(c[0].orphaned, "new-side comment on the old path orphans");
    assert!(c[1].orphaned, "old-side comment on the old path orphans");
}

#[test]
fn reanchor_binary_orphans() {
    let mut c = vec![comment(1, "img.png", Side::New, 1, Some("x"))];
    let changed = reanchor_one_file(&mut c, modified("img.png"), FileDiff::Binary);
    assert!(c[0].orphaned, "a binary file has no lines to anchor to");
    assert!(changed);
}

#[test]
fn reanchor_context_line_hunk_contraction_orphans() {
    // A Context-line comment whose nearby change was resolved: the file is still
    // listed but the anchored line left the diff window entirely.
    let mut c = vec![comment(1, "a.rs", Side::New, 40, Some("context line"))];
    // The remaining hunk covers a distant region; line 40 no longer appears and
    // no match sits within ±10.
    let diff = FileDiff::Text(vec![line(4, "context line"), line(5, "other")]);
    reanchor_one_file(&mut c, modified("a.rs"), diff);
    assert!(
        c[0].orphaned,
        "the contracted hunk no longer contains the line"
    );
    assert_eq!(c[0].line, 40);
}

#[test]
fn reanchor_only_recomputes_each_file_diff_once() {
    // Two comments on one file must share a single diff computation (lazy, cached).
    let mut c = vec![
        comment(1, "a.rs", Side::New, 3, Some("one")),
        comment(2, "a.rs", Side::New, 6, Some("two")),
    ];
    let files = [modified("a.rs")];
    let mut calls = 0;
    let diff = FileDiff::Text(vec![line(3, "one"), line(6, "two")]);
    comments::reanchor(&mut c, &files, |_| {
        calls += 1;
        diff.clone()
    });
    assert_eq!(
        calls, 1,
        "the per-file diff is computed once, not per comment"
    );
    assert!(!c[0].orphaned && !c[1].orphaned);
}

#[test]
fn reanchor_scoped_only_touches_the_selected_scope() {
    // Two comments on one file, one worktree- and one range-scoped, both stored at
    // line 10 with a context that has drifted to line 13 in the diff. Selecting one
    // scope must re-anchor only that comment and leave the other exactly as-is.
    let build = || {
        vec![
            Comment {
                scope: Scope::WorkTree,
                ..comment(1, "a.rs", Side::New, 10, Some("target"))
            },
            Comment {
                scope: Scope::Range {
                    range: "origin/main".to_string(),
                },
                ..comment(2, "a.rs", Side::New, 10, Some("target"))
            },
        ]
    };
    let files = [modified("a.rs")];
    let diff = FileDiff::Text(vec![line(12, "x"), line(13, "target"), line(14, "y")]);

    let mut c = build();
    let changed = comments::reanchor_scoped(
        &mut c,
        |cm| matches!(cm.scope, Scope::WorkTree),
        &files,
        |_| diff.clone(),
    );
    assert!(changed, "the selected worktree comment moved");
    assert_eq!(
        c[0].line, 13,
        "worktree comment re-anchored to the moved line"
    );
    assert!(!c[0].orphaned);
    assert_eq!(
        c[1].line, 10,
        "the range comment was skipped, not re-anchored"
    );

    // The inverse selection touches only the range comment.
    let mut c = build();
    let changed = comments::reanchor_scoped(
        &mut c,
        |cm| matches!(cm.scope, Scope::Range { .. }),
        &files,
        |_| diff.clone(),
    );
    assert!(changed, "the selected range comment moved");
    assert_eq!(c[0].line, 10, "the worktree comment was skipped this time");
    assert_eq!(c[1].line, 13, "range comment re-anchored to the moved line");
    assert!(!c[1].orphaned);
}

// --- worktree lifecycle / sweep engine (plan §3.2, C2c) ---

// Two distinct baseline HEADs; `base == current_head` means "HEAD unchanged".
const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A worktree comment with an explicit baseline HEAD (`base`), as C3 records it.
fn wt(id: u64, file: &str, side: Side, line: usize, context: Option<&str>, base: &str) -> Comment {
    Comment {
        base: Some(base.to_string()),
        ..comment(id, file, side, line, context)
    }
}

/// The common facts case: file present, not renamed, change not yet in HEAD.
fn present(diff: FileDiff) -> FileFacts {
    FileFacts::Present {
        diff,
        renamed_to: None,
        resolved_in_head: false,
    }
}

#[test]
fn sweep_git_add_only_leaves_the_comment_unchanged() {
    // Staging index-only: the net HEAD→worktree diff is unchanged, so the exact
    // anchor still holds — retained, and nothing to write.
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("fn keep() {"), HEAD_A)];
    let diff = FileDiff::Text(vec![line(9, "x"), line(10, "fn keep() {"), line(11, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| present(diff.clone()));
    assert!(
        !changed,
        "an index-only change moves nothing → write elided"
    );
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].line, 10);
    assert!(!c[0].stale && !c[0].orphaned);
}

#[test]
fn sweep_unrelated_edit_reanchors_within_window() {
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("target"), HEAD_A)];
    // An edit elsewhere pushed the target down to 13 (within ±10).
    let diff = FileDiff::Text(vec![line(12, "x"), line(13, "target"), line(14, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| present(diff.clone()));
    assert!(changed);
    assert_eq!(c[0].line, 13, "re-anchored to the moved line");
    assert!(!c[0].stale && !c[0].orphaned);
    assert_eq!(c.len(), 1);
}

#[test]
fn sweep_in_place_edit_marks_stale_not_deleted() {
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("original"), HEAD_A)];
    // The anchored line's text was rewritten while HEAD is unchanged; "original"
    // appears nowhere.
    let diff = FileDiff::Text(vec![
        line(9, "keep"),
        line(10, "rewritten"),
        line(11, "keep"),
    ]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| present(diff.clone()));
    assert!(changed);
    assert_eq!(c.len(), 1, "an in-place edit is staled, never swept");
    assert!(c[0].stale, "drift under an unchanged HEAD marks stale");
    assert_eq!(c[0].line, 10, "the stored line is kept for display");
}

#[test]
fn sweep_context_regroup_never_sweeps() {
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("ctx"), HEAD_A)];
    // A nearby hunk regrouped, moving the still-present context line to 12.
    let diff = FileDiff::Text(vec![line(11, "a"), line(12, "ctx"), line(13, "b")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| present(diff.clone()));
    assert_eq!(
        c.len(),
        1,
        "a context comment is never swept for regrouping"
    );
    assert_eq!(c[0].line, 12);
    assert!(!c[0].stale);
    assert!(changed);
}

#[test]
fn sweep_unrelated_commit_retains_verbatim() {
    // HEAD advanced (A → B) but this file's change is still pending and not
    // resolved in HEAD: the note is retained exactly as-is (no write).
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("pending"), HEAD_A)];
    let diff = FileDiff::Text(vec![line(9, "x"), line(10, "pending"), line(11, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_B), |_| present(diff.clone()));
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].line, 10);
    assert!(!c[0].stale && !c[0].orphaned);
    assert!(
        !changed,
        "an unrelated commit changes nothing about the note"
    );
}

#[test]
fn sweep_partial_commit_reanchors_pending_target() {
    // A partial commit landed earlier lines (HEAD moved) but the target is still
    // pending — not resolved in HEAD — and drifted to 6.
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("still pending"), HEAD_A)];
    let diff = FileDiff::Text(vec![line(5, "x"), line(6, "still pending"), line(7, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_B), |cm| {
        assert!(matches!(cm.scope, Scope::WorkTree));
        present(diff.clone())
    });
    assert_eq!(
        c.len(),
        1,
        "a partial commit keeps the still-pending target"
    );
    assert_eq!(c[0].line, 6, "re-anchored to the moved line");
    assert!(!c[0].stale && !c[0].orphaned);
    assert!(changed);
}

#[test]
fn sweep_commit_including_the_target_sweeps() {
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("landed"), HEAD_A)];
    // HEAD advanced and the anchored change resolved into it (file now clean).
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_B), |_| FileFacts::Present {
        diff: FileDiff::Text(vec![]),
        renamed_to: None,
        resolved_in_head: true,
    });
    assert!(changed);
    assert!(c.is_empty(), "the committed target is swept");
}

#[test]
fn sweep_keeps_a_still_pending_add_when_identical_text_committed_elsewhere() {
    // Critical false-positive guard: a New-side note on an added line `T`. A
    // partial commit advanced HEAD by adding an IDENTICAL `T` *elsewhere*, so the
    // whole-file membership signal (`resolved_in_head`) is true — but the
    // *commented* add is still pending in the net diff. The orphaned-after-reanchor
    // gate must keep it (re-anchored), never sweep it: deleting a human's note is
    // the worst outcome.
    let mut c = vec![wt(1, "a.rs", Side::New, 20, Some("dup"), HEAD_A)];
    // The commented add of "dup" is still pending, drifted to 21.
    let diff = FileDiff::Text(vec![line(20, "x"), line(21, "dup"), line(22, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_B), |_| FileFacts::Present {
        diff: diff.clone(),
        renamed_to: None,
        resolved_in_head: true, // membership true: an identical `T` landed elsewhere
    });
    assert_eq!(
        c.len(),
        1,
        "a still-pending add is never swept on a duplicate-text commit"
    );
    assert_eq!(c[0].line, 21, "re-anchored to its still-pending line");
    assert!(!c[0].stale && !c[0].orphaned);
    assert!(changed);
}

#[test]
fn sweep_vanished_file_sweeps() {
    let mut c = vec![wt(1, "gone.rs", Side::New, 3, Some("x"), HEAD_A)];
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| FileFacts::Gone);
    assert!(changed);
    assert!(c.is_empty(), "a vanished target file sweeps its notes");
}

#[test]
fn sweep_never_fires_while_head_is_unchanged() {
    // The sweep gate requires an actual HEAD move: even if the caller reports the
    // content resolved, an unchanged HEAD must never delete a note (it protects
    // against sweeping mid-edit before a commit).
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("x"), HEAD_A)];
    let diff = FileDiff::Text(vec![line(10, "x")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| FileFacts::Present {
        diff: diff.clone(),
        renamed_to: None,
        resolved_in_head: true,
    });
    assert_eq!(c.len(), 1, "HEAD unchanged: the sweep gate holds");
    assert!(!changed);
}

#[test]
fn sweep_rename_repoints_and_reanchors() {
    let mut c = vec![wt(1, "old.rs", Side::New, 3, Some("moved"), HEAD_A)];
    // The file was renamed old.rs → new.rs; its net diff (under the new path)
    // still carries the anchored line.
    let diff = FileDiff::Text(vec![line(2, "x"), line(3, "moved"), line(4, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| FileFacts::Present {
        diff: diff.clone(),
        renamed_to: Some("new.rs".to_string()),
        resolved_in_head: false,
    });
    assert!(changed);
    assert_eq!(c[0].file, "new.rs", "the note follows the rename");
    assert_eq!(c[0].line, 3);
    assert!(!c[0].stale && !c[0].orphaned);
}

#[test]
fn sweep_rename_with_lost_content_goes_stale() {
    let mut c = vec![wt(1, "old.rs", Side::New, 3, Some("moved"), HEAD_A)];
    // Renamed, and the anchored text is gone from the new file → stale, not swept.
    let diff = FileDiff::Text(vec![line(2, "x"), line(3, "different"), line(4, "y")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| FileFacts::Present {
        diff: diff.clone(),
        renamed_to: Some("new.rs".to_string()),
        resolved_in_head: false,
    });
    assert!(changed);
    assert_eq!(
        c[0].file, "new.rs",
        "the note is re-pointed to the new path"
    );
    assert!(
        c[0].stale,
        "content lost across the rename → stale, not swept"
    );
    assert_eq!(c.len(), 1);
}

#[test]
fn sweep_leaves_range_comments_untouched() {
    // A range comment and a worktree comment on the same (now-vanished) file: only
    // the worktree one is evaluated — range comments are never passed to
    // `facts_for` — and only it is swept.
    let mut c = vec![
        Comment {
            scope: Scope::Range {
                range: "main".to_string(),
            },
            base: None,
            ..comment(1, "gone.rs", Side::New, 3, Some("x"))
        },
        wt(2, "gone.rs", Side::New, 3, Some("x"), HEAD_A),
    ];
    let range_before = c[0].clone();
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |cm| {
        assert!(
            matches!(cm.scope, Scope::WorkTree),
            "range comments are never evaluated by the worktree sweep"
        );
        FileFacts::Gone
    });
    assert!(changed);
    assert_eq!(c.len(), 1, "only the worktree comment was swept");
    assert_eq!(
        c[0], range_before,
        "the range comment is untouched by worktree drift"
    );
}

#[test]
fn sweep_stale_flag_persists_across_reload() {
    let dir = tempfile::tempdir().unwrap();
    comments::mutate(dir.path(), |s| {
        s.branches.insert(
            "main".to_string(),
            Branch {
                active_range: None,
                comments: vec![wt(1, "a.rs", Side::New, 10, Some("original"), HEAD_A)],
            },
        );
    })
    .unwrap();

    // An in-place edit (no commit) marks the note stale; persist via the
    // write-eliding path, exactly as a refresh would.
    let diff = FileDiff::Text(vec![line(10, "rewritten")]);
    comments::mutate_if_changed(dir.path(), |s| {
        let branch = s.branches.get_mut("main").unwrap();
        let changed = comments::sweep_worktree(&mut branch.comments, Some(HEAD_A), |_| {
            present(diff.clone())
        });
        ((), changed)
    })
    .unwrap();

    let loaded = comments::load(dir.path()).unwrap();
    let comments = &loaded.branches["main"].comments;
    assert_eq!(comments.len(), 1, "staled, not swept");
    assert!(
        comments[0].stale,
        "the stale flag survived the write + reload"
    );
}

#[test]
fn sweep_no_op_pass_reports_no_change() {
    // Mirrors the re-anchor elision contract: a pass that leaves every comment
    // exactly put reports `false`, so `mutate_if_changed` skips the write.
    let mut c = vec![wt(1, "a.rs", Side::New, 10, Some("stable"), HEAD_A)];
    let diff = FileDiff::Text(vec![line(10, "stable")]);
    let changed = comments::sweep_worktree(&mut c, Some(HEAD_A), |_| present(diff.clone()));
    assert!(!changed, "nothing changed → the write is elided");
    assert_eq!(c[0].line, 10);
    assert!(!c[0].stale && !c[0].orphaned);
}
