mod common;

use std::process::Command;

use common::{
    init_repo, init_repo_with_diverged_branches, init_repo_with_history,
    init_repo_with_orphan_branch,
};
use strix::git::{ChangeKind, CommitFile, FileDiff, Repo};

/// Run git in `dir` and return trimmed stdout lines.
fn git_lines(dir: &std::path::Path, args: &[&str]) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn git_line(dir: &std::path::Path, args: &[&str]) -> String {
    git_lines(dir, args).join("").trim().to_string()
}

fn paths(files: &[CommitFile]) -> Vec<String> {
    let mut p: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    p.sort();
    p
}

fn find<'a>(files: &'a [CommitFile], path: &str) -> &'a CommitFile {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no file {path:?} in {:?}", paths(files)))
}

#[test]
fn single_rev_matches_git_three_dot() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    let spec = repo.resolve_range("main").unwrap();
    // Single rev uses merge-base(main, HEAD)..HEAD == git's three-dot semantics.
    let base_oid = git_line(dir.path(), &["merge-base", "main", "HEAD"]);
    assert_eq!(spec.base.to_string(), base_oid);
    assert_eq!(
        spec.head.to_string(),
        git_line(dir.path(), &["rev-parse", "HEAD"])
    );

    let files = repo.range_files(&spec).unwrap();
    let mut want = git_lines(dir.path(), &["diff", "--name-only", "main...HEAD"]);
    want.sort();
    assert_eq!(paths(&files), want);
    assert_eq!(want, vec!["feature.txt", "feature2.txt", "renamed.txt"]);
}

#[test]
fn two_dot_differs_from_three_dot() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    let three = repo
        .range_files(&repo.resolve_range("main...feature").unwrap())
        .unwrap();
    let two = repo
        .range_files(&repo.resolve_range("main..feature").unwrap())
        .unwrap();

    // Three-dot compares from the merge base: only what feature added.
    assert_eq!(
        paths(&three),
        vec!["feature.txt", "feature2.txt", "renamed.txt"]
    );
    // Two-dot is a literal main→feature comparison: also reverts main's own work.
    assert_eq!(
        paths(&two),
        vec![
            "README.md",
            "feature.txt",
            "feature2.txt",
            "main-only.txt",
            "renamed.txt"
        ]
    );
    assert_ne!(paths(&three), paths(&two));
    assert_eq!(find(&two, "main-only.txt").change, ChangeKind::Deleted);
    assert_eq!(find(&two, "README.md").change, ChangeKind::Modified);
}

#[test]
fn empty_sides_default_to_head() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    // `main..` ≡ `main..HEAD`; HEAD is feature.
    let elided = repo.resolve_range("main..").unwrap();
    let explicit = repo.resolve_range("main..feature").unwrap();
    assert_eq!(elided.head, explicit.head);
    assert_eq!(
        paths(&repo.range_files(&elided).unwrap()),
        paths(&repo.range_files(&explicit).unwrap())
    );

    // `...feature` ≡ `HEAD...feature`; HEAD is feature, so base == head → empty.
    let left_elided = repo.resolve_range("...feature").unwrap();
    assert_eq!(left_elided.base, left_elided.head);
    assert!(repo.range_files(&left_elided).unwrap().is_empty());
}

#[test]
fn three_dot_precedence_beats_two_dot() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    // `main...feature..HEAD` must split on the first `...`, leaving the right
    // operand `feature..HEAD` intact (which then fails to resolve as a rev).
    let err = repo
        .resolve_range("main...feature..HEAD")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("feature..HEAD"),
        "error should name the operand: {err}"
    );
}

#[test]
fn annotated_tag_endpoint_peels_to_commit() {
    let dir = init_repo_with_diverged_branches();
    common::git(dir.path(), &["tag", "-a", "v1", "-m", "release", "main"]);
    let repo = Repo::open(dir.path()).unwrap();

    let spec = repo.resolve_range("v1..feature").unwrap();
    assert_eq!(
        spec.base.to_string(),
        git_line(dir.path(), &["rev-parse", "main^{commit}"])
    );
    // Behaves exactly like naming the branch the tag points at.
    assert_eq!(
        paths(&repo.range_files(&spec).unwrap()),
        paths(
            &repo
                .range_files(&repo.resolve_range("main..feature").unwrap())
                .unwrap()
        )
    );
}

