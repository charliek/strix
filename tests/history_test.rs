mod common;

use common::{
    commit_at, git, init_empty_repo, init_repo, init_repo_with_branches, init_repo_with_history,
    setup_for_binary, write,
};
use strix::git::{ChangeKind, CommitFile, CommitInfo, FileDiff, LineKind, Repo};

fn commit<'a>(commits: &'a [CommitInfo], summary: &str) -> &'a CommitInfo {
    commits
        .iter()
        .find(|c| c.summary == summary)
        .unwrap_or_else(|| panic!("no commit summarised {summary:?}"))
}

fn file<'a>(files: &'a [CommitFile], path: &str) -> &'a CommitFile {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no changed file {path:?}"))
}

fn additions(diff: &FileDiff) -> Vec<String> {
    match diff {
        FileDiff::Text(lines) => lines
            .iter()
            .filter(|l| l.kind == LineKind::Addition)
            .map(|l| l.text.clone())
            .collect(),
        FileDiff::Binary => Vec::new(),
    }
}

#[test]
fn history_lists_head_ancestry_newest_first() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");

    let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["edit readme", "add a", "init"]);

    // Commit time is non-increasing newest-first.
    assert!(commits
        .windows(2)
        .all(|w| w[0].committer_seconds >= w[1].committer_seconds));
}

#[test]
fn history_is_bounded_by_limit() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    assert_eq!(repo.history(2).expect("history").len(), 2);
}

#[test]
fn commit_files_lists_changes_vs_first_parent() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");

    let added = repo.commit_files(commit(&commits, "add a")).expect("files");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].path, "a.txt");
    assert_eq!(added[0].change, ChangeKind::Added);

    let edited = repo
        .commit_files(commit(&commits, "edit readme"))
        .expect("files");
    assert_eq!(file(&edited, "README.md").change, ChangeKind::Modified);
}

#[test]
fn root_commit_diffs_against_empty_tree() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let root = commit(&commits, "init");
    assert!(root.parents.is_empty());

    let files = repo.commit_files(root).expect("files");
    let readme = file(&files, "README.md");
    assert_eq!(readme.change, ChangeKind::Added);
    assert_eq!(readme.stat.added, 1);
    assert_eq!(readme.stat.deleted, 0);
    assert!(!readme.stat.binary);

    let diff = repo.commit_file_diff(root, file(&files, "README.md"));
    assert!(additions(&diff).iter().any(|l| l.contains("# test")));
}

#[test]
fn commit_file_diff_shows_added_line() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let edit = commit(&commits, "edit readme");
    let files = repo.commit_files(edit).expect("files");

    let diff = repo.commit_file_diff(edit, file(&files, "README.md"));
    assert!(additions(&diff).iter().any(|l| l == "second line"));
}

#[test]
fn commit_stat_counts_added_lines() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let files = repo.commit_files(commit(&commits, "add a")).expect("files");
    let stat = file(&files, "a.txt").stat;
    assert_eq!(stat.added, 1);
    assert_eq!(stat.deleted, 0);
    assert!(!stat.binary);
}

#[test]
fn binary_commit_file_is_detected() {
    let dir = setup_for_binary();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let bin = commit(&commits, "add binary");
    let files = repo.commit_files(bin).expect("files");
    let entry = file(&files, "bin.dat");
    assert!(entry.stat.binary);
    assert_eq!(repo.commit_file_diff(bin, entry), FileDiff::Binary);
}

#[test]
fn rename_commit_file_joins_numstat_by_new_path() {
    let dir = init_repo();
    let path = dir.path();
    write(path, "old.txt", "one\ntwo\nthree\n");
    git(path, &["add", "."]);
    commit_at(path, "add old", "2021-01-02T00:00:00");
    git(path, &["mv", "old.txt", "new.txt"]);
    write(path, "new.txt", "one\nTWO\nthree\n");
    git(path, &["add", "-A"]);
    commit_at(path, "rename with edit", "2021-01-03T00:00:00");

    let repo = Repo::open(path).expect("open repo");
    let commits = repo.history(50).expect("history");
    let files = repo
        .commit_files(commit(&commits, "rename with edit"))
        .expect("files");
    let entry = file(&files, "new.txt");
    assert_eq!(entry.change, ChangeKind::Renamed);
    assert_eq!(entry.orig_path.as_deref(), Some("old.txt"));
    assert_eq!(entry.stat.added, 1);
    assert_eq!(entry.stat.deleted, 1);
    assert!(!entry.stat.binary);
}

#[test]
fn deletion_commit_file_counts_deleted_lines() {
    let dir = init_repo();
    let path = dir.path();
    write(path, "gone.txt", "one\ntwo\nthree\n");
    git(path, &["add", "."]);
    commit_at(path, "add gone", "2021-01-02T00:00:00");
    git(path, &["rm", "-q", "gone.txt"]);
    commit_at(path, "remove gone", "2021-01-03T00:00:00");

    let repo = Repo::open(path).expect("open repo");
    let commits = repo.history(50).expect("history");
    let files = repo
        .commit_files(commit(&commits, "remove gone"))
        .expect("files");
    let entry = file(&files, "gone.txt");
    assert_eq!(entry.change, ChangeKind::Deleted);
    assert_eq!(entry.stat.added, 0);
    assert_eq!(entry.stat.deleted, 3);
    assert!(!entry.stat.binary);
}

#[test]
fn merge_commit_changes_are_vs_first_parent() {
    let dir = init_repo_with_branches();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let merge = commit(&commits, "merge feature");
    assert_eq!(merge.parents.len(), 2);

    // First parent is "add main file"; the merge brings in feature.txt.
    let files = repo.commit_files(merge).expect("files");
    assert_eq!(file(&files, "feature.txt").change, ChangeKind::Added);
}

#[test]
fn ref_labels_include_branches_and_head() {
    let dir = init_repo_with_branches();
    let repo = Repo::open(dir.path()).expect("open repo");
    let labels = repo.ref_labels().expect("refs");
    let names: Vec<&str> = labels.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"main"), "labels: {names:?}");
    assert!(names.contains(&"feature"), "labels: {names:?}");
    assert!(names.contains(&"HEAD"), "labels: {names:?}");
}

#[test]
fn empty_repo_history_errors() {
    let dir = init_empty_repo();
    let repo = Repo::open(dir.path()).expect("open repo");
    assert!(repo.history(10).is_err());
}
