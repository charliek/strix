//! Commit-graph lane layout: a **pure** transform from an ordered commit list
//! (newest first) into one render row per commit, assigning each commit a lane
//! (column) and emitting the rail glyphs that connect a commit to its parents.
//!
//! This holds no repository handle and does no rendering — it maps
//! `(commits, refs)` to [`GraphRow`]s so the UI can draw colored rails and so the
//! layout is unit-testable in isolation. One row per commit keeps the Graph's
//! selection / scroll / mouse hit-testing a trivial index, matching the staging
//! list.
//!
//! The model is the standard "active lanes" walk: each lane remembers the commit
//! id it is waiting to draw next (a child has been drawn, its parent is pending).
//! Merges spawn a lane for each extra parent; branches that share a parent
//! converge back into one lane.

use crate::git::{CommitInfo, RefLabel};

/// One glyph in a graph row, tagged with the lane (column) it belongs to so the
/// renderer can colour rails by lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphCell {
    pub glyph: char,
    pub lane: usize,
}

/// The graph rail for a single commit row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    /// Index into the commit slice this row was built from.
    pub commit: usize,
    /// Column of this commit's node (`●`).
    pub node_lane: usize,
    /// Left-to-right rail glyphs for this row.
    pub cells: Vec<GraphCell>,
    /// Ref/branch labels pointing at this commit.
    pub labels: Vec<String>,
}

const NODE: char = '●';
const VERTICAL: char = '│';
const SPAWN_RIGHT: char = '╮';
const SPAWN_LEFT: char = '╭';
const JOIN_RIGHT: char = '╯';
const JOIN_LEFT: char = '╰';
const HORIZONTAL: char = '─';

/// Lay out the rail graph for `commits` (newest first). `refs` supplies the
/// labels badged onto the commits they point at.
pub fn layout(commits: &[CommitInfo], refs: &[RefLabel]) -> Vec<GraphRow> {
    // Each lane holds the oid it is waiting to draw next, or None when free.
    let mut lanes: Vec<Option<gix::ObjectId>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for (index, commit) in commits.iter().enumerate() {
        let before = lanes.clone();

        // Lanes already waiting for this commit converge on it; the leftmost is
        // the node's lane. With none, the commit is a fresh tip in a new lane.
        let converging: Vec<usize> = before
            .iter()
            .enumerate()
            .filter(|(_, slot)| **slot == Some(commit.id))
            .map(|(lane, _)| lane)
            .collect();
        let node_lane = match converging.first() {
            Some(&lane) => lane,
            None => free_lane(&mut lanes),
        };

        // The first parent continues in the node's lane; converged side-lanes
        // are released.
        lanes[node_lane] = commit.parents.first().copied();
        for &lane in converging.iter().skip(1) {
            lanes[lane] = None;
        }

        // Extra (merge) parents take their own lane: reuse one already waiting
        // for that parent, else open a fresh lane.
        let mut spawned: Vec<usize> = Vec::new();
        for parent in commit.parents.iter().skip(1) {
            if lanes.contains(&Some(*parent)) {
                continue;
            }
            let lane = free_lane(&mut lanes);
            lanes[lane] = Some(*parent);
            spawned.push(lane);
        }

        let after = &lanes;
        let cells = row_cells(node_lane, &converging, &spawned, &before, after);

        rows.push(GraphRow {
            commit: index,
            node_lane,
            cells,
            labels: labels_for(commit, refs),
        });
    }

    rows
}

/// Reuse the leftmost free lane, or append a new one. Returns its index.
fn free_lane(lanes: &mut Vec<Option<gix::ObjectId>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(lane) => lane,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

/// Build the glyph row: the node, vertical pass-throughs for lanes that survive,
/// corner connectors for converging/spawned side-lanes, and horizontal fill
/// between the node and those side-lanes.
fn row_cells(
    node_lane: usize,
    converging: &[usize],
    spawned: &[usize],
    before: &[Option<gix::ObjectId>],
    after: &[Option<gix::ObjectId>],
) -> Vec<GraphCell> {
    let width = before.len().max(after.len()).max(node_lane + 1);
    let mut glyphs = vec![' '; width];

    // Lanes that pass straight through (active before and after, untouched).
    for (lane, slot) in glyph_iter(before, width) {
        if slot.is_some() && lane != node_lane && !converging.contains(&lane) {
            glyphs[lane] = VERTICAL;
        }
    }
    for (lane, slot) in glyph_iter(after, width) {
        if slot.is_some() && lane != node_lane && !spawned.contains(&lane) {
            glyphs[lane] = VERTICAL;
        }
    }

    // Horizontal reach from the node to each side-lane, filling only gaps so
    // pass-through verticals stay visible where the rail crosses them.
    for &lane in converging.iter().chain(spawned) {
        if lane == node_lane {
            continue;
        }
        let (lo, hi) = (lane.min(node_lane), lane.max(node_lane));
        for cell in glyphs.iter_mut().take(hi).skip(lo + 1) {
            if *cell == ' ' {
                *cell = HORIZONTAL;
            }
        }
    }

    // Corners for side-lanes joining (converging) or leaving (spawned) the node.
    for &lane in converging {
        if lane != node_lane {
            glyphs[lane] = if lane > node_lane {
                JOIN_RIGHT
            } else {
                JOIN_LEFT
            };
        }
    }
    for &lane in spawned {
        glyphs[lane] = if lane > node_lane {
            SPAWN_RIGHT
        } else {
            SPAWN_LEFT
        };
    }

    glyphs[node_lane] = NODE;

    glyphs
        .into_iter()
        .enumerate()
        .map(|(lane, glyph)| GraphCell { glyph, lane })
        .collect()
}

fn glyph_iter(
    lanes: &[Option<gix::ObjectId>],
    width: usize,
) -> impl Iterator<Item = (usize, Option<gix::ObjectId>)> + '_ {
    (0..width).map(move |lane| (lane, lanes.get(lane).copied().flatten()))
}

/// The labels whose ref points exactly at this commit, branch-like refs first.
fn labels_for(commit: &CommitInfo, refs: &[RefLabel]) -> Vec<String> {
    refs.iter()
        .filter(|r| r.target == commit.id)
        .map(|r| r.name.clone())
        .collect()
}