#[test]
fn same_tip_and_ancestor_ranges_are_empty() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).unwrap();

    // Same tip on both sides.
    assert!(repo
        .range_files(&repo.resolve_range("HEAD..HEAD").unwrap())
        .unwrap()
        .is_empty());
    // Single rev at HEAD: merge-base(HEAD, HEAD) == HEAD, so HEAD..HEAD → empty.
    assert!(repo
        .range_files(&repo.resolve_range("HEAD").unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn rename_with_modification_is_reported() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    let files = repo
        .range_files(&repo.resolve_range("main...feature").unwrap())
        .unwrap();
    let renamed = find(&files, "renamed.txt");
    assert_eq!(renamed.change, ChangeKind::Renamed);
    assert_eq!(renamed.orig_path.as_deref(), Some("shared.txt"));
    // One line appended during the rename (delta); numstat counts it.
    assert_eq!(renamed.stat.added, 1);
    assert_eq!(renamed.stat.deleted, 0);

    // The lazy per-file diff reads the base blob at the old path.
    match repo.range_file_diff(&repo.resolve_range("main...feature").unwrap(), renamed) {
        FileDiff::Text(lines) => {
            assert!(lines.iter().any(|l| l.text == "delta"));
        }
        FileDiff::Binary => panic!("text file reported binary"),
    }
}

#[test]
fn added_and_deleted_files() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    let files = repo
        .range_files(&repo.resolve_range("main..feature").unwrap())
        .unwrap();
    let added = find(&files, "feature.txt");
    assert_eq!(added.change, ChangeKind::Added);
    assert_eq!(added.stat.added, 1);
    let deleted = find(&files, "main-only.txt");
    assert_eq!(deleted.change, ChangeKind::Deleted);
    assert_eq!(deleted.stat.deleted, 1);

    let spec = repo.resolve_range("main..feature").unwrap();
    // An addition has no base side; a deletion no head side.
    assert!(matches!(
        repo.range_file_diff(&spec, added),
        FileDiff::Text(_)
    ));
    assert!(matches!(
        repo.range_file_diff(&spec, deleted),
        FileDiff::Text(_)
    ));
}

#[test]
fn binary_file_numstat_is_dash() {
    let dir = init_repo();
    common::git(dir.path(), &["checkout", "-q", "-b", "work"]);
    std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();
    common::git(dir.path(), &["add", "."]);
    common::git(dir.path(), &["commit", "-q", "-m", "add binary"]);
    let repo = Repo::open(dir.path()).unwrap();

    let spec = repo.resolve_range("HEAD~1..HEAD").unwrap();
    let files = repo.range_files(&spec).unwrap();
    let bin = find(&files, "bin.dat");
    assert_eq!(bin.change, ChangeKind::Added);
    assert!(bin.stat.binary, "numstat `-` should mark the change binary");
    assert_eq!(bin.stat.added, 0);
    assert_eq!(bin.stat.deleted, 0);
    assert!(matches!(repo.range_file_diff(&spec, bin), FileDiff::Binary));
}

#[test]
fn bad_revspec_names_the_operand() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    let err = repo.resolve_range("no-such-ref").unwrap_err().to_string();
    assert!(err.contains("unknown revision"), "{err}");
    assert!(err.contains("no-such-ref"), "{err}");
}

#[test]
fn non_commit_operand_is_rejected() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    // A blob revspec resolves but isn't a commit.
    let err = repo
        .resolve_range("HEAD:README.md..feature")
        .unwrap_err()
        .to_string();
    assert!(err.contains("is not a commit"), "{err}");
    assert!(err.contains("HEAD:README.md"), "{err}");
}

#[test]
fn no_merge_base_names_both_operands() {
    let dir = init_repo_with_orphan_branch();
    let repo = Repo::open(dir.path()).unwrap();

    // `unrelated` has an independent root; there is no merge base with HEAD (main).
    let err = repo.resolve_range("unrelated").unwrap_err().to_string();
    assert!(err.contains("no merge base"), "{err}");
    assert!(err.contains("unrelated"), "{err}");
}

#[test]
fn moving_ref_updates_head_across_calls() {
    let dir = init_repo_with_diverged_branches();
    let repo = Repo::open(dir.path()).unwrap();

    let before = repo.resolve_range("main").unwrap();
    // HEAD is feature; add a commit so the feature ref moves.
    common::write(dir.path(), "feature3.txt", "even more\n");
    common::git(dir.path(), &["add", "."]);
    common::git(dir.path(), &["commit", "-q", "-m", "feat three"]);

    let after = repo.resolve_range("main").unwrap();
    assert_ne!(before.head, after.head, "head should track the moved ref");
    assert_eq!(
        after.head.to_string(),
        git_line(dir.path(), &["rev-parse", "HEAD"])
    );
    // The new file appears in the refreshed listing.
    assert!(repo
        .range_files(&after)
        .unwrap()
        .iter()
        .any(|f| f.path == "feature3.txt"));
}
