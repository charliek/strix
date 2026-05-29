mod common;

use common::{git, init_repo, write};
use strix::git::{FileDiff, LineKind, Repo, Section};

#[test]
fn unstaged_modification_has_additions_and_deletions() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# changed\nnew line\n"); // was "# test\n"

    let r = Repo::open(path).unwrap();
    let status = r.status().unwrap();
    let entry = status
        .unstaged
        .iter()
        .find(|e| e.path == "README.md")
        .unwrap();

    let FileDiff::Text(lines) = r.diff(Section::Unstaged, entry) else {
        panic!("expected a text diff");
    };
    assert!(lines.iter().any(|l| l.kind == LineKind::Hunk));
    assert!(lines
        .iter()
        .any(|l| l.kind == LineKind::Deletion && l.text.contains("# test")));
    assert!(lines
        .iter()
        .any(|l| l.kind == LineKind::Addition && l.text.contains("# changed")));
}

#[test]
fn untracked_file_is_all_additions() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "new.txt", "a\nb\nc\n");

    let r = Repo::open(path).unwrap();
    let status = r.status().unwrap();
    let entry = status
        .unstaged
        .iter()
        .find(|e| e.path == "new.txt")
        .unwrap();

    let FileDiff::Text(lines) = r.diff(Section::Unstaged, entry) else {
        panic!("expected a text diff");
    };
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == LineKind::Addition)
            .count(),
        3
    );
    assert!(lines.iter().all(|l| l.kind != LineKind::Deletion));
}

#[test]
fn staged_diff_compares_index_to_head() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nstaged change\n");
    git(path, &["add", "README.md"]);

    let r = Repo::open(path).unwrap();
    let status = r.status().unwrap();
    let entry = status
        .staged
        .iter()
        .find(|e| e.path == "README.md")
        .unwrap();

    let FileDiff::Text(lines) = r.diff(Section::Staged, entry) else {
        panic!("expected a text diff");
    };
    assert!(lines
        .iter()
        .any(|l| l.kind == LineKind::Addition && l.text.contains("staged change")));
}

#[test]
fn binary_files_are_detected() {
    let repo = init_repo();
    let path = repo.path();
    std::fs::write(path.join("blob.dat"), [0u8, 1, 2, 0, 255, 0]).unwrap();

    let r = Repo::open(path).unwrap();
    let status = r.status().unwrap();
    let entry = status
        .unstaged
        .iter()
        .find(|e| e.path == "blob.dat")
        .unwrap();

    assert!(matches!(r.diff(Section::Unstaged, entry), FileDiff::Binary));
}

#[test]
fn diff_renders_in_the_pane() {
    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# test\nADDED_LINE\n");

    let app = strix::app::App::new(path.to_path_buf()).unwrap();
    let out = strix::terminal::dump_frame(&app, 120, 30).unwrap();
    assert!(out.contains("ADDED_LINE"), "diff body shows the added line");
    assert!(out.contains("@@"), "hunk header shown");
}

#[test]
fn side_by_side_pairs_old_and_new() {
    use strix::app::{App, DiffMode};
    use strix::crossterm::event::{KeyCode, KeyEvent};

    let repo = init_repo();
    let path = repo.path();
    write(path, "README.md", "# changed\n"); // was "# test\n": one deletion + one addition

    let mut app = App::new(path.to_path_buf()).unwrap();
    app.on_key(KeyEvent::from(KeyCode::Char('d')));
    assert_eq!(app.diff_mode, DiffMode::SideBySide);

    let out = strix::terminal::dump_frame(&app, 120, 20).unwrap();
    assert!(
        out.lines()
            .any(|line| line.contains("# test") && line.contains("# changed")),
        "deletion (old) and addition (new) render on the same row"
    );
}
