//! The bounded track-tree query surface. `tracks.parent_track_id` has no
//! acyclicity constraint, so a recursive CTE terminates ONLY by the
//! `depth <= ?2` predicate; every fragment carries `id` plus depth and nothing else.

use sqlx::SqliteConnection;

use crate::error::Result;

/// A root sits at depth 0, so a legal tree has at most four levels.
pub const MAX_TRACK_TREE_DEPTH: i64 = 3;

/// Kernel default for `tracks.tree_task_budget` (the column is NULL by default).
pub const DEFAULT_TREE_TASK_BUDGET: i64 = 32;

/// Member admission requires `N <= B`, so this also bounds in-transaction
/// whole-tree reprojection work.
pub const MAX_TREE_TASK_BUDGET: i64 = 64;

/// Kernel default for `tracks.planner_task_ceiling`.
pub(crate) const DEFAULT_PLANNER_TASK_CEILING: i64 = 32;

/// Every enforcement point must decode nullable limits the same way, or a
/// bare NULL can silently remove an upper bound.
pub(crate) fn effective_limit(value: Option<i64>, default: i64) -> i64 {
    value.unwrap_or(default).max(0)
}

/// Both ancestor queries must expand this exact bounded fragment. `UNION`
/// cannot terminate a CTE carrying depth; the depth predicate is the only
/// cycle-termination guarantee.
macro_rules! bounded_track_ancestor_cte {
    () => {
        r#"
WITH RECURSIVE up(id, parent_track_id, depth) AS (
  SELECT id, parent_track_id, 0 FROM tracks WHERE id = ?1
  UNION ALL
  SELECT w.id, w.parent_track_id, up.depth + 1
    FROM tracks w JOIN up ON w.id = up.parent_track_id
   WHERE up.depth <= ?2
)
"#
    };
}

/// The downward twin. Same rule, same reason: only `id` is carried, and the
/// `depth <= ?2` predicate is the sole termination guarantee. A 2-cycle
/// (`a.parent = b`, `b.parent = a`) walks downward forever without it.
macro_rules! bounded_track_descendant_cte {
    () => {
        r#"
WITH RECURSIVE down(id, depth) AS (
  SELECT id, 0 FROM tracks WHERE id = ?1
  UNION ALL
  SELECT w.id, down.depth + 1
    FROM tracks w JOIN down ON w.parent_track_id = down.id
   WHERE down.depth <= ?2
)
"#
    };
}

pub const TRACK_ROOT_DEPTH_SQL: &str = concat!(
    bounded_track_ancestor_cte!(),
    "SELECT id AS root_id, depth AS parent_depth FROM up WHERE parent_track_id IS NULL"
);

pub const TRACK_BOUNDED_PATH_SQL: &str = concat!(
    bounded_track_ancestor_cte!(),
    "SELECT id, depth FROM up ORDER BY depth"
);

/// Deterministic `(created_at, id)` order; `created_at` is read by the OUTER
/// join, never carried through the recursion.
pub const TRACK_TREE_MEMBERS_SQL: &str = concat!(
    bounded_track_descendant_cte!(),
    "SELECT w.id, d.depth FROM tracks w \
     JOIN (SELECT id, min(depth) AS depth FROM down GROUP BY id) d ON w.id = d.id \
     ORDER BY w.created_at, w.id"
);

/// Membership plus fixed planner occupancy; pending rows are excluded because
/// they re-enter projection as candidates.
pub const TRACK_TREE_MEMBERS_WITH_FIXED_PLANNER_SQL: &str = concat!(
    bounded_track_descendant_cte!(),
    "SELECT w.id, d.depth, (SELECT count(*) FROM current_tasks t \
       WHERE t.track_id=w.id AND t.declared_by='spec' \
         AND t.status IN ('dispatched','running','verifying')) AS fixed_live \
     FROM tracks w \
     JOIN (SELECT id, min(depth) AS depth FROM down GROUP BY id) d ON w.id = d.id \
     ORDER BY w.created_at, w.id"
);

/// Whole-tree non-terminal planner inventory — enforcement point one.
pub const TRACK_TREE_PLANNER_INVENTORY_SQL: &str = concat!(
    bounded_track_descendant_cte!(),
    "SELECT count(*) FROM current_tasks t \
     JOIN (SELECT DISTINCT id FROM down) d ON t.track_id = d.id \
     WHERE t.declared_by = 'spec' AND t.status NOT IN ('done', 'failed', 'canceled')"
);
/// `floor(B / N)` each, remainder one apiece to the first `r` members, so
/// `Σ share = B` exactly. Purely a function of tree SHAPE — no projection
/// output — which keeps rebuild ≡ incremental.
pub fn deterministic_share(budget: i64, members: i64, index: i64) -> i64 {
    if members <= 0 {
        return 0;
    }
    let budget = budget.max(0);
    let base = budget / members;
    let remainder = budget % members;
    base + i64::from(index < remainder)
}

