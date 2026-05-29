mod common;

use common::{git, init_repo, write};
use strix::git::status::{parse, Change};
use strix::git::Repo;

#[test]
fn parses_branch_header() {
    let out = b"# branch.oid abc123\x00# branch.head main\x00";
    let status = parse(out);
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert!(status.is_clean());
}

#[test]
fn parses_detached_head() {
    let out = b"# branch.oid deadbeefcafe\x00# branch.head (detached)\x00";
    let status = parse(out);
    assert!(status.detached);
    assert_eq!(status.head_label().as_deref(), Some("detached @ deadbeef"));
}

#[test]
fn parses_staged_modified_and_untracked() {
    let out =
        b"# branch.head main\x001 M. N... 100644 100644 100644 1111111 2222222 src/app.rs\x00? new.txt\x00";
    let status = parse(out);
    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.staged[0].path, "src/app.rs");
    assert_eq!(status.staged[0].change, Change::Modified);
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.unstaged[0].path, "new.txt");
    assert_eq!(status.unstaged[0].change, Change::Untracked);
}

#[test]
fn parses_same_file_staged_and_unstaged() {
    let out = b"1 MM N... 100644 100644 100644 1111111 2222222 a.rs\x00";
    let status = parse(out);
    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.staged[0].change, Change::Modified);
    assert_eq!(status.unstaged[0].change, Change::Modified);
}

#[test]
fn parses_rename_with_original_path() {
    let out = b"2 R. N... 100644 100644 100644 1111111 2222222 R100 new.rs\x00old.rs\x00";
    let status = parse(out);
    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.staged[0].change, Change::Renamed);
    assert_eq!(status.staged[0].path, "new.rs");
    assert_eq!(status.staged[0].orig_path.as_deref(), Some("old.rs"));
    assert_eq!(status.staged[0].display_path(), "old.rs → new.rs");
}

#[test]
fn preserves_paths_with_spaces() {
    let out = b"1 .M N... 100644 100644 100644 1111111 2222222 my file.txt\x00";
    let status = parse(out);
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.unstaged[0].path, "my file.txt");
    assert_eq!(status.unstaged[0].change, Change::Modified);
}

#[test]
fn reads_real_repo_status() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nmore\n"); // modified (unstaged)
    write(path, "staged.txt", "hello\n");
    git(path, &["add", "staged.txt"]); // added (staged)
    write(path, "untracked.txt", "x\n"); // untracked

    let status = Repo::open(path).expect("open").status().expect("status");

    assert_eq!(status.branch.as_deref(), Some("main"));
    assert!(status
        .staged
        .iter()
        .any(|e| e.path == "staged.txt" && e.change == Change::Added));
    assert!(status
        .unstaged
        .iter()
        .any(|e| e.path == "README.md" && e.change == Change::Modified));
    assert!(status
        .unstaged
        .iter()
        .any(|e| e.path == "untracked.txt" && e.change == Change::Untracked));
}
