mod common;

use common::{init_repo_with_branches, init_repo_with_history};
use strix::git::Repo;
use strix::graph::{self, GraphRow};

fn node_glyph(row: &GraphRow) -> char {
    row.cells
        .iter()
        .find(|c| c.lane == row.node_lane)
        .map(|c| c.glyph)
        .expect("node cell present")
}

fn glyphs(row: &GraphRow) -> String {
    row.cells.iter().map(|c| c.glyph).collect()
}

#[test]
fn linear_history_is_a_single_lane() {
    let dir = init_repo_with_history();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let refs = repo.ref_labels().expect("refs");
    let rows = graph::layout(&commits, &refs);

    assert_eq!(rows.len(), commits.len());
    for row in &rows {
        assert_eq!(row.node_lane, 0);
        assert_eq!(node_glyph(row), '●');
        // No rails beyond the single node column.
        let rail: String = glyphs(row).trim().to_string();
        assert_eq!(rail, "●", "linear row should be just a node: {rail:?}");
    }
}

#[test]
fn merge_spawns_and_rejoins_a_lane() {
    let dir = init_repo_with_branches();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let refs = repo.ref_labels().expect("refs");
    let rows = graph::layout(&commits, &refs);

    // Commit-time order: merge, add main file, add feature, init.
    let node_lanes: Vec<usize> = rows.iter().map(|r| r.node_lane).collect();
    assert_eq!(node_lanes, vec![0, 0, 1, 0], "lanes: {node_lanes:?}");

    // Every node renders as a filled dot.
    for row in &rows {
        assert_eq!(node_glyph(row), '●');
    }

    // The merge (row 0) opens a second lane; the shared-parent commits rejoin it.
    let all: String = rows.iter().map(glyphs).collect();
    assert!(all.contains('╮'), "a lane should spawn: {all:?}");
    assert!(all.contains('╯'), "a lane should rejoin: {all:?}");
    assert!(
        rows.iter().any(|r| r.cells.len() >= 2),
        "the merge should widen the graph to two lanes"
    );
}

#[test]
fn merge_into_active_branch_keeps_a_connector() {
    // A "criss-cross" style: two branches each merge in the other branch's
    // tip. The second merge's extra parent is already an active lane, so the
    // connector must still be drawn (regression for the "no glyph for active
    // merge parent" bug Codex flagged).
    let dir = init_repo_with_branches();
    let path = dir.path();
    // Tip is the merge from init_repo_with_branches; extend with a second
    // merge that re-references the already-active lane.
    common::git(path, &["checkout", "-q", "-b", "feature2"]);
    common::write(path, "f2.txt", "x\n");
    common::git(path, &["add", "."]);
    common::git_env(
        path,
        &[
            ("GIT_AUTHOR_DATE", "2021-01-05T00:00:00"),
            ("GIT_COMMITTER_DATE", "2021-01-05T00:00:00"),
        ],
        &["commit", "-q", "-m", "f2 tip"],
    );
    common::git(path, &["checkout", "-q", "main"]);
    common::git_env(
        path,
        &[
            ("GIT_AUTHOR_DATE", "2021-01-06T00:00:00"),
            ("GIT_COMMITTER_DATE", "2021-01-06T00:00:00"),
        ],
        &["merge", "--no-ff", "-q", "-m", "merge f2", "feature2"],
    );
    let repo = Repo::open(path).expect("open repo");
    let commits = repo.history(50).expect("history");
    let refs = repo.ref_labels().expect("refs");
    let rows = graph::layout(&commits, &refs);

    // The newest row (the f2 merge) must show a side connector — either a
    // spawn corner or the join glyph on the merged lane. Without the fix, the
    // active-lane parent was skipped and the row had only a node.
    let merge_row = &rows[0];
    let connectors: String = merge_row
        .cells
        .iter()
        .filter(|c| matches!(c.glyph, '╮' | '╭' | '╯' | '╰' | '─'))
        .map(|c| c.glyph)
        .collect();
    assert!(
        !connectors.is_empty(),
        "merge row should keep a connector glyph: {:?}",
        merge_row.cells
    );
}

#[test]
fn tip_commit_carries_branch_labels() {
    let dir = init_repo_with_branches();
    let repo = Repo::open(dir.path()).expect("open repo");
    let commits = repo.history(50).expect("history");
    let refs = repo.ref_labels().expect("refs");
    let rows = graph::layout(&commits, &refs);

    // HEAD / main point at the merge commit (row 0); feature at "add feature".
    assert!(
        rows[0].labels.iter().any(|l| l == "main"),
        "merge row labels: {:?}",
        rows[0].labels
    );
    let feature_row = rows
        .iter()
        .find(|r| commits[r.commit].summary == "add feature")
        .expect("feature commit row");
    assert!(
        feature_row.labels.iter().any(|l| l == "feature"),
        "feature row labels: {:?}",
        feature_row.labels
    );
}