/// Whether enforcement point one may add a member without enforcement point
/// two assigning a zero share to any track.
pub fn can_add_tree_member(budget: i64, members: i64) -> bool {
    members.saturating_add(1) <= budget.max(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackTreeTerm {
    /// The track IS in a tree but its root could not be resolved. Callers must
    /// fail closed: one broken link would otherwise leave a whole subtree unbounded.
    RootUnresolved,
    Share(TreeShare),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeShare {
    pub root_id: String,
    pub budget: i64,
    pub members: i64,
    /// Zero-based position in the deterministic `(created_at, id)` order.
    pub member_index: i64,
    pub share: i64,
    /// An upgrade/corruption state has at least one member whose immutable
    /// occupancy exceeds its share. No member may admit a new block until the
    /// excess terminates; otherwise a less-full sibling could grow Σ above B.
    pub admission_frozen: bool,
    /// First legal B at which every member's immutable occupancy fits its
    /// deterministic share. `None` means either no freeze or no such B within
    /// [`MAX_TREE_TASK_BUDGET`].
    pub minimum_budget_to_unfreeze: Option<i64>,
}

/// [`TrackTreeTerm`] plus the countable seam whole-tree reprojection uses to
/// reject an accidental per-member recursive walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTreeTermOutcome {
    pub term: TrackTreeTerm,
    pub tree_cte_queries: u32,
}

pub async fn track_tree_term(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> Result<TrackTreeTermOutcome> {
    let mut queries = 0u32;
    let roots: Vec<(String, i64)> = sqlx::query_as(TRACK_ROOT_DEPTH_SQL)
        .bind(track_id)
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    queries += 1;
    let [(root_id, depth)] = roots.as_slice() else {
        return Ok(TrackTreeTermOutcome {
            term: TrackTreeTerm::RootUnresolved,
            tree_cte_queries: queries,
        });
    };
    if *depth > MAX_TRACK_TREE_DEPTH {
        return Ok(TrackTreeTermOutcome {
            term: TrackTreeTerm::RootUnresolved,
            tree_cte_queries: queries,
        });
    }
    let root_id = root_id.clone();
    let members: Vec<(String, i64, i64)> =
        sqlx::query_as(TRACK_TREE_MEMBERS_WITH_FIXED_PLANNER_SQL)
            .bind(&root_id)
            .bind(MAX_TRACK_TREE_DEPTH + 1)
            .fetch_all(&mut *conn)
            .await?;
    queries += 1;
    // Poisoned data: a member deeper than the legal bound, or a tree that does
    // not contain the track we started from.
    let budget = track_tree_budget(&mut *conn, &root_id).await?;
    let term = tree_share_from_member_inventory(root_id, track_id, budget, &members);
    Ok(TrackTreeTermOutcome {
        term,
        tree_cte_queries: queries,
    })
}

#[cfg(test)]
fn tree_share_from_members(
    root_id: String,
    track_id: &str,
    budget: i64,
    members: &[(String, i64)],
) -> TrackTreeTerm {
    tree_share_from_members_with_freeze(root_id, track_id, budget, members, false, None)
}

pub fn tree_share_from_member_inventory(
    root_id: String,
    track_id: &str,
    budget: i64,
    members: &[(String, i64, i64)],
) -> TrackTreeTerm {
    let shape = members
        .iter()
        .map(|(id, depth, _)| (id.clone(), *depth))
        .collect::<Vec<_>>();
    let count = members.len() as i64;
    let admission_frozen = members
        .iter()
        .enumerate()
        .any(|(index, (_, _, fixed_live))| {
            *fixed_live > deterministic_share(budget, count, index as i64)
        });
    let minimum_budget_to_unfreeze = admission_frozen.then(|| {
        (budget.saturating_add(1)..=MAX_TREE_TASK_BUDGET).find(|candidate| {
            members
                .iter()
                .enumerate()
                .all(|(index, (_, _, fixed_live))| {
                    *fixed_live <= deterministic_share(*candidate, count, index as i64)
                })
        })
    });
    tree_share_from_members_with_freeze(
        root_id,
        track_id,
        budget,
        &shape,
        admission_frozen,
        minimum_budget_to_unfreeze.flatten(),
    )
}

fn tree_share_from_members_with_freeze(
    root_id: String,
    track_id: &str,
    budget: i64,
    members: &[(String, i64)],
    admission_frozen: bool,
    minimum_budget_to_unfreeze: Option<i64>,
) -> TrackTreeTerm {
    let over_deep = members
        .iter()
        .any(|(_, depth)| *depth > MAX_TRACK_TREE_DEPTH);
    let index = members.iter().position(|(id, _)| id == track_id);
    let (Some(index), false) = (index, over_deep) else {
        return TrackTreeTerm::RootUnresolved;
    };
    let count = members.len() as i64;
    TrackTreeTerm::Share(TreeShare {
        root_id,
        budget,
        members: count,
        member_index: index as i64,
        share: deterministic_share(budget, count, index as i64),
        admission_frozen,
        minimum_budget_to_unfreeze,
    })
}

pub async fn track_tree_budget(conn: &mut SqliteConnection, root_id: &str) -> Result<i64> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT tree_task_budget FROM tracks WHERE id = ?1")
            .bind(root_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(effective_limit(
        row.and_then(|(budget,)| budget),
        DEFAULT_TREE_TASK_BUDGET,
    ))
}

pub async fn track_tree_planner_inventory(
    conn: &mut SqliteConnection,
    root_id: &str,
) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as(TRACK_TREE_PLANNER_INVENTORY_SQL)
        .bind(root_id)
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_one(&mut *conn)
        .await?;
    Ok(count)
}

pub async fn track_tree_member_count(conn: &mut SqliteConnection, root_id: &str) -> Result<i64> {
    let members: Vec<(String, i64)> = sqlx::query_as(TRACK_TREE_MEMBERS_SQL)
        .bind(root_id)
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    Ok(members.len() as i64)
}

/// Used after deleting excess pending rows: a member still over its new share
/// is over because of in-flight work, so callers reject the change.
pub async fn track_tree_planner_inventory_by_member(
    conn: &mut SqliteConnection,
    root_id: &str,
) -> Result<Vec<(String, i64)>> {
    Ok(sqlx::query_as(concat!(
        bounded_track_descendant_cte!(),
        "SELECT d.id, count(t.id) FROM (SELECT DISTINCT id FROM down) d \
         LEFT JOIN current_tasks t ON t.track_id=d.id AND t.declared_by='spec' \
           AND t.status NOT IN ('done','failed','canceled') \
         GROUP BY d.id ORDER BY d.id"
    ))
    .bind(root_id)
    .bind(MAX_TRACK_TREE_DEPTH + 1)
    .fetch_all(&mut *conn)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_member_sql_keeps_its_total_order_definition() {
        let without_line_comments = TRACK_TREE_MEMBERS_SQL
            .lines()
            .map(|line| line.split("--").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let mut without_comments = String::new();
        let mut rest = without_line_comments.as_str();
        while let Some(start) = rest.find("/*") {
            without_comments.push_str(&rest[..start]);
            let Some(end) = rest[start + 2..].find("*/") else {
                rest = "";
                break;
            };
            rest = &rest[start + 2 + end + 2..];
        }
        without_comments.push_str(rest);
        let normalized = without_comments
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            normalized.contains("ORDER BY w.created_at, w.id"),
            "quota membership lost its deterministic (created_at, id) order"
        );
    }

    #[test]
    fn shares_sum_to_the_budget_including_the_remainder() {
        for (budget, members) in [
            (32, 1),
            (32, 3),
            (32, 5),
            (32, 10),
            (7, 3),
            (2, 5),
            (0, 4),
            (1, 1),
        ] {
            let total: i64 = (0..members)
                .map(|index| deterministic_share(budget, members, index))
                .sum();
            assert_eq!(total, budget, "budget={budget} members={members}");
            // The remainder goes to a PREFIX of the order, so shares are
            // non-increasing and differ by at most one.
            let shares: Vec<i64> = (0..members)
                .map(|index| deterministic_share(budget, members, index))
                .collect();
            assert!(shares.windows(2).all(|w| w[0] >= w[1]), "{shares:?}");
            assert!(shares[0] - shares[members as usize - 1] <= 1, "{shares:?}");
        }
    }

    #[test]
    fn every_declaration_sequence_within_member_shares_respects_whole_tree_budget() {
        fn visit(shares: &[i64], index: usize, live_total: i64, budget: i64) {
            if index == shares.len() {
                assert!(
                    live_total <= budget,
                    "live_total={live_total} budget={budget} shares={shares:?}"
                );
                return;
            }
            for live in 0..=shares[index] {
                visit(shares, index + 1, live_total + live, budget);
            }
        }

        for budget in 0..=12 {
            for members in 1..=12 {
                let shares = (0..members)
                    .map(|index| deterministic_share(budget, members, index))
                    .collect::<Vec<_>>();
                visit(&shares, 0, 0, budget);
            }
        }
    }

    #[test]
    fn enforcement_points_are_compatible_for_every_budget_and_member_count() {
        for budget in 0..=64 {
            for members in 1..=64 {
                let after = members + 1;
                let every_member_gets_a_share =
                    (0..after).all(|index| deterministic_share(budget, after, index) > 0);
                assert_eq!(
                    can_add_tree_member(budget, members),
                    every_member_gets_a_share,
                    "admission and quota split disagree: budget={budget}, members={after}"
                );
            }
        }
    }

    #[test]
    fn a_resolved_member_set_that_omits_the_caller_fails_closed() {
        let members = vec![("root".to_owned(), 0), ("sibling".to_owned(), 1)];
        assert_eq!(
            tree_share_from_members("root".to_owned(), "caller", 8, &members),
            TrackTreeTerm::RootUnresolved
        );
    }
}
