//! Net HEAD→worktree diff (`Repo::file_diff_head_vs_worktree`) and the Status
//! diff pane that shows it (plan §0/§3.1, C2b). The helper takes a file
//! descriptor so an uncommitted rename resolves its old side from `orig_path`;
//! it is "net worktree state relative to HEAD", so a staged-then-reverted file
//! is an *empty* diff even though it lists in both sections.

mod common;

use common::{git, init_empty_repo, init_repo, write};
use strix::app::App;
use strix::crossterm::event::{KeyCode, KeyEvent};
use strix::git::{FileDiff, FileEntry, LineKind, Repo};
use strix::terminal::dump_frame;

/// Every changed file the status lists (staged first, then unstaged), so a test
/// can pull an entry by path regardless of which section it fell into.
fn all_entries(r: &Repo) -> Vec<FileEntry> {
    let status = r.status().unwrap();
    status
        .staged
        .iter()
        .chain(status.unstaged.iter())
        .cloned()
        .collect()
}

/// The first entry with `path`, preferring a staged one (staged is listed
/// first). Panics if the path isn't listed.
fn entry_for(r: &Repo, path: &str) -> FileEntry {
    all_entries(r)
        .into_iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("{path} not listed in status"))
}

fn text_lines(diff: &FileDiff) -> &[strix::git::DiffLine] {
    match diff {
        FileDiff::Text(lines) => lines,
        FileDiff::Binary => panic!("expected a text diff, got Binary"),
    }
}

fn additions(diff: &FileDiff) -> Vec<String> {
    text_lines(diff)
        .iter()
        .filter(|l| l.kind == LineKind::Addition)
        .map(|l| l.text.clone())
        .collect()
}

fn deletions(diff: &FileDiff) -> Vec<String> {
    text_lines(diff)
        .iter()
        .filter(|l| l.kind == LineKind::Deletion)
        .map(|l| l.text.clone())
        .collect()
}

#[test]
fn modified_file_has_head_vs_worktree_additions_and_deletions() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# changed\nnew line\n"); // was "# test\n"

    let r = Repo::open(path).unwrap();
    let diff = r.file_diff_head_vs_worktree(&entry_for(&r, "README.md"));

    assert!(deletions(&diff).iter().any(|t| t.contains("# test")));
    assert!(additions(&diff).iter().any(|t| t.contains("# changed")));
    assert!(additions(&diff).iter().any(|t| t.contains("new line")));
}

#[test]
fn staged_then_reverted_in_worktree_is_an_empty_net_diff() {
    let repo = init_repo();
    let path = repo.path();
    // Stage a change, then revert the worktree back to the committed content.
    write(path, "README.md", "# changed\n");
    git(path, &["add", "README.md"]);
    write(path, "README.md", "# test\n"); // back to HEAD content

    let r = Repo::open(path).unwrap();
    // README lists in *both* sections, but its net HEAD→worktree state is clean.
    let status = r.status().unwrap();
    assert!(status.staged.iter().any(|e| e.path == "README.md"));
    assert!(status.unstaged.iter().any(|e| e.path == "README.md"));

    let diff = r.file_diff_head_vs_worktree(&entry_for(&r, "README.md"));
    assert!(
        text_lines(&diff).is_empty(),
        "net diff of a staged-then-reverted file is empty: {diff:?}"
    );
}

#[test]
fn uncommitted_rename_reads_old_side_from_orig_path() {
    let repo = init_repo();
    let path = repo.path();
    // `git mv` stages the rename; then edit the new path in the worktree so the
    // net diff is non-empty and clearly derived from HEAD:<orig_path>.
    git(path, &["mv", "README.md", "renamed.md"]);
    write(path, "renamed.md", "# test\nextra\n");

    let r = Repo::open(path).unwrap();
    let entry = entry_for(&r, "renamed.md");
    assert_eq!(
        entry.orig_path.as_deref(),
        Some("README.md"),
        "the rename source is carried on the entry"
    );

    let diff = r.file_diff_head_vs_worktree(&entry);
    // Old side is HEAD:README.md ("# test\n"), so "# test" is unchanged context
    // and only "extra" is added. Were orig_path ignored (HEAD:renamed.md, which
    // has no blob), the whole file would read as additions (2), not 1.
    assert_eq!(additions(&diff), vec!["extra".to_string()]);
    assert!(deletions(&diff).is_empty());
    assert!(
        text_lines(&diff)
            .iter()
            .any(|l| l.kind == LineKind::Context && l.text.contains("# test")),
        "the HEAD:orig_path content appears as context",
    );
}

