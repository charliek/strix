//! End-to-end auto-refresh (plan 003 §3.1, C1): joins the watcher's signal to
//! the screen. Each test builds the `App` and puts it in the target view
//! *first*, spawns the watcher, drains any setup noise until quiet, makes one
//! external change, waits for the signal, then does exactly what the event
//! loop does per signal (`src/terminal.rs:183-184`: `app.reload()`), and
//! asserts the change reached the rendered frame.
//!
//! Driving `terminal::event_loop` itself was rejected: it needs a real
//! backend, and the seam it would add is two lines.

mod common;

use strix::app::App;
use strix::comments::{self, Branch, Comment, Scope, Side, Source};
use strix::git::Repo;

const W: u16 = 100;
const H: u16 = 30;

fn dump(app: &App) -> String {
    common::dump(app, W, H)
}

#[test]
fn status_reload_shows_an_externally_modified_file() {
    let repo = common::init_repo(); // README.md is tracked, clean.
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let _ = dump(&app); // Status is the default view; establish geometry.

    let rx = common::spawn_and_drain(repo.path());
    common::write(repo.path(), "README.md", "# test\nedited externally\n");
    common::expect_signal(&rx, "the external edit");

    app.reload();

    let frame = dump(&app);
    // The Changes-list row reads "  M README.md"; the diff pane's own title
    // ("… HEAD→worktree · README.md") also contains "README.md" but never
    // immediately after a bare `M`, so this needle picks the list row alone.
    assert!(
        frame.contains("M README.md"),
        "expected a modified-file (`M`) row for README.md:\n{frame}"
    );
}

#[test]
fn history_reload_shows_the_new_commit_in_the_graph() {
    let repo = common::init_repo_with_history();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.on_key(common::key('i')); // enter History, the target view.
    assert_eq!(app.view, strix::app::ViewMode::History);
    let _ = dump(&app);

    let rx = common::spawn_and_drain(repo.path());
    common::write(repo.path(), "external.txt", "from outside\n");
    common::git(repo.path(), &["add", "external.txt"]);
    // Short enough to survive the graph pane's fixed ~30-col width alongside
    // the `main HEAD` decoration on the tip commit.
    common::commit_at(repo.path(), "new work", "2021-01-04T00:00:00");
    common::expect_signal(&rx, "the external commit");

    app.reload();

    let frame = dump(&app);
    assert!(
        frame.contains("new work"),
        "expected the new commit's summary in the graph:\n{frame}"
    );
}

#[test]
fn review_reload_lists_a_file_added_on_the_head_branch() {
    let (repo, mut app) = common::review_app("main"); // HEAD is `feature`.
    assert_eq!(app.view, strix::app::ViewMode::Review);
    let _ = dump(&app);

    let rx = common::spawn_and_drain(repo.path());
    common::commit_file(
        repo.path(),
        "new_on_feature.txt",
        "hello\n",
        "add new_on_feature.txt",
    );
    common::expect_signal(&rx, "the head-branch commit");

    app.reload();

    let frame = dump(&app);
    assert!(
        frame.contains("new_on_feature.txt"),
        "expected the head-branch commit's new file in the review list:\n{frame}"
    );
}

#[test]
fn status_reload_shows_an_out_of_band_comment_as_a_box() {
    let repo = common::init_repo();
    common::write(repo.path(), "new.txt", "target\n");
    let base = common::head_oid(repo.path());
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    common::select_status_file(&mut app, "new.txt", W, H, 40);

    let rx = common::spawn_and_drain(repo.path());
    let handle = Repo::open(repo.path()).expect("open repo");
    let dir = handle.strix_dir();
    // The real atomic path an agent's CLI write goes through: read-modify-write
    // with a tmp file + rename, never a raw overwrite.
    comments::mutate(&dir, |store| {
        store.branches.insert(
            "main".to_string(),
            Branch {
                active_range: None,
                comments: vec![Comment {
                    scope: Scope::WorkTree,
                    id: 1,
                    source: Source::Agent,
                    file: "new.txt".to_string(),
                    side: Side::New,
                    line: 1,
                    text: "flagged out of band".to_string(),
                    context: Some("target".to_string()),
                    orphaned: false,
                    created_at: 1_700_000_000,
                    base: Some(base.clone()),
                    stale: false,
                }],
            },
        );
    })
    .expect("write comment store");
    common::expect_signal(&rx, "the out-of-band comment write");

    app.reload();

    let frame = dump(&app);
    assert!(
        frame.contains("flagged out of band"),
        "expected the out-of-band comment box:\n{frame}"
    );
}