#[test]
fn untracked_file_is_additions_only() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "new.txt", "a\nb\nc\n");

    let r = Repo::open(path).unwrap();
    let diff = r.file_diff_head_vs_worktree(&entry_for(&r, "new.txt"));

    assert_eq!(additions(&diff).len(), 3);
    assert!(deletions(&diff).is_empty());
}

#[test]
fn unborn_head_is_additions_only() {
    // No commits: `HEAD:path` never resolves, so the old side is empty.
    let repo = init_empty_repo();
    let path = repo.path();
    write(path, "first.txt", "one\ntwo\n");
    git(path, &["add", "first.txt"]); // staged Added under an unborn HEAD

    let r = Repo::open(path).unwrap();
    let diff = r.file_diff_head_vs_worktree(&entry_for(&r, "first.txt"));

    assert_eq!(additions(&diff), vec!["one".to_string(), "two".to_string()]);
    assert!(deletions(&diff).is_empty());
}

#[test]
fn worktree_deletion_is_deletions_only() {
    let repo = init_repo();
    let path = repo.path();
    std::fs::remove_file(path.join("README.md")).unwrap();

    let r = Repo::open(path).unwrap();
    let diff = r.file_diff_head_vs_worktree(&entry_for(&r, "README.md"));

    assert!(deletions(&diff).iter().any(|t| t.contains("# test")));
    assert!(additions(&diff).is_empty());
}

#[test]
fn binary_file_is_detected() {
    let repo = init_repo();
    let path = repo.path();
    std::fs::write(path.join("blob.dat"), [0u8, 1, 2, 0, 255, 0]).unwrap();

    let r = Repo::open(path).unwrap();
    assert_eq!(
        r.file_diff_head_vs_worktree(&entry_for(&r, "blob.dat")),
        FileDiff::Binary,
    );
}

/// A file that is staged and then re-edited lists under *both* sections. The
/// Status diff pane shows one net HEAD→worktree diff, so selecting the staged
/// row or the unstaged row resolves to the same diff — and the title reads
/// `pending · HEAD→worktree`.
#[test]
fn status_pane_shows_the_same_net_diff_from_either_section() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nstaged\n");
    git(path, &["add", "README.md"]);
    write(path, "README.md", "# test\nstaged\nunstaged\n");

    let mut app = App::new(path.to_path_buf()).expect("app");

    // Selection 0 is the staged README row. The pane already shows the *net*
    // change: both the staged line and the worktree-only line, which the old
    // HEAD-vs-index staged diff would never include.
    let staged_diff = app.current_diff.clone().expect("a diff at selection 0");
    assert!(additions(&staged_diff).iter().any(|t| t == "staged"));
    assert!(additions(&staged_diff).iter().any(|t| t == "unstaged"));

    // The title labels the pane as the net pending change.
    let out = dump_frame(&app, 100, 30).expect("dump_frame");
    assert!(
        out.contains("pending · HEAD→worktree"),
        "diff pane title labels the net change"
    );

    // Move to the unstaged README row: same path, so the pane keeps the one net
    // diff (dedup by path — no recompute, no divergence).
    app.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(
        app.selected_file().map(|(_, e)| e.path.clone()).as_deref(),
        Some("README.md"),
        "still on README, now via the unstaged section",
    );
    let unstaged_diff = app.current_diff.clone().expect("a diff at selection 1");
    assert_eq!(
        staged_diff, unstaged_diff,
        "both sections resolve to the same net diff",
    );
}
